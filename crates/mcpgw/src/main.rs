//! `mcpgw` — the gateway binary.
//!
//! `serve` runs the daemon: the Streamable HTTP front door on
//! `127.0.0.1:7777/mcp` (see ADR-0004). `serve --stdio` runs a single-client
//! gateway on stdin/stdout instead. Logs always go to stderr.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

mod daemon;

use clap::{Parser, Subcommand};
use mcpgw_auth::Token;
use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, ManagerOptions, State};
use mcpgw_store::{SqliteStore, Store, effective_servers};

#[derive(Parser)]
#[command(
    name = "mcpgw",
    version,
    about = "MCP gateway: many MCP servers behind one"
)]
struct Cli {
    /// Config file (default: $MCPGW_CONFIG or ~/.config/mcpgw/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Log format on stderr: pretty, compact or json (default: $MCPGW_LOG_FORMAT
    /// or compact). The level comes from $MCPGW_LOG (default: warn).
    #[arg(long, global = true, value_name = "FORMAT")]
    log_format: Option<mcpgw_telemetry::LogFormat>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the gateway daemon in the foreground (HTTP on gateway.listen).
    Serve {
        /// Serve one client on stdin/stdout instead of HTTP.
        #[arg(long)]
        stdio: bool,
    },
    /// Relay stdin/stdout to the daemon, starting it if needed. Use this as
    /// the command in clients that only launch stdio servers.
    Connect,
    /// Register a backend server in the store.
    AddServer {
        /// Server name ([A-Za-z0-9_-], no "__").
        name: String,
        /// Stdio command to launch.
        #[arg(long, conflicts_with = "url", required_unless_present = "url")]
        command: Option<String>,
        /// Streamable HTTP URL (connected from M2 on).
        #[arg(long)]
        url: Option<String>,
        /// Environment variable for the command, KEY=VALUE (repeatable).
        #[arg(long = "env", value_parser = parse_kv)]
        env: Vec<(String, String)>,
        /// Shorter prefix for its tools (`<alias>__<tool>`).
        #[arg(long)]
        alias: Option<String>,
        /// Register but do not start it.
        #[arg(long)]
        disabled: bool,
        /// Don't start the server now to read and pin its tools; they are
        /// captured on its first start instead.
        #[arg(long)]
        no_introspect: bool,
        /// Arguments for the command (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Remove a server from the store.
    RemoveServer {
        /// Server name.
        name: String,
    },
    /// List configured servers (config file + store).
    ListServers {
        /// Also start each server and report its status and tool count.
        #[arg(long)]
        check: bool,
    },
    /// Print the loopback token (creating it if needed).
    Token {
        /// Replace the token with a new one.
        #[arg(long)]
        rotate: bool,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.into(), v.into())),
        _ => Err(format!("expected KEY=VALUE, got {s:?}")),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(e) = mcpgw_telemetry::init(cli.log_format, "warn") {
        eprintln!("mcpgw: {e}");
        return ExitCode::FAILURE;
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcpgw: {e}");
            ExitCode::FAILURE
        }
    }
}

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

