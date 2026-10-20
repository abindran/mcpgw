//! M3-06: with `warm_pool = { min = 1 }` a server is started with the
//! gateway, so the first call has no cold start; it isn't stopped for
//! idleness or evicted, and comes back by itself after a crash. Cold
//! starts are counted and timed.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use mcpgw_core::WarmPool;
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, State};
use serde_json::json;

const DELAY_MS: u64 = 1500;

fn slow(name: &str, warm: bool) -> mcpgw_core::ServerSpec {
    let mut s = mcpgw_testkit::stdio(name, &[("FAKE_START_DELAY_MS", &DELAY_MS.to_string())]);
    s.warm_pool = warm.then_some(WarmPool { min: 1 });
    s.validate().unwrap();
    s
}

async fn timed_echo(m: &BackendManager, server: &str) -> Duration {
    let t = Instant::now();
    let r = m
        .call_tool(server, "echo", json!({"text": "x"}).as_object().cloned())
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "x");
    t.elapsed()
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
async fn warm_server_has_no_cold_start() {
    let m = BackendManager::with_catalog(
        &[slow("warm", true), slow("cold", false)],
        ManagerOptions {
            connect_timeout: Duration::from_secs(30),
            health_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_millis(500),
            backoff_initial: Duration::from_millis(100),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );
    // warm starts by itself; cold doesn't
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let w = m.status_of("warm").unwrap();
    assert!(w.warm && w.state.is_connected(), "{w:?}");
    assert!(w.last_start_ms.unwrap() >= DELAY_MS, "{w:?}");
    assert_eq!(m.status_of("cold").unwrap().state, State::Cold);

    // done-when: the warm server's first call doesn't wait for a start
    let warm_first = timed_echo(&m, "warm").await;
    assert!(
        warm_first < Duration::from_millis(DELAY_MS / 2),
        "warm first call took {warm_first:?}"
    );
    assert_eq!(m.status_of("warm").unwrap().cold_starts, 0);
    let cold_first = timed_echo(&m, "cold").await;
    assert!(
        cold_first >= Duration::from_millis(DELAY_MS),
        "cold first call took {cold_first:?}"
    );
    let c = m.status_of("cold").unwrap();
    assert_eq!(c.cold_starts, 1);
    assert!(c.last_start_ms.unwrap() >= DELAY_MS);

    // idle: the cold server stops, the warm one stays up
    assert!(until(|| m.status_of("cold").unwrap().state == State::Cold, 10).await);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(m.status_of("warm").unwrap().state.is_connected());

    // a crash: the warm server comes back without being asked
    let pid = m.status_of("warm").unwrap().pid;
    let _ = m.call_tool("warm", "crash", None).await;
    let back = || {
        let s = m.status_of("warm").unwrap();
        s.state.is_connected() && s.pid != pid && s.restarts >= 1
    };
    assert!(until(back, 20).await, "{:?}", m.status_of("warm").unwrap());
    assert!(timed_echo(&m, "warm").await < Duration::from_millis(DELAY_MS / 2));
    m.shutdown().await;
}

#[test]
fn only_one_warm_instance() {
    let mut s = slow("s", false);
    s.warm_pool = Some(WarmPool { min: 2 });
    assert!(s.validate().unwrap_err().to_string().contains("use 0 or 1"));
    let t: mcpgw_core::ServerSpec = toml::from_str(
        "name = \"t\"\ntransport = \"stdio\"\ncommand = \"x\"\nwarm_pool = { min = 1 }\n",
    )
    .unwrap();
    assert!(t.warm());
}

/// M3-07: prewarming is speculative, so it never pushes a server out.
#[tokio::test(flavor = "multi_thread")]
async fn prewarm_only_with_room() {
    let m = BackendManager::with_catalog(
        &[
            mcpgw_testkit::stdio("a", &[]),
            mcpgw_testkit::stdio("b", &[]),
        ],
        ManagerOptions {
            max_running: 1,
            health_interval: Duration::from_secs(60),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );
    timed_echo(&m, "a").await;
    assert!(!m.prewarm("b", None), "no room: a is running");
    assert_eq!(m.status_of("b").unwrap().state, State::Cold);
    m.stop("a").await.unwrap();
    assert!(m.prewarm("b", None));
    assert!(until(|| m.status_of("b").unwrap().state.is_connected(), 10).await);
    assert!(!m.prewarm("b", None), "already running");
    assert_eq!(m.status_of("b").unwrap().prewarms, 1);
    assert!(!m.prewarm("nope", None));
    m.shutdown().await;
}
