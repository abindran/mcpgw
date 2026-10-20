//! Remote backends over HTTP (M2-04).
//!
//! [`BackendConn::connect_http`] speaks Streamable HTTP (2025-03-26 and
//! later). Servers that only implement the old HTTP+SSE transport
//! (2024-11-05: `GET` an event stream whose first `endpoint` event names the
//! URL to `POST` messages to) are bridged by [`sse_transport`]: a URL ending
//! in `/sse` goes there directly, and any other URL falls back to it when
//! the Streamable HTTP handshake fails for a reason other than auth.

use std::{collections::BTreeMap, time::Duration};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::{
    ClientLifecycleMode,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};

use crate::{BackendConn, ProtoError};

/// How to reach a remote backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSpec {
    /// Endpoint URL (`…/mcp`, or `…/sse` for legacy servers).
    pub url: String,
    /// Headers sent with every request (secrets already filled in).
    pub headers: BTreeMap<String, String>,
}

fn header_map(h: &BTreeMap<String, String>) -> Result<HeaderMap, ProtoError> {
    let mut m = HeaderMap::new();
    for (k, v) in h {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| ProtoError::Connect(format!("invalid header name {k:?}")))?;
        let mut value = HeaderValue::from_str(v)
            .map_err(|_| ProtoError::Connect(format!("invalid value for header {k}")))?;
        value.set_sensitive(true);
        m.insert(name, value);
    }
    Ok(m)
}

fn http_client() -> Result<reqwest::Client, ProtoError> {
    mcpgw_core::tls::install();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ProtoError::Connect(format!("http client: {e}")))
}

/// Whether a handshake error means "authenticate", not "wrong transport".
fn is_auth_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("401")
        || m.contains("403")
        || m.contains("unauthorized")
        || m.contains("auth required")
}

impl BackendConn {
    /// Connect to a remote MCP server: Streamable HTTP, or the legacy SSE
    /// bridge (see the module docs).
    pub async fn connect_http(spec: &HttpSpec, timeout: Duration) -> Result<Self, ProtoError> {
        let legacy_url = reqwest::Url::parse(&spec.url)
            .map(|u| u.path().trim_end_matches('/').ends_with("/sse"))
            .unwrap_or(false);
        if legacy_url {
            return Self::connect_sse(spec, timeout).await;
        }
        match Self::connect_streamable(spec, timeout).await {
            Err(ProtoError::Connect(first)) if is_auth_error(&first) => {
                Err(ProtoError::Connect(format!(
                    "{} requires authentication (HTTP 401/403); check the server's headers or connect its OAuth login",
                    spec.url
                )))
            }
            Err(ProtoError::Connect(first)) => {
                tracing::debug!(url = %spec.url, error = %first, "streamable HTTP failed; trying legacy SSE");
                Self::connect_sse(spec, timeout)
                    .await
                    .map_err(|e| ProtoError::Connect(format!("{first}; legacy SSE: {e}")))
            }
            other => other,
        }
    }

    async fn connect_streamable(spec: &HttpSpec, timeout: Duration) -> Result<Self, ProtoError> {
        let mut cfg = StreamableHttpClientTransportConfig::with_uri(spec.url.clone());
        cfg.custom_headers = header_map(&spec.headers)?
            .into_iter()
            .filter_map(|(k, v)| Some((k?, v)))
            .collect();
        let t = StreamableHttpClientTransport::with_client(http_client()?, cfg);
        Self::connect_transport(t, timeout, crate::backend::backend_lifecycle()).await
    }

    async fn connect_sse(spec: &HttpSpec, timeout: Duration) -> Result<Self, ProtoError> {
        let (sink, stream, guard) = tokio::time::timeout(timeout, sse_transport(spec))
            .await
            .map_err(|_| ProtoError::Timeout(timeout))??;
        let mut conn =
            Self::connect_transport((sink, stream), timeout, ClientLifecycleMode::Initialize)
                .await?;
        conn._tree.tasks = guard;
        Ok(conn)
    }
}

/// One server-sent event.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Event {
    pub name: String,
    pub data: String,
}

/// Incremental `text/event-stream` parser.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: String,
}

impl SseParser {
    /// Feed bytes; returns the events they complete.
    pub fn push(&mut self, chunk: &str) -> Vec<Event> {
        self.buf.push_str(&chunk.replace("\r\n", "\n"));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..end + 2).collect();
            let mut ev = Event::default();
            let mut data = Vec::new();
            for line in block.lines() {
                if line.starts_with(':') {
                    continue; // comment / keep-alive
                }
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => ev.name = value.to_string(),
                    "data" => data.push(value.to_string()),
                    _ => {}
                }
            }
            if data.is_empty() && ev.name.is_empty() {
                continue;
            }
            ev.data = data.join("\n");
            if ev.name.is_empty() {
                ev.name = "message".into();
            }
            out.push(ev);
        }
        out
    }
}

