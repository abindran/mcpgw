//! Profiles and tool policies.
//!
//! A profile is a named view of the gateway (`/mcp/p/{profile}`): which
//! servers it exposes, how tools are presented, and what policy applies.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

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
///
/// `cedar` names a Cedar policy file instead (M4-07); the lists must then
/// be empty.
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
    /// A Cedar policy file (M4-07) in place of the lists; relative to the
    /// config file. `mcpgw policy convert` writes one from the lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cedar: Option<PathBuf>,
    /// The loaded `cedar` engine, attached by the binary. Until it is, a
    /// Cedar policy denies everything.
    #[serde(skip)]
    #[schemars(skip)]
    pub engine: Option<Engine>,
}

/// A policy engine other than the glob lists (Cedar, M4-07).
pub trait PolicyEngine: Send + Sync + std::fmt::Debug {
    /// Decide for a public tool name.
    fn decide(&self, tool: &str) -> Decision;
}

/// A shared [`PolicyEngine`]; two are equal when they are the same one.
#[derive(Clone, Debug)]
pub struct Engine(pub Arc<dyn PolicyEngine>);

impl PartialEq for Engine {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Engine {}

/// What a policy says about one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The tool may be called.
    Allow,
    /// The tool may be called after a human approves it (M2-09).
    Approve {
        /// The `approve` pattern that matched.
        pattern: String,
    },
    /// The tool is disabled.
    Deny {
        /// The `deny` pattern that matched, or `None` when it isn't in a
        /// non-empty `allow` list.
        pattern: Option<String>,
    },
}

/// `*` matches any run of characters (including none), `?` exactly one.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

impl Policy {
    /// Decide for a public tool name: deny beats approve, which beats allow;
    /// an empty `allow` list allows everything not denied.
    pub fn decide(&self, tool: &str) -> Decision {
        if let Some(cedar) = &self.cedar {
            return match &self.engine {
                Some(e) => e.0.decide(tool),
                // fail closed: a policy that didn't load allows nothing
                None => Decision::Deny {
                    pattern: Some(format!("{} (not loaded)", cedar.display())),
                },
            };
        }
        let first = |list: &[String]| list.iter().find(|p| glob_match(p, tool)).cloned();
        if let Some(p) = first(&self.deny) {
            return Decision::Deny { pattern: Some(p) };
        }
        if let Some(p) = first(&self.approve) {
            return Decision::Approve { pattern: p };
        }
        if self.allow.is_empty() || first(&self.allow).is_some() {
            Decision::Allow
        } else {
            Decision::Deny { pattern: None }
        }
    }

    /// Whether the tool may be listed (allowed, or allowed with approval).
    pub fn visible(&self, tool: &str) -> bool {
        !matches!(self.decide(tool), Decision::Deny { .. })
    }

    /// Check the policy's own invariants: a Cedar file replaces the lists.
    pub fn check(&self) -> Result<(), String> {
        if self.cedar.is_some()
            && !(self.allow.is_empty() && self.deny.is_empty() && self.approve.is_empty())
        {
            return Err("a Cedar policy replaces allow/deny/approve: keep one or the other (`mcpgw policy convert` writes Cedar from the lists)".into());
        }
        Ok(())
    }

    /// Whether the policy restricts anything.
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty()
            && self.deny.is_empty()
            && self.approve.is_empty()
            && self.cedar.is_none()
    }
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
    fn globs() {
        for (p, t, ok) in [
            ("gh__*", "gh__delete_repo", true),
            ("*__delete_*", "gh__delete_repo", true),
            ("*", "", true),
            ("gh__?ead", "gh__read", true),
            ("gh__?ead", "gh__thread", false),
            ("fs__read", "fs__read_file", false),
            ("*file", "fs__read_file", true),
            ("a*b*c", "axxbyyc", true),
            ("a*b*c", "axxbyy", false),
        ] {
            assert_eq!(glob_match(p, t), ok, "{p} vs {t}");
        }
    }

    #[test]
    fn deny_beats_approve_beats_allow() {
        let p = Policy {
            allow: vec!["gh__*".into(), "fs__read_*".into()],
            deny: vec!["gh__delete_*".into()],
            approve: vec!["gh__merge_*".into(), "gh__delete_repo".into()],
            ..Default::default()
        };
        assert_eq!(p.decide("gh__list_issues"), Decision::Allow);
        assert_eq!(
            p.decide("gh__delete_repo"),
            Decision::Deny {
                pattern: Some("gh__delete_*".into())
            }
        );
        assert_eq!(
            p.decide("gh__merge_pull_request"),
            Decision::Approve {
                pattern: "gh__merge_*".into()
            }
        );
        assert_eq!(
            p.decide("fs__write_file"),
            Decision::Deny { pattern: None },
            "not in allow"
        );
        assert!(Policy::default().visible("anything") && Policy::default().is_empty());
        let only_deny = Policy {
            deny: vec!["*__exec".into()],
            ..Default::default()
        };
        assert_eq!(only_deny.decide("fs__read"), Decision::Allow);
    }

    #[test]
    fn cedar_policies_fail_closed_until_loaded() {
        #[derive(Debug)]
        struct OnlyFs;
        impl PolicyEngine for OnlyFs {
            fn decide(&self, tool: &str) -> Decision {
                if tool.starts_with("fs__") {
                    Decision::Allow
                } else {
                    Decision::Deny { pattern: None }
                }
            }
        }
        let mut p: Policy = toml::from_str(r#"cedar = "p.cedar""#).unwrap();
        assert!(!p.is_empty());
        assert_eq!(
            p.decide("fs__read"),
            Decision::Deny {
                pattern: Some("p.cedar (not loaded)".into())
            }
        );
        p.engine = Some(Engine(Arc::new(OnlyFs)));
        assert_eq!(p.decide("fs__read"), Decision::Allow);
        assert!(!p.visible("gh__add"));
        // the engine isn't config: it doesn't serialize
        assert_eq!(toml::to_string(&p).unwrap().trim(), r#"cedar = "p.cedar""#);
    }

    #[test]
    fn defaults_and_typos() {
        let p: Profile = toml::from_str("name = \"x\"").unwrap();
        assert_eq!(p.mode, Mode::Meta);
        assert!(p.servers.is_empty() && p.policy == Policy::default());
        assert!(toml::from_str::<Profile>("name = \"x\"\nmdoe = \"direct\"").is_err());
    }
}
