//! `mcpgw dev ./my-server` (M4-10): run one server under a throwaway
//! gateway while you work on it.
//!
//! - **Hot reload.** The server's files are watched; an edit restarts the
//!   server, and the next call runs the new code. Changed tools are
//!   accepted (no quarantine in dev) and the change is printed.
//! - **Request log.** Every call is printed: tool, arguments, outcome,
//!   time, and the start of the result.
//! - **Inspector.** The web UI's Inspector lists the tools and calls them
//!   with arguments you type (`POST /api/v1/tools/{name}/call`).
//!
//! The gateway is private to the session: its store, token and daemon file
//! live in a temporary directory, so `mcpgw dev` never touches your
//! servers or a running daemon. Secrets still come from your vault.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::BackendManager;

use crate::AnyResult;

/// What `mcpgw dev` was asked to run.
pub struct DevArgs {
    /// The server: a directory, a `.wasm` component or an OpenAPI spec.
    pub path: Option<PathBuf>,
    /// Server name (default: from the path).
    pub name: Option<String>,
    /// Port for the gateway (default: any free one).
    pub port: u16,
    /// More files or directories to watch.
    pub watch: Vec<PathBuf>,
    /// Environment for the server.
    pub env: Vec<(String, String)>,
    /// For an OpenAPI spec: where requests go.
    pub base_url: Option<String>,
    /// How to run it, when it isn't detected.
    pub command: Vec<String>,
}

/// A name for a server from its path.
fn name_from(path: &Path) -> String {
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dev".into());
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        "dev".into()
    } else {
        clean
    }
}

/// How to run a directory: `node <main>` (package.json), `python3
/// server.py|main.py`, or `cargo run --quiet` (Cargo.toml).
fn detect(dir: &Path) -> Option<(String, Vec<String>)> {
    if let Ok(text) = std::fs::read_to_string(dir.join("package.json")) {
        let pkg: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let main = match &pkg["bin"] {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => {
                o.values().next().and_then(|v| v.as_str()).map(String::from)
            }
            _ => None,
        }
        .or_else(|| pkg["main"].as_str().map(String::from))
        .unwrap_or_else(|| "index.js".into());
        return Some(("node".into(), vec![main]));
    }
    for script in ["server.py", "main.py"] {
        if dir.join(script).is_file() {
            return Some(("python3".into(), vec![script.into()]));
        }
    }
    if dir.join("Cargo.toml").is_file() {
        return Some(("cargo".into(), vec!["run".into(), "--quiet".into()]));
    }
    None
}

/// The server spec and the paths to watch.
pub fn spec(a: &DevArgs) -> AnyResult<(ServerSpec, Vec<PathBuf>)> {
    let path = match &a.path {
        Some(p) => Some(std::path::absolute(p)?),
        None => None,
    };
    let env: BTreeMap<String, String> = a.env.iter().cloned().collect();
    let (transport, watch) = match (&path, a.command.as_slice()) {
        (_, [cmd, args @ ..]) => {
            let dir = match &path {
                Some(p) if p.is_dir() => p.clone(),
                Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
                None => std::env::current_dir()?,
            };
            let t = Transport::Stdio {
                command: cmd.clone(),
                args: args.to_vec(),
                env,
                cwd: Some(dir.clone()),
            };
            (t, vec![path.clone().unwrap_or(dir)])
        }
        (None, []) => {
            return Err(
                "say what to run: `mcpgw dev ./my-server` or `mcpgw dev -- <command>`".into(),
            );
        }
        (Some(p), []) if p.is_file() => {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
            let t = match ext {
                "wasm" => Transport::Wasm {
                    path: p.clone(),
                    allow_network: Vec::new(),
                    allow_dirs: Vec::new(),
                    env,
                    time_budget: None,
                },
                "json" | "yaml" | "yml" => Transport::OpenApi {
                    spec: p.to_string_lossy().into_owned(),
                    base_url: a.base_url.clone(),
                    headers: BTreeMap::new(),
                    auth: BTreeMap::new(),
                },
                _ => {
                    return Err(format!(
                        "{}: not a .wasm component or an OpenAPI spec; for a script, say how to run it: `mcpgw dev {} -- <command>`",
                        p.display(),
                        p.display()
                    )
                    .into());
                }
            };
            (t, vec![p.clone()])
        }
        (Some(p), []) if p.is_dir() => {
            let (command, args) = detect(p).ok_or_else(|| {
                format!(
                    "{}: no package.json, server.py, main.py or Cargo.toml; say how to run it: `mcpgw dev {} -- <command>`",
                    p.display(),
                    p.display()
                )
            })?;
            let t = Transport::Stdio {
                command,
                args,
                env,
                cwd: Some(p.clone()),
            };
            (t, vec![p.clone()])
        }
        (Some(p), []) => return Err(format!("{}: no such file or directory", p.display()).into()),
    };
    let name = match (&a.name, &path) {
        (Some(n), _) => n.clone(),
        (None, Some(p)) => name_from(p),
        (None, None) => a
            .command
            .first()
            .map(|c| name_from(Path::new(c)))
            .unwrap_or_else(|| "dev".into()),
    };
    let mut spec = ServerSpec::new(&name, transport);
    // a fresh process per edit, and no idle stop while you work
    spec.idle_timeout = Some(mcpgw_core::Duration::from(Duration::from_secs(24 * 3600)));
    spec.validate()?;
    let mut watch = watch;
    for w in &a.watch {
        watch.push(std::path::absolute(w)?);
    }
    Ok((spec, watch))
}

