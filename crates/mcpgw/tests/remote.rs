//! M2-04 end to end: `mcpgw add --url … --header` with a vault secret, then
//! the remote server's tools through the gateway.

use std::process::{Command, Stdio};

use mcpgw_testkit::{HttpFake, McpClient};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn add_remote_server_with_a_secret_header() {
    let dir = std::env::temp_dir().join(format!("mcpgw-remote-e2e-{}", std::process::id()));
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
    let key = format!("remote-{}", "k".repeat(16));
    let fake = HttpFake::start(&[("FAKE_REQUIRE_HEADER", &format!("x-api-key={key}"))]);
    let mcpgw = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "pw");
        c
    };
    let mut set = mcpgw(&["secret", "set", "api_key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut set.stdin.take().unwrap(), key.as_bytes()).unwrap();
    assert!(set.wait().unwrap().success());

    let out = mcpgw(&[
        "add",
        "api",
        "--url",
        &fake.url,
        "--header",
        "x-api-key=${secret:api_key}",
    ])
    .output()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.starts_with("added server api (7 tools, pin "),
        "{stdout}"
    );
    let list = String::from_utf8_lossy(&mcpgw(&["list"]).output().unwrap().stdout).into_owned();
    assert!(
        list.contains(&format!("api\thttp\tstore\t{}", fake.url)),
        "{list}"
    );
    let db = std::fs::read(dir.join("db.sqlite")).unwrap();
    assert!(
        !String::from_utf8_lossy(&db).contains(&key),
        "key stored in plaintext"
    );

    let mut serve = tokio::process::Command::from(mcpgw(&["serve", "--stdio"]));
    serve.stderr(Stdio::null());
    let c = McpClient::stdio(serve).await.unwrap();
    assert_eq!(
        c.call_text("api__add", json!({"a": 20, "b": 22}))
            .await
            .unwrap(),
        "42"
    );
    c.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
