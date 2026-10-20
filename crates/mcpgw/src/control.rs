//! Talking to a running daemon's `/admin` API from the CLI.

use mcpgw_api::admin::{AddServer, StatusReport};
use mcpgw_auth::Token;
use mcpgw_config::Config;
use mcpgw_core::{ServerSpec, ToolManifest};
use serde_json::Value;

use crate::{AnyResult, daemon, serve::runtime};

/// A connection to the daemon.
pub struct Control {
    base: String,
    token: String,
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
    /// What `daemon.json` says.
    pub info: daemon::DaemonInfo,
}

impl Control {
    /// The running daemon, if any.
    pub fn connect(config: &Config) -> AnyResult<Option<Self>> {
        let Some(info) = daemon::running(config) else {
            return Ok(None);
        };
        let base = info
            .url
            .strip_suffix(mcpgw_api::MCP_PATH)
            .unwrap_or(&info.url)
            .to_string();
        let token = Token::load_or_create(&config.gateway.token_path())?;
        Ok(Some(Self {
            base,
            token: token.expose().to_string(),
            client: {
                mcpgw_core::tls::install();
                reqwest::Client::new()
            },
            rt: runtime()?,
            info,
        }))
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> AnyResult<Option<Value>> {
        let url = format!("{}{path}", self.base);
        self.rt.block_on(async {
            let mut req = self.client.request(method, &url).bearer_auth(&self.token);
            if let Some(b) = body {
                req = req.json(&b);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let text = resp.text().await?;
            let json: Option<Value> = serde_json::from_str(&text).ok();
            if !status.is_success() {
                let msg = json
                    .as_ref()
                    .and_then(|j| j.get("error"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("daemon answered {status}"));
                return Err(msg.into());
            }
            Ok(json)
        })
    }

    /// `GET /admin/status`.
    pub fn status(&self) -> AnyResult<StatusReport> {
        let v = self
            .call(reqwest::Method::GET, "/admin/status", None)?
            .unwrap_or_default();
        Ok(serde_json::from_value(v)?)
    }

    /// Add (or replace) a server in the running daemon.
    pub fn add(&self, spec: &ServerSpec, manifest: Option<ToolManifest>) -> AnyResult<()> {
        if self.status()?.servers.iter().any(|s| s.name == spec.name) {
            self.remove(&spec.name)?;
        }
        let body = serde_json::to_value(AddServer {
            spec: spec.clone(),
            manifest,
        })?;
        self.call(reqwest::Method::POST, "/admin/servers", Some(body))?;
        Ok(())
    }

    /// Remove a server from the running daemon.
    pub fn remove(&self, name: &str) -> AnyResult<()> {
        self.call(
            reqwest::Method::DELETE,
            &format!("/admin/servers/{name}"),
            None,
        )?;
        Ok(())
    }

    /// `POST /admin/servers/{name}/{action}`; returns the response body.
    pub fn action(&self, name: &str, action: &str) -> AnyResult<Option<Value>> {
        self.call(
            reqwest::Method::POST,
            &format!("/admin/servers/{name}/{action}"),
            None,
        )
    }

    /// The last `lines` stderr lines of a server.
    pub fn logs(&self, name: &str, lines: usize) -> AnyResult<Vec<String>> {
        let v = self
            .call(
                reqwest::Method::GET,
                &format!("/admin/servers/{name}/logs?lines={lines}"),
                None,
            )?
            .unwrap_or_default();
        Ok(serde_json::from_value(v["lines"].clone()).unwrap_or_default())
    }

    /// Start an OAuth flow; returns `(state, url, account)`.
    pub fn oauth_start(
        &self,
        provider: &str,
        account: &str,
    ) -> AnyResult<(String, String, String)> {
        let v = self
            .call(
                reqwest::Method::POST,
                &format!("/admin/oauth/{provider}/start"),
                Some(serde_json::json!({ "account": account })),
            )?
            .unwrap_or_default();
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Ok((s("state"), s("url"), s("account")))
    }

    /// Poll an OAuth flow.
    pub fn oauth_flow(&self, state: &str) -> AnyResult<Value> {
        Ok(self
            .call(
                reqwest::Method::GET,
                &format!("/admin/oauth/flows/{state}"),
                None,
            )?
            .unwrap_or_default())
    }

    /// Connected accounts.
    pub fn oauth_connections(&self) -> AnyResult<Vec<Value>> {
        let v = self
            .call(reqwest::Method::GET, "/admin/oauth/connections", None)?
            .unwrap_or_default();
        Ok(v["connections"].as_array().cloned().unwrap_or_default())
    }

    /// Forget a connected account.
    pub fn oauth_disconnect(&self, provider: &str, account: &str) -> AnyResult<()> {
        self.call(
            reqwest::Method::DELETE,
            &format!("/admin/oauth/connections/{provider}/{account}"),
            None,
        )?;
        Ok(())
    }

    /// Ask the daemon to exit.
    pub fn shutdown(&self) -> AnyResult<()> {
        self.call(reqwest::Method::POST, "/admin/shutdown", None)?;
        Ok(())
    }
}
