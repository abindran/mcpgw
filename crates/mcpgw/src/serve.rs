//! `mcpgw serve`: the daemon (HTTP front door and admin API) or the
//! single-client stdio gateway.

use std::{sync::Arc, time::Duration};

use mcpgw_auth::Token;
use mcpgw_config::Config;
use mcpgw_core::ServerSpec;
use mcpgw_runtime::{BackendManager, ManagerOptions};
use mcpgw_store::{SqliteStore, Store, effective_servers};

use crate::{AnyResult, daemon, secrets};

/// A multi-threaded tokio runtime for one command.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Resolves on Ctrl-C, or SIGTERM on Unix.
pub async fn shutdown_signal() {
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
        accounts: config.gateway.accounts.clone(),
        login: None,
        servers: None,
        policy: crate::policy::loaded(&config.gateway.policy, "default"),
        label: "the default endpoint".into(),
        approvals: Some(approvals()),
        approval_timeout: approval_timeout(config),
        calls: Some(call_log()),
        audit: auditor(),
        no_prewarm: config.gateway.prewarm == Some(false),
        tap: None,
        project: None,
        notice: None,
    }
}

/// Recent calls on every endpoint (M2-12).
static AUDITOR: std::sync::OnceLock<mcpgw_core::audit::Auditor> = std::sync::OnceLock::new();

/// The daemon's audit log writer, once `serve` has set it up (M2-13).
pub fn auditor() -> Option<mcpgw_core::audit::Auditor> {
    AUDITOR.get().cloned()
}

/// Append to the store's audit chain; failures are logged, never fatal.
fn install_auditor(store: Arc<SqliteStore>) {
    let a: mcpgw_core::audit::Auditor = Arc::new(move |ev| {
        if let Err(e) = store.append_audit(&ev) {
            tracing::error!(error = %e, kind = %ev.kind, "could not write the audit log");
        }
    });
    if AUDITOR.set(a.clone()).is_ok() {
        approvals().set_auditor(a);
    }
}

pub fn call_log() -> Arc<mcpgw_runtime::CallLog> {
    static L: std::sync::OnceLock<Arc<mcpgw_runtime::CallLog>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default).clone()
}

/// The daemon's pending approvals, shared by every endpoint (M2-09).
pub fn approvals() -> Arc<mcpgw_runtime::Approvals> {
    static A: std::sync::OnceLock<Arc<mcpgw_runtime::Approvals>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default).clone()
}

fn approval_timeout(config: &Config) -> Duration {
    config
        .gateway
        .approval_timeout
        .map(|d| d.get())
        .unwrap_or(Duration::from_secs(120))
}

