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
mod dev;
mod doctor;
mod policy;
mod registry;
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
    /// CPU, memory and process counts of running container servers.
    Stats,
    /// Find and explain misconfigurations: config, vault, secrets,
    /// commands, Docker, ports, the daemon, logins, profiles and client
    /// configs. Fails if anything is broken.
    Doctor {
        /// Print the checks as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check servers' tools for poisoning signs (hidden characters,
    /// injection phrases, instructions about other tools).
    Scan {
        /// Only this server (read live if it has no pinned tools).
        server: Option<String>,
    },
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
    /// With a registry name (`mcpgw add io.github.github/github-mcp-server`)
    /// it installs that entry from the MCP Registry at a pinned version,
    /// shows its tools and asks before adding.
    #[command(alias = "add-server")]
    Add(Box<AddArgs>),
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
    /// Search the MCP Registry (gateway.registry_url).
    #[command(subcommand)]
    Registry(RegistryCmd),
    /// Work on a server: run it under a private gateway, restart it on
    /// every edit, log each call, and open the inspector.
    Dev(Box<DevCmd>),
    /// Tool policies: write Cedar from glob rules, check decisions.
    #[command(subcommand)]
    Policy(PolicyCmd),
    /// The package cache for npm and PyPI servers.
    #[command(subcommand)]
    Runtimes(RuntimesCmd),
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
    /// Server name ([A-Za-z0-9_-], no "__"), or an MCP Registry entry
    /// (`io.github.github/github-mcp-server`, optionally `@<version>`).
    name: String,
    /// Stdio command to launch.
    #[arg(long, conflicts_with_all = ["url", "image"])]
    command: Option<String>,
    /// Container image to run with Docker or Podman (MCP over its stdio);
    /// its digest is pinned when added.
    #[arg(long, conflicts_with = "url")]
    image: Option<String>,
    /// A Wasm component exporting the `mcpgw:tool` world, run in-process
    /// with wasmtime.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["command", "url", "image"])]
    wasm: Option<PathBuf>,
    /// An OpenAPI 3.x spec (file or URL, JSON or YAML): its operations
    /// become tools, served by mcpgw itself.
    #[arg(long, value_name = "SPEC", conflicts_with_all = ["command", "url", "image", "wasm"])]
    openapi: Option<String>,
    /// For --openapi: where requests go (default: the spec's server URL).
    #[arg(long, value_name = "URL", requires = "openapi")]
    base_url: Option<String>,
    /// For --openapi: bind a vault secret to one of the spec's security
    /// schemes, SCHEME=SECRET (repeatable): `--auth api_key=petstore_key`,
    /// `--auth oauth=oauth/github@{account}`.
    #[arg(long, value_parser = parse_kv, value_name = "SCHEME=SECRET", requires = "openapi")]
    auth: Vec<(String, String)>,
    /// For --openapi: the user name for an HTTP basic scheme, SCHEME=USER.
    #[arg(long, value_parser = parse_kv, value_name = "SCHEME=USER", requires = "openapi")]
    auth_username: Vec<(String, String)>,
    /// Let a Wasm server connect to HOST:PORT (repeatable; `*` for any).
    /// Default: no network.
    #[arg(long, value_name = "HOST:PORT", requires = "wasm")]
    allow_network: Vec<String>,
    /// Let a Wasm server read this directory (repeatable).
    #[arg(long, value_name = "DIR", requires = "wasm")]
    allow_dir: Vec<PathBuf>,
    /// Let a Wasm server read and write this directory (repeatable).
    #[arg(long, value_name = "DIR", requires = "wasm")]
    allow_dir_rw: Vec<PathBuf>,
    /// Stop a Wasm server's call after this long (e.g. 5s; default 30s).
    #[arg(long, value_name = "DURATION", requires = "wasm")]
    time_budget: Option<mcpgw_core::Duration>,
    /// Container runtime for --image: docker or podman (default: whichever
    /// is installed, Docker first).
    #[arg(long, requires = "image")]
    runtime: Option<String>,
    /// Extra `run` flag for --image (repeatable), e.g. `--run-arg=-p
    /// --run-arg=127.0.0.1:8085:8085`.
    #[arg(long = "run-arg", requires = "image", allow_hyphen_values = true)]
    run_args: Vec<String>,
    /// Sandbox for --image: standard (default), strict or off.
    #[arg(long, requires = "image", value_parser = parse_sandbox)]
    sandbox: Option<mcpgw_core::Sandbox>,
    /// Let the container reach only this host:port (repeatable), e.g.
    /// `--egress api.github.com:443`.
    #[arg(
        long,
        requires = "image",
        value_name = "HOST:PORT",
        conflicts_with = "no_network"
    )]
    egress: Vec<String>,
    /// Mount a vault secret as a read-only tmpfs file in the container,
    /// PATH=SECRET (repeatable), e.g. `--secret-file /run/secrets/sa.json=gcp_sa`.
    #[arg(long = "secret-file", value_parser = parse_kv, requires = "image", value_name = "PATH=SECRET")]
    secret_files: Vec<(String, String)>,
    /// Give the container no network at all.
    #[arg(long, requires = "image")]
    no_network: bool,
    /// Memory cap for the server, e.g. 256MiB (stdio or container servers).
    #[arg(long, conflicts_with = "url")]
    memory: Option<mcpgw_core::ByteSize>,
    /// CPU limit in cores, e.g. 0.5 (container servers).
    #[arg(long, requires = "image")]
    cpus: Option<f64>,
    /// Process limit (container servers).
    #[arg(long, requires = "image")]
    pids: Option<u32>,
    /// Remote MCP URL: Streamable HTTP, or a legacy `…/sse` endpoint.
    #[arg(long)]
    url: Option<String>,
    /// HTTP header for a remote server, NAME=VALUE (repeatable). Values may
    /// use `${secret:<name>}`, e.g. `Authorization=Bearer ${secret:api_key}`.
    #[arg(long = "header", value_parser = parse_kv)]
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
    /// Keep it warm: start it with the gateway and restart it after a crash,
    /// so calls never wait for a cold start (`warm_pool = { min = 1 }`).
    #[arg(long)]
    warm: bool,
    /// Register but do not start it.
    #[arg(long)]
    disabled: bool,
    /// Don't start the server now to read and pin its tools; they are
    /// captured on its first start instead.
    #[arg(long)]
    no_introspect: bool,
    /// Don't install an npx/uvx server's package into the cache; it then
    /// starts through npx/uvx each time.
    #[arg(long)]
    no_cache: bool,
    /// Add it even if its tool descriptions look poisoned (hidden
    /// characters, injection phrases); see `mcpgw scan`.
    #[arg(long)]
    accept_risk: bool,
    /// Registry installs: the server name to use (default: the entry's
    /// last path segment).
    #[arg(long = "as", value_name = "NAME", conflicts_with_all = ["command", "url", "image"])]
    server_name: Option<String>,
    /// Registry installs: use the entry's hosted remote, not a package.
    #[arg(long, conflicts_with_all = ["command", "url", "image", "package"])]
    remote: bool,
    /// Registry installs: use this package type (npm, pypi or oci).
    #[arg(long, value_name = "TYPE", conflicts_with_all = ["command", "url", "image"])]
    package: Option<String>,
    /// Registry installs: add without asking after showing the tools.
    #[arg(short = 'y', long)]
    yes: bool,
    /// Arguments for the command or the image's entrypoint (after `--`).
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum RuntimesCmd {
    /// Install (again) the packages of npx/uvx servers, so they start
    /// without npx/uvx and without the network.
    Install {
        /// Only this server.
        server: Option<String>,
    },
    /// List cached packages.
    Ls,
}

