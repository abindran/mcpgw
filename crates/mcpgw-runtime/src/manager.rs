//! Backend supervision (M1-05): one actor task per server drives the
//! [`state`](crate::state) machine.
//!
//! Callers talk to an actor through a command channel. The actor owns the
//! connection; spawns, calls and health pings run in their own tasks and
//! report back as internal events, so a slow server never blocks status or
//! other calls. Concurrent calls to a cold server queue behind one spawn.
//!
//! M1-06: an actor stops its server after `idle_timeout` without activity,
//! and before starting one it makes room under `max_running` by stopping
//! the least recently used idle server. `stateful` servers are exempt from
//! both.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::container::{ContainerInfo, ContainerTemplate};
use mcpgw_core::{ManifestDiff, ServerSpec, ToolManifest, Transport};
use mcpgw_proto::{BackendConn, HttpSpec, JsonObject, ProtoError, StderrSink, StdioSpec};
use mcpgw_vault::Redactor;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::{mpsc, oneshot};

use crate::{
    catalog::{Catalog, manifest_from},
    state::{Action, Event, State, step},
};

/// Looks up secret values for injection (the daemon backs this with the
/// vault). `Ok(None)` means the secret doesn't exist.
pub trait SecretResolver: Send + Sync {
    /// The value of secret `name`.
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String>;
}

/// Lines of stderr kept per server.
pub const STDERR_LINES: usize = 200;

/// Tunables for the backend manager.
#[derive(Debug, Clone)]
pub struct ManagerOptions {
    /// Spawn + handshake deadline.
    pub connect_timeout: Duration,
    /// Per tool-call deadline.
    pub call_timeout: Duration,
    /// Interval between health pings.
    pub health_interval: Duration,
    /// Deadline for one health ping.
    pub ping_timeout: Duration,
    /// First restart delay.
    pub backoff_initial: Duration,
    /// Maximum restart delay.
    pub backoff_max: Duration,
    /// Consecutive failed starts before a server is marked `Failed`.
    pub max_failures: u32,
    /// Start every enabled server at once and restart it after a crash,
    /// instead of starting on first use (the default).
    pub eager: bool,
    /// Stop a server after this long without activity (per-server
    /// `idle_timeout` overrides it).
    pub idle_timeout: Duration,
    /// Most servers running at once.
    pub max_running: usize,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(120),
            health_interval: Duration::from_secs(30),
            ping_timeout: Duration::from_secs(10),
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            max_failures: 5,
            eager: false,
            idle_timeout: Duration::from_secs(600),
            max_running: 16,
        }
    }
}

impl ManagerOptions {
    /// Delay before restart attempt `attempt` (0-based), doubling up to the max.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
        self.backoff_initial
            .saturating_mul(factor)
            .min(self.backoff_max)
    }
}

/// Status snapshot for one backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    /// Server name.
    pub name: String,
    /// Lifecycle state.
    pub state: State,
    /// Successful starts after the first one.
    pub restarts: u32,
    /// Negotiated protocol version of the current or last connection.
    pub protocol_version: Option<String>,
    /// Most recent error, or why the server is disabled.
    pub last_error: Option<String>,
    /// Process id while connected (stdio servers).
    pub pid: Option<u32>,
    /// The running container (container servers).
    pub container: Option<ContainerInfo>,
    /// Last start, call start or call end.
    pub last_used: Option<Instant>,
    /// Never stopped for idleness or evicted.
    pub stateful: bool,
    /// Kept warm (`warm_pool.min = 1`): started with the gateway, never
    /// stopped for idleness or evicted, restarted after a crash.
    pub warm: bool,
    /// Calls that had to wait for the server to start.
    pub cold_starts: u64,
    /// How long the last start took (spawn or connect through tools/list).
    pub last_start_ms: Option<u64>,
    /// Background starts requested because a call looked likely (M3-07).
    pub prewarms: u64,
}

struct Shared {
    /// Status name: the server, or `server@account` for an account instance.
    name: String,
    /// Whose manifest this is checked against: the base server's name.
    catalog_key: String,
    /// Scrubs the secrets injected into the current connection.
    redactor: Arc<std::sync::RwLock<Arc<Redactor>>>,
    /// The live manifest that differs from the pin, while quarantined.
    pending: std::sync::Mutex<Option<ToolManifest>>,
    status: std::sync::RwLock<BackendStatus>,
    tools: std::sync::RwLock<Vec<JsonObject>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    catalog: Arc<Catalog>,
}

impl Handle {
    /// The error for calls to a disabled server, if it is one.
    fn disabled_error(&self, server: &str) -> Option<ProtoError> {
        let st = self.shared.snapshot();
        (st.state == State::Disabled).then(|| {
            ProtoError::Unavailable(format!(
                "server {server} is {}",
                st.last_error.unwrap_or_else(|| "disabled".into())
            ))
        })
    }
}

impl Shared {
    fn redactor(&self) -> Arc<Redactor> {
        self.redactor.read().expect("redactor").clone()
    }

    fn sink(&self) -> StderrSink {
        let buf = self.stderr.clone();
        let name = self.name.clone();
        let redactor = self.redactor.clone();
        Arc::new(move |line: &str| {
            let r = redactor.read().expect("redactor").clone();
            let line = r.redact(line);
            tracing::debug!(target: "mcpgw::backend", server = %name, "{line}");
            let mut b = buf.lock().expect("stderr buffer");
            if b.len() == STDERR_LINES {
                b.pop_front();
            }
            b.push_back(line.to_string());
        })
    }

    fn update(&self, f: impl FnOnce(&mut BackendStatus)) {
        f(&mut self.status.write().expect("status lock"));
    }

    fn snapshot(&self) -> BackendStatus {
        self.status.read().expect("status lock").clone()
    }
}

type Reply<T> = oneshot::Sender<Result<T, ProtoError>>;

enum Cmd {
    Call {
        tool: String,
        args: Option<JsonObject>,
        /// Request `_meta` for the backend (trace context).
        meta: Option<JsonObject>,
        reply: Reply<JsonObject>,
    },
    Ensure(Reply<()>),
    Stop(oneshot::Sender<()>),
    Reset,
    SetEnabled(bool, Reply<()>),
    Shutdown(oneshot::Sender<()>),
}

enum Waiter {
    Call {
        tool: String,
        args: Option<JsonObject>,
        /// Request `_meta` for the backend (trace context).
        meta: Option<JsonObject>,
        reply: Reply<JsonObject>,
    },
    Ensure(Reply<()>),
}

enum Internal {
    Started(Result<Box<Started>, String>),
    /// Why a lost container died (see `container::post_mortem`).
    PostMortem {
        lost: String,
        cause: String,
    },
    CallDone {
        lost: bool,
        failed: bool,
    },
    Ping(Result<(), String>),
    Recheck,
}

#[derive(Clone)]
struct Handle {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Cmd>,
    prefix: String,
    /// For `server@account` instances, the base server.
    instance_of: Option<String>,
}

/// What every actor knows about its peers, for `max_running` eviction.
struct Registry {
    peers: std::sync::RwLock<Vec<Handle>>,
    max_running: usize,
}

