//! In-process tool servers (M4-01): something that lists and runs tools
//! without being an MCP server itself (a Wasm component) is served over an
//! in-memory MCP session, so it gets the same handshake, catalog, pinning,
//! policy and lifecycle as any backend.

use std::sync::Arc;

use futures::future::BoxFuture;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
};
use serde_json::Value;

use crate::backend::{BackendConn, JsonObject, ProtoError};

/// Tools served in-process.
pub trait LocalTools: Send + Sync + 'static {
    /// Every tool, as MCP `Tool` JSON objects.
    fn list_tools(&self) -> BoxFuture<'_, Result<Vec<JsonObject>, String>>;
    /// Run one; the result is a `CallToolResult` JSON object.
    fn call_tool(
        &self,
        name: String,
        arguments: JsonObject,
    ) -> BoxFuture<'_, Result<JsonObject, String>>;
}

#[derive(Clone)]
struct LocalServer(Arc<dyn LocalTools>);

fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

impl ServerHandler for LocalServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    #[allow(clippy::field_reassign_with_default)] // ListToolsResult is non_exhaustive
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.0.list_tools().await.map_err(internal)?;
        let mut r = ListToolsResult::default();
        r.tools = tools
            .into_iter()
            .map(|t| serde_json::from_value::<Tool>(Value::Object(t)).map_err(internal))
            .collect::<Result<_, _>>()?;
        Ok(r)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let out = self
            .0
            .call_tool(
                request.name.to_string(),
                request.arguments.unwrap_or_default(),
            )
            .await
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let r: CallToolResult = serde_json::from_value(Value::Object(out)).map_err(internal)?;
        Ok(r.into())
    }
}

impl BackendConn {
    /// Serve `tools` over an in-memory MCP session and connect to it.
    pub async fn connect_local(
        tools: Arc<dyn LocalTools>,
        timeout: std::time::Duration,
    ) -> Result<Self, ProtoError> {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            if let Ok(running) = LocalServer(tools).serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        match Self::connect_io(client_io, timeout).await {
            Ok(mut conn) => {
                conn._tree.tasks.push(server);
                Ok(conn)
            }
            Err(e) => {
                server.abort();
                Err(e)
            }
        }
    }
}
