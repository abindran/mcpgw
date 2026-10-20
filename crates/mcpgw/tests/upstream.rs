//! M2-05: a remote MCP server protected by OAuth. mcpgw discovers its
//! authorization server, registers itself, runs the login, and injects the
//! token; the client never holds it.

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

use mcpgw_testkit::{HttpFake, McpClient, oauth::FakeOAuth};
use serde_json::json;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-upstream-{tag}-{}", std::process::id()));
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
            .env("MCPGW_VAULT_PASSPHRASE", "pw");
        c
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let o = self.cmd(args).output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(ok, "mcpgw {args:?}: {err}");
        out
    }

    /// `auth connect`, with the test playing the browser.
    async fn login(&self, args: &[&str]) -> (bool, String) {
        let mut child = self
            .cmd(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut printed = String::new();
        loop {
            let mut line = String::new();
            if out.read_line(&mut line).unwrap() == 0 {
                break;
            }
            printed.push_str(&line);
            if let Some(u) = line.trim().strip_prefix("http") {
                mcpgw_core::tls::install();
                let page = reqwest::get(format!("http{u}")).await.unwrap();
                printed.push_str(&page.text().await.unwrap());
            }
        }
        let mut err = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
        (child.wait().unwrap().success(), printed + &err)
    }

    async fn client(&self) -> McpClient {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        let token = self.ok(&["token"]);
        McpClient::http(info["url"].as_str().unwrap(), Some(token.trim()))
            .await
            .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["down"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_protected_remote_server_connects_with_tokens_held_by_mcpgw() {
    let idp = FakeOAuth::start(3600).await;
    let mcp = HttpFake::start(&[("FAKE_AUTH_SERVER", &idp.base)]);
    let e = Env::new("connect");
    e.ok(&["up"]);

    // install: the server needs a login, and says how
    let (ok, out, err) = e.run(&["add", "atl", "--url", &mcp.url]);
    assert!(ok, "{err}");
    assert!(out.contains("added server atl"), "{out}");
    assert!(err.contains("mcpgw auth connect atl"), "{err}");

    // a call before logging in fails with the same hint
    let c = e.client().await;
    let err = c
        .call("atl__add", json!({"a": 1, "b": 2}))
        .await
        .unwrap_err();
    assert!(err.contains("mcpgw auth connect atl"), "{err}");

    // login: discovery, dynamic registration, PKCE flow with resource
    let (ok, out) = e.login(&["auth", "connect", "atl", "--no-browser"]).await;
    assert!(ok, "{out}");
    assert!(out.contains("connected atl (default)"), "{out}");
    let regs = idp.registrations();
    assert_eq!(regs.len(), 1, "{regs:?}");
    assert_eq!(regs[0]["client_name"], "mcpgw");
    assert!(
        regs[0]["redirect_uris"][0]
            .as_str()
            .unwrap()
            .ends_with("/oauth/callback")
    );
    assert_eq!(idp.last_client_id().as_deref(), Some("dcr-client-1"));
    assert!(
        idp.resources().contains(&format!("authorize:{}", mcp.url)),
        "{:?}",
        idp.resources()
    );
    assert!(
        idp.resources().contains(&format!("token:{}", mcp.url)),
        "{:?}",
        idp.resources()
    );

    // the server now works through mcpgw; the token stays in mcpgw's vault
    let mut answer = Err(String::new());
    for _ in 0..40 {
        answer = c.call_text("atl__add", json!({"a": 2, "b": 2})).await;
        if answer.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // backoff after the failed start
    }
    assert_eq!(answer.unwrap(), "4");
    assert!(
        c.tool_names()
            .await
            .unwrap()
            .contains(&"atl__add".to_string())
    );
    assert!(e.ok(&["auth", "ls"]).contains("atl\tdefault"));
    let vault = std::fs::read_to_string(e.dir.join("vault.json")).unwrap();
    assert!(
        !vault.contains("at-") && !vault.contains("dcr-client"),
        "plaintext in the vault"
    );
    c.close().await;

    // after a daemon restart the registration is reused
    e.ok(&["down"]);
    e.ok(&["up"]);
    let c = e.client().await;
    assert_eq!(
        c.call_text("atl__add", json!({"a": 3, "b": 3}))
            .await
            .unwrap(),
        "6"
    );
    assert_eq!(idp.registrations().len(), 1, "registered again");
    c.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_metadata_document_is_used_when_supported() {
    let idp = FakeOAuth::start(3600).await;
    idp.support_client_metadata_documents(true);
    let mcp = HttpFake::start(&[("FAKE_AUTH_SERVER", &idp.base)]);
    let e = Env::new("cimd");
    let cfg = std::fs::read_to_string(e.dir.join("config.toml")).unwrap();
    std::fs::write(
        e.dir.join("config.toml"),
        cfg.replace(
            "[gateway]\n",
            "[gateway]\nclient_metadata_url = \"https://example.org/mcpgw/client.json\"\n",
        ),
    )
    .unwrap();
    e.ok(&["up"]);
    e.run(&["add", "atl", "--url", &mcp.url]);
    let (ok, out) = e.login(&["auth", "connect", "atl", "--no-browser"]).await;
    assert!(ok, "{out}");
    assert!(
        idp.registrations().is_empty(),
        "registered although a metadata document was usable"
    );
    assert_eq!(
        idp.last_client_id().as_deref(),
        Some("https://example.org/mcpgw/client.json")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_without_oauth_metadata_say_so() {
    let mcp = HttpFake::start(&[("FAKE_REQUIRE_HEADER", "x-api-key=nope")]);
    let e = Env::new("nometa");
    e.ok(&["up"]);
    e.run(&["add", "plain", "--url", &mcp.url]);
    let (ok, out) = e.login(&["auth", "connect", "plain", "--no-browser"]).await;
    assert!(!ok);
    assert!(
        out.contains("publishes no OAuth protected resource metadata"),
        "{out}"
    );
}
