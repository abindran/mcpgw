//! Tool aggregation and namespacing (M1-09).
//!
//! Every backend tool is exposed as `<prefix>__<tool>`, where the prefix is
//! the server's alias (or name). Public names are kept to what strict clients
//! accept: at most [`MAX_TOOL_NAME`] characters of `[A-Za-z0-9_-]`. A name
//! that doesn't fit is shortened and given an 8-hex-digit hash of the
//! original, so two long tools can't collide:
//!
//! `github__list_pull_request_review_comments_for_repository_in_organization`
//! → `github__list_pull_request_review_comments_for_repositor_3f9a1c0b`
//!
//! Prefixes can't contain `__`, so the first `__` always identifies the
//! server; the tool part is then matched against that server's pinned tools.

use mcpgw_core::TOOL_SEPARATOR;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::Instrument;

use mcpgw_runtime::{BackendManager, JsonObject, ProtoError};

/// Longest public tool name. Several clients (Cursor, OpenAI-compatible
/// hosts) reject names over 64 characters.
pub const MAX_TOOL_NAME: usize = 64;

/// Length of the hash suffix, including its `_`.
const HASH_SUFFIX: usize = 9;

fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Public name for `tool` of the server with `prefix`.
pub fn public_name(prefix: &str, tool: &str) -> String {
    let plain = format!("{prefix}{TOOL_SEPARATOR}{tool}");
    if plain.len() <= MAX_TOOL_NAME && tool.chars().all(allowed) {
        return plain;
    }
    let digest = Sha256::digest(plain.as_bytes());
    let hash: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    let clean: String = plain
        .chars()
        .map(|c| if allowed(c) { c } else { '_' })
        .collect();
    let keep = MAX_TOOL_NAME - HASH_SUFFIX;
    let head: String = clean.chars().take(keep).collect();
    format!("{head}_{hash}")
}

/// Split a public name into `(prefix, tool part)` at the first `__`.
pub fn split_name(public: &str) -> Option<(&str, &str)> {
    let (s, t) = public.split_once(TOOL_SEPARATOR)?;
    (!s.is_empty() && !t.is_empty()).then_some((s, t))
}

/// Rewrite one backend tool object for the aggregated list: public name, and
/// a `[prefix]` tag on the description so agents can tell sources apart.
/// Returns `None` for a tool without a name.
pub fn prefix_tool(prefix: &str, mut tool: JsonObject) -> Option<JsonObject> {
    let name = tool.get("name")?.as_str()?.to_string();
    tool.insert("name".into(), Value::String(public_name(prefix, &name)));
    let desc = tool
        .get("description")
        .and_then(Value::as_str)
        .map(|d| format!("[{prefix}] {d}"))
        .unwrap_or_else(|| format!("[{prefix}]"));
    tool.insert("description".into(), Value::String(desc));
    Some(tool)
}

/// Merge per-server tool lists (`(prefix, tools)`), in the given order.
/// A public name seen twice keeps its first tool.
pub fn aggregate<'a, I>(per_server: I) -> Vec<JsonObject>
where
    I: IntoIterator<Item = (&'a str, Vec<JsonObject>)>,
{
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (prefix, tools) in per_server {
        for t in tools {
            if let Some(p) = prefix_tool(prefix, t) {
                let n = p["name"].as_str().unwrap_or_default().to_string();
                if seen.insert(n.clone()) {
                    out.push(p);
                } else {
                    tracing::warn!(tool = %n, "duplicate public tool name; skipped");
                }
            }
        }
    }
    out
}

/// Which servers an endpoint sees, and as which accounts (M2-07).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Servers this endpoint may list and call; `None` means all.
    pub servers: Option<std::collections::BTreeSet<String>>,
    /// Account per account-scoped server (M2-03).
    pub accounts: std::collections::BTreeMap<String, String>,
    /// Tool policy (M2-08), matched against public tool names.
    pub policy: mcpgw_core::Policy,
    /// Where this scope applies, for messages: `profile frontend`.
    pub label: String,
    /// Don't start servers whose tools `search_tools` returns (M3-07).
    pub no_prewarm: bool,
    /// The project whose endpoint this is (a `.mcpgw.toml`): its servers
    /// are seen as well as the global ones, and win a prefix clash.
    pub project: Option<String>,
}

