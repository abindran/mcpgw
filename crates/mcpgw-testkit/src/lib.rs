//! `mcpgw-testkit` — fake MCP servers for integration tests.
//!
//! [`fake_mcp`] locates the `fake-mcp` binary (building it on first use),
//! [`stdio`] makes a `ServerSpec` that launches it, and [`HttpFake`] runs
//! it as a Streamable HTTP server. See `src/bin/fake-mcp.rs` for the knobs.
//! [`McpClient`] is a real MCP client (rmcp) for driving the gateway itself,
//! the way Claude Code or Cursor would.

pub mod oauth;
pub mod sse;

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::OnceLock,
};

use mcpgw_core::{ServerSpec, Transport};

/// Path to the `fake-mcp` binary, built once per test process.
///
/// Tests live in `target/<profile>/deps/`, so the binary sits one level up.
pub fn fake_mcp() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().expect("current exe");
        let dir = exe
            .parent()
            .and_then(|d| {
                if d.ends_with("deps") {
                    d.parent()
                } else {
                    Some(d)
                }
            })
            .expect("target dir")
            .to_path_buf();
        let bin = dir.join(format!("fake-mcp{}", std::env::consts::EXE_SUFFIX));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args([
            "build",
            "--quiet",
            "--package",
            "mcpgw-testkit",
            "--bin",
            "fake-mcp",
        ]);
        if dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        // Cross builds put artifacts in target/<triple>/<profile>.
        let triple = dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| n.matches('-').count() >= 2);
        if let Some(triple) = triple {
            cmd.args(["--target", triple]);
        }
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        assert!(ok || bin.exists(), "could not build fake-mcp");
        bin
    })
    .clone()
}

/// A stdio `ServerSpec` named `name` that launches `fake-mcp` with `env`.
pub fn stdio(name: &str, env: &[(&str, &str)]) -> ServerSpec {
    ServerSpec::new(
        name,
        Transport::Stdio {
            command: fake_mcp().to_string_lossy().into_owned(),
            args: vec![],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            cwd: None,
        },
    )
}

/// A `fake-mcp` serving Streamable HTTP on a random loopback port.
/// The process is killed on drop.
pub struct HttpFake {
    child: Child,
    /// Endpoint URL, e.g. `http://127.0.0.1:53211/mcp`.
    pub url: String,
}

impl HttpFake {
    /// Start one with the given environment.
    pub fn start(env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(fake_mcp())
            .args(["--http", "127.0.0.1:0"])
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fake-mcp");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("read listening line");
        let url = line
            .trim()
            .strip_prefix("listening ")
            .unwrap_or_else(|| panic!("unexpected fake-mcp output: {line:?}"))
            .to_string();
        Self { child, url }
    }

