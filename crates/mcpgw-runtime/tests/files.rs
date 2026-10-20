//! M3-04: a service-account JSON is mounted into a container server as a
//! read-only file in tmpfs, readable by the sandboxed user, never on a
//! command line or in the container's config, and gone after the server
//! stops.
//!
//! Skipped without Docker for Linux containers, unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{sync::Arc, time::Duration};

use mcpgw_core::{Sandbox, Transport};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, SecretResolver, State};
use mcpgw_testkit::container::{containers_of, containers_or_skip, docker, node_server};
use secrecy::SecretString;
use serde_json::json;

fn sa_json() -> String {
    // built at runtime so the canary scan never sees a literal
    format!(
        "{{\"type\":\"service_account\",\"client_email\":\"bot@example.iam\",\"private_key\":\"-----BEGIN PRIVATE KEY-----{}\"}}",
        "k".repeat(24)
    )
}

struct Vault;

impl SecretResolver for Vault {
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String> {
        Ok((name == "gcp_sa").then(|| SecretString::from(sa_json())))
    }
}

async fn call(m: &BackendManager, server: &str, tool: &str, args: serde_json::Value) -> String {
    let r = m
        .call_tool(server, tool, args.as_object().cloned())
        .await
        .unwrap();
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn until(mut f: impl FnMut() -> bool, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Whether anything is left at the container's secrets path on the host.
fn secrets_left(container: &str) -> String {
    docker(&[
        "run",
        "--rm",
        "-v",
        "/dev/shm:/shm",
        mcpgw_runtime::container::EGRESS_IMAGE,
        "sh",
        "-c",
        &format!("ls -A /shm/mcpgw-secrets/*/{container} 2>/dev/null || true"),
    ])
}

#[tokio::test(flavor = "multi_thread")]
async fn service_account_file_in_tmpfs_gone_after_stop() {
    if !containers_or_skip("service_account_file_in_tmpfs_gone_after_stop") {
        return;
    }
    let name = format!("sa-{}", std::process::id());
    let mut spec = node_server(&name);
    if let Transport::Container { env, sandbox, .. } = &mut spec.transport {
        env.insert(
            "GOOGLE_APPLICATION_CREDENTIALS".into(),
            "/run/secrets/sa.json".into(),
        );
        *sandbox = Sandbox::Strict;
    }
    spec.files
        .insert("/run/secrets/sa.json".into(), "gcp_sa".into());
    let m = BackendManager::with_secrets(
        &[spec],
        ManagerOptions {
            connect_timeout: Duration::from_secs(60),
            health_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(3),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
        Some(Arc::new(Vault)),
    );

    // the file is there, readable by the strict sandbox's nobody user (its
    // contents are redacted on the way back, which proves they match)
    assert_eq!(call(&m, &name, "uid", json!({})).await, "65534");
    let path = call(
        &m,
        &name,
        "env",
        json!({"name": "GOOGLE_APPLICATION_CREDENTIALS"}),
    )
    .await;
    assert_eq!(path, "/run/secrets/sa.json");
    assert_eq!(
        call(&m, &name, "read", json!({"path": &path})).await,
        "[REDACTED:gcp_sa]"
    );
    // in tmpfs, read-only
    let mount = call(&m, &name, "mount", json!({"path": &path})).await;
    let fields: Vec<&str> = mount.split(' ').collect();
    assert_eq!(fields.get(2), Some(&"tmpfs"), "{mount}");
    assert!(
        fields
            .get(3)
            .is_some_and(|o| o.split(',').any(|x| x == "ro")),
        "{mount}"
    );
    assert_eq!(
        call(&m, &name, "write", json!({"path": &path})).await,
        "EROFS"
    );

    // the value is nowhere in the container's configuration
    let ctr = m.status_of(&name).unwrap().container.unwrap().name;
    let inspect = docker(&["inspect", &ctr]);
    assert!(
        !inspect.contains("PRIVATE KEY") && !inspect.contains(&"k".repeat(24)),
        "secret in inspect output"
    );
    assert!(inspect.contains("/dev/shm/mcpgw-secrets/"), "bind source");
    assert!(
        !secrets_left(&ctr).is_empty(),
        "the file should exist while running"
    );

    // idle stop: the container and the file are gone
    assert!(until(|| m.status_of(&name).unwrap().state == State::Cold, 30).await);
    assert!(until(|| containers_of(&name).is_empty(), 20).await);
    assert_eq!(secrets_left(&ctr), "", "secret file left after stop");

    // a fresh start gets a fresh file; shutdown removes it too
    assert_eq!(
        call(&m, &name, "read", json!({"path": &path})).await,
        "[REDACTED:gcp_sa]"
    );
    let second = m.status_of(&name).unwrap().container.unwrap().name;
    m.shutdown().await;
    assert!(until(|| containers_of(&name).is_empty(), 20).await);
    assert_eq!(secrets_left(&second), "");
}
