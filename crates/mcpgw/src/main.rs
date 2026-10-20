//! `mcpgw` — the gateway binary.
//!
//! `serve` runs the daemon: the Streamable HTTP front door on
//! `127.0.0.1:7777/mcp` plus the `/admin` control API (see ADR-0004);
//! `serve --stdio` runs a single-client gateway on stdin/stdout instead.
//! The other commands manage servers, secrets and the daemon. Logs always
//! go to stderr.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

mod clients;
mod control;
mod daemon;
mod secrets;
mod serve;
mod servers;

use clap::{Parser, Subcommand};
use mcpgw_auth::Token;
use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, Transport};
use mcpgw_store::SqliteStore;

#[derive(Parser)]
#[command(
    name = "mcpgw",
    version,
    about = "MCP gateway: many MCP servers behind one",
    after_help = "Quick start: `mcpgw add fs --command npx -- -y @modelcontextprotocol/server-filesystem /tmp`, \
                  `mcpgw up`, then point your client at `mcpgw connect` (stdio) or \
                  http://127.0.0.1:7777/mcp with `Authorization: Bearer $(mcpgw token)`."
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
    /// Start the daemon in the background.
    Up,
    /// Stop the running daemon.
    Down,
    /// Show the daemon and the state of every server.
    Status,
    /// Relay stdin/stdout to the daemon, starting it if needed. Use this as
    /// the command in clients that only launch stdio servers.
    Connect,
    /// Register or update a server, pin its tools, and tell the daemon.
    #[command(alias = "add-server")]
    Add(AddArgs),
    /// Remove a server.
    #[command(alias = "remove-server", alias = "rm")]
    Remove {
        /// Server name.
        name: String,
    },
    /// Enable a server.
    Enable {
        /// Server name.
        name: String,
    },
    /// Disable a server: it stops and its tools are hidden.
    Disable {
        /// Server name.
        name: String,
    },
    /// List configured servers (config file + store).
    #[command(alias = "list-servers", alias = "ls")]
    List {
        /// Also start each server and report its status and tool count.
        #[arg(long)]
        check: bool,
    },
    /// Show a server's recent stderr, or the daemon's log.
    Logs {
        /// Server name (omit for the daemon log).
        name: Option<String>,
        /// Number of lines.
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
    /// Trust a quarantined server's changed tools as its new pin.
    Pin {
        /// Server name.
        name: String,
    },
    /// Clear a failed server; it starts fresh on its next call.
    Restart {
        /// Server name.
        name: String,
    },
    /// Manage secrets in the vault.
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Connect MCP clients (Claude Code, Claude Desktop, Cursor, VS Code).
    #[command(subcommand)]
    Client(ClientCmd),
    /// Print the loopback token (creating it if needed).
    Token {
        /// Replace the token with a new one.
        #[arg(long)]
        rotate: bool,
    },
}

#[derive(clap::Args)]
struct AddArgs {
    /// Server name ([A-Za-z0-9_-], no "__").
    name: String,
    /// Stdio command to launch.
    #[arg(long, conflicts_with = "url", required_unless_present = "url")]
    command: Option<String>,
    /// Streamable HTTP URL (connected from M2-04 on).
    #[arg(long)]
    url: Option<String>,
    /// Environment variable for the command, KEY=VALUE (repeatable).
    #[arg(long = "env", value_parser = parse_kv)]
    env: Vec<(String, String)>,
    /// Inject a vault secret as an environment variable, VAR=SECRET (repeatable).
    #[arg(long = "secret", value_parser = parse_kv)]
    secrets: Vec<(String, String)>,
    /// Shorter prefix for its tools (`<alias>__<tool>`).
    #[arg(long)]
    alias: Option<String>,
    /// Keep its processes running even when idle (browser or database sessions).
    #[arg(long)]
    stateful: bool,
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
}

