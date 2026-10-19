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
//! | `FAKE_CRASH_AFTER` | exit after this many tool calls |
//! | `FAKE_HANG_PING` | `1`: never answer `ping` (health-check tests) |
//! | `FAKE_STDERR` | line written to stderr at startup |
//! | `FAKE_SPAWN_LOG` | file; the process appends its pid on start |
//!
//! Built-in tools: `add`, `echo`, `crash`, `sleep`, `env`, `spawn`, `stderr`.
//! `fake-mcp --idle` just sleeps (used as a grandchild by `spawn`).

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
    tools: Vec<Value>,
    prefix: String,
    crash_after: Option<u64>,
    hang_ping: bool,
}

#[derive(Clone)]
struct Fake {
    s: Arc<Settings>,
    calls: Arc<AtomicU64>,
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
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
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
        if self.s.hang_ping {
            std::future::pending::<()>().await;
        }
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
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
    let app = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    let local = listener.local_addr().expect("local addr");
    println!("listening http://{local}/mcp");
    std::io::stdout().flush().ok();
    axum::serve(listener, app).await.expect("serve");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    let fake = Fake {
        s: Arc::new(settings()),
        calls: Arc::new(AtomicU64::new(0)),
    };
    if let Some(i) = args.iter().position(|a| a == "--http") {
        let addr = args.get(i + 1).map(String::as_str).unwrap_or("127.0.0.1:0");
        serve_http(fake, addr).await;
        return;
    }
    let io = (tokio::io::stdin(), tokio::io::stdout());
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