/// `mcpgw dev` (M4-10).
#[derive(clap::Args)]
struct DevCmd {
    /// The server: a directory (package.json, server.py, main.py or
    /// Cargo.toml), a `.wasm` component, or an OpenAPI spec.
    path: Option<PathBuf>,
    /// Server name (default: from the path).
    #[arg(long)]
    name: Option<String>,
    /// Port for the session's gateway (default: any free port).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Also restart when this file or directory changes (repeatable).
    #[arg(long, value_name = "PATH")]
    watch: Vec<PathBuf>,
    /// Environment variable for the server, KEY=VALUE (repeatable).
    #[arg(long = "env", value_parser = parse_kv)]
    env: Vec<(String, String)>,
    /// For an OpenAPI spec: where requests go.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// How to run the server, when it isn't detected
    /// (`mcpgw dev ./srv -- deno run main.ts`).
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Subcommand)]
enum PolicyCmd {
    /// Print an endpoint's allow/deny/approve rules as Cedar policies that
    /// give the same decisions.
    Convert {
        /// The profile (default: the default endpoint).
        #[arg(long)]
        profile: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Load an endpoint's policy (Cedar files are validated) and show what
    /// it decides for each tool given.
    Check {
        /// The profile (default: the default endpoint).
        #[arg(long)]
        profile: Option<String>,
        /// Public tool names (`<server>__<tool>`).
        tools: Vec<String>,
    },
}

#[derive(Subcommand)]
enum RegistryCmd {
    /// Find servers by name or description.
    Search {
        /// Search text.
        #[arg(required = true)]
        query: Vec<String>,
        /// Most results.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show an entry's packages, remotes and inputs.
    Show {
        /// Entry name, optionally `@<version>`.
        name: String,
    },
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

fn parse_sandbox(s: &str) -> Result<mcpgw_core::Sandbox, String> {
    serde_json::from_value(serde_json::Value::String(s.into()))
        .map_err(|_| format!("sandbox must be standard, strict or off, not {s:?}"))
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
    // The daemon (serve) exports traces, metrics and logs over OTLP when an
    // endpoint is configured (M3-11); the guard flushes them at exit.
    let otlp = match &cli.cmd {
        Cmd::Serve { .. } => {
            let path = cli.config.clone().unwrap_or_else(Config::default_path);
            let configured = Config::load_or_default(&path)
                .ok()
                .and_then(|c| c.gateway.otlp_endpoint);
            mcpgw_telemetry::OtlpConfig::from_env(configured.as_deref())
        }
        _ => None,
    };
    let _otel = match mcpgw_telemetry::init_with_otlp(cli.log_format, "warn", otlp.as_ref()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("mcpgw: {e}");
            return ExitCode::FAILURE;
        }
    };
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
    // doctor reports a config that doesn't load instead of stopping at it
    if let Cmd::Doctor { json } = cli.cmd {
        return doctor::run(&path, json);
    }
    let config = Config::load_or_default(&path)?;
    mcpgw_runtime::packages::set_dir(config.gateway.runtimes_path());
    mcpgw_runtime::wasm::set_cache_dir(config.gateway.runtimes_path().join("wasm"));
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
        Cmd::Stats => servers::stats(&config),
        Cmd::Doctor { .. } => unreachable!("handled before the config loads"),
        Cmd::Scan { server } => servers::scan(&config, server.as_deref()),
        Cmd::Connect { profile } => connect(&config, &path, profile.as_deref()),
        Cmd::Add(a)
            if a.command.is_none()
                && a.url.is_none()
                && a.image.is_none()
                && a.wasm.is_none()
                && a.openapi.is_none() =>
        {
            if !a.name.contains('/') {
                return Err(format!(
                    "give --command, --url or --image for {:?}, or a registry name such as io.github.github/github-mcp-server",
                    a.name
                )
                .into());
            }
            if !a.secrets.is_empty() || !a.args.is_empty() {
                return Err(
                    "--secret and command arguments don't apply to registry installs".into(),
                );
            }
            registry::add(
                &config,
                &path,
                registry::Install {
                    entry: a.name,
                    server_name: a.server_name,
                    remote: a.remote,
                    package: a.package,
                    values: a.env.into_iter().collect(),
                    yes: a.yes,
                    alias: a.alias,
                    stateful: a.stateful,
                    warm: a.warm,
                    disabled: a.disabled,
                    no_introspect: a.no_introspect,
                    accept_risk: a.accept_risk,
                },
            )
        }
        Cmd::Registry(RegistryCmd::Search { query, limit }) => {
            registry::search(&config, &query.join(" "), limit)
        }
        Cmd::Registry(RegistryCmd::Show { name }) => registry::show(&config, &name),
        Cmd::Dev(d) => {
            let d = *d;
            dev::run(
                &config,
                dev::DevArgs {
                    path: d.path,
                    name: d.name,
                    port: d.port,
                    watch: d.watch,
                    env: d.env,
                    base_url: d.base_url,
                    command: d.command,
                },
            )
        }
        Cmd::Policy(PolicyCmd::Convert { profile, output }) => {
            policy::convert(&config, profile.as_deref(), output.as_deref())
        }
        Cmd::Policy(PolicyCmd::Check { profile, tools }) => {
            policy::check(&config, profile.as_deref(), &tools)
        }
        Cmd::Runtimes(RuntimesCmd::Install { server }) => {
            servers::runtimes_install(&config, server.as_deref())
        }
        Cmd::Runtimes(RuntimesCmd::Ls) => servers::runtimes_ls(&config),
        Cmd::Add(a) => {
            if !a.headers.is_empty() && a.url.is_none() && a.openapi.is_none() {
                return Err("--header goes with --url or --openapi".into());
            }
            let transport = match (a.command, a.url) {
                (None, None) if a.openapi.is_some() => {
                    let spec = a.openapi.clone().unwrap_or_default();
                    // a local spec is stored absolute: the daemon may run from elsewhere
                    let spec = if spec.starts_with("http://") || spec.starts_with("https://") {
                        spec
                    } else {
                        std::fs::canonicalize(&spec)
                            .map_err(|e| format!("{spec}: {e}"))?
                            .to_string_lossy()
                            .into_owned()
                    };
                    let mut auth: BTreeMap<String, mcpgw_core::AuthBinding> = a
                        .auth
                        .iter()
                        .map(|(scheme, secret)| {
                            let b = mcpgw_core::AuthBinding {
                                secret: secret.clone(),
                                username: None,
                            };
                            (scheme.clone(), b)
                        })
                        .collect();
                    for (scheme, user) in &a.auth_username {
                        match auth.get_mut(scheme) {
                            Some(b) => b.username = Some(user.clone()),
                            None => {
                                return Err(format!(
                                    "--auth-username {scheme}=... needs --auth {scheme}=<secret>"
                                )
                                .into());
                            }
                        }
                    }
                    Transport::OpenApi {
                        spec,
                        base_url: a.base_url.clone(),
                        headers: a.headers.iter().cloned().collect(),
                        auth,
                    }
                }
                (None, None) if a.wasm.is_some() => {
                    let p = a.wasm.clone().unwrap_or_default();
                    // stored absolute: the daemon may run from elsewhere
                    let path =
                        std::fs::canonicalize(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                    let dir = |p: &PathBuf, write: bool| -> AnyResult<mcpgw_core::DirAccess> {
                        Ok(mcpgw_core::DirAccess {
                            path: std::fs::canonicalize(p)
                                .map_err(|e| format!("{}: {e}", p.display()))?,
                            guest: Some(p.to_string_lossy().replace('\\', "/")),
                            write,
                        })
                    };
                    let mut allow_dirs = Vec::new();
                    for p in &a.allow_dir {
                        allow_dirs.push(dir(p, false)?);
                    }
                    for p in &a.allow_dir_rw {
                        allow_dirs.push(dir(p, true)?);
                    }
                    Transport::Wasm {
                        path,
                        allow_network: a.allow_network.clone(),
                        allow_dirs,
                        env: a.env.iter().cloned().collect(),
                        time_budget: a.time_budget,
                    }
                }
                (None, None) => Transport::Container {
                    image: a.image.unwrap_or_default(),
                    digest: None,
                    runtime: a.runtime,
                    args: a.args,
                    env: a.env.into_iter().collect(),
                    run_args: a.run_args,
                    sandbox: a.sandbox.unwrap_or_default(),
                    egress: if a.no_network {
                        Some(Vec::new())
                    } else if a.egress.is_empty() {
                        None
                    } else {
                        Some(a.egress)
                    },
                },
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
            spec.warm_pool = a.warm.then_some(mcpgw_core::WarmPool { min: 1 });
            spec.secrets = a.secrets.into_iter().collect();
            spec.files = a.secret_files.into_iter().collect();
            spec.limits = mcpgw_core::Limits {
                memory: a.memory,
                cpus: a.cpus,
                pids: a.pids,
            };
            servers::add(
                &config,
                &path,
                spec,
                servers::AddOpts {
                    introspect: !a.no_introspect,
                    install: !a.no_cache,
                    accept_risk: a.accept_risk,
                    ..Default::default()
                },
            )
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
