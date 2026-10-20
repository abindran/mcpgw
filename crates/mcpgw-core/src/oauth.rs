//! OAuth app definitions (M2-01): where to send the user and how to swap
//! the code for tokens. mcpgw ships no client credentials; you register
//! your own OAuth app and give its client id (and secret, via the vault).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One OAuth app (`[[oauth]]` in the config file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OAuthProvider {
    /// Name used in `mcpgw auth connect <name>` and secret names.
    pub name: String,
    /// Built-in endpoints to start from: `github`, `google` or `atlassian`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Authorization endpoint (overrides the preset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    /// Token endpoint (overrides the preset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    /// Your OAuth app's client id.
    pub client_id: String,
    /// Vault secret holding the client secret, for confidential apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Scopes to request (the preset's when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Extra authorization parameters (`audience`, `prompt`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_params: BTreeMap<String, String>,
}

/// Endpoints and defaults resolved from preset plus overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthEndpoints {
    /// Authorization endpoint.
    pub authorize_url: String,
    /// Token endpoint.
    pub token_url: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// Extra authorization parameters.
    pub extra_params: BTreeMap<String, String>,
}

fn preset(name: &str) -> Option<OAuthEndpoints> {
    let p = |a: &str, t: &str, s: &[&str], x: &[(&str, &str)]| OAuthEndpoints {
        authorize_url: a.into(),
        token_url: t.into(),
        scopes: s.iter().map(|x| x.to_string()).collect(),
        extra_params: x
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    Some(match name {
        "github" => p(
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            &["repo", "read:org"],
            &[],
        ),
        "google" => p(
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            &["openid", "email"],
            &[("access_type", "offline"), ("prompt", "consent")],
        ),
        "atlassian" => p(
            "https://auth.atlassian.com/authorize",
            "https://auth.atlassian.com/oauth/token",
            &["read:jira-work", "offline_access"],
            &[("audience", "api.atlassian.com"), ("prompt", "consent")],
        ),
        _ => return None,
    })
}

impl OAuthProvider {
    /// Endpoints after applying the preset and overrides.
    pub fn endpoints(&self) -> crate::Result<OAuthEndpoints> {
        let base = match &self.preset {
            Some(p) => Some(preset(p).ok_or_else(|| {
                crate::Error::Config(format!(
                    "oauth {}: unknown preset {p:?}; use github, google or atlassian",
                    self.name
                ))
            })?),
            None => None,
        };
        let pick = |own: &Option<String>, from: Option<&String>, what: &str| {
            own.clone().or_else(|| from.cloned()).ok_or_else(|| {
                crate::Error::Config(format!("oauth {}: set {what} or a preset", self.name))
            })
        };
        let mut e = OAuthEndpoints {
            authorize_url: pick(
                &self.authorize_url,
                base.as_ref().map(|b| &b.authorize_url),
                "authorize_url",
            )?,
            token_url: pick(
                &self.token_url,
                base.as_ref().map(|b| &b.token_url),
                "token_url",
            )?,
            scopes: if self.scopes.is_empty() {
                base.as_ref().map(|b| b.scopes.clone()).unwrap_or_default()
            } else {
                self.scopes.clone()
            },
            extra_params: base.map(|b| b.extra_params).unwrap_or_default(),
        };
        e.extra_params.extend(self.extra_params.clone());
        for u in [&e.authorize_url, &e.token_url] {
            let loopback = u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost");
            if !(u.starts_with("https://") || loopback) {
                return Err(crate::Error::Config(format!(
                    "oauth {}: {u} must use https",
                    self.name
                )));
            }
        }
        Ok(e)
    }
}

/// Vault secret name for a connected account: `oauth/<provider>@<account>`.
pub fn oauth_secret_name(provider: &str, account: &str) -> String {
    format!("oauth/{provider}@{account}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_overrides() {
        let p: OAuthProvider = toml::from_str(
            "name = \"github\"\npreset = \"github\"\nclient_id = \"Iv1.abc\"\nscopes = [\"repo\"]",
        )
        .unwrap();
        let e = p.endpoints().unwrap();
        assert_eq!(e.authorize_url, "https://github.com/login/oauth/authorize");
        assert_eq!(e.scopes, ["repo"]);
        let g: OAuthProvider =
            toml::from_str("name = \"g\"\npreset = \"google\"\nclient_id = \"x\"").unwrap();
        assert_eq!(
            g.endpoints().unwrap().extra_params["access_type"],
            "offline"
        );
        let bad: OAuthProvider = toml::from_str("name = \"x\"\nclient_id = \"x\"").unwrap();
        assert!(bad.endpoints().is_err());
        let http: OAuthProvider = toml::from_str(
            "name = \"x\"\nclient_id = \"x\"\nauthorize_url = \"http://evil/a\"\ntoken_url = \"https://t\"",
        )
        .unwrap();
        assert!(http.endpoints().unwrap_err().to_string().contains("https"));
        assert_eq!(oauth_secret_name("github", "work"), "oauth/github@work");
    }
}
