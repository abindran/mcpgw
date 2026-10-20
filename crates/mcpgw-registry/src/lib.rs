//! `mcpgw-registry` — the official MCP Registry (M2-14).
//!
//! [`Registry`] searches and fetches `server.json` entries from
//! `https://registry.modelcontextprotocol.io` (`/v0/servers`). [`plan`]
//! turns an entry into a [`ServerSpec`] with **pinned versions**:
//! `npx -y pkg@1.2.3`, `uvx pkg==1.2.3`, a container server running
//! `image:tag` (Docker or Podman), or the entry's remote URL. Secret inputs become vault references
//! (`${secret:<server>_<input>}`) and are listed so the caller can ask for
//! them; optional secret inputs are left out unless the secret exists.

use std::collections::BTreeMap;

use mcpgw_core::{RegistryRef, ServerSpec, Transport};
use serde::Deserialize;

/// The official registry.
pub const OFFICIAL: &str = "https://registry.modelcontextprotocol.io";

/// A `server.json` entry (the parts mcpgw uses).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerJson {
    /// Reverse-DNS name, e.g. `io.github.github/github-mcp-server`.
    pub name: String,
    /// Version of this entry.
    pub version: String,
    /// Display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Ways to run it locally.
    #[serde(default)]
    pub packages: Vec<Package>,
    /// Hosted endpoints.
    #[serde(default)]
    pub remotes: Vec<Remote>,
}

/// A runnable package.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    /// `npm`, `pypi`, `oci`, `nuget`, `mcpb`, …
    pub registry_type: String,
    /// Package name or image reference.
    pub identifier: String,
    /// Package version.
    #[serde(default)]
    pub version: Option<String>,
    /// How it talks MCP (`stdio` is the one run locally).
    #[serde(default)]
    pub transport: PackageTransport,
    /// Command to use instead of the default for the registry type.
    #[serde(default)]
    pub runtime_hint: Option<String>,
    /// Arguments for the runtime (before the package).
    #[serde(default)]
    pub runtime_arguments: Vec<Argument>,
    /// Arguments for the package (after it).
    #[serde(default)]
    pub package_arguments: Vec<Argument>,
    /// Environment variables it reads.
    #[serde(default)]
    pub environment_variables: Vec<Input>,
}

/// A package's transport.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PackageTransport {
    /// `stdio`, `streamable-http`, `sse`.
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// A hosted endpoint.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    /// `streamable-http` or `sse`.
    #[serde(rename = "type")]
    pub kind: String,
    /// URL.
    pub url: String,
    /// Headers it needs.
    #[serde(default)]
    pub headers: Vec<Input>,
}

/// A named input: environment variable, header, or template variable.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    /// Name (variable, header).
    #[serde(default)]
    pub name: String,
    /// Fixed value (may contain `{variables}`).
    #[serde(default)]
    pub value: Option<String>,
    /// Default value.
    #[serde(default)]
    pub default: Option<String>,
    /// Shown to the user.
    #[serde(default)]
    pub description: Option<String>,
    /// Must be provided.
    #[serde(default)]
    pub is_required: bool,
    /// Holds a credential.
    #[serde(default)]
    pub is_secret: bool,
    /// Variables used in `value`.
    #[serde(default)]
    pub variables: BTreeMap<String, Input>,
}

/// A command-line argument.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Argument {
    /// `named` (`--flag value`) or `positional`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The flag, for named arguments.
    #[serde(default)]
    pub name: Option<String>,
    /// The rest.
    #[serde(flatten)]
    pub input: Input,
}

#[derive(Deserialize)]
struct Wrapped {
    server: ServerJson,
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    servers: Vec<Wrapped>,
}

/// A registry client.
pub struct Registry {
    base: String,
    http: reqwest::Client,
}

impl Registry {
    /// A client for the registry at `base` (no trailing slash needed).
    pub fn new(base: &str) -> Result<Self, String> {
        mcpgw_core::tls::install();
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| e.to_string())?,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        let r = self
            .http
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("registry unreachable ({}): {e}", self.base))?;
        let status = r.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err("not found in the registry".into());
        }
        if !status.is_success() {
            return Err(format!("registry answered {status}"));
        }
        r.json()
            .await
            .map_err(|e| format!("unreadable registry answer: {e}"))
    }

    /// Search by text (newest version of each).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ServerJson>, String> {
        let mut url =
            reqwest::Url::parse(&format!("{}/v0/servers", self.base)).map_err(|e| e.to_string())?;
        url.query_pairs_mut()
            .append_pair("search", query)
            .append_pair("version", "latest")
            .append_pair("limit", &limit.to_string());
        let l: Listing = self.get_json(url.as_str()).await?;
        Ok(l.servers.into_iter().map(|w| w.server).collect())
    }

    /// One entry: `version` pinned, or the latest.
    pub async fn get(&self, name: &str, version: Option<&str>) -> Result<ServerJson, String> {
        let enc = |s: &str| s.replace('%', "%25").replace('/', "%2F");
        let url = format!(
            "{}/v0/servers/{}/versions/{}",
            self.base,
            enc(name),
            enc(version.unwrap_or("latest"))
        );
        let w: Wrapped = self
            .get_json(&url)
            .await
            .map_err(|e| format!("{name}: {e}"))?;
        Ok(w.server)
    }
}

