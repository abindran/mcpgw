//! Admin REST API v1 (M2-11), under `/api/v1`, with an OpenAPI 3.1
//! document at `/api/v1/openapi.json`.
//!
//! Same protection as `/mcp`: loopback Host, Origin check, bearer token.
//! Errors are `{"error": "<message>"}` with 400, 404 or 409. Secret values
//! can be set but never read back.

use std::{collections::BTreeSet, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use mcpgw_core::{Profile, SecretMeta, ServerSpec, Transport};
use mcpgw_runtime::{Approvals, BackendManager};
use mcpgw_store::Store;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::profiles::{Profiles, Source};

/// Secret storage as the API sees it (the daemon backs it with the vault).
pub trait SecretBackend: Send + Sync {
    /// Metadata of every secret.
    fn list(&self) -> Result<Vec<SecretMeta>, String>;
    /// Store a value (and an optional description).
    fn set(&self, name: &str, value: &str, description: Option<String>) -> Result<(), String>;
    /// Delete; returns whether it existed.
    fn remove(&self, name: &str) -> Result<bool, String>;
}

/// What the v1 API acts on.
#[derive(Clone)]
pub struct ApiState {
    /// Running servers.
    pub manager: Arc<BackendManager>,
    /// Persistent servers, manifests and profiles.
    pub store: Arc<dyn Store>,
    /// Servers defined in the config file (read-only here).
    pub config_servers: Arc<BTreeSet<String>>,
    /// Profiles and their endpoints.
    pub profiles: Arc<Profiles>,
    /// Secrets.
    pub secrets: Arc<dyn SecretBackend>,
    /// Pending approvals.
    pub approvals: Arc<Approvals>,
    /// Recent calls.
    pub calls: Arc<mcpgw_runtime::CallLog>,
    /// Daemon start, Unix seconds.
    pub started_at: u64,
    /// MCP endpoint base, e.g. `http://127.0.0.1:7777/mcp`.
    pub mcp_url: String,
}

type R = Result<Response, ApiError>;

/// An API error: status and message, sent as `{"error": "<message>"}`.
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn fail(status: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(status, msg.into())
}

fn ok(v: Value) -> R {
    Ok(Json(v).into_response())
}

fn no_content() -> R {
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ApiState {
    fn server_json(&self, spec: &ServerSpec) -> Value {
        let st = self.manager.status_of(&spec.name);
        let (transport, target) = match &spec.transport {
            Transport::Stdio { command, args, .. } => (
                "stdio",
                std::iter::once(command.clone())
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            Transport::Http { url, .. } => ("http", url.clone()),
        };
        json!({
            "name": spec.name,
            "transport": transport,
            "target": target,
            "alias": spec.alias,
            "enabled": spec.enabled,
            "stateful": spec.stateful,
            "source": if self.config_servers.contains(&spec.name) { "config" } else { "store" },
            "state": st.as_ref().map(|s| s.state.name()),
            "pid": st.as_ref().and_then(|s| s.pid),
            "restarts": st.as_ref().map(|s| s.restarts),
            "protocol_version": st.as_ref().and_then(|s| s.protocol_version.clone()),
            "last_error": st.as_ref().and_then(|s| s.last_error.clone()),
            "tools": self.manager.catalog().get(&spec.name).map(|m| m.tools.len()).unwrap_or(0),
            "pin": self.manager.catalog().get(&spec.name).map(|m| m.sha256),
        })
    }

    fn spec(&self, name: &str) -> Result<ServerSpec, ApiError> {
        self.manager
            .spec(name)
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("no server named {name:?}")))
    }

    fn writable(&self, name: &str) -> Result<(), ApiError> {
        if self.config_servers.contains(name) {
            Err(fail(
                StatusCode::CONFLICT,
                format!("server {name:?} is defined in the config file; change it there"),
            ))
        } else {
            Ok(())
        }
    }

    fn profile_json(&self, p: &Profile, source: Source) -> Value {
        let mut v = serde_json::to_value(p).unwrap_or_default();
        v["source"] = json!(source.name());
        v["url"] = json!(format!("{}/p/{}", self.mcp_url, p.name));
        v
    }
}

fn proto(e: mcpgw_runtime::ProtoError) -> ApiError {
    let m = e.to_string();
    if m.contains("unknown server") {
        fail(StatusCode::NOT_FOUND, m)
    } else {
        fail(StatusCode::CONFLICT, m)
    }
}

fn store_err(e: mcpgw_core::Error) -> ApiError {
    match e {
        mcpgw_core::Error::Config(m) => fail(StatusCode::BAD_REQUEST, m),
        other => fail(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

// ---- servers

async fn list_servers(State(s): State<ApiState>) -> R {
    let list: Vec<Value> = s
        .manager
        .names()
        .iter()
        .filter_map(|n| s.manager.spec(n))
        .map(|spec| s.server_json(&spec))
        .collect();
    ok(json!({ "servers": list }))
}

async fn get_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    let spec = s.spec(&name)?;
    let mut v = s.server_json(&spec);
    v["spec"] = serde_json::to_value(&spec).unwrap_or_default();
    ok(v)
}

async fn add_server(State(s): State<ApiState>, Json(spec): Json<ServerSpec>) -> R {
    s.writable(&spec.name)?;
    spec.validate().map_err(store_err)?;
    if s.manager.spec(&spec.name).is_some() {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("server {:?} already exists", spec.name),
        ));
    }
    if let Some(other) = s
        .manager
        .names()
        .iter()
        .filter_map(|n| s.manager.spec(n))
        .find(|o| o.prefix() == spec.prefix())
    {
        return Err(fail(
            StatusCode::CONFLICT,
            format!(
                "tool prefix {:?} is already used by server {:?}",
                spec.prefix(),
                other.name
            ),
        ));
    }
    s.store.upsert_server(&spec).map_err(store_err)?;
    s.manager.add_server(&spec).map_err(proto)?;
    Ok((StatusCode::CREATED, Json(s.server_json(&spec))).into_response())
}

async fn delete_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    s.spec(&name)?;
    s.writable(&name)?;
    s.manager.remove_server(&name).await.map_err(proto)?;
    s.store.remove_server(&name).map_err(store_err)?;
    s.store.remove_manifest(&name).map_err(store_err)?;
    s.manager.catalog().remove(&name);
    no_content()
}

