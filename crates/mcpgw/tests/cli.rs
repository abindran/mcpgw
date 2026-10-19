//! CLI subcommands, run against the built binary with an isolated config.

use std::{path::PathBuf, process::Command};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = format!(
            "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\n\n\
             [[server]]\nname = \"fromcfg\"\ntransport = \"stdio\"\ncommand = {:?}\n\
             env = {{ FAKE_PROTOCOL = \"legacy\" }}\n",
            dir.join("db.sqlite"),
            dir.join("token"),
            fixture()
        );
        std::fs::write(dir.join("config.toml"), cfg).unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture() -> String {
    mcpgw_testkit::fake_mcp().to_string_lossy().into_owned()
}

#[test]
fn add_list_remove_servers() {
    let e = Env::new("crud");
    let (ok, out, err) = e.run(&[
        "add-server",
        "extra",
        "--command",
        &fixture(),
        "--env",
        "FAKE_PREFIX=x",
        "--",
        "-v",
    ]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "added server extra");
    assert!(
        e.run(&[
            "add-server",
            "web",
            "--url",
            "https://e.com/mcp",
            "--disabled"
        ])
        .0
    );

    let (ok, out, _) = e.run(&["list-servers"]);
    assert!(ok);
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].starts_with("extra\tstdio\tstore\t") && lines[0].contains("fake-mcp"));
    assert!(lines[0].ends_with(" -v"), "{}", lines[0]);
    assert!(lines[1].starts_with("fromcfg\tstdio\tconfig\t"));
    assert_eq!(lines[2], "web\thttp\tstore\thttps://e.com/mcp [disabled]");

    // config-defined names are protected; bad names are rejected
    let (ok, _, err) = e.run(&["add-server", "fromcfg", "--command", "x"]);
    assert!(!ok && err.contains("defined in"), "{err}");
    let (ok, _, err) = e.run(&["add-server", "a__b", "--command", "x"]);
    assert!(!ok && err.contains("__"), "{err}");

    assert!(e.run(&["remove-server", "web"]).0);
    let (ok, _, err) = e.run(&["remove-server", "web"]);
    assert!(!ok && err.contains("no such server"), "{err}");
}

#[test]
fn list_servers_check_starts_backends() {
    let e = Env::new("check");
    assert!(
        e.run(&["add-server", "broken", "--command", "/nonexistent/x"])
            .0
    );
    let (ok, out, err) = e.run(&["list-servers", "--check"]);
    assert!(ok, "{err}");
    assert!(
        out.contains("fromcfg\tstdio\tconfig") && out.contains("ok (7 tools, 2025-11-25)"),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("broken") && l.contains("error:")),
        "{out}"
    );
}

#[test]
fn token_create_and_rotate() {
    let e = Env::new("tok");
    let (ok, a, _) = e.run(&["token"]);
    assert!(ok);
    assert_eq!(a.trim().len(), 64);
    assert_eq!(e.run(&["token"]).1, a);
    let (ok, b, _) = e.run(&["token", "--rotate"]);
    assert!(ok && b != a);
}

#[test]
fn version_and_bad_config() {
    let out = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("mcpgw "));
    let e = Env::new("bad");
    std::fs::write(e.dir.join("config.toml"), "nonsense = 1\n").unwrap();
    let (ok, _, err) = e.run(&["list-servers"]);
    assert!(!ok && err.starts_with("mcpgw: "), "{err}");
}