/// A secret the server needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeededSecret {
    /// Vault name (`github-mcp-server_token`).
    pub name: String,
    /// What it's for.
    pub description: String,
    /// The server can't run without it.
    pub required: bool,
}

/// How to install an entry.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPlan {
    /// The server to add (named, versions pinned, secrets as references).
    pub spec: ServerSpec,
    /// `npm 1.2.3`, `oci ghcr.io/x:1.0`, `remote https://…`.
    pub method: String,
    /// Secrets referenced (required ones must be in the vault).
    pub secrets: Vec<NeededSecret>,
    /// Optional secrets that aren't set, so their inputs were left out.
    pub skipped: Vec<String>,
    /// Things the user should know (a missing runtime, a fallback).
    pub notes: Vec<String>,
}

/// Choices for [`plan`].
pub struct PlanOptions<'a> {
    /// mcpgw server name to use.
    pub server_name: String,
    /// Use the hosted remote even if a package exists.
    pub prefer_remote: bool,
    /// Only this package type (`npm`, `pypi`, `oci`).
    pub package_type: Option<String>,
    /// Values for non-secret inputs (`NAME=value` from the command line).
    pub values: BTreeMap<String, String>,
    /// Whether a vault secret exists (decides optional secret inputs).
    pub has_secret: &'a dyn Fn(&str) -> bool,
    /// Whether a command (`npx`, `docker`) can be run here; packages whose
    /// runtime is missing lose to ones that can run, then to a remote.
    pub runtime_available: &'a dyn Fn(&str) -> bool,
}

pub use mcpgw_core::on_path;

fn runtime_of(p: &Package) -> &str {
    p.runtime_hint
        .as_deref()
        .unwrap_or(match p.registry_type.as_str() {
            "npm" => "npx",
            "pypi" => "uvx",
            _ => "docker",
        })
}

/// Whether a package can run here (an OCI image runs with Docker or Podman).
fn runnable(p: &Package, available: &dyn Fn(&str) -> bool) -> bool {
    match (p.registry_type.as_str(), p.runtime_hint.as_deref()) {
        ("oci", None) => mcpgw_core::CONTAINER_RUNTIMES.iter().any(|r| available(r)),
        _ => available(runtime_of(p)),
    }
}

/// Vault name for input `input` of `server`.
pub fn secret_name(server: &str, input: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!("{}_{}", clean(server), clean(input))
}

/// The server name `mcpgw add` uses for an entry: its last path segment.
pub fn default_server_name(registry_name: &str) -> String {
    let last = registry_name.rsplit('/').next().unwrap_or(registry_name);
    let s: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.replace("__", "_");
    s.trim_matches(|c| c == '-' || c == '_')
        .chars()
        .take(64)
        .collect()
}

struct Renderer<'a> {
    server: &'a str,
    opts: &'a PlanOptions<'a>,
    secrets: Vec<NeededSecret>,
    skipped: Vec<String>,
    /// env var → vault secret, injected at spawn
    bindings: BTreeMap<String, String>,
}

enum Rendered {
    Value(String),
    /// optional and its secret isn't set: leave the input out
    Skip,
}

