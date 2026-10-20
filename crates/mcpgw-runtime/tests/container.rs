//! M3-01: container servers (Docker) start on their first call, run the
//! pinned image, get env and secrets, and are removed when idle, on
//! shutdown, after a failed start, and by the startup sweep.
//!
//! Skipped without Docker for Linux containers, unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{sync::Arc, time::Duration};

use mcpgw_core::Transport;
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, SecretResolver, State};
use mcpgw_testkit::container::{
    IMAGE, containers_in, containers_of, containers_or_skip, docker, node_server, podman_available,
};
use secrecy::SecretString;
use serde_json::json;

struct Keys;

impl SecretResolver for Keys {
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String> {
        Ok((name == "ctr_token").then(|| SecretString::from(format!("tok-{}", "c".repeat(12)))))
    }
}

fn manager(spec: mcpgw_core::ServerSpec, idle: Duration) -> BackendManager {
    BackendManager::with_secrets(
        &[spec],
        ManagerOptions {
            // a cold image pull is done before the handshake; this covers
            // container start on a loaded machine
            connect_timeout: Duration::from_secs(60),
            health_interval: Duration::from_secs(60),
            idle_timeout: idle,
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
        Some(Arc::new(Keys)),
    )
}

fn text(r: &serde_json::Map<String, serde_json::Value>) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn args(v: serde_json::Value) -> Option<serde_json::Map<String, serde_json::Value>> {
    v.as_object().cloned()
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

fn tag(base: &str) -> String {
    format!("{base}-{}", std::process::id())
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_on_first_call_and_is_removed_when_idle() {
    if !containers_or_skip("starts_on_first_call_and_is_removed_when_idle") {
        return;
    }
    let name = tag("ctr-idle");
    let mut spec = node_server(&name);
    if let Transport::Container { env, .. } = &mut spec.transport {
        env.insert("GREETING".into(), "hello from the host".into());
    }
    spec.secrets.insert("API_TOKEN".into(), "ctr_token".into());
    let m = manager(spec, Duration::from_secs(2));

    // nothing runs until the first call
    assert_eq!(m.status_of(&name).unwrap().state, State::Cold);
    assert!(containers_of(&name).is_empty());
    let r = m
        .call_tool(&name, "echo", args(json!({"text": "hi"})))
        .await
        .unwrap();
    assert_eq!(text(&r), "hi");
    let st = m.status_of(&name).unwrap();
    let c = st.container.clone().expect("container info");
    assert_eq!((c.runtime.as_str(), c.image.as_str()), ("docker", IMAGE));
    assert!(c.name.starts_with(&format!("mcpgw-{name}-")), "{c:?}");
    assert_eq!(containers_of(&name), std::slice::from_ref(&c.name));
    assert_eq!(
        docker(&[
            "inspect",
            "--format",
            "{{index .Config.Labels \"mcpgw.managed\"}}",
            &c.name
        ]),
        "1"
    );

    // env and injected secrets reach the container (and the secret's value
    // is redacted from what comes back)
    let env = |n: &str| {
        let m = &m;
        let name = name.clone();
        let n = n.to_string();
        async move {
            text(
                &m.call_tool(&name, "env", args(json!({"name": n})))
                    .await
                    .unwrap(),
            )
        }
    };
    assert_eq!(env("GREETING").await, "hello from the host");
    assert_eq!(env("API_TOKEN").await, "[REDACTED:ctr_token]");
    assert_eq!(env("NOT_SET").await, "");

    // its stderr is captured like a local server's
    assert!(
        until(
            || m.stderr_tail(&name)
                .iter()
                .any(|l| l.contains("node-fake ready")),
            5
        )
        .await,
        "{:?}",
        m.stderr_tail(&name)
    );

    // idle: stopped and the container removed
    assert!(
        until(|| m.status_of(&name).unwrap().state == State::Cold, 20).await,
        "not stopped when idle"
    );
    assert!(
        until(|| containers_of(&name).is_empty(), 20).await,
        "{:?}",
        containers_of(&name)
    );
    assert!(m.status_of(&name).unwrap().container.is_none());

    // the next call starts a fresh container
    let r = m
        .call_tool(&name, "echo", args(json!({"text": "again"})))
        .await
        .unwrap();
    assert_eq!(text(&r), "again");
    let second = m.status_of(&name).unwrap().container.unwrap().name;
    assert_ne!(second, c.name);

    // shutdown removes it too
    m.shutdown().await;
    assert!(
        until(|| containers_of(&name).is_empty(), 20).await,
        "{:?}",
        containers_of(&name)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_digest_failed_start_and_sweep() {
    if !containers_or_skip("pinned_digest_failed_start_and_sweep") {
        return;
    }
    // pull by digest: the tag resolves to the image's registry digest
    let digest = mcpgw_runtime::container::resolve_digest("docker", IMAGE)
        .await
        .unwrap()
        .expect("registry digest");
    assert!(
        digest.starts_with("sha256:") && digest.len() == 71,
        "{digest}"
    );
    let name = tag("ctr-pinned");
    let mut spec = node_server(&name);
    if let Transport::Container { digest: d, .. } = &mut spec.transport {
        *d = Some(digest.clone());
    }
    let m = manager(spec, Duration::from_secs(60));
    let r = m
        .call_tool(&name, "echo", args(json!({"text": "pinned"})))
        .await
        .unwrap();
    assert_eq!(text(&r), "pinned");
    let c = m.status_of(&name).unwrap().container.unwrap();
    assert_eq!(c.image, format!("{IMAGE}@{digest}"));
    m.shutdown().await;
    assert!(until(|| containers_of(&name).is_empty(), 20).await);

    // a container that exits before the handshake leaves nothing behind
    let bad = tag("ctr-bad");
    let mut spec = node_server(&bad);
    if let Transport::Container { args, .. } = &mut spec.transport {
        *args = vec!["node".into(), "-e".into(), "process.exit(3)".into()];
    }
    let m = manager(spec, Duration::from_secs(60));
    assert!(m.call_tool(&bad, "echo", None).await.is_err());
    assert!(
        until(|| containers_of(&bad).is_empty(), 20).await,
        "{:?}",
        containers_of(&bad)
    );
    m.shutdown().await;

    // the sweep removes this instance's containers started by another
    // (crashed) process, and only those
    let instance = tag("sweep");
    mcpgw_runtime::container::set_instance(&instance);
    let orphan = tag("ctr-orphan");
    docker(&[
        "run",
        "-d",
        "--rm",
        "--name",
        &orphan,
        "--label",
        &format!("mcpgw.instance={instance}"),
        "--label",
        "mcpgw.pid=999999999",
        "--label",
        &format!("mcpgw.server={orphan}"),
        IMAGE,
        "sleep",
        "120",
    ]);
    let other = tag("ctr-other-instance");
    docker(&[
        "run",
        "-d",
        "--rm",
        "--name",
        &other,
        "--label",
        "mcpgw.instance=someone-else",
        "--label",
        "mcpgw.pid=999999999",
        "--label",
        &format!("mcpgw.server={other}"),
        IMAGE,
        "sleep",
        "120",
    ]);
    assert_eq!(mcpgw_runtime::container::sweep().await, 1);
    assert!(until(|| containers_of(&orphan).is_empty(), 20).await);
    assert_eq!(containers_of(&other), std::slice::from_ref(&other));
    docker(&["rm", "-f", &other]);
}

/// Podman runs the same server (only where Podman is installed; Ubuntu CI
/// runners have it).
#[tokio::test(flavor = "multi_thread")]
async fn podman_runs_the_same_server() {
    if !podman_available() {
        eprintln!("podman_runs_the_same_server: skipped (no Podman)");
        return;
    }
    let name = tag("ctr-podman");
    let mut spec = node_server(&name);
    if let Transport::Container { runtime, image, .. } = &mut spec.transport {
        *runtime = Some("podman".into());
        // Podman needs a fully qualified name without registries.conf
        *image = format!("docker.io/library/{IMAGE}");
    }
    let m = manager(spec, Duration::from_secs(60));
    let r = m
        .call_tool(&name, "echo", args(json!({"text": "podman"})))
        .await
        .unwrap();
    assert_eq!(text(&r), "podman");
    let c = m.status_of(&name).unwrap().container.unwrap();
    assert_eq!(c.runtime, "podman");
    assert_eq!(containers_in("podman", &name), [c.name]);
    m.shutdown().await;
    assert!(until(|| containers_in("podman", &name).is_empty(), 20).await);
}