impl Registry {
    /// Before `me` starts: if the cap is reached, stop the least recently
    /// used idle, non-stateful peer. The cap is soft: when every running
    /// server is busy or stateful, `me` starts anyway.
    fn make_room(&self, me: &str) {
        let peers = self.peers.read().expect("registry").clone();
        let others: Vec<(BackendStatus, &Handle)> = peers
            .iter()
            .filter(|h| h.shared.name != me)
            .map(|h| (h.shared.snapshot(), h))
            .filter(|(s, _)| s.state.is_connected() || s.state == State::Starting)
            .collect();
        if others.len() < self.max_running {
            return;
        }
        let victim = others
            .iter()
            .filter(|(s, _)| {
                matches!(s.state, State::Ready | State::Idle) && !s.stateful && !s.warm
            })
            .min_by_key(|(s, _)| s.last_used);
        match victim {
            Some((s, h)) => {
                tracing::info!(server = %s.name, starting = me, max_running = self.max_running, "evicting least recently used idle server");
                let (reply, _) = oneshot::channel();
                let _ = h.tx.send(Cmd::Stop(reply));
            }
            None => tracing::warn!(
                starting = me,
                max_running = self.max_running,
                "max_running reached and no idle server to stop; starting anyway"
            ),
        }
    }
}

