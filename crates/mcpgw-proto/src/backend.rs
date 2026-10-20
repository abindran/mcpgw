//! Connections from the gateway to backend MCP servers (M1-03).
//!
//! The public API is `rmcp`-free: tools, arguments and results cross the seam
//! as JSON objects so other crates never touch protocol types.

use std::{collections::BTreeMap, process::Stdio, sync::Arc, time::Duration};

use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient, ServiceError,
    model::{
        CallToolRequestParams, ClientRequest, PingRequest, ProtocolVersion, ServerResult, Tool,
    },
    service::{PeerRequestOptions, RunningService},
};
use serde_json::{Map, Value};

/// A JSON object.
pub type JsonObject = Map<String, Value>;

/// Errors crossing the protocol seam.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtoError {
    /// The peer returned a JSON-RPC error.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i32,
        /// Error message.
        message: String,
        /// Optional error data.
        data: Option<Value>,
    },
    /// The request did not complete in time.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The connection is gone (process exited, pipe closed, ...).
    #[error("backend connection closed")]
    Closed,
    /// Spawning or handshaking failed.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The backend is not currently connected (starting/restarting/disabled).
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    /// The call needs the user's answer first (approvals, M2-09). The front
    /// end asks the client (MRTR or elicitation) and calls again with the
    /// answer in [`crate::front::CallContext::input`].
    #[error("input required: {message}")]
    Elicit {
        /// Handle echoed back with the answer.
        state: String,
        /// The question.
        message: String,
        /// JSON Schema of the expected answer (an elicitation form).
        schema: Value,
    },
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// JSON-RPC "invalid params"; used for unknown tools.
pub const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC "internal error".
pub const INTERNAL_ERROR: i32 = -32603;

impl From<ServiceError> for ProtoError {
    fn from(e: ServiceError) -> Self {
        match e {
            ServiceError::McpError(e) => ProtoError::Rpc {
                code: e.code.0,
                message: e.message.into_owned(),
                data: e.data,
            },
            ServiceError::Timeout { timeout } => ProtoError::Timeout(timeout),
            ServiceError::TransportClosed | ServiceError::TransportSend(_) => ProtoError::Closed,
            other => ProtoError::Other(other.to_string()),
        }
    }
}

/// Receives each line a stdio backend writes to stderr.
pub type StderrSink = Arc<dyn Fn(&str) + Send + Sync>;

/// How to launch a stdio backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioSpec {
    /// Executable.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
    /// Working directory.
    pub cwd: Option<std::path::PathBuf>,
    /// Commands run in order after the process is gone, e.g. `docker rm -f
    /// <name>` for a container the runtime CLI was attached to. They run
    /// once: on shutdown (awaited), after a failed handshake (awaited,
    /// before any retry), or in the background when the connection is
    /// dropped.
    pub cleanup: Vec<Vec<String>>,
    /// Commands run after `cleanup` once the connection is over for good:
    /// at its end, or after the last failed attempt, but not between a
    /// failed handshake and its retry (e.g. removing a container's network,
    /// which the retry still needs).
    pub teardown: Vec<Vec<String>>,
    /// Memory cap for the process tree, in bytes (see [`crate::limits`]).
    pub memory_limit: Option<u64>,
}

/// Run cleanup commands in order, quietly, each within 30 s.
async fn run_cleanup(commands: Vec<Vec<String>>) {
    for argv in commands {
        let Some((cmd, args)) = argv.split_first() else {
            continue;
        };
        let mut c = tokio::process::Command::new(cmd);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(30), c.status()).await {
            Ok(Ok(s)) if s.success() => tracing::debug!(command = ?argv, "cleanup done"),
            Ok(Ok(s)) => tracing::debug!(command = ?argv, status = %s, "cleanup exited non-zero"),
            Ok(Err(e)) => tracing::warn!(command = ?argv, error = %e, "cleanup failed"),
            Err(_) => tracing::warn!(command = ?argv, "cleanup timed out"),
        }
    }
}

/// The gateway's side of a backend session: it routes the backend's
/// progress to the calls that asked (M4-09) and declares the tasks
/// extension, so a backend may run a long call as a task.
#[derive(Clone, Default)]
struct GatewayClient {
    routes: Arc<crate::progress::Routes>,
}