fn run(cli: Cli) -> AnyResult<()> {
    let path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load_or_default(&path)?;
    let store = || SqliteStore::open(config.gateway.store_path());
    match cli.cmd {
        Cmd::Serve { stdio } => serve(&config, Arc::new(store()?), stdio),
        Cmd::Connect => connect(&config, &path),
        Cmd::AddServer {
            name,
            command,
            url,
            env,
            alias,
            disabled,
            no_introspect,
            args,
        } => {
            if config.servers.iter().any(|s| s.name == name) {
                return Err(format!(
                    "server {name:?} is defined in {}; edit it there",
                    path.display()
                )
                .into());
            }
            let transport = match (command, url) {
                (Some(command), None) => Transport::Stdio {
                    command,
                    args,
                    env: env.into_iter().collect::<BTreeMap<_, _>>(),
                    cwd: None,
                },
                (None, Some(url)) => Transport::http(url),
                _ => return Err("give exactly one of --command or --url".into()),
            };
            let s = store()?;
            let existed = s.server(&name)?.is_some();
            let mut spec = ServerSpec::new(name.clone(), transport);
            spec.enabled = !disabled;
            spec.alias = alias;
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
            s.upsert_server(&spec)?;
            let verb = if existed { "updated" } else { "added" };
            // A changed spec may expose different tools: drop the old pin.
            s.remove_manifest(&name)?;
            if spec.enabled && !no_introspect && matches!(spec.transport, Transport::Stdio { .. }) {
                match runtime()?.block_on(mcpgw_runtime::introspect(&spec, Duration::from_secs(60)))
                {
                    Ok(m) => {
                        s.put_manifest(&m)?;
                        println!(
                            "{verb} server {name} ({} tools, pin {})",
                            m.tools.len(),
                            &m.sha256[..12]
                        );
                        return Ok(());
                    }
                    Err(e) => eprintln!(
                        "mcpgw: could not read the tools of {name}: {e}; they will be captured on its first start"
                    ),
                }
            }
            println!("{verb} server {name}");
            Ok(())
        }
        Cmd::RemoveServer { name } => {
            let s = store()?;
            s.remove_manifest(&name)?;
            if s.remove_server(&name)? {
                println!("removed server {name}");
                Ok(())
            } else if config.servers.iter().any(|s| s.name == name) {
                Err(format!("server {name:?} is defined in {}", path.display()).into())
            } else {
                Err(format!("no such server: {name}").into())
            }
        }
        Cmd::ListServers { check } => list_servers(&config, &store()?, check),
        Cmd::Token { rotate } => {
            let p = config.gateway.token_path();
            let t = if rotate {
                Token::rotate(&p)?
            } else {
                Token::load_or_create(&p)?
            };
            println!("{}", t.expose());
            Ok(())
        }
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Resolves on Ctrl-C, or SIGTERM on Unix.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn gateway_options(config: &Config) -> mcpgw_router::GatewayOptions {
    mcpgw_router::GatewayOptions {
        mode: config.gateway.mode,
        pinned: config.gateway.pinned_tools.clone(),
    }
}

/// A catalog seeded from the store that saves new manifests back to it.
fn catalog(store: &Arc<SqliteStore>) -> AnyResult<Arc<mcpgw_runtime::Catalog>> {
    let s = store.clone();
    let persist: mcpgw_runtime::Persist = Arc::new(move |m| {
        if let Err(e) = s.put_manifest(m) {
            tracing::warn!(server = %m.server, error = %e, "could not save manifest");
        }
    });
    Ok(Arc::new(mcpgw_runtime::Catalog::new(
        store.manifests()?,
        Some(persist),
    )))
}

/// Start and stop each enabled server that has no manifest yet, so its
/// tools can be listed. Bounded by `timeout`.
async fn capture_missing(mgr: &Arc<BackendManager>, servers: &[ServerSpec], timeout: Duration) {
    let missing: Vec<_> = servers
        .iter()
        .filter(|s| s.enabled && !mgr.catalog().contains(&s.name))
        .map(|s| {
            let mgr = mgr.clone();
            let name = s.name.clone();
            tokio::spawn(async move {
                if mgr.ensure(&name).await.is_ok() {
                    let _ = mgr.stop(&name).await;
                }
            })
        })
        .collect();
    if !missing.is_empty() {
        tracing::info!(servers = missing.len(), "capturing tool manifests");
    }
    let _ = tokio::time::timeout(timeout, async {
        for t in missing {
            let _ = t.await;
        }
    })
    .await;
}

fn serve(config: &Config, store: Arc<SqliteStore>, stdio: bool) -> AnyResult<()> {
    let token = Token::load_or_create(&config.gateway.token_path())?;
    let servers = effective_servers(&config.servers, store.as_ref())?;
    let catalog = catalog(&store)?;
    runtime()?.block_on(async {
        let opts = ManagerOptions {
            idle_timeout: config.gateway.idle_timeout(),
            max_running: config.gateway.max_running(),
            ..ManagerOptions::default()
        };
        let mgr = Arc::new(BackendManager::with_catalog(&servers, opts, catalog));
        // Servers start on first use; only those never seen are started now.
        capture_missing(&mgr, &servers, Duration::from_secs(30)).await;
        let res: AnyResult<()> = if stdio {
            tracing::info!(servers = servers.len(), "mcpgw serving on stdio");
            tokio::select! {
                r = mcpgw_router::serve_stdio(mgr.clone(), gateway_options(config)) => r.map_err(|e| e.to_string().into()),
                _ = shutdown_signal() => Ok(()),
            }
        } else {
            serve_http(config, token, mgr.clone()).await
        };
        mgr.shutdown().await;
        res
    })
}

async fn serve_http(config: &Config, token: Token, mgr: Arc<BackendManager>) -> AnyResult<()> {
    let addr = config.gateway.listen();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr()?;
    let app = mcpgw_api::edge(
        mcpgw_router::http_router(mgr, gateway_options(config)),
        mcpgw_api::EdgeOptions {
            token,
            port: local.port(),
            allowed_origins: config.gateway.allowed_origins.clone(),
        },
    );
    let info_path = daemon::info_path(config);
    daemon::DaemonInfo::current(local).write(&info_path)?;
    eprintln!("mcpgw: listening on http://{local}{}", mcpgw_api::MCP_PATH);
    tracing::info!(%local, "mcpgw serving on http");
    let res = mcpgw_api::serve(listener, app, shutdown_signal()).await;
    // Only remove the file if it is still ours (a newer daemon may own it).
    if daemon::DaemonInfo::read(&info_path).is_some_and(|i| i.pid == std::process::id()) {
        let _ = std::fs::remove_file(&info_path);
    }
    res?;
    Ok(())
}

fn connect(config: &Config, config_path: &std::path::Path) -> AnyResult<()> {
    let info = match daemon::running(config) {
        Some(info) => info,
        None => {
            tracing::info!("no running daemon; starting one");
            daemon::start(config, config_path, Duration::from_secs(60))?
        }
    };
    let token = Token::load_or_create(&config.gateway.token_path())?;
    tracing::info!(url = %info.url, pid = info.pid, "relaying stdio to the daemon");
    runtime()?.block_on(mcpgw_proto::relay::stdio_to_http(&info.url, token.expose()))?;
    Ok(())
}

fn describe(s: &ServerSpec) -> String {
    match &s.transport {
        Transport::Stdio { command, args, .. } => std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        Transport::Http { url, .. } => url.clone(),
    }
}

fn list_servers(config: &Config, store: &SqliteStore, check: bool) -> AnyResult<()> {
    let servers = effective_servers(&config.servers, store)?;
    if servers.is_empty() {
        println!("no servers configured");
        return Ok(());
    }
    let status = if check {
        Some(runtime()?.block_on(async {
            let opts = ManagerOptions {
                connect_timeout: Duration::from_secs(15),
                ..Default::default()
            };
            let mgr = Arc::new(BackendManager::start(&servers, opts));
            let checks: Vec<_> = servers
                .iter()
                .map(|s| {
                    let (mgr, name) = (mgr.clone(), s.name.clone());
                    tokio::spawn(async move {
                        let r =
                            tokio::time::timeout(Duration::from_secs(20), mgr.ensure(&name)).await;
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
        }))
    } else {
        None
    };
    for s in &servers {
        let source = if config.servers.iter().any(|c| c.name == s.name) {
            "config"
        } else {
            "store"
        };
        let kind = match s.transport {
            Transport::Stdio { .. } => "stdio",
            Transport::Http { .. } => "http",
        };
        let enabled = if s.enabled { "" } else { " [disabled]" };
        print!("{}\t{kind}\t{source}\t{}{enabled}", s.name, describe(s));
        if let Some(st) = status.as_ref().and_then(|m| m.get(&s.name)) {
            print!("\t{st}");
        }
        println!();
    }
    Ok(())
}
