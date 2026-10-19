//! The daemon's Streamable HTTP front door, driven by a real MCP client.

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use mcpgw_testkit::McpClient;
use serde_json::json;

struct Daemon {
    child: Child,
    dir: PathBuf,
    url: String,
}

impl Daemon {
    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-http-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = format!(
            "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\n\
             [[server]]\nname = \"fx\"\ntransport = \"stdio\"\ncommand = {:?}\n",
            dir.join("db.sqlite"),
            dir.join("token"),
            mcpgw_testkit::fake_mcp(),
        );
        std::fs::write(dir.join("config.toml"), cfg).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(dir.join("config.toml"))
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let url = loop {
            let line = lines.next().expect("daemon exited").unwrap();
            if let Some(u) = line.strip_prefix("mcpgw: listening on ") {
                break u.to_string();
            }
        };
        std::thread::spawn(move || lines.for_each(drop));
        Self { child, dir, url }
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.dir.join("token"))
            .unwrap()
            .trim()
            .to_string()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_with_token_lists_and_calls_over_http() {
    let d = Daemon::start("ok");
    assert!(
        d.url.starts_with("http://127.0.0.1:") && d.url.ends_with("/mcp"),
        "{}",
        d.url
    );
    let c = McpClient::http(&d.url, Some(&d.token())).await.unwrap();
    assert_eq!(c.protocol_version(), "2026-07-28");
    let names = c.tool_names().await.unwrap();
    assert!(names.contains(&"fx__add".to_string()), "{names:?}");
    assert_eq!(
        c.call_text("fx__add", json!({"a": 40, "b": 2}))
            .await
            .unwrap(),
        "42"
    );
    assert!(c.call("fx__nope", json!({})).await.is_err());
    c.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_client_gets_a_session() {
    let d = Daemon::start("legacy");
    let c = McpClient::http_legacy(&d.url, Some(&d.token()))
        .await
        .unwrap();
    assert_eq!(c.protocol_version(), "2025-11-25");
    assert_eq!(
        c.call_text("fx__echo", json!({"text": "hi"}))
            .await
            .unwrap(),
        "hi"
    );
    c.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_without_or_with_wrong_token_is_refused() {
    let d = Daemon::start("noauth");
    assert!(McpClient::http(&d.url, None).await.is_err());
    assert!(
        McpClient::http(&d.url, Some("0".repeat(64).as_str()))
            .await
            .is_err()
    );
}
