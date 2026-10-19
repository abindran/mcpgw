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
    /// Unique server name; tools appear as `<name>__<tool>`.
    pub name: String,
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
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl ServerSpec {
    /// A spec with defaults for everything but name and transport.
    pub fn new(name: impl Into<String>, transport: Transport) -> Self {
        Self {
            name: name.into(),
            transport,
            enabled: true,
            idle_timeout: None,
            stateful: false,
            startup_timeout: None,
        }
    }

    /// Check this server's own invariants: valid name, non-empty
    /// command/URL, http(s) URL.
    pub fn validate(&self) -> Result<()> {
        validate_server_name(&self.name)?;
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
