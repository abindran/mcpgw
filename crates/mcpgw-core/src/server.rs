//! Backend server specification.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Separator between server name and tool name in aggregated tool names.
/// Server names may not contain it (see [`validate_server_name`]).
pub const TOOL_SEPARATOR: &str = "__";

/// How to reach a backend server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum Transport {
    /// Spawn a local child process speaking MCP over stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment variables for the child.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// Connect to a remote streamable-HTTP endpoint.
    Http {
        /// Endpoint URL.
        url: String,
    },
}

/// One backend tool server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Unique server name.
    pub name: String,
    /// Connection details.
    #[serde(flatten)]
    pub transport: Transport,
    /// Whether the gateway should start this server. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl ServerConfig {
    /// Check this server's own invariants: valid name, non-empty
    /// command/URL, http(s) URL.
    pub fn validate(&self) -> Result<()> {
        validate_server_name(&self.name)?;
        let empty = match &self.transport {
            Transport::Stdio { command, .. } => command.trim().is_empty(),
            Transport::Http { url } => url.trim().is_empty(),
        };
        if empty {
            return Err(Error::Config(format!(
                "server {} has an empty command/url",
                self.name
            )));
        }
        if let Transport::Http { url } = &self.transport
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
        let mut s = ServerConfig {
            name: "a".into(),
            transport: Transport::Stdio {
                command: " ".into(),
                args: vec![],
                env: BTreeMap::new(),
            },
            enabled: true,
        };
        assert!(s.validate().is_err());
        s.transport = Transport::Http {
            url: "ftp://x".into(),
        };
        assert!(s.validate().is_err());
        s.transport = Transport::Http {
            url: "https://x/mcp".into(),
        };
        s.validate().unwrap();
    }
}
