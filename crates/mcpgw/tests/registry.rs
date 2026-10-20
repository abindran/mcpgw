//! M2-14: `mcpgw add <registry name>` installs a pinned entry from the MCP
//! Registry, shows its tools and asks before adding.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use mcpgw_testkit::{HttpFake, fake_mcp, registry::FakeRegistry};
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str, registry: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-reg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\nregistry_url = {registry:?}\n",
                dir.join("db.sqlite"),
                dir.join("token")
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str], input: &str) -> (bool, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "test pass")
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

    fn ok(&self, args: &[&str], input: &str) -> String {
        let (ok, stdout, stderr) = self.run(args, input);
        assert!(ok, "mcpgw {args:?} failed: {stderr}\n{stdout}");
        stdout
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn out(o: Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn tools(names: &[&str]) -> String {
    let t: Vec<Value> = names
        .iter()
        .map(|n| json!({"name": n, "description": format!("{n} things.\nMore detail.")}))
        .collect();
    serde_json::to_string(&t).unwrap()
}

/// An npm entry run by fake-mcp (its runtimeHint) instead of npx.
fn npm_entry(version: &str, tool_names: &[&str]) -> Value {
    json!({
        "name": "io.example/notes",
        "title": "Notes",
        "description": "Keeps notes.",
        "version": version,
        "packages": [{
            "registryType": "npm",
            "identifier": "@example/notes-mcp",
            "version": version,
            "runtimeHint": fake_mcp().to_string_lossy(),
            "transport": {"type": "stdio"},
            "environmentVariables": [
                {"name": "FAKE_TOOLS", "default": tools(tool_names)}
            ]
        }]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_a_pinned_entry_after_showing_its_tools() {
    let reg = FakeRegistry::start(vec![
        npm_entry("1.0.0", &["read_note"]),
        npm_entry("1.1.0", &["read_note", "search_notes"]),
    ])
    .await;
    let e = Env::new("npm", &reg.base);

    let found = e.ok(&["registry", "search", "notes"], "");
    assert!(
        found.contains("io.example/notes\t1.1.0\tnpm\tNotes"),
        "{found}"
    );
    let shown = e.ok(&["registry", "show", "io.example/notes"], "");
    assert!(
        shown.contains("package: npm @example/notes-mcp 1.1.0"),
        "{shown}"
    );
    assert!(
        shown.contains("install: mcpgw add io.example/notes@1.1.0"),
        "{shown}"
    );

    // declining adds nothing
    let s = e.ok(&["add", "io.example/notes"], "n\n");
    assert!(
        s.contains("io.example/notes 1.1.0 via npm @example/notes-mcp@1.1.0"),
        "{s}"
    );
    assert!(s.contains("notes offers 2 tools:"), "{s}");
    assert!(s.contains("  search_notes  search_notes things."), "{s}");
    assert!(s.contains("not added: notes"), "{s}");
    assert!(e.ok(&["list"], "").contains("no servers configured"));

    // a pinned older version, under another name, approved on stdin
    let s = e.ok(
        &["add", "io.example/notes@1.0.0", "--as", "old-notes"],
        "y\n",
    );
    assert!(
        s.contains("old-notes offers 1 tools:\n  read_note  read_note things."),
        "{s}"
    );
    assert!(s.contains("added server old-notes (1 tools, pin "), "{s}");
    let list = e.ok(&["list"], "");
    assert!(
        list.contains("-y @example/notes-mcp@1.0.0 [io.example/notes@1.0.0]"),
        "{list}"
    );
    assert!(
        reg.requests()
            .iter()
            .any(|r| r == "/v0/servers/io.example%2Fnotes/versions/1.0.0"),
        "{:?}",
        reg.requests()
    );

    // --yes skips the question
    let s = e.ok(&["add", "io.example/notes", "-y"], "");
    assert!(s.contains("added server notes (2 tools"), "{s}");

    // unknown entries and bare names are explained
    let (ok, _, err) = e.run(&["add", "io.example/missing", "-y"], "");
    assert!(
        !ok && err.contains("io.example/missing: not found in the registry"),
        "{err}"
    );
    let (ok, _, err) = e.run(&["add", "plain"], "");
    assert!(
        !ok && err.contains("give --command, --url or --image"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_entry_needs_its_secret_header() {
    let token = "s3cret-registry-token";
    let fake = HttpFake::start(&[(
        "FAKE_REQUIRE_HEADER",
        &format!("Authorization=Bearer {token}"),
    )]);
    let reg = FakeRegistry::start(vec![json!({
        "name": "io.example/hosted",
        "version": "2026.10.1",
        "remotes": [{
            "type": "streamable-http",
            "url": fake.url,
            "headers": [{"name": "Authorization", "isSecret": true, "isRequired": true,
                         "description": "API token"}]
        }]
    })])
    .await;
    let e = Env::new("remote", &reg.base);

    let (ok, _, err) = e.run(&["add", "io.example/hosted", "-y"], "");
    assert!(
        !ok && err.contains("needs the secret hosted_authorization (API token)")
            && err.contains("mcpgw secret set hosted_authorization"),
        "{err}"
    );
    e.ok(
        &["secret", "set", "hosted_authorization"],
        &format!("{token}\n"),
    );
    let s = e.ok(&["add", "io.example/hosted", "-y"], "");
    assert!(s.contains(&format!("via remote {}", fake.url)), "{s}");
    assert!(s.contains("added server hosted ("), "{s}");
    assert!(!s.contains(token), "{s}");
    let list = e.ok(&["list"], "");
    assert!(list.contains("[io.example/hosted@2026.10.1]"), "{list}");
}