async fn set_enabled(s: ApiState, name: String, on: bool) -> R {
    let mut spec = s.spec(&name)?;
    s.writable(&name)?;
    spec.enabled = on;
    s.store.upsert_server(&spec).map_err(store_err)?;
    s.manager.set_enabled(&name, on).await.map_err(proto)?;
    ok(s.server_json(&spec))
}

async fn enable_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    set_enabled(s, name, true).await
}

async fn disable_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    set_enabled(s, name, false).await
}

async fn restart_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    s.spec(&name)?;
    s.manager.reset(&name).map_err(proto)?;
    s.manager.stop(&name).await.map_err(proto)?;
    no_content()
}

async fn pin_server(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    s.spec(&name)?;
    let m = s.manager.approve_tools(&name).map_err(proto)?;
    ok(json!({"sha256": m.sha256, "tools": m.tools.len()}))
}

#[derive(Deserialize)]
struct Lines {
    lines: Option<usize>,
}

async fn server_logs(
    State(s): State<ApiState>,
    Path(name): Path<String>,
    Query(q): Query<Lines>,
) -> R {
    s.spec(&name)?;
    let mut lines = s.manager.stderr_tail(&name);
    if let Some(n) = q.lines {
        let skip = lines.len().saturating_sub(n);
        lines.drain(..skip);
    }
    ok(json!({ "lines": lines }))
}

