//! M4-09 end to end: long tool calls through the daemon. A call reports
//! progress to the client and outlives the call timeout while it does; a
//! cancelled request stops the tool at the backend; a client that declares
//! the tasks extension gets a task it can poll and cancel, whether the
//! gateway or the backend runs it.
//!
//! The done-when ("a 10-minute tool call reports progress and can be
//! cancelled") is `ten_minute_call`, run on demand:
//! `cargo test -p mcpgw --test long_calls -- --ignored`.

use std::{path::PathBuf, process::Command, time::Duration};

use mcpgw_testkit::{ClientKind, McpClient};
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-long-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = mcpgw_testkit::fake_mcp();
        let cfg = format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\
             call_timeout = \"1s\"\nmax_call_time = \"20m\"\n\n\
             [[server]]\nname = \"gw\"\ntransport = \"stdio\"\ncommand = {fake:?}\n\
             env = {{ FAKE_CALL_LOG = {:?} }}\n\n\
             [[server]]\nname = \"bt\"\ntransport = \"stdio\"\ncommand = {fake:?}\n\
             env = {{ FAKE_CALL_LOG = {:?}, FAKE_TASKS = \"1\" }}\n",
            dir.join("db.sqlite"),
            dir.join("token"),
            dir.join("gw.log"),
            dir.join("bt.log"),
        );
        std::fs::write(dir.join("config.toml"), cfg).unwrap();
        let e = Self { dir };
        e.ok(&["up"]);
        e
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn log(&self, server: &str) -> String {
        std::fs::read_to_string(self.dir.join(format!("{server}.log"))).unwrap_or_default()
    }

    async fn client(&self, tasks: bool) -> McpClient {
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        let token = self.ok(&["token"]);
        let kind = ClientKind {
            tasks,
            ..Default::default()
        };
        McpClient::http_kind(
            info["url"].as_str().unwrap(),
            Some(token.trim()),
            kind,
            false,
        )
        .await
        .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn until(mut f: impl FnMut() -> bool, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Poll `tasks/get` until the task leaves `working`.
async fn settle(c: &McpClient, id: &str, secs: u64) -> Value {
    let mut last = Value::Null;
    for _ in 0..secs * 5 {
        last = c.get_task(id).await.unwrap();
        if last["status"] != "working" {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("task {id} still working: {last}");
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_and_cancellation_pass_through() {
    let e = Env::new("plain");
    let c = e.client(false).await;
    // 4 s with progress every 0.5 s outlives the 1 s call timeout
    let r = c
        .call_text("gw__sleep", json!({"ms": 4000, "steps": 8}))
        .await
        .unwrap();
    assert_eq!(r, "slept");
    let p = c.progress();
    assert_eq!(p.len(), 8, "{p:?}");
    assert_eq!(p[7].progress, 8.0);
    assert_eq!(p[7].total, Some(8.0));
    assert_eq!(p[7].message.as_deref(), Some("step 8 of 8"));
    // silence does not
    let r = c.call("gw__sleep", json!({"ms": 3000})).await;
    assert!(
        format!("{r:?}").contains("timed out"),
        "a silent call outlived the timeout: {r:?}"
    );

    // a cancelled request stops the tool at the backend
    let running = c
        .start_call("gw__sleep", json!({"ms": 30000, "steps": 60}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    running.cancel().await.unwrap();
    assert!(
        until(|| e.log("gw").contains("sleep cancelled at"), 10).await,
        "{}",
        e.log("gw")
    );
    assert_eq!(
        c.call_text("gw__add", json!({"a": 1, "b": 1}))
            .await
            .unwrap(),
        "2"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_run_by_the_gateway_or_the_backend() {
    let e = Env::new("tasks");
    let c = e.client(true).await;
    assert!(
        c.server_capabilities()["extensions"]
            .get("io.modelcontextprotocol/tasks")
            .is_some(),
        "{}",
        c.server_capabilities()
    );
    // a quick call is answered directly
    let r = c
        .call_once("gw__add", json!({"a": 2, "b": 3}))
        .await
        .unwrap();
    assert_eq!(r["content"][0]["text"], "5", "{r}");

    for server in ["gw", "bt"] {
        // a long one becomes a task: polled to its end
        let tool = format!("{server}__sleep");
        let r = c
            .call_once(&tool, json!({"ms": 3000, "steps": 6}))
            .await
            .unwrap();
        assert_eq!(r["resultType"], "task", "{server}: {r}");
        let id = r["taskId"].as_str().unwrap().to_string();
        assert_eq!(r["status"], "working");
        assert!(r["pollIntervalMs"].as_u64().is_some());
        // progress shows in the status message while it runs
        let mut message = None;
        for _ in 0..50 {
            let t = c.get_task(&id).await.unwrap();
            if let Some(m) = t["statusMessage"].as_str() {
                message = Some(m.to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let message = message.unwrap_or_default();
        assert!(message.contains("of 6"), "{server}: {message:?}");
        let done = settle(&c, &id, 30).await;
        assert_eq!(done["status"], "completed", "{server}: {done}");
        assert_eq!(
            done["result"]["content"][0]["text"], "slept",
            "{server}: {done}"
        );

        // tasks/cancel stops the tool at the backend
        let r = c
            .call_once(&tool, json!({"ms": 30000, "steps": 60}))
            .await
            .unwrap();
        let id = r["taskId"].as_str().unwrap().to_string();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        c.cancel_task(&id).await.unwrap();
        let t = c.get_task(&id).await.unwrap();
        assert_eq!(t["status"], "cancelled", "{server}: {t}");
        assert!(
            until(|| e.log(server).contains("sleep cancelled at"), 10).await,
            "{server}: {}",
            e.log(server)
        );
    }
    // unknown (or other endpoints') tasks are not found
    let err = c
        .get_task("0123456789abcdef0123456789abcdef")
        .await
        .unwrap_err();
    assert!(err.contains("no task"), "{err}");
}

/// The done-when, at full length (about 10 minutes).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "takes 10 minutes; run with --ignored"]
async fn ten_minute_call() {
    let e = Env::new("ten");
    let c = e.client(false).await;
    let started = std::time::Instant::now();
    let r = c
        // progress every 0.5 s, half the 1 s call timeout
        .call_text("gw__sleep", json!({"ms": 600_000, "steps": 1200}))
        .await
        .unwrap();
    assert_eq!(r, "slept");
    assert!(started.elapsed() >= Duration::from_secs(600));
    let p = c.progress();
    assert_eq!(p.len(), 1200, "progress notifications");
    assert_eq!(p[1199].message.as_deref(), Some("step 1200 of 1200"));

    // and the same length, cancelled near the end
    let running = c
        .start_call("gw__sleep", json!({"ms": 600_000, "steps": 1200}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(590)).await;
    running.cancel().await.unwrap();
    // about step 1180 of 1200
    assert!(
        until(|| e.log("gw").contains("sleep cancelled at 11"), 10).await,
        "{}",
        e.log("gw")
    );
}
