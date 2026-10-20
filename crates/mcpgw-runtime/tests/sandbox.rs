//! M3-02: sandbox profiles and limits for container servers. A server
//! going over its 256 MiB limit is killed and reported; the gateway and
//! its other servers carry on.
//!
//! Skipped without Docker for Linux containers, unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{sync::Arc, time::Duration};

use mcpgw_core::{ByteSize, Limits, Sandbox, ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use mcpgw_testkit::container::{containers_of, containers_or_skip, docker, node_server};
use serde_json::json;

fn manager(specs: &[ServerSpec]) -> BackendManager {
    BackendManager::with_catalog(
        specs,
        ManagerOptions {
            connect_timeout: Duration::from_secs(60),
            health_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(120),
            backoff_initial: Duration::from_millis(200),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    )
}

async fn call(
    m: &BackendManager,
    server: &str,
    tool: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    m.call_tool(server, tool, args.as_object().cloned())
        .await
        .map(|r| {
            r["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .map_err(|e| e.to_string())
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

fn with_sandbox(name: &str, sandbox: Sandbox) -> ServerSpec {
    let mut s = node_server(name);
    if let Transport::Container { sandbox: sb, .. } = &mut s.transport {
        *sb = sandbox;
    }
    s
}

fn inspect(server: &str, m: &BackendManager, format: &str) -> String {
    let name = m.status_of(server).unwrap().container.unwrap().name;
    docker(&["inspect", "--format", format, &name])
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_limit_kills_the_server_not_the_gateway() {
    if !containers_or_skip("memory_limit_kills_the_server_not_the_gateway") {
        return;
    }
    let pid = std::process::id();
    let (hog, other) = (format!("hog-{pid}"), format!("calm-{pid}"));
    let mut spec = node_server(&hog);
    spec.limits = Limits {
        memory: Some(ByteSize::mib(256)),
        ..Default::default()
    };
    let m = manager(&[spec, node_server(&other)]);
    assert_eq!(
        call(&m, &hog, "echo", json!({"text": "up"})).await.unwrap(),
        "up"
    );
    assert_eq!(
        call(&m, &other, "echo", json!({"text": "up"}))
            .await
            .unwrap(),
        "up"
    );

    // the standard profile and the limit are applied
    assert_eq!(
        inspect(
            &hog,
            &m,
            "{{.HostConfig.Memory}} {{.HostConfig.MemorySwap}}"
        ),
        "268435456 268435456"
    );
    assert_eq!(inspect(&hog, &m, "{{.HostConfig.CapDrop}}"), "[ALL]");
    assert!(inspect(&hog, &m, "{{.HostConfig.SecurityOpt}}").contains("no-new-privileges"));
    assert_eq!(inspect(&hog, &m, "{{.HostConfig.PidsLimit}}"), "512");
    assert_eq!(
        call(&m, &hog, "alloc", json!({"mib": 64})).await.unwrap(),
        "holding 64 MiB"
    );

    // over the limit: the call fails, the kill is reported, the container goes
    let e = call(&m, &hog, "alloc", json!({"mib": 400}))
        .await
        .unwrap_err();
    assert!(!e.is_empty());
    let reported = || {
        m.status_of(&hog).unwrap().last_error.is_some_and(|e| {
            e.contains("out of memory: the container was killed at its 256MiB limit")
        })
    };
    assert!(
        until(reported, 20).await,
        "{:?}",
        m.status_of(&hog).unwrap()
    );
    assert!(
        m.stderr_tail(&hog)
            .iter()
            .any(|l| l.contains("mcpgw: out of memory")),
        "{:?}",
        m.stderr_tail(&hog)
    );
    assert!(
        until(|| containers_of(&hog).is_empty(), 20).await,
        "{:?}",
        containers_of(&hog)
    );

    // the gateway carries on: the other server is untouched and the killed
    // one comes back on demand
    assert_eq!(
        call(&m, &other, "echo", json!({"text": "still here"}))
            .await
            .unwrap(),
        "still here"
    );
    let mut back = Err(String::new());
    for _ in 0..50 {
        back = call(&m, &hog, "echo", json!({"text": "back"})).await;
        if back.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(back.unwrap(), "back");
    m.shutdown().await;
    assert!(
        until(
            || containers_of(&hog).is_empty() && containers_of(&other).is_empty(),
            20
        )
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_and_off_profiles() {
    if !containers_or_skip("strict_and_off_profiles") {
        return;
    }
    let pid = std::process::id();
    let (strict, off) = (format!("strict-{pid}"), format!("off-{pid}"));
    let m = manager(&[
        with_sandbox(&strict, Sandbox::Strict),
        with_sandbox(&off, Sandbox::Off),
    ]);

    // strict: nobody, read-only root, writable /tmp, tighter defaults
    assert_eq!(call(&m, &strict, "uid", json!({})).await.unwrap(), "65534");
    assert_eq!(
        call(&m, &strict, "write", json!({"path": "/opt/x"}))
            .await
            .unwrap(),
        "EROFS"
    );
    assert_eq!(
        call(&m, &strict, "write", json!({"path": "/tmp/x"}))
            .await
            .unwrap(),
        "written"
    );
    assert_eq!(
        inspect(
            &strict,
            &m,
            "{{.HostConfig.ReadonlyRootfs}} {{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} {{.HostConfig.PidsLimit}} {{.Config.User}}"
        ),
        "true 536870912 1000000000 128 65534:65534"
    );

    // off: the image's defaults (root, writable), no limits
    assert_eq!(call(&m, &off, "uid", json!({})).await.unwrap(), "0");
    assert_eq!(
        call(&m, &off, "write", json!({"path": "/opt/x"}))
            .await
            .unwrap(),
        "written"
    );
    assert_eq!(
        inspect(&off, &m, "{{.HostConfig.Memory}} {{.HostConfig.CapDrop}}"),
        "0 []"
    );
    m.shutdown().await;
    assert!(
        until(
            || containers_of(&strict).is_empty() && containers_of(&off).is_empty(),
            20
        )
        .await
    );
}
