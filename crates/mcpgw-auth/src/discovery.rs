//! OAuth for remote MCP servers (M2-05), per the MCP authorization spec:
//!
//! 1. **Protected resource metadata** (RFC 9728): from the `resource_metadata`
//!    parameter of the server's `401 WWW-Authenticate`, else the well-known
//!    URLs `/.well-known/oauth-protected-resource{path}` and
//!    `/.well-known/oauth-protected-resource`.
//! 2. **Authorization server metadata** (RFC 8414 / OIDC discovery) for the
//!    first listed authorization server.
//! 3. **Client identity**: a client ID metadata document URL when the server
//!    supports them and one is configured; otherwise **dynamic client
//!    registration** (RFC 7591) as a public client with PKCE.
//!
//! The result is an ordinary [`OAuthProvider`] whose authorization and token
//! requests carry `resource` (RFC 8707), so the broker's flow, storage and
//! refresh work unchanged.

use std::{collections::BTreeMap, time::Duration};

use mcpgw_core::OAuthProvider;
use serde::Deserialize;
use serde_json::{Value, json};

/// What discovery found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The protected resource (the MCP server's canonical URL).
    pub resource: String,
    /// Authorization server issuer.
    pub issuer: String,
    /// Authorization endpoint.
    pub authorization_endpoint: String,
    /// Token endpoint.
    pub token_endpoint: String,
    /// DCR endpoint, if offered.
    pub registration_endpoint: Option<String>,
    /// Whether client ID metadata documents are accepted.
    pub cimd_supported: bool,
    /// Scopes the resource says it supports.
    pub scopes: Vec<String>,
}

#[derive(Deserialize)]
struct ResourceMetadata {
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Deserialize)]
struct ServerMetadata {
    issuer: Option<String>,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    #[serde(default)]
    client_id_metadata_document_supported: bool,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

fn client() -> Result<reqwest::Client, String> {
    mcpgw_core::tls::install();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

/// The `resource_metadata` URL from a `WWW-Authenticate: Bearer …` value.
pub fn resource_metadata_param(www_authenticate: &str) -> Option<String> {
    let i = www_authenticate.find("resource_metadata=")?;
    let rest = &www_authenticate[i + "resource_metadata=".len()..];
    let v = match rest.strip_prefix('"') {
        Some(q) => q.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    (!v.is_empty()).then(|| v.to_string())
}

/// Well-known URL `/.well-known/<name>` for `url`, path-aware first:
/// `https://h/a/b` → [`https://h/.well-known/<name>/a/b`, `https://h/.well-known/<name>`].
fn well_known(url: &reqwest::Url, name: &str) -> Vec<String> {
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.push(format!("{origin}/.well-known/{name}{path}"));
    }
    out.push(format!("{origin}/.well-known/{name}"));
    out
}

async fn get_json<T: serde::de::DeserializeOwned>(c: &reqwest::Client, url: &str) -> Option<T> {
    let r = c
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

fn require_secure(url: &str) -> Result<(), String> {
    let loopback = url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost");
    if url.starts_with("https://") || loopback {
        Ok(())
    } else {
        Err(format!("{url} must use https"))
    }
}

/// Discover how to authorize against the MCP server at `mcp_url`.
pub async fn discover(mcp_url: &str) -> Result<Discovered, String> {
    let c = client()?;
    let url = reqwest::Url::parse(mcp_url).map_err(|e| format!("bad url {mcp_url}: {e}"))?;

    // 1. protected resource metadata
    let mut candidates = Vec::new();
    let probe = c
        .post(url.clone())
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 0, "method": "ping"}))
        .send()
        .await
        .map_err(|e| format!("{mcp_url} unreachable: {e}"))?;
    if let Some(m) = probe
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find_map(resource_metadata_param)
    {
        candidates.push(m);
    }
    candidates.extend(well_known(&url, "oauth-protected-resource"));
    let mut prm = None;
    for u in &candidates {
        if let Some(m) = get_json::<ResourceMetadata>(&c, u).await {
            prm = Some(m);
            break;
        }
    }
    let prm = prm.ok_or_else(|| {
        format!("{mcp_url} publishes no OAuth protected resource metadata; configure its headers or an [[oauth]] app instead")
    })?;
    let issuer = prm
        .authorization_servers
        .first()
        .cloned()
        .ok_or("the resource metadata lists no authorization server")?;
    require_secure(&issuer)?;

