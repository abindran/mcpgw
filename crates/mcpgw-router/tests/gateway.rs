//! Front client ⇄ gateway ⇄ real child-process backend, driven with
//! raw newline-delimited JSON-RPC (this crate may not use `rmcp`).

use std::{sync::Arc, time::Duration};

use mcpgw_core::ServerSpec;
use mcpgw_runtime::{BackendManager, ManagerOptions, ProtoError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn fixture_server(name: &str) -> ServerSpec {
    mcpgw_testkit::stdio(name, &[])
}

struct Rpc {
    w: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    r: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    next: i64,
}

impl Rpc {
    async fn send(&mut self, v: Value) {
        let mut s = v.to_string();
        s.push('\n');
        self.w.write_all(s.as_bytes()).await.unwrap();
    }
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(20), self.r.read_line(&mut line))
                .await
                .expect("response timeout")
                .unwrap();
            assert!(n > 0, "gateway closed the stream");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
        }
    }
}

async fn gateway() -> (Arc<BackendManager>, Rpc) {
    let m = Arc::new(BackendManager::start(
        &[fixture_server("fx")],
        ManagerOptions {
            eager: true,
            ..ManagerOptions::default()
        },
    ));
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let (s, c) = tokio::io::duplex(1 << 16);
    tokio::spawn(mcpgw_router::serve_io(m.clone(), s));
    let (r, w) = tokio::io::split(c);
    let mut rpc = Rpc {
        w,
        r: BufReader::new(r),
        next: 0,
    };
    let init = rpc
        .request(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        )
        .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "mcpgw", "{init}");
    rpc.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    (m, rpc)
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_and_calls_through_gateway() {
    let (m, mut rpc) = gateway().await;
    let list = rpc.request("tools/list", json!({})).await;
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names[..4], ["fx__add", "fx__crash", "fx__echo", "fx__env"]);

    let r = rpc
        .request(
            "tools/call",
            json!({"name":"fx__add","arguments":{"a":20,"b":22}}),
        )
        .await;
    assert_eq!(r["result"]["content"][0]["text"], "42", "{r}");

    let r = rpc
        .request("tools/call", json!({"name":"fx__missing"}))
        .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let r = rpc.request("tools/call", json!({"name":"nope"})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    m.shutdown().await;
}

fn fixture_env(name: &str, prefix: &str) -> ServerSpec {
    mcpgw_testkit::stdio(name, &[("FAKE_PREFIX", prefix)])
}

#[tokio::test(flavor = "multi_thread")]
async fn aggregates_two_backends_and_routes_by_prefix() {
    let m = BackendManager::start(
        &[fixture_env("one", "1:"), fixture_env("two", "2:")],
        ManagerOptions {
            eager: true,
            ..ManagerOptions::default()
        },
    );
    assert!(m.wait_ready(Duration::from_secs(20)).await);
    let all = mcpgw_router::list_all_tools(&m).await;
    let names: Vec<_> = all.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 14);
    assert_eq!(names[0], "one__add");
    assert_eq!(names[7], "two__add");
    assert_eq!(all[0]["description"], "[one] Add two integers");
    for (who, want) in [("one__echo", "1:x"), ("two__echo", "2:x")] {
        let r = mcpgw_router::call_public(&m, who, json!({"text": "x"}).as_object().cloned())
            .await
            .unwrap();
        assert_eq!(r["content"][0]["text"], want);
    }
    for bad in ["echo", "three__echo"] {
        assert!(matches!(
            mcpgw_router::call_public(&m, bad, None).await,
            Err(ProtoError::Rpc { code: -32602, .. })
        ));
    }
    m.shutdown().await;
}

/// Two servers with a tool named `search` both work; long and dotted names
/// get stable, routable public names; an alias replaces the prefix.
#[tokio::test(flavor = "multi_thread")]
async fn namespacing_keeps_same_named_tools_apart() {
    let long = "list_pull_request_review_comments_for_repository_in_organization";
    let gh_tools = format!(
        r#"[{{"name":"search","reply":"github results"}},{{"name":"{long}","reply":"long ok"}},{{"name":"read.file","reply":"dotted ok"}}]"#
    );
    let gl_tools = r#"[{"name":"search","reply":"gitlab results"}]"#;
    let mut gl = mcpgw_testkit::stdio("gitlab-enterprise", &[("FAKE_TOOLS", gl_tools)]);
    gl.alias = Some("gl".into());
    let m = BackendManager::start(
        &[
            mcpgw_testkit::stdio("github", &[("FAKE_TOOLS", &gh_tools)]),
            gl,
        ],
        ManagerOptions {
            eager: true,
            ..ManagerOptions::default()
        },
    );
    assert!(m.wait_ready(Duration::from_secs(20)).await);

    let tools = mcpgw_router::list_all_tools(&m).await;
    let names: Vec<String> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.iter().all(|n| n.len() <= mcpgw_router::MAX_TOOL_NAME),
        "{names:?}"
    );
    assert!(names.contains(&"github__search".to_string()), "{names:?}");
    assert!(names.contains(&"gl__search".to_string()), "{names:?}");
    let long_public = mcpgw_router::public_name("github", long);
    let dotted_public = mcpgw_router::public_name("github", "read.file");
    assert!(
        names.contains(&long_public) && names.contains(&dotted_public),
        "{names:?}"
    );

    let call = |n: String| {
        let m = &m;
        async move {
            let r = mcpgw_router::call_public(m, &n, None).await.unwrap();
            r["content"][0]["text"].as_str().unwrap().to_string()
        }
    };
    assert_eq!(call("github__search".into()).await, "github results");
    assert_eq!(call("gl__search".into()).await, "gitlab results");
    assert_eq!(call(long_public).await, "long ok");
    assert_eq!(call(dotted_public).await, "dotted ok");
    // the server name is not a prefix when an alias is set
    assert!(
        mcpgw_router::call_public(&m, "gitlab-enterprise__search", None)
            .await
            .is_err()
    );
    m.shutdown().await;
}
