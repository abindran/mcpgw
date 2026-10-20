//! `mcpgw-cedar` — Cedar policies as an optional policy engine (M4-07).
//!
//! A profile (or the default endpoint) can name a Cedar policy file in
//! place of its `allow` / `deny` / `approve` lists:
//!
//! ```toml
//! policy = { cedar = "policies/frontend.cedar" }
//! ```
//!
//! Each tool decision is a Cedar request:
//!
//! - **principal:** `Mcpgw::Endpoint::"<profile>"` (`"default"` for the
//!   default endpoint);
//! - **action:** `Mcpgw::Action::"call"`;
//! - **resource:** `Mcpgw::Tool::"<public name>"`, a member of
//!   `Mcpgw::Server::"<prefix>"`, with `name`, `server` and `tool`
//!   attributes.
//!
//! `forbid` beats `permit`, as always in Cedar. A `permit` with an
//! `@approve` annotation makes an allowed call wait for a human (M2-09).
//! Policies are checked against [`SCHEMA`] when they load, so a typo in
//! an attribute is an error rather than a silent deny.
//!
//! [`convert`] turns glob rules into Cedar that gives the same decisions.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use cedar_policy::{
    Authorizer, Context, Decision as CedarDecision, Entities, Entity, EntityId, EntityTypeName,
    EntityUid, PolicyId, PolicySet, Request, RestrictedExpression, Schema, ValidationMode,
    Validator,
};
use mcpgw_core::{Decision, Policy, PolicyEngine};

/// The entities and action policies are written against.
pub const SCHEMA: &str = r#"namespace Mcpgw {
  // A gateway endpoint: a profile's name, or "default".
  entity Endpoint;
  // A server, by the prefix of its public tool names.
  entity Server;
  // A tool, by its public name `<prefix>__<tool>`.
  entity Tool in [Server] = {
    "name": String,
    "server": String,
    "tool": String,
  };
  action "call" appliesTo {
    principal: [Endpoint],
    resource: [Tool],
  };
}
"#;

fn schema() -> Schema {
    Schema::from_cedarschema_str(SCHEMA)
        .map(|(s, _)| s)
        .expect("the built-in schema parses")
}

fn uid(kind: &str, id: &str) -> EntityUid {
    let kind = EntityTypeName::from_str(&format!("Mcpgw::{kind}")).expect("a valid type name");
    EntityUid::from_type_name_and_id(kind, EntityId::new(id))
}

/// A loaded, validated Cedar policy set for one endpoint.
pub struct Cedar {
    set: PolicySet,
    /// Policy ids in file order: the first matching policy names a decision.
    order: HashMap<PolicyId, usize>,
    principal: EntityUid,
    schema: Schema,
    cache: Mutex<HashMap<String, Decision>>,
}

impl std::fmt::Debug for Cedar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cedar")
            .field("policies", &self.order.len())
            .field("principal", &self.principal.to_string())
            .finish()
    }
}

impl Cedar {
    /// Parse and validate `text` for `endpoint` (a profile name, or
    /// `default`).
    pub fn parse(text: &str, endpoint: &str) -> Result<Self, String> {
        let set = PolicySet::from_str(text).map_err(|e| render(&e))?;
        let schema = schema();
        let v = Validator::new(schema.clone()).validate(&set, ValidationMode::Strict);
        if !v.validation_passed() {
            let errs: Vec<String> = v.validation_errors().map(|e| render(e)).collect();
            return Err(errs.join("; "));
        }
        // `from_str` names policies policy0, policy1, … in file order
        let order = set
            .policies()
            .map(|p| {
                let n = p.id().to_string()["policy".len()..]
                    .parse()
                    .unwrap_or(usize::MAX);
                (p.id().clone(), n)
            })
            .collect();
        Ok(Self {
            set,
            order,
            principal: uid("Endpoint", endpoint),
            schema,
            cache: Mutex::default(),
        })
    }

    /// Read and parse a policy file.
    pub fn load(path: &Path, endpoint: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text, endpoint).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// How a policy is named in messages: its `@id`, else its position.
    fn label(&self, id: &PolicyId) -> String {
        match self.set.annotation(id, "id") {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => id.to_string(),
        }
    }