impl Renderer<'_> {
    fn need(&mut self, name: &str, description: Option<&str>, required: bool) -> String {
        let secret = secret_name(self.server, name);
        if !self.secrets.iter().any(|s| s.name == secret) {
            self.secrets.push(NeededSecret {
                name: secret.clone(),
                description: description.unwrap_or(name).to_string(),
                required,
            });
        }
        format!("${{secret:{secret}}}")
    }

    /// The value of an input: fixed (with variables filled), a secret
    /// reference, a given value, or the default.
    fn input(&mut self, key: &str, i: &Input) -> Result<Rendered, String> {
        if i.is_secret && i.value.is_none() {
            let secret = secret_name(self.server, key);
            if !i.is_required && !(self.opts.has_secret)(&secret) {
                self.skipped.push(secret);
                return Ok(Rendered::Skip);
            }
            return Ok(Rendered::Value(self.need(
                key,
                i.description.as_deref(),
                i.is_required,
            )));
        }
        let Some(template) = i
            .value
            .clone()
            .or_else(|| self.opts.values.get(key).cloned())
            .or_else(|| i.default.clone())
        else {
            if i.is_required {
                return Err(format!(
                    "{key} is required{}; pass --env {key}=…",
                    i.description
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default()
                ));
            }
            return Ok(Rendered::Skip);
        };
        let mut out = template.clone();
        for (var, v) in &i.variables {
            let token = format!("{{{var}}}");
            if !out.contains(&token) {
                continue;
            }
            let fill = if v.is_secret {
                let secret = secret_name(self.server, var);
                // an optional input that needs an unset secret is left out
                if !i.is_required && !v.is_required && !(self.opts.has_secret)(&secret) {
                    self.skipped.push(secret);
                    return Ok(Rendered::Skip);
                }
                self.need(
                    var,
                    v.description.as_deref().or(i.description.as_deref()),
                    true,
                )
            } else {
                match self
                    .opts
                    .values
                    .get(var)
                    .cloned()
                    .or_else(|| v.default.clone())
                {
                    Some(x) => x,
                    None => {
                        return Err(format!(
                            "{key} needs a value for {{{var}}}; pass --env {var}=…"
                        ));
                    }
                }
            };
            out = out.replace(&token, &fill);
        }
        Ok(Rendered::Value(out))
    }

    /// `docker -e NAME={secret}`: inject NAME from the vault into the
    /// container's environment, so the value never shows up in a process
    /// list.
    fn docker_secret_env(&mut self, a: &Argument) -> Option<Rendered> {
        if !matches!(a.name.as_deref(), Some("-e" | "--env")) {
            return None;
        }
        let (env, rest) = a.input.value.as_deref()?.split_once('=')?;
        let var = rest.strip_prefix('{')?.strip_suffix('}')?;
        let v = a.input.variables.get(var).filter(|v| v.is_secret)?;
        let secret = secret_name(self.server, var);
        if !a.input.is_required && !v.is_required && !(self.opts.has_secret)(&secret) {
            self.skipped.push(secret);
            return Some(Rendered::Skip);
        }
        let desc = v.description.as_deref().or(a.input.description.as_deref());
        self.need(var, desc, a.input.is_required || v.is_required);
        self.bindings.insert(env.to_string(), secret);
        Some(Rendered::Value(env.to_string()))
    }

    fn args(&mut self, list: &[Argument], docker: bool) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for a in list {
            // injected into the container's env instead (never in argv)
            if docker && self.docker_secret_env(a).is_some() {
                continue;
            }
            let key = a.name.clone().unwrap_or_else(|| a.input.name.clone());
            let key = key.trim_start_matches('-').to_string();
            match (a.kind.as_str(), self.input(&key, &a.input)?) {
                (_, Rendered::Skip) => {}
                ("named", Rendered::Value(v)) => {
                    out.extend(a.name.clone());
                    if !v.is_empty() {
                        out.push(v);
                    }
                }
                (_, Rendered::Value(v)) => out.push(v),
            }
        }
        Ok(out)
    }
}

fn pinned_image(identifier: &str, version: Option<&str>) -> String {
    let last = identifier.rsplit('/').next().unwrap_or(identifier);
    if last.contains(':') || last.contains('@') {
        identifier.to_string()
    } else if let Some(v) = version {
        format!("{identifier}:{v}")
    } else {
        identifier.to_string()
    }
}

