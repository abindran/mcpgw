//! Lifecycle tests against real child-process backends (`fake-mcp`).

use std::time::Duration;

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, ManagerOptions, ProtoError, State};
use mcpgw_testkit::stdio as server;
use serde_json::json;

fn fast() -> ManagerOptions {
    ManagerOptions {
        connect_timeout: Duration::from_secs(20),
        call_timeout: Duration::from_secs(10),
        health_interval: Duration::from_millis(200),
        ping_timeout: Duration::from_millis(300),
        backoff_initial: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
        max_failures: 1000,
        eager: true,
    }
}

/// Health checks slow enough that a loaded machine never restarts a
/// server mid-test.
fn patient() -> ManagerOptions {
    ManagerOptions {
        health_interval: Duration::from_secs(60),
        ping_timeout: Duration::from_secs(30),
        ..fast()
    }
}

fn lazy() -> ManagerOptions {
    ManagerOptions {
        eager: false,
        ..patient()
    }
}

fn args(v: serde_json::Value) -> Option<mcpgw_runtime::JsonObject> {
    v.as_object().cloned()
}

fn state(m: &BackendManager, name: &str) -> State {
    m.status_of(name).unwrap().state
}

async fn wait_for<F: Fn(&State) -> bool>(m: &BackendManager, name: &str, f: F) -> bool {
    for _ in 0..400 {
        if f(&state(m, name)) {
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

fn temp_file(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("mcpgw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn spawns_negotiates_legacy_and_calls() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_PROTOCOL", "legacy")])], patient());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let st = m.status_of("fx").unwrap();
    assert_eq!(st.state, State::Ready);
    assert_eq!(st.protocol_version.as_deref(), Some("2025-11-25"));
    assert!(st.pid.is_some());
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
        .call_tool("fx", "add", args(json!({"a": 2, "b": 40})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), "42");
    assert_eq!(state(&m, "fx"), State::Idle);
    assert!(m.status_of("fx").unwrap().last_used.is_some());
    m.shutdown().await;
    assert_eq!(state(&m, "fx"), State::Cold);
}

#[tokio::test(flavor = "multi_thread")]
async fn lazy_server_starts_on_first_call() {
    let m = BackendManager::start(&[server("fx", &[])], lazy());
    assert!(m.wait_ready(Duration::from_secs(1)).await);
    let st = m.status_of("fx").unwrap();
    assert_eq!((st.state, st.pid), (State::Cold, None));
    let r = m
        .call_tool("fx", "add", args(json!({"a": 1, "b": 1})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), "2");
    let st = m.status_of("fx").unwrap();
    assert_eq!(st.state, State::Idle);
    assert!(st.pid.is_some());
    m.shutdown().await;
}

/// Ten calls arrive at a cold server at once; it is spawned exactly once
/// and every call is served.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_calls_spawn_once() {
    let log = temp_file("spawnlog");
    let log_s = log.to_string_lossy().into_owned();
    let m = std::sync::Arc::new(BackendManager::start(
        &[server(
            "fx",
            &[("FAKE_SPAWN_LOG", &log_s), ("FAKE_START_DELAY_MS", "300")],
        )],
        lazy(),
    ));
    let calls: Vec<_> = (0..10)
        .map(|i| {
            let m = m.clone();
            tokio::spawn(async move {
                m.call_tool("fx", "add", args(json!({"a": i, "b": 1})))
                    .await
            })
        })
        .collect();
    for (i, c) in calls.into_iter().enumerate() {
        let r = c.await.unwrap().unwrap();
        assert_eq!(text_of(&r), (i + 1).to_string());
    }
    let spawns = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        spawns.lines().count(),
        1,
        "spawned more than once: {spawns:?}"
    );
    m.shutdown().await;
    let _ = std::fs::remove_file(&log);
}

