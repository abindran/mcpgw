//! `mcpgw doctor` (M3-12): find and explain misconfigurations.
//!
//! Every check reports `ok`, `info`, `warn` or `fail` with what it found and,
//! for problems, how to fix it. The command fails when any check fails.
//! `--json` prints the checks for tools.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use mcpgw_config::{Config, clients::Client};
use mcpgw_core::{ServerSpec, Transport};
use mcpgw_store::{SqliteStore, Store, effective_servers};
use mcpgw_vault::{KeySource, Vault};
use secrecy::ExposeSecret;
use serde::Serialize;

use crate::{AnyResult, control::Control, daemon};

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Fine.
    Ok,
    /// Worth knowing.
    Info,
    /// Probably wrong.
    Warn,
    /// Broken.
    Fail,
}

/// One finding.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// Stable id, e.g. `secret_missing`.
    pub id: &'static str,
    /// What it's about: `server fs`, `vault`, `client cursor`.
    pub subject: String,
    /// Outcome.
    pub level: Level,
    /// What was found.
    pub message: String,
    /// How to fix it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Default)]
struct Report(Vec<Check>);

impl Report {
    fn add(
        &mut self,
        id: &'static str,
        subject: impl Into<String>,
        level: Level,
        message: impl Into<String>,
        fix: Option<String>,
    ) {
        self.0.push(Check {
            id,
            subject: subject.into(),
            level,
            message: message.into(),
            fix,
        });
    }
    fn ok(&mut self, id: &'static str, subject: impl Into<String>, message: impl Into<String>) {
        self.add(id, subject, Level::Ok, message, None);
    }
}

/// Run every check.
pub fn run(config_path: &Path, json: bool) -> AnyResult<()> {
    let mut r = Report::default();
    match Config::load_or_default(config_path) {
        Ok(config) => checks(&config, config_path, &mut r),
        Err(e) => r.add(
            "config_invalid",
            "config",
            Level::Fail,
            format!("{} can't be loaded: {e}", config_path.display()),
            Some(
                "fix the file (the message says where); the other checks run once it loads".into(),
            ),
        ),
    }
    clients(config_path, &mut r);
    print(&r, json)?;
    let fails = r.0.iter().filter(|c| c.level == Level::Fail).count();
    if fails > 0 {
        return Err(format!("{fails} problem(s) found").into());
    }
    Ok(())
}

fn print(r: &Report, json: bool) -> AnyResult<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&r.0)?);
        return Ok(());
    }
    for c in &r.0 {
        let tag = match c.level {
            Level::Ok => "  ok ",
            Level::Info => " info",
            Level::Warn => " warn",
            Level::Fail => " FAIL",
        };
        println!("{tag}  {}: {}", c.subject, c.message);
        if let Some(f) = &c.fix {
            println!("        fix: {f}");
        }
    }
    let n = |l| r.0.iter().filter(|c| c.level == l).count();
    println!(
        "{} problem(s), {} warning(s), {} ok",
        n(Level::Fail),
        n(Level::Warn),
        n(Level::Ok)
    );
    Ok(())
}

fn checks(config: &Config, path: &Path, r: &mut Report) {
    let store = match SqliteStore::open(config.gateway.store_path()) {
        Ok(s) => s,
        Err(e) => {
            r.add(
                "store_unreadable",
                "store",
                Level::Fail,
                format!("{}: {e}", config.gateway.store_path().display()),
                Some("check the path and its permissions (gateway.store_path)".into()),
            );
            return;
        }
    };
    let servers = effective_servers(&config.servers, &store).unwrap_or_default();
    r.ok(
        "config",
        "config",
        format!(
            "{} ({} servers, {} profiles)",
            path.display(),
            servers.len(),
            config.profiles.len()
        ),
    );
    let vault = vault(config, r);
    let secrets: Option<Vec<String>> = vault
        .as_ref()
        .map(|v| v.list().into_iter().map(|(n, ..)| n).collect());
    for s in &servers {
        server(s, config, secrets.as_deref(), r);
    }
    containers(&servers, r);
    daemon_and_port(config, r);
    token_file(config, r);
    if let Some(v) = &vault {
        oauth(v, r);
    }
    profiles(config, &store, &servers, r);
    otlp(config, r);
}

