//! Backend process lifecycle (M1-03): spawn, health-check, restart, shutdown.
//!
//! One supervisor task per backend owns its connection:
//! connect → (ping every `health_interval` | wake on reported failure) →
//! on failure drop the connection and reconnect with exponential backoff.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_proto::{BackendConn, JsonObject, ProtoError, StdioSpec};
use tokio::sync::{Notify, RwLock, watch};

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

/// Observable state of a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendState {
    /// First connection attempt in progress.
    Starting,
    /// Connected and healthy.
    Running {
        /// Negotiated protocol version.
        protocol_version: String,
    },
    /// Waiting to reconnect after a failure.
    Restarting {
        /// Consecutive failed attempts.
        attempt: u32,
        /// Most recent error.
        last_error: String,
    },
    /// Not started: transport not supported yet, or server disabled.
    Unsupported(String),
    /// Shut down.
    Stopped,
}

/// Status snapshot for one backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    /// Server name.
    pub name: String,
    /// Current state.
    pub state: BackendState,
    /// Successful reconnects after a lost connection.
    pub restarts: u32,
}

struct Backend {
    name: String,
    conn: RwLock<Option<Arc<BackendConn>>>,
    tools: RwLock<Vec<JsonObject>>,
    state: RwLock<BackendState>,
    restarts: AtomicU32,
    wake: Notify,
}

impl Backend {
    async fn set_state(&self, s: BackendState) {
        *self.state.write().await = s;
    }
}

/// Owns all backend connections.
pub struct BackendManager {
    backends: BTreeMap<String, Arc<Backend>>,
    opts: ManagerOptions,
    stop: watch::Sender<bool>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl BackendManager {
    /// Start supervising `servers`. Returns immediately; connections are
    /// established in the background (see [`BackendManager::wait_ready`]).
    pub fn start(servers: &[ServerSpec], opts: ManagerOptions) -> Self {
        let (stop, _) = watch::channel(false);
        let mut backends = BTreeMap::new();
        let mut tasks = Vec::new();
        for cfg in servers {
            let initial = match (&cfg.transport, cfg.enabled) {
                (_, false) => BackendState::Unsupported("disabled".into()),
                (Transport::Http { .. }, _) => {
                    BackendState::Unsupported("http transport arrives in M2".into())
                }
                (Transport::Stdio { .. }, true) => BackendState::Starting,
            };
            let b = Arc::new(Backend {
                name: cfg.name.clone(),
                conn: RwLock::new(None),
                tools: RwLock::new(Vec::new()),
                state: RwLock::new(initial.clone()),
                restarts: AtomicU32::new(0),
                wake: Notify::new(),
            });
            if let (
                BackendState::Starting,
                Transport::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                },
            ) = (&initial, &cfg.transport)
            {
                let spec = StdioSpec {
                    command: command.clone(),
                    args: args.clone(),
                    env: env.clone(),
                    cwd: cwd.clone(),
                };
                tasks.push(tokio::spawn(supervise(
                    b.clone(),
                    spec,
                    opts.clone(),
                    stop.subscribe(),
                )));
            }
            backends.insert(cfg.name.clone(), b);
        }
        Self {
            backends,
            opts,
            stop,
            tasks: std::sync::Mutex::new(tasks),
        }
    }

    /// Names of all configured backends.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.keys().map(String::as_str)
    }

    /// Status of every backend, ordered by name.
    pub async fn status(&self) -> Vec<BackendStatus> {
        let mut out = Vec::new();
        for b in self.backends.values() {
            out.push(BackendStatus {
                name: b.name.clone(),
                state: b.state.read().await.clone(),
                restarts: b.restarts.load(Ordering::Relaxed),
            });
        }
        out
    }

    /// Wait until no backend is in `Starting` (connected or failed once),
    /// or `timeout` elapses. Returns whether everything settled.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let starting = self
                .status()
                .await
                .iter()
                .any(|s| s.state == BackendState::Starting);
            if !starting {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Cached tool list of one backend (refreshed on every (re)connect).
    pub async fn tools(&self, server: &str) -> Vec<JsonObject> {
        match self.backends.get(server) {
            Some(b) => b.tools.read().await.clone(),
            None => Vec::new(),
        }
    }

    /// Call a tool on a backend.
    ///
    /// A `Closed` failure wakes the supervisor so it reconnects immediately.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<JsonObject, ProtoError> {
        let b = self
            .backends
            .get(server)
            .ok_or_else(|| ProtoError::Unavailable(format!("unknown server {server}")))?;
        let conn =
            b.conn.read().await.clone().ok_or_else(|| {
                ProtoError::Unavailable(format!("server {server} is not connected"))
            })?;
        let res = conn
            .call_tool(tool, arguments, self.opts.call_timeout)
            .await;
        if matches!(res, Err(ProtoError::Closed)) || conn.is_closed() {
            b.wake.notify_one();
        }
        res
    }

    /// Stop all supervisors and terminate every backend process.
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let tasks = std::mem::take(&mut *self.tasks.lock().expect("tasks mutex"));
        for t in tasks {
            let _ = t.await;
        }
    }
}

