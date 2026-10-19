//! Tool manifests: the tool list captured from a server, pinned by SHA-256.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// One tool as advertised by a backend (the MCP `Tool` object).
///
/// Known fields are typed; anything else (`icons`, `_meta`, ...) is kept in
/// `extra` so a manifest round-trips the server's tool exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    /// Tool name, unique within its server.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description shown to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema of the arguments.
    #[serde(default = "empty_object_schema")]
    pub input_schema: Value,
    /// JSON Schema of structured results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Behaviour hints (`readOnlyHint`, `destructiveHint`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// Any other fields, preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn empty_object_schema() -> Value {
    serde_json::json!({"type": "object"})
}

impl ToolDef {
    /// Parse an MCP tool JSON object.
    pub fn from_json(v: Value) -> crate::Result<Self> {
        serde_json::from_value(v).map_err(|e| crate::Error::Protocol(format!("invalid tool: {e}")))
    }

    /// The MCP tool JSON object.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("tool serialises")
    }
}

/// The tools one server advertised at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolManifest {
    /// Server name.
    pub server: String,
    /// Negotiated protocol version when captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    /// Capture time, Unix seconds.
    pub captured_at: u64,
    /// Tools, sorted by name.
    pub tools: Vec<ToolDef>,
    /// Pin: lowercase hex SHA-256 of the canonical JSON of `tools`.
    pub sha256: String,
}

impl ToolManifest {
    /// Build a manifest; tools are sorted by name and the pin computed.
    pub fn new(
        server: impl Into<String>,
        protocol_version: Option<String>,
        captured_at: u64,
        mut tools: Vec<ToolDef>,
    ) -> Self {
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let sha256 = pin(&tools);
        Self {
            server: server.into(),
            protocol_version,
            captured_at,
            tools,
            sha256,
        }
    }

    /// Whether `sha256` matches the tools (detects tampering in storage).
    pub fn verify(&self) -> bool {
        let mut tools = self.tools.clone();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        pin(&tools) == self.sha256
    }
}

/// SHA-256 pin of a tool list: order-insensitive, key-order-insensitive.
pub fn pin(tools: &[ToolDef]) -> String {
    let mut sorted: Vec<&ToolDef> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut buf = String::new();
    canonical_json(
        &Value::Array(sorted.iter().map(|t| t.to_json()).collect()),
        &mut buf,
    );
    let digest = Sha256::digest(buf.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write `v` as JSON with object keys sorted at every level, no whitespace.
pub fn canonical_json(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                canonical_json(&m[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(x, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(v: Value) -> ToolDef {
        ToolDef::from_json(v).unwrap()
    }

    #[test]
    fn keeps_unknown_fields() {
        let t = tool(json!({"name": "a", "description": "d",
            "inputSchema": {"type": "object"}, "icons": [{"src": "x"}]}));
        assert_eq!(t.extra["icons"][0]["src"], "x");
        assert_eq!(t.to_json()["icons"][0]["src"], "x");
        assert_eq!(t.to_json()["inputSchema"]["type"], "object");
    }

    #[test]
    fn pin_ignores_order_but_not_content() {
        let a = tool(
            json!({"name": "a", "description": "A", "inputSchema": {"type": "object", "properties": {}}}),
        );
        let b = tool(json!({"name": "b", "inputSchema": {"properties": {}, "type": "object"}}));
        let p1 = pin(&[a.clone(), b.clone()]);
        assert_eq!(p1.len(), 64);
        assert_eq!(p1, pin(&[b.clone(), a.clone()]));
        let mut a2 = a.clone();
        a2.description = Some("A, but different".into());
        assert_ne!(p1, pin(&[a2, b]));
    }

    #[test]
    fn manifest_roundtrip_and_verify() {
        let m = ToolManifest::new(
            "fs",
            Some("2026-07-28".into()),
            1_700_000_000,
            vec![
                tool(json!({"name": "write", "inputSchema": {"type": "object"}})),
                tool(json!({"name": "read", "inputSchema": {"type": "object"}})),
            ],
        );
        assert_eq!(m.tools[0].name, "read");
        assert!(m.verify());
        let json: ToolManifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(json, m);
        let toml_back: ToolManifest = toml::from_str(&toml::to_string(&m).unwrap()).unwrap();
        assert_eq!(toml_back, m);
        let mut bad = m.clone();
        bad.tools[0].description = Some("injected".into());
        assert!(!bad.verify());
    }

    #[test]
    fn canonical_json_sorts_nested_keys() {
        let mut s = String::new();
        canonical_json(&json!({"b": [1, {"y": true, "x": null}], "a": "é"}), &mut s);
        assert_eq!(s, r#"{"a":"é","b":[1,{"x":null,"y":true}]}"#);
    }
}
