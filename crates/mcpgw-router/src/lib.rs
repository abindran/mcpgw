//! `mcpgw-router` — aggregation, namespacing, meta-tools, policy checks and
//! the result pipeline.
//!
//! Exposes the aggregated backend tools to LLM clients. It never imports
//! `rmcp` directly; all protocol access goes through `mcpgw-proto`.

pub mod aggregate;
pub mod meta;

use std::sync::Arc;

pub use aggregate::{MAX_TOOL_NAME, call_public, list_all_tools, public_name, resolve, split_name};
use mcpgw_core::Mode;
use mcpgw_proto::front::{self, BoxFuture, FrontInfo, ToolProvider};
use mcpgw_runtime::{BackendManager, JsonObject, ProtoError};

/// Client-facing protocol version this side presents (via `mcpgw-proto`).
pub fn client_facing_version() -> &'static str {
    mcpgw_proto::version::CLIENT_FACING
}

/// How the gateway presents tools.
#[derive(Debug, Clone, Default)]
pub struct GatewayOptions {
    /// `direct`: every backend tool; `meta`: only the meta-tools;
    /// `hybrid`: meta-tools plus `pinned`.
    pub mode: Mode,
    /// Public tool names listed directly in hybrid mode.
    pub pinned: Vec<String>,
}

/// Routes front-side `tools/list` / `tools/call` to the backend manager.
pub struct GatewayProvider {
    manager: Arc<BackendManager>,
    opts: GatewayOptions,
}

impl GatewayProvider {
    /// Wrap a running backend manager, listing every tool directly.
    pub fn new(manager: Arc<BackendManager>) -> Self {
        Self::with_options(
            manager,
            GatewayOptions {
                mode: Mode::Direct,
                pinned: Vec::new(),
            },
        )
    }

    /// Wrap a running backend manager with presentation options.
    pub fn with_options(manager: Arc<BackendManager>, opts: GatewayOptions) -> Self {
        Self { manager, opts }
    }

    async fn listing(&self) -> Vec<JsonObject> {
        match self.opts.mode {
            Mode::Direct => list_all_tools(&self.manager).await,
            Mode::Meta => meta::meta_tools(),
            Mode::Hybrid => {
                let mut out = meta::meta_tools();
                out.extend(list_all_tools(&self.manager).await.into_iter().filter(|t| {
                    t["name"]
                        .as_str()
                        .is_some_and(|n| self.opts.pinned.iter().any(|p| p == n))
                }));
                out
            }
        }
    }
}

impl ToolProvider for GatewayProvider {
    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>> {
        Box::pin(async move {
            let r: Result<_, std::convert::Infallible> =
                mcpgw_telemetry::instrument_request("tools/list", None, async {
                    Ok(self.listing().await)
                })
                .await;
            r.unwrap_or_else(|never| match never {})
        })
    }

    fn call_tool(
        &self,
        name: String,
        arguments: Option<JsonObject>,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>> {
        Box::pin(async move {
            // In meta and hybrid modes the meta-tools exist; any tool can still
            // be called directly by its public name in every mode.
            let meta = self.opts.mode != Mode::Direct && meta::is_meta(&name);
            let label = match (meta, &arguments) {
                (true, Some(a)) if name == meta::CALL => a
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(meta::CALL)
                    .to_string(),
                _ => name.clone(),
            };
            mcpgw_telemetry::instrument_request("tools/call", Some(&label), async {
                if meta {
                    meta::call(&self.manager, &name, arguments).await
                } else {
                    call_public(&self.manager, &name, arguments).await
                }
            })
            .await
        })
    }
}

/// Default identity advertised to clients.
pub fn front_info() -> FrontInfo {
    FrontInfo {
        name: "mcpgw".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(
            "Tools come from several MCP servers and are named `<server>__<tool>`. If only \
             search_tools, describe_tool and call_tool are listed, search for the tool you need \
             first, then call it with call_tool."
                .into(),
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

/// The gateway's Streamable HTTP endpoint (mount under `/mcp`).
pub fn http_router(manager: Arc<BackendManager>, opts: GatewayOptions) -> axum::Router {
    front::http_service(
        Arc::new(GatewayProvider::with_options(manager, opts)),
        front_info(),
    )
}

/// Serve the gateway on stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    manager: Arc<BackendManager>,
    opts: GatewayOptions,
) -> Result<(), ProtoError> {
    front::serve_stdio(
        Arc::new(GatewayProvider::with_options(manager, opts)),
        front_info(),
    )
    .await
}
