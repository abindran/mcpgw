//! M3-05: a stdio server's memory cap holds on every OS (RLIMIT_DATA on
//! Linux, a Job Object on Windows, the watchdog on macOS and Linux). Going
//! over it stops that server, which is reported; the gateway and other
//! servers carry on, and the server comes back on demand.

use std::{sync::Arc, time::Duration};

use mcpgw_core::{ByteSize, Limits};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use serde_json::json;

const TOOLS: &str = r#"[{"name": "alloc"}, {"name": "echo"}]"#;

async fn call(
    m: &BackendManager,
    server: &str,
    tool: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    m.call_tool(server, tool, args.as_object().cloned())
        .await
        .map(|r| {
            r["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .map_err(|e| e.to_string())
}

async fn until(mut f: impl FnMut() -> bool, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_memory_cap_stops_the_server_not_the_gateway() {
    let mut hog = mcpgw_testkit::stdio("hog", &[("FAKE_TOOLS", TOOLS)]);
    hog.limits = Limits {
        memory: Some(ByteSize::mib(128)),
        ..Default::default()
    };
    hog.validate().unwrap();
    let calm = mcpgw_testkit::stdio("calm", &[("FAKE_TOOLS", TOOLS)]);
    let m = BackendManager::with_catalog(
        &[hog, calm],
        ManagerOptions {
            connect_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(60),
            health_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(120),
            backoff_initial: Duration::from_millis(200),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );

    // under the cap all is well; without a cap 200 MiB is fine
    assert_eq!(
        call(&m, "hog", "alloc", json!({"mib": 16})).await.unwrap(),
        "holding 16 MiB"
    );
    assert_eq!(
        call(&m, "calm", "alloc", json!({"mib": 200}))
            .await
            .unwrap(),
        "holding 200 MiB"
    );

    // over it: the call fails and the server is reported out of memory
    // (slowly enough for the 250 ms RSS watchdog to look, even under load)
    let r = call(&m, "hog", "alloc", json!({"mib": 400, "step_ms": 20})).await;
    assert!(r.is_err(), "allocated past its cap: {r:?}");
    let reported = || {
        m.status_of("hog")
            .unwrap()
            .last_error
            .is_some_and(|e| e.contains("out of memory") && e.contains("128MiB"))
    };
    assert!(
        until(reported, 20).await,
        "{:?}",
        m.status_of("hog").unwrap()
    );
    assert!(
        m.stderr_tail("hog")
            .iter()
            .any(|l| l.contains("mcpgw: out of memory")),
        "{:?}",
        m.stderr_tail("hog")
    );

    // the gateway carries on
    assert_eq!(
        call(&m, "calm", "echo", json!({"text": "fine"}))
            .await
            .unwrap(),
        "fine"
    );
    let mut back = Err(String::new());
    for _ in 0..50 {
        back = call(&m, "hog", "echo", json!({"text": "back"})).await;
        if back.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(back.unwrap(), "back");
    m.shutdown().await;
}

#[test]
fn limits_by_transport() {
    let mut s = mcpgw_testkit::stdio("s", &[]);
    s.limits.memory = Some(ByteSize::mib(64));
    s.validate().unwrap();
    s.limits.cpus = Some(1.0);
    assert!(
        s.validate()
            .unwrap_err()
            .to_string()
            .contains("memory limit only")
    );
    let mut h = mcpgw_core::ServerSpec::new("h", mcpgw_core::Transport::http("https://x/mcp"));
    h.limits.memory = Some(ByteSize::mib(64));
    assert!(
        h.validate()
            .unwrap_err()
            .to_string()
            .contains("not remote ones")
    );
}