    /// Process id of the server.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for HttpFake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An MCP client for tests: rmcp with the same lifecycle real clients use
/// (2026-07-28 preferred, 2025-11-25 accepted).
pub struct McpClient {
    svc: rmcp::service::RunningService<rmcp::RoleClient, TestHandler>,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// The test client's handler: its declared capabilities, and how it answers
/// form elicitations (as a user clicking "approve" or "deny").
#[derive(Clone)]
pub struct TestHandler {
    cfg: rmcp::model::ClientConfig,
    answer: Option<bool>,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    list_changed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl rmcp::ClientHandler for TestHandler {
    fn get_info(&self) -> rmcp::model::ClientConfig {
        self.cfg.clone()
    }

    async fn on_tool_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        self.list_changed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
        let message = match &request {
            ElicitRequestParams::FormElicitationParams { message, .. }
            | ElicitRequestParams::UrlElicitationParams { message, .. } => message.clone(),
            _ => String::new(),
        };
        self.asked.lock().unwrap().push(message);
        Ok(match self.answer {
            Some(yes) => ElicitResult::new(ElicitationAction::Accept)
                .with_content(serde_json::json!({"approve": yes})),
            None => ElicitResult::new(ElicitationAction::Decline),
        })
    }
}

/// What a test client declares and does.
#[derive(Clone, Copy, Default)]
pub struct ClientKind {
    /// Declare URL-mode elicitation.
    pub url_elicitation: bool,
    /// Declare form elicitation and answer every question with this.
    pub approve: Option<bool>,
}

fn handler(kind: ClientKind, asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> TestHandler {
    use rmcp::model::{
        ClientCapabilities, ElicitationCapability, FormElicitationCapability,
        UrlElicitationCapability,
    };
    let mut caps = ClientCapabilities::default();
    let mut el = ElicitationCapability::new();
    if kind.url_elicitation {
        el = el.with_url(UrlElicitationCapability::new());
    }
    if kind.approve.is_some() {
        el = el.with_form(FormElicitationCapability::new());
    }
    if kind.url_elicitation || kind.approve.is_some() {
        caps.elicitation = Some(el);
    }
    let mut cfg = rmcp::model::ClientConfig::default();
    cfg.capabilities = caps;
    cfg.client_info = rmcp::model::Implementation::new("mcpgw-testkit", env!("CARGO_PKG_VERSION"));
    TestHandler {
        cfg,
        answer: kind.approve,
        asked,
        list_changed: Default::default(),
    }
}

fn lifecycle() -> rmcp::ClientLifecycleMode {
    use rmcp::model::ProtocolVersion;
    rmcp::ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

impl McpClient {
    /// Connect over Streamable HTTP, sending `Authorization: Bearer <token>`.
    pub async fn http(url: &str, token: Option<&str>) -> Result<Self, String> {
        Self::http_with(url, token, lifecycle(), ClientKind::default()).await
    }

    /// Like [`McpClient::http`], as a client that supports URL elicitation.
    pub async fn http_url_elicitation(url: &str, token: Option<&str>) -> Result<Self, String> {
        let kind = ClientKind {
            url_elicitation: true,
            ..Default::default()
        };
        Self::http_with(url, token, lifecycle(), kind).await
    }

    /// A Streamable HTTP client of the given kind; `legacy` speaks
    /// 2025-11-25 (`initialize`, sessions), otherwise 2026-07-28.
    pub async fn http_kind(
        url: &str,
        token: Option<&str>,
        kind: ClientKind,
        legacy: bool,
    ) -> Result<Self, String> {
        let lc = if legacy {
            rmcp::ClientLifecycleMode::Initialize
        } else {
            lifecycle()
        };
        Self::http_with(url, token, lc, kind).await
    }

    /// `notifications/tools/list_changed` received on the session (legacy
    /// clients; 2026 clients use [`McpClient::listen_tools`]).
    pub fn list_changed_count(&self) -> usize {
        self.svc
            .service()
            .list_changed
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Open a 2026-07-28 `subscriptions/listen` for tool list changes.
    pub async fn listen_tools(&self) -> Result<ToolChanges, String> {
        let filter = rmcp::model::SubscriptionFilter::builder()
            .tools_list_changed()
            .build();
        let sub = self
            .svc
            .peer()
            .listen(filter)
            .await
            .map_err(|e| e.to_string())?;
        Ok(ToolChanges(sub))
    }

    /// The raw `tools/list` result (with `ttlMs`).
    pub async fn list_raw(&self) -> Result<serde_json::Value, String> {
        let r = self.svc.list_tools(None).await.map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(r).expect("list json"))
    }

    /// The server's capabilities as JSON.
    pub fn server_capabilities(&self) -> serde_json::Value {
        self.svc
            .peer_info()
            .map(|i| serde_json::to_value(&i.capabilities).unwrap_or_default())
            .unwrap_or_default()
    }

    /// Messages of the elicitations this client was shown.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// Like [`McpClient::http`] but as a 2025-11-25 client (`initialize`,
    /// HTTP sessions).
    pub async fn http_legacy(url: &str, token: Option<&str>) -> Result<Self, String> {
        Self::http_with(
            url,
            token,
            rmcp::ClientLifecycleMode::Initialize,
            ClientKind::default(),
        )
        .await
    }

    async fn http_with(
        url: &str,
        token: Option<&str>,
        lifecycle: rmcp::ClientLifecycleMode,
        kind: ClientKind,
    ) -> Result<Self, String> {
        use rmcp::{
            ClientServiceExt,
            transport::{
                StreamableHttpClientTransport,
                streamable_http_client::StreamableHttpClientTransportConfig,
            },
        };
        mcpgw_core::tls::install();
        let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        if let Some(t) = token {
            cfg = cfg.auth_header(t.to_string());
        }
        let t = StreamableHttpClientTransport::from_config(cfg);
        let asked = std::sync::Arc::default();
        let svc = handler(kind, std::sync::Arc::clone(&asked))
            .serve_with_lifecycle(t, lifecycle)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { svc, asked })
    }

    /// Spawn `cmd` and speak MCP over its stdio (e.g. `mcpgw connect`).
    pub async fn stdio(cmd: tokio::process::Command) -> Result<Self, String> {
        Self::stdio_with(cmd, lifecycle()).await
    }

    /// Like [`McpClient::stdio`] but as a 2025-11-25 client.
    pub async fn stdio_legacy(cmd: tokio::process::Command) -> Result<Self, String> {
        Self::stdio_with(cmd, rmcp::ClientLifecycleMode::Initialize).await
    }

    async fn stdio_with(
        cmd: tokio::process::Command,
        lifecycle: rmcp::ClientLifecycleMode,
    ) -> Result<Self, String> {
        use rmcp::{ClientServiceExt, transport::TokioChildProcess};
        let t = TokioChildProcess::new(cmd).map_err(|e| e.to_string())?;
        let asked = std::sync::Arc::default();
        let svc = handler(ClientKind::default(), std::sync::Arc::clone(&asked))
            .serve_with_lifecycle(t, lifecycle)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { svc, asked })
    }

    /// Negotiated protocol version.
    pub fn protocol_version(&self) -> String {
        self.svc
            .peer_info()
            .map(|i| i.protocol_version.to_string())
            .unwrap_or_default()
    }

    /// Names of all tools.
    pub async fn tool_names(&self) -> Result<Vec<String>, String> {
        let tools = self.svc.list_all_tools().await.map_err(|e| e.to_string())?;
        Ok(tools.iter().map(|t| t.name.to_string()).collect())
    }

    /// All tools as JSON.
    pub async fn tools(&self) -> Result<Vec<serde_json::Value>, String> {
        let tools = self.svc.list_all_tools().await.map_err(|e| e.to_string())?;
        Ok(tools
            .into_iter()
            .map(|t| serde_json::to_value(t).expect("tool json"))
            .collect())
    }

    /// Call a tool; `Ok` is the `CallToolResult` JSON, `Err` a protocol error.
    pub async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        let r = self.svc.call_tool(p).await.map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(r).expect("result json"))
    }

