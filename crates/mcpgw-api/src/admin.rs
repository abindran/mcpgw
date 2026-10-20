//! Daemon control endpoints used by the CLI (M1-13), under `/admin`.
//! Same protection as `/mcp`: loopback Host, Origin check, bearer token.
//! M2-11 grows this into the versioned REST API with OpenAPI.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use mcpgw_core::{ServerSpec, ToolManifest};
use mcpgw_runtime::{BackendManager, ProtoError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What the admin endpoints act on.
#[derive(Clone)]
pub struct AdminState {
    /// The running servers.
    pub manager: Arc<BackendManager>,
    /// Signalled by `POST /admin/shutdown`.
    pub shutdown: Arc<tokio::sync::Notify>,
}

/// One server in `GET /admin/status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerInfo {
    /// Name.
    pub name: String,
    /// Lifecycle state (`cold`, `ready`, `quarantined`, ...).
    pub state: String,
    /// Process id while running.
    pub pid: Option<u32>,
    /// Restarts after the first start.
    pub restarts: u32,
    /// Negotiated protocol version.
    pub protocol_version: Option<String>,
    /// Last error, or why it is disabled.
    pub last_error: Option<String>,
    /// Never stopped for idleness.
    pub stateful: bool,
    /// Tools in its pinned manifest.
    pub tools: usize,
}

/// `GET /admin/status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// mcpgw version.
    pub version: String,
    /// Daemon pid.
    pub pid: u32,
    /// Servers, by name.
    pub servers: Vec<ServerInfo>,
}

/// `POST /admin/servers` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddServer {
    /// The server.
    pub spec: ServerSpec,
    /// Its pinned tools, if captured at install.
    pub manifest: Option<ToolManifest>,
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

fn proto(e: ProtoError) -> Response {
    let msg = e.to_string();
    let status = if msg.contains("unknown server") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::CONFLICT
    };
    err(status, msg)
}

async fn status(State(s): State<AdminState>) -> Json<StatusReport> {
    let mut servers = Vec::new();
    for st in s.manager.status().await {
        let tools = s
            .manager
            .catalog()
            .get(&st.name)
            .map(|m| m.tools.len())
            .unwrap_or(0);
        servers.push(ServerInfo {
            state: st.state.name().to_string(),
            pid: st.pid,
            restarts: st.restarts,
            protocol_version: st.protocol_version,
            last_error: st.last_error,
            stateful: st.stateful,
            tools,
            name: st.name,
        });
    }
    Json(StatusReport {
        version: env!("CARGO_PKG_VERSION").into(),
        pid: std::process::id(),
        servers,
    })
}

async fn add(State(s): State<AdminState>, Json(body): Json<AddServer>) -> Response {
    if let Err(e) = body.spec.validate() {
        return err(StatusCode::BAD_REQUEST, e.to_string());
    }
    if let Some(m) = body.manifest {
        s.manager.catalog().record(m);
    }
    match s.manager.add_server(&body.spec) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => proto(e),
    }
}

async fn remove(State(s): State<AdminState>, Path(name): Path<String>) -> Response {
    match s.manager.remove_server(&name).await {
        Ok(()) => {
            s.manager.catalog().remove(&name);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => proto(e),
    }
}

async fn set_enabled(s: AdminState, name: String, on: bool) -> Response {
    match s.manager.set_enabled(&name, on).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => proto(e),
    }
}

async fn enable(State(s): State<AdminState>, Path(name): Path<String>) -> Response {
    set_enabled(s, name, true).await
}

async fn disable(State(s): State<AdminState>, Path(name): Path<String>) -> Response {
    set_enabled(s, name, false).await
}

async fn restart(State(s): State<AdminState>, Path(name): Path<String>) -> Response {
    if let Err(e) = s.manager.reset(&name) {
        return proto(e);
    }
    match s.manager.stop(&name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => proto(e),
    }
}

async fn pin(State(s): State<AdminState>, Path(name): Path<String>) -> Response {
    match s.manager.approve_tools(&name) {
        Ok(m) => Json(json!({"sha256": m.sha256, "tools": m.tools.len()})).into_response(),
        Err(e) => proto(e),
    }
}

#[derive(Deserialize)]
struct LogsQuery {
    lines: Option<usize>,
}

async fn logs(
    State(s): State<AdminState>,
    Path(name): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Response {
    if s.manager.status_of(&name).is_none() {
        return err(StatusCode::NOT_FOUND, format!("unknown server {name}"));
    }
    let mut lines = s.manager.stderr_tail(&name);
    if let Some(n) = q.lines {
        let skip = lines.len().saturating_sub(n);
        lines.drain(..skip);
    }
    Json(json!({ "lines": lines })).into_response()
}

async fn shutdown(State(s): State<AdminState>) -> Response {
    s.shutdown.notify_waiters();
    s.shutdown.notify_one();
    (StatusCode::ACCEPTED, Json(Value::Null)).into_response()
}

/// The `/admin` routes (mount behind the edge's checks).
pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/status", get(status))
        .route("/admin/servers", post(add))
        .route("/admin/servers/{name}", delete(remove))
        .route("/admin/servers/{name}/enable", post(enable))
        .route("/admin/servers/{name}/disable", post(disable))
        .route("/admin/servers/{name}/restart", post(restart))
        .route("/admin/servers/{name}/pin", post(pin))
        .route("/admin/servers/{name}/logs", get(logs))
        .route("/admin/shutdown", post(shutdown))
        .with_state(state)
}
