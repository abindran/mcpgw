//! M4-09 at the manager: a call that reports progress outlives the call
//! timeout and passes its progress on; dropping a call cancels it at the
//! backend; a backend that runs the call as a task is followed to its end.

use std::{path::PathBuf, sync::Arc, time::Duration};

use mcpgw_proto::progress::{ProgressSink, with_progress};
use mcpgw_runtime::{BackendManager, Catalog, ManagerOptions};
use serde_json::json;

fn manager(dir: &std::path::Path, tasks: bool) -> BackendManager {
    manager_with(dir, tasks, Duration::from_secs(60))
}

fn manager_with(dir: &std::path::Path, tasks: bool, health: Duration) -> BackendManager {
    let log = dir.join("calls.log");
    let log = log.to_str().unwrap();
    let mut env = vec![("FAKE_CALL_LOG", log)];
    if tasks {
        env.push(("FAKE_TASKS", "1"));
    }
    BackendManager::with_catalog(
        &[mcpgw_testkit::stdio("slow", &env)],
        ManagerOptions {
            connect_timeout: Duration::from_secs(30),
            // a call may stay silent for 1 s at most
            call_timeout: Duration::from_secs(1),
            max_call_time: Duration::from_secs(60),
            health_interval: health,
            idle_timeout: Duration::from_secs(120),
            ..ManagerOptions::default()
        },
        Arc::new(Catalog::default()),
    )
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mcpgw-long-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn log(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
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

fn text(r: &serde_json::Map<String, serde_json::Value>) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_keeps_a_long_call_alive_and_reaches_the_caller() {
    let d = dir("progress");
    let m = manager(&d, false);
    // 4 s in 8 steps: four times the call timeout, with progress every 0.5 s
    let (sink, mut rx) = ProgressSink::channel();
    let args = json!({"ms": 4000, "steps": 8}).as_object().cloned();
    let r = with_progress(sink, m.call_tool("slow", "sleep", args))
        .await
        .unwrap();
    assert_eq!(text(&r), "slept");
    let mut seen = Vec::new();
    while let Ok(p) = rx.try_recv() {
        seen.push((p.progress, p.total, p.message));
    }
    assert_eq!(seen.len(), 8, "{seen:?}");
    assert_eq!(seen[7], (8.0, Some(8.0), Some("step 8 of 8".into())));

    // silence past the timeout still fails
    let args = json!({"ms": 3000}).as_object().cloned();
    let e = m.call_tool("slow", "sleep", args).await.unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
    m.shutdown().await;
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_call_cancels_it_at_the_backend() {
    let d = dir("cancel");
    let m = Arc::new(manager(&d, false));
    let m2 = m.clone();
    let call = tokio::spawn(async move {
        let args = json!({"ms": 20000, "steps": 40}).as_object().cloned();
        m2.call_tool("slow", "sleep", args).await
    });
    assert!(until(|| log(&d).contains("sleep"), 30).await);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    call.abort();
    assert!(
        until(|| log(&d).contains("sleep cancelled at"), 10).await,
        "{}",
        log(&d)
    );
    assert!(!log(&d).contains("sleep done"));
    // the server is fine afterwards
    let args = json!({"a": 1, "b": 2}).as_object().cloned();
    assert_eq!(text(&m.call_tool("slow", "add", args).await.unwrap()), "3");
    m.shutdown().await;
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test(flavor = "multi_thread")]
async fn backend_tasks_are_followed_and_cancelled() {
    let d = dir("tasks");
    let m = Arc::new(manager(&d, true));
    // the backend answers with a task; its status messages become progress
    let (sink, mut rx) = ProgressSink::channel();
    let args = json!({"ms": 2000, "steps": 4}).as_object().cloned();
    let r = with_progress(sink, m.call_tool("slow", "sleep", args))
        .await
        .unwrap();
    assert_eq!(text(&r), "slept");
    let mut messages = Vec::new();
    while let Ok(p) = rx.try_recv() {
        messages.extend(p.message);
    }
    assert!(
        messages.contains(&"step 2 of 4".to_string()),
        "{messages:?}"
    );

    // dropping the call sends tasks/cancel
    let m2 = m.clone();
    let call = tokio::spawn(async move {
        let args = json!({"ms": 20000, "steps": 40}).as_object().cloned();
        m2.call_tool("slow", "sleep", args).await
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    call.abort();
    assert!(
        until(|| log(&d).contains("sleep cancelled at"), 10).await,
        "{}",
        log(&d)
    );
    m.shutdown().await;
    let _ = std::fs::remove_dir_all(&d);
}

/// Health checks during a long call leave it alone: a 2026-07-28 server
/// that answers `ping` with "method not found" is alive, not lost (this
/// once restarted servers under their calls every 30 s).
#[tokio::test(flavor = "multi_thread")]
async fn health_checks_leave_a_long_call_alone() {
    let d = dir("health");
    let m = Arc::new(manager_with(&d, false, Duration::from_millis(300)));
    let args = json!({"a": 1, "b": 1}).as_object().cloned();
    m.call_tool("slow", "add", args).await.unwrap();
    let pid = m.status_of("slow").unwrap().pid;
    let m2 = m.clone();
    let call = tokio::spawn(async move {
        let args = json!({"ms": 20000, "steps": 40}).as_object().cloned();
        m2.call_tool("slow", "sleep", args).await
    });
    // many health checks go by
    tokio::time::sleep(Duration::from_secs(3)).await;
    let st = m.status_of("slow").unwrap();
    assert_eq!(st.pid, pid, "restarted: {:?}", st.last_error);
    assert!(st.last_error.is_none(), "{:?}", st.last_error);
    call.abort();
    assert!(
        until(|| log(&d).contains("sleep cancelled at"), 10).await,
        "{}",
        log(&d)
    );
    m.shutdown().await;
    let _ = std::fs::remove_dir_all(&d);
}
