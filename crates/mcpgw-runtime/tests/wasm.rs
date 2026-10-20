//! M4-01: a Wasm tool server runs under the manager like any other: listed
//! from its pin, started on the first call (instantiated from the pool and
//! served over an in-memory MCP session), stopped when idle, and fast to
//! start again.

use std::{sync::Arc, time::Duration};

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, State};
use serde_json::json;

fn echo_tool() -> ServerSpec {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mcpgw-wasm/tests/fixtures/echo-tool.wasm");
    ServerSpec::new("wecho", Transport::wasm(path))
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
async fn wasm_server_lifecycle() {
    let spec = echo_tool();
    let pinned = mcpgw_runtime::introspect(&spec, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        pinned
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["alloc", "echo", "reverse", "spin"]
    );
    let m = BackendManager::with_catalog(
        &[spec],
        ManagerOptions {
            idle_timeout: Duration::from_millis(500),
            health_interval: Duration::from_secs(60),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::new(vec![pinned], None)),
    );
    assert_eq!(m.status_of("wecho").unwrap().state, State::Cold);
    assert_eq!(m.tools("wecho").await.len(), 4);
    let r = m
        .call_tool(
            "wecho",
            "echo",
            json!({"text": "from wasm"}).as_object().cloned(),
        )
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "from wasm");
    let st = m.status_of("wecho").unwrap();
    assert!(st.state.is_connected() && st.pid.is_none(), "{st:?}");
    println!(
        "wasm server start (instantiate + MCP handshake + tools/list): {:?} ms",
        st.last_start_ms
    );
    assert!(st.last_start_ms.unwrap() < 200, "{st:?}");

    // idle stop, then a fresh instance
    assert!(until(|| m.status_of("wecho").unwrap().state == State::Cold, 10).await);
    let r = m
        .call_tool(
            "wecho",
            "reverse",
            json!({"text": "wasm"}).as_object().cloned(),
        )
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "msaw");
    // a guest error is a tool error, not a crash
    let e = m.call_tool("wecho", "nope", None).await.unwrap_err();
    assert!(e.to_string().contains("unknown tool"), "{e}");
    assert!(m.status_of("wecho").unwrap().state.is_connected());
    m.shutdown().await;
}

/// M4-03 through the manager: `time_budget` and `limits.memory` apply; a
/// stopped call is a tool error and the server keeps serving.
#[tokio::test(flavor = "multi_thread")]
async fn wasm_limits_under_the_manager() {
    let mut spec = echo_tool();
    if let Transport::Wasm { time_budget, .. } = &mut spec.transport {
        *time_budget = Some(mcpgw_core::Duration::from_millis(300));
    }
    spec.limits.memory = Some(mcpgw_core::ByteSize::mib(32));
    spec.validate().unwrap();
    let m = BackendManager::with_catalog(
        &[spec],
        ManagerOptions {
            health_interval: Duration::from_secs(60),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );
    let t = std::time::Instant::now();
    let r = m.call_tool("wecho", "spin", None).await.unwrap();
    assert_eq!(r["isError"], true, "{r:?}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("300ms time budget"),
        "{r:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    let r = m
        .call_tool("wecho", "alloc", json!({"mib": 64}).as_object().cloned())
        .await
        .unwrap();
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("32MiB limit"),
        "{r:?}"
    );
    let r = m
        .call_tool(
            "wecho",
            "echo",
            json!({"text": "still serving"}).as_object().cloned(),
        )
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "still serving");
    assert!(m.status_of("wecho").unwrap().state.is_connected());
    m.shutdown().await;
}