/// The session's gateway config: yours, with a private store, token and
/// port, only this server, no profiles, every tool listed directly.
pub fn config(base: &Config, server: ServerSpec, dir: &Path, port: u16) -> Config {
    let mut c = base.clone();
    c.gateway.store_path = Some(dir.join("db.sqlite"));
    c.gateway.token_path = Some(dir.join("token"));
    c.gateway.listen = Some(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
    c.gateway.mode = mcpgw_core::Mode::Direct;
    c.gateway.policy = mcpgw_core::Policy::default();
    c.servers = vec![server];
    c.profiles = Vec::new();
    c
}

const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    ".mypy_cache",
];

/// Every file under `paths` with its modification time and size.
fn snapshot(paths: &[PathBuf]) -> BTreeMap<PathBuf, (Option<SystemTime>, u64)> {
    fn walk(p: &Path, out: &mut BTreeMap<PathBuf, (Option<SystemTime>, u64)>, depth: usize) {
        let Ok(meta) = std::fs::metadata(p) else {
            return;
        };
        if meta.is_file() {
            out.insert(p.to_path_buf(), (meta.modified().ok(), meta.len()));
            return;
        }
        if !meta.is_dir() || depth > 12 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(p) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if SKIP.contains(&name.as_ref()) {
                continue;
            }
            walk(&e.path(), out, depth + 1);
        }
    }
    let mut out = BTreeMap::new();
    for p in paths {
        walk(p, &mut out, 0);
    }
    out
}

/// The first path that differs between two snapshots.
fn changed(
    a: &BTreeMap<PathBuf, (Option<SystemTime>, u64)>,
    b: &BTreeMap<PathBuf, (Option<SystemTime>, u64)>,
) -> Option<PathBuf> {
    b.iter()
        .find(|(k, v)| a.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .or_else(|| a.keys().find(|k| !b.contains_key(*k)).cloned())
}

fn tool_names(tools: &[serde_json::Map<String, serde_json::Value>]) -> Vec<String> {
    let mut n: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect();
    n.sort();
    n
}

/// Start (or restart) the server and say how it went.
async fn start(mgr: &BackendManager, name: &str, before: &[String]) -> bool {
    let mut r = mgr.ensure(name).await;
    // its tools changed: in dev that's the point, so accept them
    if r.is_err() && mgr.approve_tools(name).is_ok() {
        r = mgr.ensure(name).await;
    }
    match r {
        Ok(()) => {
            let now = tool_names(&mgr.tools(name).await);
            let added: Vec<&String> = now.iter().filter(|t| !before.contains(t)).collect();
            let removed: Vec<&String> = before.iter().filter(|t| !now.contains(t)).collect();
            let mut line = format!("mcpgw dev: {name} ready, {} tool(s)", now.len());
            if !before.is_empty() && !added.is_empty() {
                line.push_str(&format!("; added {}", join(&added)));
            }
            if !removed.is_empty() {
                line.push_str(&format!("; removed {}", join(&removed)));
            }
            eprintln!("{line}");
            true
        }
        Err(e) => {
            eprintln!("mcpgw dev: {name} failed to start: {e}");
            eprintln!("mcpgw dev: fix it and save; still watching");
            false
        }
    }
}

fn join(v: &[&String]) -> String {
    v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}

/// Watch `paths` and restart `name` after each change (once the files have
/// been still for a moment).
pub async fn watch(mgr: Arc<BackendManager>, name: String, paths: Vec<PathBuf>) {
    let mut last = snapshot(&paths);
    let mut before = Vec::new();
    if start(&mgr, &name, &before).await {
        before = tool_names(&mgr.tools(&name).await);
    }
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut now = snapshot(&paths);
        let Some(first) = changed(&last, &now) else {
            continue;
        };
        // let an editor or a build finish writing
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let again = snapshot(&paths);
            if again == now {
                break;
            }
            now = again;
        }
        last = now;
        let shown = paths
            .iter()
            .find_map(|p| {
                first
                    .strip_prefix(p)
                    .ok()
                    .filter(|r| !r.as_os_str().is_empty())
            })
            // a watched file: its own name
            .or_else(|| first.file_name().map(Path::new))
            .unwrap_or(&first)
            .display()
            .to_string();
        eprintln!("mcpgw dev: {shown} changed; restarting {name}");
        let _ = mgr.reset(&name);
        let _ = mgr.stop(&name).await;
        if start(&mgr, &name, &before).await {
            before = tool_names(&mgr.tools(&name).await);
        }
    }
}

