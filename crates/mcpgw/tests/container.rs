//! M3-01 end to end: `mcpgw add --image` pins the digest; through the
//! daemon the container starts on the first call, shows in `status`,
//! `stats` and `logs`, and is removed when idle and on `mcpgw down`.
//!
//! Skipped without Docker for Linux containers, unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{path::PathBuf, process::Command, time::Duration};

use mcpgw_testkit::{
    McpClient,
    container::{IMAGE, SERVER_JS, containers_of, containers_or_skip},
};
use serde_json::json;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-ctr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nidle_timeout = \"3s\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n",
                dir.join("db.sqlite"),
                dir.join("token")
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "test pass")
            .output()
            .unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, stdout, stderr) = self.run(args);
        assert!(ok, "mcpgw {args:?} failed: {stderr}\n{stdout}");
        stdout
    }

    fn url(&self) -> String {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        info["url"].as_str().unwrap().to_string()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["down"]);
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

#[tokio::test(flavor = "multi_thread")]
async fn container_server_through_the_daemon() {
    if !containers_or_skip("container_server_through_the_daemon") {
        return;
    }
    let e = Env::new("e2e");
    let name = format!("ctr{}", std::process::id());

    // add: the image digest is pinned and the tools are captured
    let added = e.ok(&[
        "add",
        &name,
        "--image",
        IMAGE,
        "--env",
        "GREETING=hi",
        "--",
        "node",
        "-e",
        SERVER_JS,
    ]);
    assert!(
        added.contains(&format!("pinned {IMAGE} to sha256:")),
        "{added}"
    );
    assert!(
        added.contains(&format!("added server {name} (5 tools, pin ")),
        "{added}"
    );
    assert!(
        containers_of(&name).is_empty(),
        "introspection left a container"
    );
    let list = e.ok(&["list"]);
    assert!(
        list.contains(&format!("{name}\tcontainer\tstore\t{IMAGE}@sha256:")),
        "{list}"
    );
    let (ok, _, err) = e.run(&["add", "bad", "--image", IMAGE, "--run-arg=-d"]);
    assert!(!ok && err.contains("managed by mcpgw"), "{err}");

    // first call starts it
    assert!(e.ok(&["up"]).starts_with("daemon started"));
    assert!(
        e.ok(&["stats"])
            .contains("no container servers are running")
    );
    let token = e.ok(&["token"]);
    let c = McpClient::http(&e.url(), Some(token.trim())).await.unwrap();
    let r = c
        .call_text(&format!("{name}__env"), json!({"name": "GREETING"}))
        .await
        .unwrap();
    assert_eq!(r, "hi");
    let ctr = containers_of(&name);
    assert_eq!(ctr.len(), 1, "{ctr:?}");
    let st = e.ok(&["status"]);
    assert!(
        st.contains(&format!("{name}\tidle\t5 tools\tdocker {}", ctr[0])),
        "{st}"
    );
    let stats = e.ok(&["stats"]);
    assert!(
        stats.contains(&format!("{name}\t{}\t", ctr[0])) && stats.contains('%'),
        "{stats}"
    );
    assert!(e.ok(&["logs", &name]).contains("node-fake ready"));

    // idle: stopped and removed
    assert!(
        until(|| e.ok(&["status"]).contains(&format!("{name}\tcold")), 30).await,
        "{}",
        e.ok(&["status"])
    );
    assert!(
        until(|| containers_of(&name).is_empty(), 20).await,
        "{:?}",
        containers_of(&name)
    );

    // down removes a running one
    assert_eq!(
        c.call_text(&format!("{name}__echo"), json!({"text": "x"}))
            .await
            .unwrap(),
        "x"
    );
    assert_eq!(containers_of(&name).len(), 1);
    e.ok(&["down"]);
    assert!(
        until(|| containers_of(&name).is_empty(), 20).await,
        "{:?}",
        containers_of(&name)
    );
}
