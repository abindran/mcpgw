//! A fake Swagger Petstore (`/api/v3`) for the OpenAPI adapter tests: a
//! small in-memory store, and a record of every request.

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use serde_json::{Value, json};

/// One request as received.
#[derive(Debug, Clone)]
pub struct Seen {
    /// `GET`, `POST`, …
    pub method: String,
    /// Path and query.
    pub uri: String,
    /// Headers (lowercase names).
    pub headers: Vec<(String, String)>,
    /// Body text.
    pub body: String,
}

#[derive(Default)]
struct Inner {
    pets: Vec<Value>,
    seen: Vec<Seen>,
}

type Shared = Arc<Mutex<Inner>>;

/// A running fake Petstore.
pub struct FakePetstore {
    inner: Shared,
    /// Base URL, `http://127.0.0.1:<port>/api/v3`.
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakePetstore {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn record(s: &Shared, method: &Method, uri: &Uri, headers: &HeaderMap, body: &Bytes) {
    s.lock().unwrap().seen.push(Seen {
        method: method.to_string(),
        uri: uri.to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: String::from_utf8_lossy(body).into_owned(),
    });
}

async fn pet(
    State(s): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&s, &method, &uri, &headers, &body);
    let Ok(mut p) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid input").into_response();
    };
    let mut st = s.lock().unwrap();
    if p.get("id").is_none() {
        p["id"] = json!(st.pets.len() as u64 + 1);
    }
    st.pets.retain(|x| x["id"] != p["id"]);
    st.pets.push(p.clone());
    Json(p).into_response()
}

async fn by_id(
    State(s): State<Shared>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&s, &method, &uri, &headers, &body);
    let mut st = s.lock().unwrap();
    let Ok(n) = id.parse::<u64>() else {
        return (StatusCode::BAD_REQUEST, "invalid ID supplied").into_response();
    };
    let pos = st.pets.iter().position(|p| p["id"] == n);
    match (method.as_str(), pos) {
        ("GET", Some(i)) => Json(st.pets[i].clone()).into_response(),
        ("DELETE", Some(i)) if headers.get("api_key").is_some() => {
            st.pets.remove(i);
            (StatusCode::OK, "pet deleted").into_response()
        }
        ("DELETE", Some(_)) => {
            (StatusCode::UNAUTHORIZED, "api_key header required").into_response()
        }
        ("POST", Some(i)) => {
            // updatePetWithForm: name and status in the query
            for (k, v) in uri
                .query()
                .unwrap_or_default()
                .split('&')
                .filter_map(|kv| kv.split_once('='))
            {
                st.pets[i][k] = json!(v);
            }
            Json(st.pets[i].clone()).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            json!({"message": "Pet not found"}).to_string(),
        )
            .into_response(),
    }
}

async fn find(
    State(s): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&s, &method, &uri, &headers, &body);
    let st = s.lock().unwrap();
    let q = uri.query().unwrap_or_default().to_string();
    let wanted: Vec<String> = q
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_string())
        .collect();
    let found: Vec<Value> = st
        .pets
        .iter()
        .filter(|p| {
            wanted.iter().any(|w| p["status"] == w.as_str())
                || p["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|t| wanted.iter().any(|w| t["name"] == w.as_str()))
        })
        .cloned()
        .collect();
    Json(found).into_response()
}

async fn other(
    State(s): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&s, &method, &uri, &headers, &body);
    match uri.path() {
        "/api/v3/store/inventory" => Json(json!({"available": 1, "sold": 0})).into_response(),
        // its own spec, like the real petstore3.swagger.io
        "/api/v3/openapi.json" => (
            [("content-type", "application/json")],
            include_str!("../../mcpgw-openapi/tests/fixtures/petstore.json"),
        )
            .into_response(),
        p if p.ends_with("/uploadImage") => {
            Json(json!({"code": 200, "message": format!("{} bytes", body.len())})).into_response()
        }
        "/api/v3/user/login" => (StatusCode::OK, "logged in user session:42").into_response(),
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

impl FakePetstore {
    /// Start one on a random loopback port.
    pub async fn start() -> Self {
        let inner: Shared = Arc::default();
        let app = Router::new()
            .route("/api/v3/pet", axum::routing::post(pet).put(pet))
            .route("/api/v3/pet/findByStatus", get(find))
            .route("/api/v3/pet/findByTags", get(find))
            .route("/api/v3/pet/{id}", any(by_id))
            .fallback(other)
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v3", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { inner, base, task }
    }

    /// Every request so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().seen.clone()
    }
}
