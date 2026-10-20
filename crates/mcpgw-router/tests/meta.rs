//! Meta-tools mode with a real MCP client: servers enabled or added while
//! the client is connected become findable at once.

use std::{sync::Arc, time::Duration};

use mcpgw_core::{Mode, ToolDef, ToolManifest};
use mcpgw_router::{GatewayOptions, http_router};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

const ZOO: &str =
    r#"[{"name": "zebra_facts", "description": "Facts about zebras", "reply": "stripes"}]"#;

fn manifest(server: &str, tools: &str) -> ToolManifest {
    let defs = serde_json::from_str::<Vec<Value>>(tools)
        .unwrap()
        .into_iter()
        .map(|mut t| {
            t.as_object_mut().unwrap().remove("reply");
            ToolDef::from_json(t).unwrap()
        })
        .collect();
    ToolManifest::new(server, None, 0, defs)
}

async fn serve(m: Arc<BackendManager>, mode: Mode) -> String {
    let app = axum::Router::new().nest_service(
        "/mcp",
        http_router(
            m,
            GatewayOptions {
                mode,
                pinned: vec!["fx__add".into()],
                ..GatewayOptions::default()
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn search(c: &McpClient, q: &str) -> Vec<String> {
    let r = c.call("search_tools", json!({"query": q})).await.unwrap();
    r["structuredContent"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn enabling_a_server_makes_its_tools_findable_within_a_second() {
    let mut zoo = mcpgw_testkit::stdio("zoo", &[("FAKE_TOOLS", ZOO)]);
    zoo.enabled = false;
    let catalog = Arc::new(Catalog::new(vec![manifest("zoo", ZOO)], None));
    let m = Arc::new(BackendManager::with_catalog(
        &[mcpgw_testkit::stdio("fx", &[]), zoo],
        ManagerOptions::default(),
        catalog,
    ));
    let url = serve(m.clone(), Mode::Meta).await;
    let c = McpClient::http(&url, None).await.unwrap();

    assert_eq!(
        c.tool_names().await.unwrap(),
        ["search_tools", "describe_tool", "call_tool"]
    );
    assert!(search(&c, "zebra").await.is_empty());

    let t0 = std::time::Instant::now();
    m.set_enabled("zoo", true).await.unwrap();
    let mut found = Vec::new();
    while t0.elapsed() < Duration::from_secs(1) {
        found = search(&c, "zebra").await;
        if !found.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(found, ["zoo__zebra_facts"], "not findable within 1 s");
    // same session, no restart: describe and call it
    let d = c
        .call("describe_tool", json!({"name": "zoo__zebra_facts"}))
        .await
        .unwrap();
    assert_eq!(
        d["structuredContent"]["description"],
        "[zoo] Facts about zebras"
    );
    let r = c
        .call(
            "call_tool",
            json!({"name": "zoo__zebra_facts", "arguments": {}}),
        )
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "stripes");
    // the listing itself never changed
    assert_eq!(c.tool_names().await.unwrap().len(), 3);

    // disabling hides it again
    m.set_enabled("zoo", false).await.unwrap();
    assert!(search(&c, "zebra").await.is_empty());
    c.close().await;
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn added_servers_and_meta_errors() {
    let catalog = Arc::new(Catalog::new(vec![], None));
    let m = Arc::new(BackendManager::with_catalog(
        &[mcpgw_testkit::stdio("fx", &[])],
        ManagerOptions::default(),
        catalog.clone(),
    ));
    let url = serve(m.clone(), Mode::Meta).await;
    let c = McpClient::http(&url, None).await.unwrap();

    // a server added at runtime, with its manifest captured at install
    catalog.record(manifest("zoo", ZOO));
    m.add_server(&mcpgw_testkit::stdio("zoo", &[("FAKE_TOOLS", ZOO)]))
        .unwrap();
    assert_eq!(search(&c, "zebra facts").await, ["zoo__zebra_facts"]);

    // problems come back as tool errors the model can read
    let r = c
        .call("describe_tool", json!({"name": "nope__x"}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("search_tools")
    );
    let r = c
        .call("call_tool", json!({"name": "nope__x"}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true);
    let r = c
        .call("call_tool", json!({"name": "call_tool"}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true);
    // direct calls by public name still work in meta mode
    let r = c.call("zoo__zebra_facts", json!({})).await.unwrap();
    assert_eq!(r["content"][0]["text"], "stripes");
    c.close().await;
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_lists_meta_tools_plus_pinned() {
    let spec = mcpgw_testkit::stdio("fx", &[]);
    let pinned = mcpgw_runtime::introspect(&spec, Duration::from_secs(20))
        .await
        .unwrap();
    let m = Arc::new(BackendManager::with_catalog(
        &[spec],
        ManagerOptions::default(),
        Arc::new(Catalog::new(vec![pinned], None)),
    ));
    let url = serve(m.clone(), Mode::Hybrid).await;
    let c = McpClient::http(&url, None).await.unwrap();
    assert_eq!(
        c.tool_names().await.unwrap(),
        ["search_tools", "describe_tool", "call_tool", "fx__add"]
    );
    assert_eq!(
        c.call_text("fx__add", json!({"a": 2, "b": 3}))
            .await
            .unwrap(),
        "5"
    );
    c.close().await;
    m.shutdown().await;
}
