//! MCP client configs (M1-14): read the servers a client is configured
//! with, convert them into mcpgw servers, and write the one gateway entry
//! that replaces them.
//!
//! | Client | User config | Servers live under |
//! |--------|-------------|--------------------|
//! | Claude Code | `~/.claude.json` (project: `.mcp.json`) | `mcpServers` |
//! | Claude Desktop | `…/Claude/claude_desktop_config.json` | `mcpServers` |
//! | Cursor | `~/.cursor/mcp.json` (project: `.cursor/mcp.json`) | `mcpServers` |
//! | VS Code | `…/Code/User/mcp.json` (workspace: `.vscode/mcp.json`) | `servers` |

use std::path::{Path, PathBuf};

use mcpgw_core::{ServerSpec, Transport};
use serde_json::{Map, Value, json};

/// Name of the gateway's own entry in client configs.
pub const GATEWAY_ENTRY: &str = "mcpgw";

/// A supported MCP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// Claude Code (CLI).
    ClaudeCode,
    /// Claude Desktop.
    ClaudeDesktop,
    /// Cursor.
    Cursor,
    /// Visual Studio Code.
    VsCode,
}

impl std::str::FromStr for Client {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "claude-code" | "claude" => Ok(Self::ClaudeCode),
            "claude-desktop" => Ok(Self::ClaudeDesktop),
            "cursor" => Ok(Self::Cursor),
            "vscode" | "vs-code" | "code" => Ok(Self::VsCode),
            other => Err(format!(
                "unknown client {other:?}; use claude-code, claude-desktop, cursor or vscode"
            )),
        }
    }
}

impl Client {
    /// Every client.
    pub const ALL: [Client; 4] = [
        Self::ClaudeCode,
        Self::ClaudeDesktop,
        Self::Cursor,
        Self::VsCode,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Cursor => "cursor",
            Self::VsCode => "vscode",
        }
    }

    /// The key holding the server map.
    pub fn servers_key(self) -> &'static str {
        match self {
            Self::VsCode => "servers",
            _ => "mcpServers",
        }
    }

    /// The user-level config file, from `HOME` (and `APPDATA` on Windows).
    pub fn user_config(self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        let app_support = |app: &str| -> PathBuf {
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support").join(app)
            } else if cfg!(windows) {
                appdata
                    .clone()
                    .unwrap_or_else(|| home.join("AppData/Roaming"))
                    .join(app)
            } else {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"))
                    .join(app)
            }
        };
        Some(match self {
            Self::ClaudeCode => home.join(".claude.json"),
            Self::ClaudeDesktop => app_support("Claude").join("claude_desktop_config.json"),
            Self::Cursor => home.join(".cursor/mcp.json"),
            Self::VsCode => app_support("Code/User").join("mcp.json"),
        })
    }

    /// The gateway entry for this client: `mcpgw connect` over stdio.
    pub fn gateway_entry(self, exe: &str, config: Option<&Path>, profile: Option<&str>) -> Value {
        let mut args = Vec::new();
        if let Some(c) = config {
            args.push(Value::String("--config".into()));
            args.push(Value::String(c.to_string_lossy().into_owned()));
        }
        args.push(Value::String("connect".into()));
        if let Some(p) = profile {
            args.push(Value::String("--profile".into()));
            args.push(Value::String(p.into()));
        }
        match self {
            Self::VsCode => json!({"type": "stdio", "command": exe, "args": args}),
            _ => json!({"command": exe, "args": args}),
        }
    }
}