fn vault(config: &Config, r: &mut Report) -> Option<Vault> {
    let path = config.gateway.vault_path();
    let source = KeySource::from_env();
    let how = match &source {
        KeySource::Passphrase(_) => "passphrase from MCPGW_VAULT_PASSPHRASE",
        KeySource::Keychain => "key in the OS keychain",
    };
    if !path.exists() {
        r.add(
            "vault",
            "vault",
            Level::Info,
            format!(
                "no vault yet ({}); it's created by the first `mcpgw secret set`",
                path.display()
            ),
            None,
        );
        return None;
    }
    match Vault::open(&path, &source) {
        Ok(v) => {
            r.ok(
                "vault",
                "vault",
                format!(
                    "{} opens ({how}), {} secrets",
                    path.display(),
                    v.list().len()
                ),
            );
            Some(v)
        }
        Err(e) => {
            let fix = match source {
                KeySource::Passphrase(_) => {
                    "set MCPGW_VAULT_PASSPHRASE to the passphrase the vault was created with"
                }
                KeySource::Keychain => {
                    "unlock the keychain, or set MCPGW_VAULT_PASSPHRASE if the vault was created with a passphrase"
                }
            };
            r.add(
                "vault_unreadable",
                "vault",
                Level::Fail,
                format!("{} can't be opened with the {how}: {e}", path.display()),
                Some(fix.into()),
            );
            None
        }
    }
}

fn server(s: &ServerSpec, config: &Config, secrets: Option<&[String]>, r: &mut Report) {
    let subject = format!("server {}", s.name);
    if !s.enabled {
        r.add("server_disabled", &subject, Level::Info, "disabled", None);
        return;
    }
    if let Transport::Stdio { command, args, .. } = &s.transport {
        let cached = mcpgw_runtime::packages::detect(command, args)
            .and_then(|p| mcpgw_runtime::packages::installed(&config.gateway.runtimes_path(), &p));
        let runs = if cached.is_some() {
            "node"
        } else {
            command.as_str()
        };
        // a cached npx package runs on node; a cached uvx one needs nothing else
        let needs = match &cached {
            Some((_, i)) if i.kind == mcpgw_runtime::packages::Kind::Pypi => None,
            Some(_) => Some("node"),
            None => Some(runs),
        };
        if let Some(cmd) = needs
            && !mcpgw_core::on_path(cmd)
        {
            r.add(
                "command_missing",
                &subject,
                Level::Fail,
                format!("its command {cmd:?} isn't installed (not a file, not on PATH)"),
                Some(format!("install {cmd}, or point the server at the right program: `mcpgw add {} --command <path> -- …`", s.name)),
            );
        } else if let Some(p) = mcpgw_runtime::packages::detect(command, args)
            && cached.is_none()
        {
            r.add(
                "package_not_cached",
                &subject,
                Level::Info,
                format!(
                    "{} runs through {command} on each start (network, slower)",
                    p.spec
                ),
                Some(format!("`mcpgw runtimes install {}`", s.name)),
            );
        } else {
            r.ok("command", &subject, format!("{runs} found"));
        }
    }
    if let Some(have) = secrets {
        for name in s.secret_refs() {
            let name = name.replace(mcpgw_core::ACCOUNT_PLACEHOLDER, mcpgw_core::DEFAULT_ACCOUNT);
            if !have.contains(&name) {
                let fix = if let Some(rest) = name.strip_prefix("oauth/") {
                    let (p, a) = rest.split_once('@').unwrap_or((rest, "default"));
                    format!("`mcpgw auth connect {p} --account {a}`")
                } else {
                    format!("`mcpgw secret set {name}`")
                };
                r.add(
                    "secret_missing",
                    &subject,
                    Level::Fail,
                    format!(
                        "uses the secret {name:?}, which isn't in the vault, so it can't start"
                    ),
                    Some(fix),
                );
            }
        }
    } else if !s.secret_refs().is_empty() && !config.gateway.vault_path().exists() {
        r.add(
            "secret_missing",
            &subject,
            Level::Fail,
            format!(
                "uses secrets ({}) but there is no vault",
                s.secret_refs().into_iter().collect::<Vec<_>>().join(", ")
            ),
            Some("store them with `mcpgw secret set <name>`".into()),
        );
    }
}

fn containers(servers: &[ServerSpec], r: &mut Report) {
    let wanted: Vec<(&str, Option<&str>)> = servers
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| match &s.transport {
            Transport::Container { runtime, .. } => Some((s.name.as_str(), runtime.as_deref())),
            _ => None,
        })
        .collect();
    let mut seen = Vec::new();
    for (name, runtime) in wanted {
        let subject = format!("server {name}");
        let rt = match mcpgw_runtime::container::pick_runtime(runtime) {
            Ok(rt) => rt,
            Err(e) => {
                r.add(
                    "container_runtime_missing",
                    &subject,
                    Level::Fail,
                    format!("is a container server, but {e}"),
                    Some("install Docker or Podman (or set the server's `runtime`)".into()),
                );
                continue;
            }
        };
        if seen.contains(&rt) {
            continue;
        }
        seen.push(rt.clone());
        let o = std::process::Command::new(&rt)
            .args(["info", "--format", "{{.ServerVersion}}"])
            .stdin(std::process::Stdio::null())
            .output();
        match o {
            Ok(o) if o.status.success() => r.ok(
                "container_runtime",
                rt.clone(),
                format!("running ({})", String::from_utf8_lossy(&o.stdout).trim()),
            ),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let line = err
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                r.add("container_runtime_down", rt.clone(), Level::Fail, format!("installed but not answering: {line}"), Some(format!("start it (Docker Desktop, `colima start`, `podman machine start`, or the {rt} service)")));
            }
            Err(e) => r.add(
                "container_runtime_down",
                rt.clone(),
                Level::Fail,
                format!("can't run: {e}"),
                None,
            ),
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    }
}

