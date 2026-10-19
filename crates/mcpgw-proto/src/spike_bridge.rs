//! M0-05 spike: `rmcp` as an MCP client, plus version bridging.
//! Throwaway; proves the gateway can speak `2025-11-25` (legacy `initialize`)
//! to a backend while presenting `2026-07-28` (`server/discover`) to clients.

use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, ClientHandler, ErrorData, Peer, RoleClient, RoleServer,
    ServerHandler, ServiceError,
};
use serde::Deserialize;

/// Backend fixture: pinned to protocol versions up to `2025-11-25`.
#[derive(Clone)]
pub struct LegacyBackend {
    tool_router: ToolRouter<Self>,
}

impl Default for LegacyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    pub a: i64,
    pub b: i64,
}

#[tool_router]
impl LegacyBackend {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Add two integers")]
    async fn add(
        &self,
        Parameters(args): Parameters<AddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            (args.a + args.b).to_string(),
        )]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LegacyBackend {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
    }
}

/// Client-side handler used by the gateway toward a backend.
#[derive(Clone, Default)]
pub struct BackendClient;
impl ClientHandler for BackendClient {}

fn map_err(e: ServiceError) -> ErrorData {
    match e {
        ServiceError::McpError(e) => e,
        other => ErrorData::internal_error(format!("backend error: {other}"), None),
    }
}

/// Spike gateway: server toward front clients, forwards to one backend peer.
#[derive(Clone)]
pub struct Bridge {
    backend: Peer<RoleClient>,
    // Keeps the backend session alive for the lifetime of the bridge.
    _keepalive: Arc<dyn std::any::Any + Send + Sync>,
}

impl Bridge {
    pub fn new<K: std::any::Any + Send + Sync>(backend: Peer<RoleClient>, keepalive: K) -> Self {
        Self {
            backend,
            _keepalive: Arc::new(keepalive),
        }
    }
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    // `ListToolsResult` is `#[non_exhaustive]`, so field assignment is required.
    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.backend.list_all_tools().await.map_err(map_err)?;
        let mut result = ListToolsResult::default();
        result.tools = tools;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Only the tool name and arguments are forwarded; per-request `_meta`
        // belongs to the front session and must not leak to the backend.
        let mut params = CallToolRequestParams::new(request.name.clone());
        params.arguments = request.arguments.clone();
        self.backend.call_tool_once(params).await.map_err(map_err)
    }
}