/// Remove `//` and `/* */` comments and trailing commas (JSONC, used by
/// VS Code), leaving strings untouched.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = next {
                    out.push(n);
                    i += 1;
                }
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
        } else if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            // drop a comma that only precedes whitespace and a closing bracket
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if !matches!(chars.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Parse a client config file (JSON or JSONC). A missing file is `{}`.
pub fn read(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&strip_jsonc(&t))
            .map_err(|e| format!("{}: not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The client's server entries, without the gateway's own.
pub fn entries(client: Client, config: &Value) -> Vec<(String, Value)> {
    config
        .get(client.servers_key())
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.as_str() != GATEWAY_ENTRY)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// A valid mcpgw server name for a client's entry name.
pub fn sanitize_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches(|c| c == '_' || c == '-').to_string();
    let s: String = s.chars().take(64).collect();
    if s.is_empty() { "server".into() } else { s }
}

fn string_map(v: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Convert one client entry into a server spec.
pub fn to_spec(name: &str, entry: &Value) -> Result<ServerSpec, String> {
    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
    let transport = if let Some(cmd) = entry.get("command").and_then(Value::as_str) {
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Transport::Stdio {
            command: cmd.to_string(),
            args,
            env: string_map(entry.get("env")),
            cwd: entry.get("cwd").and_then(Value::as_str).map(PathBuf::from),
        }
    } else if let Some(url) = entry.get("url").and_then(Value::as_str) {
        if kind == "sse" {
            tracing::debug!(
                name,
                "legacy SSE server imported as http (bridged from M2-04)"
            );
        }
        Transport::Http {
            url: url.to_string(),
            headers: string_map(entry.get("headers")),
        }
    } else {
        return Err(format!("server {name:?} has neither a command nor a url"));
    };
    let mut spec = ServerSpec::new(sanitize_name(name), transport);
    if entry.get("disabled").and_then(Value::as_bool) == Some(true) {
        spec.enabled = false;
    }
    Ok(spec)
}

/// Whether an environment variable or header name looks like it holds a
/// credential.
pub fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "AUTHORIZATION",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
    ]
    .iter()
    .any(|n| k.contains(n))
}

/// Move credential-looking env values out of `spec` into secret bindings.
/// Returns `(secret name, value)` for the vault. Values that are already
/// `${secret:…}` placeholders or empty are left alone.
pub fn extract_secrets(spec: &mut ServerSpec) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let server = spec.name.to_ascii_lowercase().replace('-', "_");
    if let Transport::Stdio { env, .. } = &mut spec.transport {
        let keys: Vec<String> = env.keys().filter(|k| looks_secret(k)).cloned().collect();
        for k in keys {
            let v = env.remove(&k).unwrap_or_default();
            if v.is_empty() || v.contains(mcpgw_core::SECRET_PLACEHOLDER) {
                env.insert(k, v);
                continue;
            }
            let secret = format!("{server}_{}", k.to_ascii_lowercase());
            spec.secrets.insert(k, secret.clone());
            out.push((secret, v));
        }
    }
    if let Transport::Http { headers, .. } = &mut spec.transport {
        for (k, v) in headers.iter_mut() {
            if looks_secret(k) && !v.is_empty() && !v.contains(mcpgw_core::SECRET_PLACEHOLDER) {
                let secret = format!("{server}_{}", k.to_ascii_lowercase().replace('-', "_"));
                out.push((secret.clone(), std::mem::take(v)));
                *v = format!("${{secret:{secret}}}");
            }
        }
    }
    out
}

/// Set the client's server map to exactly the gateway entry, keeping every
/// other key in the file.
pub fn replace_with_gateway(client: Client, config: &mut Value, entry: Value) {
    let obj = ensure_object(config);
    let mut servers = Map::new();
    servers.insert(GATEWAY_ENTRY.into(), entry);
    obj.insert(client.servers_key().into(), Value::Object(servers));
}

/// Add or replace the gateway entry, keeping the client's other servers.
pub fn install_gateway(client: Client, config: &mut Value, entry: Value) {
    let obj = ensure_object(config);
    let servers = obj
        .entry(client.servers_key())
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    servers
        .as_object_mut()
        .expect("object")
        .insert(GATEWAY_ENTRY.into(), entry);
}

