//! Server and daemon commands: add, remove, enable, disable, list, status,
//! logs, pin, restart, up, down.

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use mcpgw_config::Config;
use mcpgw_core::{
    ServerSpec, ToolManifest, Transport,
    scan::{Finding, Severity, has_high, scan_tools},
};
use mcpgw_runtime::{BackendManager, ManagerOptions, State};
use mcpgw_store::{SqliteStore, Store, effective_servers};

use crate::{AnyResult, control::Control, daemon, serve};

fn store(config: &Config) -> AnyResult<SqliteStore> {
    Ok(SqliteStore::open(config.gateway.store_path())?)
}

fn in_config(config: &Config, name: &str) -> bool {
    config.servers.iter().any(|s| s.name == name)
}

/// How `mcpgw add` treats a new server.
#[derive(Debug, Clone, Copy)]
pub struct AddOpts {
    /// Start it now to read and pin its tools.
    pub introspect: bool,
    /// Install its npm/PyPI package into the cache first (M3-08).
    pub install: bool,
    /// How long reading the tools may take.
    pub timeout: Duration,
    /// Add even with high-risk scan findings (M3-10).
    pub accept_risk: bool,
}

impl Default for AddOpts {
    fn default() -> Self {
        Self {
            introspect: true,
            install: true,
            timeout: Duration::from_secs(60),
            accept_risk: false,
        }
    }
}

/// `mcpgw add`: register (or update) a server, pin its tools, and tell a
/// running daemon.
pub fn add(config: &Config, path: &Path, spec: ServerSpec, opts: AddOpts) -> AnyResult<()> {
    add_reviewed(config, path, spec, opts, None).map(|_| ())
}

/// Install a stdio server's npx/uvx package into the cache (M3-08), unless
/// it's there. Failures are reported; the server then runs through npx.
pub fn install_package(config: &Config, spec: &ServerSpec, force: bool) -> AnyResult<bool> {
    let Transport::Stdio { command, args, .. } = &spec.transport else {
        return Ok(false);
    };
    let Some(p) = mcpgw_runtime::packages::detect(command, args) else {
        return Ok(false);
    };
    let cache = config.gateway.runtimes_path();
    if !force && mcpgw_runtime::packages::installed(&cache, &p).is_some() {
        return Ok(true);
    }
    eprintln!("installing {} into the package cache", p.spec);
    match serve::runtime()?.block_on(mcpgw_runtime::packages::install(&cache, &p)) {
        Ok(i) => {
            println!(
                "installed {}@{} for {} (starts without {command})",
                i.name, i.version, spec.name
            );
            Ok(true)
        }
        Err(e) => {
            eprintln!(
                "mcpgw: could not cache {}: {e}; {} starts through {command}",
                p.spec, spec.name
            );
            Ok(false)
        }
    }
}

/// Shown the tools read from a new server (`None`: they couldn't be read
/// now); returns whether to add it.
pub type Review<'a> = &'a dyn Fn(Option<&ToolManifest>, &[Finding]) -> AnyResult<bool>;

/// Tool names of every server but `name` (stored pins).
fn other_tool_names(s: &SqliteStore, name: &str) -> Vec<String> {
    s.manifests()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.server != name)
        .flat_map(|m| m.tools.into_iter().map(|t| t.name))
        .collect()
}

/// Print scan findings, high first.
pub fn print_findings(server: &str, findings: &[Finding]) {
    let mut f: Vec<&Finding> = findings.iter().collect();
    f.sort_by_key(|x| std::cmp::Reverse(x.severity));
    for x in f {
        let level = match x.severity {
            Severity::High => "HIGH",
            Severity::Warning => "warning",
        };
        eprintln!(
            "  {level}: {server}/{} ({}): {}",
            x.tool, x.location, x.message
        );
    }
}

