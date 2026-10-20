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

use mcpgw_core::{ManifestDiff, ServerSpec, ToolManifest, Transport};
use mcpgw_proto::{BackendConn, JsonObject, ProtoError, StderrSink, StdioSpec};
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::{
    catalog::{Catalog, manifest_from},
    state::{Action, Event, State, step},
};

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
    /// Last start, call start or call end.
    pub last_used: Option<Instant>,
    /// Never stopped for idleness or evicted.
    pub stateful: bool,
}

struct Shared {
    name: String,
    /// The live manifest that differs from the pin, while quarantined.
    pending: std::sync::Mutex<Option<ToolManifest>>,
    status: std::sync::RwLock<BackendStatus>,
    tools: RwLock<Vec<JsonObject>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    catalog: Arc<Catalog>,
}

impl Shared {
    fn sink(&self) -> StderrSink {
        let buf = self.stderr.clone();
        let name = self.name.clone();
        Arc::new(move |line: &str| {
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
        reply: Reply<JsonObject>,
    },
    Ensure(Reply<()>),
    Stop(oneshot::Sender<()>),
    Reset,
    Shutdown(oneshot::Sender<()>),
}

enum Waiter {
    Call {
        tool: String,
        args: Option<JsonObject>,
        reply: Reply<JsonObject>,
    },
    Ensure(Reply<()>),
}

enum Internal {
    Started(Result<(BackendConn, Vec<JsonObject>), String>),
    CallDone { lost: bool, failed: bool },
    Ping(Result<(), String>),
    Recheck,
}

#[derive(Clone)]
struct Handle {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Cmd>,
}

/// What every actor knows about its peers, for `max_running` eviction.
struct Registry {
    peers: std::sync::OnceLock<Vec<Handle>>,
    max_running: usize,
}

impl Registry {
    /// Before `me` starts: if the cap is reached, stop the least recently
    /// used idle, non-stateful peer. The cap is soft: when every running
    /// server is busy or stateful, `me` starts anyway.
    fn make_room(&self, me: &str) {
        let Some(peers) = self.peers.get() else {
            return;
        };
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
            .filter(|(s, _)| matches!(s.state, State::Ready | State::Idle) && !s.stateful)
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

/// Owns all backend actors.
pub struct BackendManager {
    backends: BTreeMap<String, Handle>,
    opts: ManagerOptions,
    catalog: Arc<Catalog>,
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
        let mut backends = BTreeMap::new();
        let mut tasks = Vec::new();
        let registry = Arc::new(Registry {
            peers: std::sync::OnceLock::new(),
            max_running: opts.max_running.max(1),
        });
        for cfg in servers {
            let (state, why) = match (&cfg.transport, cfg.enabled) {
                (_, false) => (State::Disabled, Some("disabled".to_string())),
                (Transport::Http { .. }, _) => (
                    State::Disabled,
                    Some("http transport arrives in M2-04".to_string()),
                ),
                (Transport::Stdio { .. }, true) => (State::Cold, None),
            };
            let shared = Arc::new(Shared {
                name: cfg.name.clone(),
                pending: std::sync::Mutex::new(None),
                status: std::sync::RwLock::new(BackendStatus {
                    name: cfg.name.clone(),
                    state: state.clone(),
                    restarts: 0,
                    protocol_version: None,
                    last_error: why,
                    pid: None,
                    last_used: None,
                    stateful: cfg.stateful,
                }),
                tools: RwLock::new(Vec::new()),
                stderr: Arc::default(),
                catalog: catalog.clone(),
            });
            let (tx, rx) = mpsc::unbounded_channel();
            if let Transport::Stdio {
                command,
                args,
                env,
                cwd,
            } = &cfg.transport
            {
                let spec = StdioSpec {
                    command: command.clone(),
                    args: args.clone(),
                    env: env.clone(),
                    cwd: cwd.clone(),
                };
                let connect_timeout = cfg
                    .startup_timeout
                    .map(|d| d.get())
                    .unwrap_or(opts.connect_timeout);
                let idle_timeout = cfg
                    .idle_timeout
                    .map(|d| d.get())
                    .unwrap_or(opts.idle_timeout);
                let actor = Actor::new(
                    shared.clone(),
                    spec,
                    opts.clone(),
                    Limits {
                        connect_timeout,
                        idle_timeout,
                        stateful: cfg.stateful,
                    },
                    state,
                    rx,
                    registry.clone(),
                );
                tasks.push(tokio::spawn(actor.run()));
                if opts.eager && cfg.enabled {
                    let (reply, _) = oneshot::channel();
                    let _ = tx.send(Cmd::Ensure(reply));
                }
            }
            backends.insert(cfg.name.clone(), Handle { shared, tx });
        }
        let _ = registry.peers.set(backends.values().cloned().collect());
        Self {
            backends,
            opts,
            catalog,
            tasks: std::sync::Mutex::new(tasks),
        }
    }

    /// The tool catalog.
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// Names of all configured backends.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.keys().map(String::as_str)
    }

    /// Status of every backend, ordered by name.
    pub async fn status(&self) -> Vec<BackendStatus> {
        self.backends
            .values()
            .map(|h| h.shared.snapshot())
            .collect()
    }

    /// Status of one backend.
    pub fn status_of(&self, server: &str) -> Option<BackendStatus> {
        self.backends.get(server).map(|h| h.shared.snapshot())
    }

    /// Wait until no backend is starting (and, when eager, none is still
    /// cold), or `timeout` elapses. Returns whether everything settled.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let busy = self.backends.values().any(|h| {
                let s = h.shared.snapshot().state;
                s == State::Starting || (self.opts.eager && s == State::Cold)
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
        match self.backends.get(server) {
            Some(h) if h.shared.snapshot().state != State::Disabled => self.catalog.tools(server),
            _ => Vec::new(),
        }
    }

    /// The tools the running server reported on its last start.
    pub async fn live_tools(&self, server: &str) -> Vec<JsonObject> {
        match self.backends.get(server) {
            Some(h) => h.shared.tools.read().await.clone(),
            None => Vec::new(),
        }
    }

    /// The last lines a server wrote to stderr (across restarts).
    pub fn stderr_tail(&self, server: &str) -> Vec<String> {
        match self.backends.get(server) {
            Some(h) => h
                .shared
                .stderr
                .lock()
                .expect("stderr buffer")
                .iter()
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    fn handle(&self, server: &str) -> Result<&Handle, ProtoError> {
        self.backends
            .get(server)
            .ok_or_else(|| ProtoError::Unavailable(format!("unknown server {server}")))
    }

    /// Call a tool, starting the server first if it is cold.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let h = self.handle(server)?;
        let (reply, rx) = oneshot::channel();
        h.tx.send(Cmd::Call {
            tool: tool.to_string(),
            args: arguments,
            reply,
        })
        .map_err(|_| ProtoError::Unavailable(format!("server {server} is shut down")))?;
        let limit = self.opts.connect_timeout + self.opts.call_timeout;
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ProtoError::Unavailable(format!(
                "server {server} is shut down"
            ))),
            Err(_) => Err(ProtoError::Timeout(limit)),
        }
    }

