//! M1-13: every CLI command has help text and works against a real daemon.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use mcpgw_testkit::McpClient;
use serde_json::json;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-cmd-{tag}-{}", std::process::id()));
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
        Self { dir }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "test pass");
        c
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        out(self.cmd(args).output().unwrap())
    }

    fn run_stdin(&self, args: &[&str], input: &str) -> (bool, String, String) {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        out(child.wait_with_output().unwrap())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, stdout, stderr) = self.run(args);
        assert!(ok, "mcpgw {args:?} failed: {stderr}");
        stdout
    }

    fn url(&self) -> String {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        info["url"].as_str().unwrap().to_string()
    }
}

fn out(o: Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["down"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn every_command_has_help() {
    let e = Env::new("help");
    for (cmd, needle) in [
        (vec!["serve"], "foreground"),
        (vec!["up"], "background"),
        (vec!["down"], "Stop"),
        (vec!["status"], "state"),
        (vec!["connect"], "stdin"),
        (vec!["add"], "pin its tools"),
        (vec!["add"], "MCP Registry"),
        (vec!["registry", "search"], "description"),
        (vec!["registry", "show"], "packages"),
        (vec!["remove"], "Remove"),
        (vec!["enable"], "Enable"),
        (vec!["disable"], "hidden"),
        (vec!["list"], "configured"),
        (vec!["logs"], "stderr"),
        (vec!["pin"], "quarantined"),
        (vec!["restart"], "failed"),
        (vec!["secret", "set"], "hidden"),
        (vec!["secret", "ls"], "never values"),
        (vec!["secret", "rm"], "Delete"),
        (vec!["token"], "token"),
    ] {
        let mut args = cmd.clone();
        args.push("--help");
        let help = e.ok(&args);
        assert!(
            help.contains(needle),
            "`mcpgw {}` help lacks {needle:?}:\n{help}",
            cmd.join(" ")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_server_and_secret_commands() {
    let e = Env::new("flow");
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    assert!(e.ok(&["status"]).contains("daemon: not running"));

    // secrets go into the vault, never onto the screen
    let value = format!("canary-{}", "v".repeat(20));
    let (ok, stdout, stderr) = e.run_stdin(
        &["secret", "set", "gh_token", "--description", "GitHub PAT"],
        &format!("{value}\n"),
    );
    assert!(ok, "{stderr}");
    assert_eq!(stdout.trim(), "stored secret gh_token (vault: passphrase)");
    let ls = e.ok(&["secret", "ls"]);
    assert!(
        ls.contains("gh_token") && ls.contains("GitHub PAT") && !ls.contains(&value),
        "{ls}"
    );

    // a server bound to the secret is introspected with it injected
    let added = e.ok(&[
        "add",
        "fx",
        "--command",
        &fake,
        "--secret",
        "GITHUB_TOKEN=gh_token",
        "--env",
        "FAKE_STDERR=fx booted",
    ]);
    assert!(
        added.starts_with("added server fx (7 tools, pin "),
        "{added}"
    );

    // up / status
    assert!(e.ok(&["up"]).starts_with("daemon started (pid "));
    assert!(e.ok(&["up"]).starts_with("daemon already running"));
    let st = e.ok(&["status"]);
    assert!(st.starts_with("daemon: running (pid "), "{st}");
    assert!(st.contains("fx\tcold\t7 tools\t-"), "{st}");

    // use the server through the daemon; the bound secret stays redacted
    let token = e.ok(&["token"]);
    let c = McpClient::http(&e.url(), Some(token.trim())).await.unwrap();
    let r = c
        .call_text("fx__env", json!({"name": "GITHUB_TOKEN"}))
        .await
        .unwrap();
    assert_eq!(r, "[REDACTED:gh_token]");
    assert!(e.ok(&["status"]).contains("fx\tidle\t7 tools\tpid "));
    assert!(e.ok(&["logs", "fx"]).contains("fx booted"));
    assert!(e.ok(&["logs"]).contains("listening on"), "daemon log");

    // add while running: the daemon picks it up without a restart
    let added = e.ok(&["add", "extra", "--command", &fake]);
    assert!(
        added.contains("the running daemon now serves extra"),
        "{added}"
    );
    assert!(
        c.tool_names()
            .await
            .unwrap()
            .contains(&"extra__add".to_string())
    );

    // disable / enable, live: a 2026 client caches tools/list for its
    // ttlMs, and the list_changed it subscribed to refreshes it (M2-10)
    let mut changes = c.listen_tools().await.unwrap();
    e.ok(&["disable", "extra"]);
    assert!(e.ok(&["status"]).contains("extra\tdisabled"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), changes.next())
            .await
            .unwrap()
    );
    assert!(
        !c.tool_names()
            .await
            .unwrap()
            .iter()
            .any(|n| n.starts_with("extra__"))
    );
    e.ok(&["enable", "extra"]);
    assert!(e.ok(&["status"]).contains("extra\tcold"));
    assert!(e.ok(&["list"]).contains("extra\tstdio\tstore"));

    // pin is for quarantined servers; restart clears and stops
    let (ok, _, err) = e.run(&["pin", "fx"]);
    assert!(!ok && err.contains("not quarantined"), "{err}");
    assert!(e.ok(&["restart", "fx"]).contains("restarted fx"));

    // remove, while running
    e.ok(&["remove", "extra"]);
    assert!(!e.ok(&["status"]).contains("extra"));
    c.close().await;

    e.ok(&["secret", "rm", "gh_token"]);
    assert!(e.ok(&["secret", "ls"]).contains("no secrets"));
    let (ok, _, err) = e.run(&["secret", "rm", "gh_token"]);
    assert!(!ok && err.contains("no such secret"));

    assert!(e.ok(&["down"]).starts_with("daemon stopped (pid "));
    assert!(e.ok(&["status"]).contains("daemon: not running"));
    assert!(e.ok(&["down"]).contains("not running"));
}

#[test]
fn errors_name_the_problem() {
    let e = Env::new("errors");
    let (ok, _, err) = e.run(&["enable", "ghost"]);
    assert!(!ok && err.contains("no such server: ghost"), "{err}");
    let (ok, _, err) = e.run(&["restart", "ghost"]);
    assert!(!ok && err.contains("daemon is not running"), "{err}");
    let (ok, _, err) = e.run(&["logs", "ghost"]);
    assert!(!ok && err.contains("daemon is not running"), "{err}");
    let (ok, _, err) = e.run_stdin(&["secret", "set", "x"], "");
    assert!(!ok && err.contains("empty value"), "{err}");
    let (ok, _, err) = e.run_stdin(&["secret", "set", "bad name"], "v");
    assert!(!ok && err.contains("secret name"), "{err}");
}
