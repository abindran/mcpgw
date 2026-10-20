//! `fake-mcp` — a scriptable MCP server for integration tests.
//!
//! Serves over stdio by default, or Streamable HTTP with `--http ADDR`
//! (prints `listening <url>` on stdout once bound). Behaviour is driven by
//! environment variables so a `ServerConfig` can set it:
//!
//! | Variable | Effect |
//! |----------|--------|
//! | `FAKE_PROTOCOL` | `legacy` (≤ 2025-11-25), `modern` (2026-07-28 only) or `both` (default) |
//! | `FAKE_TOOLS` | JSON array replacing the built-in tools: `{name, description?, inputSchema?, reply?}` |
//! | `FAKE_DESC_SUFFIX` | appended to every tool description (drift tests) |
//! | `FAKE_PREFIX` | prefix for `echo` replies |
//! | `FAKE_START_DELAY_MS` | sleep before serving (cold-start tests) |
//! | `FAKE_DELAY_MARKER` | file; with it, the start delay applies only until the file exists (first start only) |
//! | `FAKE_CRASH_AFTER` | exit after this many tool calls |
//! | `FAKE_HANG_PING` | `1`: never answer `ping` (health-check tests) |
//! | `FAKE_STDERR` | line written to stderr at startup |
//! | `FAKE_SPAWN_LOG` | file; the process appends its pid on start |
//! | `FAKE_CALL_LOG` | file; each tool call appends the tool name |
//! | `FAKE_META_LOG` | file; each tool call appends its request `_meta` as a JSON line |
//! | `FAKE_REQUIRE_HEADER` | `--http` only: `Name=value` every request must carry, else 401 |
//! | `FAKE_AUTH_SERVER` | `--http` only: OAuth-protected. Requests need `Authorization: Bearer at-…`, else 401 with `resource_metadata`; serves protected resource metadata naming this issuer |
//!
//! Built-in tools: `add`, `echo`, `crash`, `sleep`, `env`, `spawn`, `stderr`.
//! `alloc` (hold `mib` MiB, `step_ms` apart) works when listed in `FAKE_TOOLS`.
//! `fake-mcp --idle` just sleeps (used as a grandchild by `spawn`);
//! `fake-mcp --exit` exits at once (spawn-cost baseline for timing tests).

use std::{
    borrow::Cow,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Protocol {
    Legacy,
    Modern,
    Both,
}

struct Settings {
    protocol: Protocol,
    /// FAKE_TASKS=1: run `sleep` as a SEP-2663 task for clients that
    /// declare the tasks extension.
    tasks: bool,
    tools: Vec<Value>,
    prefix: String,
    crash_after: Option<u64>,
    hang_ping: bool,
}

#[derive(Clone)]
struct Fake {
    s: Arc<Settings>,
    calls: Arc<AtomicU64>,
    tasks: Arc<rmcp::task_manager::TaskManager>,
}

/// A line in FAKE_CALL_LOG.
fn log_call(line: &str) {
    if let Some(path) = env("FAKE_CALL_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        writeln!(f, "{line}").ok();
    }
}

/// `sleep` with `steps`: `ms` in that many steps, reporting each.
fn steps(args: &Map<String, Value>) -> Option<(u64, u64)> {
    let n = args
        .get("steps")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)?;
    let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(0);
    Some((n, ms / n))
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn builtin_tools() -> Vec<Value> {
    let obj = |props: Value| json!({"type": "object", "properties": props});
    vec![
        json!({"name": "add", "description": "Add two integers",
               "inputSchema": obj(json!({"a": {"type": "integer"}, "b": {"type": "integer"}}))}),
        json!({"name": "echo", "description": "Echo text back",
               "inputSchema": obj(json!({"text": {"type": "string"}}))}),
        json!({"name": "crash", "description": "Exit the process", "inputSchema": obj(json!({}))}),
        json!({"name": "sleep", "description": "Sleep for ms milliseconds",
               "inputSchema": obj(json!({"ms": {"type": "integer"}}))}),
        json!({"name": "env", "description": "Return an environment variable",
               "inputSchema": obj(json!({"name": {"type": "string"}}))}),
        json!({"name": "spawn", "description": "Start an idle grandchild; returns its pid",
               "inputSchema": obj(json!({}))}),
        json!({"name": "stderr", "description": "Write text to stderr",
               "inputSchema": obj(json!({"text": {"type": "string"}}))}),
    ]
}

fn settings() -> Settings {
    let protocol = match env("FAKE_PROTOCOL").as_deref() {
        Some("legacy") => Protocol::Legacy,
        Some("modern") => Protocol::Modern,
        _ => Protocol::Both,
    };
    let mut tools = match env("FAKE_TOOLS") {
        Some(j) => serde_json::from_str::<Vec<Value>>(&j).expect("FAKE_TOOLS must be a JSON array"),
        None => builtin_tools(),
    };
    for t in &mut tools {
        let o = t.as_object_mut().expect("tool must be an object");
        o.entry("inputSchema")
            .or_insert_with(|| json!({"type": "object"}));
        if let Some(sfx) = env("FAKE_DESC_SUFFIX") {
            let d = o
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            o.insert("description".into(), Value::String(format!("{d}{sfx}")));
        }
    }
    Settings {
        protocol,
        tools,
        prefix: env("FAKE_PREFIX").unwrap_or_default(),
        crash_after: env("FAKE_CRASH_AFTER").and_then(|v| v.parse().ok()),
        hang_ping: env("FAKE_HANG_PING").as_deref() == Some("1"),
        tasks: env("FAKE_TASKS").as_deref() == Some("1"),
    }
}

fn text(s: impl Into<String>) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(s.into())]).into()
}