/// Owns all backend actors. Servers can be added, removed, enabled and
/// disabled while the gateway runs.
pub struct BackendManager {
    backends: std::sync::RwLock<BTreeMap<String, Handle>>,
    /// Specs as configured (with `{account}` unresolved).
    specs: std::sync::RwLock<BTreeMap<String, ServerSpec>>,
    opts: ManagerOptions,
    secrets: Option<Arc<dyn SecretResolver>>,
    catalog: Arc<Catalog>,
    registry: Arc<Registry>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl BackendManager {
    /// Start one actor per server. Returns immediately; with
    /// `opts.eager`, enabled servers start in the background (see
    /// [`BackendManager::wait_ready`]).
    pub fn start(servers: &[ServerSpec], opts: ManagerOptions) -> Self {
        Self::with_catalog(servers, opts, Arc::new(Catalog::default()))
    }

    /// Like [`BackendManager::start`], with tool manifests from `catalog`.
    pub fn with_catalog(
        servers: &[ServerSpec],
        opts: ManagerOptions,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self::with_secrets(servers, opts, catalog, None)
    }

    /// Like [`BackendManager::with_catalog`], injecting secrets from
    /// `secrets` into servers that bind them.
    pub fn with_secrets(
        servers: &[ServerSpec],
        opts: ManagerOptions,
        catalog: Arc<Catalog>,
        secrets: Option<Arc<dyn SecretResolver>>,
    ) -> Self {
        let registry = Arc::new(Registry {
            peers: std::sync::RwLock::new(Vec::new()),
            max_running: opts.max_running.max(1),
        });
        let m = Self {
            backends: std::sync::RwLock::new(BTreeMap::new()),
            specs: std::sync::RwLock::new(BTreeMap::new()),
            opts,
            secrets,
            catalog,
            registry,
            tasks: std::sync::Mutex::new(Vec::new()),
        };
        for cfg in servers {
            if let Err(e) = m.add_server(cfg) {
                tracing::warn!(server = %cfg.name, error = %e, "server not added");
            }
        }
        m
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Handle>> {
        self.backends.read().expect("backends lock")
    }

    fn handle(&self, server: &str) -> Result<Handle, ProtoError> {
        self.read()
            .get(server)
            .cloned()
            .ok_or_else(|| ProtoError::Unavailable(format!("unknown server {server}")))
    }

    fn sync_registry(&self) {
        *self.registry.peers.write().expect("registry") = self.read().values().cloned().collect();
    }

    /// Add a server (and start its actor). Fails if the name exists. Its
    /// tool prefix falls back to the name if the alias is taken.
    ///
    /// A server whose secrets use `{account}` serves the `default` account
    /// itself; other accounts get their own instance on first use
    /// ([`BackendManager::call_tool_as`]).
    pub fn add_server(&self, cfg: &ServerSpec) -> Result<(), ProtoError> {
        if self.read().contains_key(&cfg.name) {
            return Err(ProtoError::Unavailable(format!(
                "server {} already exists",
                cfg.name
            )));
        }
        self.specs
            .write()
            .expect("specs lock")
            .insert(cfg.name.clone(), cfg.clone());
        let launch = if cfg.account_scoped() {
            cfg.for_account(mcpgw_core::DEFAULT_ACCOUNT)
        } else {
            cfg.clone()
        };
        self.insert(&cfg.name, &launch, None)?;
        self.catalog.bump();
        Ok(())
    }

    /// The backend key serving `server` for `account`, creating the
    /// account's instance if needed.
    fn instance(&self, server: &str, account: Option<&str>) -> Result<String, ProtoError> {
        let account = account.unwrap_or(mcpgw_core::DEFAULT_ACCOUNT);
        let spec = self.specs.read().expect("specs lock").get(server).cloned();
        let Some(spec) = spec else {
            return Err(ProtoError::Unavailable(format!("unknown server {server}")));
        };
        if account == mcpgw_core::DEFAULT_ACCOUNT || !spec.account_scoped() {
            return Ok(server.to_string());
        }
        mcpgw_core::validate_server_name(account)
            .map_err(|_| ProtoError::Unavailable(format!("invalid account name {account:?}")))?;
        let key = format!("{server}@{account}");
        if self.read().contains_key(&key) {
            return Ok(key);
        }
        if let Some(e) = self.handle(server)?.disabled_error(server) {
            return Err(e);
        }
        let mut inst = spec.for_account(account);
        inst.name = key.clone();
        match self.insert(&key, &inst, Some(server)) {
            Ok(()) => Ok(key),
            // another caller created it first
            Err(_) if self.read().contains_key(&key) => Ok(key),
            Err(e) => Err(e),
        }
    }

    /// Call a tool as `account` (see [`BackendManager::add_server`]).
    pub async fn call_tool_as(
        &self,
        server: &str,
        account: Option<&str>,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let key = self.instance(server, account)?;
        self.call_tool(&key, tool, arguments).await
    }

    /// [`call_tool_as`](Self::call_tool_as) with request `_meta` for the
    /// backend.
    pub async fn call_tool_as_meta(
        &self,
        server: &str,
        account: Option<&str>,
        tool: &str,
        arguments: Option<JsonObject>,
        meta: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let key = self.instance(server, account)?;
        self.call_tool_meta(&key, tool, arguments, meta).await
    }

    /// Account instances of `server` (`server@account` keys).
    fn instances_of(&self, server: &str) -> Vec<String> {
        self.read()
            .iter()
            .filter(|(_, h)| h.instance_of.as_deref() == Some(server))
            .map(|(k, _)| k.clone())
            .collect()
    }

    async fn drop_backend(&self, key: &str) {
        let h = self.backends.write().expect("backends lock").remove(key);
        self.sync_registry();
        if let Some(h) = h {
            let (reply, rx) = oneshot::channel();
            if h.tx.send(Cmd::Shutdown(reply)).is_ok() {
                let _ = rx.await;
            }
        }
    }

    fn insert(
        &self,
        key: &str,
        cfg: &ServerSpec,
        instance_of: Option<&str>,
    ) -> Result<(), ProtoError> {
        let catalog_key = instance_of.unwrap_or(key).to_string();
        let (state, why) = match cfg.enabled {
            false => (State::Disabled, Some("disabled".to_string())),
            true => (State::Cold, None),
        };
        let mut backends = self.backends.write().expect("backends lock");
        if backends.contains_key(key) {
            return Err(ProtoError::Unavailable(format!(
                "server {key} already exists"
            )));
        }
        // Instances aren't listed, so their prefix just has to be unique.
        let mut prefix = match instance_of {
            Some(_) => key.to_string(),
            None => cfg.prefix().to_string(),
        };
        if instance_of.is_none() && backends.values().any(|h| h.prefix == prefix) {
            tracing::warn!(server = %cfg.name, %prefix, "tool prefix already used; using the server name");
            prefix = cfg.name.clone();
        }
        let shared = Arc::new(Shared {
            name: key.to_string(),
            catalog_key,
            redactor: Arc::new(std::sync::RwLock::new(Arc::new(Redactor::default()))),
            pending: std::sync::Mutex::new(None),
            status: std::sync::RwLock::new(BackendStatus {
                name: key.to_string(),
                state: state.clone(),
                restarts: 0,
                protocol_version: None,
                last_error: why,
                pid: None,
                container: None,
                last_used: None,
                stateful: cfg.stateful,
                warm: cfg.warm(),
                cold_starts: 0,
                last_start_ms: None,
                prewarms: 0,
            }),
            tools: std::sync::RwLock::new(Vec::new()),
            stderr: Arc::default(),
            catalog: self.catalog.clone(),
        });
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let limits = Limits {
                connect_timeout: cfg
                    .startup_timeout
                    .map(|d| d.get())
                    .unwrap_or(self.opts.connect_timeout),
                idle_timeout: cfg
                    .idle_timeout
                    .map(|d| d.get())
                    .unwrap_or(self.opts.idle_timeout),
                stateful: cfg.stateful,
                warm: cfg.warm(),
            };
            let actor = Actor::new(
                shared.clone(),
                Launch::new(
                    cfg,
                    self.secrets.clone(),
                    instance_of.unwrap_or(key),
                    key.split_once('@')
                        .map_or(mcpgw_core::DEFAULT_ACCOUNT, |(_, a)| a),
                ),
                self.opts.clone(),
                limits,
                state,
                rx,
                self.registry.clone(),
            );
            self.tasks
                .lock()
                .expect("tasks mutex")
                .push(tokio::spawn(actor.run()));
            if (self.opts.eager || cfg.warm()) && cfg.enabled {
                let (reply, _) = oneshot::channel();
                let _ = tx.send(Cmd::Ensure(reply));
            }
        }
        backends.insert(
            key.to_string(),
            Handle {
                shared,
                tx,
                prefix,
                instance_of: instance_of.map(str::to_string),
            },
        );
        drop(backends);
        self.sync_registry();
        Ok(())
    }

    /// Remove a server: stop it and forget it. Its manifest stays in the
    /// catalog unless removed there too.
    pub async fn remove_server(&self, server: &str) -> Result<(), ProtoError> {
        if !self
            .read()
            .get(server)
            .is_some_and(|h| h.instance_of.is_none())
        {
            return Err(ProtoError::Unavailable(format!("unknown server {server}")));
        }
        self.specs.write().expect("specs lock").remove(server);
        for key in self.instances_of(server) {
            self.drop_backend(&key).await;
        }
        self.drop_backend(server).await;
        self.catalog.bump();
        Ok(())
    }

    /// Enable or disable a server. Disabling drains and stops it; its tools
    /// disappear from listings until it is enabled again.
    pub async fn set_enabled(&self, server: &str, enabled: bool) -> Result<(), ProtoError> {
        let h = self.handle(server)?;
        if !enabled {
            // account instances go; they come back on demand once re-enabled
            for key in self.instances_of(server) {
                self.drop_backend(&key).await;
            }
        }
        let (reply, rx) = oneshot::channel();
        h.tx.send(Cmd::SetEnabled(enabled, reply)).map_err(|_| {
            ProtoError::Unavailable(format!(
                "server {server} can't be enabled: {}",
                h.shared.snapshot().last_error.unwrap_or_default()
            ))
        })?;
        let r = rx
            .await
            .map_err(|_| ProtoError::Unavailable(format!("server {server} is shut down")))?;
        if r.is_ok()
            && let Some(spec) = self.specs.write().expect("specs lock").get_mut(server)
        {
            spec.enabled = enabled;
        }
        self.catalog.bump();
        r
    }

    /// A server's spec as configured.
    pub fn spec(&self, server: &str) -> Option<ServerSpec> {
        self.specs.read().expect("specs lock").get(server).cloned()
    }

    /// The tool catalog.
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// Prefix of a server's public tool names (its alias or name).
    pub fn prefix(&self, server: &str) -> Option<String> {
        self.read().get(server).map(|h| h.prefix.clone())
    }

    /// The server whose tools use `prefix`.
    pub fn server_for_prefix(&self, prefix: &str) -> Option<String> {
        self.read()
            .iter()
            .find(|(_, h)| h.prefix == prefix)
            .map(|(n, _)| n.clone())
    }

    /// Names of all servers, sorted (account instances excluded).
    pub fn names(&self) -> Vec<String> {
        self.read()
            .iter()
            .filter(|(_, h)| h.instance_of.is_none())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Status of every backend, ordered by name.
    pub async fn status(&self) -> Vec<BackendStatus> {
        self.read().values().map(|h| h.shared.snapshot()).collect()
    }

    /// Status of one backend.
    pub fn status_of(&self, server: &str) -> Option<BackendStatus> {
        self.read().get(server).map(|h| h.shared.snapshot())
    }

    /// Wait until no backend is starting (and, when eager, none is still
    /// cold; warm ones never count as settled while cold), or `timeout`
    /// elapses. Returns whether everything settled.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let busy = self.read().values().any(|h| {
                let snap = h.shared.snapshot();
                let s = snap.state;
                s == State::Starting || ((self.opts.eager || snap.warm) && s == State::Cold)
            });
            if !busy {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A server's tools from its pinned manifest, without starting it.
    /// Disabled and unknown servers have none.
    pub async fn tools(&self, server: &str) -> Vec<JsonObject> {
        match self.handle(server) {
            Ok(h) if h.shared.snapshot().state != State::Disabled => self.catalog.tools(server),
            _ => Vec::new(),
        }
    }

    /// The tools the running server reported on its last start.
    pub async fn live_tools(&self, server: &str) -> Vec<JsonObject> {
        match self.handle(server) {
            Ok(h) => h.shared.tools.read().expect("tools lock").clone(),
            Err(_) => Vec::new(),
        }
    }

    /// The last lines a server wrote to stderr (across restarts).
    pub fn stderr_tail(&self, server: &str) -> Vec<String> {
        match self.handle(server) {
            Ok(h) => h
                .shared
                .stderr
                .lock()
                .expect("stderr buffer")
                .iter()
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Call a tool, starting the server first if it is cold.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        self.call_tool_meta(server, tool, arguments, None).await
    }

    /// [`call_tool`](Self::call_tool), passing request `_meta` (such as a
    /// W3C `traceparent`, M3-11) on to the backend.
    pub async fn call_tool_meta(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<JsonObject>,
        meta: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let h = self.handle(server)?;
        let (reply, rx) = oneshot::channel();
        h.tx.send(Cmd::Call {
            tool: tool.to_string(),
            args: arguments,
            meta,
            reply,
        })
        .map_err(|_| self.gone(server, &h))?;
        let limit = self.opts.connect_timeout + self.opts.call_timeout;
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ProtoError::Unavailable(format!(
                "server {server} is shut down"
            ))),
            Err(_) => Err(ProtoError::Timeout(limit)),
        }
    }

    /// The error for a server whose actor isn't running (unsupported
    /// transport, or shut down).
    fn gone(&self, server: &str, h: &Handle) -> ProtoError {
        match h.shared.snapshot().last_error {
            Some(why) => ProtoError::Unavailable(format!("server {server} is {why}")),
            None => ProtoError::Unavailable(format!("server {server} is shut down")),
        }
    }