#[derive(Subcommand)]
enum ClientCmd {
    /// Add the mcpgw entry to a client's config, keeping its other servers.
    Install {
        /// claude-code, claude-desktop, cursor or vscode.
        client: mcpgw_config::clients::Client,
        /// Config file to edit (default: the client's user config).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print the result instead of writing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Move a client's servers into mcpgw and replace them with one mcpgw
    /// entry. Credentials in their env move to the vault. The original file
    /// is backed up.
    Import {
        /// claude-code, claude-desktop, cursor or vscode.
        client: mcpgw_config::clients::Client,
        /// Config file to read and rewrite (default: the client's user config).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Show what would be imported without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Leave credential-looking env values in place instead of moving
        /// them to the vault.
        #[arg(long)]
        keep_env: bool,
    },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Store a secret; the value is read hidden from the terminal, or from stdin.
    Set {
        /// Secret name, e.g. github_token.
        name: String,
        /// A note shown by `secret ls`.
        #[arg(long)]
        description: Option<String>,
    },
    /// List secret names (never values).
    Ls,
    /// Delete a secret.
    Rm {
        /// Secret name.
        name: String,
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
    let explicit_config = cli.config.clone();
    let path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load_or_default(&path)?;
    match cli.cmd {
        Cmd::Serve { stdio } => serve::serve(
            &config,
            std::sync::Arc::new(SqliteStore::open(config.gateway.store_path())?),
            stdio,
        ),
        Cmd::Up => servers::up(&config, &path),
        Cmd::Down => servers::down(&config),
        Cmd::Status => servers::status(&config),
        Cmd::Connect => connect(&config, &path),
        Cmd::Add(a) => {
            let transport = match (a.command, a.url) {
                (Some(command), None) => Transport::Stdio {
                    command,
                    args: a.args,
                    env: a.env.into_iter().collect::<BTreeMap<_, _>>(),
                    cwd: None,
                },
                (None, Some(url)) => Transport::http(url),
                _ => return Err("give exactly one of --command or --url".into()),
            };
            let mut spec = ServerSpec::new(a.name, transport);
            spec.enabled = !a.disabled;
            spec.alias = a.alias;
            spec.stateful = a.stateful;
            spec.secrets = a.secrets.into_iter().collect();
            servers::add(&config, &path, spec, !a.no_introspect)
        }
        Cmd::Remove { name } => servers::remove(&config, &path, &name),
        Cmd::Enable { name } => servers::set_enabled(&config, &path, &name, true),
        Cmd::Disable { name } => servers::set_enabled(&config, &path, &name, false),
        Cmd::List { check } => servers::list(&config, check),
        Cmd::Logs { name, lines } => servers::logs(&config, name.as_deref(), lines),
        Cmd::Pin { name } => servers::pin(&config, &name),
        Cmd::Restart { name } => servers::restart(&config, &name),
        Cmd::Secret(SecretCmd::Set { name, description }) => {
            secrets::cmd::set(&config, &name, description)
        }
        Cmd::Secret(SecretCmd::Ls) => secrets::cmd::ls(&config),
        Cmd::Client(ClientCmd::Install {
            client,
            path,
            dry_run,
        }) => clients::install(client, path, explicit_config.as_deref(), dry_run),
        Cmd::Client(ClientCmd::Import {
            client,
            path,
            dry_run,
            keep_env,
        }) => clients::import(
            &config,
            client,
            path,
            explicit_config.as_deref(),
            dry_run,
            keep_env,
        ),
        Cmd::Secret(SecretCmd::Rm { name }) => secrets::cmd::rm(&config, &name),
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

fn connect(config: &Config, config_path: &std::path::Path) -> AnyResult<()> {
    let info = match daemon::running(config) {
        Some(info) => info,
        None => {
            tracing::info!("no running daemon; starting one");
            daemon::start(config, config_path, std::time::Duration::from_secs(60))?
        }
    };
    let token = Token::load_or_create(&config.gateway.token_path())?;
    tracing::info!(url = %info.url, pid = info.pid, "relaying stdio to the daemon");
    serve::runtime()?.block_on(mcpgw_proto::relay::stdio_to_http(&info.url, token.expose()))?;
    Ok(())
}
