//! Meta-tools mode (M1-10): instead of every backend tool, the client sees
//! three tools and finds the rest by search.
//!
//! - `search_tools {query, limit?}`: keyword match over tool names,
//!   descriptions and parameter names
//! - `describe_tool {name}`: a tool's full definition, including its schema
//! - `call_tool {name, arguments?}`: run any tool by its public name
//!
//! The tool list never changes, so servers added or enabled at runtime are
//! findable at once, even by clients that ignore `list_changed`.

use serde_json::{Map, Value, json};

use mcpgw_runtime::{BackendManager, JsonObject, ProtoError};

use crate::aggregate::{Scope, call_in, list_tools_in};

/// `search_tools`.
pub const SEARCH: &str = "search_tools";
/// `describe_tool`.
pub const DESCRIBE: &str = "describe_tool";
/// `call_tool`.
pub const CALL: &str = "call_tool";

/// Default and maximum number of search results.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

fn obj(v: Value) -> JsonObject {
    v.as_object().cloned().expect("object")
}

/// Whether `name` is one of the meta-tools.
pub fn is_meta(name: &str) -> bool {
    matches!(name, SEARCH | DESCRIBE | CALL)
}

/// Definitions of the three meta-tools.
pub fn meta_tools() -> Vec<JsonObject> {
    vec![
        obj(json!({
            "name": SEARCH,
            "description": "Search the tools of every connected MCP server by keywords. \
                Returns matching tool names with descriptions; use describe_tool for a \
                tool's parameters and call_tool to run it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Keywords, e.g. \"create github issue\""},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT}
                },
                "required": ["query"]
            },
            "annotations": {"readOnlyHint": true}
        })),
        obj(json!({
            "name": DESCRIBE,
            "description": "Show a tool's full definition, including its input schema.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string", "description": "Tool name from search_tools"}},
                "required": ["name"]
            },
            "annotations": {"readOnlyHint": true}
        })),
        obj(json!({
            "name": CALL,
            "description": "Call a tool found with search_tools, by name, with its arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Tool name from search_tools"},
                    "arguments": {"type": "object", "description": "Arguments matching the tool's input schema"}
                },
                "required": ["name"]
            }
        })),
    ]
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn param_names(tool: &JsonObject) -> Vec<String> {
    tool.get("inputSchema")
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .map(|p| p.keys().map(|k| k.to_lowercase()).collect())
        .unwrap_or_default()
}

/// Keyword score of a tool for `query` tokens: a name hit counts 3, a
/// parameter name 2, a description word 1. Zero means no match.
pub fn score(tool: &JsonObject, query: &[String]) -> u32 {
    let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
    let name_tokens = tokens(name);
    let desc = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let desc_tokens = tokens(&desc);
    let params = param_names(tool);
    query
        .iter()
        .map(|q| {
            let mut s = 0;
            if name_tokens.iter().any(|t| t.starts_with(q.as_str())) {
                s += 3;
            }
            if params.iter().any(|p| p.contains(q.as_str())) {
                s += 2;
            }
            if desc_tokens.iter().any(|t| t.starts_with(q.as_str())) {
                s += 1;
            }
            s
        })
        .sum()
}

/// Best matches for `query` among `tools` (already public-named): BM25
/// over names, descriptions and parameters (see [`crate::search`]). An
/// empty query lists tools by name.
pub fn search(tools: &[JsonObject], query: &str, limit: usize) -> Vec<JsonObject> {
    let limit = limit.clamp(1, MAX_LIMIT);
    if tokens(query).is_empty() {
        return keyword_search(tools, query, limit);
    }
    match crate::search::index_for(tools) {
        Some(index) => index
            .search(query, limit)
            .into_iter()
            .filter_map(|i| tools.get(i).cloned())
            .collect(),
        None => keyword_search(tools, query, limit),
    }
}

/// The M1-10 keyword search (prefix matches, name 3 / parameter 2 /
/// description 1), kept as a fallback and a benchmark baseline.
pub fn keyword_search(tools: &[JsonObject], query: &str, limit: usize) -> Vec<JsonObject> {
    let q = tokens(query);
    let mut hits: Vec<(u32, &JsonObject)> = tools
        .iter()
        .map(|t| (if q.is_empty() { 1 } else { score(t, &q) }, t))
        .filter(|(s, _)| *s > 0)
        .collect();
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["name"].as_str().cmp(&b.1["name"].as_str()))
    });
    hits.into_iter()
        .take(limit.clamp(1, MAX_LIMIT))
        .map(|(_, t)| t.clone())
        .collect()
}

fn text_result(text: String, structured: Value, is_error: bool) -> JsonObject {
    let mut r = Map::new();
    r.insert("content".into(), json!([{"type": "text", "text": text}]));
    if !structured.is_null() {
        r.insert("structuredContent".into(), structured);
    }
    if is_error {
        r.insert("isError".into(), Value::Bool(true));
    }
    r
}

fn tool_error(msg: impl Into<String>) -> JsonObject {
    text_result(msg.into(), Value::Null, true)
}

