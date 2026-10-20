//! Backend server specification.

use crate::{Duration, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Separator between server name and tool name in aggregated tool names.
/// Server names may not contain it (see [`validate_server_name`]).
pub const TOOL_SEPARATOR: &str = "__";

/// How to reach a backend server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum Transport {
    /// Spawn a local child process speaking MCP over stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Extra environment variables for the child.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// Working directory. Default: the gateway's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    /// Connect to a remote Streamable HTTP endpoint.
    Http {
        /// Endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

impl Transport {
    /// A stdio transport with no extra env or working directory.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::Stdio {
            command: command.into(),
            args,
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    /// An HTTP transport with no extra headers.
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http {
            url: url.into(),
            headers: BTreeMap::new(),
        }
    }

    /// Short kind name: `stdio` or `http`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http { .. } => "http",
        }
    }
}

/// One backend tool server (`[[server]]` in the config file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerSpec {
    /// Unique server name.
    pub name: String,
    /// Shorter prefix for its tools' public names (`<alias>__<tool>`).
    /// Default: the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Connection details.
    #[serde(flatten)]
    pub transport: Transport,
    /// Whether the gateway may start this server. Default: `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Stop the server after this long without calls. Default: gateway-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<Duration>,
    /// The server keeps user state (browser, database session): it is never
    /// stopped for idleness or evicted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stateful: bool,
    /// Deadline for start-up and the MCP handshake. Default: gateway-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout: Option<Duration>,
    /// Secrets injected at spawn: environment variable → vault secret name,
    /// e.g. `{ GITHUB_TOKEN = "github_token" }`. Args and env values may
    /// also contain `${secret:<name>}`. Injected values are redacted from
    /// everything the server sends back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
}

/// Stands for the caller's account in secret names:
/// `secrets = { GITHUB_TOKEN = "oauth/github@{account}" }`.
pub const ACCOUNT_PLACEHOLDER: &str = "{account}";

/// Account used when a caller names none.
pub const DEFAULT_ACCOUNT: &str = "default";

/// Prefix of a secret placeholder in args and env values.
pub const SECRET_PLACEHOLDER: &str = "${secret:";

