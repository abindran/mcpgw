//! M4-01: a Wasm tool (the `echo-tool` component built from `wasm/` by
//! `cargo xtask wasm`) instantiates in under 10 ms, lists its tools and
//! answers calls.

use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo-tool.wasm")
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn instantiates_in_under_10ms_and_answers() {
    let compile = Instant::now();
    let tools = mcpgw_wasm::load(&fixture()).unwrap();
    println!("compile and link: {:?}", compile.elapsed());

    // instantiation, from the pre-linked component through the pool
    let mut times = Vec::new();
    let mut keep = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        let r = tools.instantiate().await.unwrap();
        times.push(t.elapsed());
        keep.push(r);
    }
    times.sort();
    let median = times[times.len() / 2];
    println!(
        "instantiate: median {median:?}, max {:?} (50 instances held at once)",
        times[times.len() - 1]
    );
    assert!(
        median < Duration::from_millis(10),
        "median instantiation {median:?}"
    );

    let r = &keep[0];
    let listed = r.list_tools().await.unwrap();
    let names: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["echo", "reverse", "spin"]);
    assert_eq!(
        listed[0]["inputSchema"]["properties"]["text"]["type"],
        "string"
    );
    let out = r
        .call_tool("echo", &args(json!({"text": "hello wasm"})))
        .await
        .unwrap();
    assert_eq!(out["content"][0]["text"], "hello wasm");
    let out = keep[1]
        .call_tool("reverse", &args(json!({"text": "abc"})))
        .await
        .unwrap();
    assert_eq!(out["content"][0]["text"], "cba");
    let e = r.call_tool("nope", &Map::new()).await.unwrap_err();
    assert!(e.contains("unknown tool"), "{e}");

    // loading again is cached
    assert!(std::sync::Arc::ptr_eq(
        &tools,
        &mcpgw_wasm::load(&fixture()).unwrap()
    ));
}

#[test]
fn rejects_files_that_are_not_tool_servers() {
    let dir = std::env::temp_dir().join(format!("mcpgw-wasm-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.wasm");
    std::fs::write(&junk, b"not wasm").unwrap();
    let e = mcpgw_wasm::load(&junk).map(|_| ()).unwrap_err();
    assert!(e.contains("not a valid component"), "{e}");
    assert!(mcpgw_wasm::load(&dir.join("missing.wasm")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