/// Profiles from the config file and the store (the file wins on a clash).
pub fn profiles(config: &Config, store: &dyn Store) -> AnyResult<Vec<mcpgw_core::Profile>> {
    let mut out = config.profiles.clone();
    for p in store.profiles()? {
        if !out.iter().any(|c| c.name == p.name) {
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Options for a profile's endpoint: its servers, mode, pinned tools, and
/// accounts (over the gateway's).
fn profile_options(config: &Config, p: &mcpgw_core::Profile) -> mcpgw_router::GatewayOptions {
    let mut accounts = config.gateway.accounts.clone();
    accounts.extend(p.accounts.clone());
    mcpgw_router::GatewayOptions {
        mode: p.mode,
        pinned: p.pinned_tools.clone(),
        accounts,
        servers: (!p.servers.is_empty()).then(|| p.servers.iter().cloned().collect()),
        login: None,
        policy: crate::policy::loaded(&p.policy, &p.name),
        label: format!("profile {}", p.name),
        approvals: Some(approvals()),
        approval_timeout: approval_timeout(config),
        calls: Some(call_log()),
        audit: auditor(),
        no_prewarm: config.gateway.prewarm == Some(false),
        tap: None,
        project: None,
        notice: None,
    }
}

/// The vault for `config`, shared by secret injection and OAuth.
pub fn vault(config: &Config) -> Arc<secrets::SharedVault> {
    secrets::SharedVault::new(
        config.gateway.vault_path(),
        mcpgw_vault::KeySource::from_env(),
    )
}

/// The vault-backed secret source for `config`.
pub fn resolver(config: &Config) -> Arc<dyn mcpgw_runtime::SecretResolver> {
    Arc::new(secrets::VaultResolver::shared(vault(config)))
}

/// The OAuth broker for `config`'s `[[oauth]]` apps, storing tokens in `vault`.
pub fn broker(config: &Config, vault: Arc<secrets::SharedVault>) -> Arc<mcpgw_auth::oauth::Broker> {
    let saved = saved_clients(&vault);
    let tokens = Arc::new(secrets::VaultTokens::new(vault.clone()));
    let lookup: mcpgw_auth::oauth::SecretLookup = Arc::new(move |name: &str| {
        use secrecy::ExposeSecret;
        vault
            .with(false, |v| match v {
                Some(v) => v.get(name).map_err(|e| e.to_string()),
                None => Ok(None),
            })
            .ok()
            .flatten()
            .map(|s| s.expose_secret().to_string())
    });
    let b = Arc::new(mcpgw_auth::oauth::Broker::new(
        config.oauth.clone(),
        tokens,
        lookup,
    ));
    for p in saved {
        if b.provider(&p.name).is_none() {
            b.add_provider(p);
        }
    }
    b
}

/// Vault prefix for OAuth clients registered with remote MCP servers.
const CLIENT_PREFIX: &str = "oauth-client/";

/// Clients registered with remote MCP servers in earlier runs (M2-05).
fn saved_clients(vault: &secrets::SharedVault) -> Vec<mcpgw_core::OAuthProvider> {
    use secrecy::ExposeSecret;
    vault
        .with(false, |v| {
            let Some(v) = v else { return Ok(Vec::new()) };
            let mut out = Vec::new();
            for (name, ..) in v.list() {
                let Some(server) = name.strip_prefix(CLIENT_PREFIX) else {
                    continue;
                };
                if server.contains('/') {
                    continue; // a client secret
                }
                if let Some(json) = v.get(&name).map_err(|e| e.to_string())?
                    && let Ok(p) = serde_json::from_str(json.expose_secret())
                {
                    out.push(p);
                }
            }
            Ok(out)
        })
        .unwrap_or_default()
}

/// Discovery and registration for the remote MCP servers in `mgr`.
fn remote_login(
    config: &Config,
    mgr: &Arc<BackendManager>,
    vault: Arc<secrets::SharedVault>,
) -> mcpgw_api::oauth::RemoteLogin {
    let m = mgr.clone();
    let server_url: mcpgw_api::oauth::ServerUrl =
        Arc::new(move |name: &str| match m.spec(name)?.transport {
            mcpgw_core::Transport::Http { url, .. } => Some(url),
            _ => None,
        });
    let save_client: mcpgw_api::oauth::SaveClient = Arc::new(move |p, secret| {
        let json = serde_json::to_string(p).map_err(|e| e.to_string())?;
        vault.with(true, |v| {
            let v = v.expect("created");
            v.set(
                &format!("{CLIENT_PREFIX}{}", p.name),
                &secrecy::SecretString::from(json),
            )
            .map_err(|e| e.to_string())?;
            if let Some(s) = secret {
                v.set(
                    &format!("{CLIENT_PREFIX}{}/secret", p.name),
                    &secrecy::SecretString::from(s.to_string()),
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    });
    mcpgw_api::oauth::RemoteLogin {
        server_url,
        save_client,
        client_metadata_url: config.gateway.client_metadata_url.clone(),
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

/// A `mcpgw dev` session (M4-10): the server under development and what
/// to watch for edits.
pub struct Dev {
    /// The server's name.
    pub name: String,
    /// Files and directories whose changes restart it.
    pub watch: Vec<std::path::PathBuf>,
}

/// Run the gateway until a signal (or `mcpgw down`) stops it.
pub fn serve(
    config: &Config,
    store: Arc<SqliteStore>,
    stdio: bool,
    profile: Option<&str>,
) -> AnyResult<()> {
    serve_with(config, store, stdio, profile, None)
}

/// [`serve`], optionally as a `mcpgw dev` session.
pub fn serve_with(
    config: &Config,
    store: Arc<SqliteStore>,
    stdio: bool,
    profile: Option<&str>,
    dev: Option<Dev>,
) -> AnyResult<()> {
    let token = Token::load_or_create(&config.gateway.token_path())?;
    let servers = effective_servers(&config.servers, store.as_ref())?;
    let profiles = profiles(config, store.as_ref())?;
    for p in &profiles {
        for s in p
            .servers
            .iter()
            .filter(|s| !servers.iter().any(|x| &x.name == *s))
        {
            tracing::warn!(profile = %p.name, server = %s, "profile names a server that does not exist");
        }
    }
    let stdio_opts = match profile {
        None => gateway_options(config),
        Some(name) => {
            let p = profiles
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| format!("no profile named {name:?}"))?;
            profile_options(config, p)
        }
    };
    let catalog = catalog(&store)?;
    install_auditor(store.clone());
    // Containers are labelled with this store, so a later daemon can remove
    // the ones a crashed predecessor left running.
    let instance = mcpgw_core::audit::args_digest(&serde_json::json!(
        config.gateway.store_path().to_string_lossy()
    ));
    mcpgw_runtime::container::set_instance(&instance[..16]);
    runtime()?.block_on(async {
        tokio::spawn(async {
            let n = mcpgw_runtime::container::sweep().await;
            if n > 0 {
                tracing::warn!(removed = n, "removed containers left by an earlier daemon");
            }
        });
        let opts = ManagerOptions {
            idle_timeout: config.gateway.idle_timeout(),
            max_running: config.gateway.max_running(),
            call_timeout: config.gateway.call_timeout(),
            max_call_time: config.gateway.max_call_time(),
            // `cargo run` may build first
            connect_timeout: if dev.is_some() {
                Duration::from_secs(300)
            } else {
                ManagerOptions::default().connect_timeout
            },
            ..ManagerOptions::default()
        };
        let vault = vault(config);
        let broker = broker(config, vault.clone());
        let resolver = secrets::VaultResolver::shared(vault.clone())
            .with_refresh(broker.clone(), tokio::runtime::Handle::current());
        let mgr = Arc::new(BackendManager::with_secrets(
            &servers,
            opts,
            catalog,
            Some(Arc::new(resolver)),
        ));
        let refresher = tokio::spawn(refresh_loop(broker.clone(), config.gateway.oauth_refresh_interval()));
        // Servers start on first use; only those never seen are started now.
        // A dev session starts its server itself and restarts it on edits.
        let watcher = match &dev {
            Some(d) => Some(tokio::spawn(crate::dev::watch(
                mgr.clone(),
                d.name.clone(),
                d.watch.clone(),
            ))),
            None => {
                capture_missing(&mgr, &servers, Duration::from_secs(30)).await;
                None
            }
        };
        let res: AnyResult<()> = if stdio {
            tracing::info!(servers = servers.len(), "mcpgw serving on stdio");
            tokio::select! {
                r = mcpgw_router::serve_stdio(mgr.clone(), stdio_opts) => r.map_err(|e| e.to_string().into()),
                _ = shutdown_signal() => Ok(()),
            }
        } else {
            let login = remote_login(config, &mgr, vault.clone());
            serve_http(
                config,
                mgr.clone(),
                Http {
                    token,
                    broker,
                    login,
                    store: store.clone(),
                    vault: vault.clone(),
                },
                dev.as_ref(),
            ).await
        };
        if let Some(w) = watcher {
            w.abort();
        }
        refresher.abort();
        mgr.shutdown().await;
        res
    })
}

/// Refresh OAuth tokens shortly before they expire, so a server started
/// later gets a valid one and connections show `needs login` when a
/// refresh is refused.
async fn refresh_loop(broker: Arc<mcpgw_auth::oauth::Broker>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        let b = broker.clone();
        let due =
            tokio::task::spawn_blocking(move || b.due_connections(mcpgw_auth::oauth::REFRESH_LEAD))
                .await
                .unwrap_or_default();
        for (p, a) in due {
            if let Err(e) = broker.refresh(&p, &a).await {
                tracing::warn!(provider = %p, account = %a, error = %e, "background token refresh failed");
            }
        }
    }
}

/// After a login for `(provider, account)`, clear the backoff of every
/// server that uses it, so the next call starts it with the new token.
fn reset_after_login(mgr: &BackendManager, provider: &str, account: &str) {
    let secret = mcpgw_core::oauth_secret_name(provider, account);
    for name in mgr.names() {
        let Some(spec) = mgr.spec(&name) else {
            continue;
        };
        let uses = name == provider
            || spec.secret_refs().contains(&secret)
            || spec.for_account(account).secret_refs().contains(&secret);
        if !uses {
            continue;
        }
        let key = if account == mcpgw_core::DEFAULT_ACCOUNT {
            name.clone()
        } else {
            format!("{name}@{account}")
        };
        for k in [&name, &key] {
            let _ = mgr.reset(k);
        }
        tracing::info!(server = %name, %provider, %account, "login completed; server will start fresh");
    }
}

/// What the HTTP front door is built from.
struct Http {
    token: Token,
    broker: Arc<mcpgw_auth::oauth::Broker>,
    login: mcpgw_api::oauth::RemoteLogin,
    store: Arc<SqliteStore>,
    vault: Arc<secrets::SharedVault>,
}

async fn serve_http(
    config: &Config,
    mgr: Arc<BackendManager>,
    http: Http,
    dev: Option<&Dev>,
) -> AnyResult<()> {
    let Http {
        token,
        broker,
        login,
        store,
        vault,
    } = http;
    let addr = config.gateway.listen();
    let token_text = token.expose().to_string();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr()?;
    let stop = Arc::new(tokio::sync::Notify::new());
    let admin = mcpgw_api::admin::router(mcpgw_api::admin::AdminState {
        manager: mgr.clone(),
        shutdown: stop.clone(),
        approvals: approvals(),
    });
    let m = mgr.clone();
    let oauth = mcpgw_api::oauth::OAuthState {
        broker,
        base_url: format!("http://127.0.0.1:{}", local.port()),
        remote: Some(login),
        on_login: Some(Arc::new(move |p: &str, a: &str| {
            if let Some(au) = auditor() {
                au(mcpgw_core::AuditEvent::new(
                    "auth.login",
                    "browser",
                    &format!("{p}@{a}"),
                    serde_json::json!({}),
                ));
            }
            reset_after_login(&m, p, a)
        })),
    };
    let link_state = oauth.clone();
    let link: mcpgw_router::login::LoginLink = Arc::new(move |p: String, a: String| {
        let s = link_state.clone();
        Box::pin(async move { mcpgw_api::oauth::begin_login(&s, &p, &a).await })
    });
    let opts = mcpgw_router::GatewayOptions {
        login: Some(link.clone()),
        tap: dev.map(|_| crate::dev::tap()),
        ..gateway_options(config)
    };
    // the inspector calls tools as the default endpoint (M4-10), or as a
    // project's endpoint when it names one
    let invoke: mcpgw_api::v1::Invoke = {
        let default = Arc::new(mcpgw_router::GatewayProvider::with_options(
            mgr.clone(),
            opts.clone(),
        ));
        let (mgr, opts) = (mgr.clone(), opts.clone());
        Arc::new(move |name, args, project| {
            let p = match project {
                None => default.clone(),
                Some(ep) => Arc::new(mcpgw_router::GatewayProvider::with_options(
                    mgr.clone(),
                    mcpgw_router::GatewayOptions {
                        label: ep.label,
                        project: Some(ep.id),
                        notice: Some(ep.notice),
                        ..opts.clone()
                    },
                )),
            };
            Box::pin(async move {
                use mcpgw_proto::front::ToolProvider;
                p.call_tool(name, args, mcpgw_proto::front::CallContext::default())
                    .await
                    .map_err(|e| e.to_string())
            })
        })
    };
    // /mcp is the default endpoint; /mcp/p/<name> resolves profiles per
    // request, so profiles added through the API work at once
    let build: mcpgw_api::profiles::ProfileService = {
        let (cfg, mgr, link) = (config.clone(), mgr.clone(), link.clone());
        Arc::new(move |p: &mcpgw_core::Profile| {
            let o = mcpgw_router::GatewayOptions {
                login: Some(link.clone()),
                ..profile_options(&cfg, p)
            };
            mcpgw_router::http_router(mgr.clone(), o)
        })
    };
    let profiles =
        mcpgw_api::profiles::Profiles::new(config.profiles.clone(), store.profiles()?, build);
    // /mcp/project/<id>: a project's .mcpgw.toml servers with the global
    // ones, as the default endpoint presents them
    let build_project: mcpgw_api::projects::ProjectService = {
        let (cfg, mgr, link) = (config.clone(), mgr.clone(), link.clone());
        Arc::new(move |p: mcpgw_api::projects::ProjectEndpoint| {
            let o = mcpgw_router::GatewayOptions {
                login: Some(link.clone()),
                label: p.label,
                project: Some(p.id),
                notice: Some(p.notice),
                ..gateway_options(&cfg)
            };
            mcpgw_router::http_router(mgr.clone(), o)
        })
    };
    let mcp_url = format!("http://{local}{}", mcpgw_api::MCP_PATH);
    let projects = mcpgw_api::projects::Projects::new(
        mgr.clone(),
        store.clone(),
        build_project,
        auditor(),
        mcp_url.clone(),
    );
    projects.start().await;
    let mcp = mcpgw_router::http_router(mgr.clone(), opts)
        .merge(profiles.routes())
        .merge(projects.routes());
    let api = mcpgw_api::v1::router(mcpgw_api::v1::ApiState {
        manager: mgr.clone(),
        store: store.clone(),
        config_servers: Arc::new(config.servers.iter().map(|s| s.name.clone()).collect()),
        profiles,
        secrets: Arc::new(secrets::ApiSecrets::new(vault, store.clone())),
        approvals: approvals(),
        calls: call_log(),
        audit: auditor(),
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        mcp_url,
        invoke: Some(invoke),
        projects,
    });
    let app = mcpgw_api::edge_full(
        mcp,
        admin
            .merge(mcpgw_api::oauth::admin_routes(oauth.clone()))
            .merge(api),
        mcpgw_api::oauth::public_routes(oauth).merge(mcpgw_api::ui::routes()),
        mcpgw_api::EdgeOptions {
            token,
            port: local.port(),
            allowed_origins: config.gateway.allowed_origins.clone(),
        },
    );
    let info_path = daemon::info_path(config);
    daemon::DaemonInfo::current(local).write(&info_path)?;
    eprintln!("mcpgw: listening on http://{local}{}", mcpgw_api::MCP_PATH);
    if let Some(d) = dev {
        use std::io::Write;
        let watched: Vec<String> = d.watch.iter().map(|p| p.display().to_string()).collect();
        println!(
            "mcpgw dev: serving {} at http://{local}{}",
            d.name,
            mcpgw_api::MCP_PATH
        );
        println!("inspector: http://{local}/ui#token={}", token_text);
        println!("watching {} (Ctrl-C to stop)", watched.join(", "));
        let _ = std::io::stdout().flush();
    }
    tracing::info!(%local, "mcpgw serving on http");
    let shutdown = async move {
        tokio::select! {
            _ = shutdown_signal() => {}
            _ = stop.notified() => tracing::info!("shutdown requested"),
        }
    };
    let res = mcpgw_api::serve(listener, app, shutdown).await;
    // Only remove the file if it is still ours (a newer daemon may own it).
    if daemon::DaemonInfo::read(&info_path).is_some_and(|i| i.pid == std::process::id()) {
        let _ = std::fs::remove_file(&info_path);
    }
    res?;
    Ok(())
}