/// The request log: one line per call.
pub fn tap() -> mcpgw_router::CallTap {
    Arc::new(
        |tool: &str, args: Option<&serde_json::Value>, outcome: &str, ms: u64, result: &str| {
            let args = args.map(|a| a.to_string()).unwrap_or_else(|| "{}".into());
            let result: String = result.chars().take(160).collect();
            let at = humantime_now();
            eprintln!("{at}  {tool} {args}  ->  {outcome} {ms} ms  {result}");
        },
    )
}

fn humantime_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// `mcpgw dev`: build the session and run it until Ctrl-C.
pub fn run(base: &Config, a: DevArgs) -> AnyResult<()> {
    let (server, watch) = spec(&a)?;
    let dir = std::env::temp_dir().join(format!("mcpgw-dev-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let cfg = config(base, server.clone(), &dir, a.port);
    let store = Arc::new(mcpgw_store::SqliteStore::open(cfg.gateway.store_path())?);
    let dev = crate::serve::Dev {
        name: server.name.clone(),
        watch,
    };
    let r = crate::serve::serve_with(&cfg, store, false, None, Some(dev));
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(path: Option<&Path>, command: &[&str]) -> DevArgs {
        DevArgs {
            path: path.map(Path::to_path_buf),
            name: None,
            port: 0,
            watch: Vec::new(),
            env: Vec::new(),
            base_url: None,
            command: command.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn detects_how_to_run_a_directory() {
        let d = std::env::temp_dir().join(format!("mcpgw-dev-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(spec(&args(Some(&d), &[])).is_err(), "nothing to run");
        std::fs::write(d.join("server.py"), "").unwrap();
        let (s, w) = spec(&args(Some(&d), &[])).unwrap();
        let Transport::Stdio {
            command,
            args: a,
            cwd,
            ..
        } = &s.transport
        else {
            panic!()
        };
        assert_eq!(
            (command.as_str(), a.as_slice()),
            ("python3", &["server.py".to_string()][..])
        );
        assert_eq!(cwd.as_deref(), Some(d.as_path()));
        assert_eq!(w, std::slice::from_ref(&d));
        assert!(s.name.starts_with("mcpgw-dev-detect"));
        std::fs::write(d.join("package.json"), r#"{"main": "dist/server.js"}"#).unwrap();
        let (s, _) = spec(&args(Some(&d), &[])).unwrap();
        let Transport::Stdio {
            command, args: a, ..
        } = &s.transport
        else {
            panic!()
        };
        assert_eq!(
            (command.as_str(), a.as_slice()),
            ("node", &["dist/server.js".to_string()][..])
        );
        // an explicit command wins
        let (s, _) = spec(&args(Some(&d), &["deno", "run", "main.ts"])).unwrap();
        let Transport::Stdio { command, .. } = &s.transport else {
            panic!()
        };
        assert_eq!(command, "deno");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn files_and_names() {
        let d = std::env::temp_dir().join(format!("mcpgw-dev-files-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = d.join("my_tool.wasm");
        std::fs::write(&w, b"").unwrap();
        let (s, _) = spec(&args(Some(&w), &[])).unwrap();
        assert_eq!(s.name, "my-tool");
        assert!(matches!(s.transport, Transport::Wasm { .. }));
        let o = d.join("api.yaml");
        std::fs::write(&o, b"").unwrap();
        let (s, _) = spec(&args(Some(&o), &[])).unwrap();
        assert!(matches!(s.transport, Transport::OpenApi { .. }));
        let t = d.join("notes.txt");
        std::fs::write(&t, b"").unwrap();
        assert!(spec(&args(Some(&t), &[])).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn snapshots_see_edits_but_not_build_output() {
        let d = std::env::temp_dir().join(format!("mcpgw-dev-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("target")).unwrap();
        std::fs::write(d.join("a.js"), "1").unwrap();
        let a = snapshot(std::slice::from_ref(&d));
        std::fs::write(d.join("target/out.o"), "x").unwrap();
        assert_eq!(changed(&a, &snapshot(std::slice::from_ref(&d))), None);
        std::fs::write(d.join("a.js"), "22").unwrap();
        assert_eq!(
            changed(&a, &snapshot(std::slice::from_ref(&d))),
            Some(d.join("a.js"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
