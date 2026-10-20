//! M4-02: Wasm capability grants are deny-by-default. The probe component
//! (built from `wasm/probe-tool`) tries a TCP connect, a DNS lookup, file
//! reads and writes and an environment read; each works only when granted.

use std::{net::TcpListener, path::Path};

use mcpgw_wasm::{DirGrant, Grants};
use serde_json::{Map, Value, json};

fn probe() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe-tool.wasm")
}

async fn call(g: &Grants, tool: &str, args: Value) -> String {
    let tools = mcpgw_wasm::load(&probe()).unwrap();
    let r = tools.instantiate_with(g).await.unwrap();
    let a: Map<String, Value> = args.as_object().cloned().unwrap();
    let out = r.call_tool(tool, &a).await.unwrap();
    out["content"][0]["text"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn network_is_denied_without_a_grant() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = json!({"host": "127.0.0.1", "port": port});

    // done-when: no network grant, no socket
    let none = call(&Grants::default(), "connect", target.clone()).await;
    assert!(none.starts_with("denied:"), "{none}");
    assert_eq!(
        call(&Grants::default(), "lookup", json!({"host": "localhost"}))
            .await
            .split(':')
            .next(),
        Some("denied")
    );

    // granted exactly this address: it connects
    let this = Grants {
        connect: vec![format!("127.0.0.1:{port}").parse().unwrap()],
        ..Default::default()
    };
    assert_eq!(call(&this, "connect", target.clone()).await, "ok connected");
    // a grant for another port doesn't open this one
    let other = Grants {
        connect: vec!["127.0.0.1:9".parse().unwrap()],
        ..Default::default()
    };
    let r = call(&other, "connect", target.clone()).await;
    assert!(r.starts_with("denied:"), "{r}");
    // `*` allows any destination
    let any = Grants {
        any_network: true,
        ..Default::default()
    };
    assert_eq!(call(&any, "connect", target).await, "ok connected");
    // DNS only with a grant that asks for it
    let dns = Grants {
        any_network: true,
        dns: true,
        ..Default::default()
    };
    assert_eq!(
        call(&dns, "lookup", json!({"host": "localhost"})).await,
        "ok resolved"
    );
    drop(listener);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_env_only_when_granted() {
    let dir = std::env::temp_dir().join(format!("mcpgw-wasm-grants-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "hello from the host").unwrap();

    let none = Grants::default();
    assert_eq!(
        call(&none, "read", json!({"path": "/data/note.txt"})).await,
        "denied: no directories granted"
    );
    assert_eq!(
        call(&none, "env", json!({"name": "HOME"})).await,
        "denied: not set"
    );

    let ro = Grants {
        dirs: vec![DirGrant {
            host: dir.clone(),
            guest: "/data".into(),
            write: false,
        }],
        env: vec![("REGION".into(), "eu".into())],
        ..Default::default()
    };
    assert_eq!(
        call(&ro, "read", json!({"path": "/data/note.txt"})).await,
        "ok hello from the host"
    );
    let w = call(&ro, "write", json!({"path": "/data/new.txt"})).await;
    assert!(
        w.starts_with("denied:"),
        "read-only grant allowed a write: {w}"
    );
    assert!(!dir.join("new.txt").exists());
    let out = call(&ro, "read", json!({"path": "/etc/passwd"})).await;
    assert!(out.starts_with("denied:"), "{out}");
    assert_eq!(call(&ro, "env", json!({"name": "REGION"})).await, "ok eu");
    assert_eq!(
        call(&ro, "env", json!({"name": "PATH"})).await,
        "denied: not set",
        "the host's env never leaks in"
    );

    let rw = Grants {
        dirs: vec![DirGrant {
            host: dir.clone(),
            guest: "/data".into(),
            write: true,
        }],
        ..Default::default()
    };
    assert_eq!(
        call(&rw, "write", json!({"path": "/data/new.txt"})).await,
        "ok written"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("new.txt")).unwrap(),
        "written by wasm"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