async fn server_tools(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    s.spec(&name)?;
    let m = s.manager.catalog().get(&name);
    ok(json!({
        "pin": m.as_ref().map(|m| m.sha256.clone()),
        "tools": m.map(|m| m.tools.iter().map(|t| t.to_json()).collect::<Vec<_>>()).unwrap_or_default(),
    }))
}

// ---- tools

#[derive(Deserialize)]
struct ToolsQuery {
    profile: Option<String>,
}

async fn list_tools(State(s): State<ApiState>, Query(q): Query<ToolsQuery>) -> R {
    let scope = match &q.profile {
        None => mcpgw_router::Scope::default(),
        Some(name) => {
            let (p, _) = s
                .profiles
                .get(name)
                .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("no profile named {name:?}")))?;
            mcpgw_router::Scope {
                servers: (!p.servers.is_empty()).then(|| p.servers.iter().cloned().collect()),
                accounts: p.accounts.clone(),
                policy: p.policy.clone(),
                label: format!("profile {}", p.name),
            }
        }
    };
    let tools = mcpgw_router::list_tools_in(&s.manager, &scope).await;
    let tools: Vec<Value> = tools
        .into_iter()
        .map(|t| {
            let name = t["name"].as_str().unwrap_or_default().to_string();
            let server = mcpgw_router::split_name(&name)
                .and_then(|(p, _)| s.manager.server_for_prefix(p))
                .unwrap_or_default();
            let mut v = Value::Object(t);
            v["server"] = json!(server);
            v
        })
        .collect();
    ok(json!({ "tools": tools }))
}

// ---- profiles

async fn list_profiles(State(s): State<ApiState>) -> R {
    let list: Vec<Value> = s
        .profiles
        .all()
        .iter()
        .map(|(p, src)| s.profile_json(p, *src))
        .collect();
    ok(json!({ "profiles": list }))
}

async fn get_profile(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    let (p, src) = s
        .profiles
        .get(&name)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, format!("no profile named {name:?}")))?;
    ok(s.profile_json(&p, src))
}

async fn put_profile(
    State(s): State<ApiState>,
    Path(name): Path<String>,
    Json(p): Json<Profile>,
) -> R {
    if p.name != name {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("body names profile {:?}, URL {name:?}", p.name),
        ));
    }
    if let Some((_, Source::Config)) = s.profiles.get(&name) {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("profile {name:?} is defined in the config file"),
        ));
    }
    if let Some(unknown) = p.servers.iter().find(|x| s.manager.spec(x).is_none()) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("profile names unknown server {unknown:?}"),
        ));
    }
    let created = s.profiles.get(&name).is_none();
    s.store.upsert_profile(&p).map_err(store_err)?;
    s.profiles.put(p.clone());
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(s.profile_json(&p, Source::Store))).into_response())
}

async fn delete_profile(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    match s.profiles.get(&name) {
        None => Err(fail(
            StatusCode::NOT_FOUND,
            format!("no profile named {name:?}"),
        )),
        Some((_, Source::Config)) => Err(fail(
            StatusCode::CONFLICT,
            format!("profile {name:?} is defined in the config file"),
        )),
        Some(_) => {
            s.store.remove_profile(&name).map_err(store_err)?;
            s.profiles.remove(&name);
            no_content()
        }
    }
}

// ---- secrets

async fn list_secrets(State(s): State<ApiState>) -> R {
    let list = s
        .secrets
        .list()
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    ok(json!({ "secrets": list }))
}

#[derive(Deserialize)]
struct SecretBody {
    value: String,
    #[serde(default)]
    description: Option<String>,
}

async fn put_secret(
    State(s): State<ApiState>,
    Path(name): Path<String>,
    Json(b): Json<SecretBody>,
) -> R {
    mcpgw_core::validate_secret_name(&name).map_err(store_err)?;
    if b.value.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "empty value"));
    }
    s.secrets
        .set(&name, &b.value, b.description)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    no_content()
}

