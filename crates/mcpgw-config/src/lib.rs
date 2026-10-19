//! `mcpgw-config` — gateway configuration loaded from TOML.

use mcpgw_core::{Error, Result, ServerSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the config file location.
pub const CONFIG_ENV: &str = "MCPGW_CONFIG";

/// Root configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Gateway-wide behaviour.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Backend tool servers.
    #[serde(default, rename = "server", skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerSpec>,
}

/// JSON Schema of the config file.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Config)).expect("schema serialises")
}

/// Gateway-wide settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Whether the agent may add new backend servers itself. Default: `false`.
    #[serde(default)]
    pub allow_agent_add_servers: bool,
    /// Path to the SQLite store. Default: `<data dir>/mcpgw/mcpgw.db`.
    #[serde(default)]
    pub store_path: Option<PathBuf>,
    /// Path of the static loopback auth token file.
    /// Default: `<data dir>/mcpgw/token`.
    #[serde(default)]
    pub token_path: Option<PathBuf>,
}

/// Base directory for gateway data: `$XDG_DATA_HOME`, else `$HOME/.local/share`,
/// else the current directory.
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("mcpgw")
}

fn xdg_dir(var: &str, home_rel: &str) -> PathBuf {
    if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h).join(home_rel),
        None => PathBuf::from("."),
    }
}

impl GatewayConfig {
    /// Effective store path (configured or default).
    pub fn store_path(&self) -> PathBuf {
        self.store_path
            .clone()
            .unwrap_or_else(|| data_dir().join("mcpgw.db"))
    }

    /// Effective token file path (configured or default).
    pub fn token_path(&self) -> PathBuf {
        self.token_path
            .clone()
            .unwrap_or_else(|| data_dir().join("token"))
    }
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

    /// Load from `path` if it exists, otherwise return defaults.
    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        match std::fs::read_to_string(path.as_ref()) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Default config path: `$MCPGW_CONFIG`, else
    /// `$XDG_CONFIG_HOME/mcpgw/config.toml` (or `~/.config/...`).
    pub fn default_path() -> PathBuf {
        if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
            return PathBuf::from(p);
        }
        xdg_dir("XDG_CONFIG_HOME", ".config")
            .join("mcpgw")
            .join("config.toml")
    }

    /// Serialize to TOML.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))
    }

    /// Enabled servers only.
    pub fn enabled_servers(&self) -> impl Iterator<Item = &ServerSpec> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// Check invariants: every server valid, names unique.
    pub fn validate(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for s in &self.servers {
            s.validate()?;
            if !seen.insert(s.name.as_str()) {
                return Err(Error::Config(format!("duplicate server name: {}", s.name)));
            }
        }
        Ok(())
    }

    /// Look up a server by name.
    pub fn server(&self, name: &str) -> Result<&ServerSpec> {
        self.servers
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::UnknownServer(name.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcpgw_core::Transport;

    #[test]
    fn locked_defaults() {
        let c = Config::from_toml_str("").unwrap();
        assert!(!c.gateway.allow_agent_add_servers);
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
            url = "http://u"
            [[server]]
            name = "a"
            transport = "http"
            url = "http://v"
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

    #[test]
    fn enabled_env_and_paths() {
        let c = Config::from_toml_str(
            r#"
            [gateway]
            store_path = "/tmp/x.db"

            [[server]]
            name = "a"
            transport = "stdio"
            command = "run"
            env = { K = "V" }

            [[server]]
            name = "b"
            transport = "stdio"
            command = "run"
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(c.gateway.store_path(), PathBuf::from("/tmp/x.db"));
        assert!(c.gateway.token_path().ends_with("mcpgw/token"));
        let names: Vec<_> = c.enabled_servers().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a"]);
        let Transport::Stdio { env, .. } = &c.servers[0].transport else {
            panic!()
        };
        assert_eq!(env["K"], "V");
    }

    #[test]
    fn rejects_bad_url() {
        let s = r#"
            [[server]]
            name = "a"
            transport = "http"
            url = "ftp://x"
        "#;
        assert!(Config::from_toml_str(s).is_err());
    }

    #[test]
    fn load_or_default_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mcpgw-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Config::load_or_default(&p).unwrap(), Config::default());

        let mut c = Config::default();
        c.servers.push(ServerSpec::new(
            "fs",
            Transport::stdio("mcp-fs", vec!["--ro".into()]),
        ));
        std::fs::write(&p, c.to_toml_string().unwrap()).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