/// Where a `${secret:` placeholder's name ends (its closing `}`), stepping
/// over a nested `{account}`.
fn placeholder_end(after: &str) -> Option<usize> {
    let mut i = 0;
    while i < after.len() {
        if after[i..].starts_with(ACCOUNT_PLACEHOLDER) {
            i += ACCOUNT_PLACEHOLDER.len();
        } else if after[i..].starts_with('}') {
            return Some(i);
        } else {
            i += after[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// Names referenced by `${secret:<name>}` placeholders in `s`.
pub fn secret_placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(SECRET_PLACEHOLDER) {
        let after = &rest[i + SECRET_PLACEHOLDER.len()..];
        match placeholder_end(after) {
            Some(j) => {
                out.push(after[..j].to_string());
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    out
}

/// Replace `${secret:<name>}` placeholders using `lookup`.
pub fn fill_placeholders(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(SECRET_PLACEHOLDER) {
        out.push_str(&rest[..i]);
        let after = &rest[i + SECRET_PLACEHOLDER.len()..];
        match placeholder_end(after) {
            Some(j) => {
                out.push_str(&lookup(&after[..j]).unwrap_or_default());
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl ServerSpec {
    /// Prefix of this server's public tool names: the alias, else the name.
    pub fn prefix(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }

    /// A spec with defaults for everything but name and transport.
    pub fn new(name: impl Into<String>, transport: Transport) -> Self {
        Self {
            name: name.into(),
            alias: None,
            transport,
            enabled: true,
            idle_timeout: None,
            stateful: false,
            startup_timeout: None,
            secrets: BTreeMap::new(),
        }
    }

    /// Whether this server's secrets depend on the caller's account.
    pub fn account_scoped(&self) -> bool {
        self.secret_refs()
            .iter()
            .any(|r| r.contains(ACCOUNT_PLACEHOLDER))
    }

    /// This server with `{account}` filled in, for one account.
    pub fn for_account(&self, account: &str) -> ServerSpec {
        let fill = |s: &str| s.replace(ACCOUNT_PLACEHOLDER, account);
        let mut s = self.clone();
        for v in s.secrets.values_mut() {
            *v = fill(v);
        }
        match &mut s.transport {
            Transport::Stdio { args, env, .. } => {
                for a in args.iter_mut() {
                    *a = fill(a);
                }
                for v in env.values_mut() {
                    *v = fill(v);
                }
            }
            Transport::Http { headers, .. } => {
                for v in headers.values_mut() {
                    *v = fill(v);
                }
            }
        }
        s
    }

    /// Every secret this server uses: bindings and placeholders.
    pub fn secret_refs(&self) -> std::collections::BTreeSet<String> {
        let mut refs: std::collections::BTreeSet<String> = self.secrets.values().cloned().collect();
        if let Transport::Stdio { args, env, .. } = &self.transport {
            for s in args.iter().chain(env.values()) {
                refs.extend(secret_placeholders(s));
            }
        }
        if let Transport::Http { headers, .. } = &self.transport {
            for s in headers.values() {
                refs.extend(secret_placeholders(s));
            }
        }
        refs
    }

    /// Check this server's own invariants: valid name, non-empty
    /// command/URL, http(s) URL.
    pub fn validate(&self) -> Result<()> {
        validate_server_name(&self.name)?;
        for (var, secret) in &self.secrets {
            let ok_var = !var.is_empty()
                && !var.starts_with(|c: char| c.is_ascii_digit())
                && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok_var {
                return Err(Error::Config(format!(
                    "server {}: {var:?} is not an environment variable name",
                    self.name
                )));
            }
            crate::validate_secret_name(&secret.replace(ACCOUNT_PLACEHOLDER, DEFAULT_ACCOUNT))?;
        }
        for r in self.secret_refs() {
            crate::validate_secret_name(&r.replace(ACCOUNT_PLACEHOLDER, DEFAULT_ACCOUNT)).map_err(
                |_| {
                    Error::Config(format!(
                        "server {}: bad secret placeholder name {r:?}",
                        self.name
                    ))
                },
            )?;
        }
        if let Some(a) = &self.alias {
            validate_server_name(a).map_err(|_| {
                Error::Config(format!("invalid alias {a:?} for server {}", self.name))
            })?;
        }
        let empty = match &self.transport {
            Transport::Stdio { command, .. } => command.trim().is_empty(),
            Transport::Http { url, .. } => url.trim().is_empty(),
        };
        if empty {
            return Err(Error::Config(format!(
                "server {} has an empty command/url",
                self.name
            )));
        }
        if let Transport::Http { url, .. } = &self.transport
            && !(url.starts_with("http://") || url.starts_with("https://"))
        {
            return Err(Error::Config(format!(
                "server {} url must start with http:// or https://",
                self.name
            )));
        }
        Ok(())
    }
}

/// Validate a server name: 1-64 chars of `[A-Za-z0-9_-]`, no `__`.
pub fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(Error::Config(format!(
            "server name must be 1-64 characters: {name:?}"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Config(format!(
            "server name may only contain [A-Za-z0-9_-]: {name:?}"
        )));
    }
    if name.contains(TOOL_SEPARATOR) || name.starts_with('_') || name.ends_with('_') {
        return Err(Error::Config(format!(
            "server name may not contain {TOOL_SEPARATOR:?} or start/end with '_': {name:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_name_rules() {
        for ok in ["fs", "my-server", "a_b", "X1"] {
            validate_server_name(ok).unwrap();
        }
        for bad in ["", "a__b", "_a", "a_", "a b", "a.b", &"x".repeat(65)] {
            assert!(validate_server_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn validates_transport_fields() {
        let mut s = ServerSpec::new("a", Transport::stdio(" ", vec![]));
        assert!(s.validate().is_err());
        s.transport = Transport::http("ftp://x");
        assert!(s.validate().is_err());
        s.transport = Transport::http("https://x/mcp");
        s.validate().unwrap();
    }

    #[test]
    fn secret_bindings_and_placeholders() {
        let mut s = ServerSpec::new(
            "gh",
            Transport::Stdio {
                command: "gh-mcp".into(),
                args: vec!["--token=${secret:gh_pat}".into(), "--x".into()],
                env: [("URL".to_string(), "https://${secret:host}/api".to_string())].into(),
                cwd: None,
            },
        );
        s.secrets
            .insert("GITHUB_TOKEN".into(), "github_token".into());
        s.validate().unwrap();
        assert_eq!(
            s.secret_refs().into_iter().collect::<Vec<_>>(),
            ["gh_pat", "github_token", "host"]
        );
        let filled = fill_placeholders("a ${secret:x} b ${secret:y} ${secret:", &|n| {
            (n == "x").then(|| "X".to_string())
        });
        assert_eq!(filled, "a X b  ${secret:");
        s.secrets.insert("1BAD".into(), "k".into());
        assert!(s.validate().is_err());
    }

    #[test]
    fn account_placeholders() {
        let mut s = ServerSpec::new(
            "gh",
            Transport::Stdio {
                command: "gh-mcp".into(),
                args: vec!["--token=${secret:oauth/github@{account}}".into()],
                env: Default::default(),
                cwd: None,
            },
        );
        assert!(!ServerSpec::new("x", Transport::stdio("x", vec![])).account_scoped());
        assert!(s.account_scoped());
        s.secrets
            .insert("GITHUB_TOKEN".into(), "oauth/github@{account}".into());
        s.validate().unwrap();
        let w = s.for_account("work");
        assert!(!w.account_scoped());
        assert_eq!(w.secrets["GITHUB_TOKEN"], "oauth/github@work");
        assert!(w.secret_refs().contains("oauth/github@work"));
        assert_eq!(w.name, "gh");
    }

    #[test]
    fn toml_and_json_roundtrip() {
        let toml_src = r#"
            name = "gh"
            transport = "stdio"
            command = "github-mcp"
            args = ["--ro"]
            env = { LOG = "1" }
            idle_timeout = "5m"
            stateful = true
        "#;
        let s: ServerSpec = toml::from_str(toml_src).unwrap();
        assert_eq!(s.idle_timeout, Some(Duration::from_secs(300)));
        assert!(s.stateful && s.enabled);
        let back: ServerSpec = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["transport"], "stdio");
        assert_eq!(json["idle_timeout"], "5m");
        assert!(
            json.get("enabled").is_none(),
            "defaults are omitted: {json}"
        );
        assert_eq!(serde_json::from_value::<ServerSpec>(json).unwrap(), s);
    }
}