async fn delete_secret(State(s): State<ApiState>, Path(name): Path<String>) -> R {
    match s.secrets.remove(&name) {
        Ok(true) => no_content(),
        Ok(false) => Err(fail(
            StatusCode::NOT_FOUND,
            format!("no secret named {name:?}"),
        )),
        Err(e) => Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

// ---- runtime and approvals

async fn runtime(State(s): State<ApiState>) -> R {
    let status = s.manager.status().await;
    let running = status.iter().filter(|x| x.state.is_connected()).count();
    let endpoints: Vec<String> = std::iter::once(s.mcp_url.clone())
        .chain(
            s.profiles
                .all()
                .iter()
                .map(|(p, _)| format!("{}/p/{}", s.mcp_url, p.name)),
        )
        .collect();
    ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "started_at": s.started_at,
        "uptime_secs": now().saturating_sub(s.started_at),
        "servers": {"total": s.manager.names().len(), "running": running},
        "approvals_pending": s.approvals.list().iter().filter(|a| a.status == "pending").count(),
        "endpoints": endpoints,
    }))
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

async fn recent_calls(State(s): State<ApiState>, Query(q): Query<Limit>) -> R {
    ok(
        json!({ "calls": s.calls.recent(q.limit.unwrap_or(50).min(mcpgw_runtime::calls::CALL_LOG_LEN)) }),
    )
}

async fn list_approvals(State(s): State<ApiState>) -> R {
    ok(json!({ "approvals": s.approvals.list() }))
}

async fn approve(State(s): State<ApiState>, Path(id): Path<String>) -> R {
    s.approvals
        .decide(&id, true)
        .map_err(|e| fail(StatusCode::NOT_FOUND, e))?;
    no_content()
}

async fn deny(State(s): State<ApiState>, Path(id): Path<String>) -> R {
    s.approvals
        .decide(&id, false)
        .map_err(|e| fail(StatusCode::NOT_FOUND, e))?;
    no_content()
}

async fn spec() -> Json<Value> {
    Json(openapi())
}

/// The `/api/v1` routes (mount behind the edge's token check).
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/openapi.json", get(spec))
        .route("/api/v1/runtime", get(runtime))
        .route("/api/v1/servers", get(list_servers).post(add_server))
        .route(
            "/api/v1/servers/{name}",
            get(get_server).delete(delete_server),
        )
        .route("/api/v1/servers/{name}/enable", post(enable_server))
        .route("/api/v1/servers/{name}/disable", post(disable_server))
        .route("/api/v1/servers/{name}/restart", post(restart_server))
        .route("/api/v1/servers/{name}/pin", post(pin_server))
        .route("/api/v1/servers/{name}/logs", get(server_logs))
        .route("/api/v1/servers/{name}/tools", get(server_tools))
        .route("/api/v1/tools", get(list_tools))
        .route("/api/v1/profiles", get(list_profiles))
        .route(
            "/api/v1/profiles/{name}",
            get(get_profile).put(put_profile).delete(delete_profile),
        )
        .route("/api/v1/secrets", get(list_secrets))
        .route(
            "/api/v1/secrets/{name}",
            axum::routing::put(put_secret).delete(delete_secret),
        )
        .route("/api/v1/calls", get(recent_calls))
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/approvals/{id}/approve", post(approve))
        .route("/api/v1/approvals/{id}/deny", post(deny))
        .with_state(state)
}

fn schema<T: schemars::JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
    }
    v
}

