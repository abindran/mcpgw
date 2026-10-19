use mcpgw_proto::spike_server::EchoServer;
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, ServiceExt,
    model::{CallToolRequestParams, ProtocolVersion},
};

#[derive(Clone, Default)]
struct TestClient;
impl ClientHandler for TestClient {}

fn args(text: &str) -> CallToolRequestParams {
    CallToolRequestParams::new("echo").with_arguments(
        serde_json::json!({ "text": text })
            .as_object()
            .unwrap()
            .clone(),
    )
}

#[tokio::test]
async fn legacy_initialize_list_and_call() {
    let (s, c) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let svc = EchoServer::new().serve(s).await.unwrap();
        svc.waiting().await.unwrap();
    });
    let client = TestClient.serve(c).await.unwrap();
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "echo");
    let r = client.call_tool(args("hi")).await.unwrap();
    assert!(format!("{:?}", r).contains("hi"));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn discover_2026_07_28() {
    let (s, c) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let svc = EchoServer::new().serve(s).await.unwrap();
        svc.waiting().await.unwrap();
    });
    let client = TestClient
        .serve_with_lifecycle(
            c,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    let r = client.call_tool(args("new")).await.unwrap();
    assert!(format!("{:?}", r).contains("new"));
    client.cancel().await.unwrap();
}
