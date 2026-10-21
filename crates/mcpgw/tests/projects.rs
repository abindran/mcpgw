//! Project servers (`.mcpgw.toml`): a project's endpoint serves its own
//! servers with the global ones, only once its file's content is trusted;
//! its servers win a name clash, other endpoints never see them, an edit
//! turns them off until it's trusted again, trust survives a restart, and
//! `mcpgw connect` finds the project from its working directory.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
    base: String,
    token: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-projects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        // a global `echo`, plain
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\n\
                 [[server]]\nname = \"echo\"\ntransport = \"stdio\"\ncommand = {:?}\n",
                dir.join("db.sqlite"),
                dir.join("token"),
                mcpgw_testkit::fake_mcp()
            ),
        )
        .unwrap();
        Self {
            dir,
            base: String::new(),
            token: String::new(),
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "pw")
            .env("HOME", &self.dir);
        c
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.cmd(args).current_dir(cwd).output().unwrap();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.dir, args)
    }

    fn up(&mut self) {
        self.ok(&["up"]);
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        self.base = info["url"]
            .as_str()
            .unwrap()
            .trim_end_matches("/mcp")
            .to_string();
        self.token = self.ok(&["token"]).trim().to_string();
    }

    /// A project directory with `.mcpgw.toml`: a `local` server and its
    /// own `echo` that prefixes its answers.
    fn project(&self, name: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(
            p.join(".mcpgw.toml"),
            format!(
                "[project]\nname = {name:?}\n\n\
                 [[server]]\nname = \"local\"\ntransport = \"stdio\"\ncommand = {fake:?}\n\n\
                 [[server]]\nname = \"echo\"\ntransport = \"stdio\"\ncommand = {fake:?}\nenv = {{ FAKE_PREFIX = \"{name}:\" }}\n",
                fake = mcpgw_testkit::fake_mcp()
            ),
        )
        .unwrap();
        p.canonicalize().unwrap()
    }

    async fn register(&self, root: &Path) -> Value {
        mcpgw_core::tls::install();
        let r = reqwest::Client::new()
            .post(format!("{}/api/v1/projects", self.base))
            .bearer_auth(&self.token)
            .json(&json!({ "path": root }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.status());
        r.json().await.unwrap()
    }

    async fn client(&self, url: &str) -> McpClient {
        McpClient::http(url, Some(&self.token)).await.unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd(&["down"]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn project_servers_run_only_while_trusted() {
    let mut e = Env::new("trust");
    e.up();
    let shop = e.project("shop");
    let p = e.register(&shop).await;
    assert_eq!(p["state"], "untrusted", "{p}");
    let url = p["url"].as_str().unwrap().to_string();

    // untrusted: the global tools and the notice, none of the project's
    let c = e.client(&url).await;
    let names = c.tool_names().await.unwrap();
    assert!(
        names.contains(&"project_not_trusted".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"echo__echo".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("local__")), "{names:?}");
    let r = c.call("project_not_trusted", json!({})).await.unwrap();
    assert_eq!(r["isError"], true);
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("mcpgw project trust") && text.contains("2 server(s)"),
        "{text}"
    );

    // trusting from the CLI (through the daemon) turns them on at once
    let out = e.ok_in(&shop.join("src"), &["project", "trust", "--yes"]);
    assert!(
        out.contains("not trusted yet") && out.contains("- local: runs "),
        "{out}"
    );
    assert!(out.contains("trusted shop"), "{out}");
    let c = e.client(&url).await;
    let names = c.tool_names().await.unwrap();
    assert!(
        !names.contains(&"project_not_trusted".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"local__add".to_string()), "{names:?}");
    // the project's `echo` wins over the global one here...
    assert_eq!(
        c.call_text("echo__echo", json!({"text": "hi"}))
            .await
            .unwrap(),
        "shop:hi"
    );
    assert_eq!(
        c.call_text("local__add", json!({"a": 2, "b": 3}))
            .await
            .unwrap(),
        "5"
    );
    assert_eq!(
        names.iter().filter(|n| *n == "echo__echo").count(),
        1,
        "{names:?}"
    );
    // ...and the default endpoint never sees the project's servers
    let d = e.client(&format!("{}/mcp", e.base)).await;
    assert_eq!(
        d.call_text("echo__echo", json!({"text": "hi"}))
            .await
            .unwrap(),
        "hi"
    );
    assert!(
        !d.tool_names()
            .await
            .unwrap()
            .iter()
            .any(|n| n.starts_with("local__"))
    );
    assert!(
        !e.ok(&["list"]).contains("local"),
        "project servers aren't global"
    );

    // a second project sees its own servers, not shop's
    let cafe = e.project("cafe");
    e.ok_in(&cafe, &["project", "trust", "--yes"]);
    let q = e.register(&cafe).await;
    assert_eq!(q["state"], "trusted");
    let c2 = e.client(q["url"].as_str().unwrap()).await;
    assert_eq!(
        c2.call_text("echo__echo", json!({"text": "hi"}))
            .await
            .unwrap(),
        "cafe:hi"
    );
    let ls = e.ok(&["project", "ls"]);
    assert!(
        ls.contains("shop\ttrusted\t2 servers") && ls.contains("cafe\ttrusted"),
        "{ls}"
    );

    // an edit turns shop's servers off until it's trusted again
    let file = shop.join(".mcpgw.toml");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n# edited\n");
    std::fs::write(&file, &text).unwrap();
    let c = e.client(&url).await;
    let names = c.tool_names().await.unwrap();
    assert!(
        names.contains(&"project_not_trusted".to_string()),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.starts_with("local__")), "{names:?}");
    let r = c.call("project_not_trusted", json!({})).await.unwrap();
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("changed since it was trusted")
    );
    assert!(e.ok(&["project", "ls"]).contains("shop\tchanged"));
    let doctor = e
        .cmd(&["doctor", "--json"])
        .current_dir(&shop)
        .output()
        .unwrap();
    let checks: Vec<Value> = serde_json::from_slice(&doctor.stdout).unwrap();
    assert!(
        checks.iter().any(|c| c["id"] == "project_changed"),
        "{checks:#?}"
    );
    e.ok_in(&shop, &["project", "trust", "--yes"]);
    let c = e.client(&url).await;
    assert!(
        c.tool_names()
            .await
            .unwrap()
            .contains(&"local__add".to_string())
    );

    // trusting an edit that changes a server's tools pins them afresh
    // instead of quarantining it
    let text = std::fs::read_to_string(&file).unwrap().replacen(
        "name = \"local\"\ntransport = \"stdio\"\n",
        "name = \"local\"\ntransport = \"stdio\"\nenv = { FAKE_DESC_SUFFIX = \" (v2)\" }\n",
        1,
    );
    std::fs::write(&file, text).unwrap();
    e.ok_in(&shop, &["project", "trust", "--yes"]);
    let c = e.client(&url).await;
    let tools = c.tools().await.unwrap();
    let add = tools.iter().find(|t| t["name"] == "local__add").unwrap();
    assert!(
        add["description"].as_str().unwrap().ends_with("(v2)"),
        "{add}"
    );
    assert_eq!(
        c.call_text("local__add", json!({"a": 1, "b": 1}))
            .await
            .unwrap(),
        "2"
    );

    // trust survives a restart: the endpoint works before anyone registers
    e.ok(&["down"]);
    e.up();
    // (a new port: listen = 127.0.0.1:0)
    let url = format!("{}/mcp/project/{}", e.base, p["id"].as_str().unwrap());
    let c = e.client(&url).await;
    assert_eq!(
        c.call_text("echo__echo", json!({"text": "back"}))
            .await
            .unwrap(),
        "shop:back"
    );

    // untrusting turns them off again
    let out = e.ok_in(&shop, &["project", "untrust"]);
    assert!(out.contains("no longer trusted"), "{out}");
    let c = e.client(&url).await;
    assert!(
        c.tool_names()
            .await
            .unwrap()
            .contains(&"project_not_trusted".to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_serves_the_project_it_runs_in() {
    let mut e = Env::new("connect");
    e.up();
    let shop = e.project("shop");
    e.ok_in(&shop, &["project", "trust", "--yes"]);

    // a client starting `mcpgw connect` inside the project gets its servers
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcpgw"));
    cmd.arg("--config")
        .arg(e.dir.join("config.toml"))
        .arg("connect")
        .current_dir(shop.join("src"))
        .env("MCPGW_VAULT_PASSPHRASE", "pw")
        .env("HOME", &e.dir);
    let c = McpClient::stdio(cmd).await.unwrap();
    let names = sorted(c.tool_names().await.unwrap());
    assert!(names.contains(&"local__add".to_string()), "{names:?}");
    assert_eq!(
        c.call_text("echo__echo", json!({"text": "x"}))
            .await
            .unwrap(),
        "shop:x"
    );
    c.close().await;

    // --no-project serves the default endpoint
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcpgw"));
    cmd.arg("--config")
        .arg(e.dir.join("config.toml"))
        .args(["connect", "--no-project"])
        .current_dir(&shop)
        .env("HOME", &e.dir);
    let c = McpClient::stdio(cmd).await.unwrap();
    assert!(
        !c.tool_names()
            .await
            .unwrap()
            .iter()
            .any(|n| n.starts_with("local__"))
    );
    c.close().await;

    // --project names it from anywhere
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcpgw"));
    cmd.arg("--config")
        .arg(e.dir.join("config.toml"))
        .args(["connect", "--project"])
        .arg(&shop)
        .current_dir(&e.dir)
        .env("HOME", &e.dir);
    let c = McpClient::stdio(cmd).await.unwrap();
    assert!(
        c.tool_names()
            .await
            .unwrap()
            .contains(&"local__add".to_string())
    );
    c.close().await;
}

#[test]
fn init_writes_the_file_and_client_entries() {
    let e = Env::new("init");
    let p = e.dir.join("app");
    std::fs::create_dir_all(p.join(".vscode")).unwrap();
    std::fs::write(
        p.join(".vscode/mcp.json"),
        r#"{"servers": {"other": {"type": "stdio", "command": "x"}}}"#,
    )
    .unwrap();
    let out = e.ok_in(
        &p,
        &[
            "project",
            "init",
            "--client",
            "claude-code",
            "--client",
            "vscode",
        ],
    );
    assert!(out.contains("wrote") && out.contains(".mcp.json"), "{out}");
    let text = std::fs::read_to_string(p.join(".mcpgw.toml")).unwrap();
    assert!(text.contains("name = \"app\""), "{text}");
    // the template loads (no servers yet) and isn't trusted
    let ls = e.ok_in(&p, &["project", "trust", "--yes"]);
    assert!(ls.contains("no servers"), "{ls}");
    let claude: Value =
        serde_json::from_str(&std::fs::read_to_string(p.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        claude["mcpServers"]["mcpgw"],
        json!({"command": "mcpgw", "args": ["connect"]})
    );
    let code: Value =
        serde_json::from_str(&std::fs::read_to_string(p.join(".vscode/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(code["servers"]["other"]["command"], "x", "kept its server");
    assert_eq!(
        code["servers"]["mcpgw"]["args"],
        json!(["connect", "--project", "${workspaceFolder}"])
    );
    // a second init leaves the file alone
    std::fs::write(p.join(".mcpgw.toml"), "# mine\n").unwrap();
    assert!(e.ok_in(&p, &["project", "init"]).contains("exists"));
    assert_eq!(
        std::fs::read_to_string(p.join(".mcpgw.toml")).unwrap(),
        "# mine\n"
    );
    // trusting without a terminal needs --yes
    let o = e
        .cmd(&["project", "trust"])
        .current_dir(&p)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
}
