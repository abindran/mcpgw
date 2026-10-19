//! Gateway configuration types, loaded from TOML.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Root configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Gateway-wide behaviour.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Tool search behaviour.
    #[serde(default)]
    pub search: SearchConfig,
    /// Backend tool servers.
    #[serde(default, rename = "server")]
    pub servers: Vec<ServerConfig>,
}

/// Gateway-wide settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Whether the agent may add new backend servers itself. Default: `false`.
    #[serde(default)]
    pub allow_agent_add_servers: bool,
}

/// Tool search settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Whether embeddings-based search is enabled. Default: `true`.
    #[serde(default = "default_true")]
    pub embeddings: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { embeddings: true }
    }
}

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
}

impl Config {
    /// Parse and validate configuration from TOML text.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and validate configuration from a file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_toml_str(&std::fs::read_to_string(path)?)
    }

    /// Check invariants: non-empty unique names, non-empty commands/URLs.
    pub fn validate(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for s in &self.servers {
            if s.name.trim().is_empty() {
                return Err(Error::Config("server name must not be empty".into()));
            }
            if !seen.insert(s.name.as_str()) {
                return Err(Error::Config(format!("duplicate server name: {}", s.name)));
            }
            let empty = match &s.transport {
                Transport::Stdio { command, .. } => command.trim().is_empty(),
                Transport::Http { url } => url.trim().is_empty(),
            };
            if empty {
                return Err(Error::Config(format!(
                    "server {} has an empty command/url",
                    s.name
                )));
            }
        }
        Ok(())
    }

    /// Look up a server by name.
    pub fn server(&self, name: &str) -> Result<&ServerConfig> {
        self.servers
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::UnknownServer(name.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_defaults() {
        let c = Config::from_toml_str("").unwrap();
        assert!(!c.gateway.allow_agent_add_servers);
        assert!(c.search.embeddings);
        assert!(c.servers.is_empty());
    }

    #[test]
    fn parses_servers() {
        let c = Config::from_toml_str(
            r#"
            [[server]]
            name = "fs"
            transport = "stdio"
            command = "mcp-fs"
            args = ["--ro"]

            [[server]]
            name = "web"
            transport = "http"
            url = "https://example.com/mcp"
            "#,
        )
        .unwrap();
        assert_eq!(c.servers.len(), 2);
        assert!(matches!(
            c.server("web").unwrap().transport,
            Transport::Http { .. }
        ));
        assert!(matches!(c.server("nope"), Err(Error::UnknownServer(_))));
    }

    #[test]
    fn rejects_duplicates_and_unknown_fields() {
        let dup = r#"
            [[server]]
            name = "a"
            transport = "http"
            url = "u"
            [[server]]
            name = "a"
            transport = "http"
            url = "v"
        "#;
        assert!(matches!(Config::from_toml_str(dup), Err(Error::Config(_))));
        assert!(Config::from_toml_str("bogus = 1").is_err());
    }

    #[test]
    fn rejects_empty_command() {
        let s = r#"
            [[server]]
            name = "a"
            transport = "stdio"
            command = " "
        "#;
        assert!(matches!(Config::from_toml_str(s), Err(Error::Config(_))));
    }
}
