//! Server and daemon commands: add, remove, enable, disable, list, status,
//! logs, pin, restart, up, down.

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, ToolManifest, Transport};
use mcpgw_runtime::{BackendManager, ManagerOptions, State};
use mcpgw_store::{SqliteStore, Store, effective_servers};

use crate::{AnyResult, control::Control, daemon, serve};

fn store(config: &Config) -> AnyResult<SqliteStore> {
    Ok(SqliteStore::open(config.gateway.store_path())?)
}

fn in_config(config: &Config, name: &str) -> bool {
    config.servers.iter().any(|s| s.name == name)
}

/// `mcpgw add`: register (or update) a server, pin its tools, and tell a
/// running daemon.
pub fn add(config: &Config, path: &Path, spec: ServerSpec, introspect: bool) -> AnyResult<()> {
    add_reviewed(
        config,
        path,
        spec,
        introspect,
        Duration::from_secs(60),
        None,
    )
    .map(|_| ())
}

/// Shown the tools read from a new server (`None`: they couldn't be read
/// now); returns whether to add it.
pub type Review<'a> = &'a dyn Fn(Option<&ToolManifest>) -> AnyResult<bool>;

/// [`add`], reading the tools within `timeout` and asking `review` before
/// anything is stored. Returns whether the server was added.
pub fn add_reviewed(
    config: &Config,
    path: &Path,
    spec: ServerSpec,
    introspect: bool,
    timeout: Duration,
    review: Option<Review>,
) -> AnyResult<bool> {
    let name = spec.name.clone();
    if in_config(config, &name) {
        return Err(format!(
            "server {name:?} is defined in {}; edit it there",
            path.display()
        )
        .into());
    }
    let s = store(config)?;
    let clash = effective_servers(&config.servers, &s)?
        .into_iter()
        .find(|o| o.name != spec.name && o.prefix() == spec.prefix());
    if let Some(o) = clash {
        return Err(format!(
            "tool prefix {:?} is already used by server {:?}; pick another --alias",
            spec.prefix(),
            o.name
        )
        .into());
    }
    spec.validate()?;
    let existed = s.server(&name)?.is_some();
    let verb = if existed { "updated" } else { "added" };
    let mut manifest = None;
    if spec.enabled && introspect {
        let capture =
            mcpgw_runtime::introspect_with_secrets(&spec, timeout, Some(serve::resolver(config)));
        match serve::runtime()?.block_on(capture) {
            Ok(m) => manifest = Some(m),
            Err(e) if e.to_string().contains("requires authentication") => eprintln!(
                "mcpgw: {name} requires a login; sign in with `mcpgw auth connect {name}` (its tools are captured on its first start)"
            ),
            Err(e) => eprintln!(
                "mcpgw: could not read the tools of {name}: {e}; they will be captured on its first start"
            ),
        }
    }
    if let Some(review) = review
        && !review(manifest.as_ref())?
    {
        println!("not added: {name}");
        return Ok(false);
    }
    s.upsert_server(&spec)?;
    // A changed spec may expose different tools: drop the old pin.
    s.remove_manifest(&name)?;
    if let Some(m) = &manifest {
        s.put_manifest(m)?;
    }
    crate::audit::record(
        config,
        if existed {
            "server.update"
        } else {
            "server.add"
        },
        &name,
        serde_json::json!({
            "transport": spec.transport.kind(),
            "pin": manifest.as_ref().map(|m| m.sha256.clone()),
            "registry": spec.registry,
        }),
    );
    match &manifest {
        Some(m) => println!(
            "{verb} server {name} ({} tools, pin {})",
            m.tools.len(),
            &m.sha256[..12]
        ),
        None => println!("{verb} server {name}"),
    }
    if let Some(c) = Control::connect(config)? {
        c.add(&spec, manifest)?;
        println!("the running daemon now serves {name}");
    }
    Ok(true)
}

