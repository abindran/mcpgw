//! `mcpgw-api` — the daemon's HTTP edge: the MCP front door, and (from
//! M2-11) the admin REST API, SSE events and the web UI.
//!
//! Every request passes two checks before reaching a handler:
//!
//! 1. **Host** must be a loopback name (`localhost`, `127.0.0.1`, `[::1]`,
//!    any port). This defeats DNS rebinding. Otherwise `403`.
//! 2. **Origin**, when present, must be the daemon's own origin or listed in
//!    `allowed_origins`. Browsers send it on cross-site requests, so a web
//!    page can't drive the gateway. Otherwise `403`.
//!
//! `/mcp` additionally needs `Authorization: Bearer <loopback token>`,
//! otherwise `401` with `WWW-Authenticate: Bearer`. `/healthz` does not, so
//! the stdio shim can find a running daemon.

pub mod admin;
pub mod oauth;
pub mod profiles;
pub mod v1;

use std::{future::Future, sync::Arc};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use mcpgw_auth::Token;

/// Path of the MCP endpoint.
pub const MCP_PATH: &str = "/mcp";

/// Settings for [`edge`].
#[derive(Clone)]
pub struct EdgeOptions {
    /// Loopback bearer token required on `/mcp`.
    pub token: Token,
    /// Port the daemon listens on (its own origins are allowed).
    pub port: u16,
    /// Extra allowed `Origin` values, e.g. `"http://localhost:5173"`.
    pub allowed_origins: Vec<String>,
}

impl EdgeOptions {
    fn origin_allowed(&self, origin: &str) -> bool {
        let own = [
            format!("http://127.0.0.1:{}", self.port),
            format!("http://localhost:{}", self.port),
            format!("http://[::1]:{}", self.port),
        ];
        own.iter().any(|o| o == origin) || self.allowed_origins.iter().any(|o| o == origin)
    }
}

/// Whether a `Host` header names this machine's loopback interface.
pub fn host_is_loopback(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, tail)) if tail.is_empty() || is_port(tail) => ip,
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if is_port(&format!(":{port}")) => name,
            Some(_) => return false,
            None => host,
        }
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn is_port(s: &str) -> bool {
    s.strip_prefix(':')
        .is_some_and(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn deny(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, axum::Json(serde_json::json!({ "error": msg }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"mcpgw\""),
        );
    }
    r
}

async fn host_and_origin(
    State(opts): State<Arc<EdgeOptions>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()));
    if !host.is_some_and(host_is_loopback) {
        tracing::warn!(host = ?host, "rejected request: Host is not loopback");
        return deny(StatusCode::FORBIDDEN, "host not allowed");
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin.to_str().is_ok_and(|o| opts.origin_allowed(o));
        if !ok {
            tracing::warn!(origin = ?origin, "rejected request: Origin not allowed");
            return deny(StatusCode::FORBIDDEN, "origin not allowed");
        }
    }
    next.run(req).await
}

async fn bearer(
    State(opts): State<Arc<EdgeOptions>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| opts.token.verify_bearer(h));
    if !ok {
        return deny(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
    }
    next.run(req).await
}

/// Compose the edge: `mcp` mounted at [`MCP_PATH`] behind the token check,
/// `/healthz`, and the Host/Origin checks on everything.
pub fn edge(mcp: Router, opts: EdgeOptions) -> Router {
    edge_with(mcp, Router::new(), opts)
}

/// Like [`edge`], with extra token-protected routes (`/admin`).
pub fn edge_with(mcp: Router, extra: Router, opts: EdgeOptions) -> Router {
    edge_full(mcp, extra, Router::new(), opts)
}

/// The full edge: `mcp` and `protected` need the token; `public` (the OAuth
/// redirect) does not. Host and Origin checks cover all of them.
pub fn edge_full(mcp: Router, protected: Router, public: Router, opts: EdgeOptions) -> Router {
    let state = Arc::new(opts);
    let protected = Router::new()
        .nest_service(MCP_PATH, mcp)
        .merge(protected)
        .layer(middleware::from_fn_with_state(state.clone(), bearer));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(public)
        .merge(protected)
        .layer(middleware::from_fn_with_state(state, host_and_origin))
}

/// Serve `app` on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        for ok in [
            "localhost",
            "127.0.0.1:7777",
            "LOCALHOST:1",
            "[::1]",
            "[::1]:7777",
        ] {
            assert!(host_is_loopback(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "127.0.0.1.evil.com",
            "localhost.evil.com:7777",
            "10.0.0.1:7777",
            "[::2]:1",
            "localhost:",
            "localhost:x",
            "[::1]x",
            "",
        ] {
            assert!(!host_is_loopback(bad), "{bad}");
        }
    }
}