/// Stop waits for in-flight calls, then the server is cold and starts
/// again on the next call.
#[tokio::test(flavor = "multi_thread")]
async fn stop_drains_in_flight_calls() {
    let m = std::sync::Arc::new(BackendManager::start(&[server("fx", &[])], lazy()));
    m.ensure("fx").await.unwrap();
    let slow = {
        let m = m.clone();
        tokio::spawn(async move { m.call_tool("fx", "sleep", args(json!({"ms": 400}))).await })
    };
    assert!(wait_for(&m, "fx", |s| matches!(s, State::Busy { inflight: 1 })).await);
    let stopper = {
        let m = m.clone();
        tokio::spawn(async move { m.stop("fx").await })
    };
    assert!(wait_for(&m, "fx", |s| matches!(s, State::Draining { inflight: 1 })).await);
    assert_eq!(text_of(&slow.await.unwrap().unwrap()), "slept");
    stopper.await.unwrap().unwrap();
    assert_eq!(state(&m, "fx"), State::Cold);
    assert_eq!(m.status_of("fx").unwrap().pid, None);
    let r = m
        .call_tool("fx", "add", args(json!({"a": 3, "b": 4})))
        .await
        .unwrap();
    assert_eq!(text_of(&r), "7");
    assert_eq!(m.status_of("fx").unwrap().restarts, 1);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn passes_env_and_rpc_errors() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_PREFIX", ">> ")])], patient());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let r = m
        .call_tool("fx", "echo", args(json!({"text": "hi"})))
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
        let s = m.status_of("fx").unwrap();
        if s.restarts >= 1 && s.state.is_connected() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ok, "backend did not restart: {:?}", m.status().await);
    let r = m
        .call_tool("fx", "add", args(json!({"a": 1, "b": 1})))
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
        if m.status_of("fx").unwrap().restarts >= 1 {
            restarted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(restarted, "{:?}", m.status().await);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_start_failures_end_in_failed_until_reset() {
    let bad = ServerSpec::new("bad", Transport::stdio("/nonexistent/mcp-server", vec![]));
    let opts = ManagerOptions {
        max_failures: 3,
        ..fast()
    };
    let m = BackendManager::start(&[bad], opts);
    assert!(
        wait_for(&m, "bad", |s| *s == State::Failed).await,
        "{:?}",
        m.status().await
    );
    let st = m.status_of("bad").unwrap();
    assert!(st.last_error.unwrap().contains("spawn /nonexistent"));
    let err = m.call_tool("bad", "x", None).await.unwrap_err();
    assert!(
        matches!(&err, ProtoError::Unavailable(m) if m.contains("failed to start 3 times")),
        "{err:?}"
    );
    m.reset("bad").unwrap();
    assert!(wait_for(&m, "bad", |s| *s != State::Failed).await);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lazy_start_failure_is_reported_to_the_caller() {
    let bad = ServerSpec::new("bad", Transport::stdio("/nonexistent/mcp-server", vec![]));
    let m = BackendManager::start(&[bad], lazy());
    let err = m.call_tool("bad", "x", None).await.unwrap_err();
    assert!(
        matches!(&err, ProtoError::Connect(m) if m.contains("spawn")),
        "{err:?}"
    );
    assert!(matches!(state(&m, "bad"), State::Backoff { attempt: 1 }));
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_and_unsupported_servers() {
    let web = ServerSpec::new("web", Transport::http("http://127.0.0.1:1/mcp"));
    let mut off = server("off", &[]);
    off.enabled = false;
    let m = BackendManager::start(&[web, off], fast());
    assert!(m.wait_ready(Duration::from_secs(5)).await);
    for name in ["off", "web"] {
        assert_eq!(state(&m, name), State::Disabled, "{name}");
        assert!(matches!(
            m.call_tool(name, "x", None).await,
            Err(ProtoError::Unavailable(_))
        ));
    }
    assert!(
        m.status_of("web")
            .unwrap()
            .last_error
            .unwrap()
            .contains("http")
    );
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
    let marker = temp_file("slow");
    let marker_s = marker.to_string_lossy().into_owned();
    let opts = ManagerOptions {
        connect_timeout: Duration::from_secs(40),
        ..patient()
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
    // Must go straight from Starting to Ready: a pass through Backoff means
    // the first handshake failed and the supervisor had to retry.
    let mut seen = Vec::new();
    for _ in 0..800 {
        let st = state(&m, "slow");
        if seen.last() != Some(&st) {
            seen.push(st.clone());
        }
        if st == State::Ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen: Vec<_> = seen.into_iter().filter(|s| *s != State::Cold).collect();
    assert_eq!(seen, [State::Starting, State::Ready]);
    m.shutdown().await;
    let _ = std::fs::remove_file(&marker);
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
}

async fn gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Stopping a server removes its whole process tree, like the `node`
/// process an `npx` launcher leaves behind.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_server_kills_its_process_tree() {
    let m = BackendManager::start(&[server("fx", &[])], patient());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let pid: u32 = text_of(&m.call_tool("fx", "spawn", None).await.unwrap())
        .parse()
        .unwrap();
    assert!(alive(pid), "grandchild {pid} should be running");
    m.shutdown().await;
    assert!(gone(pid).await, "grandchild {pid} survived its server");
}

/// A crashed server's leftovers go too: the restart cleans up the old tree.
#[tokio::test(flavor = "multi_thread")]
async fn restart_after_crash_cleans_up_grandchildren() {
    let m = BackendManager::start(&[server("fx", &[])], patient());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let pid: u32 = text_of(&m.call_tool("fx", "spawn", None).await.unwrap())
        .parse()
        .unwrap();
    let _ = m.call_tool("fx", "crash", None).await;
    assert!(
        gone(pid).await,
        "grandchild {pid} survived the crash restart"
    );
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn captures_stderr_lines() {
    let m = BackendManager::start(&[server("fx", &[("FAKE_STDERR", "booting fx")])], patient());
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    m.call_tool("fx", "stderr", args(json!({"text": "warn: disk"})))
        .await
        .unwrap();
    let mut tail = Vec::new();
    for _ in 0..100 {
        tail = m.stderr_tail("fx");
        if tail.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(tail, ["booting fx", "warn: disk"]);
    assert!(m.stderr_tail("nope").is_empty());
    m.shutdown().await;
}
