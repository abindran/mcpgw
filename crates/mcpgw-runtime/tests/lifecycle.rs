//! Lifecycle tests against real child-process backends (`fake-mcp`).

use std::time::Duration;

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, BackendState, ManagerOptions, ProtoError};

use mcpgw_testkit::stdio as server;

fn fast() -> ManagerOptions {
    ManagerOptions {
        connect_timeout: Duration::from_secs(20),
        call_timeout: Duration::from_secs(10),
        health_interval: Duration::from_millis(200),
        ping_timeout: Duration::from_millis(300),
        backoff_initial: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
    }
}

fn args(v: serde_json::Value) -> Option<mcpgw_runtime::JsonObject> {
    v.as_object().cloned()
}

async fn wait_for<F: Fn(&BackendState) -> bool>(m: &BackendManager, name: &str, f: F) -> bool {
    for _ in 0..300 {
        let st = m.status().await;
        if st.iter().any(|s| s.name == name && f(&s.state)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn text_of(r: &mcpgw_runtime::JsonObject) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn spawns_negotiates_legacy_and_calls() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_PROTOCOL", "legacy")])], fast());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let st = m.status().await;
    assert_eq!(
        st[0].state,
        BackendState::Running {
            protocol_version: "2025-11-25".into()
        }
    );
    let names: Vec<_> = m
        .tools("fx")
        .await
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        ["add", "echo", "crash", "sleep", "env", "spawn", "stderr"]
    );
    let r = m
        .call_tool("fx", "add", args(serde_json::json!({"a": 2, "b": 40})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), "42");
    m.shutdown().await;
    assert_eq!(m.status().await[0].state, BackendState::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn passes_env_and_rpc_errors() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_PREFIX", ">> ")])], fast());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let r = m
        .call_tool("fx", "echo", args(serde_json::json!({"text": "hi"})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), ">> hi");
    let err = m.call_tool("fx", "nope", None).await.unwrap_err();
    assert!(
        matches!(err, ProtoError::Rpc { code: -32602, .. }),
        "{err:?}"
    );
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_after_crash() {
    let m = BackendManager::start(&[server("fx", &[])], fast());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let err = m.call_tool("fx", "crash", None).await.unwrap_err();
    assert!(
        matches!(err, ProtoError::Closed | ProtoError::Other(_)),
        "{err:?}"
    );
    let mut ok = false;
    for _ in 0..300 {
        let s = &m.status().await[0];
        if s.restarts >= 1 && matches!(s.state, BackendState::Running { .. }) {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "backend did not restart: {:?}", m.status().await);
    let r = m
        .call_tool("fx", "add", args(serde_json::json!({"a": 1, "b": 1})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), "2");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_health_check_triggers_restart() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_HANG_PING", "1")])], fast());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let mut restarted = false;
    for _ in 0..300 {
        if m.status().await[0].restarts >= 1 {
            restarted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(restarted, "{:?}", m.status().await);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_command_backs_off_and_unsupported_is_reported() {
    let bad = ServerSpec::new("bad", Transport::stdio("/nonexistent/mcp-server", vec![]));
    let web = ServerSpec::new("web", Transport::http("http://127.0.0.1:1/mcp"));
    let mut off = server("off", &[]);
    off.enabled = false;
    let m = BackendManager::start(&[bad, web, off], fast());
    assert!(m.wait_ready(Duration::from_secs(10)).await);
    assert!(
        wait_for(
            &m,
            "bad",
            |s| matches!(s, BackendState::Restarting { attempt, .. } if *attempt >= 2)
        )
        .await
    );
    let st = m.status().await;
    assert!(matches!(st[1].state, BackendState::Unsupported(_))); // off
    assert!(matches!(st[2].state, BackendState::Unsupported(_))); // web
    assert!(matches!(
        m.call_tool("bad", "x", None).await,
        Err(ProtoError::Unavailable(_))
    ));
    assert!(matches!(
        m.call_tool("missing", "x", None).await,
        Err(ProtoError::Unavailable(_))
    ));
    m.shutdown().await;
}

/// A legacy server that takes longer than rmcp's 10 s discover probe on its
/// first start (a cold `npx` download) still connects: the late reply to the
/// probe is not fatal because the gateway retries with plain `initialize`.
#[tokio::test(flavor = "multi_thread")]
async fn slow_first_start_of_legacy_server_connects() {
    let marker = std::env::temp_dir().join(format!("mcpgw-slow-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let marker_s = marker.to_string_lossy().into_owned();
    let opts = ManagerOptions {
        connect_timeout: Duration::from_secs(40),
        ..fast()
    };
    let m = BackendManager::start(
        &[server(
            "slow",
            &[
                ("FAKE_PROTOCOL", "legacy"),
                ("FAKE_START_DELAY_MS", "10500"),
                ("FAKE_DELAY_MARKER", &marker_s),
            ],
        )],
        opts,
    );
    // Must go straight from Starting to Running: a pass through Restarting
    // means the first handshake failed and the supervisor had to retry.
    let mut seen = Vec::new();
    for _ in 0..800 {
        let st = m.status().await[0].state.clone();
        if seen.last() != Some(&st) {
            seen.push(st.clone());
        }
        if matches!(st, BackendState::Running { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        matches!(
            seen.as_slice(),
            [BackendState::Starting, BackendState::Running { .. }]
        ),
        "{seen:?}"
    );
    m.shutdown().await;
    let _ = std::fs::remove_file(&marker);
}
