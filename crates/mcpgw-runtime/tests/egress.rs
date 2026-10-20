//! M3-03: a container server with `egress = ["api.github.com:443"]` reaches
//! api.github.com:443 (a verified TLS connection) and nothing else: no other
//! host or port, no IP address, no other name over a shared port. `[]`
//! means no network. The proxy sidecar and network go with the server.
//!
//! Needs Docker and internet access; skipped without Docker unless
//! MCPGW_TEST_CONTAINERS=1 (the Linux CI jobs).

use std::{sync::Arc, time::Duration};

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use mcpgw_testkit::container::{containers_of, containers_or_skip, docker, node_server};
use serde_json::json;

fn with_egress(name: &str, egress: Option<&[&str]>) -> ServerSpec {
    let mut s = node_server(name);
    if let Transport::Container { egress: e, .. } = &mut s.transport {
        *e = egress.map(|l| l.iter().map(|x| x.to_string()).collect());
    }
    s
}

async fn connect(
    m: &BackendManager,
    server: &str,
    host: &str,
    port: u16,
    tls: bool,
    sni: Option<&str>,
) -> String {
    let mut args = json!({"host": host, "port": port, "tls": tls});
    if let Some(n) = sni {
        args["servername"] = json!(n);
    }
    let r = m
        .call_tool(server, "connect", args.as_object().cloned())
        .await
        .unwrap_or_else(|e| panic!("{server}: {e}"));
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

fn networks_of(server: &str) -> Vec<String> {
    docker(&[
        "network",
        "ls",
        "--filter",
        &format!("label=mcpgw.server={server}"),
        "--format",
        "{{.Name}}",
    ])
    .lines()
    .map(str::to_string)
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_allowlisted_hosts_are_reachable() {
    if !containers_or_skip("only_allowlisted_hosts_are_reachable") {
        return;
    }
    let pid = std::process::id();
    let (open, gh, multi, none) = (
        format!("open-{pid}"),
        format!("gh-{pid}"),
        format!("ghmulti-{pid}"),
        format!("nonet-{pid}"),
    );
    let m = BackendManager::with_catalog(
        &[
            with_egress(&open, None),
            with_egress(&gh, Some(&["api.github.com:443"])),
            with_egress(&multi, Some(&["api.github.com:443", "github.com:443"])),
            with_egress(&none, Some(&[])),
        ],
        ManagerOptions {
            connect_timeout: Duration::from_secs(90),
            call_timeout: Duration::from_secs(30),
            health_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(300),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );

    // without an allowlist the network works (else nothing below means much)
    let reach = connect(&m, &open, "example.com", 443, true, None).await;
    if !reach.starts_with("ok") {
        assert!(
            std::env::var("MCPGW_TEST_CONTAINERS").as_deref() != Ok("1"),
            "no internet from containers: {reach}"
        );
        eprintln!(
            "only_allowlisted_hosts_are_reachable: skipped (no internet from containers: {reach})"
        );
        m.shutdown().await;
        return;
    }

    // allowlisted: a real, verified TLS connection to GitHub
    let ok = connect(&m, &gh, "api.github.com", 443, true, None).await;
    assert!(ok.starts_with("ok"), "api.github.com:443: {ok}");
    // anything else is blocked
    for (host, port, tls) in [
        ("example.com", 443, true),
        ("github.com", 443, true),
        ("api.github.com", 80, false),
        ("1.1.1.1", 443, false),
        ("140.82.112.6", 443, false),
    ] {
        let r = connect(&m, &gh, host, port, tls, None).await;
        assert!(!r.starts_with("ok"), "{host}:{port} was reachable: {r}");
    }
    let side = format!("{}-net", m.status_of(&gh).unwrap().container.unwrap().name);
    assert_eq!(
        docker(&["network", "inspect", "--format", "{{.Internal}}", &side]),
        "true"
    );

    // several hosts on one port: routed by name, others refused
    for host in ["api.github.com", "github.com"] {
        let r = connect(&m, &multi, host, 443, true, None).await;
        assert!(r.starts_with("ok"), "{host}: {r}");
    }
    let r = connect(&m, &multi, "api.github.com", 443, true, Some("example.com")).await;
    assert!(!r.starts_with("ok"), "an unlisted name went through: {r}");

    // [] is no network at all
    let r = connect(&m, &none, "api.github.com", 443, true, None).await;
    assert!(!r.starts_with("ok"), "{r}");
    let ctr = m.status_of(&none).unwrap().container.unwrap().name;
    assert_eq!(
        docker(&["inspect", "--format", "{{.HostConfig.NetworkMode}}", &ctr]),
        "none"
    );

    // the sidecar and network go with the server
    assert_eq!(containers_of(&gh).len(), 2, "server and proxy");
    assert_eq!(networks_of(&gh).len(), 1);
    m.shutdown().await;
    for s in [&open, &gh, &multi, &none] {
        assert!(
            until(|| containers_of(s).is_empty(), 30).await,
            "{s}: {:?}",
            containers_of(s)
        );
        assert!(networks_of(s).is_empty(), "{s}: {:?}", networks_of(s));
    }
}