impl Scope {
    /// Whether `server` is visible here.
    pub fn allows(&self, server: &str) -> bool {
        self.servers.as_ref().is_none_or(|s| s.contains(server))
    }

    /// The tool error for a call the policy refuses, if it does.
    pub fn denial(&self, public: &str) -> Option<JsonObject> {
        let mcpgw_core::Decision::Deny { pattern } = self.policy.decide(public) else {
            return None;
        };
        let at = if self.label.is_empty() {
            "this endpoint"
        } else {
            self.label.as_str()
        };
        let why = match pattern {
            Some(p) => format!("{at} denies `{p}`"),
            None if self.policy.cedar.is_some() => format!("no Cedar policy of {at} permits it"),
            None => format!("{at} only allows {}", self.policy.allow.join(", ")),
        };
        tracing::warn!(tool = public, scope = at, "call refused by policy");
        let text = format!("Tool {public} is disabled by policy ({why}).");
        Some(
            serde_json::json!({"content": [{"type": "text", "text": text}], "isError": true})
                .as_object()
                .cloned()
                .expect("object"),
        )
    }
}

/// Aggregated, prefixed tool list across all servers (from their manifests).
pub async fn list_all_tools(manager: &BackendManager) -> Vec<JsonObject> {
    list_tools_in(manager, &Scope::default()).await
}

/// Aggregated tool list of the servers `scope` allows.
pub async fn list_tools_in(manager: &BackendManager, scope: &Scope) -> Vec<JsonObject> {
    let mut per = Vec::new();
    for name in manager
        .names_in(scope.project.as_deref())
        .into_iter()
        .filter(|n| scope.allows(n))
    {
        let prefix = manager.prefix(&name).unwrap_or_else(|| name.clone());
        let tools = manager.tools(&name).await;
        per.push((prefix, tools));
    }
    let mut out = aggregate(per.iter().map(|(p, t)| (p.as_str(), t.clone())));
    if !scope.policy.is_empty() {
        out.retain(|t| t["name"].as_str().is_some_and(|n| scope.policy.visible(n)));
    }
    out
}

/// Map a public name back to `(server, backend tool name)`.
pub async fn resolve(manager: &BackendManager, public: &str) -> Option<(String, String)> {
    resolve_in(manager, public, &Scope::default()).await
}

/// Like [`resolve`], only among the servers `scope` allows.
pub async fn resolve_in(
    manager: &BackendManager,
    public: &str,
    scope: &Scope,
) -> Option<(String, String)> {
    let (prefix, part) = split_name(public)?;
    let server = manager.server_for_prefix_in(prefix, scope.project.as_deref())?;
    if !scope.allows(&server) {
        return None;
    }
    for t in manager.tools(&server).await {
        if let Some(name) = t.get("name").and_then(Value::as_str)
            && public_name(prefix, name) == public
        {
            return Some((server, name.to_string()));
        }
    }
    // Not in the manifest (yet): pass the plain tool part through.
    (public_name(prefix, part) == public).then(|| (server, part.to_string()))
}

/// The server behind a public tool name, if it is one.
pub fn server_of(manager: &BackendManager, public: &str) -> Option<String> {
    server_of_in(manager, public, &Scope::default())
}

/// The server behind a public tool name as `scope` sees it.
pub fn server_of_in(manager: &BackendManager, public: &str, scope: &Scope) -> Option<String> {
    let (prefix, _) = split_name(public)?;
    manager.server_for_prefix_in(prefix, scope.project.as_deref())
}

/// Route a call by public name to the owning backend (default accounts).
pub async fn call_public(
    manager: &BackendManager,
    public: &str,
    arguments: Option<JsonObject>,
) -> Result<JsonObject, ProtoError> {
    call_public_as(manager, public, arguments, &Default::default()).await
}

/// Route a call by public name, using `accounts` (server → account) for
/// account-scoped servers.
pub async fn call_public_as(
    manager: &BackendManager,
    public: &str,
    arguments: Option<JsonObject>,
    accounts: &std::collections::BTreeMap<String, String>,
) -> Result<JsonObject, ProtoError> {
    let scope = Scope {
        accounts: accounts.clone(),
        ..Scope::default()
    };
    call_in(manager, public, arguments, &scope).await
}

