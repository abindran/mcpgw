//! M1-06 end to end: installed servers cost nothing until used, and an
//! idle one stops by itself.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use mcpgw_testkit::McpClient;
use serde_json::json;

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
}

fn pids(log: &Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

struct Daemon {
    child: Child,
    dir: PathBuf,
    url: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn daemon(tag: &str, gateway_extra: &str, servers: usize) -> (Daemon, PathBuf) {
    let dir = std::env::temp_dir().join(format!("mcpgw-lazy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("spawns.log");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n{gateway_extra}\n",
            dir.join("db.sqlite"),
            dir.join("token")
        ),
    )
    .unwrap();
    let mcpgw = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config").arg(dir.join("config.toml"));
        c
    };
    for i in 0..servers {
        let ok = mcpgw()
            .args(["add-server", &format!("s{i}"), "--command"])
            .arg(mcpgw_testkit::fake_mcp())
            .arg("--env")
            .arg(format!("FAKE_SPAWN_LOG={}", log.display()))
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    let mut child = mcpgw()
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
    (Daemon { child, dir, url }, log)
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_installed_servers_run_no_processes_until_used() {
    let (d, log) = daemon("ten", "", 10);
    assert_eq!(pids(&log).len(), 10, "one introspection spawn per install");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(pids(&log).len(), 10, "the daemon started a server");
    for pid in pids(&log) {
        assert!(!alive(pid), "server process {pid} is running");
    }

    let token = std::fs::read_to_string(d.dir.join("token")).unwrap();
    let c = McpClient::http(&d.url, Some(token.trim())).await.unwrap();
    assert_eq!(c.tool_names().await.unwrap().len(), 70);
    assert_eq!(pids(&log).len(), 10, "listing started a server");
    assert_eq!(
        c.call_text("s3__add", json!({"a": 1, "b": 2}))
            .await
            .unwrap(),
        "3"
    );
    assert_eq!(
        pids(&log).len(),
        11,
        "exactly one server started for the call"
    );
    c.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_server_stops_after_gateway_idle_timeout() {
    // long enough that a slow `alive` check (Windows CI) can't miss the
    // running server, short enough to keep the test quick
    let (d, log) = daemon("idle", "idle_timeout = \"2s\"", 1);
    let token = std::fs::read_to_string(d.dir.join("token")).unwrap();
    let c = McpClient::http(&d.url, Some(token.trim())).await.unwrap();
    c.call_text("s0__add", json!({"a": 1, "b": 2}))
        .await
        .unwrap();
    let pid = *pids(&log).last().unwrap();
    assert!(alive(pid));
    let mut stopped = false;
    for _ in 0..150 {
        if !alive(pid) {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stopped, "idle server {pid} still running");
    c.close().await;
}
