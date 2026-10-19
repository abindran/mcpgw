//! `mcpgw connect`: a stdio client (like Cursor's stdio entry) reaches the
//! daemon, which is started on demand and then shared.

use std::path::PathBuf;

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-connect-{tag}-{}", std::process::id()));
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
        Self { dir }
    }

    fn connect_cmd(&self) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .arg("connect");
        c
    }

    fn daemon(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.dir.join("daemon.json")).ok()?;
        serde_json::from_str(&text).ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon().and_then(|d| d["pid"].as_u64()) {
            #[cfg(unix)]
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
            #[cfg(windows)]
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_starts_the_daemon_and_later_clients_share_it() {
    let e = Env::new("share");
    assert!(e.daemon().is_none());

    let first = McpClient::stdio(e.connect_cmd()).await.unwrap();
    assert_eq!(first.protocol_version(), "2026-07-28");
    assert!(
        first
            .tool_names()
            .await
            .unwrap()
            .contains(&"fx__add".to_string())
    );
    assert_eq!(
        first
            .call_text("fx__add", json!({"a": 1, "b": 2}))
            .await
            .unwrap(),
        "3"
    );
    let daemon = e
        .daemon()
        .expect("daemon.json written by the auto-started daemon");
    assert!(
        daemon["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );

    // A second client, here a 2025-11-25 one, reuses the same daemon.
    let second = McpClient::stdio_legacy(e.connect_cmd()).await.unwrap();
    assert_eq!(second.protocol_version(), "2025-11-25");
    assert_eq!(
        second
            .call_text("fx__echo", json!({"text": "hi"}))
            .await
            .unwrap(),
        "hi"
    );
    assert_eq!(
        e.daemon().unwrap()["pid"],
        daemon["pid"],
        "no second daemon"
    );

    // Clients come and go; the daemon stays up.
    first.close().await;
    second.close().await;
    let third = McpClient::stdio(e.connect_cmd()).await.unwrap();
    assert_eq!(
        third
            .call_text("fx__add", json!({"a": 2, "b": 2}))
            .await
            .unwrap(),
        "4"
    );
    assert_eq!(e.daemon().unwrap()["pid"], daemon["pid"]);
    third.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_daemon_file_is_replaced() {
    let e = Env::new("stale");
    std::fs::write(
        e.dir.join("daemon.json"),
        r#"{"pid": 999999, "url": "http://127.0.0.1:1/mcp", "version": "0", "started_at": 0}"#,
    )
    .unwrap();
    let c = McpClient::stdio(e.connect_cmd()).await.unwrap();
    assert_eq!(
        c.call_text("fx__add", json!({"a": 5, "b": 5}))
            .await
            .unwrap(),
        "10"
    );
    assert_ne!(e.daemon().unwrap()["pid"], 999999);
    c.close().await;
}
