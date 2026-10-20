//! M2-03: two endpoints (as two profiles will) call the same server with
//! different accounts. Each account gets its own server process and token.

use std::{collections::BTreeMap, sync::Arc};

use mcpgw_core::Mode;
use mcpgw_router::{GatewayOptions, http_router};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, SecretResolver};
use mcpgw_testkit::McpClient;
use secrecy::SecretString;
use serde_json::json;

struct Tokens;

impl SecretResolver for Tokens {
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String> {
        let v = match name {
            "oauth/github@default" => "token-for-default-acct",
            "oauth/github@work" => "token-for-work-acct",
            "oauth/github@personal" => "token-for-personal-acct",
            _ => return Ok(None),
        };
        Ok(Some(SecretString::from(v.to_string())))
    }
}

async fn endpoint(m: Arc<BackendManager>, accounts: &[(&str, &str)]) -> String {
    let opts = GatewayOptions {
        mode: Mode::Direct,
        accounts: accounts
            .iter()
            .map(|(s, a)| (s.to_string(), a.to_string()))
            .collect::<BTreeMap<_, _>>(),
        ..GatewayOptions::default()
    };
    let app = axum::Router::new().nest_service("/mcp", http_router(m, opts));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn token(c: &McpClient) -> String {
    c.call_text("github__env", json!({"name": "GITHUB_TOKEN"}))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_profiles_use_the_same_server_with_different_accounts() {
    let mut gh = mcpgw_testkit::stdio("github", &[]);
    gh.secrets
        .insert("GITHUB_TOKEN".into(), "oauth/github@{account}".into());
    let m = Arc::new(BackendManager::with_secrets(
        &[gh],
        ManagerOptions::default(),
        Arc::new(Catalog::default()),
        Some(Arc::new(Tokens)),
    ));
    let work = McpClient::http(&endpoint(m.clone(), &[("github", "work")]).await, None)
        .await
        .unwrap();
    let personal = McpClient::http(&endpoint(m.clone(), &[("github", "personal")]).await, None)
        .await
        .unwrap();
    let plain = McpClient::http(&endpoint(m.clone(), &[]).await, None)
        .await
        .unwrap();

    assert_eq!(token(&work).await, "[REDACTED:oauth/github@work]");
    assert_eq!(token(&personal).await, "[REDACTED:oauth/github@personal]");
    assert_eq!(token(&plain).await, "[REDACTED:oauth/github@default]");
    // again, in the other order: each endpoint keeps its own account
    assert_eq!(token(&personal).await, "[REDACTED:oauth/github@personal]");
    assert_eq!(token(&work).await, "[REDACTED:oauth/github@work]");

    // one process per account; listings show the server once
    let st = m.status().await;
    let names: Vec<_> = st.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["github", "github@personal", "github@work"]);
    let pids: std::collections::HashSet<_> = st.iter().map(|s| s.pid.unwrap()).collect();
    assert_eq!(pids.len(), 3, "accounts share a process: {st:?}");
    assert_eq!(m.names(), ["github"]);
    assert_eq!(
        work.tool_names().await.unwrap(),
        personal.tool_names().await.unwrap()
    );

    // disabling the server stops every account; re-enabled, accounts return on demand
    m.set_enabled("github", false).await.unwrap();
    assert_eq!(m.status().await.len(), 1);
    assert!(
        work.call("github__env", json!({"name": "GITHUB_TOKEN"}))
            .await
            .is_err()
    );
    m.set_enabled("github", true).await.unwrap();
    assert_eq!(token(&work).await, "[REDACTED:oauth/github@work]");

    m.remove_server("github").await.unwrap();
    assert!(m.status().await.is_empty());
    for c in [work, personal, plain] {
        c.close().await;
    }
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_without_account_secrets_ignore_the_account() {
    let m = Arc::new(BackendManager::start(
        &[mcpgw_testkit::stdio("fx", &[])],
        ManagerOptions::default(),
    ));
    let c = McpClient::http(&endpoint(m.clone(), &[("fx", "work")]).await, None)
        .await
        .unwrap();
    assert_eq!(
        c.call_text("fx__add", json!({"a": 1, "b": 2}))
            .await
            .unwrap(),
        "3"
    );
    assert_eq!(m.names(), ["fx"]);
    assert_eq!(
        m.status().await.len(),
        1,
        "no instance for a server without {{account}}"
    );
    c.close().await;
    m.shutdown().await;
}
