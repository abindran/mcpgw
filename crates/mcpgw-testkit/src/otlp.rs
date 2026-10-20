//! A fake OTLP/HTTP collector (JSON protocol) for telemetry tests: it keeps
//! every export it receives.

use std::sync::{Arc, Mutex};

use axum::{Router, extract::State, http::StatusCode, routing::post};
use serde_json::Value;

type Shared = Arc<Mutex<Vec<(String, Value)>>>;

/// A running fake collector.
pub struct FakeOtlp {
    received: Shared,
    /// Base URL, for `OTEL_EXPORTER_OTLP_ENDPOINT`.
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeOtlp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn take(State(s): State<Shared>, uri: axum::http::Uri, body: String) -> StatusCode {
    let signal = uri.path().trim_start_matches("/v1/").to_string();
    match serde_json::from_str(&body) {
        Ok(v) => {
            s.lock().unwrap().push((signal, v));
            StatusCode::OK
        }
        Err(_) => StatusCode::BAD_REQUEST,
    }
}

impl FakeOtlp {
    /// Start one on a random loopback port.
    pub async fn start() -> Self {
        let received: Shared = Arc::default();
        let app = Router::new()
            .route("/v1/traces", post(take))
            .route("/v1/metrics", post(take))
            .route("/v1/logs", post(take))
            .with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            received,
            base,
            task,
        }
    }

    /// Every span received: (name, kind, traceId, spanId, parentSpanId, attributes).
    pub fn spans(&self) -> Vec<Value> {
        self.of("traces")
            .iter()
            .flat_map(|v| v["resourceSpans"].as_array().cloned().unwrap_or_default())
            .flat_map(|rs| rs["scopeSpans"].as_array().cloned().unwrap_or_default())
            .flat_map(|ss| ss["spans"].as_array().cloned().unwrap_or_default())
            .collect()
    }

    /// Names of every metric received.
    pub fn metric_names(&self) -> Vec<String> {
        self.of("metrics")
            .iter()
            .flat_map(|v| v["resourceMetrics"].as_array().cloned().unwrap_or_default())
            .flat_map(|rm| rm["scopeMetrics"].as_array().cloned().unwrap_or_default())
            .flat_map(|sm| sm["metrics"].as_array().cloned().unwrap_or_default())
            .filter_map(|m| m["name"].as_str().map(str::to_string))
            .collect()
    }

    /// Number of log records received.
    pub fn log_count(&self) -> usize {
        self.of("logs")
            .iter()
            .flat_map(|v| v["resourceLogs"].as_array().cloned().unwrap_or_default())
            .flat_map(|rl| rl["scopeLogs"].as_array().cloned().unwrap_or_default())
            .map(|sl| sl["logRecords"].as_array().map_or(0, Vec::len))
            .sum()
    }

    fn of(&self, signal: &str) -> Vec<Value> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s == signal)
            .map(|(_, v)| v.clone())
            .collect()
    }
}
