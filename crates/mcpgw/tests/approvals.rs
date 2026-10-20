//! M2-09: an `approve`-listed tool waits for a human, through MRTR on
//! 2026-07-28 clients, session elicitation on 2025-11-25 clients, or
//! `mcpgw approve` when the client can't be asked.

use std::{path::PathBuf, process::Command, time::Duration};

use mcpgw_testkit::{ClientKind, McpClient};
use serde_json::json;

const TOOLS: &str =
    r#"[{"name": "merge_pull_request", "reply": "merged"}, {"name": "list_prs", "reply": "prs"}]"#;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("mcpgw-approvals-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\
             approval_timeout = \"20s\"\npolicy = {{ approve = [\"gh__merge_*\"] }}\n\n\
             [[server]]\nname = \"gh\"\ntransport = \"stdio\"\ncommand = {:?}\n\
             env = {{ FAKE_TOOLS = {:?}, FAKE_CALL_LOG = {:?} }}\n",
            dir.join("db.sqlite"),
            dir.join("token"),
            mcpgw_testkit::fake_mcp(),
            TOOLS,
            dir.join("calls.log"),
        );
        std::fs::write(dir.join("config.toml"), cfg).unwrap();
        Self { dir }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args);
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn merges(&self) -> usize {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "merge_pull_request")
            .count()
    }

    async fn client(&self, approve: Option<bool>, legacy: bool) -> McpClient {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        let token = self.ok(&["token"]);
        let kind = ClientKind {
            approve,
            ..Default::default()
        };
        McpClient::http_kind(
            info["url"].as_str().unwrap(),
            Some(token.trim()),
            kind,
            legacy,
        )
        .await
        .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd(&["down"]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(r: &serde_json::Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_waits_for_approval_in_2026_and_2025_clients() {
    let e = Env::new("clients");
    e.ok(&["up"]);
    let pr = json!({"repo": "acme/web", "pr": 7});

    // 2026-07-28 client: MRTR input request, answered, then the retry runs
    let modern = e.client(Some(true), false).await;
    assert_eq!(modern.protocol_version(), "2026-07-28");
    assert_eq!(
        modern
            .call_text("gh__merge_pull_request", pr.clone())
            .await
            .unwrap(),
        "merged"
    );
    let asked = modern.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].starts_with("Allow gh__merge_pull_request on the default endpoint?"),
        "{asked:?}"
    );
    assert!(asked[0].contains(r#""pr":7"#), "{asked:?}");
    assert_eq!(e.merges(), 1);

    // 2025-11-25 client: elicitation/create on its session
    let legacy = e.client(Some(true), true).await;
    assert_eq!(legacy.protocol_version(), "2025-11-25");
    assert_eq!(
        legacy
            .call_text("gh__merge_pull_request", pr.clone())
            .await
            .unwrap(),
        "merged"
    );
    assert_eq!(legacy.asked().len(), 1);
    assert_eq!(e.merges(), 2);

    // a user who says no: the tool never runs
    for (approve, legacy) in [(false, false), (false, true)] {
        let c = e.client(Some(approve), legacy).await;
        let r = c.call("gh__merge_pull_request", pr.clone()).await.unwrap();
        assert_eq!(r["isError"], true, "{r}");
        assert!(
            text(&r).contains("gh__merge_pull_request was not approved"),
            "{r}"
        );
        c.close().await;
    }
    assert_eq!(e.merges(), 2);

    // tools not on the approve list don't ask
    assert_eq!(
        modern.call_text("gh__list_prs", json!({})).await.unwrap(),
        "prs"
    );
    assert_eq!(modern.asked().len(), 1);
    modern.close().await;
    legacy.close().await;
}

/// A client that can't be asked waits for `mcpgw approve` / `mcpgw deny`.
#[tokio::test(flavor = "multi_thread")]
async fn cli_fallback_approves_and_denies() {
    let e = Env::new("cli");
    e.ok(&["up"]);
    let c = std::sync::Arc::new(e.client(None, false).await);

    for (verb, expect) in [("approve", "merged"), ("deny", "was not approved")] {
        let call = {
            let c = c.clone();
            tokio::spawn(async move { c.call("gh__merge_pull_request", json!({"pr": 9})).await })
        };
        let mut id = None;
        for _ in 0..100 {
            let list = e.ok(&["approvals"]);
            if let Some(line) = list
                .lines()
                .find(|l| l.contains("\tpending\tgh__merge_pull_request\t"))
            {
                assert!(
                    line.contains("the default endpoint") && line.contains(r#"{"pr":9}"#),
                    "{line}"
                );
                id = Some(line.split('\t').next().unwrap().to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let id = id.expect("approval never listed");
        assert!(e.ok(&[verb, &id]).contains(&id));
        let r = call.await.unwrap().unwrap();
        assert!(text(&r).contains(expect), "{verb}: {r}");
    }
    assert_eq!(e.merges(), 1);
    assert!(e.ok(&["approvals"]).contains("no calls waiting"));
    let o = e.cmd(&["approve", "nope"]).output().unwrap();
    assert!(
        !o.status.success()
            && String::from_utf8_lossy(&o.stderr).contains("no pending approval nope")
    );
}