fn ensure_object(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().expect("object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_and_trailing_commas() {
        let src = r#"{
            // a comment with "quotes"
            "servers": { /* inline */ "a": {"command": "x // not a comment", "args": ["/*no*/",],}, },
            "é": "ü"
        }"#;
        let v: Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["servers"]["a"]["command"], "x // not a comment");
        assert_eq!(v["servers"]["a"]["args"][0], "/*no*/");
        assert_eq!(v["é"], "ü");
    }

    #[test]
    fn names_and_specs() {
        assert_eq!(sanitize_name("github"), "github");
        assert_eq!(sanitize_name("my server.v2"), "my-server-v2");
        assert_eq!(sanitize_name("__a__b__"), "a_b");
        assert_eq!(sanitize_name("@scope/pkg"), "scope-pkg");
        let s = to_spec(
            "fs",
            &json!({"command": "npx", "args": ["-y", "pkg"], "env": {"A": "1"}}),
        )
        .unwrap();
        assert!(
            matches!(&s.transport, Transport::Stdio { command, args, .. } if command == "npx" && args.len() == 2)
        );
        let h = to_spec(
            "remote",
            &json!({"type": "http", "url": "https://x/mcp", "headers": {"X": "y"}}),
        )
        .unwrap();
        assert!(matches!(h.transport, Transport::Http { .. }));
        let d = to_spec("off", &json!({"command": "x", "disabled": true})).unwrap();
        assert!(!d.enabled);
        assert!(to_spec("bad", &json!({"foo": 1})).is_err());
    }

    #[test]
    fn credentials_move_to_bindings() {
        let mut s = to_spec(
            "git-hub",
            &json!({"command": "gh", "env": {"GITHUB_TOKEN": "ghp_x", "LOG": "debug", "API_KEY": "${secret:k}"}}),
        )
        .unwrap();
        let moved = extract_secrets(&mut s);
        assert_eq!(
            moved,
            [("git_hub_github_token".to_string(), "ghp_x".to_string())]
        );
        assert_eq!(s.secrets["GITHUB_TOKEN"], "git_hub_github_token");
        let Transport::Stdio { env, .. } = &s.transport else {
            panic!()
        };
        assert_eq!(env.get("LOG").map(String::as_str), Some("debug"));
        assert!(!env.contains_key("GITHUB_TOKEN"));
        assert_eq!(env["API_KEY"], "${secret:k}", "placeholders stay");

        let mut h = to_spec(
            "r",
            &json!({"url": "https://x", "headers": {"Authorization": "Bearer abc"}}),
        )
        .unwrap();
        let moved = extract_secrets(&mut h);
        assert_eq!(
            moved[0],
            ("r_authorization".to_string(), "Bearer abc".to_string())
        );
        let Transport::Http { headers, .. } = &h.transport else {
            panic!()
        };
        assert_eq!(headers["Authorization"], "${secret:r_authorization}");
    }

    #[test]
    fn gateway_entry_install_and_replace() {
        let mut cfg = json!({"theme": "dark", "mcpServers": {"fs": {"command": "x"}}});
        let entry = Client::Cursor.gateway_entry("/bin/mcpgw", None, None);
        assert_eq!(entry, json!({"command": "/bin/mcpgw", "args": ["connect"]}));
        install_gateway(Client::Cursor, &mut cfg, entry.clone());
        assert_eq!(
            entries(Client::Cursor, &cfg).len(),
            1,
            "gateway entry is not listed"
        );
        replace_with_gateway(Client::Cursor, &mut cfg, entry);
        assert_eq!(cfg["theme"], "dark");
        assert_eq!(cfg["mcpServers"].as_object().unwrap().len(), 1);
        let vs =
            Client::VsCode.gateway_entry("mcpgw", Some(Path::new("/c.toml")), Some("frontend"));
        assert_eq!(
            vs,
            json!({"type": "stdio", "command": "mcpgw", "args": ["--config", "/c.toml", "connect", "--profile", "frontend"]})
        );
        assert_eq!("VS-Code".parse::<Client>(), Ok(Client::VsCode));
    }
}
