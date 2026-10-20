//! Parsing: syntax and type errors with positions, unknown-key checks inside
//! `[[server]]`, semantic validation, and environment overrides.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

use mcpgw_core::{ServerSpec, Transport, validate_server_name};
use serde::Deserialize;
use toml::Spanned;

use crate::{Config, ConfigError, Origin, error::did_you_mean};

/// Prefix of override variables: `MCPGW_GATEWAY__STORE_PATH=/x.db`.
pub const ENV_PREFIX: &str = "MCPGW_";

/// Sections an environment variable may override.
const ENV_SECTIONS: &[&str] = &["gateway"];

/// `MCPGW_<SECTION>__<KEY>` variables from the process environment.
/// Other `MCPGW_*` variables (`MCPGW_CONFIG`, `MCPGW_LOG`, ...) are ignored.
pub fn env_overrides() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with(ENV_PREFIX) && k.contains("__"))
        .collect()
}

type RawTable = BTreeMap<Spanned<String>, Spanned<toml::Value>>;

/// The file with spans kept, used to point errors at keys and values.
#[derive(Deserialize, Default)]
struct Located {
    #[serde(default)]
    server: Vec<Spanned<RawTable>>,
}

impl Located {
    fn server_key(&self, i: usize, key: &str) -> Option<usize> {
        let t = self.server.get(i)?;
        match t.get_ref().get(key) {
            Some(v) => Some(v.span().start),
            None => Some(t.span().start),
        }
    }
}

pub(crate) fn parse(
    src: &str,
    path: &Path,
    env: Vec<(String, String)>,
) -> Result<Config, ConfigError> {
    let origin = Origin::File(path.to_path_buf());
    let err = |msg: String| ConfigError::new(origin.clone(), msg);

    // Unknown keys inside [[server]] first: a typo like `comand` would
    // otherwise surface as a less helpful "missing field `command`".
    let located: Located = toml::from_str(src).unwrap_or_default();
    check_server_keys(src, &located, &err)?;

    // Syntax and types; the toml error carries a span.
    let mut cfg: Config = toml::from_str(src).map_err(|e| {
        let (msg, hint) = explain(e.message());
        let mut ce = err(msg).at(src, e.span().map(|r| r.start));
        if let Some(h) = hint {
            ce = ce.hint(h);
        }
        ce
    })?;
    validate(&cfg).map_err(|issue| {
        let offset = issue
            .server
            .and_then(|i| located.server_key(i, issue.field));
        let mut ce = err(issue.message).at(src, offset);
        if let Some(h) = issue.hint {
            ce = ce.hint(h);
        }
        ce
    })?;

    if !env.is_empty() {
        cfg = apply_env(src, env)?;
    }
    Ok(cfg)
}

/// Turn serde/toml messages into a message plus a "did you mean" hint.
fn explain(msg: &str) -> (String, Option<String>) {
    let msg = msg.trim().to_string();
    // "unknown field `x`, expected one of `a`, `b`"  /  "expected `a` or `b`"
    if let Some(rest) = msg.strip_prefix("unknown field `") {
        let (field, tail) = rest.split_once('`').unwrap_or((rest, ""));
        let expected: Vec<&str> = tail.split('`').skip(1).step_by(2).collect();
        let hint = match did_you_mean(field, &expected) {
            Some(c) => format!("did you mean `{c}`?"),
            None if !expected.is_empty() => format!("valid keys: {}", expected.join(", ")),
            None => "remove it".into(),
        };
        return (format!("unknown key `{field}`"), Some(hint));
    }
    if let Some(rest) = msg.strip_prefix("missing field `") {
        let field = rest.split('`').next().unwrap_or(rest);
        return (
            msg.clone(),
            Some(format!("add `{field} = ...` to this table")),
        );
    }
    if msg.starts_with("unknown variant") {
        return (msg, Some("check the spelling of the value".into()));
    }
    if msg.contains("invalid duration") {
        return (
            msg,
            Some("durations look like \"30s\", \"10m\" or \"2h\"".into()),
        );
    }
    (msg, None)
}