/// Route a call by public name within `scope`: servers outside it are
/// unknown, and account-scoped servers use its accounts.
pub async fn call_in(
    manager: &BackendManager,
    public: &str,
    arguments: Option<JsonObject>,
    scope: &Scope,
) -> Result<JsonObject, ProtoError> {
    if let Some(refusal) = scope.denial(public) {
        return Ok(refusal);
    }
    let accounts = &scope.accounts;
    let (server, tool) =
        resolve_in(manager, public, scope)
            .await
            .ok_or_else(|| ProtoError::Rpc {
                code: mcpgw_proto::backend::INVALID_PARAMS,
                message: format!("tool not found: {public}"),
                data: None,
            })?;
    let account = accounts.get(&server).map(String::as_str);
    // the call to the backend is its own (client) span, and its trace
    // context goes to the backend in `_meta` (M3-11)
    let span = tracing::info_span!(
        "backend",
        otel.name = %format!("backend tools/call {server}"),
        otel.kind = "client",
        server = %server,
        tool = %tool,
    );
    async move {
        let meta = mcpgw_telemetry::traceparent().map(|tp| {
            let mut m = JsonObject::new();
            m.insert("traceparent".into(), Value::String(tp));
            m
        });
        manager
            .call_tool_as_meta(&server, account, &tool, arguments, meta)
            .await
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, desc: Option<&str>) -> JsonObject {
        let mut v = json!({"name": name, "inputSchema": {"type": "object"}});
        if let Some(d) = desc {
            v["description"] = json!(d);
        }
        v.as_object().unwrap().clone()
    }

    #[test]
    fn short_names_are_plain() {
        assert_eq!(public_name("fs", "read"), "fs__read");
        assert_eq!(split_name("fs__read"), Some(("fs", "read")));
        // tool names may contain the separator; prefixes may not
        assert_eq!(split_name("fs__a__b"), Some(("fs", "a__b")));
        assert_eq!(split_name("noprefix"), None);
        assert_eq!(split_name("__x"), None);
        assert_eq!(split_name("x__"), None);
    }

    #[test]
    fn long_names_are_truncated_with_a_hash() {
        let long = "list_pull_request_review_comments_for_repository_in_organization";
        let p = public_name("github", long);
        assert_eq!(p.len(), MAX_TOOL_NAME, "{p}");
        assert!(
            p.starts_with("github__list_pull_request_review_comments_for_r"),
            "{p}"
        );
        assert!(p.chars().all(allowed));
        // same input, same name; a different tail, a different hash
        assert_eq!(p, public_name("github", long));
        let other = public_name("github", &format!("{long}_v2"));
        assert_ne!(p, other);
        assert_eq!(p[..55], other[..55]);
        // exactly 64 stays plain
        let fits = "x".repeat(64 - "gh__".len());
        assert_eq!(public_name("gh", &fits), format!("gh__{fits}"));
    }

    #[test]
    fn disallowed_characters_get_a_hash_too() {
        let a = public_name("fs", "read.file");
        let b = public_name("fs", "read_file");
        assert_eq!(b, "fs__read_file");
        assert!(
            a.starts_with("fs__read_file_") && a.len() == "fs__read_file_".len() + 8,
            "{a}"
        );
        assert_ne!(a, b, "sanitised names must not collide");
        assert!(public_name("fs", "dir/list").chars().all(allowed));
    }

    #[test]
    fn prefixes_name_and_description() {
        let t = prefix_tool("fs", tool("read", Some("Read a file"))).unwrap();
        assert_eq!(t["name"], "fs__read");
        assert_eq!(t["description"], "[fs] Read a file");
        assert_eq!(t["inputSchema"]["type"], "object");
        let t = prefix_tool("fs", tool("x", None)).unwrap();
        assert_eq!(t["description"], "[fs]");
        assert!(prefix_tool("fs", json!({"x":1}).as_object().unwrap().clone()).is_none());
    }

    #[test]
    fn same_tool_name_on_two_servers_does_not_collide() {
        let out = aggregate([
            ("github", vec![tool("search", None), tool("search", None)]),
            ("gitlab", vec![tool("search", None)]),
        ]);
        let names: Vec<_> = out.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["github__search", "gitlab__search"]);
    }
}
