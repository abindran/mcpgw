//! M4-03: an infinite-loop tool is stopped within its time budget, a tool
//! over its memory cap is stopped, and in both cases the next call gets a
//! fresh instance. Calls stay cancellable: a guest yields at every tick.

use std::time::{Duration, Instant};

use mcpgw_wasm::{Grants, Limits};
use serde_json::{Map, Value, json};

fn echo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo-tool.wasm")
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn text(r: &Map<String, Value>) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn infinite_loop_is_stopped_within_its_budget() {
    let tools = mcpgw_wasm::load(&echo()).unwrap();
    let budget = Duration::from_millis(300);
    let r = tools
        .instantiate_limited(
            &Grants::default(),
            Limits {
                time: budget,
                ..Limits::default()
            },
        )
        .await
        .unwrap();
    let t = Instant::now();
    let out = r.call_tool("spin", &Map::new()).await.unwrap();
    let took = t.elapsed();
    println!("spin stopped after {took:?} (budget {budget:?})");
    assert_eq!(out["isError"], true, "{out:?}");
    assert!(
        text(&out).contains("ran past its 300ms time budget"),
        "{out:?}"
    );
    assert!(
        took >= budget && took < budget + Duration::from_millis(500),
        "{took:?}"
    );
    // a fresh instance answers the next call
    assert_eq!(
        text(
            &r.call_tool("echo", &args(json!({"text": "alive"})))
                .await
                .unwrap()
        ),
        "alive"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_cap_stops_the_tool() {
    let tools = mcpgw_wasm::load(&echo()).unwrap();
    let r = tools
        .instantiate_limited(
            &Grants::default(),
            Limits {
                memory: 32 << 20,
                ..Limits::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        text(
            &r.call_tool("alloc", &args(json!({"mib": 8})))
                .await
                .unwrap()
        ),
        "held 8 MiB"
    );
    let out = r
        .call_tool("alloc", &args(json!({"mib": 64})))
        .await
        .unwrap();
    assert_eq!(out["isError"], true, "{out:?}");
    assert!(
        text(&out).contains("out of memory at its 32MiB limit"),
        "{out:?}"
    );
    assert_eq!(
        text(
            &r.call_tool("echo", &args(json!({"text": "after"})))
                .await
                .unwrap()
        ),
        "after"
    );
    let too_big = tools
        .instantiate_limited(
            &Grants::default(),
            Limits {
                memory: 1 << 30,
                ..Limits::default()
            },
        )
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(too_big.contains("isn't supported"), "{too_big}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spinning_call_can_be_cancelled() {
    // the guest yields at every epoch tick, so dropping the call's future
    // (a caller's timeout) stops it long before its own budget
    let tools = mcpgw_wasm::load(&echo()).unwrap();
    let r = tools
        .instantiate_limited(
            &Grants::default(),
            Limits {
                time: Duration::from_secs(60),
                ..Limits::default()
            },
        )
        .await
        .unwrap();
    let t = Instant::now();
    let res =
        tokio::time::timeout(Duration::from_millis(200), r.call_tool("spin", &Map::new())).await;
    assert!(res.is_err(), "spin returned");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}