/// Keys a `[[server]]` table may contain, read from the `ServerSpec` schema
/// (so new fields are picked up automatically), per transport.
fn server_keys() -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let schema = serde_json::to_value(schemars::schema_for!(ServerSpec)).expect("schema");
    let props = |v: &serde_json::Value| -> BTreeSet<String> {
        v.get("properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let common = props(&schema);
    let mut per = BTreeMap::new();
    let variants = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for v in &variants {
        let keys = props(v);
        let tag = v["properties"]["transport"]
            .get("const")
            .or_else(|| v["properties"]["transport"]["enum"].get(0))
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        per.insert(tag, keys);
    }
    (common, per)
}

fn check_server_keys(
    src: &str,
    located: &Located,
    err: &impl Fn(String) -> ConfigError,
) -> Result<(), ConfigError> {
    let (common, per) = server_keys();
    for table in &located.server {
        let t = table.get_ref();
        let Some(transport) = t.get("transport").and_then(|v| v.get_ref().as_str()) else {
            continue; // serde already reported a missing/invalid transport
        };
        let Some(specific) = per.get(transport) else {
            continue;
        };
        let allowed: Vec<&str> = common.iter().chain(specific).map(String::as_str).collect();
        for key in t.keys() {
            if !allowed.contains(&key.get_ref().as_str()) {
                let hint = match did_you_mean(key.get_ref(), &allowed) {
                    Some(c) => format!("did you mean `{c}`?"),
                    None => format!(
                        "valid keys for a {transport} server: {}",
                        allowed.join(", ")
                    ),
                };
                return Err(
                    err(format!("unknown key `{}` in [[server]]", key.get_ref()))
                        .at(src, Some(key.span().start))
                        .hint(hint),
                );
            }
        }
    }
    Ok(())
}

struct Issue {
    server: Option<usize>,
    field: &'static str,
    message: String,
    hint: Option<String>,
}

fn validate(cfg: &Config) -> Result<(), Issue> {
    if let Some(addr) = cfg.gateway.listen
        && !addr.ip().is_loopback()
    {
        return Err(Issue {
            server: None,
            field: "listen",
            message: format!("gateway.listen {addr} is not a loopback address"),
            hint: Some("mcpgw only listens locally; use 127.0.0.1:<port> or [::1]:<port>".into()),
        });
    }
    if cfg.gateway.max_running == Some(0) {
        return Err(Issue {
            server: None,
            field: "max_running",
            message: "gateway.max_running must be at least 1".into(),
            hint: Some("remove it for the default of 16".into()),
        });
    }
    for o in &cfg.gateway.allowed_origins {
        if !(o.starts_with("http://") || o.starts_with("https://")) || o.ends_with('/') {
            return Err(Issue {
                server: None,
                field: "allowed_origins",
                message: format!("allowed origin {o:?} is not an origin"),
                hint: Some(
                    "write scheme://host[:port] with no path, e.g. \"http://localhost:5173\""
                        .into(),
                ),
            });
        }
    }
    let mut seen = HashSet::new();
    let mut prefixes = std::collections::HashMap::new();
    for (i, s) in cfg.servers.iter().enumerate() {
        if let Some(other) = prefixes.insert(s.prefix().to_string(), s.name.clone()) {
            return Err(Issue {
                server: Some(i),
                field: if s.alias.is_some() { "alias" } else { "name" },
                message: format!(
                    "tool prefix `{}` of server `{}` is already used by server `{other}`",
                    s.prefix(),
                    s.name
                ),
                hint: Some("give one of them a different alias".into()),
            });
        }
        let issue = |field, message: String, hint: &str| Issue {
            server: Some(i),
            field,
            message,
            hint: Some(hint.into()),
        };
        if let Err(e) = validate_server_name(&s.name) {
            return Err(issue(
                "name",
                strip(e),
                "use 1-64 letters, digits, '-' or '_', without \"__\"",
            ));
        }
        if !seen.insert(s.name.as_str()) {
            return Err(issue(
                "name",
                format!("duplicate server name `{}`", s.name),
                "server names must be unique",
            ));
        }
        match &s.transport {
            Transport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(issue(
                    "command",
                    format!("server `{}` has an empty command", s.name),
                    "set command to the executable to run, e.g. \"npx\"",
                ));
            }
            Transport::Http { url, .. }
                if !(url.starts_with("http://") || url.starts_with("https://")) =>
            {
                return Err(issue(
                    "url",
                    format!(
                        "server `{}` url must start with http:// or https://",
                        s.name
                    ),
                    "use the server's full MCP endpoint, e.g. \"https://example.com/mcp\"",
                ));
            }
            _ => {}
        }
        // Anything else ServerSpec itself enforces.
        s.validate()
            .map_err(|e| issue("name", strip(e), "see the message"))?;
    }
    Ok(())
}