fn daemon_and_port(config: &Config, r: &mut Report) {
    let info_path = daemon::info_path(config);
    if let Some(info) = daemon::running(config) {
        r.ok(
            "daemon",
            "daemon",
            format!("running (pid {}, {})", info.pid, info.url),
        );
        if let Ok(Some(c)) = Control::connect(config)
            && let Ok(st) = c.status()
        {
            for s in st.servers {
                let why = s.last_error.clone().unwrap_or_default();
                match s.state.as_str() {
                    "quarantined" => r.add(
                        "server_quarantined",
                        format!("server {}", s.name),
                        Level::Warn,
                        format!("quarantined: {why}"),
                        Some(format!(
                            "review its tools (`mcpgw scan {}`), then `mcpgw pin {}`",
                            s.name, s.name
                        )),
                    ),
                    "failed" | "backoff" => r.add(
                        "server_failing",
                        format!("server {}", s.name),
                        Level::Warn,
                        format!("{}: {why}", s.state),
                        Some(format!(
                            "see `mcpgw logs {}`; then `mcpgw restart {}`",
                            s.name, s.name
                        )),
                    ),
                    _ => {}
                }
            }
        }
        return;
    }
    if let Some(info) = daemon::DaemonInfo::read(&info_path) {
        let why = if pid_alive(info.pid) {
            format!(
                "process {} is alive but not answering at {}",
                info.pid, info.url
            )
        } else {
            format!("process {} is gone", info.pid)
        };
        r.add(
            "stale_daemon_file",
            "daemon",
            Level::Warn,
            format!(
                "{} describes a daemon that isn't running: {why}",
                info_path.display()
            ),
            Some(format!(
                "`mcpgw up` replaces it (or delete {})",
                info_path.display()
            )),
        );
    } else {
        r.add(
            "daemon",
            "daemon",
            Level::Info,
            "not running (`mcpgw up` starts it)",
            None,
        );
    }
    let addr = config.gateway.listen();
    if addr.port() != 0 {
        match std::net::TcpListener::bind(addr) {
            Ok(_) => r.ok("port", format!("listen {addr}"), "free"),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => r.add(
                "port_in_use",
                format!("listen {addr}"),
                Level::Fail,
                format!("port {} is taken by another program, so the daemon can't listen there", addr.port()),
                Some(format!("stop that program, or choose another port: `[gateway] listen = \"127.0.0.1:{}\"`", addr.port().wrapping_add(1).max(1024))),
            ),
            Err(e) => r.add("port_unusable", format!("listen {addr}"), Level::Fail, format!("can't listen: {e}"), Some("pick a loopback address you can bind (gateway.listen)".into())),
        }
    }
}

fn token_file(config: &Config, r: &mut Report) {
    let p = config.gateway.token_path();
    #[cfg(unix)]
    if let Ok(m) = std::fs::metadata(&p) {
        use std::os::unix::fs::PermissionsExt;
        let mode = m.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            r.add("token_permissions", "token", Level::Warn, format!("{} is readable by others (mode {mode:o}); anyone on this machine could use the gateway", p.display()), Some(format!("`chmod 600 {}` (or `mcpgw token --rotate`)", p.display())));
            return;
        }
        r.ok("token", "token", format!("{} (mode {mode:o})", p.display()));
    }
    #[cfg(not(unix))]
    let _ = (p, r);
}

fn oauth(v: &Vault, r: &mut Report) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for (name, ..) in v.list() {
        let Some(rest) = name.strip_prefix("oauth/") else {
            continue;
        };
        let (p, a) = rest.split_once('@').unwrap_or((rest, "default"));
        let subject = format!("login {p}@{a}");
        let Ok(Some(val)) = v.get(&name) else {
            continue;
        };
        let Ok(t) = serde_json::from_str::<mcpgw_auth::oauth::TokenSet>(val.expose_secret()) else {
            r.add(
                "oauth_unreadable",
                &subject,
                Level::Warn,
                "the stored login isn't a token set",
                Some(format!("`mcpgw auth connect {p} --account {a}`")),
            );
            continue;
        };
        let fix = Some(format!(
            "sign in again: `mcpgw auth connect {p} --account {a}`"
        ));
        if t.needs_reauth {
            r.add("oauth_login_needed", &subject, Level::Fail, "the provider refused its refresh token; calls through it fail until you sign in again", fix);
        } else if t.expires_at.is_some_and(|e| e <= now) && t.refresh_token.is_none() {
            r.add(
                "oauth_expired",
                &subject,
                Level::Fail,
                "the access token expired and there's no refresh token to renew it",
                fix,
            );
        } else {
            r.ok(
                "oauth",
                &subject,
                if t.refresh_token.is_some() {
                    "valid, refreshable"
                } else {
                    "valid"
                },
            );
        }
    }
}

