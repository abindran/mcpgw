//! The client-facing MCP server.
//!
//! Other crates implement [`ToolProvider`] with plain JSON; this module adapts
//! it to an `rmcp` server that answers both `server/discover` (2026-07-28)
//! and legacy `initialize` (2025-11-25) clients, over stdio or Streamable
//! HTTP ([`http_service`]).

use std::{future::Future, pin::Pin, sync::Arc};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
};
use serde_json::Value;

use crate::backend::{INTERNAL_ERROR, JsonObject, ProtoError, tool_from_json};

/// Boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the front server exposes. Implemented outside the protocol crate.
pub trait ToolProvider: Send + Sync + 'static {
    /// Every tool, as MCP `Tool` JSON objects.
    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>>;
    /// Execute a tool; `Ok` is a `CallToolResult` JSON object.
    fn call_tool(
        &self,
        name: String,
        arguments: Option<JsonObject>,
        ctx: CallContext,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>>;
}

/// What the gateway knows about the client making a call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallContext {
    /// The client handles URL-mode elicitation (MCP 2025-11-25), so a
    /// `-32042` "URL elicitation required" error will show it a link.
    pub url_elicitation: bool,
    /// The client's name, if it said.
    pub client_name: Option<String>,
}

/// JSON-RPC error code: the request needs the user to visit a URL first.
pub const URL_ELICITATION_REQUIRED: i32 = -32042;

/// Identity + instructions advertised to clients.
#[derive(Debug, Clone)]
pub struct FrontInfo {
    /// Implementation name.
    pub name: String,
    /// Implementation version.
    pub version: String,
    /// Optional instructions for the model.
    pub instructions: Option<String>,
}

struct Front<P> {
    provider: Arc<P>,
    info: FrontInfo,
}

impl<P> Clone for Front<P> {
    fn clone(&self) -> Self {
        Self {
            provider: self.provider.clone(),
            info: self.info.clone(),
        }
    }
}

/// Convert a seam error into a JSON-RPC error for the front client.
pub fn to_error_data(e: ProtoError) -> ErrorData {
    match e {
        ProtoError::Rpc {
            code,
            message,
            data,
        } => ErrorData::new(ErrorCode(code), message, data),
        other => ErrorData::new(ErrorCode(INTERNAL_ERROR), other.to_string(), None),
    }
}

impl<P: ToolProvider> ServerHandler for Front<P> {
    fn get_info(&self) -> ServerConfig {
        let mut cfg = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                self.info.name.clone(),
                self.info.version.clone(),
            ));
        if let Some(i) = &self.info.instructions {
            cfg = cfg.with_instructions(i.clone());
        }
        cfg
    }

    // `ListToolsResult` is `#[non_exhaustive]`.
    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = Vec::new();
        for t in self.provider.list_tools().await {
            match tool_from_json(t) {
                Ok(t) => tools.push(t),
                Err(e) => tracing::warn!(error = %e, "skipping malformed tool"),
            }
        }
        let mut result = ListToolsResult::default();
        result.tools = tools;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caps = context.client_capabilities();
        let ctx = CallContext {
            url_elicitation: caps
                .as_ref()
                .and_then(|c| c.elicitation.as_ref())
                .is_some_and(|e| e.url.is_some()),
            client_name: context.client_info().map(|i| i.name),
        };
        let out = self
            .provider
            .call_tool(request.name.to_string(), request.arguments, ctx)
            .await
            .map_err(to_error_data)?;
        let result: CallToolResult = serde_json::from_value(Value::Object(out)).map_err(|e| {
            ErrorData::new(
                ErrorCode(INTERNAL_ERROR),
                format!("backend returned malformed result: {e}"),
                None,
            )
        })?;
        Ok(result.into())
    }
}

/// Serve `provider` over an arbitrary byte stream until the client hangs up.
pub async fn serve_io<P, T>(provider: Arc<P>, info: FrontInfo, io: T) -> Result<(), ProtoError>
where
    P: ToolProvider,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let svc = Front { provider, info }
        .serve(io)
        .await
        .map_err(|e| ProtoError::Connect(e.to_string()))?;
    svc.waiting()
        .await
        .map_err(|e| ProtoError::Other(e.to_string()))?;
    Ok(())
}

/// Serve `provider` on this process's stdin/stdout.
pub async fn serve_stdio<P: ToolProvider>(
    provider: Arc<P>,
    info: FrontInfo,
) -> Result<(), ProtoError> {
    serve_io(
        provider,
        info,
        tokio::io::join(tokio::io::stdin(), tokio::io::stdout()),
    )
    .await
}

/// The Streamable HTTP endpoint as an `axum` router (mount it with
/// `Router::nest_service`). 2026-07-28 requests are stateless; legacy clients
/// get sessions. Authentication, `Host` and `Origin` checks belong to the
/// caller's middleware; rmcp's own loopback-only `Host` check stays on as a
/// second line of defence.
pub fn http_service<P: ToolProvider>(provider: Arc<P>, info: FrontInfo) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let front = Front { provider, info };
    let service = StreamableHttpService::new(
        move || Ok(front.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().fallback_service(service)
}
