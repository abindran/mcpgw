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

/// Aggregated, prefixed tool list across all servers (from their manifests).
pub async fn list_all_tools(manager: &BackendManager) -> Vec<JsonObject> {
    let mut per = Vec::new();
    for name in manager.names() {
        let prefix = manager.prefix(&name).unwrap_or_else(|| name.clone());
        let tools = manager.tools(&name).await;
        per.push((prefix, tools));
    }
    aggregate(per.iter().map(|(p, t)| (p.as_str(), t.clone())))
}

/// Map a public name back to `(server, backend tool name)`.
pub async fn resolve(manager: &BackendManager, public: &str) -> Option<(String, String)> {
    let (prefix, part) = split_name(public)?;
    let server = manager.server_for_prefix(prefix)?;
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

/// Route a call by public name to the owning backend.
pub async fn call_public(
    manager: &BackendManager,
    public: &str,
    arguments: Option<JsonObject>,
) -> Result<JsonObject, ProtoError> {
    let (server, tool) = resolve(manager, public)
        .await
        .ok_or_else(|| ProtoError::Rpc {
            code: mcpgw_proto::backend::INVALID_PARAMS,
            message: format!("tool not found: {public}"),
            data: None,
        })?;
    manager.call_tool(&server, &tool, arguments).await
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