/// `mcpgw scan [server]`: check pinned tools (or, without a pin, the live
/// ones) for poisoning signs. Fails if anything is high risk.
pub fn scan(config: &Config, only: Option<&str>) -> AnyResult<()> {
    let s = store(config)?;
    let servers = effective_servers(&config.servers, &s)?;
    if let Some(n) = only
        && !servers.iter().any(|x| x.name == n)
    {
        return Err(format!("no such server: {n}").into());
    }
    let mut high = 0;
    for spec in servers.iter().filter(|x| only.is_none_or(|n| n == x.name)) {
        let tools = match s.manifest(&spec.name)? {
            Some(m) => m.tools,
            None if only.is_some() => {
                eprintln!("{} has no pinned tools; reading them now", spec.name);
                serve::runtime()?
                    .block_on(mcpgw_runtime::introspect_with_secrets(
                        spec,
                        Duration::from_secs(60),
                        Some(serve::resolver(config)),
                    ))?
                    .tools
            }
            None => {
                println!("{}\tnot scanned (no pinned tools yet)", spec.name);
                continue;
            }
        };
        let f = scan_tools(&tools, &other_tool_names(&s, &spec.name));
        let h = f.iter().filter(|x| x.severity == Severity::High).count();
        high += h;
        println!(
            "{}\t{} tools\t{h} high, {} warnings",
            spec.name,
            tools.len(),
            f.len() - h
        );
        print_findings(&spec.name, &f);
    }
    if high > 0 {
        return Err(format!("{high} high-risk finding(s)").into());
    }
    Ok(())
}

/// [`add`], reading the tools within `timeout` and asking `review` before
/// anything is stored. Returns whether the server was added.
pub fn add_reviewed(
    config: &Config,
    path: &Path,
    mut spec: ServerSpec,
    opts: AddOpts,
    review: Option<Review>,
) -> AnyResult<bool> {
    let AddOpts {
        introspect,
        install,
        timeout,
        accept_risk,
    } = opts;
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
    pin_digest(&mut spec);
    if install {
        install_package(config, &spec, false)?;
    }
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
    // M3-10: poisoning signs are shown before anything is approved
    let findings = manifest
        .as_ref()
        .map(|m| scan_tools(&m.tools, &other_tool_names(&s, &name)))
        .unwrap_or_default();
    if !findings.is_empty() {
        let h = findings
            .iter()
            .filter(|x| x.severity == Severity::High)
            .count();
        eprintln!(
            "tool scan of {name}: {h} high-risk, {} warnings",
            findings.len() - h
        );
        print_findings(&name, &findings);
    }
    let risky = has_high(&findings);
    match review {
        Some(review) => {
            if !review(manifest.as_ref(), &findings)? {
                println!("not added: {name}");
                return Ok(false);
            }
        }
        None if risky && !accept_risk => {
            return Err(format!(
                "not adding {name}: its tools look poisoned (high-risk findings above); if you trust it, add it with --accept-risk"
            )
            .into());
        }
        None => {}
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
            "scan": {
                "high": findings.iter().filter(|x| x.severity == Severity::High).count(),
                "warnings": findings.iter().filter(|x| x.severity == Severity::Warning).count(),
            },
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

/// Pull a container server's image and pin its digest, so it always runs
/// exactly this image. Without a runtime or registry digest it runs by tag.
fn pin_digest(spec: &mut ServerSpec) {
    let Transport::Container {
        image,
        digest,
        runtime,
        ..
    } = &mut spec.transport
    else {
        return;
    };
    if digest.is_some() || image.contains('@') {
        return;
    }
    let rt = match mcpgw_runtime::container::pick_runtime(runtime.as_deref()) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("mcpgw: {e}; {image} runs by tag until it is pinned (`mcpgw add` again)");
            return;
        }
    };
    eprintln!("pulling {image} to pin its digest");
    let resolved = serve::runtime()
        .map_err(|e| e.to_string())
        .and_then(|r| r.block_on(mcpgw_runtime::container::resolve_digest(&rt, image)));
    match resolved {
        Ok(Some(d)) => {
            println!("pinned {image} to {d}");
            *digest = Some(d);
        }
        Ok(None) => {
            eprintln!("mcpgw: {image} has no registry digest (a local build?); it runs by tag")
        }
        Err(e) => eprintln!("mcpgw: could not pin {image}: {e}; it runs by tag"),
    }
}