    fn first<'a>(&self, ids: impl Iterator<Item = &'a PolicyId>) -> Option<&'a PolicyId> {
        ids.min_by_key(|id| self.order.get(*id).copied().unwrap_or(usize::MAX))
    }

    fn evaluate(&self, public: &str) -> Decision {
        let (server, tool) = public.split_once("__").unwrap_or(("", public));
        let resource = uid("Tool", public);
        let server_uid = uid("Server", server);
        let attrs = HashMap::from([
            (
                "name".to_string(),
                RestrictedExpression::new_string(public.into()),
            ),
            (
                "server".to_string(),
                RestrictedExpression::new_string(server.into()),
            ),
            (
                "tool".to_string(),
                RestrictedExpression::new_string(tool.into()),
            ),
        ]);
        let built = Entity::new(resource.clone(), attrs, HashSet::from([server_uid.clone()]))
            .map_err(|e| e.to_string())
            .and_then(|t| {
                Entities::from_entities(
                    [
                        t,
                        Entity::new_no_attrs(server_uid, HashSet::new()),
                        Entity::new_no_attrs(self.principal.clone(), HashSet::new()),
                    ],
                    Some(&self.schema),
                )
                .map_err(|e| e.to_string())
            })
            .and_then(|entities| {
                Request::new(
                    self.principal.clone(),
                    uid("Action", "call"),
                    resource,
                    Context::empty(),
                    Some(&self.schema),
                )
                .map(|r| (r, entities))
                .map_err(|e| e.to_string())
            });
        let (request, entities) = match built {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(tool = public, error = %e, "cedar request refused");
                return Decision::Deny {
                    pattern: Some(format!("cedar: {e}")),
                };
            }
        };
        let response = Authorizer::new().is_authorized(&request, &self.set, &entities);
        for e in response.diagnostics().errors() {
            // a policy that errors is skipped (Cedar's rule): say so
            tracing::warn!(tool = public, error = %e, "cedar policy error");
        }
        let reasons = response.diagnostics().reason();
        match response.decision() {
            CedarDecision::Deny => Decision::Deny {
                pattern: self.first(reasons).map(|id| self.label(id)),
            },
            CedarDecision::Allow => {
                let approve =
                    self.first(reasons.filter(|id| self.set.annotation(id, "approve").is_some()));
                match approve {
                    Some(id) => Decision::Approve {
                        pattern: match self.set.annotation(id, "approve") {
                            Some(p) if !p.is_empty() => p.to_string(),
                            _ => self.label(id),
                        },
                    },
                    None => Decision::Allow,
                }
            }
        }
    }
}

impl PolicyEngine for Cedar {
    fn decide(&self, tool: &str) -> Decision {
        if let Some(d) = self.cache.lock().unwrap().get(tool) {
            return d.clone();
        }
        let d = self.evaluate(tool);
        self.cache
            .lock()
            .unwrap()
            .insert(tool.to_string(), d.clone());
        d
    }
}

/// Cedar errors with their help text, on one line.
fn render(e: &dyn miette::Diagnostic) -> String {
    let mut s = e.to_string();
    if let Some(h) = e.help() {
        s.push_str(&format!(" ({h})"));
    }
    if let Some(related) = e.related() {
        for r in related {
            s.push_str(&format!("; {}", render(r)));
        }
    }
    s
}

type Cached = (Option<SystemTime>, Arc<Cedar>);
static LOADED: Mutex<Option<HashMap<(PathBuf, String), Cached>>> = Mutex::new(None);

/// Give `policy` its Cedar engine when it names a policy file. Files are
/// loaded once per endpoint and again when they change. A file that fails
/// to load leaves the engine unset, so every tool is denied (the policy
/// fails closed), and the error is returned.
pub fn attach(policy: &mut Policy, endpoint: &str) -> Result<(), String> {
    let Some(path) = policy.cedar.clone() else {
        return Ok(());
    };
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let key = (path.clone(), endpoint.to_string());
    let mut loaded = LOADED.lock().unwrap();
    let cache = loaded.get_or_insert_with(HashMap::new);
    if let Some((at, engine)) = cache.get(&key)
        && *at == modified
    {
        policy.engine = Some(mcpgw_core::Engine(engine.clone()));
        return Ok(());
    }
    policy.engine = None;
    let engine = Arc::new(Cedar::load(&path, endpoint)?);
    cache.insert(key, (modified, engine.clone()));
    policy.engine = Some(mcpgw_core::Engine(engine));
    Ok(())
}

/// A glob pattern as a Cedar `like` pattern. `*` carries over; `?` (one
/// character) has no Cedar equivalent.
fn like(glob: &str) -> Result<String, String> {
    if glob.contains('?') {
        return Err(format!(
            "pattern {glob:?} uses `?`, which Cedar can't express; rewrite it with `*`"
        ));
    }
    let escaped = glob.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

/// Cedar policies that give the same decisions as `policy`'s glob lists:
/// a `forbid` per `deny` pattern, an `@approve` `permit` per `approve`
/// pattern and a `permit` per `allow` pattern (one for everything when
/// `allow` is empty). Messages name the same patterns.
pub fn convert(policy: &Policy) -> Result<String, String> {
    if policy.cedar.is_some() {
        return Err("this policy is already Cedar".into());
    }
    const HEAD: &str = "(principal, action == Mcpgw::Action::\"call\", resource)";
    let mut out = String::from(
        "// Converted from allow/deny/approve by `mcpgw policy convert`.\n\
         // forbid beats permit; a permit with @approve needs a human per call.\n",
    );
    for p in &policy.deny {
        out.push_str(&format!(
            "\n@id({id})\nforbid {HEAD}\nwhen {{ resource.name like {pat} }};\n",
            id = like(p)?,
            pat = like(p)?
        ));
    }
    for p in &policy.approve {
        out.push_str(&format!(
            "\n@id({id})\n@approve({pat})\npermit {HEAD}\nwhen {{ resource.name like {pat} }};\n",
            id = like(&format!("approve {p}"))?,
            pat = like(p)?
        ));
    }
    if policy.allow.is_empty() {
        out.push_str(&format!("\n@id(\"allow all\")\npermit {HEAD};\n"));
    }
    for p in &policy.allow {
        out.push_str(&format!(
            "\n@id({id})\npermit {HEAD}\nwhen {{ resource.name like {pat} }};\n",
            id = like(&format!("allow {p}"))?,
            pat = like(p)?
        ));
    }
    Ok(out)
}