/// How many of the best search hits get their servers prewarmed.
pub const PREWARM_TOP: usize = 3;

/// Start, in the background, the cold servers behind the best hits: the
/// model's next step is usually to call one of them (M3-07). The manager
/// only does so when there's room under `max_running`.
fn prewarm(manager: &BackendManager, hits: &[JsonObject], scope: &Scope) {
    let mut seen = Vec::new();
    for t in hits.iter().take(PREWARM_TOP) {
        let Some(server) = t["name"]
            .as_str()
            .and_then(|n| crate::aggregate::server_of_in(manager, n, scope))
        else {
            continue;
        };
        if seen.contains(&server) {
            continue;
        }
        let account = scope.accounts.get(&server).map(String::as_str);
        manager.prewarm(&server, account);
        seen.push(server);
    }
}

/// Run a meta-tool. Problems the model can fix (unknown tool, a failing
/// backend) come back as tool errors (`isError`), so the model sees them.
pub async fn call(
    manager: &BackendManager,
    name: &str,
    args: Option<JsonObject>,
    scope: &Scope,
) -> Result<JsonObject, ProtoError> {
    let args = args.unwrap_or_default();
    let str_arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        SEARCH => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_LIMIT);
            let query = str_arg("query");
            let hits = search(&list_tools_in(manager, scope).await, &query, limit);
            if !scope.no_prewarm {
                prewarm(manager, &hits, scope);
            }
            let summary: Vec<Value> = hits
                .iter()
                .map(|t| json!({"name": t["name"], "description": t.get("description").cloned().unwrap_or(Value::Null)}))
                .collect();
            let text = if hits.is_empty() {
                format!("No tools match {query:?}.")
            } else {
                hits.iter()
                    .map(|t| {
                        format!(
                            "{} - {}",
                            t["name"].as_str().unwrap_or_default(),
                            t.get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(text_result(text, json!({"tools": summary}), false))
        }
        DESCRIBE => {
            let wanted = str_arg("name");
            if let Some(refusal) = scope.denial(&wanted) {
                return Ok(refusal);
            }
            match list_tools_in(manager, scope)
                .await
                .into_iter()
                .find(|t| t["name"] == wanted.as_str())
            {
                Some(t) => {
                    let v = Value::Object(t);
                    Ok(text_result(
                        serde_json::to_string_pretty(&v).unwrap_or_default(),
                        v,
                        false,
                    ))
                }
                None => Ok(tool_error(format!(
                    "No tool named {wanted:?}; use search_tools to find tool names."
                ))),
            }
        }
        CALL => {
            let target = str_arg("name");
            if target.is_empty() || is_meta(&target) {
                return Ok(tool_error(
                    "call_tool needs the name of a tool from search_tools.",
                ));
            }
            let inner = args.get("arguments").and_then(Value::as_object).cloned();
            match call_in(manager, &target, inner, scope).await {
                Ok(r) => Ok(r),
                // the gateway turns these into a sign-in link
                Err(e) if crate::login::needs_login(&e) => Err(e),
                Err(ProtoError::Rpc { message, .. }) => Ok(tool_error(message)),
                Err(e) => Ok(tool_error(e.to_string())),
            }
        }
        _ => Err(ProtoError::Rpc {
            code: mcpgw_proto::backend::INVALID_PARAMS,
            message: format!("tool not found: {name}"),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, desc: &str, params: &[&str]) -> JsonObject {
        let props: Map<String, Value> = params
            .iter()
            .map(|p| (p.to_string(), json!({"type": "string"})))
            .collect();
        obj(
            json!({"name": name, "description": desc, "inputSchema": {"type": "object", "properties": props}}),
        )
    }

    #[test]
    fn keyword_search_ranks_names_first() {
        let tools = vec![
            t(
                "github__create_issue",
                "[github] Create an issue in a repository",
                &["repo", "title"],
            ),
            t("github__search_code", "[github] Search code", &["query"]),
            t(
                "jira__create_ticket",
                "[jira] Create a Jira issue",
                &["project"],
            ),
            t("fs__read_file", "[fs] Read a file", &["path"]),
        ];
        let names = |hits: Vec<JsonObject>| -> Vec<String> {
            hits.iter()
                .map(|h| h["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(search(&tools, "create issue", 10)),
            ["github__create_issue", "jira__create_ticket"]
        );
        assert_eq!(names(search(&tools, "read", 10)), ["fs__read_file"]);
        assert_eq!(names(search(&tools, "path", 10)), ["fs__read_file"]);
        assert!(search(&tools, "zebra", 10).is_empty());
        assert_eq!(search(&tools, "", 2).len(), 2, "empty query browses");
        assert_eq!(search(&tools, "github", 1).len(), 1);
    }

    #[test]
    fn meta_tool_definitions() {
        let names: Vec<_> = meta_tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, [SEARCH, DESCRIBE, CALL]);
        assert!(names.iter().all(|n| is_meta(n)));
        assert!(!is_meta("fs__read"));
    }
}