impl ClientHandler for GatewayClient {
    fn get_info(&self) -> rmcp::model::ClientConfig {
        let mut cfg = rmcp::model::ClientConfig::default();
        cfg.capabilities = rmcp::model::ClientCapabilities::builder()
            .enable_tasks()
            .build();
        cfg
    }

    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) {
        self.routes.deliver(
            params.progress_token,
            crate::progress::Progress {
                progress: params.progress,
                total: params.total,
                message: params.message,
            },
        );
    }
}

/// Limits for one call (M4-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLimits {
    /// How long the backend may stay silent: the call fails when neither
    /// the result nor progress arrives for this long.
    pub idle: Duration,
    /// How long the call may run at all, progress or not.
    pub max: Duration,
}

impl CallLimits {
    /// `timeout` for both: a call that must finish in `timeout`.
    pub fn fixed(timeout: Duration) -> Self {
        Self {
            idle: timeout,
            max: timeout,
        }
    }
}

/// Sends `notifications/cancelled` for a request whose caller went away.
struct CancelOnDrop {
    peer: Option<rmcp::service::Peer<RoleClient>>,
    id: rmcp::model::RequestId,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(peer) = self.peer.take() else {
            return;
        };
        let id = self.id.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let p = rmcp::model::CancelledNotificationParam::new(
                    Some(id),
                    Some("cancelled by the client".into()),
                );
                let _ = peer.notify_cancelled(p).await;
            });
        }
    }
}

/// Sends `tasks/cancel` for a backend task whose caller went away.
struct CancelTaskOnDrop {
    peer: Option<rmcp::service::Peer<RoleClient>>,
    id: String,
}

impl Drop for CancelTaskOnDrop {
    fn drop(&mut self) {
        let Some(peer) = self.peer.take() else {
            return;
        };
        let id = std::mem::take(&mut self.id);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = peer
                    .cancel_task(rmcp::model::CancelTaskParams::new(id))
                    .await;
            });
        }
    }
}

/// A JSON-RPC error object (`{code, message, data}`) as a [`ProtoError`].
fn rpc_from_json(e: &JsonObject) -> ProtoError {
    ProtoError::Rpc {
        code: e.get("code").and_then(Value::as_i64).unwrap_or(-32603) as i32,
        message: e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("task failed")
            .to_string(),
        data: e.get("data").cloned(),
    }
}

/// The lifecycle used toward backends.
///
/// Prefers `2026-07-28` via `server/discover`, accepts `2025-11-25`, and falls
/// back to legacy `initialize` for servers that reject discovery. Both
/// versions must be in `preferred_versions` (finding from M0-05).
pub(crate) fn backend_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

/// Forward a child's stderr line by line until it closes. The pipe must be
/// drained even when nobody listens, or the child blocks on write.
async fn drain_stderr(err: tokio::process::ChildStderr, command: String, sink: Option<StderrSink>) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(err).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match &sink {
            Some(s) => s(&line),
            None => tracing::debug!(target: "mcpgw::backend", %command, "{line}"),
        }
    }
}

/// Plain legacy `initialize` at 2025-11-25 (retry path, see `connect_stdio`).
fn legacy_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Initialize
}

/// Owns a stdio server's process and kills its whole tree when dropped:
/// `killpg` on the process group (Unix) or `TerminateJobObject` (Windows).
///
/// rmcp's own child transport only waits for the server to exit after
/// closing stdin; a server that exits by itself, or crashes, would leave its
/// children (`npx` → `node`) running. Killing the group or job covers both
/// cases, even after the leader is gone: the group id can't be reused while
/// any member is alive, and the job outlives its first process.
pub(crate) struct TreeGuard {
    pid: Option<u32>,
    /// Background tasks of the connection (the SSE bridge), aborted on drop.
    pub(crate) tasks: Vec<tokio::task::JoinHandle<()>>,
    child: Option<std::sync::Mutex<Box<dyn process_wrap::tokio::ChildWrapper>>>,
    cleanup: Vec<Vec<String>>,
    teardown: Vec<Vec<String>>,
    /// Why the tree was stopped from the inside (the memory watchdog).
    note: Arc<std::sync::Mutex<Option<String>>>,
    #[cfg(windows)]
    limit_job: Option<crate::limits::LimitJob>,
}

