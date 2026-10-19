use mcpgw_proto::spike_bridge::{BackendClient, Bridge, LegacyBackend};
use rmcp::{
    model::{CallToolRequestParams, ProtocolVersion},
    ClientHandler, ClientLifecycleMode, ClientServiceExt, ServiceExt,
};

#[derive(Clone, Default)]
struct FrontClient;
impl ClientHandler for FrontClient {}

fn add_args(a: i64, b: i64) -> CallToolRequestParams {
    CallToolRequestParams::new("add").with_arguments(
        serde_json::json!({ "a": a, "b": b })
            .as_object()
            .unwrap()
            .clone(),
    )
}

fn spawn_backend() -> tokio::io::DuplexStream {
    let (s, c) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let svc = LegacyBackend::new().serve(s).await.unwrap();
        let _ = svc.waiting().await;
    });
    c
}

#[tokio::test]
async fn client_negotiates_legacy_version_with_backend() {
    let backend = BackendClient.serve(spawn_backend()).await.unwrap();
    let info = backend.peer_info().expect("peer info");
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
    let tools = backend.list_all_tools().await.unwrap();
    assert_eq!(tools[0].name, "add");
    let r = backend.call_tool(add_args(2, 3)).await.unwrap();
    assert!(format!("{:?}", r).contains('5'));
    backend.cancel().await.unwrap();
}

#[tokio::test]
async fn backend_rejects_new_version_via_discover() {
    // The backend does not advertise 2026-07-28, so a strict Discover fails.
    let res = BackendClient
        .serve_with_lifecycle(
            spawn_backend(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await;
    assert!(res.is_err());
}

#[tokio::test]
async fn auto_mode_only_new_version_is_a_hard_error() {
    // Finding: the backend answers `server/discover`, so `Auto` does NOT fall
    // back to `initialize`; it reports "no compatible protocol version".
    let res = BackendClient
        .serve_with_lifecycle(
            spawn_backend(),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await;
    let err = res.err().expect("expected error");
    assert!(format!("{err}").contains("no compatible protocol version"));
}

#[tokio::test]
async fn auto_mode_with_legacy_in_preferred_list_connects() {
    let svc = BackendClient
        .serve_with_lifecycle(
            spawn_backend(),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![
                    ProtocolVersion::V_2026_07_28,
                    ProtocolVersion::V_2025_11_25,
                ],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await
        .expect("connect");
    let tools = svc.list_all_tools().await.unwrap();
    assert_eq!(tools[0].name, "add");
    svc.cancel().await.unwrap();
}

async fn bridged_front() -> rmcp::service::RunningService<rmcp::RoleClient, FrontClient> {
    let backend = BackendClient.serve(spawn_backend()).await.unwrap();
    let bridge = Bridge::new(backend.peer().clone(), backend);
    let (s, c) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let svc = bridge.serve(s).await.unwrap();
        let _ = svc.waiting().await;
    });
    FrontClient
        .serve_with_lifecycle(
            c,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn bridge_new_front_to_legacy_backend() {
    let front = bridged_front().await;
    let tools = front.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "add");
    let r = front.call_tool(add_args(40, 2)).await.unwrap();
    assert!(format!("{:?}", r).contains("42"));
    front.cancel().await.unwrap();
}

#[tokio::test]
async fn bridge_passes_unknown_tool_error_through() {
    let front = bridged_front().await;
    let err = front
        .call_tool(CallToolRequestParams::new("nope"))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("tool not found"), "{err}");
    front.cancel().await.unwrap();
}