impl Drop for BackendManager {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

async fn close(conn: Option<Arc<BackendConn>>) {
    if let Some(c) = conn {
        match Arc::try_unwrap(c) {
            Ok(c) => c.shutdown().await,
            Err(c) => drop(c), // in-flight call holds a ref; child is killed on last drop
        }
    }
}

/// Sleep for `d` unless stopped first. Returns true if stopped.
async fn sleep_or_stop(d: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    if *stop.borrow() {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(d) => *stop.borrow(),
        _ = stop.changed() => true,
    }
}

async fn supervise(
    b: Arc<Backend>,
    spec: StdioSpec,
    opts: ManagerOptions,
    mut stop: watch::Receiver<bool>,
) {
    let mut attempt: u32 = 0;
    let mut ever_connected = false;
    loop {
        if *stop.borrow() {
            break;
        }
        let connected = match BackendConn::connect_stdio(&spec, opts.connect_timeout).await {
            Ok(c) => match c.list_tools().await {
                Ok(tools) => Ok((c, tools)),
                Err(e) => {
                    c.shutdown().await;
                    Err(format!("tools/list failed: {e}"))
                }
            },
            Err(e) => Err(e.to_string()),
        };
        match connected {
            Ok((conn, tools)) => {
                let conn = Arc::new(conn);
                let pv = conn.protocol_version().unwrap_or_default();
                tracing::info!(server = %b.name, protocol = %pv, tools = tools.len(), "backend connected");
                *b.tools.write().await = tools;
                *b.conn.write().await = Some(conn.clone());
                if ever_connected {
                    b.restarts.fetch_add(1, Ordering::Relaxed);
                }
                ever_connected = true;
                attempt = 0;
                b.set_state(BackendState::Running {
                    protocol_version: pv,
                })
                .await;

                // Monitor until unhealthy or stopped.
                let reason = loop {
                    tokio::select! {
                        _ = tokio::time::sleep(opts.health_interval) => {}
                        _ = b.wake.notified() => {}
                        _ = stop.changed() => {}
                    }
                    if *stop.borrow() {
                        break None;
                    }
                    if conn.is_closed() {
                        break Some("connection closed".to_string());
                    }
                    if let Err(e) = conn.ping(opts.ping_timeout).await {
                        break Some(format!("health check failed: {e}"));
                    }
                };
                let old = b.conn.write().await.take();
                drop(conn);
                close(old).await;
                b.tools.write().await.clear();
                match reason {
                    None => break,
                    Some(err) => {
                        tracing::warn!(server = %b.name, error = %err, "backend lost; restarting");
                        b.set_state(BackendState::Restarting {
                            attempt: 0,
                            last_error: err,
                        })
                        .await;
                    }
                }
                // Reconnect quickly after a previously healthy session.
                if sleep_or_stop(opts.backoff_initial, &mut stop).await {
                    break;
                }
            }
            Err(err) => {
                tracing::warn!(server = %b.name, error = %err, attempt, "backend connect failed");
                b.set_state(BackendState::Restarting {
                    attempt: attempt + 1,
                    last_error: err,
                })
                .await;
                let delay = opts.backoff(attempt);
                attempt = attempt.saturating_add(1);
                if sleep_or_stop(delay, &mut stop).await {
                    break;
                }
            }
        }
    }
    close(b.conn.write().await.take()).await;
    b.set_state(BackendState::Stopped).await;
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