impl TreeGuard {
    fn none() -> Self {
        Self {
            pid: None,
            child: None,
            tasks: Vec::new(),
            cleanup: Vec::new(),
            teardown: Vec::new(),
            note: Arc::default(),
            #[cfg(windows)]
            limit_job: None,
        }
    }

    /// Why the server died, if its memory cap stopped it.
    fn death_note(&self) -> Option<String> {
        if let Some(n) = self.note.lock().ok().and_then(|n| n.clone()) {
            return Some(n);
        }
        #[cfg(windows)]
        if let Some(j) = &self.limit_job {
            return j.note();
        }
        None
    }

    /// Kill the tree, reap it, and run the cleanup command, waiting for all.
    async fn finish(&mut self) {
        if let Some(c) = self.child.as_mut().and_then(|c| c.get_mut().ok()) {
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
        }
        let mut all = std::mem::take(&mut self.cleanup);
        all.append(&mut self.teardown);
        run_cleanup(all).await;
    }

    /// Whether the server process has exited (crashed or quit).
    fn exited(&self) -> bool {
        self.child.as_ref().is_some_and(|c| {
            c.lock()
                .map(|mut c| matches!(c.try_wait(), Ok(Some(_)) | Err(_)))
                .unwrap_or(true)
        })
    }

    /// Wait up to `grace` for the server to exit on its own.
    async fn wait_exit(&mut self, grace: Duration) {
        if let Some(c) = self.child.as_mut().and_then(|c| c.get_mut().ok()) {
            let _ = tokio::time::timeout(grace, c.wait()).await;
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        let mut cleanup = std::mem::take(&mut self.cleanup);
        cleanup.append(&mut self.teardown);
        if let Some(mut c) = self.child.take().and_then(|c| c.into_inner().ok()) {
            let _ = c.start_kill();
            // Reap, so no zombie is left behind on Unix.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = c.wait().await;
                    run_cleanup(cleanup).await;
                });
                return;
            }
        }
        if !cleanup.is_empty() {
            // no async runtime left: run them in order on a thread
            std::thread::spawn(move || {
                for argv in cleanup {
                    if let Some((cmd, args)) = argv.split_first() {
                        let _ = std::process::Command::new(cmd)
                            .args(args)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                }
            });
        }
    }
}

/// A live connection to one backend.
pub struct BackendConn {
    service: RunningService<RoleClient, GatewayClient>,
    routes: Arc<crate::progress::Routes>,
    // Declared after `service` so it drops (kills the tree) last.
    pub(crate) _tree: TreeGuard,
}

impl BackendConn {
    /// Spawn a stdio child process and complete the MCP handshake.
    ///
    /// The child's stderr is inherited; stdout carries the protocol. The child
    /// is killed when the connection is dropped.
    ///
    /// The handshake probes `server/discover` and falls back to `initialize`.
    /// rmcp gives up on the probe after a fixed 10 s; a slow legacy server
    /// (a cold `npx` download) then answers the abandoned probe late and the
    /// fallback fails on the uncorrelated reply. So when the automatic
    /// handshake fails, the server is started once more with a plain legacy
    /// `initialize`.
    ///
    /// The server runs in its own process group (Unix) or Job Object
    /// (Windows): when the connection is dropped or shut down the whole
    /// process tree is killed, including grandchildren such as the `node`
    /// process behind `npx`. stderr is piped line by line to `stderr`, or to
    /// the debug log when no sink is given.
    pub async fn connect_stdio(
        spec: &StdioSpec,
        timeout: Duration,
        stderr: Option<StderrSink>,
    ) -> Result<Self, ProtoError> {
        let r = Self::connect_stdio_attempts(spec, timeout, stderr).await;
        if r.is_err() {
            run_cleanup(spec.teardown.clone()).await;
        }
        r
    }

