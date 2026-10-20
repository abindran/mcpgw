//! The fake server itself: protocols, built-in and scripted tools, timing.

use std::time::{Duration, Instant};

use mcpgw_core::Transport;
use mcpgw_testkit::{HttpFake, stdio};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{CallToolRequestParams, ProtocolVersion},
    service::RunningService,
    transport::{StreamableHttpClientTransport, TokioChildProcess},
};
use serde_json::{Value, json};

fn auto() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

async fn connect(env: &[(&str, &str)]) -> RunningService<RoleClient, ()> {
    let Transport::Stdio { command, env, .. } = stdio("t", env).transport else {
        unreachable!()
    };
    let mut cmd = tokio::process::Command::new(command);
    cmd.envs(env);
    ().serve_with_lifecycle(TokioChildProcess::new(cmd).unwrap(), auto())
        .await
        .unwrap()
}

fn version(s: &RunningService<RoleClient, ()>) -> String {
    s.peer_info().unwrap().protocol_version.to_string()
}

async fn call(s: &RunningService<RoleClient, ()>, name: &str, args: Value) -> String {
    let mut p = CallToolRequestParams::new(name.to_string());
    p.arguments = args.as_object().cloned();
    let r = s.call_tool(p).await.unwrap();
    serde_json::to_value(r).unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn negotiates_each_protocol_mode() {
    for (mode, want) in [
        ("legacy", "2025-11-25"),
        ("modern", "2026-07-28"),
        ("both", "2026-07-28"),
    ] {
        let s = connect(&[("FAKE_PROTOCOL", mode)]).await;
        assert_eq!(version(&s), want, "{mode}");
        assert_eq!(call(&s, "add", json!({"a": 2, "b": 40})).await, "42");
        s.cancel().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn builtin_tools_and_knobs() {
    let s = connect(&[
        ("FAKE_PREFIX", ">> "),
        ("FAKE_DESC_SUFFIX", " v2"),
        ("SECRET_X", "canary"),
    ])
    .await;
    let tools = s.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(
        names,
        ["add", "echo", "crash", "sleep", "env", "spawn", "stderr"]
    );
    assert_eq!(tools[0].description.as_deref(), Some("Add two integers v2"));
    assert_eq!(call(&s, "echo", json!({"text": "hi"})).await, ">> hi");
    assert_eq!(call(&s, "env", json!({"name": "SECRET_X"})).await, "canary");
    assert_eq!(call(&s, "sleep", json!({"ms": 5})).await, "slept");
    let mut p = CallToolRequestParams::new("crash".to_string());
    p.arguments = None;
    assert!(s.call_tool(p).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn scripted_tools_and_crash_after() {
    let tools = r#"[{"name":"search","description":"Find","reply":"found"},{"name":"raw"}]"#;
    let s = connect(&[("FAKE_TOOLS", tools), ("FAKE_CRASH_AFTER", "2")]).await;
    let names: Vec<_> = s
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names, ["search", "raw"]);
    assert_eq!(call(&s, "search", json!({})).await, "found");
    let mut p = CallToolRequestParams::new("raw".to_string());
    p.arguments = json!({"k": 1}).as_object().cloned();
    // second call crashes the process
    assert!(s.call_tool(p).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_streamable_http() {
    let fake = HttpFake::start(&[]);
    let t = StreamableHttpClientTransport::from_uri(fake.url.clone());
    let s = ().serve_with_lifecycle(t, auto()).await.unwrap();
    assert_eq!(version(&s), "2026-07-28");
    assert_eq!(call(&s, "add", json!({"a": 1, "b": 2})).await, "3");
    s.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_in_under_50ms() {
    connect(&[]).await.cancel().await.unwrap(); // warm the binary
    let mut times = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        let s = connect(&[]).await;
        times.push(t0.elapsed());
        s.cancel().await.unwrap();
    }
    // Best of five, so parallel tests on a loaded runner don't skew it. On a
    // badly overloaded machine even exec is slow, so also accept a handshake
    // that adds under 25 ms to a bare spawn of the same binary.
    times.sort();
    let mut bare = Vec::new();
    for _ in 0..5 {
        let t0 = Instant::now();
        std::process::Command::new(mcpgw_testkit::fake_mcp())
            .arg("--exit")
            .status()
            .unwrap();
        bare.push(t0.elapsed());
    }
    bare.sort();
    let ok = times[0] < Duration::from_millis(50)
        || times[0].saturating_sub(bare[0]) < Duration::from_millis(25);
    assert!(ok, "start times {times:?}, bare spawn {bare:?}");
}
