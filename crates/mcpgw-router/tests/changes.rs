//! M2-10: in direct mode, clients that support notifications see new tools
//! without reconnecting: `tools/list_changed` on 2025-11-25 sessions, and
//! through `subscriptions/listen` on 2026-07-28.

use std::{sync::Arc, time::Duration};

use mcpgw_core::{Mode, ToolDef, ToolManifest};
use mcpgw_router::{GatewayOptions, http_router};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use mcpgw_testkit::{ClientKind, McpClient};

fn manifest(server: &str) -> ToolManifest {
    let t = ToolDef::from_json(serde_json::json!({"name": "add"})).unwrap();
    ToolManifest::new(server, None, 0, vec![t])
}

async fn endpoint(m: Arc<BackendManager>, mode: Mode) -> String {
    let opts = GatewayOptions {
        mode,
        ..GatewayOptions::default()
    };
    let app = axum::Router::new().nest_service("/mcp", http_router(m, opts));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..40 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn clients_see_new_tools_without_reconnecting() {
    let catalog = Arc::new(Catalog::new(vec![manifest("fx")], None));
    let m = Arc::new(BackendManager::with_catalog(
        &[mcpgw_testkit::stdio("fx", &[])],
        ManagerOptions::default(),
        catalog.clone(),
    ));
    let url = endpoint(m.clone(), Mode::Direct).await;
    let legacy = McpClient::http_kind(&url, None, ClientKind::default(), true)
        .await
        .unwrap();
    let modern = McpClient::http(&url, None).await.unwrap();
    assert_eq!(legacy.protocol_version(), "2025-11-25");
    assert_eq!(modern.protocol_version(), "2026-07-28");
    assert_eq!(legacy.server_capabilities()["tools"]["listChanged"], true);
    assert_eq!(modern.list_raw().await.unwrap()["ttlMs"], 5000);
    let mut changes = modern.listen_tools().await.unwrap();
    assert_eq!(modern.tool_names().await.unwrap(), ["fx__add"]);

    // a server is added at runtime (with its pinned tools)
    catalog.record(manifest("extra"));
    m.add_server(&mcpgw_testkit::stdio("extra", &[])).unwrap();

    // the 2025 session is notified...
    assert!(
        eventually(|| legacy.list_changed_count() >= 1).await,
        "no list_changed on the session"
    );
    assert!(
        legacy
            .tool_names()
            .await
            .unwrap()
            .contains(&"extra__add".to_string())
    );
    // ...and so is the 2026 subscription, on the same connection
    assert!(
        tokio::time::timeout(Duration::from_secs(5), changes.next())
            .await
            .unwrap()
    );
    assert!(
        modern
            .tool_names()
            .await
            .unwrap()
            .contains(&"extra__add".to_string())
    );

    // disabling is a change too
    let before = legacy.list_changed_count();
    m.set_enabled("extra", false).await.unwrap();
    assert!(eventually(|| legacy.list_changed_count() > before).await);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), changes.next())
            .await
            .unwrap()
    );
    assert_eq!(modern.tool_names().await.unwrap(), ["fx__add"]);

    legacy.close().await;
    modern.close().await;
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_mode_needs_no_notifications() {
    let m = Arc::new(BackendManager::with_catalog(
        &[mcpgw_testkit::stdio("fx", &[])],
        ManagerOptions::default(),
        Arc::new(Catalog::new(vec![manifest("fx")], None)),
    ));
    let url = endpoint(m.clone(), Mode::Meta).await;
    let c = McpClient::http_kind(&url, None, ClientKind::default(), true)
        .await
        .unwrap();
    assert!(
        c.server_capabilities()["tools"]
            .get("listChanged")
            .is_none()
    );
    assert_eq!(c.list_raw().await.unwrap()["ttlMs"], 3_600_000);
    c.close().await;
    m.shutdown().await;
}
