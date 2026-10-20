//! A legacy MCP server on the old HTTP+SSE transport (2024-11-05), for
//! testing the gateway's SSE bridge.
//!
//! `GET /sse` opens an event stream whose first event is `endpoint` with
//! `/messages?sessionId=<id>`; JSON-RPC requests are `POST`ed there (answered
//! `202`), and replies come back as `message` events on the stream. Tools:
//! `echo {text}` and `header {name}` (the value of that request header on
//! the POST, to check header injection).

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use serde_json::{Value, json};

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, tokio::sync::mpsc::Sender<String>>,
    counter: u64,
    connections: u64,
    require: Option<(String, String)>,
}

type S = Arc<Mutex<Inner>>;

/// A running legacy SSE server (stops when dropped).
pub struct SseFake {
    inner: S,
    /// The SSE URL, e.g. `http://127.0.0.1:5000/sse`.
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SseFake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn authorized(s: &S, headers: &HeaderMap) -> bool {
    match &s.lock().unwrap().require {
        None => true,
        Some((n, v)) => headers.get(n.as_str()).and_then(|h| h.to_str().ok()) == Some(v.as_str()),
    }
}

async fn open(State(s): State<S>, headers: HeaderMap) -> Response {
    if !authorized(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
    let id = {
        let mut st = s.lock().unwrap();
        st.counter += 1;
        st.connections += 1;
        let id = format!("s{}", st.counter);
        st.sessions.insert(id.clone(), tx);
        id
    };
    let first = futures::stream::once(async move {
        Ok::<_, Infallible>(
            Event::default()
                .event("endpoint")
                .data(format!("/messages?sessionId={id}")),
        )
    });
    let rest = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|m| {
            (
                Ok::<_, Infallible>(Event::default().event("message").data(m)),
                rx,
            )
        })
    });
    Sse::new(futures::StreamExt::chain(first, rest)).into_response()
}

fn reply(msg: &Value, headers: &HeaderMap) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg["method"].as_str().unwrap_or_default();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let err = |code: i32, m: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}});
    let text = |t: String| ok(json!({"content": [{"type": "text", "text": t}]}));
    Some(match method {
        "initialize" => ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "sse-fake", "version": "0.1.0"}
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": [
            {"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
            {"name": "header", "description": "Show a request header", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}}
        ]})),
        "tools/call" => {
            let args = &msg["params"]["arguments"];
            match msg["params"]["name"].as_str() {
                Some("echo") => text(args["text"].as_str().unwrap_or_default().to_string()),
                Some("header") => text(
                    headers
                        .get(args["name"].as_str().unwrap_or_default())
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or_default()
                        .to_string(),
                ),
                _ => err(-32602, "tool not found"),
            }
        }
        _ => err(-32601, "method not found"),
    })
}

async fn message(
    State(s): State<S>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(msg): Json<Value>,
) -> Response {
    if !authorized(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let tx = q
        .get("sessionId")
        .and_then(|id| s.lock().unwrap().sessions.get(id).cloned());
    let Some(tx) = tx else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(r) = reply(&msg, &headers) {
        let _ = tx.send(r.to_string()).await;
    }
    StatusCode::ACCEPTED.into_response()
}

impl SseFake {
    /// Start one on a random loopback port. With `require = Some(("x-api-key",
    /// "k"))`, requests without that header get `401`.
    pub async fn start(require: Option<(&str, &str)>) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            require: require.map(|(n, v)| (n.to_ascii_lowercase(), v.to_string())),
            ..Default::default()
        }));
        let app = Router::new()
            .route("/sse", get(open))
            .route("/messages", post(message))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { inner, url, task }
    }

    /// SSE streams opened so far.
    pub fn connections(&self) -> u64 {
        self.inner.lock().unwrap().connections
    }

    /// Streams currently open.
    pub fn open_sessions(&self) -> usize {
        let mut st = self.inner.lock().unwrap();
        st.sessions.retain(|_, tx| !tx.is_closed());
        st.sessions.len()
    }
}
