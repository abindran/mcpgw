//! M3-11 done-when: a real Jaeger shows one trace from the client's call to
//! the backend. Jaeger (all-in-one, OTLP on 4318) runs in Docker; the
//! daemon exports to it over OTLP/HTTP protobuf; the trace is read back
//! through Jaeger's query API.
//!
//! Skipped without Docker for Linux containers, unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{path::Path, process::Command, time::Duration};

use mcpgw_testkit::{
    McpClient,
    container::{containers_or_skip, docker},
};
use serde_json::{Value, json};

const JAEGER: &str = "jaegertracing/jaeger:2.11.0";

struct Jaeger(String);

impl Drop for Jaeger {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

fn mcpgw(dir: &Path, otlp: &str, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--config")
        .arg(dir.join("config.toml"))
        .args(args)
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", otlp)
        .env("OTEL_SERVICE_NAME", "mcpgw-jaeger-e2e")
        .env("OTEL_BSP_SCHEDULE_DELAY", "200")
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

#[tokio::test(flavor = "multi_thread")]
async fn jaeger_shows_the_trace_from_client_to_backend() {
    if !containers_or_skip("jaeger_shows_the_trace_from_client_to_backend") {
        return;
    }
    mcpgw_core::tls::install();
    let name = format!("mcpgw-jaeger-{}", std::process::id());
    docker(&[
        "run",
        "-d",
        "--rm",
        "--name",
        &name,
        "-p",
        "127.0.0.1::4318",
        "-p",
        "127.0.0.1::16686",
        JAEGER,
    ]);
    let _jaeger = Jaeger(name.clone());
    let port = |p: &str| {
        docker(&["port", &name, p])
            .lines()
            .next()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .to_string()
    };
    let (otlp, query) = (
        format!("http://127.0.0.1:{}", port("4318")),
        format!("http://127.0.0.1:{}", port("16686")),
    );
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut up = false;
    for _ in 0..100 {
        if http
            .get(format!("{query}/api/services"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(up, "jaeger did not come up");

    let dir = std::env::temp_dir().join(format!("mcpgw-jaeger-{}", std::process::id()));
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
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    mcpgw(&dir, &otlp, &["add", "fx", "--command", &fake]);
    mcpgw(&dir, &otlp, &["up"]);
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = mcpgw(&dir, &otlp, &["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    let trace = "5ad16b3ae96a4f6e9b4c2e1d0f1a2b3c";
    c.call_meta(
        "fx__echo",
        json!({"text": "to jaeger"}),
        json!({"traceparent": format!("00-{trace}-1122334455667788-01")}),
    )
    .await
    .unwrap();
    mcpgw(&dir, &otlp, &["down"]);

    // Jaeger has the trace: the gateway's call, child of the client's
    // span, and the backend call under it
    let mut spans: Vec<Value> = Vec::new();
    for _ in 0..100 {
        if let Ok(r) = http.get(format!("{query}/api/traces/{trace}")).send().await
            && let Ok(v) = r.json::<Value>().await
        {
            spans = v["data"][0]["spans"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if spans.len() >= 2 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let op = |s: &Value| s["operationName"].as_str().unwrap_or_default().to_string();
    println!(
        "jaeger trace {trace}: {:?}",
        spans.iter().map(op).collect::<Vec<_>>()
    );
    let call = spans
        .iter()
        .find(|s| op(s) == "tools/call fx__echo")
        .expect("tools/call span in Jaeger");
    let backend = spans
        .iter()
        .find(|s| op(s) == "backend tools/call fx")
        .expect("backend span in Jaeger");
    let parent = |s: &Value| {
        s["references"][0]["spanID"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(parent(call), "1122334455667788", "{call:#}");
    assert_eq!(
        parent(backend),
        call["spanID"].as_str().unwrap(),
        "{backend:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
