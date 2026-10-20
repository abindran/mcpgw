//! `mcpgw-wasm` — Wasm tool servers (M4-01 to M4-03).
//!
//! A Wasm tool server is a component exporting the `mcpgw:tool` world
//! (`wit/tool.wit`): `list-tools` and `call-tool`, with JSON arguments. It
//! runs in wasmtime with the component model and WASI p2 linked, and is
//! granted nothing by default: no network, no files, no environment
//! ([`Grants`], M4-02). Instances come from the **pooling allocator**, from a
//! pre-linked [`ToolServerPre`], so instantiation takes well under a
//! millisecond once a component is compiled. Compiled code is kept in
//! memory per file and, with [`set_cache_dir`], on disk by content hash, so
//! a restart doesn't recompile.
//!
//! Each call runs under a **time budget** (epoch interruption on a 10 ms
//! tick; the guest yields to the executor at every tick, so a call is
//! always cancellable, and it is stopped once it runs past its budget) and
//! each instance under a **memory cap** ([`Limits`], M4-03). A stopped
//! instance is replaced by a fresh one, so the next call works.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, ResourceLimiter, Store,
    UpdateDeadline,
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

/// Per-instance state: the WASI context and the limits' bookkeeping.
pub struct Host {
    wasi: WasiCtx,
    table: ResourceTable,
    limiter: Limiter,
    /// When the running call began, and its budget.
    call: Option<(Instant, Duration)>,
    /// Set when the epoch callback stops a call.
    out_of_time: bool,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Resource limits for one instance (M4-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Longest one call may run.
    pub time: Duration,
    /// Most linear memory the instance may use (at most [`MAX_MEMORY`]).
    pub memory: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            time: Duration::from_secs(30),
            memory: MAX_MEMORY,
        }
    }
}

/// Refuses memory past the cap, and remembers that it did.
struct Limiter {
    max: usize,
    denied: bool,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max {
            self.denied = true;
            return Ok(false);
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 1_000_000)
    }
}

/// Instances the pool holds at once (each running Wasm server is one).
const POOL_INSTANCES: u32 = 64;
/// Largest linear memory a tool may grow to.
pub const MAX_MEMORY: usize = 256 << 20;
/// Epoch tick: the granularity of time budgets.
const TICK: Duration = Duration::from_millis(10);

fn mib(bytes: usize) -> String {
    format!("{}MiB", bytes >> 20)
}

fn config(pooling: bool) -> Config {
    let mut c = Config::new();
    c.wasm_component_model(true);
    c.epoch_interruption(true);
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
/// address space, else on-demand allocation; with the epoch ticker that
/// time budgets are measured against.
pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let engine = Engine::new(&config(true)).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Wasm pooling allocator unavailable; allocating on demand");
            Engine::new(&config(false)).expect("wasmtime engine")
        });
        let ticker = engine.weak();
        std::thread::Builder::new()
            .name("mcpgw-wasm-epoch".into())
            .spawn(move || {
                while let Some(e) = ticker.upgrade() {
                    e.increment_epoch();
                    drop(e);
                    std::thread::sleep(TICK);
                }
            })
            .expect("epoch thread");
        engine
    })
}

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Keep compiled components in `dir` (by content hash).
pub fn set_cache_dir(dir: PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

/// A core module (what `cargo build --target wasm32-unknown-unknown` makes
/// from a wit-bindgen guest) as a component, using the WIT it embeds.
/// Components pass through.
pub fn componentize(bytes: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    // module: \0asm version 1; component: \0asm layer 1 (0x0d 0x00 0x01 0x00)
    let is_module = bytes.len() >= 8 && &bytes[..4] == b"\0asm" && bytes[4..8] == [1, 0, 0, 0];
    if !is_module {
        return Ok(std::borrow::Cow::Borrowed(bytes));
    }
    wit_component::ComponentEncoder::default()
        .module(bytes)
        .and_then(|e| e.validate(true).encode())
        .map(std::borrow::Cow::Owned)
        .map_err(|e| {
            format!("a core module, but not an mcpgw tool guest (no embedded WIT?): {e:#}")
        })
}

fn compile(bytes: &[u8]) -> Result<Component, String> {
    let bytes = &*componentize(bytes)?;
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

struct Inst {
    store: Store<Host>,
    bindings: ToolServer,
}

impl WasmTools {
    /// A fresh instance, granted nothing, with default limits.
    pub async fn instantiate(self: &Arc<Self>) -> Result<Running, String> {
        self.instantiate_limited(&Grants::default(), Limits::default())
            .await
    }

    /// A fresh instance granted `grants` (and nothing else), default limits.
    pub async fn instantiate_with(self: &Arc<Self>, grants: &Grants) -> Result<Running, String> {
        self.instantiate_limited(grants, Limits::default()).await
    }

    /// A fresh instance granted `grants`, under `limits`.
    pub async fn instantiate_limited(
        self: &Arc<Self>,
        grants: &Grants,
        limits: Limits,
    ) -> Result<Running, String> {
        let inst = self.new_instance(grants, limits).await?;
        Ok(Running {
            tools: self.clone(),
            grants: grants.clone(),
            limits,
            inst: tokio::sync::Mutex::new(Some(inst)),
        })
    }

    async fn new_instance(&self, grants: &Grants, limits: Limits) -> Result<Inst, String> {
        if limits.memory > MAX_MEMORY {
            return Err(format!(
                "a memory limit above {} isn't supported for Wasm servers",
                mib(MAX_MEMORY)
            ));
        }
        let wasi = grants.wasi()?;
        let mut store = Store::new(
            engine(),
            Host {
                wasi,
                table: ResourceTable::new(),
                limiter: Limiter {
                    max: limits.memory,
                    denied: false,
                },
                call: None,
                out_of_time: false,
            },
        );
        store.limiter(|h| &mut h.limiter);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|mut ctx| {
            let h = ctx.data_mut();
            match h.call {
                Some((started, budget)) if started.elapsed() > budget => {
                    h.out_of_time = true;
                    Ok(UpdateDeadline::Interrupt)
                }
                // stay cooperative: hand the thread back every tick
                _ => Ok(UpdateDeadline::Yield(1)),
            }
        });
        let bindings = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| format!("instantiating: {e:#}"))?;
        Ok(Inst { store, bindings })
    }
}

