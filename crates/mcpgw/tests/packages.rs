//! M3-08 end to end: `mcpgw add --command npx -- -y <package>` installs the
//! package into the cache, `list` and `runtimes ls` show it, and the server
//! is introspected from the cache. Skipped where npm isn't installed.

use std::{path::Path, process::Command};

fn npm() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

fn pack(dir: &Path) -> Option<std::path::PathBuf> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("package.json"),
        r#"{"name": "node-fake-mcp", "version": "1.0.0", "bin": "server.js"}"#,
    )
    .unwrap();
    std::fs::write(src.join("server.js"), mcpgw_testkit::container::SERVER_JS).unwrap();
    let o = Command::new(npm())
        .args(["pack", "--pack-destination"])
        .arg(dir)
        .current_dir(&src)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| dir.join("node-fake-mcp-1.0.0.tgz"))
}

#[test]
fn add_installs_npx_packages_into_the_cache() {
    let dir = std::env::temp_dir().join(format!("mcpgw-pkg-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(tgz) = pack(&dir) else {
        eprintln!("add_installs_npx_packages_into_the_cache: skipped (no npm)");
        return;
    };
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\n",
            dir.join("db.sqlite"),
            dir.join("token")
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
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
    let tgz = tgz.to_string_lossy().into_owned();
    let added = run(&["add", "nf", "--command", "npx", "--", "-y", &tgz]);
    assert!(
        added.contains("installed node-fake-mcp@1.0.0 for nf (starts without npx)"),
        "{added}"
    );
    assert!(added.contains("added server nf (8 tools, pin "), "{added}");
    assert!(run(&["list"]).contains("[cached node-fake-mcp@1.0.0]"));
    let ls = run(&["runtimes", "ls"]);
    assert!(ls.starts_with("npm\tnode-fake-mcp@1.0.0\t"), "{ls}");
    assert!(
        dir.join("runtimes").join("npm").is_dir(),
        "cache next to the store"
    );
    let again = run(&["runtimes", "install", "nf"]);
    assert!(again.contains("installed node-fake-mcp@1.0.0"), "{again}");
    // --no-cache leaves it to npx
    let plain = run(&[
        "add",
        "nf2",
        "--no-cache",
        "--no-introspect",
        "--command",
        "npx",
        "--",
        "-y",
        "other-pkg",
    ]);
    assert!(!plain.contains("installed"), "{plain}");
    let _ = std::fs::remove_dir_all(&dir);
}