fn arg_str(args: &Map<String, Value>, k: &str) -> String {
    args.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Fake {
    fn tool_def(&self, name: &str) -> Option<&Value> {
        self.s.tools.iter().find(|t| t["name"] == name)
    }

    async fn run(
        &self,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(def) = self.tool_def(name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {name}"),
                None,
            ));
        };
        if let Some(reply) = def.get("reply") {
            return Ok(text(match reply {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }));
        }
        let int = |k: &str| args.get(k).and_then(Value::as_i64).unwrap_or(0);
        Ok(match name {
            "add" => text((int("a") + int("b")).to_string()),
            "echo" => text(format!("{}{}", self.s.prefix, arg_str(&args, "text"))),
            "crash" => std::process::exit(3),
            "sleep" => {
                tokio::time::sleep(Duration::from_millis(int("ms").max(0) as u64)).await;
                text("slept")
            }
            "env" => text(std::env::var(arg_str(&args, "name")).unwrap_or_default()),
            "spawn" => {
                let exe = std::env::current_exe().map_err(internal)?;
                let child = std::process::Command::new(exe)
                    .arg("--idle")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .map_err(internal)?;
                text(child.id().to_string())
            }
            "stderr" => {
                eprintln!("{}", arg_str(&args, "text"));
                text("ok")
            }
            // Not built in (list it in FAKE_TOOLS): hold `mib` more MiB of
            // touched memory, pausing `step_ms` after each.
            "alloc" => {
                static HELD: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());
                for _ in 0..int("mib").max(0) {
                    HELD.lock().unwrap().push(vec![1u8; 1 << 20]);
                    let step = int("step_ms").max(0) as u64;
                    if step > 0 {
                        tokio::time::sleep(Duration::from_millis(step)).await;
                    }
                }
                text(format!("holding {} MiB", HELD.lock().unwrap().len()))
            }
            // Scripted tool without a reply: echo the arguments back.
            _ => text(Value::Object(args).to_string()),
        })
    }
}

fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

impl ServerHandler for Fake {
    fn get_info(&self) -> ServerConfig {
        let caps = ServerCapabilities::builder().enable_tools();
        let caps = if self.s.tasks {
            caps.enable_tasks()
        } else {
            caps
        };
        ServerConfig::new(caps.build())
    }

    async fn get_task(
        &self,
        request: rmcp::model::GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetTaskResult, ErrorData> {
        self.tasks
            .get_task(&request.task_id)
            .map(rmcp::model::GetTaskResult::new)
    }

    async fn cancel_task(
        &self,
        request: rmcp::model::CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks.cancel_task(&request.task_id)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        match self.s.protocol {
            Protocol::Legacy => {
                Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
            }
            Protocol::Modern => Cow::Owned(vec![ProtocolVersion::V_2026_07_28]),
            Protocol::Both => {
                Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2026_07_28))
            }
        }
    }

    async fn ping(&self, _context: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Ok(())
    }

    #[allow(clippy::field_reassign_with_default)] // ListToolsResult is non_exhaustive
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .s
            .tools
            .iter()
            .map(|t| serde_json::from_value::<Tool>(t.clone()).map_err(internal))
            .collect::<Result<Vec<_>, _>>()?;
        let mut r = ListToolsResult::default();
        r.tools = tools;
        Ok(r)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(path) = env("FAKE_META_LOG")
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let mut meta = serde_json::Map::new();
            meta.extend(context.meta.0.0.clone());
            if let Some(m) = &request.meta {
                meta.extend(m.0.0.clone());
            }
            writeln!(f, "{}", Value::Object(meta)).ok();
        }
        log_call(&request.name);
        let args = request.arguments.clone().unwrap_or_default();
        if request.name == "sleep"
            && let Some((n, step)) = steps(&args)
        {
            // M4-09: a long call that reports progress and stops when cancelled
            let tasks_ok = context
                .client_capabilities()
                .is_some_and(|c| c.supports_tasks());
            if self.s.tasks && tasks_ok {
                let task = self.tasks.spawn(
                    rmcp::task_manager::TaskOptions::new().with_poll_interval_ms(100),
                    move |ctx| {
                        Box::pin(async move {
                            for i in 1..=n {
                                tokio::select! {
                                    _ = tokio::time::sleep(Duration::from_millis(step)) => {}
                                    _ = ctx.cancelled() => {
                                        log_call(&format!("sleep cancelled at {i}"));
                                        return Err(rmcp::task_manager::TaskExit::Cancelled);
                                    }
                                }
                                ctx.set_status_message(format!("step {i} of {n}"));
                            }
                            log_call("sleep done");
                            Ok(CallToolResult::success(vec![ContentBlock::text("slept")]))
                        })
                    },
                );
                return Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(
                    task,
                )));
            }
            let token = request
                .meta
                .as_ref()
                .and_then(|m| m.get_progress_token())
                .or_else(|| context.meta.get_progress_token());
            for i in 1..=n {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(step)) => {}
                    _ = context.ct.cancelled() => {
                        log_call(&format!("sleep cancelled at {i}"));
                        return Err(ErrorData::internal_error("cancelled", None));
                    }
                }
                if let Some(t) = &token {
                    let p = rmcp::model::ProgressNotificationParam::new(t.clone(), i as f64)
                        .with_total(n as f64)
                        .with_message(format!("step {i} of {n}"));
                    let _ = context.peer.notify_progress(p).await;
                }
            }
            log_call("sleep done");
            return Ok(text("slept"));
        }
        let out = self
            .run(&request.name, request.arguments.unwrap_or_default())
            .await;
        if self.s.crash_after.is_some_and(|max| n >= max) {
            std::process::exit(4);
        }
        out
    }
}