    /// Start `server` (as `account`) in the background if it is cold, as a
    /// hint that a call is likely soon (M3-07: `search_tools` just returned
    /// its tools). Speculative, so it never pushes another server out: it
    /// only starts when there is room under `max_running`. Returns whether
    /// a start was requested.
    pub fn prewarm(&self, server: &str, account: Option<&str>) -> bool {
        let Ok(key) = self.instance(server, account) else {
            return false;
        };
        let Ok(h) = self.handle(&key) else {
            return false;
        };
        if h.shared.snapshot().state != State::Cold {
            return false;
        }
        let running = self
            .read()
            .values()
            .filter(|o| {
                let s = o.shared.snapshot().state;
                s.is_connected() || s == State::Starting
            })
            .count();
        if running >= self.opts.max_running {
            return false;
        }
        let (reply, _) = oneshot::channel();
        if h.tx.send(Cmd::Ensure(reply)).is_err() {
            return false;
        }
        h.shared.update(|s| s.prewarms += 1);
        tracing::debug!(server = %key, "prewarming");
        true
    }

    /// Start a server (if needed) and wait until it is connected.
    pub async fn ensure(&self, server: &str) -> Result<(), ProtoError> {
        let h = self.handle(server)?;
        let (reply, rx) = oneshot::channel();
        h.tx.send(Cmd::Ensure(reply))
            .map_err(|_| self.gone(server, &h))?;
        let limit = self.opts.connect_timeout + Duration::from_secs(1);
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ProtoError::Unavailable(format!(
                "server {server} is shut down"
            ))),
            Err(_) => Err(ProtoError::Timeout(limit)),
        }
    }

    /// Stop a server (after its in-flight calls finish). It starts again on
    /// the next call.
    pub async fn stop(&self, server: &str) -> Result<(), ProtoError> {
        let h = self.handle(server)?;
        let (reply, rx) = oneshot::channel();
        let _ = h.tx.send(Cmd::Stop(reply));
        let _ = rx.await;
        Ok(())
    }

    /// The tools a quarantined server reports now, which differ from its pin.
    pub fn pending_manifest(&self, server: &str) -> Option<ToolManifest> {
        self.handle(server)
            .ok()?
            .shared
            .pending
            .lock()
            .expect("pending")
            .clone()
    }

    /// Trust a quarantined server's new tools: they become its pin and the
    /// server can start again. Returns the new manifest.
    pub fn approve_tools(&self, server: &str) -> Result<ToolManifest, ProtoError> {
        let h = self.handle(server)?;
        let m = h
            .shared
            .pending
            .lock()
            .expect("pending")
            .take()
            .ok_or_else(|| {
                ProtoError::Unavailable(format!("server {server} is not quarantined"))
            })?;
        self.catalog.record(m.clone());
        let _ = h.tx.send(Cmd::Reset);
        Ok(m)
    }

    /// Clear a `Failed` server so the next call tries again.
    pub fn reset(&self, server: &str) -> Result<(), ProtoError> {
        let _ = self.handle(server)?.tx.send(Cmd::Reset);
        Ok(())
    }

    /// Stop every actor and terminate every backend process.
    pub async fn shutdown(&self) {
        let handles: Vec<Handle> = self.read().values().cloned().collect();
        let mut waits = Vec::new();
        for h in &handles {
            let (reply, rx) = oneshot::channel();
            if h.tx.send(Cmd::Shutdown(reply)).is_ok() {
                waits.push(rx);
            }
        }
        for w in waits {
            let _ = w.await;
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().expect("tasks mutex"));
        for t in tasks {
            let _ = t.await;
        }
        for h in &handles {
            if h.shared.snapshot().state != State::Disabled {
                h.shared.update(|s| s.state = State::Cold);
            }
        }
    }
}

/// How to launch a server: the spec with secret placeholders, its env
/// bindings, and where secrets come from.
pub(crate) struct Launch {
    pub(crate) template: Template,
    /// For HTTP servers without an `Authorization` header: the vault name of
    /// its OAuth login (`oauth/<server>@<account>`), injected when present.
    pub(crate) oauth: Option<String>,
    pub(crate) bindings: BTreeMap<String, String>,
    pub(crate) refs: std::collections::BTreeSet<String>,
    pub(crate) resolver: Option<Arc<dyn SecretResolver>>,
}

/// A started backend: connection, tools, redactor, container.
type Started = (
    BackendConn,
    Vec<JsonObject>,
    Redactor,
    Option<ContainerInfo>,
);

/// A backend to connect to, before or after secrets are filled in.
#[derive(Debug, Clone)]
pub(crate) enum Template {
    Stdio(StdioSpec),
    Http(HttpSpec),
    Container(ContainerTemplate),
    Wasm(crate::wasm::WasmTemplate),
    OpenApi(crate::openapi::OpenApiTemplate),
}

impl Template {
    /// Connect: spawn the process or container, or open the HTTP session.
    pub(crate) async fn connect(
        &self,
        timeout: Duration,
        sink: Option<StderrSink>,
    ) -> Result<(BackendConn, Option<ContainerInfo>), ProtoError> {
        match self {
            Template::Stdio(s) => Ok((BackendConn::connect_stdio(s, timeout, sink).await?, None)),
            Template::Http(h) => Ok((BackendConn::connect_http(h, timeout).await?, None)),
            Template::Container(c) => {
                let (conn, info) = c.connect(timeout, sink).await?;
                Ok((conn, Some(info)))
            }
            Template::Wasm(w) => Ok((w.connect(timeout).await?, None)),
            Template::OpenApi(o) => Ok((o.connect(timeout).await?, None)),
        }
    }
}

impl Launch {
    /// What runs the server, for metrics: the command's name (`node`,
    /// `npx`, `uvx`, `python3` …), the container runtime, or `http`.
    pub(crate) fn runtime_kind(&self) -> String {
        match &self.template {
            Template::Stdio(s) => {
                let p = std::path::Path::new(&s.command);
                // a cached PyPI package runs a venv script: report python
                let venv = p
                    .parent()
                    .and_then(|d| d.file_name())
                    .is_some_and(|d| d == "bin" || d == "Scripts")
                    && p.ancestors()
                        .any(|a| a.file_name().is_some_and(|n| n == "venv"));
                if venv {
                    "python".into()
                } else {
                    p.file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("stdio")
                        .to_string()
                }
            }
            Template::Container(c) => crate::container::pick_runtime(c.runtime.as_deref())
                .unwrap_or_else(|_| "container".into()),
            Template::Http(_) => "http".into(),
            Template::Wasm(_) => "wasm".into(),
            Template::OpenApi(_) => "openapi".into(),
        }
    }

