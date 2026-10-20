//! `mcpgw-wasm` — Wasm tool servers (M4-01).
//!
//! A Wasm tool server is a component exporting the `mcpgw:tool` world
//! (`wit/tool.wit`): `list-tools` and `call-tool`, with JSON arguments. It
//! runs in wasmtime with the component model and WASI p2 linked, and is
//! granted nothing by default: no network, no files, no environment
//! ([`Grants`], M4-02). Instances come from the **pooling
//! allocator**, from a pre-linked [`ToolServerPre`], so instantiation takes
//! well under a millisecond once a component is compiled. Compiled code is
//! kept in memory per file and, with [`set_cache_dir`], on disk by content
//! hash, so a restart doesn't recompile.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{
    FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView, sockets::SocketAddrUse,
};

/// What an instance may use; everything else is denied (M4-02).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    /// TCP destinations it may connect to (already resolved). Nothing else:
    /// no listening, no UDP.
    pub connect: Vec<SocketAddr>,
    /// Connect anywhere (an explicit `*` grant).
    pub any_network: bool,
    /// May resolve host names (when names were granted).
    pub dns: bool,
    /// Host directories visible inside, at a guest path, read-only unless
    /// `write`.
    pub dirs: Vec<DirGrant>,
    /// Environment variables it sees.
    pub env: Vec<(String, String)>,
}

/// One directory grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirGrant {
    /// On the host.
    pub host: PathBuf,
    /// Where the guest finds it.
    pub guest: String,
    /// May create and change files.
    pub write: bool,
}

