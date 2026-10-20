//! M4-10 end to end: `mcpgw dev` runs a server under a private gateway.
//! Editing the server restarts it and the next call uses the new code; a
//! broken edit is reported and the session carries on; every call is
//! logged; the inspector calls tools over the API.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use mcpgw_testkit::McpClient;
use serde_json::json;

/// A running `mcpgw dev`, with its output collected.
struct Session {
    child: Child,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
    dir: PathBuf,
}

impl Session {
    fn start(dir: &Path, args: &[&str]) -> Self {
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "[gateway]\n").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(&cfg)
            .arg("dev")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));
        for (pipe, buf) in [
            (
                Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
                out.clone(),
            ),
            (Box::new(child.stderr.take().unwrap()), err.clone()),
        ] {
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut b = buf.lock().unwrap();
                    b.push_str(&line);
                    b.push('\n');
                }
            });
        }
        Self {
            child,
            out,
            err,
            dir: dir.to_path_buf(),
        }
    }

    fn stderr(&self) -> String {
        self.err.lock().unwrap().clone()
    }

    async fn wait_for(&self, what: &str, secs: u64) -> bool {
        for _ in 0..secs * 10 {
            if self.stderr().contains(what) || self.out.lock().unwrap().contains(what) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// The MCP URL and the token, from the banner.
    async fn endpoint(&self) -> (String, String) {
        assert!(
            self.wait_for("inspector: ", 60).await,
            "no banner: {}{}",
            self.out.lock().unwrap(),
            self.stderr()
        );
        let out = self.out.lock().unwrap().clone();
        let url = out
            .lines()
            .find_map(|l| {
                l.split(" at ")
                    .nth(1)
                    .filter(|_| l.starts_with("mcpgw dev: serving"))
            })
            .unwrap()
            .to_string();
        let token = out
            .lines()
            .find_map(|l| l.split("#token=").nth(1))
            .unwrap()
            .to_string();
        (url, token)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tools(dir: &Path, reply: &str, extra: bool) {
    let mut t = vec![json!({"name": "hello", "description": "Say hello", "reply": reply})];
    if extra {
        t.push(json!({"name": "bye", "reply": "bye"}));
    }
    // write-then-rename, as editors do
    let tmp = dir.join("tools.json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&t).unwrap()).unwrap();
    std::fs::rename(&tmp, dir.join("tools.json")).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_restarts_the_server_and_the_next_call_uses_it() {
    let dir = std::env::temp_dir().join(format!("mcpgw-dev-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server = dir.join("my-server");
    std::fs::create_dir_all(&server).unwrap();
    tools(&server, "hello v1", false);
    let fake = mcpgw_testkit::fake_mcp();
    let file = format!("FAKE_TOOLS_FILE={}", server.join("tools.json").display());
    let s = Session::start(
        &dir,
        &[
            server.to_str().unwrap(),
            "--env",
            &file,
            "--",
            fake.to_str().unwrap(),
        ],
    );
    let (url, token) = s.endpoint().await;
    assert!(url.ends_with("/mcp"), "{url}");
    assert!(
        s.wait_for("my-server ready, 1 tool(s)", 30).await,
        "{}",
        s.stderr()
    );
    let c = McpClient::http(&url, Some(&token)).await.unwrap();
    assert_eq!(
        c.call_text("my-server__hello", json!({})).await.unwrap(),
        "hello v1"
    );
    // the request log
    assert!(
        s.wait_for("my-server__hello {}  ->  ok", 10).await,
        "{}",
        s.stderr()
    );

    // edit: the next call runs the new code, and the new tool is there
    tools(&server, "hello v2", true);
    assert!(
        s.wait_for("changed; restarting my-server", 20).await,
        "{}",
        s.stderr()
    );
    assert!(
        s.wait_for("my-server ready, 2 tool(s); added bye", 30)
            .await,
        "{}",
        s.stderr()
    );
    assert_eq!(
        c.call_text("my-server__hello", json!({})).await.unwrap(),
        "hello v2"
    );
    let names = c.tool_names().await.unwrap();
    assert!(names.contains(&"my-server__bye".to_string()), "{names:?}");

    // a broken edit is reported and the session goes on
    std::fs::write(server.join("tools.json"), "[{").unwrap();
    assert!(
        s.wait_for("my-server failed to start", 30).await,
        "{}",
        s.stderr()
    );
    tools(&server, "hello v3", false);
    assert!(
        s.wait_for("my-server ready, 1 tool(s); removed bye", 30)
            .await,
        "{}",
        s.stderr()
    );
    assert_eq!(
        c.call_text("my-server__hello", json!({})).await.unwrap(),
        "hello v3"
    );

    // the inspector's call, over the API
    mcpgw_core::tls::install();
    let base = url.trim_end_matches("/mcp");
    let r: serde_json::Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/api/v1/tools/my-server__hello/call"))
        .bearer_auth(&token)
        .json(&json!({"arguments": {}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "hello v3", "{r}");
    let page = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{base}/ui"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Inspector"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_wasm_component_is_reloaded() {
    let dir = std::env::temp_dir().join(format!("mcpgw-dev-wasm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../mcpgw-wasm/tests/fixtures");
    let wasm = dir.join("tool.wasm");
    std::fs::copy(fixtures.join("echo-tool.wasm"), &wasm).unwrap();
    std::fs::create_dir_all(dir.join("session")).unwrap();
    let s = Session::start(&dir.join("session"), &[wasm.to_str().unwrap()]);
    let (url, token) = s.endpoint().await;
    assert!(s.wait_for("tool ready", 60).await, "{}", s.stderr());
    let c = McpClient::http(&url, Some(&token)).await.unwrap();
    let before = c.tool_names().await.unwrap();
    // "rebuild": another component in its place
    let tmp = dir.join("tool.wasm.tmp");
    std::fs::copy(fixtures.join("probe-tool.wasm"), &tmp).unwrap();
    std::fs::rename(&tmp, &wasm).unwrap();
    assert!(
        s.wait_for("tool.wasm changed; restarting tool", 20).await,
        "{}",
        s.stderr()
    );
    assert!(s.wait_for("added ", 60).await, "{}", s.stderr());
    // a new session: the first one may still cache the old list (`ttlMs`)
    let c2 = McpClient::http(&url, Some(&token)).await.unwrap();
    let after = c2.tool_names().await.unwrap();
    assert_ne!(before, after);
    let _ = std::fs::remove_dir_all(&dir);
}
