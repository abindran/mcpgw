//! `mcpgw-server` — the MCP server side of the gateway.
//!
//! Exposes the aggregated backend tools to LLM clients. It never imports
//! `rmcp` directly; all protocol access goes through `mcpgw-proto`.

use std::sync::Arc;

use mcpgw_client::{BackendManager, JsonObject, ProtoError};
use mcpgw_proto::front::{self, BoxFuture, FrontInfo, ToolProvider};

/// Client-facing protocol version this side presents (via `mcpgw-proto`).
pub fn client_facing_version() -> &'static str {
    mcpgw_proto::version::CLIENT_FACING
}

/// Routes front-side `tools/list` / `tools/call` to the backend manager.
pub struct GatewayProvider {
    manager: Arc<BackendManager>,
}

impl GatewayProvider {
    /// Wrap a running backend manager.
    pub fn new(manager: Arc<BackendManager>) -> Self {
        Self { manager }
    }
}

impl ToolProvider for GatewayProvider {
    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>> {
        Box::pin(self.manager.list_all_tools())
    }

    fn call_tool(
        &self,
        name: String,
        arguments: Option<JsonObject>,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>> {
        Box::pin(async move { self.manager.call_public(&name, arguments).await })
    }
}

/// Default identity advertised to clients.
pub fn front_info() -> FrontInfo {
    FrontInfo {
        name: "mcpgw".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(
            "Tools are aggregated from several MCP servers and named `<server>__<tool>`.".into(),
        ),
    }
}

/// Serve the gateway over any byte stream (tests, sockets).
pub async fn serve_io<T>(manager: Arc<BackendManager>, io: T) -> Result<(), ProtoError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    front::serve_io(Arc::new(GatewayProvider::new(manager)), front_info(), io).await
}

/// Serve the gateway on stdin/stdout until the client disconnects.
pub async fn serve_stdio(manager: Arc<BackendManager>) -> Result<(), ProtoError> {
    front::serve_stdio(Arc::new(GatewayProvider::new(manager)), front_info()).await
}
