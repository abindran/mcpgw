//! `mcpgw` — the gateway binary.
//!
//! `serve` runs the gateway on stdio. stdout carries MCP, so all logs go
//! to stderr.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use mcpgw_auth::Token;
use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, BackendState, ManagerOptions};
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
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the gateway on stdin/stdout.
    Serve,
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
        /// Register but do not start it.
        #[arg(long)]
        disabled: bool,
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

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("MCPGW_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();
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
        Cmd::Serve => serve(&config, &store()?),
        Cmd::AddServer {
            name,
            command,
            url,
            env,
            disabled,
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
            s.upsert_server(&spec)?;
            println!(
                "{} server {name}",
                if existed { "updated" } else { "added" }
            );
            Ok(())
        }
        Cmd::RemoveServer { name } => {
            if store()?.remove_server(&name)? {
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

fn serve(config: &Config, store: &SqliteStore) -> AnyResult<()> {
    Token::load_or_create(&config.gateway.token_path())?;
    let servers = effective_servers(&config.servers, store)?;
    runtime()?.block_on(async {
        let mgr = Arc::new(BackendManager::start(&servers, ManagerOptions::default()));
        // Give backends a moment so the first tools/list is complete.
        mgr.wait_ready(Duration::from_secs(30)).await;
        tracing::info!(servers = servers.len(), "mcpgw serving on stdio");
        let res = tokio::select! {
            r = mcpgw_router::serve_stdio(mgr.clone()) => r.map_err(|e| e.to_string()),
            _ = tokio::signal::ctrl_c() => Ok(()),
        };
        mgr.shutdown().await;
        res
    })?;
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
            let mgr = BackendManager::start(&servers, opts);
            mgr.wait_ready(Duration::from_secs(20)).await;
            let mut out = BTreeMap::new();
            for st in mgr.status().await {
                let n = mgr.tools(&st.name).await.len();
                let text = match st.state {
                    BackendState::Running { protocol_version } => {
                        format!("ok ({n} tools, {protocol_version})")
                    }
                    BackendState::Restarting { last_error, .. } => format!("error: {last_error}"),
                    BackendState::Unsupported(why) => format!("skipped: {why}"),
                    other => format!("{other:?}"),
                };
                out.insert(st.name, text);
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
