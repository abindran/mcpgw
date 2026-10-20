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
    Arc::new(mcpgw_auth::oauth::Broker::new(
        config.oauth.clone(),
        tokens,
        lookup,
    ))
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

/// Run the gateway until a signal (or `mcpgw down`) stops it.
pub fn serve(config: &Config, store: Arc<SqliteStore>, stdio: bool) -> AnyResult<()> {
    let token = Token::load_or_create(&config.gateway.token_path())?;
    let servers = effective_servers(&config.servers, store.as_ref())?;
    let catalog = catalog(&store)?;
    runtime()?.block_on(async {
        let opts = ManagerOptions {
            idle_timeout: config.gateway.idle_timeout(),
            max_running: config.gateway.max_running(),
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
        capture_missing(&mgr, &servers, Duration::from_secs(30)).await;
        let res: AnyResult<()> = if stdio {
            tracing::info!(servers = servers.len(), "mcpgw serving on stdio");
            tokio::select! {
                r = mcpgw_router::serve_stdio(mgr.clone(), gateway_options(config)) => r.map_err(|e| e.to_string().into()),
                _ = shutdown_signal() => Ok(()),
            }
        } else {
            serve_http(config, token, mgr.clone(), broker).await
        };
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

async fn serve_http(
    config: &Config,
    token: Token,
    mgr: Arc<BackendManager>,
    broker: Arc<mcpgw_auth::oauth::Broker>,
) -> AnyResult<()> {
    let addr = config.gateway.listen();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr()?;
    let stop = Arc::new(tokio::sync::Notify::new());
    let admin = mcpgw_api::admin::router(mcpgw_api::admin::AdminState {
        manager: mgr.clone(),
        shutdown: stop.clone(),
    });
    let oauth = mcpgw_api::oauth::OAuthState {
        broker,
        base_url: format!("http://127.0.0.1:{}", local.port()),
    };
    let app = mcpgw_api::edge_full(
        mcpgw_router::http_router(mgr, gateway_options(config)),
        admin.merge(mcpgw_api::oauth::admin_routes(oauth.clone())),
        mcpgw_api::oauth::public_routes(oauth),
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
