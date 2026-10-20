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
        ["echo", "reverse", "spin"]
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
    assert_eq!(m.tools("wecho").await.len(), 3);
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