fn strip(e: mcpgw_core::Error) -> String {
    let s = e.to_string();
    s.strip_prefix("configuration error: ")
        .unwrap_or(&s)
        .to_string()
}

/// Re-parse with `MCPGW_<SECTION>__<KEY>` overrides merged in. Errors name
/// the variable responsible.
fn apply_env(src: &str, env: Vec<(String, String)>) -> Result<Config, ConfigError> {
    let base: toml::Table = toml::from_str(src).expect("already parsed");
    let mut merged = base.clone();
    for (var, raw) in &env {
        set_override(&mut merged, var, raw)?;
    }
    match finish(merged) {
        Ok(c) => Ok(c),
        Err(msg) => {
            // Find the variable that breaks it on its own.
            for (var, raw) in &env {
                let mut one = base.clone();
                set_override(&mut one, var, raw)?;
                if let Err(m) = finish(one) {
                    let (m, hint) = explain(&m);
                    let mut e = ConfigError::new(Origin::Env(var.clone()), m);
                    if let Some(h) = hint {
                        e = e.hint(h);
                    }
                    return Err(e);
                }
            }
            Err(ConfigError::new(Origin::Env("MCPGW_*".into()), msg))
        }
    }
}

fn finish(table: toml::Table) -> Result<Config, String> {
    let cfg = Config::deserialize(toml::Value::Table(table)).map_err(|e| e.to_string())?;
    validate(&cfg).map_err(|i| i.message)?;
    Ok(cfg)
}