type Inbound = std::pin::Pin<Box<dyn futures::Stream<Item = ServerJsonRpcMessage> + Send>>;

/// Open the legacy SSE stream, wait for its `endpoint`, and return a sink
/// (POSTs messages) and a stream (server messages), plus the tasks to abort
/// when the connection goes.
pub(crate) async fn sse_transport(
    spec: &HttpSpec,
) -> Result<
    (
        futures::channel::mpsc::Sender<ClientJsonRpcMessage>,
        Inbound,
        Vec<tokio::task::JoinHandle<()>>,
    ),
    ProtoError,
> {
    let client = http_client()?;
    let headers = header_map(&spec.headers)?;
    let base =
        reqwest::Url::parse(&spec.url).map_err(|e| ProtoError::Connect(format!("bad url: {e}")))?;
    let resp = client
        .get(base.clone())
        .headers(headers.clone())
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| ProtoError::Connect(format!("GET {base}: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProtoError::Connect(format!(
            "{base} requires authentication (HTTP {})",
            status.as_u16()
        )));
    }
    if !status.is_success() {
        return Err(ProtoError::Connect(format!("GET {base} answered {status}")));
    }
    let mut body = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut pending = Vec::new();
    let endpoint = 'find: loop {
        let Some(chunk) = body.next().await else {
            return Err(ProtoError::Connect(
                "SSE stream ended before the endpoint event".into(),
            ));
        };
        let chunk = chunk.map_err(|e| ProtoError::Connect(format!("SSE stream: {e}")))?;
        for ev in parser.push(&String::from_utf8_lossy(&chunk)) {
            if ev.name == "endpoint" {
                break 'find base.join(ev.data.trim()).map_err(|e| {
                    ProtoError::Connect(format!("bad endpoint {:?}: {e}", ev.data))
                })?;
            }
            pending.push(ev);
        }
    };
    if endpoint.origin() != base.origin() {
        return Err(ProtoError::Connect(format!(
            "SSE endpoint {endpoint} is on another origin than {base}; refusing to send messages there"
        )));
    }

    let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(64);
    let reader = tokio::spawn(async move {
        let deliver = |ev: Event| -> Option<ServerJsonRpcMessage> {
            if ev.name != "message" {
                return None;
            }
            match serde_json::from_str(&ev.data) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(error = %e, "unreadable SSE message from backend");
                    None
                }
            }
        };
        for ev in pending {
            if let Some(m) = deliver(ev) {
                let _ = in_tx.send(m).await;
            }
        }
        loop {
            tokio::select! {
                _ = in_tx.closed() => break,
                chunk = body.next() => {
                    let Some(Ok(chunk)) = chunk else { break };
                    for ev in parser.push(&String::from_utf8_lossy(&chunk)) {
                        if let Some(m) = deliver(ev)
                            && in_tx.send(m).await.is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    });

    let (out_tx, mut out_rx) = futures::channel::mpsc::channel::<ClientJsonRpcMessage>(64);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.next().await {
            let r = client
                .post(endpoint.clone())
                .headers(headers.clone())
                .json(&msg)
                .send()
                .await;
            match r {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => tracing::warn!(status = %r.status(), "backend rejected an SSE message"),
                Err(e) => {
                    tracing::warn!(error = %e, "could not POST to the SSE backend");
                    break;
                }
            }
        }
    });

    let inbound: Inbound = Box::pin(futures::stream::poll_fn(move |cx| in_rx.poll_recv(cx)));
    Ok((out_tx, inbound, vec![reader, writer]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push("event: endpoint\r\ndata: /messages?s").is_empty());
        let evs = p.push("=1\r\n\r\n: keep-alive\n\ndata: {\"a\":\ndata: 1}\n\n");
        assert_eq!(
            evs,
            [
                Event {
                    name: "endpoint".into(),
                    data: "/messages?s=1".into()
                },
                Event {
                    name: "message".into(),
                    data: "{\"a\":\n1}".into()
                },
            ]
        );
    }

    #[test]
    fn auth_errors_are_not_retried_as_sse() {
        assert!(is_auth_error("HTTP 401 Unauthorized"));
        assert!(is_auth_error(
            "Transport error: Auth required, when send discover request"
        ));
        assert!(!is_auth_error("HTTP 405 Method Not Allowed"));
    }
}
