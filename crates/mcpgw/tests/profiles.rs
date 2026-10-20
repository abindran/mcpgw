//! M2-07: profiles are endpoints with their own server set, mode and pinned
//! tools. Cursor on the frontend profile sees only its three servers.

use std::{collections::BTreeSet, path::PathBuf, process::Command};

use mcpgw_testkit::McpClient;
use serde_json::json;

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        Self::with(tag, "")
    }

    fn with(tag: &str, extra: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-profiles-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = mcpgw_testkit::fake_mcp();
        let mut cfg = format!(
            "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\n",
            dir.join("db.sqlite"),
            dir.join("token")
        );
        for s in ["fs", "gh", "figma", "db", "slack"] {
            cfg += &format!(
                "[[server]]\nname = \"{s}\"\ntransport = \"stdio\"\ncommand = {fake:?}\n\n"
            );
        }
        cfg += "[[profile]]\nname = \"frontend\"\nservers = [\"fs\", \"gh\", \"figma\"]\nmode = \"direct\"\n\n";
        cfg += "[[profile]]\nname = \"backend\"\nservers = [\"db\"]\nmode = \"hybrid\"\npinned_tools = [\"db__add\"]\n";
        cfg += extra;
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

    fn url(&self) -> String {
        let info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        info["url"].as_str().unwrap().to_string()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd(&["down"]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn servers_of(names: &[String]) -> BTreeSet<String> {
    names
        .iter()
        .filter_map(|n| n.split_once("__").map(|(s, _)| s.to_string()))
        .collect()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_scope_servers_mode_and_pinned_tools() {
    let e = Env::new("scope");
    e.ok(&["up"]);
    let token = e.ok(&["token"]);
    let token = token.trim();

    // default endpoint: everything
    let all = McpClient::http(&e.url(), Some(token)).await.unwrap();
    assert_eq!(
        servers_of(&all.tool_names().await.unwrap()),
        set(&["db", "figma", "fs", "gh", "slack"])
    );

    // Cursor's stdio entry on the frontend profile: three servers only
    let mut cursor = tokio::process::Command::from(e.cmd(&["connect", "--profile", "frontend"]));
    cursor.stderr(std::process::Stdio::null());
    let fe = McpClient::stdio(cursor).await.unwrap();
    let names = fe.tool_names().await.unwrap();
    assert_eq!(servers_of(&names), set(&["figma", "fs", "gh"]), "{names:?}");
    assert_eq!(
        fe.call_text("gh__add", json!({"a": 1, "b": 2}))
            .await
            .unwrap(),
        "3"
    );
    let err = fe
        .call("db__add", json!({"a": 1, "b": 2}))
        .await
        .unwrap_err();
    assert!(
        err.contains("tool not found: db__add"),
        "outside the profile: {err}"
    );

    // backend: hybrid, meta-tools plus its pinned tool; search sees only db
    let be = McpClient::http(&format!("{}/p/backend", e.url()), Some(token))
        .await
        .unwrap();
    assert_eq!(
        be.tool_names().await.unwrap(),
        ["search_tools", "describe_tool", "call_tool", "db__add"]
    );
    let r = be
        .call("search_tools", json!({"query": "add"}))
        .await
        .unwrap();
    let hits: Vec<String> = r["structuredContent"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(hits, ["db__add"]);
    let r = be
        .call(
            "call_tool",
            json!({"name": "fs__add", "arguments": {"a": 1, "b": 1}}),
        )
        .await
        .unwrap();
    assert_eq!(r["isError"], true, "fs is not in the backend profile");

    // an unknown profile is not silently the default endpoint
    assert!(
        McpClient::http(&format!("{}/p/nope", e.url()), Some(token))
            .await
            .is_err()
    );

    let list = e.ok(&["profiles"]);
    assert!(
        list.contains("frontend\thttp://127.0.0.1:0/mcp/p/frontend\tdirect\tfs, gh, figma"),
        "{list}"
    );
    assert!(
        list.contains("backend\t") && list.contains("\thybrid\tdb"),
        "{list}"
    );

    for c in [all, fe, be] {
        c.close().await;
    }
}

#[test]
fn client_install_points_at_a_profile() {
    let e = Env::new("install");
    let path = e.dir.join("cursor.json");
    e.ok(&[
        "client",
        "install",
        "cursor",
        "--path",
        path.to_str().unwrap(),
        "--profile",
        "frontend",
    ]);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args: Vec<&str> = v["mcpServers"]["mcpgw"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        &args[args.len() - 3..],
        ["connect", "--profile", "frontend"]
    );
}

#[test]
fn invalid_profiles_are_rejected() {
    let dir = std::env::temp_dir().join(format!("mcpgw-badprofile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("c.toml"), "[[profile]]\nname = \"a b\"\n").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--config")
        .arg(dir.join("c.toml"))
        .arg("profiles")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("profile name \"a b\" is invalid"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn text(r: &serde_json::Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// M2-08: a denied tool is hidden and, if called anyway, answers "disabled
/// by policy" instead of failing silently.
#[tokio::test(flavor = "multi_thread")]
async fn policy_denies_with_a_clear_message() {
    let e = Env::with(
        "policy",
        "\n[[profile]]\nname = \"safe\"\nservers = [\"gh\", \"fs\"]\nmode = \"direct\"\n\
         policy = { deny = [\"gh__crash\", \"*__spawn\"] }\n\
         \n[[profile]]\nname = \"narrow\"\nservers = [\"gh\"]\nmode = \"meta\"\n\
         policy = { allow = [\"gh__add\", \"gh__echo\"], deny = [\"gh__echo\"] }\n",
    );
    e.ok(&["up"]);
    let token = e.ok(&["token"]);
    let token = token.trim();

    let safe = McpClient::http(&format!("{}/p/safe", e.url()), Some(token))
        .await
        .unwrap();
    let names = safe.tool_names().await.unwrap();
    assert!(names.contains(&"gh__add".to_string()), "{names:?}");
    for hidden in ["gh__crash", "gh__spawn", "fs__spawn"] {
        assert!(
            !names.contains(&hidden.to_string()),
            "{hidden} listed: {names:?}"
        );
    }
    let r = safe.call("gh__crash", json!({})).await.unwrap();
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "Tool gh__crash is disabled by policy (profile safe denies `gh__crash`)."
    );
    let r = safe.call("fs__spawn", json!({})).await.unwrap();
    assert_eq!(
        text(&r),
        "Tool fs__spawn is disabled by policy (profile safe denies `*__spawn`)."
    );
    // the denied call never reached the server: it is still up
    assert_eq!(
        safe.call_text("gh__add", json!({"a": 2, "b": 2}))
            .await
            .unwrap(),
        "4"
    );

    // meta mode with an allow list: search, describe and call_tool all agree
    let narrow = McpClient::http(&format!("{}/p/narrow", e.url()), Some(token))
        .await
        .unwrap();
    let r = narrow
        .call("search_tools", json!({"query": "gh"}))
        .await
        .unwrap();
    let hits: Vec<&str> = r["structuredContent"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(hits, ["gh__add"]);
    let r = narrow
        .call("describe_tool", json!({"name": "gh__echo"}))
        .await
        .unwrap();
    assert_eq!(
        text(&r),
        "Tool gh__echo is disabled by policy (profile narrow denies `gh__echo`)."
    );
    let r = narrow
        .call(
            "call_tool",
            json!({"name": "gh__env", "arguments": {"name": "HOME"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        text(&r),
        "Tool gh__env is disabled by policy (profile narrow only allows gh__add, gh__echo)."
    );

    // the default endpoint has no policy
    let all = McpClient::http(&e.url(), Some(token)).await.unwrap();
    assert!(
        all.tool_names()
            .await
            .unwrap()
            .contains(&"gh__crash".to_string())
    );
    for c in [safe, narrow, all] {
        c.close().await;
    }
}