    async fn connect_stdio_attempts(
        spec: &StdioSpec,
        timeout: Duration,
        stderr: Option<StderrSink>,
    ) -> Result<Self, ProtoError> {
        let deadline = tokio::time::Instant::now() + timeout;
        match Self::spawn_with(spec, timeout, backend_lifecycle(), stderr.clone()).await {
            Err(ProtoError::Connect(first)) if !first.starts_with("spawn ") => {
                tracing::debug!(command = %spec.command, error = %first, "auto handshake failed; retrying with initialize");
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                Self::spawn_with(
                    spec,
                    left.max(Duration::from_secs(1)),
                    legacy_lifecycle(),
                    stderr,
                )
                .await
                .map_err(|e| ProtoError::Connect(format!("{first}; legacy retry: {e}")))
            }
            other => other,
        }
    }

    async fn spawn_with(
        spec: &StdioSpec,
        timeout: Duration,
        lifecycle: ClientLifecycleMode,
        stderr: Option<StderrSink>,
    ) -> Result<Self, ProtoError> {
        use process_wrap::tokio::{CommandWrap, KillOnDrop};
        let mut cmd = CommandWrap::with_new(&spec.command, |c| {
            c.args(&spec.args)
                .envs(&spec.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(dir) = &spec.cwd {
                c.current_dir(dir);
            }
            #[cfg(target_os = "linux")]
            if let Some(bytes) = spec.memory_limit {
                // SAFETY: the hook only calls setrlimit (async-signal-safe).
                unsafe {
                    c.pre_exec(move || crate::limits::set_data_limit(bytes));
                }
            }
        });
        #[cfg(unix)]
        cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        cmd.wrap(process_wrap::tokio::JobObject);
        cmd.wrap(KillOnDrop);
        let spawn_err =
            |e: std::io::Error| ProtoError::Connect(format!("spawn {}: {e}", spec.command));
        let mut child = cmd.spawn().map_err(spawn_err)?;
        let io = child.inner_mut();
        let (Some(stdin), Some(stdout)) = (io.stdin().take(), io.stdout().take()) else {
            return Err(ProtoError::Connect(format!(
                "spawn {}: no stdio pipes",
                spec.command
            )));
        };
        if let Some(err) = io.stderr().take() {
            tokio::spawn(drain_stderr(err, spec.command.clone(), stderr));
        }
        let pid = child.id();
        let mut tree = TreeGuard {
            pid,
            child: Some(std::sync::Mutex::new(child)),
            tasks: Vec::new(),
            cleanup: spec.cleanup.clone(),
            teardown: spec.teardown.clone(),
            note: Arc::default(),
            #[cfg(windows)]
            limit_job: None,
        };
        if let (Some(limit), Some(pid)) = (spec.memory_limit, pid) {
            #[cfg(unix)]
            tree.tasks.push(tokio::spawn(crate::limits::watch(
                pid,
                limit,
                tree.note.clone(),
            )));
            #[cfg(windows)]
            match crate::limits::LimitJob::new(pid, limit) {
                Ok(j) => tree.limit_job = Some(j),
                Err(e) => {
                    tree.finish().await;
                    return Err(ProtoError::Connect(format!(
                        "spawn {}: could not apply its memory limit: {e}",
                        spec.command
                    )));
                }
            }
        }
        match Self::connect_transport((stdout, stdin), timeout, lifecycle).await {
            Ok(mut conn) => {
                conn._tree = tree;
                Ok(conn)
            }
            Err(e) => {
                // clean up this attempt fully before a retry starts the
                // server again; the teardown waits for the final outcome
                tree.teardown.clear();
                tree.finish().await;
                Err(e)
            }
        }
    }

    /// Handshake over an arbitrary transport (in-memory pipes in tests).
    pub async fn connect_io<T>(io: T, timeout: Duration) -> Result<Self, ProtoError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        Self::connect_transport(io, timeout, backend_lifecycle()).await
    }

