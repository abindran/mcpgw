//! Profiles and tool policies.
//!
//! A profile is a named view of the gateway (`/mcp/p/{profile}`): which
//! servers it exposes, how tools are presented, and what policy applies.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a profile presents tools to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Only the meta-tools (`search_tools`, `describe_tool`, `call_tool`).
    #[default]
    Meta,
    /// Every tool directly.
    Direct,
    /// Meta-tools plus the profile's pinned tools.
    Hybrid,
}

/// Tool policy: glob lists matched against public tool names
/// (`<server>__<tool>`). `*` matches any run of characters, `?` one.
///
/// Precedence: `deny` beats `approve`, which beats `allow`. When `allow` is
/// empty every tool not denied is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Tools that may be called. Empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Tools that are never called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// Tools that need a human approval per call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approve: Vec<String>,
}

/// A named profile (`[[profile]]` in the config file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Profile name (URL segment).
    pub name: String,
    /// Servers in this profile. Empty means every enabled server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<String>,
    /// Tool presentation. Default: `meta`.
    #[serde(default)]
    pub mode: Mode,
    /// Public tool names always listed directly in `hybrid` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_tools: Vec<String>,
    /// Account bindings: server name → account label (`github` → `work`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, String>,
    /// Tool policy.
    #[serde(default)]
    pub policy: Policy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_toml_and_json() {
        let p: Profile = toml::from_str(
            r#"
            name = "frontend"
            servers = ["fs", "gh", "figma"]
            mode = "hybrid"
            pinned_tools = ["fs__read_file"]
            accounts = { gh = "work" }
            [policy]
            deny = ["gh__delete_*"]
            approve = ["gh__merge_pull_request"]
            "#,
        )
        .unwrap();
        assert_eq!(p.mode, Mode::Hybrid);
        assert_eq!(p.policy.deny, ["gh__delete_*"]);
        assert_eq!(p.accounts["gh"], "work");
        let back: Profile = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let back: Profile = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn defaults_and_typos() {
        let p: Profile = toml::from_str("name = \"x\"").unwrap();
        assert_eq!(p.mode, Mode::Meta);
        assert!(p.servers.is_empty() && p.policy == Policy::default());
        assert!(toml::from_str::<Profile>("name = \"x\"\nmdoe = \"direct\"").is_err());
    }
}
