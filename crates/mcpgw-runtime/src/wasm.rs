//! Wasm tool servers in the runtime (M4-01): a `wasm` server is compiled
//! and linked once (cached by file), instantiated from the pool for each
//! start, and served to the manager over an in-memory MCP session
//! ([`BackendConn::connect_local`]), so it has the same lifecycle,
//! pinning, policy and audit as any other server.

use std::{path::PathBuf, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use mcpgw_proto::{BackendConn, JsonObject, LocalTools, ProtoError};

pub use mcpgw_wasm::set_cache_dir;

/// How to start a Wasm server.
#[derive(Debug, Clone)]
pub struct WasmTemplate {
    /// The component file.
    pub path: PathBuf,
}

struct Local(mcpgw_wasm::Running);

impl LocalTools for Local {
    fn list_tools(&self) -> BoxFuture<'_, Result<Vec<JsonObject>, String>> {
        Box::pin(self.0.list_tools())
    }

    fn call_tool(
        &self,
        name: String,
        arguments: JsonObject,
    ) -> BoxFuture<'_, Result<JsonObject, String>> {
        Box::pin(async move { self.0.call_tool(&name, &arguments).await })
    }
}

impl WasmTemplate {
    /// Load (compiling on first use, off the async threads), instantiate
    /// and connect.
    pub async fn connect(&self, timeout: Duration) -> Result<BackendConn, ProtoError> {
        let path = self.path.clone();
        let tools = tokio::task::spawn_blocking(move || mcpgw_wasm::load(&path))
            .await
            .map_err(|e| ProtoError::Connect(e.to_string()))?
            .map_err(ProtoError::Connect)?;
        let started = std::time::Instant::now();
        let running = tools.instantiate().await.map_err(ProtoError::Connect)?;
        tracing::debug!(path = %self.path.display(), us = started.elapsed().as_micros() as u64, "wasm instance ready");
        BackendConn::connect_local(Arc::new(Local(running)), timeout).await
    }
}
