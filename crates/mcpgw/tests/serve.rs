//! End to end through the binary: an MCP client on `mcpgw serve`'s stdio,
//! a fake backend behind it, and the request log on stderr.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde_json::{Value, json};

#[test]
fn serve_lists_and_calls_and_logs_one_line_per_request() {
    let dir = std::env::temp_dir().join(format!("mcpgw-serve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = format!(
        "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\n\n\
         [[server]]\nname = \"fx\"\ntransport = \"stdio\"\ncommand = {:?}\n",
        dir.join("db.sqlite"),
        dir.join("token"),
        mcpgw_testkit::fake_mcp(),
    );
    std::fs::write(dir.join("config.toml"), cfg).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .args(["--config"])
        .arg(dir.join("config.toml"))
        .args(["--log-format", "json", "serve", "--stdio"])
        .env("MCPGW_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let (tx, rx) = mpsc::channel::<Value>();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str(&line) {
                let _ = tx.send(v);
            }
        }
    });
    let stderr = child.stderr.take().unwrap();
    let logs = std::thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut send = |v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let recv = |id: i64| loop {
        let v = rx.recv_timeout(Duration::from_secs(30)).expect("response");
        if v["id"] == id {
            return v;
        }
    };

    send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
    );
    assert_eq!(recv(1)["result"]["serverInfo"]["name"], "mcpgw");
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let list = recv(2);
    assert!(
        list["result"]["tools"].as_array().unwrap().len() >= 4,
        "{list}"
    );
    send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"fx__add","arguments":{"a":2,"b":3}}}));
    assert_eq!(recv(3)["result"]["content"][0]["text"], "5");
    send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fx__nope"}}));
    assert!(recv(4)["error"].is_object());

    drop(stdin); // client hangs up; the gateway exits
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");

    let requests: Vec<Value> = logs
        .join()
        .unwrap()
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["message"] == "request")
        .collect();
    let summary: Vec<(String, String, String)> = requests
        .iter()
        .map(|r| {
            assert!(r["duration_ms"].is_number(), "{r}");
            assert_eq!(r["span"]["request_id"].as_str().unwrap().len(), 16, "{r}");
            (
                r["span"]["method"].as_str().unwrap().into(),
                r["span"]["tool"].as_str().unwrap().into(),
                r["outcome"].as_str().unwrap().into(),
            )
        })
        .collect();
    let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
    assert_eq!(
        summary,
        [
            s("tools/list", "", "ok"),
            s("tools/call", "fx__add", "ok"),
            s("tools/call", "fx__nope", "error"),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `add-server` pins the tools once; afterwards the gateway lists them
/// without starting the server, and starts it only for a call.
#[tokio::test(flavor = "multi_thread")]
async fn tools_list_after_install_spawns_nothing() {
    let dir = std::env::temp_dir().join(format!("mcpgw-pinned-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("spawns.log");
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
        c.arg("--config").arg(dir.join("config.toml")).args(args);
        c
    };
    let spawns = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };

    let out = mcpgw(&[
        "add-server",
        "fx",
        "--command",
        &mcpgw_testkit::fake_mcp().to_string_lossy(),
        "--env",
        &format!("FAKE_SPAWN_LOG={}", log.display()),
    ])
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("(7 tools, pin "));
    assert_eq!(spawns(), 1, "introspection spawns once");

    let mut serve = tokio::process::Command::from(mcpgw(&["serve", "--stdio"]));
    serve.stderr(Stdio::null());
    let c = mcpgw_testkit::McpClient::stdio(serve).await.unwrap();
    let names = c.tool_names().await.unwrap();
    assert_eq!(names.len(), 7, "{names:?}");
    assert_eq!(spawns(), 1, "tools/list must not start the server");
    assert_eq!(
        c.call_text("fx__add", json!({"a": 1, "b": 1}))
            .await
            .unwrap(),
        "2"
    );
    assert_eq!(spawns(), 2, "the call starts it");
    c.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
