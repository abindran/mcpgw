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