    pub(crate) async fn connect_transport<T, E, A>(
        transport: T,
        timeout: Duration,
        lifecycle: ClientLifecycleMode,
    ) -> Result<Self, ProtoError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client = GatewayClient::default();
        let routes = client.routes.clone();
        let fut = client.serve_with_lifecycle(transport, lifecycle);
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(service)) => Ok(Self {
                service,
                routes,
                _tree: TreeGuard::none(),
            }),
            Ok(Err(e)) => Err(ProtoError::Connect(e.to_string())),
            Err(_) => Err(ProtoError::Timeout(timeout)),
        }
    }

    /// Process id of a stdio server (its process group id on Unix).
    pub fn pid(&self) -> Option<u32> {
        self._tree.pid
    }

    /// Why a stdio server with a memory cap died, if the cap stopped it:
    /// `out of memory: killed at its 128MiB limit (using 131MiB)`.
    pub fn death_note(&self) -> Option<String> {
        self._tree.death_note()
    }

    /// Negotiated protocol version, e.g. `"2025-11-25"`.
    pub fn protocol_version(&self) -> Option<String> {
        self.service
            .peer_info()
            .map(|i| i.protocol_version.to_string())
    }

    /// True once the transport has closed or the server process has exited.
    /// (On Windows a crashed server's pipes can be reported closed only
    /// after a call has already failed, so the process is checked too.)
    pub fn is_closed(&self) -> bool {
        self.service.is_closed() || self.service.is_transport_closed() || self._tree.exited()
    }

    /// All tools (following pagination), each as its JSON object.
    pub async fn list_tools(&self) -> Result<Vec<JsonObject>, ProtoError> {
        let tools = self.service.list_all_tools().await?;
        tools
            .into_iter()
            .map(|t| match serde_json::to_value(t) {
                Ok(Value::Object(o)) => Ok(o),
                Ok(_) => Err(ProtoError::Other("tool is not an object".into())),
                Err(e) => Err(ProtoError::Other(e.to_string())),
            })
            .collect()
    }

    /// Call a tool; returns the `CallToolResult` JSON object.
    ///
    /// Only name and arguments are forwarded (no front-session `_meta`).
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        timeout: Duration,
    ) -> Result<JsonObject, ProtoError> {
        self.call_tool_with_meta(name, arguments, None, timeout)
            .await
    }

    /// [`call_tool`](Self::call_tool) with request `_meta` for the backend,
    /// such as the W3C `traceparent` of the gateway's call (M3-11).
    pub async fn call_tool_with_meta(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        meta: Option<JsonObject>,
        timeout: Duration,
    ) -> Result<JsonObject, ProtoError> {
        self.call_tool_with(
            name,
            arguments,
            meta,
            CallLimits::fixed(timeout),
            crate::progress::ProgressSink::default(),
        )
        .await
    }

    /// Call a tool as a long call (M4-09): the backend's progress goes to
    /// `progress` and keeps the call alive (`limits.idle` counts from the
    /// last sign of life, `limits.max` from the start). Dropping the future
    /// cancels the call at the backend. A backend that answers with a task
    /// (SEP-2663) is polled until the task ends; its status messages become
    /// progress.
    pub async fn call_tool_with(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        meta: Option<JsonObject>,
        limits: CallLimits,
        progress: crate::progress::ProgressSink,
    ) -> Result<JsonObject, ProtoError> {
        let mut params = CallToolRequestParams::new(name.to_string());
        params.arguments = arguments;
        if let Some(m) = meta {
            params.meta = Some(rmcp::model::RequestMetaObject::from(
                rmcp::model::MetaObject::from(m),
            ));
        }
        let req = ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params.clone()));
        let options = PeerRequestOptions::with_timeout(limits.idle)
            .reset_timeout_on_progress()
            .with_max_total_timeout(limits.max);
        let started = tokio::time::Instant::now();
        let handle = self.service.send_request_with_option(req, options).await?;
        let _route = self
            .routes
            .register(handle.progress_token.clone(), progress.clone());
        let mut cancel = CancelOnDrop {
            peer: Some(handle.peer.clone()),
            id: handle.id.clone(),
        };
        let response = handle.await_response().await;
        // answered, or timed out (rmcp cancels those itself)
        cancel.peer = None;
        let result = match response? {
            ServerResult::CallToolResult(r) => serde_json::to_value(r),
            ServerResult::CreateTaskResult(t) => {
                let left = limits.max.saturating_sub(started.elapsed());
                return self.follow_task(t.task, limits.idle, left, &progress).await;
            }
            // MRTR: let rmcp drive the rounds, as before M4-09
            ServerResult::InputRequiredResult(_) => {
                let fut = self.service.call_tool(params);
                match tokio::time::timeout(limits.idle, fut).await {
                    Ok(r) => serde_json::to_value(r?),
                    Err(_) => return Err(ProtoError::Timeout(limits.idle)),
                }
            }
            _ => {
                return Err(ProtoError::Other(
                    "unexpected response to tools/call".into(),
                ));
            }
        };
        match result {
            Ok(Value::Object(o)) => Ok(o),
            Ok(_) => Err(ProtoError::Other("result is not an object".into())),
            Err(e) => Err(ProtoError::Other(e.to_string())),
        }
    }

    /// Poll a backend task until it ends.
    async fn follow_task(
        &self,
        task: rmcp::model::Task,
        idle: Duration,
        max: Duration,
        progress: &crate::progress::ProgressSink,
    ) -> Result<JsonObject, ProtoError> {
        use rmcp::model::{GetTaskParams, TaskPayload};
        let peer = self.service.peer().clone();
        let id = task.task_id.clone();
        let mut cancel = CancelTaskOnDrop {
            peer: Some(peer.clone()),
            id: id.clone(),
        };
        let deadline = tokio::time::Instant::now() + max;
        let mut interval = task.poll_interval_ms;
        let mut last = task.status_message.clone();
        let mut steps = 0.0;
        loop {
            let wait = Duration::from_millis(interval.unwrap_or(1000))
                .clamp(Duration::from_millis(50), Duration::from_secs(5));
            if tokio::time::Instant::now() + wait > deadline {
                return Err(ProtoError::Timeout(max)); // `cancel` stops the task
            }
            tokio::time::sleep(wait).await;
            let got = tokio::time::timeout(idle, peer.get_task(GetTaskParams::new(id.clone())))
                .await
                .map_err(|_| ProtoError::Timeout(idle))??;
            let t = got.task;
            interval = t.task.poll_interval_ms.or(interval);
            if t.task.status_message != last {
                last = t.task.status_message.clone();
                if let Some(m) = &last {
                    steps += 1.0;
                    progress.send(crate::progress::Progress {
                        progress: steps,
                        total: None,
                        message: Some(m.clone()),
                    });
                }
            }
            match t.payload {
                TaskPayload::Completed { result } => {
                    cancel.peer = None;
                    return Ok(result);
                }
                TaskPayload::Failed { error } => {
                    cancel.peer = None;
                    return Err(rpc_from_json(&error));
                }
                TaskPayload::Cancelled => {
                    cancel.peer = None;
                    return Err(ProtoError::Other("the server cancelled the task".into()));
                }
                TaskPayload::InputRequired { .. } => {
                    return Err(ProtoError::Other(
                        "the server's task asks for input, which mcpgw doesn't relay yet".into(),
                    ));
                }
                _ => {}
            }
        }
    }

    /// Liveness probe via MCP `ping`.
    pub async fn ping(&self, timeout: Duration) -> Result<(), ProtoError> {
        if self.is_closed() {
            return Err(ProtoError::Closed);
        }
        let req = ClientRequest::PingRequest(PingRequest::default());
        let handle = self
            .service
            .send_request_with_option(req, PeerRequestOptions::with_timeout(timeout))
            .await?;
        match handle.await_response().await {
            Ok(ServerResult::EmptyResult(_)) => Ok(()),
            // Some servers answer ping with a non-empty object; still alive.
            Ok(_) => Ok(()),
            // An error *reply* is an answer too: 2026-07-28 servers may not
            // implement `ping` (-32601), and that must not get them restarted
            // in the middle of a long call (found by M4-09's 10-minute test).
            Err(ServiceError::McpError(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Close the session: stdin closes, the server gets a moment to exit
    /// on its own, then whatever is left of its process tree is killed.
    pub async fn shutdown(self) {
        let Self {
            service, mut _tree, ..
        } = self;
        let _ = service.cancel().await;
        _tree.wait_exit(Duration::from_secs(2)).await;
        _tree.finish().await;
    }
}

/// Parse a tool JSON object back into a typed tool (used by the front server).
pub(crate) fn tool_from_json(o: JsonObject) -> Result<Tool, ProtoError> {
    serde_json::from_value(Value::Object(o)).map_err(|e| ProtoError::Other(e.to_string()))
}
