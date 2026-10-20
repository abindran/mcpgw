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
    transport::TokioChildProcess,
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
}

#[derive(Clone, Default)]
struct GatewayClient;
impl ClientHandler for GatewayClient {}

/// The lifecycle used toward backends.
///
/// Prefers `2026-07-28` via `server/discover`, accepts `2025-11-25`, and falls
/// back to legacy `initialize` for servers that reject discovery. Both
/// versions must be in `preferred_versions` (finding from M0-05).
fn backend_lifecycle() -> ClientLifecycleMode {
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

/// Kills a stdio server's whole process group when dropped (Unix).
///
/// rmcp's graceful shutdown closes stdin and waits for the server itself to
/// exit; when it does, nothing signals the rest of the group, so children
/// (`npx` → `node`) would outlive it. The group id is the server's pid
/// (`ProcessGroup::leader`). The group can't be recycled while any member
/// is alive, so a group that is already gone just gives `ESRCH`. On Windows
/// the Job Object does the same when its handle closes.
struct TreeGuard {
    pid: Option<u32>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl TreeGuard {
    fn new(pid: Option<u32>) -> Self {
        Self {
            pid,
            #[cfg(unix)]
            pgid: pid.and_then(|p| i32::try_from(p).ok()),
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

/// A live connection to one backend.
pub struct BackendConn {
    service: RunningService<RoleClient, GatewayClient>,
    // Declared after `service` so it drops (kills the tree) last.
    _tree: TreeGuard,
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
            c.args(&spec.args).envs(&spec.env);
            if let Some(dir) = &spec.cwd {
                c.current_dir(dir);
            }
        });
        #[cfg(unix)]
        cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        cmd.wrap(process_wrap::tokio::JobObject);
        cmd.wrap(KillOnDrop);
        let (child, err) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ProtoError::Connect(format!("spawn {}: {e}", spec.command)))?;
        if let Some(err) = err {
            tokio::spawn(drain_stderr(err, spec.command.clone(), stderr));
        }
        let tree = TreeGuard::new(child.id());
        let mut conn = Self::connect_transport(child, timeout, lifecycle).await?;
        conn._tree = tree;
        Ok(conn)
    }

    /// Handshake over an arbitrary transport (in-memory pipes in tests).
    pub async fn connect_io<T>(io: T, timeout: Duration) -> Result<Self, ProtoError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        Self::connect_transport(io, timeout, backend_lifecycle()).await
    }

    async fn connect_transport<T, E, A>(
        transport: T,
        timeout: Duration,
        lifecycle: ClientLifecycleMode,
    ) -> Result<Self, ProtoError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let fut = GatewayClient.serve_with_lifecycle(transport, lifecycle);
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(service)) => Ok(Self {
                service,
                _tree: TreeGuard::new(None),
            }),
            Ok(Err(e)) => Err(ProtoError::Connect(e.to_string())),
            Err(_) => Err(ProtoError::Timeout(timeout)),
        }
    }

    /// Process id of a stdio server (its process group id on Unix).
    pub fn pid(&self) -> Option<u32> {
        self._tree.pid
    }

    /// Negotiated protocol version, e.g. `"2025-11-25"`.
    pub fn protocol_version(&self) -> Option<String> {
        self.service
            .peer_info()
            .map(|i| i.protocol_version.to_string())
    }

    /// True once the underlying transport has closed.
    pub fn is_closed(&self) -> bool {
        self.service.is_closed() || self.service.is_transport_closed()
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
        let mut params = CallToolRequestParams::new(name.to_string());
        params.arguments = arguments;
        let fut = self.service.call_tool(params);
        let result = match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r?,
            Err(_) => return Err(ProtoError::Timeout(timeout)),
        };
        match serde_json::to_value(result) {
            Ok(Value::Object(o)) => Ok(o),
            Ok(_) => Err(ProtoError::Other("result is not an object".into())),
            Err(e) => Err(ProtoError::Other(e.to_string())),
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
        match handle.await_response().await? {
            ServerResult::EmptyResult(_) => Ok(()),
            // Some servers answer ping with a non-empty object; still alive.
            _ => Ok(()),
        }
    }

    /// Close the session; the child process is terminated.
    pub async fn shutdown(self) {
        let _ = self.service.cancel().await;
    }
}

/// Parse a tool JSON object back into a typed tool (used by the front server).
pub(crate) fn tool_from_json(o: JsonObject) -> Result<Tool, ProtoError> {
    serde_json::from_value(Value::Object(o)).map_err(|e| ProtoError::Other(e.to_string()))
}