    /// Call a tool; `Err` is the JSON-RPC error object (`code`, `message`,
    /// `data`) when the server returned one.
    pub async fn call_raw(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        match self.svc.call_tool(p).await {
            Ok(r) => Ok(serde_json::to_value(r).expect("result json")),
            Err(rmcp::ServiceError::McpError(e)) => {
                Err(serde_json::to_value(e).expect("error json"))
            }
            Err(e) => Err(serde_json::json!({"message": e.to_string()})),
        }
    }

    /// The first text content of a tool call, for brevity in assertions.
    pub async fn call_text(&self, name: &str, args: serde_json::Value) -> Result<String, String> {
        let v = self.call(name, args).await?;
        Ok(v["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Hang up.
    pub async fn close(self) {
        let _ = self.svc.cancel().await;
    }
}

/// A `subscriptions/listen` stream of tool list changes.
pub struct ToolChanges(rmcp::service::Subscription);

impl ToolChanges {
    /// Wait for the next `tools/list_changed`; `false` if the subscription
    /// ended.
    pub async fn next(&mut self) -> bool {
        loop {
            match self.0.next().await {
                Ok(Some(rmcp::model::ServerNotification::ToolListChangedNotification(_))) => {
                    return true;
                }
                Ok(Some(_)) => continue,
                _ => return false,
            }
        }
    }
}
