//! M2-11: every `/api/v1` endpoint against a running daemon. The test
//! records each (method, path) it exercised and checks that the set equals
//! the paths in the published OpenAPI document.

use std::{collections::BTreeSet, path::PathBuf, process::Command, sync::Mutex};

use mcpgw_testkit::McpClient;
use reqwest::StatusCode;
use serde_json::{Value, json};

struct Api {
    dir: PathBuf,
    base: String,
    token: String,
    http: reqwest::Client,
    seen: Mutex<BTreeSet<(String, String)>>,
}

impl Api {
    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-api-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n\
                 approval_timeout = \"20s\"\npolicy = {{ approve = [\"fx__echo\"] }}\n\n\
                 [[server]]\nname = \"fx\"\ntransport = \"stdio\"\ncommand = {:?}\n",
                dir.join("db.sqlite"),
                dir.join("token"),
                mcpgw_testkit::fake_mcp()
            ),
        )
        .unwrap();
        let mut api = Self {
            dir,
            base: String::new(),
            token: String::new(),
            http: {
                mcpgw_core::tls::install();
                reqwest::Client::new()
            },
            seen: Mutex::default(),
        };
        api.mcpgw(&["up"]);
        let token = api.mcpgw(&["token"]).trim().to_string();
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(api.dir.join("daemon.json")).unwrap())
                .unwrap();
        api.base = info["url"]
            .as_str()
            .unwrap()
            .trim_end_matches("/mcp")
            .to_string();
        api.token = token;
        api
    }

    fn mcpgw(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("MCPGW_VAULT_PASSPHRASE", "pw")
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    /// Call `method path`; `template` is the OpenAPI path it exercises.
    async fn call(
        &self,
        method: &str,
        template: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.seen
            .lock()
            .unwrap()
            .insert((method.to_lowercase(), template.to_string()));
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn mcp(&self, path: &str) -> McpClient {
        McpClient::http(&format!("{}/mcp{path}", self.base), Some(&self.token))
            .await
            .unwrap()
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.mcpgw(&["down"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_endpoint_works_and_is_documented() {
    let a = Api::start("all");
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    use StatusCode as S;

    // spec and runtime
    let (s, spec) = a
        .call("GET", "/api/v1/openapi.json", "/api/v1/openapi.json", None)
        .await;
    assert_eq!((s, spec["openapi"].as_str()), (S::OK, Some("3.1.0")));
    let (s, rt) = a
        .call("GET", "/api/v1/runtime", "/api/v1/runtime", None)
        .await;
    assert_eq!(s, S::OK);
    assert_eq!(rt["servers"]["total"], 1);
    assert_eq!(rt["version"], env!("CARGO_PKG_VERSION"));

    // servers
    let (s, list) = a
        .call("GET", "/api/v1/servers", "/api/v1/servers", None)
        .await;
    assert_eq!(
        (s, names(&list, "servers")),
        (S::OK, vec!["fx".to_string()])
    );
    assert_eq!(list["servers"][0]["source"], "config");
    let extra = json!({"name": "extra", "transport": "stdio", "command": fake, "env": {"FAKE_STDERR": "extra booted"}});
    let (s, v) = a
        .call(
            "POST",
            "/api/v1/servers",
            "/api/v1/servers",
            Some(extra.clone()),
        )
        .await;
    assert_eq!(
        (s, v["source"].as_str()),
        (S::CREATED, Some("store")),
        "{v}"
    );
    let (s, v) = a
        .call("POST", "/api/v1/servers", "/api/v1/servers", Some(extra))
        .await;
    assert_eq!(s, S::CONFLICT, "{v}");
    let (s, _) = a
        .call(
            "POST",
            "/api/v1/servers",
            "/api/v1/servers",
            Some(json!({"name": "bad__name", "transport": "stdio", "command": "x"})),
        )
        .await;
    assert_eq!(s, S::BAD_REQUEST);
    let (s, v) = a
        .call(
            "GET",
            "/api/v1/servers/{name}",
            "/api/v1/servers/extra",
            None,
        )
        .await;
    assert_eq!(
        (s, v["spec"]["command"].as_str()),
        (S::OK, Some(fake.as_str()))
    );
    let (s, _) = a
        .call(
            "GET",
            "/api/v1/servers/{name}",
            "/api/v1/servers/nope",
            None,
        )
        .await;
    assert_eq!(s, S::NOT_FOUND);
    let (s, v) = a
        .call(
            "POST",
            "/api/v1/servers/{name}/disable",
            "/api/v1/servers/extra/disable",
            None,
        )
        .await;
    assert_eq!(
        (s, v["enabled"].as_bool(), v["state"].as_str()),
        (S::OK, Some(false), Some("disabled"))
    );
    let (s, v) = a
        .call(
            "POST",
            "/api/v1/servers/{name}/enable",
            "/api/v1/servers/extra/enable",
            None,
        )
        .await;
    assert_eq!((s, v["enabled"].as_bool()), (S::OK, Some(true)));
    let (s, _) = a
        .call(
            "POST",
            "/api/v1/servers/{name}/disable",
            "/api/v1/servers/fx/disable",
            None,
        )
        .await;
    assert_eq!(s, S::CONFLICT, "config servers are read-only");

    // the new server works over MCP, which captures its tools
    let c = a.mcp("").await;
    assert_eq!(
        c.call_text("extra__add", json!({"a": 1, "b": 2}))
            .await
            .unwrap(),
        "3"
    );
    let (s, v) = a
        .call(
            "GET",
            "/api/v1/servers/{name}/tools",
            "/api/v1/servers/extra/tools",
            None,
        )
        .await;
    assert_eq!((s, v["tools"].as_array().map(Vec::len)), (S::OK, Some(7)));
    assert_eq!(v["pin"].as_str().map(str::len), Some(64));
    let (s, v) = a
        .call(
            "GET",
            "/api/v1/servers/{name}/logs",
            "/api/v1/servers/extra/logs?lines=5",
            None,
        )
        .await;
    assert_eq!(s, S::OK);
    assert!(
        v["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l == "extra booted"),
        "{v}"
    );
    let (s, _) = a
        .call(
            "POST",
            "/api/v1/servers/{name}/restart",
            "/api/v1/servers/extra/restart",
            None,
        )
        .await;
    assert_eq!(s, S::NO_CONTENT);
    let (s, v) = a
        .call(
            "POST",
            "/api/v1/servers/{name}/pin",
            "/api/v1/servers/extra/pin",
            None,
        )
        .await;
    assert_eq!(s, S::CONFLICT, "not quarantined: {v}");

    // tools
    let (s, v) = a.call("GET", "/api/v1/tools", "/api/v1/tools", None).await;
    assert_eq!(s, S::OK);
    let all = names(&v, "tools");
    assert!(
        all.contains(&"fx__add".to_string()) && all.contains(&"extra__add".to_string()),
        "{all:?}"
    );
    assert!(
        v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["server"] == "extra")
    );

    // profiles: created through the API, usable at once
    let web = json!({"name": "web", "servers": ["extra"], "mode": "direct"});
    let (s, v) = a
        .call(
            "PUT",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            Some(web.clone()),
        )
        .await;
    assert_eq!(
        (s, v["source"].as_str()),
        (S::CREATED, Some("store")),
        "{v}"
    );
    assert!(v["url"].as_str().unwrap().ends_with("/mcp/p/web"));
    let (s, _) = a
        .call(
            "PUT",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            Some(web),
        )
        .await;
    assert_eq!(s, S::OK);
    let (s, _) = a
        .call(
            "PUT",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            Some(json!({"name": "other"})),
        )
        .await;
    assert_eq!(s, S::BAD_REQUEST);
    let (s, _) = a
        .call(
            "PUT",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/x",
            Some(json!({"name": "x", "servers": ["ghost"]})),
        )
        .await;
    assert_eq!(s, S::BAD_REQUEST);
    let (s, v) = a
        .call("GET", "/api/v1/profiles", "/api/v1/profiles", None)
        .await;
    assert_eq!((s, names(&v, "profiles")), (S::OK, vec!["web".to_string()]));
    let (s, v) = a
        .call(
            "GET",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            None,
        )
        .await;
    assert_eq!((s, v["servers"][0].as_str()), (S::OK, Some("extra")));
    let p = a.mcp("/p/web").await;
    let pt = p.tool_names().await.unwrap();
    assert!(
        !pt.is_empty() && pt.iter().all(|n| n.starts_with("extra__")),
        "{pt:?}"
    );
    p.close().await;
    let (s, v) = a
        .call("GET", "/api/v1/tools", "/api/v1/tools?profile=web", None)
        .await;
    assert_eq!(s, S::OK);
    assert!(names(&v, "tools").iter().all(|n| n.starts_with("extra__")));
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            None,
        )
        .await;
    assert_eq!(s, S::NO_CONTENT);
    assert!(
        McpClient::http(&format!("{}/mcp/p/web", a.base), Some(&a.token))
            .await
            .is_err()
    );
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/profiles/{name}",
            "/api/v1/profiles/web",
            None,
        )
        .await;
    assert_eq!(s, S::NOT_FOUND);

    // secrets: write-only values
    let value = format!("api-secret-{}", "s".repeat(16));
    let (s, _) = a
        .call(
            "PUT",
            "/api/v1/secrets/{name}",
            "/api/v1/secrets/api_key",
            Some(json!({"value": value, "description": "test key"})),
        )
        .await;
    assert_eq!(s, S::NO_CONTENT);
    let (s, v) = a
        .call("GET", "/api/v1/secrets", "/api/v1/secrets", None)
        .await;
    assert_eq!(s, S::OK);
    assert_eq!(v["secrets"][0]["name"], "api_key");
    assert_eq!(v["secrets"][0]["description"], "test key");
    assert!(!v.to_string().contains(&value), "value returned: {v}");
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/secrets/{name}",
            "/api/v1/secrets/api_key",
            None,
        )
        .await;
    assert_eq!(s, S::NO_CONTENT);
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/secrets/{name}",
            "/api/v1/secrets/api_key",
            None,
        )
        .await;
    assert_eq!(s, S::NOT_FOUND);

    // approvals
    let (s, v) = a
        .call("GET", "/api/v1/approvals", "/api/v1/approvals", None)
        .await;
    assert_eq!(
        (s, v["approvals"].as_array().map(Vec::len)),
        (S::OK, Some(0))
    );
    let c = std::sync::Arc::new(c);
    for (verb, expect) in [("approve", "hello"), ("deny", "was not approved")] {
        let call = {
            let c = c.clone();
            tokio::spawn(async move { c.call("fx__echo", json!({"text": "hello"})).await })
        };
        let mut id = None;
        for _ in 0..100 {
            let (_, v) = a
                .call("GET", "/api/v1/approvals", "/api/v1/approvals", None)
                .await;
            if let Some(x) = v["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["status"] == "pending")
            {
                id = Some(x["id"].as_str().unwrap().to_string());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let id = id.expect("no pending approval");
        let template = format!("/api/v1/approvals/{{id}}/{verb}");
        let (s, _) = a
            .call(
                "POST",
                &template,
                &format!("/api/v1/approvals/{id}/{verb}"),
                None,
            )
            .await;
        assert_eq!(s, S::NO_CONTENT);
        let r = call.await.unwrap().unwrap();
        assert!(
            r["content"][0]["text"].as_str().unwrap().contains(expect),
            "{verb}: {r}"
        );
    }
    let (s, _) = a
        .call(
            "POST",
            "/api/v1/approvals/{id}/approve",
            "/api/v1/approvals/nope/approve",
            None,
        )
        .await;
    assert_eq!(s, S::NOT_FOUND);

    // the audit log saw the API's changes and the calls
    let (s, v) = a
        .call(
            "GET",
            "/api/v1/audit",
            "/api/v1/audit?after=0&limit=500",
            None,
        )
        .await;
    assert_eq!(s, S::OK);
    let entries = v["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|x| x["kind"] == "server.add" && x["actor"] == "api" && x["subject"] == "extra"),
        "{v}"
    );
    assert!(
        entries
            .iter()
            .any(|x| x["kind"] == "profile.delete" && x["subject"] == "web")
    );
    assert!(
        entries
            .iter()
            .any(|x| x["kind"] == "secret.set" && x["subject"] == "api_key")
    );
    assert!(
        entries
            .iter()
            .any(|x| x["kind"] == "approval.decide" && x["detail"]["approved"] == false)
    );
    assert!(
        entries
            .iter()
            .any(|x| x["kind"] == "tool.call" && x["subject"] == "fx__echo")
    );
    let (s, v) = a
        .call("GET", "/api/v1/audit/verify", "/api/v1/audit/verify", None)
        .await;
    assert_eq!((s, v["ok"].as_bool()), (S::OK, Some(true)), "{v}");

    // recent calls, newest first: the denied echo is last
    let (s, v) = a
        .call("GET", "/api/v1/calls", "/api/v1/calls?limit=10", None)
        .await;
    assert_eq!(s, S::OK);
    let calls = v["calls"].as_array().unwrap();
    assert_eq!(calls[0]["tool"], "fx__echo");
    assert_eq!(calls[0]["outcome"], "tool_error");
    assert!(
        calls
            .iter()
            .any(|c| c["tool"] == "extra__add" && c["outcome"] == "ok"),
        "{v}"
    );
    assert!(
        !v.to_string().contains("hello"),
        "arguments must not be logged"
    );

    // removing servers
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/servers/{name}",
            "/api/v1/servers/extra",
            None,
        )
        .await;
    assert_eq!(s, S::NO_CONTENT);
    let (s, _) = a
        .call(
            "DELETE",
            "/api/v1/servers/{name}",
            "/api/v1/servers/fx",
            None,
        )
        .await;
    assert_eq!(s, S::CONFLICT);
    let (_, list) = a
        .call("GET", "/api/v1/servers", "/api/v1/servers", None)
        .await;
    assert_eq!(names(&list, "servers"), ["fx"]);
    assert!(
        !a.mcpgw(&["list"]).contains("extra"),
        "removal not persisted"
    );

    // the token is required
    let r = a
        .http
        .get(format!("{}/api/v1/runtime", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), S::UNAUTHORIZED);

    // every documented operation was exercised, and nothing else exists
    let documented: BTreeSet<(String, String)> = spec["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(p, ops)| {
            ops.as_object()
                .unwrap()
                .keys()
                .map(move |m| (m.clone(), p.clone()))
        })
        .collect();
    let seen = a.seen.lock().unwrap().clone();
    assert_eq!(seen, documented);
}

/// M2-12: the UI is three static files, served without the token under a
/// strict CSP, and still only to loopback hosts.
#[tokio::test(flavor = "multi_thread")]
async fn web_ui_is_served_with_a_strict_csp() {
    let a = Api::start("ui");
    for (path, kind) in [
        ("/ui", "text/html"),
        ("/ui/app.js", "text/javascript"),
        ("/ui/app.css", "text/css"),
    ] {
        let r = a
            .http
            .get(format!("{}{path}", a.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        let h = r.headers();
        assert!(
            h["content-type"].to_str().unwrap().starts_with(kind),
            "{path}"
        );
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
            "{csp}"
        );
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h["referrer-policy"], "no-referrer");
    }
    let page = a
        .http
        .get(format!("{}/ui", a.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        page.contains("/ui/app.js") && !page.contains(&a.token),
        "the page must not embed the token"
    );
    let r = a
        .http
        .get(format!("{}/ui", a.base))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "DNS rebinding");
    let url = a.mcpgw(&["ui", "--print"]);
    assert!(
        url.trim().ends_with(&format!("/ui#token={}", a.token)),
        "{url}"
    );
}
