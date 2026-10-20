//! OAuth endpoints (M2-01).
//!
//! - `POST /admin/oauth/{provider}/start {account}`: start a flow, get the
//!   URL to open (token-protected)
//! - `GET /admin/oauth/flows/{state}`: poll it (token-protected)
//! - `GET /admin/oauth/connections`, `DELETE …/{provider}/{account}`
//! - `GET /oauth/callback?code&state`: where the browser lands. It can't
//!   carry the bearer token, so it is public; the unguessable `state` and
//!   the PKCE verifier (which never leaves the daemon) protect it.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
};
use mcpgw_auth::oauth::{Broker, FlowStatus};
use serde::Deserialize;
use serde_json::json;

/// Saves a discovered client registration (and its secret, if any).
pub type SaveClient =
    Arc<dyn Fn(&mcpgw_core::OAuthProvider, Option<&str>) -> Result<(), String> + Send + Sync>;

/// Finds the URL of a remote MCP server by name.
pub type ServerUrl = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Logging in to remote MCP servers that have no `[[oauth]]` app (M2-05).
#[derive(Clone)]
pub struct RemoteLogin {
    /// Server name → its URL, for HTTP servers.
    pub server_url: ServerUrl,
    /// Persist a registration.
    pub save_client: SaveClient,
    /// `gateway.client_metadata_url`, used when the server accepts client
    /// ID metadata documents.
    pub client_metadata_url: Option<String>,
}

/// What the OAuth endpoints need.
#[derive(Clone)]
pub struct OAuthState {
    /// The broker.
    pub broker: Arc<Broker>,
    /// `http://127.0.0.1:<port>`: the redirect URI is this plus
    /// `/oauth/callback`.
    pub base_url: String,
    /// Discovery for remote MCP servers.
    pub remote: Option<RemoteLogin>,
}

/// Discover, register and add a provider for remote server `name`.
async fn discover_provider(s: &OAuthState, name: &str, redirect: &str) -> Result<(), String> {
    let Some(remote) = &s.remote else {
        return Err(format!(
            "no OAuth app named {name:?}; add an [[oauth]] entry to the config"
        ));
    };
    let Some(url) = (remote.server_url)(name) else {
        return Err(format!(
            "no OAuth app or remote server named {name:?}; add an [[oauth]] entry, or `mcpgw add {name} --url …`"
        ));
    };
    use mcpgw_auth::discovery;
    let d = discovery::discover(&url).await?;
    let reg = discovery::register(&d, redirect, remote.client_metadata_url.as_deref()).await?;
    let secret_name = reg
        .client_secret
        .as_ref()
        .map(|_| format!("oauth-client/{name}/secret"));
    let p = discovery::provider(name, &d, &reg, secret_name);
    (remote.save_client)(&p, reg.client_secret.as_deref())?;
    tracing::info!(server = name, issuer = %d.issuer, method = reg.method, "registered OAuth client for remote server");
    s.broker.add_provider(p);
    Ok(())
}

#[derive(Deserialize, Default)]
struct StartBody {
    #[serde(default)]
    account: Option<String>,
}

async fn start(
    State(s): State<OAuthState>,
    Path(provider): Path<String>,
    body: Option<Json<StartBody>>,
) -> Response {
    let account = body
        .and_then(|b| b.0.account)
        .unwrap_or_else(|| "default".into());
    let redirect = format!("{}/oauth/callback", s.base_url);
    if s.broker.provider(&provider).is_none()
        && let Err(e) = discover_provider(&s, &provider, &redirect).await
    {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    match s.broker.start(&provider, &account, &redirect) {
        Ok((state, url)) => {
            Json(json!({"state": state, "url": url, "account": account})).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    }
}

async fn flow(State(s): State<OAuthState>, Path(state): Path<String>) -> Json<FlowStatus> {
    Json(s.broker.status(&state))
}

async fn connections(State(s): State<OAuthState>) -> Response {
    let store = s.broker.store();
    match store.list() {
        Ok(list) => {
            let items: Vec<_> = list
                .into_iter()
                .map(|(p, a)| {
                    let t = store.load(&p, &a).ok().flatten();
                    json!({
                        "provider": p,
                        "account": a,
                        "expires_at": t.as_ref().and_then(|t| t.expires_at),
                        "scope": t.as_ref().and_then(|t| t.scope.clone()),
                        "refreshable": t.as_ref().is_some_and(|t| t.refresh_token.is_some()),
                        "needs_reauth": t.as_ref().is_some_and(|t| t.needs_reauth),
                    })
                })
                .collect();
            Json(json!({ "connections": items })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

async fn disconnect(
    State(s): State<OAuthState>,
    Path((provider, account)): Path<(String, String)>,
) -> Response {
    match s.broker.store().remove(&provider, &account) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such connection"})),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn page(title: &str, body: &str) -> Html<String> {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>mcpgw: {t}</title>\
         <style>body{{font:16px system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem}}</style>\
         </head><body><h1>{t}</h1><p>{b}</p></body></html>",
        t = esc(title),
        b = esc(body)
    ))
}

async fn callback(State(s): State<OAuthState>, Query(q): Query<CallbackQuery>) -> Response {
    let Some(state) = q.state else {
        return (
            StatusCode::BAD_REQUEST,
            page("Login failed", "The redirect had no state parameter."),
        )
            .into_response();
    };
    let error = q.error.map(|e| match q.error_description {
        Some(d) => format!("{e}: {d}"),
        None => e,
    });
    match s
        .broker
        .complete(&state, q.code.as_deref(), error.as_deref())
        .await
    {
        FlowStatus::Done { provider, account } => page(
            "Connected",
            &format!("mcpgw is connected to {provider} ({account}). You can close this tab."),
        )
        .into_response(),
        FlowStatus::Error { message } => {
            (StatusCode::BAD_REQUEST, page("Login failed", &message)).into_response()
        }
        FlowStatus::Pending => page("Still working", "Try again in a moment.").into_response(),
    }
}

/// Token-protected routes.
pub fn admin_routes(state: OAuthState) -> Router {
    Router::new()
        .route("/admin/oauth/{provider}/start", post(start))
        .route("/admin/oauth/flows/{state}", get(flow))
        .route("/admin/oauth/connections", get(connections))
        .route(
            "/admin/oauth/connections/{provider}/{account}",
            delete(disconnect),
        )
        .with_state(state)
}

/// Public routes (the browser redirect).
pub fn public_routes(state: OAuthState) -> Router {
    Router::new()
        .route("/oauth/callback", get(callback))
        .with_state(state)
}
