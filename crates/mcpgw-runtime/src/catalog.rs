//! The tool catalog (M1-07): every server's pinned [`ToolManifest`], so
//! `tools/list` is answered without starting anything.
//!
//! A manifest is captured when a server is installed (`introspect`) or, if
//! it has none yet, the first time it starts. The catalog keeps them in
//! memory and hands each new one to a persistence callback (the store).

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use mcpgw_core::{ServerSpec, ToolDef, ToolManifest};
use mcpgw_proto::{JsonObject, ProtoError};

/// Saves a manifest durably (wired to the store by the daemon).
pub type Persist = Arc<dyn Fn(&ToolManifest) + Send + Sync>;

/// In-memory manifests by server name.
#[derive(Default)]
pub struct Catalog {
    manifests: std::sync::RwLock<BTreeMap<String, ToolManifest>>,
    persist: Option<Persist>,
}

impl Catalog {
    /// A catalog seeded with stored manifests.
    pub fn new(initial: Vec<ToolManifest>, persist: Option<Persist>) -> Self {
        Self {
            manifests: std::sync::RwLock::new(
                initial.into_iter().map(|m| (m.server.clone(), m)).collect(),
            ),
            persist,
        }
    }

    /// A server's manifest.
    pub fn get(&self, server: &str) -> Option<ToolManifest> {
        self.manifests.read().expect("catalog").get(server).cloned()
    }

    /// Whether a server has a manifest.
    pub fn contains(&self, server: &str) -> bool {
        self.manifests.read().expect("catalog").contains_key(server)
    }

    /// A server's tools as MCP `Tool` JSON objects (empty if unknown).
    pub fn tools(&self, server: &str) -> Vec<JsonObject> {
        self.get(server)
            .map(|m| {
                m.tools
                    .iter()
                    .filter_map(|t| t.to_json().as_object().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Store a manifest (replacing any previous one) and persist it.
    pub fn record(&self, m: ToolManifest) {
        if let Some(p) = &self.persist {
            p(&m);
        }
        self.manifests
            .write()
            .expect("catalog")
            .insert(m.server.clone(), m);
    }

    /// Forget a server's manifest.
    pub fn remove(&self, server: &str) {
        self.manifests.write().expect("catalog").remove(server);
    }
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build a manifest from a live tool list; malformed tools are skipped.
pub fn manifest_from(
    server: &str,
    protocol_version: Option<String>,
    tools: &[JsonObject],
) -> ToolManifest {
    let defs = tools
        .iter()
        .filter_map(
            |t| match ToolDef::from_json(serde_json::Value::Object(t.clone())) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::warn!(server, error = %e, "skipping malformed tool");
                    None
                }
            },
        )
        .collect();
    ToolManifest::new(server, protocol_version, now_secs(), defs)
}

/// Start a server, read its tools, stop it: the install-time capture.
pub async fn introspect(spec: &ServerSpec, timeout: Duration) -> Result<ToolManifest, ProtoError> {
    introspect_with_secrets(spec, timeout, None).await
}

/// Like [`introspect`], injecting the server's bound secrets.
pub async fn introspect_with_secrets(
    spec: &ServerSpec,
    timeout: Duration,
    secrets: Option<Arc<dyn crate::SecretResolver>>,
) -> Result<ToolManifest, ProtoError> {
    // Account-scoped servers are introspected as the default account.
    let owned;
    let spec = if spec.account_scoped() {
        owned = spec.for_account(mcpgw_core::DEFAULT_ACCOUNT);
        &owned
    } else {
        spec
    };
    let launch =
        crate::manager::Launch::new(spec, secrets, &spec.name, mcpgw_core::DEFAULT_ACCOUNT);
    let (target, _) = tokio::task::spawn_blocking(move || launch.resolve())
        .await
        .map_err(|e| ProtoError::Other(e.to_string()))?
        .map_err(ProtoError::Connect)?;
    let timeout = spec.startup_timeout.map(|d| d.get()).unwrap_or(timeout);
    let conn = target.connect(timeout, None).await?;
    let tools = conn.list_tools().await;
    let pv = conn.protocol_version();
    conn.shutdown().await;
    Ok(manifest_from(&spec.name, pv, &tools?))
}