    /// Start a server (if needed) and wait until it is connected.
    pub async fn ensure(&self, server: &str) -> Result<(), ProtoError> {
        let h = self.handle(server)?;
        let (reply, rx) = oneshot::channel();
        h.tx.send(Cmd::Ensure(reply))
            .map_err(|_| ProtoError::Unavailable(format!("server {server} is shut down")))?;
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
        self.backends
            .get(server)?
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
        let mut waits = Vec::new();
        for h in self.backends.values() {
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
        for h in self.backends.values() {
            if h.shared.snapshot().state != State::Disabled {
                h.shared.update(|s| s.state = State::Cold);
            }
        }
    }
}

/// Per-server limits resolved from the spec and the defaults.
struct Limits {
    connect_timeout: Duration,
    idle_timeout: Duration,
    stateful: bool,
}

struct Actor {
    shared: Arc<Shared>,
    spec: StdioSpec,
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
    waiters: Vec<Waiter>,
    retry_at: Option<tokio::time::Instant>,
    ping_inflight: bool,
    stop_replies: Vec<oneshot::Sender<()>>,
    exiting: Option<oneshot::Sender<()>>,
}

impl Actor {
    fn new(
        shared: Arc<Shared>,
        spec: StdioSpec,
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
            spec,
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
            waiters: Vec::new(),
            retry_at: None,
            ping_inflight: false,
            stop_replies: Vec::new(),
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
        self.publish();
        if let Some(r) = self.exiting.take() {
            let _ = r.send(());
        }
    }