/// Turn a registry entry into a server to add.
pub fn plan(server: &ServerJson, opts: &PlanOptions) -> Result<InstallPlan, String> {
    let mut r = Renderer {
        server: &opts.server_name,
        opts,
        secrets: Vec::new(),
        skipped: Vec::new(),
        bindings: BTreeMap::new(),
    };
    let mut notes = Vec::new();
    let order = ["npm", "pypi", "oci"];
    let mut candidates: Vec<&Package> = server
        .packages
        .iter()
        .filter(|p| p.transport.kind.is_empty() || p.transport.kind == "stdio")
        .filter(|p| order.contains(&p.registry_type.as_str()))
        .filter(|p| {
            opts.package_type
                .as_deref()
                .is_none_or(|t| t == p.registry_type)
        })
        .collect();
    candidates.sort_by_key(|p| order.iter().position(|t| *t == p.registry_type));
    if let Some(t) = &opts.package_type
        && candidates.is_empty()
    {
        return Err(format!("{} has no {t} package mcpgw can run", server.name));
    }
    let has_remote = server
        .remotes
        .iter()
        .any(|x| x.kind == "streamable-http" || x.kind == "sse");
    let package = if opts.prefer_remote {
        None
    } else if let Some(p) = candidates
        .iter()
        .find(|p| runnable(p, opts.runtime_available))
    {
        Some(*p)
    } else if let Some(p) = candidates.first() {
        let missing = runtime_of(p).to_string();
        if has_remote && opts.package_type.is_none() {
            notes.push(format!(
                "{missing} is not installed; using the hosted remote instead"
            ));
            None
        } else {
            notes.push(format!(
                "{missing} is not installed; the server can't start until it is"
            ));
            Some(*p)
        }
    } else {
        None
    };
    let (transport, method) = match package {
        Some(p) if p.registry_type == "oci" => {
            let image = pinned_image(&p.identifier, p.version.as_deref());
            // `-e NAME={secret}` runtime args become injected env (see
            // `docker_secret_env`); the rest are run flags
            let run_args = r.args(&p.runtime_arguments, true)?;
            let mut env = BTreeMap::new();
            for v in &p.environment_variables {
                if let Rendered::Value(val) = r.input(&v.name, v)? {
                    if v.is_secret && v.value.is_none() {
                        r.bindings
                            .insert(v.name.clone(), secret_name(&opts.server_name, &v.name));
                    } else {
                        env.insert(v.name.clone(), val);
                    }
                }
            }
            let args = r.args(&p.package_arguments, false)?;
            let runtime = p
                .runtime_hint
                .clone()
                .filter(|h| mcpgw_core::CONTAINER_RUNTIMES.contains(&h.as_str()));
            let mut spec = ServerSpec::new(
                opts.server_name.clone(),
                Transport::Container {
                    image: image.clone(),
                    digest: None,
                    runtime,
                    args,
                    env,
                    run_args,
                },
            );
            spec.secrets = std::mem::take(&mut r.bindings);
            (spec, format!("oci {image}"))
        }
        Some(p) => {
            let version = p.version.clone();
            let mut args = Vec::new();
            let (command, method, package) = match p.registry_type.as_str() {
                "npm" => {
                    args.push("-y".to_string());
                    let pkg = match &version {
                        Some(v) => format!("{}@{v}", p.identifier),
                        None => p.identifier.clone(),
                    };
                    ("npx", format!("npm {pkg}"), pkg)
                }
                _ => {
                    let pkg = match &version {
                        Some(v) => format!("{}=={v}", p.identifier),
                        None => p.identifier.clone(),
                    };
                    ("uvx", format!("pypi {pkg}"), pkg)
                }
            };
            args.extend(r.args(&p.runtime_arguments, false)?);
            let mut env = BTreeMap::new();
            for v in &p.environment_variables {
                if let Rendered::Value(val) = r.input(&v.name, v)? {
                    if v.is_secret && v.value.is_none() {
                        r.bindings
                            .insert(v.name.clone(), secret_name(&opts.server_name, &v.name));
                    } else {
                        env.insert(v.name.clone(), val);
                    }
                }
            }
            args.push(package);
            args.extend(r.args(&p.package_arguments, false)?);
            let command = p
                .runtime_hint
                .clone()
                .unwrap_or_else(|| command.to_string());
            let mut spec = ServerSpec::new(
                opts.server_name.clone(),
                Transport::Stdio {
                    command,
                    args,
                    env,
                    cwd: None,
                },
            );
            spec.secrets = std::mem::take(&mut r.bindings);
            (spec, method)
        }
        None => {
            let remote = server
                .remotes
                .iter()
                .find(|x| x.kind == "streamable-http" || x.kind == "sse")
                .ok_or_else(|| {
                    format!(
                        "{} has no package mcpgw can run ({}) and no remote",
                        server.name,
                        server
                            .packages
                            .iter()
                            .map(|p| p.registry_type.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            let mut headers = BTreeMap::new();
            for h in &remote.headers {
                if let Rendered::Value(v) = r.input(&h.name, h)? {
                    // a bare secret Authorization header is a bearer token
                    let v = if h.name.eq_ignore_ascii_case("authorization")
                        && h.is_secret
                        && h.value.is_none()
                    {
                        format!("Bearer {v}")
                    } else {
                        v
                    };
                    headers.insert(h.name.clone(), v);
                }
            }
            let spec = ServerSpec::new(
                opts.server_name.clone(),
                Transport::Http {
                    url: remote.url.clone(),
                    headers,
                },
            );
            (spec, format!("remote {}", remote.url))
        }
    };
    let mut spec = transport;
    spec.registry = Some(RegistryRef {
        name: server.name.clone(),
        version: server.version.clone(),
    });
    spec.validate().map_err(|e| e.to_string())?;
    Ok(InstallPlan {
        spec,
        method,
        secrets: r.secrets,
        skipped: r.skipped,
        notes,
    })
}