async fn serve_http(fake: Fake, addr: &str) {
    let service = StreamableHttpService::new(
        move || Ok(fake.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut app = axum::Router::new().nest_service("/mcp", service);
    if let Some(issuer) = env("FAKE_AUTH_SERVER") {
        let metadata = format!("http://{local}/.well-known/oauth-protected-resource/mcp");
        app = app.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let ok = req
                    .headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|h| h.starts_with("Bearer at-"));
                let challenge = format!("Bearer resource_metadata=\"{metadata}\"");
                async move {
                    if ok {
                        next.run(req).await
                    } else {
                        axum::response::IntoResponse::into_response((
                            axum::http::StatusCode::UNAUTHORIZED,
                            [("www-authenticate", challenge)],
                            "unauthorized",
                        ))
                    }
                }
            },
        ));
        let prm = serde_json::json!({
            "resource": format!("http://{local}/mcp"),
            "authorization_servers": [issuer],
            "scopes_supported": ["mcp:tools"],
        });
        app = app.route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(move || {
                let prm = prm.clone();
                async move { axum::Json(prm) }
            }),
        );
    }
    if let Some((name, value)) = env("FAKE_REQUIRE_HEADER").and_then(|v| {
        v.split_once('=')
            .map(|(n, v)| (n.to_ascii_lowercase(), v.to_string()))
    }) {
        app = app.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let ok = req
                    .headers()
                    .get(name.as_str())
                    .and_then(|h| h.to_str().ok())
                    == Some(value.as_str());
                async move {
                    if ok {
                        next.run(req).await
                    } else {
                        axum::response::IntoResponse::into_response((
                            axum::http::StatusCode::UNAUTHORIZED,
                            [("www-authenticate", "Bearer")],
                            "missing or wrong header",
                        ))
                    }
                }
            },
        ));
    }
    println!("listening http://{local}/mcp");
    std::io::stdout().flush().ok();
    axum::serve(listener, app).await.expect("serve");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--exit") {
        return;
    }
    if args.first().map(String::as_str) == Some("--idle") {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }
    if let Some(path) = env("FAKE_SPAWN_LOG") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("spawn log");
        writeln!(f, "{}", std::process::id()).ok();
    }
    if let Some(line) = env("FAKE_STDERR") {
        eprintln!("{line}");
    }
    if let Some(ms) = env("FAKE_START_DELAY_MS").and_then(|v| v.parse().ok()) {
        let marker = env("FAKE_DELAY_MARKER");
        if marker
            .as_ref()
            .is_none_or(|m| !std::path::Path::new(m).exists())
        {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if let Some(m) = marker {
                std::fs::write(m, b"started").ok();
            }
        }
    }
    let fake = Fake {
        s: Arc::new(settings()),
        calls: Arc::new(AtomicU64::new(0)),
        tasks: Arc::new(rmcp::task_manager::TaskManager::new()),
    };
    if let Some(i) = args.iter().position(|a| a == "--http") {
        let addr = args.get(i + 1).map(String::as_str).unwrap_or("127.0.0.1:0");
        serve_http(fake, addr).await;
        return;
    }
    // FAKE_HANG_PING: the server never sees a `ping` (rmcp answers it before
    // any handler would run), so the gateway's health check times out
    let stdin: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>> = if fake.s.hang_ping {
        let (mut tx, rx) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("\"method\":\"ping\"") {
                    continue;
                }
                if tx.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        Box::pin(rx)
    } else {
        Box::pin(tokio::io::stdin())
    };
    let io = (stdin, tokio::io::stdout());
    match fake.serve(io).await {
        Ok(svc) => {
            let _ = svc.waiting().await;
        }
        // A client that only probed `server/discover` and hung up is normal.
        Err(e) if e.to_string().starts_with("connection closed") => {}
        Err(e) => {
            eprintln!("fake-mcp: handshake failed: {e}");
            std::process::exit(1);
        }
    }
}
