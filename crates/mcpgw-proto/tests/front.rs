//! M1-05: the front server answers both 2026-07-28 and legacy clients.

use std::sync::Arc;

use mcpgw_proto::{
    front::{serve_io, BoxFuture, FrontInfo, ToolProvider},
    JsonObject, ProtoError,
};
use rmcp::{
    model::{CallToolRequestParams, ProtocolVersion},
    ClientHandler, ClientLifecycleMode, ClientServiceExt, ServiceExt,
};
use serde_json::json;

struct Fake;
impl ToolProvider for Fake {
    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>> {
        Box::pin(async {
            vec![
                json!({"name": "a__echo", "description": "[a] Echo",
                       "inputSchema": {"type": "object"}})
                .as_object()
                .unwrap()
                .clone(),
                json!({"broken": true}).as_object().unwrap().clone(),
            ]
        })
    }
    fn call_tool(
        &self,
        name: String,
        args: Option<JsonObject>,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>> {
        Box::pin(async move {
            match name.as_str() {
                "a__echo" => Ok(json!({"content": [{"type": "text",
                    "text": args.unwrap()["text"].clone()}], "isError": false})
                .as_object()
                .unwrap()
                .clone()),
                "a__down" => Err(ProtoError::Unavailable("a".into())),
                _ => Err(ProtoError::Rpc {
                    code: -32602,
                    message: format!("tool not found: {name}"),
                    data: None,
                }),
            }
        })
    }
}

#[derive(Clone, Default)]
struct C;
impl ClientHandler for C {}

fn front() -> tokio::io::DuplexStream {
    let (s, c) = tokio::io::duplex(8192);
    let info = FrontInfo {
        name: "mcpgw".into(),
        version: "0.0.0".into(),
        instructions: Some("hi".into()),
    };
    tokio::spawn(serve_io(Arc::new(Fake), info, s));
    c
}

fn echo(text: &str) -> CallToolRequestParams {
    CallToolRequestParams::new("a__echo")
        .with_arguments(json!({"text": text}).as_object().unwrap().clone())
}

#[tokio::test]
async fn modern_client() {
    let c = C
        .serve_with_lifecycle(
            front(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    let info = c.peer_info().unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
    assert_eq!(info.server_info.as_ref().unwrap().name, "mcpgw");
    let tools = c.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 1, "malformed tool is skipped");
    assert_eq!(tools[0].name, "a__echo");
    let r = c.call_tool(echo("yo")).await.unwrap();
    assert!(format!("{r:?}").contains("yo"));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn legacy_client_and_error_mapping() {
    let c = C.serve(front()).await.unwrap();
    assert_eq!(
        c.peer_info().unwrap().protocol_version,
        ProtocolVersion::V_2025_11_25
    );
    let r = c.call_tool(echo("old")).await.unwrap();
    assert!(format!("{r:?}").contains("old"));
    let e = c
        .call_tool(CallToolRequestParams::new("zzz"))
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("tool not found: zzz"), "{e}");
    let e = c
        .call_tool(CallToolRequestParams::new("a__down"))
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("backend unavailable"), "{e}");
    c.cancel().await.unwrap();
}