    /// How to launch `cfg`, with secrets from `resolver`.
    pub(crate) fn new(
        cfg: &ServerSpec,
        resolver: Option<Arc<dyn SecretResolver>>,
        server: &str,
        account: &str,
    ) -> Self {
        let template = match &cfg.transport {
            Transport::Stdio {
                command,
                args,
                env,
                cwd,
            } => Template::Stdio({
                // an npx/uvx package in the cache starts directly (M3-08)
                let (command, args) = crate::packages::cached_launch(command, args)
                    .unwrap_or_else(|| (command.clone(), args.clone()));
                StdioSpec {
                    command,
                    args,
                    env: env.clone(),
                    cwd: cwd.clone(),
                    cleanup: Vec::new(),
                    teardown: Vec::new(),
                    memory_limit: cfg.limits.memory.map(|m| m.get()),
                }
            }),
            Transport::Container {
                image,
                digest,
                runtime,
                args,
                env,
                run_args,
                sandbox,
                egress,
            } => Template::Container(ContainerTemplate {
                server: server.to_string(),
                runtime: runtime.clone(),
                image: mcpgw_core::pinned_image_ref(image, digest.as_deref()),
                args: args.clone(),
                env: env.clone(),
                run_args: run_args.clone(),
                sandbox: *sandbox,
                limits: cfg.limits.clone(),
                egress: egress.clone(),
                files: cfg.files.clone(),
            }),
            Transport::Wasm {
                path,
                allow_network,
                allow_dirs,
                env,
                time_budget,
            } => Template::Wasm(crate::wasm::WasmTemplate {
                path: path.clone(),
                allow_network: allow_network.clone(),
                allow_dirs: allow_dirs.clone(),
                env: env.clone(),
                limits: {
                    let d = mcpgw_wasm::Limits::default();
                    mcpgw_wasm::Limits {
                        time: time_budget.map(|t| t.get()).unwrap_or(d.time),
                        memory: cfg
                            .limits
                            .memory
                            .map(|m| m.get() as usize)
                            .unwrap_or(d.memory),
                    }
                },
            }),
            Transport::OpenApi {
                spec,
                base_url,
                headers,
                auth,
            } => Template::OpenApi(crate::openapi::OpenApiTemplate {
                spec: spec.clone(),
                base_url: base_url.clone(),
                headers: headers.clone(),
                auth: auth
                    .iter()
                    .map(|(k, b)| (k.clone(), (b.username.clone(), b.secret.clone())))
                    .collect(),
            }),
            Transport::Http { url, headers } => Template::Http(HttpSpec {
                url: url.clone(),
                headers: headers.clone(),
            }),
        };
        let oauth = match &template {
            Template::Http(h)
                if !h
                    .headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("authorization"))
                    && !cfg
                        .secrets
                        .keys()
                        .any(|k| k.eq_ignore_ascii_case("authorization")) =>
            {
                Some(mcpgw_core::oauth_secret_name(server, account))
            }
            _ => None,
        };
        Self {
            template,
            oauth,
            bindings: cfg.secrets.clone(),
            refs: cfg.secret_refs(),
            resolver,
        }
    }

    /// The concrete backend plus a redactor for the injected values.
    /// For HTTP servers, `secrets` bindings become headers.
    pub(crate) fn resolve(&self) -> Result<(Template, Redactor), String> {
        // An OAuth login for a remote server is optional: without one the
        // server answers 401 and the start fails with a hint.
        let login = match (&self.oauth, &self.resolver) {
            (Some(name), Some(r)) => r.resolve(name)?.map(|t| (name.clone(), t)),
            _ => None,
        };
        if self.refs.is_empty() && login.is_none() {
            return Ok((self.template.clone(), Redactor::default()));
        }
        let Some(r) = &self.resolver else {
            return Err("this server needs secrets, but no vault is configured".into());
        };
        let mut values: BTreeMap<String, SecretString> = BTreeMap::new();
        for name in &self.refs {
            match r.resolve(name)? {
                Some(v) => {
                    values.insert(name.clone(), v);
                }
                None if name.starts_with("oauth/") => {
                    let (p, a) = name["oauth/".len()..]
                        .split_once('@')
                        .unwrap_or((name, "default"));
                    return Err(format!(
                        "{name} is not connected; run `mcpgw auth connect {p} --account {a}`"
                    ));
                }
                None => {
                    return Err(format!(
                        "secret {name:?} is not set; add it with `mcpgw secret set {name}`"
                    ));
                }
            }
        }
        let lookup = |n: &str| values.get(n).map(|v| v.expose_secret().to_string());
        let fill = |s: &str| mcpgw_core::fill_placeholders(s, &lookup);
        let mut spec = self.template.clone();
        match &mut spec {
            Template::Stdio(StdioSpec { args, env, .. })
            | Template::Container(ContainerTemplate { args, env, .. }) => {
                *args = args.iter().map(|a| fill(a)).collect();
                for v in env.values_mut() {
                    *v = fill(v);
                }
                for (var, name) in &self.bindings {
                    env.insert(var.clone(), lookup(name).unwrap_or_default());
                }
            }
            Template::Wasm(w) => {
                for v in w.env.values_mut() {
                    *v = fill(v);
                }
                for (var, name) in &self.bindings {
                    w.env.insert(var.clone(), lookup(name).unwrap_or_default());
                }
            }
            Template::OpenApi(o) => {
                for v in o.headers.values_mut() {
                    *v = fill(v);
                }
                for (header, name) in &self.bindings {
                    o.headers
                        .insert(header.clone(), lookup(name).unwrap_or_default());
                }
            }
            Template::Http(_) => {}
        }
        if let Template::Container(c) = &mut spec {
            for v in c.files.values_mut() {
                *v = lookup(v).unwrap_or_default();
            }
        }
        if let Template::OpenApi(o) = &mut spec {
            for (_, secret) in o.auth.values_mut() {
                *secret = lookup(secret).unwrap_or_default();
            }
        }
        match &mut spec {
            Template::Stdio(_)
            | Template::Container(_)
            | Template::Wasm(_)
            | Template::OpenApi(_) => {}
            Template::Http(h) => {
                h.url = fill(&h.url);
                for v in h.headers.values_mut() {
                    *v = fill(v);
                }
                for (header, name) in &self.bindings {
                    h.headers
                        .insert(header.clone(), lookup(name).unwrap_or_default());
                }
                if let Some((_, token)) = &login {
                    h.headers.insert(
                        "Authorization".into(),
                        format!("Bearer {}", token.expose_secret()),
                    );
                }
            }
        }
        if let Some((name, token)) = login {
            values.insert(name, token);
        }
        let redactor = Redactor::new(values.iter().map(|(n, v)| (n.as_str(), v.expose_secret())));
        Ok((spec, redactor))
    }
}

/// Redact secret values from an error crossing back to the client.
fn redact_error(e: ProtoError, r: &Redactor) -> ProtoError {
    if r.is_empty() {
        return e;
    }
    let s = |m: String| r.redact(&m).into_owned();
    match e {
        ProtoError::Rpc {
            code,
            message,
            mut data,
        } => {
            if let Some(d) = data.as_mut() {
                r.redact_json(d);
            }
            ProtoError::Rpc {
                code,
                message: s(message),
                data,
            }
        }
        ProtoError::Connect(m) => ProtoError::Connect(s(m)),
        ProtoError::Unavailable(m) => ProtoError::Unavailable(s(m)),
        ProtoError::Other(m) => ProtoError::Other(s(m)),
        other => other,
    }
}

/// Per-server limits resolved from the spec and the defaults.
struct Limits {
    connect_timeout: Duration,
    idle_timeout: Duration,
    stateful: bool,
    warm: bool,
}

struct Actor {
    shared: Arc<Shared>,
    launch: Arc<Launch>,
    opts: ManagerOptions,
    limits: Limits,
    registry: Arc<Registry>,
    idle_at: Option<tokio::time::Instant>,
    sink: StderrSink,
    rx: mpsc::UnboundedReceiver<Cmd>,
    itx: mpsc::UnboundedSender<Internal>,
    irx: mpsc::UnboundedReceiver<Internal>,
    state: State,
    failures: u32,
    ever_connected: bool,
    conn: Option<Arc<BackendConn>>,
    /// The container behind `conn`, if any.
    container: Option<ContainerInfo>,
    /// Background closes still running (see `closing`).
    closes: Vec<tokio::task::JoinHandle<()>>,
    /// When the current start began.
    start_began: Option<Instant>,
    waiters: Vec<Waiter>,
    retry_at: Option<tokio::time::Instant>,
    ping_inflight: bool,
    stop_replies: Vec<oneshot::Sender<()>>,
    disable_replies: Vec<Reply<()>>,
    exiting: Option<oneshot::Sender<()>>,
}

