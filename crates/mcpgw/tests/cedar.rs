//! M4-07 end to end: `mcpgw policy convert` writes Cedar from a profile's
//! glob rules; the profile then runs on the Cedar file with the same
//! decisions, through the daemon (listing, refusals, approvals), and a
//! broken file fails closed and is reported.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::{ClientKind, McpClient};
use serde_json::json;

struct Env {
    dir: PathBuf,
}

const TOOLS: &[&str] = &[
    "gh__add",
    "gh__echo",
    "gh__crash",
    "gh__sleep",
    "gh__env",
    "gh__spawn",
    "gh__stderr",
];

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-cedar-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("policies")).unwrap();
        let e = Self { dir };
        e.config(
            "policy = { allow = [\"gh__add\", \"gh__e*\", \"gh__s*\"], deny = [\"gh__echo\", \"*__spawn\"], approve = [\"gh__env\"] }",
            "policy = { deny = [\"gh__crash\"] }",
        );
        e
    }

    /// The config with these `policy` lines for profile `safe` and the
    /// default endpoint.
    fn config(&self, safe: &str, gateway: &str) {
        let cfg = format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\
             approval_timeout = \"20s\"\n{gateway}\n\n\
             [[server]]\nname = \"gh\"\ntransport = \"stdio\"\ncommand = {:?}\n\n\
             [[profile]]\nname = \"safe\"\nservers = [\"gh\"]\nmode = \"direct\"\n{safe}\n\n\
             [[profile]]\nname = \"broken\"\nservers = [\"gh\"]\nmode = \"direct\"\n\
             policy = {{ cedar = \"policies/broken.cedar\" }}\n",
            self.dir.join("db.sqlite"),
            self.dir.join("token"),
            mcpgw_testkit::fake_mcp(),
        );
        std::fs::write(self.dir.join("config.toml"), cfg).unwrap();
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

    /// `mcpgw policy check` decisions, without the header line.
    fn decisions(&self, profile: Option<&str>) -> Vec<String> {
        let mut args = vec!["policy", "check"];
        if let Some(p) = profile {
            args.extend(["--profile", p]);
        }
        args.extend(TOOLS);
        self.ok(&args).lines().skip(1).map(String::from).collect()
    }

    async fn client(&self, path: &str, approve: Option<bool>) -> McpClient {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        let token = self.ok(&["token"]);
        let kind = ClientKind {
            approve,
            ..Default::default()
        };
        let url = format!("{}{path}", info["url"].as_str().unwrap());
        McpClient::http_kind(&url, Some(token.trim()), kind, false)
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
async fn glob_rules_convert_to_cedar_with_the_same_decisions() {
    let e = Env::new();
    std::fs::write(
        e.dir.join("policies/broken.cedar"),
        "permit (principal, action, resource) when { resource.nmae == \"gh__add\" };",
    )
    .unwrap();
    let globs = e.decisions(Some("safe"));
    let gateway_globs = e.decisions(None);
    assert_eq!(
        globs,
        [
            "gh__add\tallow",
            "gh__echo\tdeny (gh__echo)",
            "gh__crash\tdeny (nothing allows it)",
            "gh__sleep\tallow",
            "gh__env\tapprove (gh__env)",
            "gh__spawn\tdeny (*__spawn)",
            "gh__stderr\tallow",
        ]
    );

    // convert both endpoints, then serve them from the Cedar files
    let safe_file = e.dir.join("policies/safe.cedar");
    let wrote = e.ok(&[
        "policy",
        "convert",
        "--profile",
        "safe",
        "-o",
        safe_file.to_str().unwrap(),
    ]);
    assert!(
        wrote.contains("profile safe's policy = { cedar = "),
        "{wrote}"
    );
    let gateway_cedar = e.ok(&["policy", "convert"]);
    assert!(gateway_cedar.contains("forbid"), "{gateway_cedar}");
    std::fs::write(e.dir.join("policies/default.cedar"), &gateway_cedar).unwrap();
    // relative to the config file
    e.config(
        "policy = { cedar = \"policies/safe.cedar\" }",
        "policy = { cedar = \"policies/default.cedar\" }",
    );
    let check = e.ok(&["policy", "check", "--profile", "safe"]);
    assert!(
        check.contains("Cedar policies load and validate"),
        "{check}"
    );
    assert_eq!(e.decisions(Some("safe")), globs);
    assert_eq!(e.decisions(None), gateway_globs);

    // a broken file is an error for `check` and `doctor`
    let o = e
        .cmd(&["policy", "check", "--profile", "broken"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("nmae"), "{err}");
    let doctor = e.cmd(&["doctor", "--json"]).output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let cedar: Vec<_> = report
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == "policy_cedar")
        .map(|c| (c["subject"].as_str().unwrap(), c["level"].as_str().unwrap()))
        .collect();
    assert_eq!(
        cedar,
        [
            ("gateway policy", "ok"),
            ("profile safe", "ok"),
            ("profile broken", "fail")
        ],
        "{report}"
    );

    // through the daemon
    e.ok(&["up"]);
    let safe = e.client("/p/safe", Some(true)).await;
    let mut names = safe.tool_names().await.unwrap();
    names.sort();
    assert_eq!(names, ["gh__add", "gh__env", "gh__sleep", "gh__stderr"]);
    let r = safe.call("gh__echo", json!({"text": "x"})).await.unwrap();
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "Tool gh__echo is disabled by policy (profile safe denies `gh__echo`)."
    );
    let r = safe.call("gh__crash", json!({})).await.unwrap();
    assert_eq!(
        text(&r),
        "Tool gh__crash is disabled by policy (no Cedar policy of profile safe permits it)."
    );
    assert_eq!(
        safe.call_text("gh__add", json!({"a": 2, "b": 3}))
            .await
            .unwrap(),
        "5"
    );
    // @approve: the client is asked first
    safe.call_text("gh__env", json!({"name": "MCPGW_NOPE"}))
        .await
        .unwrap();
    let asked = safe.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].starts_with("Allow gh__env on profile safe?"),
        "{asked:?}"
    );

    // the default endpoint runs on its own converted file
    let all = e.client("", None).await;
    let names = all.tool_names().await.unwrap();
    assert!(
        names.contains(&"gh__echo".to_string()) && !names.contains(&"gh__crash".to_string()),
        "{names:?}"
    );
    let r = all.call("gh__crash", json!({})).await.unwrap();
    assert_eq!(
        text(&r),
        "Tool gh__crash is disabled by policy (the default endpoint denies `gh__crash`)."
    );

    // a broken file fails closed: nothing is listed or callable
    let broken = e.client("/p/broken", None).await;
    assert!(broken.tool_names().await.unwrap().is_empty());
    let r = broken
        .call("gh__add", json!({"a": 1, "b": 1}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("(not loaded)"), "{}", text(&r));
}
