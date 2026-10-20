//! OpenAPI servers in the runtime (M4-05): the spec is loaded at start
//! (file or URL), its operations become tools, and they're served over an
//! in-memory MCP session like any server.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use mcpgw_openapi::{Api, RequestExtras};
use mcpgw_proto::{BackendConn, JsonObject, LocalTools, ProtoError};

/// How to start an OpenAPI server.
#[derive(Clone)]
pub struct OpenApiTemplate {
    /// Spec file or URL.
    pub spec: String,
    /// Base URL override.
    pub base_url: Option<String>,
    /// Headers on every request (secrets filled in).
    pub headers: BTreeMap<String, String>,
    /// Credentials by security scheme: (username for basic, secret name,
    /// replaced by its value when secrets are resolved).
    pub auth: BTreeMap<String, (Option<String>, String)>,
}

impl std::fmt::Debug for OpenApiTemplate {
    // headers may hold credentials: names only
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenApiTemplate")
            .field("spec", &self.spec)
            .field("base_url", &self.base_url)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("auth", &self.auth.keys().collect::<Vec<_>>())
            .finish()
    }
}

struct Local {
    api: Api,
    http: reqwest::Client,
    extras: RequestExtras,
    creds: mcpgw_openapi::Credentials,
}

impl LocalTools for Local {
    fn list_tools(&self) -> BoxFuture<'_, Result<Vec<JsonObject>, String>> {
        Box::pin(async move { Ok(self.api.tools()) })
    }

    fn call_tool(
        &self,
        name: String,
        arguments: JsonObject,
    ) -> BoxFuture<'_, Result<JsonObject, String>> {
        Box::pin(async move {
            self.api
                .call_with(&self.http, &name, &arguments, &self.extras, &self.creds)
                .await
        })
    }
}

fn local_url(u: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| u.starts_with(p))
}

impl OpenApiTemplate {
    /// Load the spec, build the tools, connect.
    pub async fn connect(&self, timeout: Duration) -> Result<BackendConn, ProtoError> {
        mcpgw_core::tls::install();
        let err = |e: String| ProtoError::Connect(e);
        let base_is_local =
            self.base_url.as_deref().is_some_and(local_url) || local_url(&self.spec);
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(60));
        // an API on this machine is reached directly, never through a proxy
        if base_is_local {
            b = b.no_proxy();
        }
        let http = b.build().map_err(|e| err(e.to_string()))?;
        let remote = self.spec.starts_with("http://") || self.spec.starts_with("https://");
        let text = if remote {
            let r = http
                .get(&self.spec)
                .send()
                .await
                .map_err(|e| err(format!("fetching {}: {e}", self.spec)))?;
            if !r.status().is_success() {
                return Err(err(format!("fetching {}: HTTP {}", self.spec, r.status())));
            }
            r.text().await.map_err(|e| err(e.to_string()))?
        } else {
            std::fs::read_to_string(&self.spec).map_err(|e| err(format!("{}: {e}", self.spec)))?
        };
        let doc = mcpgw_openapi::parse(&text).map_err(|e| err(format!("{}: {e}", self.spec)))?;
        let api = mcpgw_openapi::api(
            &doc,
            self.base_url.as_deref(),
            remote.then_some(self.spec.as_str()),
        )
        .map_err(|e| err(format!("{}: {e}", self.spec)))?;
        let extras = RequestExtras {
            headers: self
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ..Default::default()
        };
        if let Some(unknown) = self
            .auth
            .keys()
            .find(|k| !api.security_schemes.contains_key(*k))
        {
            let known: Vec<&str> = api.security_schemes.keys().map(String::as_str).collect();
            return Err(err(format!(
                "auth: {} has no security scheme {unknown:?} (it has: {})",
                self.spec,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
        let creds = self
            .auth
            .iter()
            .map(|(scheme, (user, value))| {
                let c = match user {
                    Some(u) => mcpgw_openapi::Credential::Basic {
                        username: u.clone(),
                        password: value.clone(),
                    },
                    None => mcpgw_openapi::Credential::Secret(value.clone()),
                };
                (scheme.clone(), c)
            })
            .collect();
        let local = Local {
            api,
            http,
            extras,
            creds,
        };
        BackendConn::connect_local(Arc::new(local), timeout).await
    }
}