/// The OpenAPI 3.1 document for `/api/v1`.
pub fn openapi() -> Value {
    let r = |s: &str| json!({ "$ref": format!("#/components/schemas/{s}") });
    let err =
        json!({"description": "Error", "content": {"application/json": {"schema": r("Error")}}});
    let body = |s: Value| json!({"required": true, "content": {"application/json": {"schema": s}}});
    let resp = |desc: &str, s: Value| json!({"description": desc, "content": {"application/json": {"schema": s}}});
    let name =
        json!([{"name": "name", "in": "path", "required": true, "schema": {"type": "string"}}]);
    let id = json!([{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}]);
    let op = |tag: &str,
              summary: &str,
              params: Value,
              req: Option<Value>,
              ok_code: &str,
              ok: Value| {
        let mut o = json!({
            "tags": [tag],
            "summary": summary,
            "responses": { ok_code: ok, "400": err.clone(), "404": err.clone(), "409": err.clone() }
        });
        if params.as_array().is_some_and(|a| !a.is_empty()) {
            o["parameters"] = params;
        }
        if let Some(b) = req {
            o["requestBody"] = b;
        }
        o
    };
    let none = json!([]);
    let empty = json!({"description": "Done"});
    let list = |key: &str, item: Value| json!({"type": "object", "properties": {key: {"type": "array", "items": item}}});
    let lines = json!([
        {"name": "name", "in": "path", "required": true, "schema": {"type": "string"}},
        {"name": "lines", "in": "query", "required": false, "schema": {"type": "integer", "minimum": 0}}
    ]);
    let profile_q = json!([{"name": "profile", "in": "query", "required": false, "schema": {"type": "string"}}]);
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "mcpgw admin API",
            "version": "1",
            "description": "Manage the local MCP gateway: servers, tools, profiles, secrets (metadata only), runtime and approvals. Send `Authorization: Bearer <token>` (`mcpgw token`)."
        },
        "servers": [{"url": "http://127.0.0.1:7777"}],
        "security": [{"bearer": []}],
        "paths": {
            "/api/v1/openapi.json": {"get": op("meta", "This document", none.clone(), None, "200", json!({"description": "OpenAPI document"}))},
            "/api/v1/runtime": {"get": op("runtime", "Daemon status", none.clone(), None, "200", resp("Runtime", r("Runtime")))},
            "/api/v1/servers": {
                "get": op("servers", "List servers with their state", none.clone(), None, "200", resp("Servers", list("servers", r("Server")))),
                "post": op("servers", "Add a server (stored, started on first use)", none.clone(), Some(body(r("ServerSpec"))), "201", resp("Added", r("Server")))
            },
            "/api/v1/servers/{name}": {
                "get": op("servers", "One server, with its spec", name.clone(), None, "200", resp("Server", r("Server"))),
                "delete": op("servers", "Remove a server", name.clone(), None, "204", empty.clone())
            },
            "/api/v1/servers/{name}/enable": {"post": op("servers", "Enable a server", name.clone(), None, "200", resp("Server", r("Server")))},
            "/api/v1/servers/{name}/disable": {"post": op("servers", "Disable a server (stops it, hides its tools)", name.clone(), None, "200", resp("Server", r("Server")))},
            "/api/v1/servers/{name}/restart": {"post": op("servers", "Clear failures and stop; starts fresh on its next call", name.clone(), None, "204", empty.clone())},
            "/api/v1/servers/{name}/pin": {"post": op("servers", "Accept a quarantined server's new tools", name.clone(), None, "200", resp("New pin", json!({"type": "object", "properties": {"sha256": {"type": "string"}, "tools": {"type": "integer"}}})))},
            "/api/v1/servers/{name}/logs": {"get": op("servers", "Recent stderr (redacted)", lines, None, "200", resp("Lines", list("lines", json!({"type": "string"}))))},
            "/api/v1/servers/{name}/tools": {"get": op("servers", "A server's pinned tools", name.clone(), None, "200", resp("Tools", json!({"type": "object", "properties": {"pin": {"type": ["string", "null"]}, "tools": {"type": "array", "items": {"type": "object"}}}})))},
            "/api/v1/tools": {"get": op("tools", "Public tools, optionally as a profile sees them", profile_q, None, "200", resp("Tools", list("tools", json!({"type": "object"}))))},
            "/api/v1/profiles": {"get": op("profiles", "List profiles", none.clone(), None, "200", resp("Profiles", list("profiles", r("Profile"))))},
            "/api/v1/profiles/{name}": {
                "get": op("profiles", "One profile", name.clone(), None, "200", resp("Profile", r("Profile"))),
                "put": op("profiles", "Create or replace a profile; its endpoint works at once", name.clone(), Some(body(r("Profile"))), "200", resp("Profile", r("Profile"))),
                "delete": op("profiles", "Remove a profile", name.clone(), None, "204", empty.clone())
            },
            "/api/v1/secrets": {"get": op("secrets", "Secret metadata (never values)", none.clone(), None, "200", resp("Secrets", list("secrets", r("SecretMeta"))))},
            "/api/v1/secrets/{name}": {
                "put": op("secrets", "Store a secret", name.clone(), Some(body(json!({"type": "object", "required": ["value"], "properties": {"value": {"type": "string", "writeOnly": true}, "description": {"type": "string"}}}))), "204", empty.clone()),
                "delete": op("secrets", "Delete a secret", name.clone(), None, "204", empty.clone())
            },
            "/api/v1/calls": {"get": op("runtime", "Recent tool calls, newest first (names and outcomes only)", json!([{"name": "limit", "in": "query", "required": false, "schema": {"type": "integer", "minimum": 1, "maximum": 200}}]), None, "200", resp("Calls", list("calls", r("Call"))))},
            "/api/v1/approvals": {"get": op("approvals", "Tool calls waiting for approval", none, None, "200", resp("Approvals", list("approvals", r("Approval"))))},
            "/api/v1/approvals/{id}/approve": {"post": op("approvals", "Approve a waiting call", id.clone(), None, "204", empty.clone())},
            "/api/v1/approvals/{id}/deny": {"post": op("approvals", "Refuse a waiting call", id, None, "204", empty)}
        },
        "components": {
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}},
            "schemas": {
                "Error": {"type": "object", "required": ["error"], "properties": {"error": {"type": "string"}}},
                "ServerSpec": schema::<ServerSpec>(),
                "Profile": schema::<Profile>(),
                "SecretMeta": schema::<SecretMeta>(),
                "Server": {"type": "object", "properties": {
                    "name": {"type": "string"}, "transport": {"enum": ["stdio", "http"]}, "target": {"type": "string"},
                    "alias": {"type": ["string", "null"]}, "enabled": {"type": "boolean"}, "stateful": {"type": "boolean"},
                    "source": {"enum": ["config", "store"]}, "state": {"type": ["string", "null"]},
                    "pid": {"type": ["integer", "null"]}, "restarts": {"type": ["integer", "null"]},
                    "protocol_version": {"type": ["string", "null"]}, "last_error": {"type": ["string", "null"]},
                    "tools": {"type": "integer"}, "pin": {"type": ["string", "null"]}, "spec": {"$ref": "#/components/schemas/ServerSpec"}
                }},
                "Runtime": {"type": "object", "properties": {
                    "version": {"type": "string"}, "pid": {"type": "integer"}, "started_at": {"type": "integer"},
                    "uptime_secs": {"type": "integer"}, "approvals_pending": {"type": "integer"},
                    "servers": {"type": "object", "properties": {"total": {"type": "integer"}, "running": {"type": "integer"}}},
                    "endpoints": {"type": "array", "items": {"type": "string"}}
                }},
                "Call": {"type": "object", "properties": {
                    "at_ms": {"type": "integer"}, "endpoint": {"type": "string"}, "tool": {"type": "string"},
                    "outcome": {"enum": ["ok", "tool_error", "error"]}, "duration_ms": {"type": "integer"},
                    "client": {"type": ["string", "null"]}
                }},
                "Approval": {"type": "object", "properties": {
                    "id": {"type": "string"}, "tool": {"type": "string"}, "arguments": {"type": "string"},
                    "endpoint": {"type": "string"}, "client": {"type": ["string", "null"]},
                    "created_at": {"type": "integer"}, "status": {"enum": ["pending", "approved", "denied"]}
                }}
            }
        }
    })
}
