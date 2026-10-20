//! M2-13: the audit log is a hash chain in the store. The CLI and the
//! daemon append to it, tool calls carry argument digests (never the
//! arguments), `export` writes JSONL, and changing any row is detected.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\n\
                 [[server]]\nname = \"fx\"\ntransport = \"stdio\"\ncommand = {:?}\n",
                dir.join("db.sqlite"),
                dir.join("token"),
                mcpgw_testkit::fake_mcp()
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "pw")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        if let Some(s) = stdin {
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), s.as_bytes()).unwrap();
        }
        drop(child.stdin.take());
        let o = child.wait_with_output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args, None);
        assert!(ok, "mcpgw {args:?}: {err}");
        out
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["down"], None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_chain_records_changes_and_calls_and_detects_tampering() {
    let e = Env::new();
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    let secret = format!("audit-secret-{}", "a".repeat(12));

    assert!(e.run(&["secret", "set", "k"], Some(&secret)).0);
    e.ok(&["add", "extra", "--command", &fake]);
    e.ok(&["up"]);
    e.ok(&["disable", "extra"]);
    e.ok(&["enable", "extra"]);

    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(e.dir.join("daemon.json")).unwrap()).unwrap();
    let token = e.ok(&["token"]);
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(token.trim()))
        .await
        .unwrap();
    c.call_text("fx__add", json!({"a": 1, "b": 2}))
        .await
        .unwrap();
    c.call_text("fx__echo", json!({"text": "private words"}))
        .await
        .unwrap();
    assert!(c.call("fx__nope", json!({})).await.is_err());
    c.close().await;

    assert!(
        e.ok(&["audit", "verify"])
            .starts_with("audit log OK: 7 entries")
    );
    let lines: Vec<Value> = e
        .ok(&["audit", "export"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<(String, String, String)> = lines
        .iter()
        .map(|l| {
            let s = |k: &str| l[k].as_str().unwrap().to_string();
            (s("kind"), s("actor"), s("subject"))
        })
        .collect();
    let k = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
    assert_eq!(
        kinds,
        [
            k("secret.set", "cli", "k"),
            k("server.add", "cli", "extra"),
            k("server.disable", "cli", "extra"),
            k("server.enable", "cli", "extra"),
            k(
                "tool.call",
                "the default endpoint / mcpgw-testkit",
                "fx__add"
            ),
            k(
                "tool.call",
                "the default endpoint / mcpgw-testkit",
                "fx__echo"
            ),
            k(
                "tool.call",
                "the default endpoint / mcpgw-testkit",
                "fx__nope"
            ),
        ],
        "one entry per change: the daemon doesn't duplicate CLI changes"
    );
    assert_eq!(lines[4]["detail"]["outcome"], "ok");
    assert_eq!(lines[6]["detail"]["outcome"], "error");
    assert_eq!(
        lines[4]["args_digest"],
        mcpgw_core::audit::args_digest(&json!({"b": 2, "a": 1}))
    );
    assert_eq!(lines[0]["seq"], 1);
    assert_eq!(lines[1]["prev_hash"], lines[0]["hash"]);
    let all = lines.iter().map(Value::to_string).collect::<String>();
    assert!(
        !all.contains("private words") && !all.contains(&secret),
        "arguments or secrets in the log"
    );
    assert_eq!(
        e.ok(&["audit", "export", "--after", "5"]).lines().count(),
        2
    );
    let tail = e.ok(&["audit", "tail", "-n", "2"]);
    assert!(
        tail.contains("fx__echo") && tail.contains("fx__nope") && !tail.contains("fx__add"),
        "{tail}"
    );

    // someone edits a row in the database file
    e.ok(&["down"]);
    let db = rusqlite::Connection::open(e.dir.join("db.sqlite")).unwrap();
    assert!(
        db.execute(
            "UPDATE audit SET subject = 'fx__harmless' WHERE seq = 6",
            []
        )
        .is_err(),
        "append-only"
    );
    db.execute_batch("DROP TRIGGER audit_no_update").unwrap();
    db.execute(
        "UPDATE audit SET subject = 'fx__harmless' WHERE seq = 6",
        [],
    )
    .unwrap();
    drop(db);
    let (ok, _, err) = e.run(&["audit", "verify"], None);
    assert!(!ok, "tampering not detected");
    assert!(
        err.contains("audit chain broken at entry 6: content does not match its hash"),
        "{err}"
    );
}
