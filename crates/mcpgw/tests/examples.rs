//! M4 exit: the public examples work. `examples/wasm-text-tools` builds
//! and serves its tools through the daemon; `examples/openapi-petstore.toml`
//! is a valid config whose server is the Petstore API.
//!
//! The Wasm part needs the wasm32-unknown-unknown target; it is skipped
//! without it unless MCPGW_TEST_WASM=1 (the Linux glibc CI job).

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use mcpgw_testkit::McpClient;
use serde_json::json;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn wasm_target_installed() -> bool {
    let Ok(o) = Command::new("rustc").args(["--print", "sysroot"]).output() else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&o.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib/wasm32-unknown-unknown")
        .is_dir()
}

#[test]
fn the_openapi_example_is_a_valid_config() {
    let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--config")
        .arg(repo().join("examples/openapi-petstore.toml"))
        .arg("list")
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        out.contains("pets\topenapi\tconfig\thttps://petstore3.swagger.io/api/v3/openapi.json"),
        "{out}"
    );
}

struct Daemon(PathBuf);

impl Drop for Daemon {
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
async fn the_wasm_example_builds_and_serves_its_tools() {
    if !wasm_target_installed() {
        assert!(
            std::env::var("MCPGW_TEST_WASM").as_deref() != Ok("1"),
            "MCPGW_TEST_WASM=1 but the wasm32-unknown-unknown target isn't installed"
        );
        eprintln!("the_wasm_example_builds_and_serves_its_tools: skipped (no wasm32 target)");
        return;
    }
    let target = repo().join("target/examples");
    let o = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(repo().join("examples/wasm-text-tools"))
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "build: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let wasm = target.join("wasm32-unknown-unknown/release/text_tools.wasm");

    let dir = std::env::temp_dir().join(format!("mcpgw-examples-{}", std::process::id()));
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
    let daemon = Daemon(dir.clone());
    let mcpgw = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(dir.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let added = mcpgw(&["add", "text-tools", "--wasm", wasm.to_str().unwrap()]);
    assert!(
        added.contains("added server text-tools (2 tools, pin "),
        "{added}"
    );
    mcpgw(&["up"]);
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = mcpgw(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    assert_eq!(
        c.call_text(
            "text-tools__slugify",
            json!({"title": "Hello, Wasm World!"})
        )
        .await
        .unwrap(),
        "hello-wasm-world"
    );
    let counts = c
        .call("text-tools__count_words", json!({"text": "one two\nthree"}))
        .await
        .unwrap();
    let text = counts["content"][0]["text"].as_str().unwrap();
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        (v["words"].as_u64(), v["lines"].as_u64()),
        (Some(3), Some(2)),
        "{text}"
    );
    let bad = c
        .call("text-tools__slugify", json!({"title": "!!"}))
        .await
        .unwrap();
    assert_eq!(bad["isError"], true);
    drop(daemon);
}