/// A running tool server: one instance at a time, calls taking turns. An
/// instance stopped by its limits is replaced on the next call.
pub struct Running {
    tools: Arc<WasmTools>,
    grants: Grants,
    limits: Limits,
    inst: tokio::sync::Mutex<Option<Inst>>,
}

fn text_result(text: String, is_error: bool) -> Map<String, Value> {
    let mut o = Map::new();
    o.insert("content".into(), json!([{"type": "text", "text": text}]));
    if is_error {
        o.insert("isError".into(), Value::Bool(true));
    }
    o
}

impl Running {
    /// The live instance, making a fresh one if the last was stopped.
    async fn live(&self) -> Result<tokio::sync::MutexGuard<'_, Option<Inst>>, String> {
        let mut g = self.inst.lock().await;
        if g.is_none() {
            *g = Some(self.tools.new_instance(&self.grants, self.limits).await?);
        }
        Ok(g)
    }

    /// Why an instance stopped, after a trap.
    fn stop_reason(&self, inst: &mut Inst, trap: &wasmtime::Error) -> String {
        let h = inst.store.data_mut();
        if std::mem::take(&mut h.out_of_time) {
            format!(
                "stopped: it ran past its {:?} time budget",
                self.limits.time
            )
        } else if std::mem::take(&mut h.limiter.denied) {
            format!(
                "stopped: out of memory at its {} limit",
                mib(self.limits.memory)
            )
        } else {
            format!("the tool trapped: {trap:#}")
        }
    }

    /// Its tools, as MCP `Tool` JSON objects.
    pub async fn list_tools(&self) -> Result<Vec<Map<String, Value>>, String> {
        let mut g = self.live().await?;
        let inst = g.as_mut().expect("live instance");
        inst.store.data_mut().call = Some((Instant::now(), self.limits.time));
        let res = inst.bindings.call_list_tools(&mut inst.store).await;
        inst.store.data_mut().call = None;
        let tools = match res {
            Ok(t) => t,
            Err(e) => {
                let why = self.stop_reason(inst, &e);
                *g = None;
                return Err(format!("list-tools: {why}"));
            }
        };
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

    /// Run a tool; the result is a `CallToolResult` JSON object. A call
    /// stopped by the limits is a tool error (`isError`) the model sees.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let args = Value::Object(arguments.clone()).to_string();
        let mut g = self.live().await?;
        let inst = g.as_mut().expect("live instance");
        inst.store.data_mut().call = Some((Instant::now(), self.limits.time));
        let res = inst
            .bindings
            .call_call_tool(&mut inst.store, name, &args)
            .await;
        inst.store.data_mut().call = None;
        match res {
            Ok(Ok(out)) => Ok(text_result(out.text, out.is_error)),
            Ok(Err(msg)) => Err(msg),
            Err(trap) => {
                let why = self.stop_reason(inst, &trap);
                tracing::warn!(tool = name, %why, "wasm call stopped; the next call gets a fresh instance");
                // a trapped instance can't be entered again
                *g = None;
                Ok(text_result(why, true))
            }
        }
    }
}