    /// Keep the idle timer armed exactly while the server is connected and
    /// unused (and not stateful).
    fn rearm_idle(&mut self) {
        let idle = matches!(self.state, State::Ready | State::Idle) && !self.limits.stateful;
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
                self.registry.make_room(&self.shared.name);
                let spec = self.spec.clone();
                let timeout = self.limits.connect_timeout;
                let sink = self.sink.clone();
                let itx = self.itx.clone();
                tokio::spawn(async move {
                    let res = match BackendConn::connect_stdio(&spec, timeout, Some(sink)).await {
                        Ok(c) => match c.list_tools().await {
                            Ok(tools) => Ok((c, tools)),
                            Err(e) => {
                                c.shutdown().await;
                                Err(format!("tools/list failed: {e}"))
                            }
                        },
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = itx.send(Internal::Started(res));
                });
            }
            Action::Close => {
                if let Some(c) = self.conn.take() {
                    tokio::spawn(close(c));
                }
            }
            Action::Retry => {
                if let Some(c) = self.conn.take() {
                    tokio::spawn(close(c));
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
        let state = self.state.clone();
        self.shared.update(|s| {
            s.state = state;
            s.pid = pid;
        });
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Call { tool, args, reply } => {
                if let Some(e) = self.refusal() {
                    let _ = reply.send(Err(e));
                } else if matches!(self.state, State::Ready | State::Idle | State::Busy { .. }) {
                    self.dispatch(tool, args, reply);
                } else {
                    self.waiters.push(Waiter::Call { tool, args, reply });
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
                self.apply(Event::Reset);
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

    fn dispatch(&mut self, tool: String, args: Option<JsonObject>, reply: Reply<JsonObject>) {
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
        tokio::spawn(async move {
            let res = conn.call_tool(&tool, args, timeout).await;
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
            Internal::Started(Ok((conn, tools))) => {
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
                if !self.apply(Event::StartOk) {
                    if let Some(c) = self.conn.take() {
                        tokio::spawn(close(c));
                    }
                    return;
                }
                if self.state == State::Ready {
                    tracing::info!(server = %self.shared.name, protocol = ?pv, tools = tools.len(), "backend connected");
                    if self.ever_connected {
                        self.shared.update(|s| s.restarts += 1);
                    }
                    self.ever_connected = true;
                    self.failures = 0;
                    self.touch();
                    if !self.shared.catalog.contains(&self.shared.name) {
                        let m = manifest_from(&self.shared.name, pv.clone(), &tools);
                        tracing::info!(server = %self.shared.name, pin = %m.sha256, tools = m.tools.len(), "captured manifest");
                        self.shared.catalog.record(m);
                    }
                    self.shared.update(|s| {
                        s.protocol_version = pv;
                        s.last_error = None;
                    });
                    if let Ok(mut t) = self.shared.tools.try_write() {
                        *t = tools;
                    } else {
                        let shared = self.shared.clone();
                        tokio::spawn(async move { *shared.tools.write().await = tools });
                    }
                    for w in std::mem::take(&mut self.waiters) {
                        match w {
                            Waiter::Call { tool, args, reply } => self.dispatch(tool, args, reply),
                            Waiter::Ensure(reply) => {
                                let _ = reply.send(Ok(()));
                            }
                        }
                    }
                }
            }
            Internal::Started(Err(e)) => {
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
        let pinned = self.shared.catalog.get(&self.shared.name)?;
        let live = manifest_from(&self.shared.name, pv, tools);
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
        tracing::warn!(server = %self.shared.name, error = %why, "backend lost");
        self.shared.update(|s| s.last_error = Some(why));
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

    fn on_retry(&mut self) {
        self.retry_at = None;
        self.apply(Event::RetryDue);
        if self.exiting.is_none() && (self.opts.eager || !self.waiters.is_empty()) {
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
