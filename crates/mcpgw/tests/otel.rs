//! M3-11 end to end: with an OTLP endpoint the daemon exports one trace
//! from the client's call to the backend: the client's `traceparent`
//! (`_meta`) is the parent of the gateway's `tools/call` span, whose
//! `backend` span's trace context reaches the backend in `_meta`. Metrics
//! and logs are exported too.

use std::{path::PathBuf, process::Command, time::Duration};

use mcpgw_testkit::{McpClient, otlp::FakeOtlp};
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
    otlp: String,
}

impl Env {
    fn run(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", &self.otlp)
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json")
            .env("OTEL_SERVICE_NAME", "mcpgw-e2e")
            .env("OTEL_BSP_SCHEDULE_DELAY", "200")
            .env("OTEL_METRIC_EXPORT_INTERVAL", "500")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "{args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_trace_from_client_call_to_backend() {
    let otlp = FakeOtlp::start().await;
    let dir = std::env::temp_dir().join(format!("mcpgw-otel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n",
            dir.join("db.sqlite"),
            dir.join("token")
        ),
    )
    .unwrap();
    let e = Env {
        dir: dir.clone(),
        otlp: otlp.base.clone(),
    };
    let meta_log = dir.join("meta.jsonl");
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    e.run(&[
        "add",
        "fx",
        "--command",
        &fake,
        "--env",
        &format!("FAKE_META_LOG={}", meta_log.display()),
    ]);
    e.run(&["up"]);
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = e.run(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();

    // the client's own trace
    let (trace, client_span) = ("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7");
    let r = c
        .call_meta(
            "fx__echo",
            json!({"text": "traced"}),
            json!({"traceparent": format!("00-{trace}-{client_span}-01")}),
        )
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "traced");
    // stopping the daemon flushes its exporters (just after `down` returns)
    e.run(&["down"]);
    for _ in 0..100 {
        let names = otlp.metric_names();
        if names.iter().any(|n| n == "mcpgw_tool_calls") && otlp.log_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let spans = otlp.spans();
    let find = |name: &str| {
        spans
            .iter()
            .find(|s| s["name"] == name && s["traceId"] == trace)
            .unwrap_or_else(|| panic!("no span {name:?} in trace {trace}: {spans:#?}"))
            .clone()
    };
    let call = find("tools/call fx__echo");
    assert_eq!(
        call["parentSpanId"], client_span,
        "child of the client's span"
    );
    assert_eq!(call["kind"], 2, "SERVER");
    let backend = find("backend tools/call fx");
    assert_eq!(backend["parentSpanId"], call["spanId"]);
    assert_eq!(backend["kind"], 3, "CLIENT");

    // the backend got the backend span's trace context
    let metas = std::fs::read_to_string(&meta_log).unwrap();
    let sent = metas
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|m| m["traceparent"].as_str().map(str::to_string));
    let sent = sent.unwrap_or_else(|| panic!("no traceparent reached the backend: {metas}"));
    assert_eq!(
        sent,
        format!("00-{trace}-{}-01", backend["spanId"].as_str().unwrap())
    );

    // metrics and logs
    let metrics = otlp.metric_names();
    for m in [
        "mcpgw_tool_calls",
        "mcpgw_tool_call_duration_ms",
        "mcpgw_server_start_ms",
    ] {
        assert!(metrics.iter().any(|n| n == m), "{m} missing: {metrics:?}");
    }
    assert!(otlp.log_count() > 0, "no log records");
}
