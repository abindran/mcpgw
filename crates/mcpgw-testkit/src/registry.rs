//! A fake MCP Registry (`/v0/servers`) for install tests.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Inner {
    /// server.json entries; the last one per name is the latest
    entries: Vec<Value>,
    requests: Vec<String>,
}

type Shared = Arc<Mutex<Inner>>;

/// A running fake registry.
pub struct FakeRegistry {
    inner: Shared,
    /// Base URL, e.g. `http://127.0.0.1:1234`.
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn wrap(e: &Value, latest: bool) -> Value {
    json!({
        "server": e,
        "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active", "isLatest": latest}}
    })
}

fn is_latest(all: &[Value], e: &Value) -> bool {
    all.iter().rev().find(|x| x["name"] == e["name"]) == Some(e)
}

async fn list(
    State(s): State<Shared>,
    uri: Uri,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let mut st = s.lock().unwrap();
    st.requests.push(uri.to_string());
    let needle = q.get("search").cloned().unwrap_or_default().to_lowercase();
    let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(30);
    let servers: Vec<Value> = st
        .entries
        .iter()
        .filter(|e| is_latest(&st.entries, e))
        .filter(|e| {
            e["name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&needle)
                || e["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&needle)
        })
        .take(limit)
        .map(|e| wrap(e, true))
        .collect();
    let n = servers.len();
    Json(json!({"servers": servers, "metadata": {"count": n}})).into_response()
}

async fn version(
    State(s): State<Shared>,
    uri: Uri,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    let mut st = s.lock().unwrap();
    st.requests.push(uri.to_string());
    let found = st
        .entries
        .iter()
        .rfind(|e| {
            e["name"] == name.as_str() && (version == "latest" || e["version"] == version.as_str())
        })
        .cloned();
    match found {
        Some(e) => Json(wrap(&e, is_latest(&st.entries, &e))).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"title": "Not Found"}))).into_response(),
    }
}

impl FakeRegistry {
    /// Start one serving these `server.json` entries (later versions of a
    /// name after earlier ones).
    pub async fn start(entries: Vec<Value>) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            entries,
            ..Default::default()
        }));
        let app = Router::new()
            .route("/v0/servers", get(list))
            .route("/v0/servers/{name}/versions/{version}", get(version))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { inner, base, task }
    }

    /// Request paths (with query) seen so far.
    pub fn requests(&self) -> Vec<String> {
        self.inner.lock().unwrap().requests.clone()
    }
}
