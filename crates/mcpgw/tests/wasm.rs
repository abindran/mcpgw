//! M4-01 end to end: `mcpgw add NAME --wasm FILE` pins a Wasm tool's
//! tools; the daemon serves it like any server; compiled code is cached on
//! disk next to the store.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::McpClient;
use serde_json::json;

struct Env(PathBuf);

impl Env {
    fn ok(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.0.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "{args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.0.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm_tool_through_the_daemon() {
    let dir = std::env::temp_dir().join(format!("mcpgw-wasm-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n",
            dir.join("db.sqlite"),
            dir.join("token")
        ),
    )
    .unwrap();
    let e = Env(dir.clone());
    let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mcpgw-wasm/tests/fixtures/echo-tool.wasm");
    let added = e.ok(&["add", "wt", "--wasm", wasm.to_str().unwrap()]);
    assert!(added.contains("added server wt (3 tools, pin "), "{added}");
    let list = e.ok(&["list"]);
    assert!(
        list.contains("wt\twasm\tstore\t") && list.contains("echo-tool.wasm"),
        "{list}"
    );
    let cached: Vec<_> = std::fs::read_dir(dir.join("runtimes/wasm"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(cached.len(), 1, "compiled component cached on disk");

    e.ok(&["up"]);
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    assert_eq!(
        c.call_text("wt__echo", json!({"text": "via the daemon"}))
            .await
            .unwrap(),
        "via the daemon"
    );
    let st = e.ok(&["status"]);
    assert!(st.contains("wt\tidle\t3 tools\t-"), "{st}");
}
