//! M2-01: `mcpgw auth connect` runs authorization code + PKCE against a
//! (fake) provider, with the daemon's loopback callback, and stores the
//! tokens encrypted in the vault.

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

use mcpgw_testkit::oauth::FakeOAuth;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str, extra: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-oauth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\n{extra}",
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

    /// Run `auth connect`, play the browser on the URL it prints, and
    /// return (exit ok, stdout, final page status, final page body).
    async fn connect(&self, args: &[&str]) -> (bool, String, u16, String) {
        let mut child = self
            .cmd(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut printed = String::new();
        let url = loop {
            let mut line = String::new();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "no URL printed: {printed}"
            );
            printed.push_str(&line);
            if let Some(u) = line.trim().strip_prefix("http") {
                break format!("http{u}");
            }
        };
        mcpgw_core::tls::install();
        let page = reqwest::get(&url).await.unwrap(); // follows the redirects
        let status = page.status().as_u16();
        let body = page.text().await.unwrap();
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
        let ok = child.wait().unwrap().success();
        (ok, printed + &rest, status, body)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["down"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_connect_stores_encrypted_tokens() {
    let idp = FakeOAuth::start(3600).await;
    let e = Env::new("connect", &idp.config("fake"));
    e.ok(&["up"]);

    let (ok, out, status, page) = e
        .connect(&[
            "auth",
            "connect",
            "fake",
            "--account",
            "work",
            "--no-browser",
        ])
        .await;
    assert!(ok, "{out}");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("connected to fake (work)"), "{page}");
    assert!(
        out.contains("Sign in to fake (work)") && out.contains("connected fake (work)"),
        "{out}"
    );
    assert_eq!(idp.last_challenge_method().as_deref(), Some("S256"));
    assert_eq!(idp.token_requests(), 1);

    // stored in the vault, encrypted
    let ls = e.ok(&["auth", "ls"]);
    assert!(
        ls.contains("fake\twork\texpires ") && ls.contains("refreshable"),
        "{ls}"
    );
    assert!(e.ok(&["secret", "ls"]).contains("oauth/fake@work"));
    let vault = std::fs::read_to_string(e.dir.join("vault.json")).unwrap();
    assert!(
        !vault.contains("at-") && !vault.contains("rt-"),
        "token in plaintext"
    );

    // a second account at the same provider
    let (ok, _, _, _) = e
        .connect(&["auth", "connect", "fake", "--no-browser"])
        .await;
    assert!(ok);
    assert!(e.ok(&["auth", "ls"]).contains("fake\tdefault"));

    e.ok(&["auth", "rm", "fake", "--account", "work"]);
    let ls = e.ok(&["auth", "ls"]);
    assert!(!ls.contains("work") && ls.contains("default"), "{ls}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_and_forged_logins_fail_clearly() {
    let idp = FakeOAuth::start(3600).await;
    let e = Env::new("refused", &idp.config("fake"));
    e.ok(&["up"]);

    idp.deny(true);
    let (ok, out, status, page) = e
        .connect(&["auth", "connect", "fake", "--no-browser"])
        .await;
    assert!(!ok, "{out}");
    assert_eq!(status, 400);
    assert!(
        page.contains("the provider refused: access_denied"),
        "{page}"
    );
    idp.deny(false);

    // a callback with a state the daemon never issued
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.dir.join("daemon.json")).unwrap()).unwrap();
    let base = info["url"]
        .as_str()
        .unwrap()
        .trim_end_matches("/mcp")
        .to_string();
    mcpgw_core::tls::install();
    let r = reqwest::get(format!("{base}/oauth/callback?state=forged&code=x"))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    assert!(r.text().await.unwrap().contains("unknown or expired"));

    // unknown provider
    let (ok, _, err) = e.run(&["auth", "connect", "nope", "--no-browser"]);
    assert!(
        !ok && err.contains("no OAuth app or remote server named \"nope\""),
        "{err}"
    );
    assert!(e.ok(&["auth", "ls"]).contains("no connected accounts"));
}

fn daemon_url(e: &Env) -> String {
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.dir.join("daemon.json")).unwrap()).unwrap();
    info["url"].as_str().unwrap().to_string()
}

async fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// M2-02: a token that is about to expire is refreshed in the background and
/// before a server gets it; a revoked grant shows `needs login` and the
/// server's next start says how to fix it.
#[tokio::test(flavor = "multi_thread")]
async fn tokens_refresh_and_revocation_needs_a_new_login() {
    let idp = FakeOAuth::start(1).await; // every token is due at once
    let e = Env::new(
        "refresh",
        &format!(
            "oauth_refresh_interval = \"200ms\"\n\n{}",
            idp.config("fake")
        ),
    );
    e.ok(&["up"]);
    let (ok, out, _, _) = e
        .connect(&["auth", "connect", "fake", "--no-browser"])
        .await;
    assert!(ok, "{out}");

    // background refresh
    assert!(
        wait_until(|| idp.refresh_requests() >= 2).await,
        "no background refresh"
    );
    let ls = e.ok(&["auth", "ls"]);
    assert!(
        ls.contains("fake\tdefault\texpires ") && !ls.contains("needs login"),
        "{ls}"
    );

    // a server bound to the connection gets a current token, redacted
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    e.ok(&[
        "add",
        "fx",
        "--command",
        &fake,
        "--secret",
        "GITHUB_TOKEN=oauth/fake@default",
    ]);
    let token = e.ok(&["token"]);
    let c = mcpgw_testkit::McpClient::http(&daemon_url(&e), Some(token.trim()))
        .await
        .unwrap();
    let r = c
        .call_text("fx__env", serde_json::json!({"name": "GITHUB_TOKEN"}))
        .await
        .unwrap();
    assert_eq!(r, "[REDACTED:oauth/fake@default]");

    // the provider revokes the grant: the next refresh fails for good
    idp.revoke_all();
    assert!(
        wait_until(|| e.ok(&["auth", "ls"]).contains("needs login")).await,
        "revocation not noticed"
    );
    let settled = idp.refresh_requests();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(
        idp.refresh_requests(),
        settled,
        "kept refreshing a dead grant"
    );

    // the running server keeps working; a fresh start needs the new login,
    // and the call answers with a sign-in link for it (M2-06)
    e.ok(&["restart", "fx"]);
    let r = c
        .call("fx__env", serde_json::json!({"name": "GITHUB_TOKEN"}))
        .await
        .unwrap();
    assert_eq!(r["isError"], true, "{r}");
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Sign in to fake to use this tool."),
        "{text}"
    );
    assert!(text.contains(&format!("{}/authorize?", idp.base)), "{text}");
    c.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn long_lived_tokens_are_not_refreshed_early() {
    let idp = FakeOAuth::start(3600).await;
    let e = Env::new(
        "noearly",
        &format!(
            "oauth_refresh_interval = \"200ms\"\n\n{}",
            idp.config("fake")
        ),
    );
    e.ok(&["up"]);
    let (ok, _, _, _) = e
        .connect(&["auth", "connect", "fake", "--no-browser"])
        .await;
    assert!(ok);
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    assert_eq!(idp.refresh_requests(), 0);
}
