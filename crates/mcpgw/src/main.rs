//! `mcpgw` — the gateway binary.
//!
//! `serve` runs the daemon: the Streamable HTTP front door on
//! `127.0.0.1:7777/mcp` plus the `/admin` control API (see ADR-0004);
//! `serve --stdio` runs a single-client gateway on stdin/stdout instead.
//! The other commands manage servers, secrets and the daemon. Logs always
//! go to stderr.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

mod audit;
mod auth;
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
        /// With --stdio: serve this profile's view.
        #[arg(long, requires = "stdio")]
        profile: Option<String>,
    },
    /// Start the daemon in the background.
    Up,
    /// Stop the running daemon.
    Down,
    /// Show the daemon and the state of every server.
    Status,
    /// Relay stdin/stdout to the daemon, starting it if needed. Use this as
    /// the command in clients that only launch stdio servers.
    Connect {
        /// Use this profile's endpoint (/mcp/p/<name>).
        #[arg(long)]
        profile: Option<String>,
    },
    /// List profiles and their endpoints.
    Profiles,
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
    /// Open the web UI in a browser (starts the daemon if needed).
    Ui {
        /// Print the address instead of opening a browser.
        #[arg(long)]
        print: bool,
    },
    /// List tool calls waiting for approval.
    Approvals,
    /// Approve a waiting tool call (id from `mcpgw approvals`).
    Approve {
        /// Approval id.
        id: String,
    },
    /// Refuse a waiting tool call.
    Deny {
        /// Approval id.
        id: String,
    },
    /// Inspect the hash-chained audit log.
    #[command(subcommand)]
    Audit(AuditCmd),
    /// Manage secrets in the vault.
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Connect accounts at OAuth apps (`[[oauth]]` in the config).
    #[command(subcommand)]
    Auth(AuthCmd),
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
    /// Remote MCP URL: Streamable HTTP, or a legacy `…/sse` endpoint.
    #[arg(long)]
    url: Option<String>,
    /// HTTP header for a remote server, NAME=VALUE (repeatable). Values may
    /// use `${secret:<name>}`, e.g. `Authorization=Bearer ${secret:api_key}`.
    #[arg(long = "header", value_parser = parse_kv, requires = "url")]
    headers: Vec<(String, String)>,
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
enum AuthCmd {
    /// Sign in to an OAuth app in the browser; tokens go to the vault.
    Connect {
        /// The `[[oauth]]` app name, e.g. github.
        provider: String,
        /// Account label, for several accounts at one provider (github@work).
        #[arg(long, default_value = "default")]
        account: String,
        /// Print the sign-in URL instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// List connected accounts.
    Ls,
    /// Forget a connected account.
    Rm {
        /// Provider.
        provider: String,
        /// Account label.
        #[arg(long, default_value = "default")]
        account: String,
    },
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
        /// Point the entry at this profile.
        #[arg(long)]
        profile: Option<String>,
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
enum AuditCmd {
    /// Check that no entry was changed, removed or reordered.
    Verify,
    /// Write entries as JSON lines (to stdout).
    Export {
        /// Only entries after this sequence number.
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Show the latest entries.
    Tail {
        /// Number of entries.
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
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
    mcpgw_core::tls::install();
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
        Cmd::Serve { stdio, profile } => serve::serve(
            &config,
            std::sync::Arc::new(SqliteStore::open(config.gateway.store_path())?),
            stdio,
            profile.as_deref(),
        ),
        Cmd::Profiles => list_profiles(&config),
        Cmd::Up => servers::up(&config, &path),
        Cmd::Down => servers::down(&config),
        Cmd::Status => servers::status(&config),
        Cmd::Connect { profile } => connect(&config, &path, profile.as_deref()),
        Cmd::Add(a) => {
            let transport = match (a.command, a.url) {
                (Some(command), None) => Transport::Stdio {
                    command,
                    args: a.args,
                    env: a.env.into_iter().collect::<BTreeMap<_, _>>(),
                    cwd: None,
                },
                (None, Some(url)) => Transport::Http {
                    url,
                    headers: a.headers.into_iter().collect(),
                },
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
        Cmd::Ui { print } => {
            let info = match daemon::running(&config) {
                Some(i) => i,
                None => daemon::start(&config, &path, std::time::Duration::from_secs(60))?,
            };
            let token = Token::load_or_create(&config.gateway.token_path())?;
            let base = info.url.trim_end_matches(mcpgw_api::MCP_PATH);
            let url = format!("{base}/ui#token={}", token.expose());
            if print || !auth::open_browser(&url) {
                println!("{url}");
            } else {
                println!("opened {base}/ui");
            }
            Ok(())
        }
        Cmd::Approvals => {
            let c = control::Control::connect(&config)?.ok_or("the daemon is not running")?;
            let list = c.approvals()?;
            if list.is_empty() {
                println!("no calls waiting for approval");
            }
            for a in list {
                let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    s("id"),
                    s("status"),
                    s("tool"),
                    s("endpoint"),
                    s("arguments")
                );
            }
            Ok(())
        }
        Cmd::Approve { id } => {
            let c = control::Control::connect(&config)?.ok_or("the daemon is not running")?;
            c.decide(&id, true)?;
            println!("approved {id}");
            Ok(())
        }
        Cmd::Deny { id } => {
            let c = control::Control::connect(&config)?.ok_or("the daemon is not running")?;
            c.decide(&id, false)?;
            println!("denied {id}");
            Ok(())
        }
        Cmd::Secret(SecretCmd::Set { name, description }) => {
            secrets::cmd::set(&config, &name, description)
        }
        Cmd::Secret(SecretCmd::Ls) => secrets::cmd::ls(&config),
        Cmd::Audit(AuditCmd::Verify) => audit::verify(&config),
        Cmd::Audit(AuditCmd::Export { after }) => audit::export(&config, after),
        Cmd::Audit(AuditCmd::Tail { lines }) => audit::tail(&config, lines),
        Cmd::Auth(AuthCmd::Connect {
            provider,
            account,
            no_browser,
        }) => auth::connect(&config, &path, &provider, &account, !no_browser),
        Cmd::Auth(AuthCmd::Ls) => auth::ls(&config),
        Cmd::Auth(AuthCmd::Rm { provider, account }) => auth::rm(&config, &provider, &account),
        Cmd::Client(ClientCmd::Install {
            client,
            path,
            dry_run,
            profile,
        }) => clients::install(
            client,
            path,
            explicit_config.as_deref(),
            dry_run,
            profile.as_deref(),
        ),
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

fn list_profiles(config: &Config) -> AnyResult<()> {
    let store = SqliteStore::open(config.gateway.store_path())?;
    let list = serve::profiles(config, &store)?;
    if list.is_empty() {
        println!("no profiles (the default endpoint /mcp serves every server)");
    }
    let base = format!("http://{}{}", config.gateway.listen(), mcpgw_api::MCP_PATH);
    for p in list {
        let servers = if p.servers.is_empty() {
            "all servers".to_string()
        } else {
            p.servers.join(", ")
        };
        let mode = serde_json::to_value(p.mode)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        println!("{}	{base}/p/{}	{mode}	{servers}", p.name, p.name);
    }
    Ok(())
}

fn connect(config: &Config, config_path: &std::path::Path, profile: Option<&str>) -> AnyResult<()> {
    let info = match daemon::running(config) {
        Some(info) => info,
        None => {
            tracing::info!("no running daemon; starting one");
            daemon::start(config, config_path, std::time::Duration::from_secs(60))?
        }
    };
    let token = Token::load_or_create(&config.gateway.token_path())?;
    tracing::info!(url = %info.url, pid = info.pid, "relaying stdio to the daemon");
    let url = match profile {
        Some(p) => format!("{}/p/{p}", info.url),
        None => info.url.clone(),
    };
    serve::runtime()?.block_on(mcpgw_proto::relay::stdio_to_http(&url, token.expose()))?;
    Ok(())
}
