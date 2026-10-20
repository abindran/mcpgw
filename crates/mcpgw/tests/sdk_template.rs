//! M4-04 done-when: a new tool goes from the template to a working call in
//! under 15 minutes. The template (`sdk/template`) is rendered as
//! `cargo generate` would, built for wasm32-unknown-unknown against the
//! in-repo SDK, added with `mcpgw add --wasm` (the plain `cargo build`
//! output: mcpgw makes the component), and called through the daemon.
//!
//! Needs the wasm32-unknown-unknown target; skipped without it unless
//! MCPGW_TEST_WASM=1 (the Linux glibc CI job).

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use mcpgw_testkit::McpClient;
use serde_json::json;

fn wasm_target_installed() -> bool {
    let Ok(o) = Command::new("rustc").args(["--print", "sysroot"]).output() else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&o.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib/wasm32-unknown-unknown")
        .is_dir()
}

fn copy_dir(from: &Path, to: &Path, render: &dyn Fn(&str) -> String) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let dest = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dest, render);
        } else {
            std::fs::write(&dest, render(&std::fs::read_to_string(&p).unwrap())).unwrap();
        }
    }
}

struct Daemon(PathBuf);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.0.join("config.toml"))
            .arg("down")
            .output();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn template_to_working_call_in_under_15_minutes() {
    if !wasm_target_installed() {
        assert!(
            std::env::var("MCPGW_TEST_WASM").as_deref() != Ok("1"),
            "MCPGW_TEST_WASM=1 but the wasm32-unknown-unknown target isn't installed"
        );
        eprintln!(
            "template_to_working_call_in_under_15_minutes: skipped (no wasm32-unknown-unknown target)"
        );
        return;
    }
    let started = Instant::now();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = std::env::temp_dir().join(format!("mcpgw-sdk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // 1. generate (what `cargo generate --git … sdk/template --name hello-tool` does)
    let project = dir.join("hello-tool");
    let sdk = repo.join("crates/mcpgw-tool");
    let render = |s: &str| {
        s.replace("{{project-name}}", "hello-tool")
            .replace("{{crate_name}}", "hello_tool")
            // build against this checkout's SDK
            .replace(
                r#"mcpgw-tool = { git = "https://github.com/abindran/mcpgw" }"#,
                &format!("mcpgw-tool = {{ path = {:?} }}", sdk.to_string_lossy()),
            )
    };
    copy_dir(&repo.join("sdk/template"), &project, &render);
    std::fs::remove_file(project.join("cargo-generate.toml")).unwrap();

    // 2. build
    let target = repo.join("target/sdk-template");
    let o = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "build: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let wasm = target.join("wasm32-unknown-unknown/release/hello_tool.wasm");
    println!(
        "built {} ({} bytes) after {:?}",
        wasm.display(),
        std::fs::metadata(&wasm).unwrap().len(),
        started.elapsed()
    );

    // 3. add and call
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
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "{args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    };
    let added = mcpgw(&["add", "hello-tool", "--wasm", wasm.to_str().unwrap()]);
    assert!(
        added.contains("added server hello-tool (1 tools, pin "),
        "{added}"
    );
    mcpgw(&["up"]);
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = mcpgw(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    let tools = c.tools().await.unwrap();
    let greet = tools
        .iter()
        .find(|t| t["name"] == "hello-tool__greet")
        .expect("greet listed");
    assert_eq!(
        greet["inputSchema"]["properties"]["name"]["description"],
        "Who to greet."
    );
    assert_eq!(
        c.call_text("hello-tool__greet", json!({"name": "mcpgw"}))
            .await
            .unwrap(),
        "Hello, mcpgw!"
    );
    let bad = c
        .call("hello-tool__greet", json!({"name": " "}))
        .await
        .unwrap();
    assert_eq!(bad["isError"], true);

    let took = started.elapsed();
    println!("template to working call: {took:?}");
    assert!(took < Duration::from_secs(15 * 60), "{took:?}");
    // `mcpgw down` needs the config: stop the daemon before removing it
    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
