//! M4-05 end to end: `mcpgw add NAME --openapi SPEC` serves an OpenAPI
//! API's operations as tools through the daemon, from a spec file (with
//! --base-url) or straight from the API's own spec URL.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::{McpClient, petstore::FakePetstore};
use serde_json::json;

struct Env(PathBuf);

impl Env {
    fn ok(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.0.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "{args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.0.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn petstore_through_the_daemon() {
    let fake = FakePetstore::start().await;
    let dir = std::env::temp_dir().join(format!("mcpgw-openapi-e2e-{}", std::process::id()));
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
    let e = Env(dir.clone());
    let spec = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mcpgw-openapi/tests/fixtures/petstore.yaml");

    // from a file, with the base URL given
    let added = e.ok(&[
        "add",
        "pets",
        "--openapi",
        spec.to_str().unwrap(),
        "--base-url",
        &fake.base,
    ]);
    assert!(
        added.contains("added server pets (19 tools, pin "),
        "{added}"
    );
    // from the API's own spec URL: its relative server URL resolves against it
    let url = format!("{}/openapi.json", fake.base);
    let added = e.ok(&["add", "pets2", "--openapi", &url]);
    assert!(added.contains("added server pets2 (19 tools"), "{added}");
    let list = e.ok(&["list"]);
    assert!(
        list.contains("pets\topenapi\tstore\t") && list.contains(&format!("→ {}", fake.base)),
        "{list}"
    );

    e.ok(&["up"]);
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).unwrap()).unwrap();
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(info["url"].as_str().unwrap(), Some(&token))
        .await
        .unwrap();
    let tools = c.tool_names().await.unwrap();
    assert!(
        tools.contains(&"pets__addPet".to_string())
            && tools.contains(&"pets2__getPetById".to_string())
    );
    let added = c
        .call_text(
            "pets__addPet",
            json!({"body": {"name": "nemo", "photoUrls": [], "status": "available"}}),
        )
        .await
        .unwrap();
    assert!(added.contains("nemo"), "{added}");
    assert!(
        c.call_text("pets2__getPetById", json!({"petId": 1}))
            .await
            .unwrap()
            .contains("nemo")
    );
    let missing = c
        .call("pets__getPetById", json!({"petId": 404}))
        .await
        .unwrap();
    assert_eq!(missing["isError"], true);
    // arguments that don't fit the schema are refused before any request
    let bad = c
        .call_raw("pets__getPetById", json!({"petId": "x"}))
        .await
        .unwrap_err();
    assert!(bad.to_string().contains("schema"), "{bad}");
}

/// M4-06: `--auth SCHEME=SECRET` binds vault secrets to the spec's security
/// schemes; each operation gets the credential its scheme asks for.
#[tokio::test(flavor = "multi_thread")]
async fn security_schemes_from_the_vault() {
    use std::process::Stdio;
    let fake = FakePetstore::start().await;
    let dir = std::env::temp_dir().join(format!("mcpgw-openapi-auth-{}", std::process::id()));
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
    let mcpgw = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "pw");
        c
    };
    let key = format!("pet-key-{}", "k".repeat(12));
    let token = format!("pet-token-{}", "t".repeat(12));
    for (name, value) in [("petstore_key", &key), ("petstore_token", &token)] {
        let mut set = mcpgw(&["secret", "set", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut set.stdin.take().unwrap(), value.as_bytes()).unwrap();
        assert!(set.wait().unwrap().success());
    }
    let spec = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mcpgw-openapi/tests/fixtures/petstore.yaml");
    let out = mcpgw(&[
        "add",
        "pets",
        "--openapi",
        spec.to_str().unwrap(),
        "--base-url",
        &fake.base,
        "--auth",
        "api_key=petstore_key",
        "--auth",
        "petstore_auth=petstore_token",
    ])
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // a username without its scheme's secret is refused
    let bad = mcpgw(&[
        "add",
        "pets2",
        "--openapi",
        spec.to_str().unwrap(),
        "--auth-username",
        "basic=bob",
    ])
    .output()
    .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("needs --auth basic="));
    // a scheme the spec doesn't have stops the server's start
    let typo = mcpgw(&[
        "add",
        "pets3",
        "--openapi",
        spec.to_str().unwrap(),
        "--base-url",
        &fake.base,
        "--auth",
        "apikey=petstore_key",
    ])
    .output()
    .unwrap();
    assert!(typo.status.success());
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(
        stderr.contains("no security scheme \"apikey\" (it has: api_key, petstore_auth)"),
        "{stderr}"
    );
    assert!(
        mcpgw(&["remove", "pets3"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let db = std::fs::read(dir.join("db.sqlite")).unwrap();
    let db = String::from_utf8_lossy(&db);
    assert!(
        !db.contains(&key) && !db.contains(&token),
        "secret stored in plaintext"
    );

    let mut serve = tokio::process::Command::from(mcpgw(&["serve", "--stdio"]));
    serve.stderr(Stdio::null());
    let c = McpClient::stdio(serve).await.unwrap();
    // api_key: a header named by the scheme
    let inv = c.call_text("pets__getInventory", json!({})).await.unwrap();
    assert!(inv.contains("available"), "{inv}");
    // oauth2: a bearer token
    c.call_text(
        "pets__addPet",
        json!({"body": {"name": "nemo", "photoUrls": []}}),
    )
    .await
    .unwrap();
    // an operation without security gets neither
    c.call_text("pets__logoutUser", json!({})).await.unwrap();
    let seen = fake.seen();
    let header = |path: &str, name: &str| {
        seen.iter()
            .find(|s| s.uri.contains(path))
            .unwrap_or_else(|| panic!("no request to {path}: {seen:?}"))
            .headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("/store/inventory", "api_key"), Some(key.clone()));
    assert_eq!(header("/store/inventory", "authorization"), None);
    assert_eq!(
        header("/pet", "authorization"),
        Some(format!("Bearer {token}"))
    );
    assert_eq!(header("/pet", "api_key"), None);
    assert_eq!(header("/user/logout", "api_key"), None);
    assert_eq!(header("/user/logout", "authorization"), None);
    c.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