impl Actor {
    fn new(
        shared: Arc<Shared>,
        launch: Launch,
        opts: ManagerOptions,
        limits: Limits,
        state: State,
        rx: mpsc::UnboundedReceiver<Cmd>,
        registry: Arc<Registry>,
    ) -> Self {
        let (itx, irx) = mpsc::unbounded_channel();
        let sink = shared.sink();
        Self {
            shared,
            launch: Arc::new(launch),
            opts,
            limits,
            registry,
            idle_at: None,
            sink,
            rx,
            itx,
            irx,
            state,
            failures: 0,
            ever_connected: false,
            conn: None,
            container: None,
            closes: Vec::new(),
            start_began: None,
            waiters: Vec::new(),
            retry_at: None,
            ping_inflight: false,
            stop_replies: Vec::new(),
            disable_replies: Vec::new(),
            exiting: None,
        }
    }

    async fn run(mut self) {
        let mut health = tokio::time::interval(self.opts.health_interval);
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        health.tick().await; // first tick is immediate
        loop {
            let retry = async {
                match self.retry_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let idle = async {
                match self.idle_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    Some(c) => self.on_cmd(c),
                    None => self.begin_exit(None),
                },
                Some(ev) = self.irx.recv() => self.on_internal(ev),
                _ = health.tick() => self.check_health(),
                _ = retry => self.on_retry(),
                _ = idle => {
                    self.idle_at = None;
                    if matches!(self.state, State::Ready | State::Idle) {
                        tracing::info!(server = %self.shared.name, idle = ?self.limits.idle_timeout, "stopping idle server");
                        self.apply(Event::Stop);
                    }
                }
            }
            self.rearm_idle();
            // A disable completes once the server is stopped.
            if !self.disable_replies.is_empty()
                && matches!(
                    self.state,
                    State::Cold | State::Backoff { .. } | State::Failed | State::Quarantined
                )
            {
                self.shared
                    .update(|s| s.last_error = Some("disabled".into()));
                self.apply(Event::Disable);
                for r in self.disable_replies.drain(..) {
                    let _ = r.send(Ok(()));
                }
            }
            // Calls that queued while the server drained start it again.
            if self.state == State::Cold && !self.waiters.is_empty() && self.exiting.is_none() {
                self.apply(Event::Demand);
            }
            if self.exiting.is_some() && !self.state.is_connected() && self.state != State::Starting
            {
                break;
            }
        }
        if let Some(c) = self.conn.take() {
            close(c).await;
        }
        // Earlier closes (a container's removal) finish before we report out.
        for t in self.closes.drain(..) {
            let _ = t.await;
        }
        self.publish();
        if let Some(r) = self.exiting.take() {
            let _ = r.send(());
        }
    }

    /// Keep the idle timer armed exactly while the server is connected and
    /// unused (and not stateful).
    fn rearm_idle(&mut self) {
        let idle = matches!(self.state, State::Ready | State::Idle)
            && !self.limits.stateful
            && !self.limits.warm;
        if !idle {
            self.idle_at = None;
        } else if self.idle_at.is_none() {
            self.idle_at = Some(tokio::time::Instant::now() + self.limits.idle_timeout);
        }
    }

    fn touch(&self) {
        self.shared.update(|s| s.last_used = Some(Instant::now()));
    }

