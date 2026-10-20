//! M3-07 end to end: in meta mode, `search_tools` starts the servers behind
//! its best hits in the background, so the `call_tool` that follows a
//! moment later doesn't wait. Done when the cold start is hidden in more
//! than 80% of search-then-call sequences; without prewarm, in none.

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

/// Start delay of every server, and the model's "think time" between a
/// search and the call.
const START_MS: u64 = 700;
const THINK_MS: u64 = 1000;

const TOPICS: [&str; 10] = [
    "weather",
    "stocks",
    "calendar",
    "translate",
    "recipes",
    "flights",
    "invoices",
    "playlists",
    "tickets",
    "glossary",
];

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str, extra: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-prewarm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"meta\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n{extra}",
                dir.join("db.sqlite"),
                dir.join("token")
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    }

    fn url(&self) -> String {
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).unwrap())
                .unwrap();
        info["url"].as_str().unwrap().to_string()
    }

    /// Add one slow server per topic, each with a single tool about it.
    fn add_servers(&self, topics: &[&str]) {
        let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
        for t in topics {
            let tools = json!([{"name": format!("lookup_{t}"), "description": format!("Look up {t} details")}]);
            self.ok(&[
                "add",
                t,
                "--command",
                &fake,
                "--env",
                &format!("FAKE_START_DELAY_MS={START_MS}"),
                "--env",
                &format!("FAKE_TOOLS={tools}"),
            ]);
        }
    }

    /// `cold_starts` per server, from the API.
    async fn cold_starts(&self, token: &str) -> Vec<(String, u64)> {
        mcpgw_core::tls::install();
        let base = self.url().trim_end_matches("/mcp").to_string();
        let v: Value = reqwest::Client::new()
            .get(format!("{base}/api/v1/servers"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v.as_array()
            .or_else(|| v["servers"].as_array())
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap().to_string(),
                    s["cold_starts"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Search for each topic, think, then call the tool found. Returns each
/// call's latency.
async fn sequences(c: &McpClient, topics: &[&str]) -> Vec<Duration> {
    let mut out = Vec::new();
    for t in topics {
        let found = c.call("search_tools", json!({"query": t})).await.unwrap();
        let name = found["structuredContent"]["tools"][0]["name"]
            .as_str()
            .unwrap_or_else(|| panic!("no hit for {t}: {found}"))
            .to_string();
        assert!(name.ends_with(&format!("lookup_{t}")), "{name}");
        tokio::time::sleep(Duration::from_millis(THINK_MS)).await;
        let started = Instant::now();
        let r = c
            .call("call_tool", json!({"name": name, "arguments": {"q": t}}))
            .await
            .unwrap();
        assert_ne!(r["isError"], true, "{r}");
        out.push(started.elapsed());
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn search_then_call_hides_the_cold_start() {
    let e = Env::new("on", "");
    e.add_servers(&TOPICS);
    e.ok(&["up"]);
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(&e.url(), Some(&token)).await.unwrap();
    let lat = sequences(&c, &TOPICS).await;
    let cold = e.cold_starts(&token).await;
    let hidden = cold.iter().filter(|(_, n)| *n == 0).count();
    let ratio = hidden as f64 / TOPICS.len() as f64;
    println!(
        "prewarm on: cold start hidden in {hidden}/{} sequences ({:.0}%); call latencies {lat:?}",
        TOPICS.len(),
        ratio * 100.0
    );
    assert!(
        ratio > 0.8,
        "hidden in only {hidden}/{}: {cold:?}",
        TOPICS.len()
    );
    let slow = lat
        .iter()
        .filter(|d| **d >= Duration::from_millis(START_MS))
        .count();
    assert!(
        slow * 10 < TOPICS.len() * 2,
        "{slow} calls still waited a full start: {lat:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_prewarm_every_call_waits() {
    let topics = &TOPICS[..3];
    let e = Env::new("off", "prewarm = false\n");
    e.add_servers(topics);
    e.ok(&["up"]);
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(&e.url(), Some(&token)).await.unwrap();
    let lat = sequences(&c, topics).await;
    let cold = e.cold_starts(&token).await;
    println!("prewarm off: call latencies {lat:?}");
    assert!(cold.iter().all(|(_, n)| *n == 1), "{cold:?}");
    assert!(
        lat.iter().all(|d| *d >= Duration::from_millis(START_MS)),
        "{lat:?}"
    );
}
