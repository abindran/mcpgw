//! M3-08: an `npx -y <package>` server installed into the package cache
//! starts from it directly, in under 2 s and with no network access (every
//! proxy and the npm registry point at a dead address).
//!
//! The package is the Node test server, packed locally with `npm pack`.
//! Skipped where npm isn't installed.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use mcpgw_core::{ServerSpec, Transport};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions, packages};
use serde_json::json;

fn npm() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

/// Pack the Node test server as `node-fake-mcp-1.0.0.tgz` in `dir`.
fn pack(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("package.json"),
        r#"{"name": "node-fake-mcp", "version": "1.0.0", "bin": {"node-fake-mcp": "server.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("server.js"),
        format!(
            "#!/usr/bin/env node\n{}",
            mcpgw_testkit::container::SERVER_JS
        ),
    )
    .unwrap();
    let o = Command::new(npm())
        .args(["pack", "--pack-destination"])
        .arg(dir)
        .current_dir(&src)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| dir.join("node-fake-mcp-1.0.0.tgz"))
}

#[tokio::test(flavor = "multi_thread")]
async fn cached_node_server_starts_fast_offline() {
    let dir = std::env::temp_dir().join(format!("mcpgw-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(tgz) = pack(&dir) else {
        eprintln!("cached_node_server_starts_fast_offline: skipped (no npm)");
        return;
    };
    let cache = dir.join("runtimes");
    packages::set_dir(cache.clone());

    // as `mcpgw add` sees it
    let args = vec!["-y".to_string(), tgz.to_string_lossy().into_owned()];
    let p = packages::detect("npx", &args).unwrap();
    assert!(
        packages::cached_launch("npx", &args).is_none(),
        "nothing cached yet"
    );
    let installed = packages::install(&cache, &p).await.unwrap();
    assert_eq!(
        (installed.name.as_str(), installed.version.as_str()),
        ("node-fake-mcp", "1.0.0")
    );
    let (program, launch) = packages::cached_launch("npx", &args).unwrap();
    assert_eq!(program, "node");
    assert!(
        launch[0].ends_with("node_modules/node-fake-mcp/server.js"),
        "{launch:?}"
    );
    assert_eq!(packages::list(&cache).len(), 1);

    // started from the cache with the network unreachable
    let dead = "http://127.0.0.1:9";
    let env = [
        ("HTTP_PROXY", dead),
        ("HTTPS_PROXY", dead),
        ("http_proxy", dead),
        ("https_proxy", dead),
        ("npm_config_registry", dead),
        ("npm_config_offline", "true"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .into();
    let spec = ServerSpec::new(
        "nodefake",
        Transport::Stdio {
            command: "npx".into(),
            args,
            env,
            cwd: None,
        },
    );
    let m = BackendManager::with_catalog(
        &[spec],
        ManagerOptions {
            health_interval: Duration::from_secs(60),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    );
    let t = Instant::now();
    let r = m
        .call_tool(
            "nodefake",
            "echo",
            json!({"text": "offline"}).as_object().cloned(),
        )
        .await
        .unwrap();
    let first = t.elapsed();
    assert_eq!(r["content"][0]["text"], "offline");
    let st = m.status_of("nodefake").unwrap();
    println!(
        "cached node server: first call {first:?}, start {:?} ms",
        st.last_start_ms
    );
    assert!(first < Duration::from_secs(2), "first call took {first:?}");
    assert!(st.last_start_ms.unwrap() < 2000, "{st:?}");
    m.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