    /// Apply an event; returns whether it was valid.
    fn apply(&mut self, event: Event) -> bool {
        match step(&self.state, event, self.failures, self.opts.max_failures) {
            Ok((next, action)) => {
                tracing::debug!(server = %self.shared.name, from = self.state.name(), to = next.name(), ?event, "transition");
                self.state = next;
                self.act(action);
                self.publish();
                if !self.state.is_connected() && self.state != State::Starting {
                    for r in self.stop_replies.drain(..) {
                        let _ = r.send(());
                    }
                }
                true
            }
            Err(invalid) => {
                tracing::debug!(server = %self.shared.name, ?invalid, "ignored event");
                false
            }
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Spawn => {
                self.start_began = Some(Instant::now());
                self.registry.make_room(&self.shared.name);
                let launch = self.launch.clone();
                let timeout = self.limits.connect_timeout;
                let sink = self.sink.clone();
                let itx = self.itx.clone();
                let redactor_cell = self.shared.redactor.clone();
                tokio::spawn(async move {
                    // Resolving may touch the keychain; keep it off the actor.
                    let resolved = tokio::task::spawn_blocking(move || launch.resolve())
                        .await
                        .unwrap_or_else(|e| Err(format!("secret lookup failed: {e}")));
                    let (spec, redactor) = match resolved {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = itx.send(Internal::Started(Err(e)));
                            return;
                        }
                    };
                    // stderr may speak before the handshake completes
                    *redactor_cell.write().expect("redactor") = Arc::new(redactor.clone());
                    let res = match spec.connect(timeout, Some(sink)).await {
                        Ok((c, container)) => match c.list_tools().await {
                            Ok(tools) => Ok((c, tools, redactor, container)),
                            Err(e) => {
                                c.shutdown().await;
                                Err(format!("tools/list failed: {e}"))
                            }
                        },
                        Err(e) => {
                            // the actor wraps this in ProtoError::Connect again
                            let msg = match e {
                                ProtoError::Connect(m) => m,
                                other => other.to_string(),
                            };
                            Err(redactor.redact(&msg).into_owned())
                        }
                    };
                    let _ = itx.send(Internal::Started(res.map(Box::new)));
                });
            }
            Action::Close => {
                if let Some(c) = self.conn.take() {
                    self.closing(close(c));
                }
            }
            Action::Retry => {
                if let Some(c) = self.conn.take() {
                    self.closing(close(c));
                }
                let attempt = match self.state {
                    State::Backoff { attempt } => attempt,
                    _ => 1,
                };
                if self.exiting.is_none() {
                    self.retry_at =
                        Some(tokio::time::Instant::now() + self.opts.backoff(attempt - 1));
                }
            }
        }
    }

    fn publish(&self) {
        let pid = self.conn.as_ref().and_then(|c| c.pid());
        let container = self.conn.as_ref().and(self.container.clone());
        let state = self.state.clone();
        self.shared.update(|s| {
            s.state = state;
            s.pid = pid;
            s.container = container;
        });
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Call {
                tool,
                args,
                meta,
                reply,
            } => {
                if let Some(e) = self.refusal() {
                    let _ = reply.send(Err(e));
                } else if matches!(self.state, State::Ready | State::Idle | State::Busy { .. }) {
                    self.dispatch(tool, args, meta, reply);
                } else {
                    // this call waits for a start
                    self.shared.update(|s| s.cold_starts += 1);
                    tracing::info!(
                        monotonic_counter.mcpgw_cold_starts = 1u64,
                        server = %self.shared.name,
                        runtime = self.launch.runtime_kind(),
                        "call waits for a start"
                    );
                    self.waiters.push(Waiter::Call {
                        tool,
                        args,
                        meta,
                        reply,
                    });
                    self.apply(Event::Demand);
                }
            }
            Cmd::Ensure(reply) => {
                if let Some(e) = self.refusal() {
                    let _ = reply.send(Err(e));
                } else if matches!(self.state, State::Ready | State::Idle | State::Busy { .. }) {
                    let _ = reply.send(Ok(()));
                } else {
                    self.waiters.push(Waiter::Ensure(reply));
                    self.apply(Event::Demand);
                }
            }
            Cmd::Stop(reply) => {
                self.retry_at = None;
                self.stop_replies.push(reply);
                self.apply(Event::Stop);
                if !self.state.is_connected() && self.state != State::Starting {
                    for r in self.stop_replies.drain(..) {
                        let _ = r.send(());
                    }
                }
            }
            Cmd::Reset => {
                self.failures = 0;
                if matches!(self.state, State::Backoff { .. }) {
                    self.retry_at = None;
                }
                self.apply(Event::Reset);
            }
            Cmd::SetEnabled(true, reply) => {
                self.disable_replies.clear();
                if self.state == State::Disabled {
                    self.shared.update(|s| s.last_error = None);
                    self.apply(Event::Enable);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::SetEnabled(false, reply) => {
                if self.state == State::Disabled {
                    let _ = reply.send(Ok(()));
                    return;
                }
                self.disable_replies.push(reply);
                self.retry_at = None;
                self.fail_waiters(&ProtoError::Unavailable(format!(
                    "server {} was disabled",
                    self.shared.name
                )));
                self.apply(Event::Stop);
            }
            Cmd::Shutdown(reply) => self.begin_exit(Some(reply)),
        }
    }

    fn begin_exit(&mut self, reply: Option<oneshot::Sender<()>>) {
        self.exiting = Some(reply.unwrap_or_else(|| oneshot::channel().0));
        self.retry_at = None;
        self.fail_waiters(&ProtoError::Unavailable("gateway is shutting down".into()));
        self.apply(Event::Stop);
    }

    /// Why a call can't run at all right now.
    fn refusal(&self) -> Option<ProtoError> {
        let name = &self.shared.name;
        let why = || self.shared.snapshot().last_error.unwrap_or_default();
        match self.state {
            _ if self.exiting.is_some() => {
                Some(ProtoError::Unavailable("gateway is shutting down".into()))
            }
            State::Disabled => Some(ProtoError::Unavailable(format!(
                "server {name} is {}",
                why()
            ))),
            State::Quarantined => Some(ProtoError::Unavailable(format!(
                "server {name} is quarantined: its tools changed since they were pinned ({}). \
                 Calls are blocked until someone reviews the change and runs `mcpgw pin {name}`",
                why()
            ))),
            State::Failed => Some(ProtoError::Unavailable(format!(
                "server {name} failed to start {} times (last error: {}); fix it and run `mcpgw restart {name}`",
                self.failures,
                why()
            ))),
            _ => None,
        }
    }

    fn dispatch(
        &mut self,
        tool: String,
        args: Option<JsonObject>,
        meta: Option<JsonObject>,
        reply: Reply<JsonObject>,
    ) {
        if reply.is_closed() {
            return; // the caller gave up while queued; don't run it late
        }
        let Some(conn) = self.conn.clone() else {
            let _ = reply.send(Err(ProtoError::Unavailable("not connected".into())));
            return;
        };
        self.apply(Event::CallStart);
        self.touch();
        let timeout = self.opts.call_timeout;
        let itx = self.itx.clone();
        let redactor = self.shared.redactor();
        tokio::spawn(async move {
            let res = match conn.call_tool_with_meta(&tool, args, meta, timeout).await {
                Ok(r) if !redactor.is_empty() => {
                    let mut v = serde_json::Value::Object(r);
                    redactor.redact_json(&mut v);
                    match v {
                        serde_json::Value::Object(o) => Ok(o),
                        _ => unreachable!("redaction keeps the shape"),
                    }
                }
                Err(e) => Err(redact_error(e, &redactor)),
                ok => ok,
            };
            let lost = matches!(res, Err(ProtoError::Closed)) || conn.is_closed();
            // A transport error (not a tool's own JSON-RPC error) may mean the
            // server died a moment before its pipes report it.
            let failed = matches!(&res, Err(e) if !matches!(e, ProtoError::Rpc { .. }));
            let _ = reply.send(res);
            let _ = itx.send(Internal::CallDone { lost, failed });
        });
    }

    fn fail_waiters(&mut self, err: &ProtoError) {
        for w in self.waiters.drain(..) {
            match w {
                Waiter::Call { reply, .. } => {
                    let _ = reply.send(Err(err.clone()));
                }
                Waiter::Ensure(reply) => {
                    let _ = reply.send(Err(err.clone()));
                }
            }
        }
    }

    fn on_internal(&mut self, ev: Internal) {
        match ev {
            Internal::PostMortem { lost, cause } => {
                tracing::warn!(server = %self.shared.name, %cause, "container died");
                (self.sink)(&format!("mcpgw: {cause}"));
                // unless something newer has been reported since
                self.shared.update(|s| {
                    if s.last_error.as_deref() == Some(lost.as_str()) {
                        s.last_error = Some(format!("{lost}: {cause}"));
                    }
                });
            }
            Internal::Started(Ok(started)) => {
                let (conn, tools, redactor, container) = *started;
                *self.shared.redactor.write().expect("redactor") = Arc::new(redactor);
                self.container = container;
                let pv = conn.protocol_version();
                self.conn = Some(Arc::new(conn));
                if let Some(diff) = self.drift(pv.clone(), &tools) {
                    tracing::warn!(server = %self.shared.name, %diff, "tools changed since pinned; quarantined");
                    self.shared
                        .update(|s| s.last_error = Some(diff.to_string()));
                    self.apply(Event::Drift);
                    if let Some(e) = self.refusal() {
                        self.fail_waiters(&e);
                    }
                    return;
                }
                // Record the connection's facts (protocol, start time, its
                // tools and manifest) before `Ready` is published, so a reader
                // never sees Ready without them.
                let started_ms = self
                    .start_began
                    .take()
                    .map(|t| t.elapsed().as_millis() as u64);
                if let Some(ms) = started_ms {
                    // cold-start time per runtime (node, uvx, docker, http …)
                    tracing::info!(
                        histogram.mcpgw_server_start_ms = ms,
                        server = %self.shared.name,
                        runtime = self.launch.runtime_kind(),
                        "server started"
                    );
                }
                self.shared.update(|s| {
                    s.protocol_version = pv.clone();
                    if started_ms.is_some() {
                        s.last_start_ms = started_ms;
                    }
                });
                let tool_count = tools.len();
                if !self.shared.catalog.contains(&self.shared.catalog_key) {
                    let m = manifest_from(&self.shared.catalog_key, pv.clone(), &tools);
                    // First sight of tools nobody reviewed (no pin yet): if
                    // they look poisoned, hold them like a drift (M3-10).
                    let others = self
                        .shared
                        .catalog
                        .tool_names_except(&self.shared.catalog_key);
                    let findings = mcpgw_core::scan::scan_tools(&m.tools, &others);
                    if mcpgw_core::scan::has_high(&findings) {
                        let high: Vec<&mcpgw_core::scan::Finding> = findings
                            .iter()
                            .filter(|f| f.severity == mcpgw_core::scan::Severity::High)
                            .collect();
                        let why = format!(
                            "tool scan: {} high-risk finding(s), e.g. {}: {}; review with `mcpgw scan {}`, accept with `mcpgw pin {}`",
                            high.len(),
                            high[0].tool,
                            high[0].message,
                            self.shared.catalog_key,
                            self.shared.catalog_key
                        );
                        tracing::warn!(server = %self.shared.name, %why, "tools look poisoned; quarantined");
                        *self.shared.pending.lock().expect("pending") = Some(m);
                        self.shared.update(|s| s.last_error = Some(why));
                        self.apply(Event::Drift);
                        if let Some(e) = self.refusal() {
                            self.fail_waiters(&e);
                        }
                        return;
                    }
                    tracing::info!(server = %self.shared.name, pin = %m.sha256, tools = m.tools.len(), "captured manifest");
                    self.shared.catalog.record(m);
                }
                *self.shared.tools.write().expect("tools lock") = tools;
                if !self.apply(Event::StartOk) {
                    if let Some(c) = self.conn.take() {
                        self.closing(close(c));
                    }
                    return;
                }
                if self.state == State::Ready {
                    tracing::info!(server = %self.shared.name, protocol = ?pv, tools = tool_count, "backend connected");
                    if self.ever_connected {
                        self.shared.update(|s| s.restarts += 1);
                    }
                    self.ever_connected = true;
                    self.failures = 0;
                    self.touch();
                    self.shared.update(|s| s.last_error = None);
                    for w in std::mem::take(&mut self.waiters) {
                        match w {
                            Waiter::Call {
                                tool,
                                args,
                                meta,
                                reply,
                            } => self.dispatch(tool, args, meta, reply),
                            Waiter::Ensure(reply) => {
                                let _ = reply.send(Ok(()));
                            }
                        }
                    }
                }
            }
            Internal::Started(Err(mut e)) => {
                if e.contains("requires authentication") {
                    let account = self
                        .shared
                        .name
                        .split_once('@')
                        .map(|(_, a)| format!(" --account {a}"))
                        .unwrap_or_default();
                    e.push_str(&format!(
                        "; sign in with `mcpgw auth connect {}{account}`",
                        self.shared.catalog_key
                    ));
                }
                tracing::warn!(server = %self.shared.name, error = %e, attempt = self.failures + 1, "backend start failed");
                self.shared.update(|s| s.last_error = Some(e.clone()));
                self.apply(Event::StartErr);
                self.failures += 1;
                self.fail_waiters(&ProtoError::Connect(e));
            }
            Internal::CallDone { lost, failed } => {
                self.apply(Event::CallEnd);
                self.touch();
                if lost {
                    self.lost("connection closed".into());
                } else if failed {
                    let itx = self.itx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let _ = itx.send(Internal::Recheck);
                    });
                }
            }
            Internal::Recheck => {
                if self.conn.as_ref().is_some_and(|c| c.is_closed()) {
                    self.lost("connection closed".into());
                }
            }
            Internal::Ping(Ok(())) => self.ping_inflight = false,
            Internal::Ping(Err(e)) => {
                self.ping_inflight = false;
                self.lost(format!("health check failed: {e}"));
            }
        }
    }

    /// Compare the tools a server reports on start with its pin. On a
    /// mismatch, keeps the live manifest for approval and returns the diff.
    fn drift(&mut self, pv: Option<String>, tools: &[JsonObject]) -> Option<ManifestDiff> {
        let pinned = self.shared.catalog.get(&self.shared.catalog_key)?;
        let live = manifest_from(&self.shared.catalog_key, pv, tools);
        if live.sha256 == pinned.sha256 {
            return None;
        }
        let diff = ManifestDiff::between(&pinned, &live);
        *self.shared.pending.lock().expect("pending") = Some(live);
        Some(diff)
    }

    fn lost(&mut self, why: String) {
        if !matches!(
            self.state,
            State::Ready | State::Idle | State::Busy { .. } | State::Draining { .. }
        ) {
            return;
        }
        // A stdio server stopped by its memory cap says so.
        let why = match self.oom_note() {
            Some(note) => {
                (self.sink)(&format!("mcpgw: {note}"));
                format!("{why}: {note}")
            }
            None => why,
        };
        tracing::warn!(server = %self.shared.name, error = %why, "backend lost");
        self.shared.update(|s| s.last_error = Some(why.clone()));
        // A dead container is inspected before it's removed, to say why it
        // died (out of memory at its limit, an exit code).
        if let (Some(info), Some(conn)) = (self.container.clone(), self.conn.take()) {
            let itx = self.itx.clone();
            self.closing(async move {
                if let Some(cause) = crate::container::post_mortem(&info).await {
                    let _ = itx.send(Internal::PostMortem { lost: why, cause });
                }
                close(conn).await;
            });
        }
        self.apply(Event::Lost);
    }

    fn check_health(&mut self) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        if self.ping_inflight || !self.state.is_connected() {
            return;
        }
        if conn.is_closed() {
            self.lost("connection closed".into());
            return;
        }
        self.ping_inflight = true;
        let timeout = self.opts.ping_timeout;
        let itx = self.itx.clone();
        tokio::spawn(async move {
            let r = conn.ping(timeout).await.map_err(|e| e.to_string());
            let _ = itx.send(Internal::Ping(r));
        });
    }

    /// Why a stdio server with a memory cap died, if the cap stopped it:
    /// the watchdog's or the job's own note, else its last stderr lines
    /// saying it ran out of memory (the kernel refused an allocation).
    fn oom_note(&self) -> Option<String> {
        let Template::Stdio(spec) = &self.launch.template else {
            return None;
        };
        let limit = spec.memory_limit?;
        if let Some(n) = self.conn.as_ref().and_then(|c| c.death_note()) {
            return Some(n);
        }
        let tail = self.shared.stderr.lock().ok()?;
        tail.iter()
            .rev()
            .take(20)
            .any(|l| mcpgw_proto::limits::mentions_oom(l))
            .then(|| format!("out of memory at its {} limit", mcpgw_core::ByteSize(limit)))
    }

    /// Close a connection in the background, tracked so the actor waits for
    /// it before exiting (the runtime may end right after).
    fn closing(&mut self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        self.closes.retain(|t| !t.is_finished());
        self.closes.push(tokio::spawn(f));
    }

    fn on_retry(&mut self) {
        self.retry_at = None;
        self.apply(Event::RetryDue);
        if self.exiting.is_none()
            && (self.opts.eager || self.limits.warm || !self.waiters.is_empty())
        {
            self.apply(Event::Demand);
        }
    }
}

