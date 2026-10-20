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

/// M4-02 end to end: grants given on the command line, a secret bound into
/// the component's environment, and the default of nothing.
#[tokio::test(flavor = "multi_thread")]
async fn wasm_grants_from_the_command_line() {
    let dir = std::env::temp_dir().join(format!("mcpgw-wasm-grants-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::write(dir.join("shared/hello.txt"), "shared file").unwrap();
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mcpgw-wasm/tests/fixtures/probe-tool.wasm");
    let shared = dir.join("shared");

    let mut set = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--config")
        .arg(dir.join("config.toml"))
        .args(["secret", "set", "probe_key"])
        .env("MCPGW_VAULT_PASSPHRASE", "p")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    set.stdin
        .take()
        .unwrap()
        .write_all(format!("key-{}\n", "q".repeat(16)).as_bytes())
        .unwrap();
    assert!(set.wait().unwrap().success());

    let add = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "p")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    add(&[
        "add",
        "granted",
        "--wasm",
        probe.to_str().unwrap(),
        "--allow-network",
        &format!("127.0.0.1:{port}"),
        "--allow-dir",
        shared.to_str().unwrap(),
        "--env",
        "REGION=eu",
        "--secret",
        "API_KEY=probe_key",
    ]);
    add(&["add", "bare", "--wasm", probe.to_str().unwrap()]);

    let up = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--config")
        .arg(dir.join("config.toml"))
        .arg("up")
        .env("MCPGW_VAULT_PASSPHRASE", "p")
        .output()
        .unwrap();
    assert!(up.status.success());
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    let call = |tool: &str, args: serde_json::Value| {
        let c = &c;
        let tool = tool.to_string();
        async move { c.call_text(&tool, args).await.unwrap() }
    };
    let target = json!({"host": "127.0.0.1", "port": port});
    assert_eq!(
        call("granted__connect", target.clone()).await,
        "ok connected"
    );
    assert!(call("bare__connect", target).await.starts_with("denied:"));
    let file = format!("{}/hello.txt", shared.to_string_lossy().replace('\\', "/"));
    assert_eq!(
        call("granted__read", json!({"path": file})).await,
        "ok shared file"
    );
    assert_eq!(
        call("bare__read", json!({"path": file})).await,
        "denied: no directories granted"
    );
    assert_eq!(
        call("granted__env", json!({"name": "REGION"})).await,
        "ok eu"
    );
    // the bound secret reaches the component, and is redacted on the way out
    assert_eq!(
        call("granted__env", json!({"name": "API_KEY"})).await,
        "ok [REDACTED:probe_key]"
    );
    assert_eq!(
        call("bare__env", json!({"name": "REGION"})).await,
        "denied: not set"
    );
    drop(listener);
}