/// `mcpgw remove`.
pub fn remove(config: &Config, path: &Path, name: &str) -> AnyResult<()> {
    let s = store(config)?;
    s.remove_manifest(name)?;
    if s.remove_server(name)? {
        if let Some(c) = Control::connect(config)? {
            let _ = c.remove(name);
        }
        crate::audit::record(config, "server.remove", name, serde_json::json!({}));
        println!("removed server {name}");
        Ok(())
    } else if in_config(config, name) {
        Err(format!("server {name:?} is defined in {}", path.display()).into())
    } else {
        Err(format!("no such server: {name}").into())
    }
}

/// `mcpgw enable` / `mcpgw disable`.
pub fn set_enabled(config: &Config, path: &Path, name: &str, on: bool) -> AnyResult<()> {
    if in_config(config, name) {
        return Err(format!(
            "server {name:?} is defined in {}; set `enabled = {on}` there",
            path.display()
        )
        .into());
    }
    let s = store(config)?;
    let mut spec = s
        .server(name)?
        .ok_or_else(|| format!("no such server: {name}"))?;
    spec.enabled = on;
    s.upsert_server(&spec)?;
    if let Some(c) = Control::connect(config)? {
        c.action(name, if on { "enable" } else { "disable" })?;
    }
    crate::audit::record(
        config,
        if on {
            "server.enable"
        } else {
            "server.disable"
        },
        name,
        serde_json::json!({}),
    );
    println!("{} server {name}", if on { "enabled" } else { "disabled" });
    Ok(())
}

/// One-line form of how a server is reached.
pub fn describe(s: &ServerSpec) -> String {
    match &s.transport {
        Transport::Stdio { command, args, .. } => std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        Transport::Http { url, .. } => url.clone(),
    }
}

/// `mcpgw list`: configured servers; with `--check`, start each one.
pub fn list(config: &Config, check: bool) -> AnyResult<()> {
    let servers = effective_servers(&config.servers, &store(config)?)?;
    if servers.is_empty() {
        println!("no servers configured");
        return Ok(());
    }
    let status = if check {
        Some(serve::runtime()?.block_on(check_all(&servers, config)))
    } else {
        None
    };
    for s in &servers {
        let source = if in_config(config, &s.name) {
            "config"
        } else {
            "store"
        };
        let enabled = if s.enabled { "" } else { " [disabled]" };
        let from = s
            .registry
            .as_ref()
            .map(|r| format!(" [{}@{}]", r.name, r.version))
            .unwrap_or_default();
        print!(
            "{}\t{}\t{source}\t{}{enabled}{from}",
            s.name,
            s.transport.kind(),
            describe(s)
        );
        if let Some(st) = status.as_ref().and_then(|m| m.get(&s.name)) {
            print!("\t{st}");
        }
        println!();
    }
    Ok(())
}

async fn check_all(servers: &[ServerSpec], config: &Config) -> BTreeMap<String, String> {
    let opts = ManagerOptions {
        connect_timeout: Duration::from_secs(15),
        ..Default::default()
    };
    let mgr = Arc::new(BackendManager::with_secrets(
        servers,
        opts,
        Arc::new(mcpgw_runtime::Catalog::default()),
        Some(serve::resolver(config)),
    ));
    let checks: Vec<_> = servers
        .iter()
        .map(|s| {
            let (mgr, name) = (mgr.clone(), s.name.clone());
            tokio::spawn(async move {
                let r = tokio::time::timeout(Duration::from_secs(20), mgr.ensure(&name)).await;
                (name, r)
            })
        })
        .collect();
    let mut out = BTreeMap::new();
    for c in checks {
        let Ok((name, r)) = c.await else { continue };
        let st = mgr.status_of(&name).expect("configured server");
        let text = match r {
            Ok(Ok(())) => format!(
                "ok ({} tools, {})",
                mgr.live_tools(&name).await.len(),
                st.protocol_version.as_deref().unwrap_or("?")
            ),
            _ if st.state == State::Disabled => {
                format!("skipped: {}", st.last_error.unwrap_or_default())
            }
            Ok(Err(e)) => format!("error: {e}"),
            Err(_) => "error: timed out".to_string(),
        };
        out.insert(name, text);
    }
    mgr.shutdown().await;
    out
}

