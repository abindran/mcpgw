//! `mcpgw-config` — gateway configuration loaded from TOML.
//!
//! Loading reports problems with file, line, column and a fix hint
//! ([`ConfigError`]), rejects unknown keys (including inside `[[server]]`),
//! and applies `MCPGW_<SECTION>__<KEY>` environment overrides.

pub mod clients;
mod error;
mod load;

pub use error::{ConfigError, Origin};
pub use load::{ENV_PREFIX, env_overrides};

use mcpgw_core::{Error, Result, ServerSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
    /// OAuth apps for `mcpgw auth connect`.
    #[serde(default, rename = "oauth", skip_serializing_if = "Vec::is_empty")]
    pub oauth: Vec<mcpgw_core::OAuthProvider>,
    /// Profiles: named endpoints at `/mcp/p/<name>` (M2-07).
    #[serde(default, rename = "profile", skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<mcpgw_core::Profile>,
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
    /// Path of the secret vault. Default: `vault.json` next to the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<PathBuf>,
    /// HTTPS URL of a client ID metadata document describing mcpgw, used as
    /// the OAuth client id for remote MCP servers that accept them
    /// (otherwise mcpgw registers itself dynamically).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_metadata_url: Option<String>,
    /// Address of the HTTP front door; must be a loopback address.
    /// Default: `127.0.0.1:7777`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<std::net::SocketAddr>,
    /// How tools are presented to clients: `meta` (default: only
    /// `search_tools`, `describe_tool`, `call_tool`), `direct` (every tool)
    /// or `hybrid` (meta-tools plus `pinned_tools`).
    #[serde(default)]
    pub mode: mcpgw_core::Mode,
    /// Account per server on the default endpoint, for servers whose
    /// secrets use `{account}`: `accounts = { github = "work" }`.
    /// Profiles (M2-07) set their own. Default: `default`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub accounts: std::collections::BTreeMap<String, String>,
    /// Public tool names always listed in `hybrid` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_tools: Vec<String>,
    /// How often the daemon checks for OAuth tokens to refresh.
    /// Default: `"60s"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_refresh_interval: Option<mcpgw_core::Duration>,
    /// Stop a server after this long without calls (per-server
    /// `idle_timeout` overrides it). Default: `"10m"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<mcpgw_core::Duration>,
    /// Most servers running at once; starting one more stops the least
    /// recently used idle server. Default: 16.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_running: Option<usize>,
    /// Extra browser origins allowed to call the daemon, e.g.
    /// `"http://localhost:5173"`. The daemon's own origin is always allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
}

/// Default HTTP front door address.
pub const DEFAULT_LISTEN: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 7777);

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

    /// Effective vault path (configured or next to the store).
    pub fn vault_path(&self) -> PathBuf {
        self.vault_path
            .clone()
            .unwrap_or_else(|| self.store_path().with_file_name("vault.json"))
    }

    /// Effective idle timeout (configured or 10 minutes).
    pub fn idle_timeout(&self) -> std::time::Duration {
        self.idle_timeout
            .map(|d| d.get())
            .unwrap_or(std::time::Duration::from_secs(600))
    }

    /// Effective OAuth refresh check interval (configured or 60 s).
    pub fn oauth_refresh_interval(&self) -> std::time::Duration {
        self.oauth_refresh_interval
            .map(|d| d.get())
            .unwrap_or(std::time::Duration::from_secs(60))
    }

    /// Effective running-server cap (configured or 16).
    pub fn max_running(&self) -> usize {
        self.max_running.unwrap_or(16)
    }

    /// Effective listen address (configured or default).
    pub fn listen(&self) -> std::net::SocketAddr {
        self.listen.unwrap_or(DEFAULT_LISTEN)
    }

    /// Effective token file path (configured or default).
    pub fn token_path(&self) -> PathBuf {
        self.token_path
            .clone()
            .unwrap_or_else(|| data_dir().join("token"))
    }
}

impl Config {
    /// Parse and validate TOML text (no environment overrides).
    pub fn from_toml_str(s: &str) -> Result<Self> {
        Ok(Self::parse(s, Path::new(""), [])?)
    }

    /// Parse and validate `src` read from `path`, then apply `env`
    /// overrides (`(name, value)` pairs, normally from [`env_overrides`]).
    pub fn parse(
        src: &str,
        path: &Path,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> std::result::Result<Self, ConfigError> {
        load::parse(src, path, env.into_iter().collect())
    }

    /// Load a file and apply overrides from the process environment.
    pub fn load(path: impl AsRef<Path>) -> std::result::Result<Self, ConfigError> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path).map_err(|e| {
            ConfigError::new(Origin::File(path.into()), format!("cannot read: {e}"))
        })?;
        Self::parse(&src, path, env_overrides())
    }

    /// Like [`Config::load`], but a missing file means defaults (still with
    /// environment overrides).
    pub fn load_or_default(path: impl AsRef<Path>) -> std::result::Result<Self, ConfigError> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Self::parse("", path, env_overrides())
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
    fn parses_servers_and_paths() {
        let c = Config::from_toml_str(
            r#"
            [gateway]
            store_path = "/tmp/x.db"

            [[server]]
            name = "fs"
            transport = "stdio"
            command = "mcp-fs"
            args = ["--ro"]
            env = { K = "V" }

            [[server]]
            name = "web"
            transport = "http"
            url = "https://example.com/mcp"
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(c.servers.len(), 2);
        assert!(matches!(
            c.server("web").unwrap().transport,
            Transport::Http { .. }
        ));
        assert!(matches!(c.server("nope"), Err(Error::UnknownServer(_))));
        assert_eq!(c.gateway.store_path(), PathBuf::from("/tmp/x.db"));
        assert!(c.gateway.token_path().ends_with("mcpgw/token"));
        let names: Vec<_> = c.enabled_servers().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fs"]);
    }

    #[test]
    fn sample_config_loads() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/config.toml");
        let c = Config::parse(&std::fs::read_to_string(&path).unwrap(), &path, []).unwrap();
        assert!(c.servers.len() >= 3, "{c:?}");
    }

    #[test]
    fn load_or_default_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mcpgw-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Config::parse("", &p, []).unwrap(), Config::default());

        let mut c = Config::default();
        c.servers.push(ServerSpec::new(
            "fs",
            Transport::stdio("mcp-fs", vec!["--ro".into()]),
        ));
        std::fs::write(&p, c.to_toml_string().unwrap()).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(Config::parse(&text, &p, []).unwrap(), c);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