/// One-line form of how a server is reached.
pub fn describe(s: &ServerSpec) -> String {
    s.transport.target()
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
        let cached = match &s.transport {
            Transport::Stdio { command, args, .. } => {
                mcpgw_runtime::packages::detect(command, args)
                    .and_then(|p| {
                        mcpgw_runtime::packages::installed(&config.gateway.runtimes_path(), &p)
                    })
                    .map(|(_, i)| format!(" [cached {}@{}]", i.name, i.version))
                    .unwrap_or_default()
            }
            _ => String::new(),
        };
        let from = s
            .registry
            .as_ref()
            .map(|r| format!(" [{}@{}]", r.name, r.version))
            .unwrap_or_default();
        print!(
            "{}\t{}\t{source}\t{}{enabled}{from}{cached}",
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
        let pid = match (&s.container, s.pid) {
            (Some(c), _) => format!("{} {}", c.runtime, c.name),
            (None, Some(p)) => format!("pid {p}"),
            (None, None) => "-".into(),
        };
        let err = s.last_error.map(|e| format!("\t{e}")).unwrap_or_default();
        let warm = if s.warm { " (warm)" } else { "" };
        println!(
            "{}\t{}{warm}\t{} tools\t{pid}{err}",
            s.name, s.state, s.tools
        );
    }
    Ok(())
}

/// `mcpgw stats`: CPU, memory and process count of running container
/// servers, from `docker stats` / `podman stats`.
pub fn stats(config: &Config) -> AnyResult<()> {
    let c = Control::connect(config)?.ok_or("the daemon is not running")?;
    let mut by_runtime: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for s in c.status()?.servers {
        if let Some(ctr) = s.container {
            by_runtime
                .entry(ctr.runtime)
                .or_default()
                .push((s.name, ctr.name));
        }
    }
    if by_runtime.is_empty() {
        println!("no container servers are running");
        return Ok(());
    }
    println!("server\tcontainer\tcpu\tmemory\tpids");
    for (rt, list) in by_runtime {
        let mut cmd = std::process::Command::new(&rt);
        cmd.args(["stats", "--no-stream", "--format"])
            .arg("{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.PIDs}}")
            .args(list.iter().map(|(_, n)| n));
        let out = cmd.output().map_err(|e| format!("{rt} stats: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{rt} stats: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let ctr = line.split('\t').next().unwrap_or_default();
            let server = list.iter().find(|(_, n)| n == ctr).map_or("?", |(s, _)| s);
            println!("{server}\t{line}");
        }
    }
    Ok(())
}

/// `mcpgw runtimes install [server]`: (re)install packages into the cache.
pub fn runtimes_install(config: &Config, only: Option<&str>) -> AnyResult<()> {
    let servers = effective_servers(&config.servers, &store(config)?)?;
    let mut any = false;
    for s in servers.iter().filter(|s| only.is_none_or(|n| n == s.name)) {
        let Transport::Stdio { command, args, .. } = &s.transport else {
            continue;
        };
        if mcpgw_runtime::packages::detect(command, args).is_some() {
            any = true;
            install_package(config, s, true)?;
        }
    }
    if let Some(n) = only
        && !servers.iter().any(|s| s.name == n)
    {
        return Err(format!("no such server: {n}").into());
    }
    if !any {
        println!("no npx or uvx servers to install");
    } else if Control::connect(config)?.is_some() {
        println!("restart the daemon (`mcpgw down && mcpgw up`) to start them from the cache");
    }
    Ok(())
}

/// `mcpgw runtimes ls`: what's in the package cache.
pub fn runtimes_ls(config: &Config) -> AnyResult<()> {
    let cache = config.gateway.runtimes_path();
    let list = mcpgw_runtime::packages::list(&cache);
    if list.is_empty() {
        println!("the package cache ({}) is empty", cache.display());
    }
    for (slot, i) in list {
        let kind = serde_json::to_value(i.kind)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        println!(
            "{kind}\t{}@{}\t{}\t{}",
            i.name,
            i.version,
            i.spec,
            slot.display()
        );
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