/// `mcpgw status`: the daemon and each server's state.
pub fn status(config: &Config) -> AnyResult<()> {
    let Some(c) = Control::connect(config)? else {
        println!("daemon: not running (start it with `mcpgw up`)");
        let servers = effective_servers(&config.servers, &store(config)?)?;
        for s in servers {
            println!(
                "{}\t{}",
                s.name,
                if s.enabled { "cold" } else { "disabled" }
            );
        }
        return Ok(());
    };
    let r = c.status()?;
    println!(
        "daemon: running (pid {}, {}, mcpgw {})",
        r.pid, c.info.url, r.version
    );
    for s in r.servers {
        let pid = s
            .pid
            .map(|p| format!("pid {p}"))
            .unwrap_or_else(|| "-".into());
        let err = s.last_error.map(|e| format!("\t{e}")).unwrap_or_default();
        println!("{}\t{}\t{} tools\t{pid}{err}", s.name, s.state, s.tools);
    }
    Ok(())
}

/// `mcpgw logs [name]`: a server's stderr, or the daemon's log.
pub fn logs(config: &Config, name: Option<&str>, lines: usize) -> AnyResult<()> {
    match name {
        Some(n) => {
            let c = Control::connect(config)?.ok_or("the daemon is not running")?;
            for l in c.logs(n, lines)? {
                println!("{l}");
            }
        }
        None => {
            let path = daemon::log_path(config);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("no daemon log at {}: {e}", path.display()))?;
            let all: Vec<&str> = text.lines().collect();
            for l in &all[all.len().saturating_sub(lines)..] {
                println!("{l}");
            }
        }
    }
    Ok(())
}

/// `mcpgw pin <name>`: trust a quarantined server's new tools.
pub fn pin(config: &Config, name: &str) -> AnyResult<()> {
    let s = store(config)?;
    let spec = effective_servers(&config.servers, &s)?
        .into_iter()
        .find(|x| x.name == name)
        .ok_or_else(|| format!("no such server: {name}"))?;
    if let Some(c) = Control::connect(config)? {
        let v = c.action(name, "pin")?.unwrap_or_default();
        crate::audit::record(
            config,
            "server.pin",
            name,
            serde_json::json!({"sha256": v["sha256"]}),
        );
        println!(
            "pinned {name}: {} tools, pin {}",
            v["tools"],
            v["sha256"].as_str().map(|p| &p[..12]).unwrap_or("?")
        );
        return Ok(());
    }
    // No daemon: capture the current tools and store them as the pin.
    let m = serve::runtime()?.block_on(mcpgw_runtime::introspect_with_secrets(
        &spec,
        Duration::from_secs(60),
        Some(serve::resolver(config)),
    ))?;
    s.put_manifest(&m)?;
    crate::audit::record(
        config,
        "server.pin",
        name,
        serde_json::json!({"sha256": m.sha256}),
    );
    println!(
        "pinned {name}: {} tools, pin {}",
        m.tools.len(),
        &m.sha256[..12]
    );
    Ok(())
}

/// `mcpgw restart <name>`: clear a failed server and stop it; the next
/// call starts it fresh.
pub fn restart(config: &Config, name: &str) -> AnyResult<()> {
    let c = Control::connect(config)?.ok_or("the daemon is not running")?;
    c.action(name, "restart")?;
    println!("restarted {name}; it starts again on its next call");
    Ok(())
}

/// `mcpgw up`: start the daemon in the background.
pub fn up(config: &Config, path: &Path) -> AnyResult<()> {
    if let Some(info) = daemon::running(config) {
        println!("daemon already running (pid {}, {})", info.pid, info.url);
        return Ok(());
    }
    let info = daemon::start(config, path, Duration::from_secs(60))?;
    println!("daemon started (pid {}, {})", info.pid, info.url);
    Ok(())
}

/// `mcpgw down`: stop the daemon and wait for it to exit.
pub fn down(config: &Config) -> AnyResult<()> {
    let Some(c) = Control::connect(config)? else {
        println!("daemon is not running");
        return Ok(());
    };
    let pid = c.info.pid;
    c.shutdown()?;
    for _ in 0..200 {
        if daemon::running(config).is_none() {
            println!("daemon stopped (pid {pid})");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("daemon (pid {pid}) did not stop within 10 s").into())
}
