//! Wasm tool servers in the runtime (M4-01): a `wasm` server is compiled
//! and linked once (cached by file), instantiated from the pool for each
//! start, and served to the manager over an in-memory MCP session
//! ([`BackendConn::connect_local`]), so it has the same lifecycle,
//! pinning, policy and audit as any other server.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use mcpgw_proto::{BackendConn, JsonObject, LocalTools, ProtoError};

pub use mcpgw_wasm::set_cache_dir;

/// How to start a Wasm server.
#[derive(Clone)]
pub struct WasmTemplate {
    /// The component file.
    pub path: PathBuf,
    /// `host:port` / `ip:port` destinations, or `*`.
    pub allow_network: Vec<String>,
    /// Directory grants.
    pub allow_dirs: Vec<mcpgw_core::DirAccess>,
    /// Environment (secrets filled in).
    pub env: BTreeMap<String, String>,
}

impl std::fmt::Debug for WasmTemplate {
    // env may hold secrets: names only
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmTemplate")
            .field("path", &self.path)
            .field("allow_network", &self.allow_network)
            .field("allow_dirs", &self.allow_dirs)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The grants for one start: names resolved to addresses now, so a grant
/// for `api.example.com:443` covers the addresses it has at start.
fn grants(t: &WasmTemplate) -> Result<mcpgw_wasm::Grants, String> {
    use std::net::ToSocketAddrs;
    let mut g = mcpgw_wasm::Grants {
        env: t.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        dirs: t
            .allow_dirs
            .iter()
            .map(|d| mcpgw_wasm::DirGrant {
                host: d.path.clone(),
                guest: d
                    .guest
                    .clone()
                    .unwrap_or_else(|| d.path.to_string_lossy().replace('\\', "/")),
                write: d.write,
            })
            .collect(),
        ..Default::default()
    };
    for entry in &t.allow_network {
        if entry == "*" {
            g.any_network = true;
            g.dns = true;
            continue;
        }
        if let Ok(a) = entry.parse::<std::net::SocketAddr>() {
            g.connect.push(a);
            continue;
        }
        g.dns = true;
        let addrs = entry
            .to_socket_addrs()
            .map_err(|e| format!("network grant {entry}: {e}"))?;
        g.connect.extend(addrs);
    }
    Ok(g)
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
        let template = self.clone();
        let grants = tokio::task::spawn_blocking(move || grants(&template))
            .await
            .map_err(|e| ProtoError::Connect(e.to_string()))?
            .map_err(ProtoError::Connect)?;
        let started = std::time::Instant::now();
        let running = tools
            .instantiate_with(&grants)
            .await
            .map_err(ProtoError::Connect)?;
        tracing::debug!(path = %self.path.display(), us = started.elapsed().as_micros() as u64, "wasm instance ready");
        BackendConn::connect_local(Arc::new(Local(running)), timeout).await
    }
}