async fn close(conn: Arc<BackendConn>) {
    match Arc::try_unwrap(conn) {
        Ok(c) => c.shutdown().await,
        Err(c) => drop(c), // an in-flight call holds a ref; the tree dies on last drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_kinds_for_metrics() {
        let kind = |command: &str| {
            let spec = ServerSpec::new("s", Transport::stdio(command, vec![]));
            Launch::new(&spec, None, "s", "default").runtime_kind()
        };
        assert_eq!(kind("npx"), "npx");
        assert_eq!(kind("/usr/local/bin/node"), "node");
        assert_eq!(
            kind("/data/runtimes/pypi/mcp-server-git-1a2b/venv/bin/mcp-server-git"),
            "python"
        );
        assert_eq!(kind("/usr/bin/python3"), "python3");
        let http = ServerSpec::new("h", Transport::http("https://x/mcp"));
        assert_eq!(
            Launch::new(&http, None, "h", "default").runtime_kind(),
            "http"
        );
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let o = ManagerOptions {
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
            ..Default::default()
        };
        assert_eq!(o.backoff(0), Duration::from_millis(100));
        assert_eq!(o.backoff(1), Duration::from_millis(200));
        assert_eq!(o.backoff(3), Duration::from_millis(800));
        assert_eq!(o.backoff(4), Duration::from_secs(1));
        assert_eq!(o.backoff(1000), Duration::from_secs(1));
    }
}