fn profiles(config: &Config, store: &SqliteStore, servers: &[ServerSpec], r: &mut Report) {
    let mut all = config.profiles.clone();
    all.extend(store.profiles().unwrap_or_default());
    for p in all {
        for s in p
            .servers
            .iter()
            .filter(|s| !servers.iter().any(|x| &x.name == *s))
        {
            r.add(
                "profile_unknown_server",
                format!("profile {}", p.name),
                Level::Warn,
                format!("names server {s:?}, which doesn't exist; its endpoint just won't show it"),
                Some("add the server, or remove it from the profile's `servers`".into()),
            );
        }
    }
}

fn otlp(config: &Config, r: &mut Report) {
    let Some(ep) = &config.gateway.otlp_endpoint else {
        return;
    };
    let hostport = ep
        .split("://")
        .nth(1)
        .unwrap_or(ep)
        .split('/')
        .next()
        .unwrap_or("");
    let default_port = if ep.starts_with("https") { 443 } else { 80 };
    let target = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:{default_port}")
    };
    use std::net::ToSocketAddrs;
    let reachable = target
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(1)).is_ok());
    if reachable {
        r.ok("otlp", "otlp", format!("{ep} reachable"));
    } else {
        r.add("otlp_unreachable", "otlp", Level::Warn, format!("nothing answers at {ep}; traces, metrics and logs are dropped"), Some("start the collector (Jaeger, Grafana Alloy, an OTel Collector) or fix gateway.otlp_endpoint".into()));
    }
}

/// Is `entry` (a client's server entry) mcpgw itself?
fn is_mcpgw(entry: &serde_json::Value) -> bool {
    let cmd = entry["command"].as_str().unwrap_or_default();
    let base = Path::new(cmd)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    base == "mcpgw"
}

fn clients(config_path: &Path, r: &mut Report) {
    for client in Client::ALL {
        let Some(path) = client.user_config() else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        let subject = format!("client {}", client.name());
        let cfg = match mcpgw_config::clients::read(&path) {
            Ok(c) => c,
            Err(e) => {
                r.add(
                    "client_config_unreadable",
                    &subject,
                    Level::Warn,
                    format!("{} can't be read: {e}", path.display()),
                    Some("fix the JSON; the client can't read it either".into()),
                );
                continue;
            }
        };
        // every entry, mcpgw's own included (`clients::entries` skips it)
        let entries: Vec<(String, serde_json::Value)> = cfg
            .get(client.servers_key())
            .and_then(|v| v.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let ours: Vec<&(String, serde_json::Value)> =
            entries.iter().filter(|(_, e)| is_mcpgw(e)).collect();
        if ours.is_empty() {
            if !entries.is_empty() {
                r.add(
                    "client_not_using_mcpgw",
                    &subject,
                    Level::Info,
                    format!(
                        "{} has {} server(s) of its own, none through mcpgw",
                        path.display(),
                        entries.len()
                    ),
                    Some(format!(
                        "`mcpgw client import {}` moves them into mcpgw",
                        client.name()
                    )),
                );
            }
            continue;
        }
        for (name, e) in ours {
            let cmd = e["command"].as_str().unwrap_or_default();
            let p = PathBuf::from(cmd);
            if (p.is_absolute() && !p.is_file()) || (!p.is_absolute() && !mcpgw_core::on_path(cmd))
            {
                r.add(
                    "client_entry_broken",
                    &subject,
                    Level::Fail,
                    format!(
                        "its {name:?} entry in {} runs {cmd:?}, which doesn't exist",
                        path.display()
                    ),
                    Some(format!(
                        "`mcpgw client install {}` rewrites it with this mcpgw",
                        client.name()
                    )),
                );
                continue;
            }
            let args: Vec<&str> = e["args"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            if let Some(i) = args.iter().position(|a| *a == "--config")
                && let Some(c) = args.get(i + 1)
                && Path::new(c) != config_path
            {
                r.add(
                    "client_other_config",
                    &subject,
                    Level::Warn,
                    format!("its mcpgw entry uses another config ({c})"),
                    Some(format!(
                        "`mcpgw --config {} client install {}` if it should use this one",
                        config_path.display(),
                        client.name()
                    )),
                );
            } else {
                r.ok("client", &subject, format!("{} runs mcpgw", path.display()));
            }
        }
    }
}
