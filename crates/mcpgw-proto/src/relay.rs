//! Message relay between a stdio MCP client and the daemon's Streamable HTTP
//! endpoint (the `mcpgw connect` shim, M1-02).
//!
//! The relay works on whole JSON-RPC messages, not bytes: rmcp's stdio
//! transport parses what the client writes and its Streamable HTTP client
//! transport turns each message into the right request (sessions for
//! 2025-11-25, `MCP-Protocol-Version` and the 2026-07-28 `Mcp-Method` /
//! `Mcp-Name` headers, SSE streams). Nothing is interpreted or rewritten.

use rmcp::{
    RoleClient, RoleServer,
    transport::{
        IntoTransport, StreamableHttpClientTransport, Transport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};

use crate::ProtoError;

/// Relay between this process's stdin/stdout and `url` until either side
/// closes. `token` is sent as `Authorization: Bearer`.
pub async fn stdio_to_http(url: &str, token: &str) -> Result<(), ProtoError> {
    let stdio = IntoTransport::<RoleServer, std::io::Error, _>::into_transport((
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    relay(stdio, url, token).await
}

/// Relay between an arbitrary byte stream (tests) and `url`.
pub async fn io_to_http<T>(io: T, url: &str, token: &str) -> Result<(), ProtoError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let local = IntoTransport::<RoleServer, std::io::Error, _>::into_transport(io);
    relay(local, url, token).await
}

async fn relay<L>(mut local: L, url: &str, token: &str) -> Result<(), ProtoError>
where
    L: Transport<RoleServer>,
{
    let cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .auth_header(token.to_string());
    let mut remote: StreamableHttpClientTransport<_> =
        StreamableHttpClientTransport::from_config(cfg);
    loop {
        tokio::select! {
            from_client = local.receive() => match from_client {
                Some(msg) => Transport::<RoleClient>::send(&mut remote, msg)
                    .await
                    .map_err(|e| ProtoError::Other(format!("send to daemon: {e}")))?,
                None => break,
            },
            from_daemon = Transport::<RoleClient>::receive(&mut remote) => match from_daemon {
                Some(msg) => local
                    .send(msg)
                    .await
                    .map_err(|e| ProtoError::Other(format!("send to client: {e}")))?,
                None => {
                    let _ = local.close().await;
                    return Err(ProtoError::Closed);
                }
            },
        }
    }
    let _ = Transport::<RoleClient>::close(&mut remote).await;
    Ok(())
}