impl Grants {
    fn wasi(&self) -> Result<WasiCtx, String> {
        let mut b = WasiCtxBuilder::new();
        for (k, v) in &self.env {
            b.env(k, v);
        }
        for d in &self.dirs {
            let perms = if d.write {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            b.preopened_dir(&d.host, &d.guest, perms)
                .map_err(|e| format!("granting {}: {e}", d.host.display()))?;
        }
        let tcp = self.any_network || !self.connect.is_empty();
        b.allow_tcp(tcp)
            .allow_udp(false)
            .allow_ip_name_lookup(self.dns && tcp);
        let allowed = self.connect.clone();
        let any = self.any_network;
        b.socket_addr_check(move |addr, used| {
            let ok = match used {
                SocketAddrUse::TcpConnect => any || allowed.contains(&addr),
                // the implicit bind a connect makes, to an unspecified address
                SocketAddrUse::TcpBind => {
                    (any || !allowed.is_empty()) && addr.ip().is_unspecified() && addr.port() == 0
                }
                _ => false,
            };
            Box::pin(async move { ok })
        });
        Ok(b.build())
    }
}

wasmtime::component::bindgen!({
    path: "wit",
    world: "tool-server",
    exports: { default: async },
});

/// Per-instance state: the (empty by default) WASI context.
pub struct Host {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Instances the pool holds at once (each running Wasm server is one).
const POOL_INSTANCES: u32 = 64;
/// Largest linear memory a tool may grow to.
pub const MAX_MEMORY: usize = 256 << 20;

fn config(pooling: bool) -> Config {
    let mut c = Config::new();
    c.wasm_component_model(true);
    // small reservations keep the pool's address space modest
    c.memory_reservation(MAX_MEMORY as u64);
    c.memory_guard_size(64 << 10);
    if pooling {
        let mut pool = PoolingAllocationConfig::default();
        pool.total_component_instances(POOL_INSTANCES);
        pool.total_core_instances(POOL_INSTANCES * 4);
        pool.total_memories(POOL_INSTANCES * 2);
        pool.total_tables(POOL_INSTANCES * 4);
        pool.max_memory_size(MAX_MEMORY);
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
    }
    c
}

/// The process's engine: pooling allocator if the system grants its
/// address space, else on-demand allocation.
pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        Engine::new(&config(true)).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Wasm pooling allocator unavailable; allocating on demand");
            Engine::new(&config(false)).expect("wasmtime engine")
        })
    })
}

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Keep compiled components in `dir` (by content hash).
pub fn set_cache_dir(dir: PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

fn compile(bytes: &[u8]) -> Result<Component, String> {
    let engine = engine();
    let Some(dir) = CACHE_DIR.get() else {
        return Component::new(engine, bytes).map_err(|e| format!("not a valid component: {e:#}"));
    };
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let file = dir.join(format!(
        "{}-{}.cwasm",
        &hash[..32],
        env!("CARGO_PKG_VERSION")
    ));
    if file.is_file() {
        // SAFETY: the file was written by `Component::serialize` of this
        // engine version; a mismatch is rejected by wasmtime's header check.
        match unsafe { Component::deserialize_file(engine, &file) } {
            Ok(c) => return Ok(c),
            Err(e) => tracing::debug!(error = %e, file = %file.display(), "recompiling"),
        }
    }
    let c = Component::new(engine, bytes).map_err(|e| format!("not a valid component: {e:#}"))?;
    if let Ok(serialized) = c.serialize() {
        let _ = std::fs::create_dir_all(dir);
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, serialized).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
    Ok(c)
}

/// A compiled, linked tool component, ready to instantiate.
pub struct WasmTools {
    pre: ToolServerPre<Host>,
}

type Key = (PathBuf, Option<SystemTime>, u64);

/// Load (compile and link) the component at `path`; cached per file version.
pub fn load(path: &Path) -> Result<Arc<WasmTools>, String> {
    static LOADED: OnceLock<Mutex<HashMap<Key, Arc<WasmTools>>>> = OnceLock::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = (path.to_path_buf(), meta.modified().ok(), meta.len());
    let loaded = LOADED.get_or_init(Default::default);
    if let Some(t) = loaded.lock().expect("wasm cache").get(&key) {
        return Ok(t.clone());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let component = compile(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut linker = Linker::new(engine());
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| e.to_string())?;
    let pre = linker
        .instantiate_pre(&component)
        .map_err(|e| format!("{}: {e:#}", path.display()))?;
    let pre = ToolServerPre::new(pre).map_err(|e| {
        format!(
            "{} is not an mcpgw tool server (world mcpgw:tool/tool-server): {e:#}",
            path.display()
        )
    })?;
    let t = Arc::new(WasmTools { pre });
    loaded.lock().expect("wasm cache").insert(key, t.clone());
    Ok(t)
}

impl WasmTools {
    /// A fresh instance with its own store, granted nothing.
    pub async fn instantiate(&self) -> Result<Running, String> {
        self.instantiate_with(&Grants::default()).await
    }

    /// A fresh instance granted `grants` (and nothing else).
    pub async fn instantiate_with(&self, grants: &Grants) -> Result<Running, String> {
        let wasi = grants.wasi()?;
        let mut store = Store::new(
            engine(),
            Host {
                wasi,
                table: ResourceTable::new(),
            },
        );
        let bindings = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| format!("instantiating: {e:#}"))?;
        Ok(Running {
            store: tokio::sync::Mutex::new(store),
            bindings,
        })
    }
}

/// A running instance. Calls take turns (one store).
pub struct Running {
    store: tokio::sync::Mutex<Store<Host>>,
    bindings: ToolServer,
}

impl Running {
    /// Its tools, as MCP `Tool` JSON objects.
    pub async fn list_tools(&self) -> Result<Vec<Map<String, Value>>, String> {
        let mut store = self.store.lock().await;
        let tools = self
            .bindings
            .call_list_tools(&mut *store)
            .await
            .map_err(|e| format!("list-tools trapped: {e:#}"))?;
        Ok(tools
            .into_iter()
            .map(|t| {
                let schema: Value = serde_json::from_str(&t.input_schema)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({"type": "object"}));
                let mut o = Map::new();
                o.insert("name".into(), Value::String(t.name));
                o.insert("description".into(), Value::String(t.description));
                o.insert("inputSchema".into(), schema);
                o
            })
            .collect())
    }

    /// Run a tool; the result is a `CallToolResult` JSON object.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let args = Value::Object(arguments.clone()).to_string();
        let mut store = self.store.lock().await;
        match self.bindings.call_call_tool(&mut *store, name, &args).await {
            Ok(Ok(out)) => {
                let mut o = Map::new();
                o.insert(
                    "content".into(),
                    json!([{"type": "text", "text": out.text}]),
                );
                if out.is_error {
                    o.insert("isError".into(), Value::Bool(true));
                }
                Ok(o)
            }
            Ok(Err(msg)) => Err(msg),
            Err(trap) => Err(format!("the tool trapped: {trap:#}")),
        }
    }
}