fn set_override(table: &mut toml::Table, var: &str, raw: &str) -> Result<(), ConfigError> {
    let origin = || Origin::Env(var.to_string());
    let rest = &var[ENV_PREFIX.len()..];
    let (section, key) = rest.split_once("__").expect("filtered on __");
    let section = section.to_ascii_lowercase();
    let key = key.to_ascii_lowercase();
    if !ENV_SECTIONS.contains(&section.as_str()) {
        let mut e = ConfigError::new(origin(), format!("unknown config section `{section}`"));
        e = e.hint(match did_you_mean(&section, ENV_SECTIONS) {
            Some(c) => format!(
                "did you mean MCPGW_{}__{}?",
                c.to_ascii_uppercase(),
                key.to_ascii_uppercase()
            ),
            None => format!(
                "sections that can be overridden: {}",
                ENV_SECTIONS.join(", ")
            ),
        });
        return Err(e);
    }
    // TOML literal if it parses (true, 30, "x", [..]); otherwise a string.
    let value = toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| toml::Value::String(raw.to_string()));
    let entry = table
        .entry(section)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    match entry {
        toml::Value::Table(t) => {
            t.insert(key, value);
            Ok(())
        }
        _ => Err(ConfigError::new(origin(), "section is not a table")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_err(src: &str) -> ConfigError {
        parse(src, Path::new("c.toml"), vec![]).unwrap_err()
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn typo_in_server_key_has_line_and_hint() {
        let e = parse_err("[[server]]\nname = \"gh\"\ntransport = \"stdio\"\ncomand = \"x\"\n");
        assert_eq!(e.position, Some((4, 1)));
        assert_eq!(e.message, "unknown key `comand` in [[server]]");
        assert_eq!(e.hint.as_deref(), Some("did you mean `command`?"));
        assert_eq!(
            e.to_string(),
            "c.toml:4:1: unknown key `comand` in [[server]]\n  hint: did you mean `command`?"
        );
    }

    #[test]
    fn transport_specific_keys() {
        // `url` is not valid on a stdio server
        let e = parse_err(
            "[[server]]\nname = \"a\"\ntransport = \"stdio\"\ncommand = \"x\"\nurl = \"http://u\"\n",
        );
        assert_eq!(e.position, Some((5, 1)));
        assert!(e.message.contains("`url`"));
    }

    #[test]
    fn top_level_typo_and_syntax_errors() {
        let e = parse_err("[gateway]\nallow_agent_add_server = true\n");
        assert_eq!(e.position, Some((2, 1)), "{e}");
        assert_eq!(
            e.hint.as_deref(),
            Some("did you mean `allow_agent_add_servers`?")
        );
        let e = parse_err("[[server]\n");
        assert_eq!(e.position.map(|p| p.0), Some(1), "{e}");
        let e = parse_err(
            "[[server]]\nname = \"a\"\ntransport = \"stdio\"\ncommand = \"x\"\nidle_timeout = \"5 minutes\"\n",
        );
        assert_eq!(e.position.map(|p| p.0), Some(5), "{e}");
        assert!(e.hint.unwrap().contains("10m"));
    }

    #[test]
    fn semantic_errors_point_at_the_value() {
        let e = parse_err("[[server]]\nname = \"a__b\"\ntransport = \"stdio\"\ncommand = \"x\"\n");
        assert_eq!(e.position, Some((2, 8)));
        assert!(e.hint.unwrap().contains("letters"));
        let src = "[[server]]\nname = \"a\"\ntransport = \"http\"\nurl = \"http://u\"\n\n[[server]]\nname = \"a\"\ntransport = \"http\"\nurl = \"http://v\"\n";
        let e = parse_err(src);
        assert_eq!(e.position, Some((7, 8)));
        assert!(e.message.contains("duplicate"));
        let e = parse_err("[[server]]\nname = \"w\"\ntransport = \"http\"\nurl = \"ftp://x\"\n");
        assert_eq!(e.position, Some((4, 7)));
    }

    #[test]
    fn listen_must_be_loopback() {
        let e = parse_err("[gateway]\nlisten = \"0.0.0.0:7777\"\n");
        assert!(e.message.contains("not a loopback"), "{e}");
        let c = parse(
            "[gateway]\nlisten = \"127.0.0.1:9000\"\n",
            Path::new("c"),
            vec![],
        )
        .unwrap();
        assert_eq!(c.gateway.listen().port(), 9000);
        assert_eq!(
            Config::default().gateway.listen().to_string(),
            "127.0.0.1:7777"
        );
        let e = parse_err("[gateway]\nallowed_origins = [\"localhost:5173\"]\n");
        assert!(e.message.contains("not an origin"), "{e}");
    }

    #[test]
    fn env_overrides_apply_and_report() {
        let c = parse(
            "[gateway]\nstore_path = \"/a.db\"\n",
            Path::new("c.toml"),
            env(&[
                ("MCPGW_GATEWAY__STORE_PATH", "/b.db"),
                ("MCPGW_GATEWAY__ALLOW_AGENT_ADD_SERVERS", "true"),
            ]),
        )
        .unwrap();
        assert_eq!(c.gateway.store_path(), Path::new("/b.db"));
        assert!(c.gateway.allow_agent_add_servers);

        let e = parse(
            "",
            Path::new("c.toml"),
            env(&[("MCPGW_GATEWAY__STORE_PTH", "/x")]),
        )
        .unwrap_err();
        assert_eq!(e.origin, Origin::Env("MCPGW_GATEWAY__STORE_PTH".into()));
        assert_eq!(e.hint.as_deref(), Some("did you mean `store_path`?"));

        let e = parse(
            "",
            Path::new("c.toml"),
            env(&[("MCPGW_GATEWY__STORE_PATH", "/x")]),
        )
        .unwrap_err();
        assert_eq!(
            e.hint.as_deref(),
            Some("did you mean MCPGW_GATEWAY__STORE_PATH?")
        );

        let e = parse(
            "",
            Path::new("c.toml"),
            env(&[("MCPGW_GATEWAY__ALLOW_AGENT_ADD_SERVERS", "yes")]),
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .starts_with("environment variable MCPGW_GATEWAY__ALLOW_AGENT_ADD_SERVERS"),
            "{e}"
        );
    }
}
