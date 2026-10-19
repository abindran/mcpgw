//! Tool aggregation across backends (M1-04).
//!
//! Every backend tool is exposed as `<server>__<tool>`. Server names cannot
//! contain `__` (enforced by config validation), so the first `__` always
//! splits a public name unambiguously, even if the tool name contains `__`.

use mcpgw_core::TOOL_SEPARATOR;
use serde_json::Value;

use crate::{BackendManager, JsonObject, ProtoError};

/// Maximum tool-name length many MCP clients accept.
pub const MAX_TOOL_NAME: usize = 128;

/// Public (prefixed) name for a backend tool.
pub fn public_name(server: &str, tool: &str) -> String {
    format!("{server}{TOOL_SEPARATOR}{tool}")
}

/// Split a public name into `(server, tool)`.
pub fn split_name(public: &str) -> Option<(&str, &str)> {
    let (s, t) = public.split_once(TOOL_SEPARATOR)?;
    (!s.is_empty() && !t.is_empty()).then_some((s, t))
}

/// Rewrite one backend tool object for the aggregated list.
///
/// Renames it, and prefixes the description with `[server]` so agents can
/// tell sources apart. Returns `None` (tool dropped) if the tool has no name
/// or the prefixed name would exceed [`MAX_TOOL_NAME`].
pub fn prefix_tool(server: &str, mut tool: JsonObject) -> Option<JsonObject> {
    let name = tool.get("name")?.as_str()?.to_string();
    let public = public_name(server, &name);
    if public.len() > MAX_TOOL_NAME {
        tracing::warn!(server, tool = %name, "tool name too long after prefixing; skipped");
        return None;
    }
    tool.insert("name".into(), Value::String(public));
    let desc = tool
        .get("description")
        .and_then(Value::as_str)
        .map(|d| format!("[{server}] {d}"))
        .unwrap_or_else(|| format!("[{server}]"));
    tool.insert("description".into(), Value::String(desc));
    Some(tool)
}

/// Merge per-server tool lists, in the given (sorted) server order.
pub fn aggregate<'a, I>(per_server: I) -> Vec<JsonObject>
where
    I: IntoIterator<Item = (&'a str, Vec<JsonObject>)>,
{
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (server, tools) in per_server {
        for t in tools {
            if let Some(p) = prefix_tool(server, t) {
                let n = p["name"].as_str().unwrap_or_default().to_string();
                if seen.insert(n) {
                    out.push(p);
                }
            }
        }
    }
    out
}

impl BackendManager {
    /// Aggregated, prefixed tool list across all connected backends.
    pub async fn list_all_tools(&self) -> Vec<JsonObject> {
        let mut per = Vec::new();
        for name in self.names() {
            per.push((name, self.tools(name).await));
        }
        aggregate(per)
    }

    /// Route a call by public name to the owning backend.
    pub async fn call_public(
        &self,
        public: &str,
        arguments: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let (server, tool) = split_name(public).ok_or_else(|| ProtoError::Rpc {
            code: mcpgw_proto::backend::INVALID_PARAMS,
            message: format!("tool not found: {public}"),
            data: None,
        })?;
        if !self.names().any(|n| n == server) {
            return Err(ProtoError::Rpc {
                code: mcpgw_proto::backend::INVALID_PARAMS,
                message: format!("tool not found: {public}"),
                data: None,
            });
        }
        self.call_tool(server, tool, arguments).await
    }
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
    fn names_roundtrip() {
        assert_eq!(public_name("fs", "read"), "fs__read");
        assert_eq!(split_name("fs__read"), Some(("fs", "read")));
        // tool names may contain the separator; server names may not
        assert_eq!(split_name("fs__a__b"), Some(("fs", "a__b")));
        assert_eq!(split_name("noprefix"), None);
        assert_eq!(split_name("__x"), None);
        assert_eq!(split_name("x__"), None);
    }

    #[test]
    fn prefixes_name_and_description() {
        let t = prefix_tool("fs", tool("read", Some("Read a file"))).unwrap();
        assert_eq!(t["name"], "fs__read");
        assert_eq!(t["description"], "[fs] Read a file");
        assert_eq!(t["inputSchema"]["type"], "object");
        let t = prefix_tool("fs", tool("x", None)).unwrap();
        assert_eq!(t["description"], "[fs]");
    }

    #[test]
    fn drops_nameless_and_too_long() {
        assert!(prefix_tool("fs", json!({"x":1}).as_object().unwrap().clone()).is_none());
        assert!(prefix_tool("fs", tool(&"a".repeat(130), None)).is_none());
    }

    #[test]
    fn aggregates_same_tool_name_from_two_servers() {
        let out = aggregate([
            ("a", vec![tool("add", None), tool("add", None)]),
            ("b", vec![tool("add", None)]),
        ]);
        let names: Vec<_> = out.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["a__add", "b__add"]);
    }
}