    // 2. authorization server metadata
    let issuer_url =
        reqwest::Url::parse(&issuer).map_err(|e| format!("bad issuer {issuer}: {e}"))?;
    let mut as_urls = well_known(&issuer_url, "oauth-authorization-server");
    as_urls.extend(well_known(&issuer_url, "openid-configuration"));
    as_urls.push(format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    ));
    let mut asm = None;
    for u in &as_urls {
        if let Some(m) = get_json::<ServerMetadata>(&c, u).await {
            asm = Some(m);
            break;
        }
    }
    let asm = asm.ok_or_else(|| format!("no authorization server metadata found for {issuer}"))?;
    if let Some(i) = &asm.issuer
        && i.trim_end_matches('/') != issuer.trim_end_matches('/')
    {
        return Err(format!(
            "authorization server metadata is for {i}, not {issuer}"
        ));
    }
    if !asm.code_challenge_methods_supported.is_empty()
        && !asm
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
    {
        return Err(format!("{issuer} does not support PKCE with S256"));
    }
    require_secure(&asm.authorization_endpoint)?;
    require_secure(&asm.token_endpoint)?;
    Ok(Discovered {
        resource: prm.resource.unwrap_or_else(|| mcp_url.to_string()),
        issuer,
        authorization_endpoint: asm.authorization_endpoint,
        token_endpoint: asm.token_endpoint,
        registration_endpoint: asm.registration_endpoint,
        cimd_supported: asm.client_id_metadata_document_supported,
        scopes: prm.scopes_supported,
    })
}

/// A client registration (DCR result or metadata document URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Client id.
    pub client_id: String,
    /// Client secret, if the server issued one (stored in the vault).
    pub client_secret: Option<String>,
    /// How the client id was obtained: `metadata_document` or `dynamic`.
    pub method: &'static str,
}

/// Get a client id: the metadata document URL if supported and configured,
/// else dynamic registration.
pub async fn register(
    d: &Discovered,
    redirect_uri: &str,
    metadata_url: Option<&str>,
) -> Result<Registration, String> {
    if d.cimd_supported
        && let Some(u) = metadata_url
    {
        return Ok(Registration {
            client_id: u.to_string(),
            client_secret: None,
            method: "metadata_document",
        });
    }
    let Some(endpoint) = &d.registration_endpoint else {
        return Err(format!(
            "{} supports neither client ID metadata documents{} nor dynamic registration; add an [[oauth]] app for it",
            d.issuer,
            if d.cimd_supported {
                " (set gateway.client_metadata_url)"
            } else {
                ""
            }
        ));
    };
    require_secure(endpoint)?;
    let body = json!({
        "client_name": "mcpgw",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let r = client()?
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("client registration at {endpoint} failed: {e}"))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "client registration at {endpoint} answered {status}: {v}"
        ));
    }
    let client_id = v["client_id"]
        .as_str()
        .ok_or("client registration returned no client_id")?
        .to_string();
    Ok(Registration {
        client_id,
        client_secret: v["client_secret"].as_str().map(str::to_string),
        method: "dynamic",
    })
}

/// The provider for an MCP server: `name` is the server name, and
/// `client_secret` names the vault secret holding a DCR secret, if any.
pub fn provider(
    name: &str,
    d: &Discovered,
    reg: &Registration,
    client_secret: Option<String>,
) -> OAuthProvider {
    OAuthProvider {
        name: name.to_string(),
        preset: None,
        authorize_url: Some(d.authorization_endpoint.clone()),
        token_url: Some(d.token_endpoint.clone()),
        client_id: reg.client_id.clone(),
        client_secret,
        scopes: d.scopes.clone(),
        extra_params: BTreeMap::from([("resource".to_string(), d.resource.clone())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn www_authenticate_and_well_known() {
        assert_eq!(
            resource_metadata_param(
                r#"Bearer realm="x", resource_metadata="https://r/.well-known/oauth-protected-resource""#
            ),
            Some("https://r/.well-known/oauth-protected-resource".into())
        );
        assert_eq!(
            resource_metadata_param("Bearer resource_metadata=https://r/m, error=x"),
            Some("https://r/m".into())
        );
        assert_eq!(resource_metadata_param("Bearer realm=\"x\""), None);
        let u = reqwest::Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        assert_eq!(
            well_known(&u, "oauth-protected-resource"),
            [
                "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource"
            ]
        );
        assert!(require_secure("http://evil.example/token").is_err());
    }
}
