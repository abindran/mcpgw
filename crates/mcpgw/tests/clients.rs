//! M1-14: `mcpgw client import` moves a client's servers into mcpgw and
//! leaves one gateway entry; `client install` adds the entry.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-clients-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\n",
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

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read(p: &PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn import_moves_servers_and_credentials_and_writes_one_entry() {
    let e = Env::new("import");
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    let secret = format!("ghp_{}", "x".repeat(30));
    let claude = e.file("claude.json");
    let original = json!({
        "numStartups": 42,
        "projects": {"/w": {"allowedTools": []}},
        "mcpServers": {
            "github tools": {"command": fake, "env": {"GITHUB_TOKEN": secret, "LOG": "1"}},
            "fs": {"command": fake, "args": ["-v"]},
            "remote": {"type": "http", "url": "https://example.com/mcp", "headers": {"Authorization": "Bearer abc123def"}},
            "mcpgw": {"command": "old-mcpgw", "args": ["connect"]}
        }
    });
    std::fs::write(&claude, serde_json::to_string_pretty(&original).unwrap()).unwrap();

    let dry = e.ok(&[
        "client",
        "import",
        "claude-code",
        "--path",
        claude.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(
        dry.contains(
            "github tools -> github-tools [stdio] (secrets to vault: github_tools_github_token)"
        ),
        "{dry}"
    );
    assert!(dry.contains("dry run: nothing changed"));
    assert_eq!(read(&claude), original, "dry run wrote the file");

    let out = e.ok(&[
        "client",
        "import",
        "claude-code",
        "--path",
        claude.to_str().unwrap(),
    ]);
    assert!(out.contains("imported 3 server(s)"), "{out}");

    // the client file keeps its other keys and has exactly one server
    let after = read(&claude);
    assert_eq!(after["numStartups"], 42);
    assert_eq!(after["projects"], original["projects"]);
    let servers = after["mcpServers"].as_object().unwrap();
    assert_eq!(servers.keys().collect::<Vec<_>>(), ["mcpgw"]);
    let args = servers["mcpgw"]["args"].as_array().unwrap();
    assert_eq!(args.last().unwrap(), "connect");
    assert_eq!(args[0], "--config");
    assert!(
        servers["mcpgw"]["command"]
            .as_str()
            .unwrap()
            .contains("mcpgw")
    );
    assert!(!std::fs::read_to_string(&claude).unwrap().contains(&secret));

    // the original is backed up
    let backup = std::fs::read_dir(&e.dir)
        .unwrap()
        .filter_map(|d| d.ok())
        .find(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with("claude.json.mcpgw-backup-")
        })
        .expect("backup file");
    assert_eq!(read(&backup.path()), original);

    // servers are in mcpgw; credentials in the vault, bound by name
    let list = e.ok(&["list"]);
    for n in ["github-tools\tstdio", "fs\tstdio", "remote\thttp"] {
        assert!(list.contains(n), "{n} missing:\n{list}");
    }
    let secrets = e.ok(&["secret", "ls"]);
    assert!(
        secrets.contains("github_tools_github_token") && secrets.contains("remote_authorization"),
        "{secrets}"
    );
    let db = std::fs::read(e.dir.join("db.sqlite")).unwrap();
    assert!(
        !String::from_utf8_lossy(&db).contains(&secret),
        "credential left in the store"
    );

    // the imported server works, gets its token, and never returns it
    let mut serve = tokio::process::Command::from(e.cmd(&["serve", "--stdio"]));
    serve.stderr(std::process::Stdio::null());
    let c = McpClient::stdio(serve).await.unwrap();
    let r = c
        .call_text("github-tools__env", json!({"name": "GITHUB_TOKEN"}))
        .await
        .unwrap();
    assert_eq!(r, "[REDACTED:github_tools_github_token]");
    c.close().await;

    // importing again finds nothing left to move
    let again = e.ok(&[
        "client",
        "import",
        "claude-code",
        "--path",
        claude.to_str().unwrap(),
    ]);
    assert!(again.contains("no servers to import"), "{again}");
}

#[test]
fn import_reads_vscode_jsonc() {
    let e = Env::new("vscode");
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    let path = e.file("mcp.json");
    std::fs::write(
        &path,
        format!(
            "{{\n  // VS Code allows comments\n  \"servers\": {{\n    \"tools\": {{ \"type\": \"stdio\", \"command\": {fake:?}, }},\n  }},\n  \"inputs\": []\n}}\n"
        ),
    )
    .unwrap();
    let out = e.ok(&[
        "client",
        "import",
        "vscode",
        "--path",
        path.to_str().unwrap(),
    ]);
    assert!(out.contains("tools -> tools [stdio]"), "{out}");
    let after = read(&path);
    assert_eq!(after["servers"]["mcpgw"]["type"], "stdio");
    assert_eq!(after["inputs"], json!([]));
    assert!(e.ok(&["list"]).contains("tools\tstdio\tstore"));
}

#[test]
fn install_adds_the_entry_and_keeps_other_servers() {
    let e = Env::new("install");
    let path = e.file("cursor-mcp.json");
    std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "x"}}}"#).unwrap();
    let dry = e.ok(&[
        "client",
        "install",
        "cursor",
        "--path",
        path.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(dry.contains("\"mcpgw\""), "{dry}");
    assert!(
        !read(&path)["mcpServers"]
            .as_object()
            .unwrap()
            .contains_key("mcpgw")
    );

    let out = e.ok(&[
        "client",
        "install",
        "cursor",
        "--path",
        path.to_str().unwrap(),
    ]);
    assert!(out.contains("installed the mcpgw entry"), "{out}");
    let servers = read(&path)["mcpServers"].as_object().unwrap().clone();
    assert!(servers.contains_key("other") && servers.contains_key("mcpgw"));

    // a client with no config yet gets a new file
    let fresh = e.file("new/desktop.json");
    e.ok(&[
        "client",
        "install",
        "claude-desktop",
        "--path",
        fresh.to_str().unwrap(),
    ]);
    assert!(read(&fresh)["mcpServers"]["mcpgw"]["args"].is_array());
}
