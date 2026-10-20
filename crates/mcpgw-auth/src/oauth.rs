//! OAuth broker (M2-01): authorization code flow with PKCE (RFC 7636) and a
//! loopback redirect (RFC 8252) to the daemon's `/oauth/callback`.
//!
//! `start` creates a pending flow (random `state`, PKCE verifier) and the
//! URL to open in a browser. The provider redirects back with `code` and
//! `state`; `complete` checks the state, swaps the code for tokens at the
//! token endpoint with the verifier, and hands them to a [`TokenStore`].
//! Flows expire after ten minutes.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use mcpgw_core::OAuthProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a started flow stays valid.
pub const FLOW_TTL: Duration = Duration::from_secs(600);

/// Tokens for one connected account. `Debug` hides the tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    /// Access token.
    pub access_token: String,
    /// Refresh token, if the provider issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Expiry, Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Granted scopes (space-separated, as the provider returned them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Usually `bearer`.
    #[serde(default = "bearer")]
    pub token_type: String,
    /// Set when a refresh failed with `invalid_grant`: the user must log in
    /// again (M2-02).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_reauth: bool,
}

fn bearer() -> String {
    "bearer".into()
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// Where connected accounts are kept (the daemon uses the vault).
pub trait TokenStore: Send + Sync {
    /// Save tokens for `provider` / `account`.
    fn save(&self, provider: &str, account: &str, tokens: &TokenSet) -> Result<(), String>;
    /// Load them.
    fn load(&self, provider: &str, account: &str) -> Result<Option<TokenSet>, String>;
    /// Every connected `(provider, account)`.
    fn list(&self) -> Result<Vec<(String, String)>, String>;
    /// Forget one; returns whether it existed.
    fn remove(&self, provider: &str, account: &str) -> Result<bool, String>;
}

/// A PKCE pair.
pub struct Pkce {
    /// Kept secret until the token request.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`, sent with the authorization request.
    pub challenge: String,
}

fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("OS random number generator");
    URL_SAFE_NO_PAD.encode(b)
}

impl Pkce {
    /// A fresh pair (43-character verifier).
    pub fn new() -> Self {
        let verifier = random_b64(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// State of a flow, for the CLI to poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum FlowStatus {
    /// Waiting for the browser.
    Pending,
    /// Tokens stored.
    Done {
        /// Provider.
        provider: String,
        /// Account label.
        account: String,
    },
    /// Failed.
    Error {
        /// Why.
        message: String,
    },
}

struct Flow {
    provider: OAuthProvider,
    account: String,
    verifier: String,
    redirect_uri: String,
    started: Instant,
    status: FlowStatus,
}

/// Resolves a client secret name (vault) to its value.
pub type SecretLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Refresh a token when it has less than this left.
pub const REFRESH_LEAD: Duration = Duration::from_secs(300);

/// Runs OAuth flows for the daemon.
pub struct Broker {
    providers: Vec<OAuthProvider>,
    flows: Mutex<HashMap<String, Flow>>,
    /// One lock per connection, so concurrent refreshes don't race.
    refreshing: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    store: Arc<dyn TokenStore>,
    secrets: SecretLookup,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
    token_type: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Why a token request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The grant is no longer valid (revoked, expired refresh token):
    /// the user must connect again.
    InvalidGrant(String),
    /// Anything else (network, server error, bad response).
    Other(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGrant(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

impl Broker {
    /// A broker for `providers`, saving tokens to `store`.
    pub fn new(
        providers: Vec<OAuthProvider>,
        store: Arc<dyn TokenStore>,
        secrets: SecretLookup,
    ) -> Self {
        mcpgw_core::tls::install();
        Self {
            providers,
            flows: Mutex::new(HashMap::new()),
            refreshing: Mutex::new(HashMap::new()),
            store,
            secrets,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
        }
    }

    /// The token store.
    pub fn store(&self) -> &Arc<dyn TokenStore> {
        &self.store
    }

    /// A configured provider.
    pub fn provider(&self, name: &str) -> Option<&OAuthProvider> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Names of configured providers.
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name.clone()).collect()
    }

    /// Start a flow; returns `(state, authorization URL)`.
    pub fn start(
        &self,
        provider: &str,
        account: &str,
        redirect_uri: &str,
    ) -> Result<(String, String), String> {
        let p = self
            .provider(provider)
            .ok_or_else(|| {
                format!("no OAuth app named {provider:?}; add an [[oauth]] entry to the config")
            })?
            .clone();
        mcpgw_core::validate_secret_name(&mcpgw_core::oauth_secret_name(provider, account))
            .map_err(|e| e.to_string())?;
        let e = p.endpoints().map_err(|e| e.to_string())?;
        let pkce = Pkce::new();
        let state = random_b64(24);
        let mut url = reqwest::Url::parse(&e.authorize_url).map_err(|e| e.to_string())?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &p.client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("state", &state)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256");
            if !e.scopes.is_empty() {
                q.append_pair("scope", &e.scopes.join(" "));
            }
            for (k, v) in &e.extra_params {
                q.append_pair(k, v);
            }
        }
        let mut flows = self.flows.lock().expect("flows");
        flows.retain(|_, f| {
            f.started.elapsed() < FLOW_TTL || !matches!(f.status, FlowStatus::Pending)
        });
        flows.insert(
            state.clone(),
            Flow {
                provider: p,
                account: account.to_string(),
                verifier: pkce.verifier,
                redirect_uri: redirect_uri.to_string(),
                started: Instant::now(),
                status: FlowStatus::Pending,
            },
        );
        Ok((state, url.to_string()))
    }

    /// The status of a flow (unknown states read as an error).
    pub fn status(&self, state: &str) -> FlowStatus {
        match self.flows.lock().expect("flows").get(state) {
            Some(f) if f.status == FlowStatus::Pending && f.started.elapsed() >= FLOW_TTL => {
                FlowStatus::Error {
                    message: "the login took too long; start again".into(),
                }
            }
            Some(f) => f.status.clone(),
            None => FlowStatus::Error {
                message: "unknown login; start again".into(),
            },
        }
    }

    /// Handle the redirect: `code` (or the provider's `error`) for `state`.
    /// Returns the final status.
    pub async fn complete(
        &self,
        state: &str,
        code: Option<&str>,
        error: Option<&str>,
    ) -> FlowStatus {
        let taken = {
            let mut flows = self.flows.lock().expect("flows");
            match flows.get_mut(state) {
                Some(f) if f.status == FlowStatus::Pending && f.started.elapsed() < FLOW_TTL => {
                    Some((
                        f.provider.clone(),
                        f.account.clone(),
                        f.verifier.clone(),
                        f.redirect_uri.clone(),
                    ))
                }
                _ => None,
            }
        };
        let Some((provider, account, verifier, redirect_uri)) = taken else {
            return FlowStatus::Error {
                message: "unknown or expired login; start again with `mcpgw auth connect`".into(),
            };
        };
        let result = match (code, error) {
            (_, Some(e)) => Err(format!("the provider refused: {e}")),
            (None, None) => Err("no authorization code in the redirect".into()),
            (Some(code), None) => {
                let form = vec![
                    ("grant_type", "authorization_code".to_string()),
                    ("code", code.to_string()),
                    ("redirect_uri", redirect_uri),
                    ("code_verifier", verifier),
                ];
                match self.token_request(&provider, form).await {
                    Ok(t) => self
                        .store
                        .save(&provider.name, &account, &t)
                        .map_err(|e| format!("could not store the tokens: {e}")),
                    Err(e) => Err(e.to_string()),
                }
            }
        };
        let status = match result {
            Ok(()) => FlowStatus::Done {
                provider: provider.name.clone(),
                account,
            },
            Err(message) => FlowStatus::Error { message },
        };
        if let Some(f) = self.flows.lock().expect("flows").get_mut(state) {
            f.status = status.clone();
        }
        status
    }

    /// Whether `t` should be refreshed now (`lead` before expiry).
    pub fn due(t: &TokenSet, lead: Duration) -> bool {
        !t.needs_reauth
            && t.refresh_token.is_some()
            && t.expires_at.is_some_and(|at| at <= now() + lead.as_secs())
    }

    /// Connections that are due for a refresh.
    pub fn due_connections(&self, lead: Duration) -> Vec<(String, String)> {
        self.store
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|(p, a)| {
                self.store
                    .load(p, a)
                    .ok()
                    .flatten()
                    .is_some_and(|t| Self::due(&t, lead))
            })
            .collect()
    }

    /// Refresh one connection with its refresh token. A rejected grant
    /// (`invalid_grant`: revoked, or the refresh token expired) marks the
    /// connection `needs_reauth` and returns the error.
    pub async fn refresh(&self, provider: &str, account: &str) -> Result<TokenSet, TokenError> {
        let key = mcpgw_core::oauth_secret_name(provider, account);
        let lock = self
            .refreshing
            .lock()
            .expect("refresh locks")
            .entry(key)
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let current = self
            .store
            .load(provider, account)
            .map_err(TokenError::Other)?
            .ok_or_else(|| TokenError::Other(format!("{provider} ({account}) is not connected")))?;
        if current.needs_reauth {
            return Err(TokenError::InvalidGrant(format!(
                "{provider} ({account}) needs a new login"
            )));
        }
        // Someone else refreshed while we waited.
        if !Self::due(&current, Duration::from_secs(30)) && current.expires_at.is_some() {
            return Ok(current);
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            return Err(TokenError::InvalidGrant(format!(
                "{provider} ({account}) has no refresh token"
            )));
        };
        let p = self
            .provider(provider)
            .ok_or_else(|| TokenError::Other(format!("no OAuth app named {provider:?}")))?
            .clone();
        let form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.clone()),
        ];
        match self.token_request(&p, form).await {
            Ok(mut t) => {
                // Providers that don't rotate refresh tokens omit them.
                if t.refresh_token.is_none() {
                    t.refresh_token = Some(refresh_token);
                }
                self.store
                    .save(provider, account, &t)
                    .map_err(TokenError::Other)?;
                tracing::info!(provider, account, expires_at = ?t.expires_at, "refreshed OAuth token");
                Ok(t)
            }
            Err(TokenError::InvalidGrant(m)) => {
                let mut dead = current;
                dead.needs_reauth = true;
                let _ = self.store.save(provider, account, &dead);
                tracing::warn!(provider, account, error = %m, "refresh rejected; a new login is needed");
                Err(TokenError::InvalidGrant(m))
            }
            Err(e) => Err(e),
        }
    }

    /// POST to the token endpoint (authorization code or refresh).
    pub async fn token_request(
        &self,
        p: &OAuthProvider,
        mut form: Vec<(&str, String)>,
    ) -> Result<TokenSet, TokenError> {
        let e = p
            .endpoints()
            .map_err(|e| TokenError::Other(e.to_string()))?;
        form.push(("client_id", p.client_id.clone()));
        if let Some(name) = &p.client_secret {
            let secret = (self.secrets)(name).ok_or_else(|| {
                TokenError::Other(format!("client secret {name:?} is not in the vault"))
            })?;
            form.push(("client_secret", secret));
        }
        let resp = self
            .http
            .post(&e.token_url)
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|e| TokenError::Other(format!("token endpoint unreachable: {e}")))?;
        let status = resp.status();
        let body: TokenResponse = resp.json().await.map_err(|e| {
            TokenError::Other(format!(
                "token endpoint answered {status} with unreadable JSON: {e}"
            ))
        })?;
        if let Some(err) = body.error {
            let msg = match body.error_description {
                Some(d) => format!("{err}: {d}"),
                None => err.clone(),
            };
            return Err(if err == "invalid_grant" {
                TokenError::InvalidGrant(msg)
            } else {
                TokenError::Other(msg)
            });
        }
        let access_token = body.access_token.ok_or_else(|| {
            TokenError::Other(format!(
                "token endpoint answered {status} without an access token"
            ))
        })?;
        Ok(TokenSet {
            access_token,
            refresh_token: body.refresh_token,
            expires_at: body.expires_in.map(|s| now() + s),
            scope: body.scope,
            token_type: body.token_type.unwrap_or_else(bearer),
            needs_reauth: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_is_s256_of_the_verifier() {
        let p = Pkce::new();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(
            p.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()))
        );
        assert_ne!(Pkce::new().verifier, p.verifier);
    }

    #[test]
    fn refresh_is_due_only_near_expiry_with_a_refresh_token() {
        let t = |expires_in: Option<u64>, rt: bool, dead: bool| TokenSet {
            access_token: "a".into(),
            refresh_token: rt.then(|| "r".into()),
            expires_at: expires_in.map(|s| now() + s),
            scope: None,
            token_type: "bearer".into(),
            needs_reauth: dead,
        };
        let lead = Duration::from_secs(300);
        assert!(Broker::due(&t(Some(60), true, false), lead));
        assert!(!Broker::due(&t(Some(3600), true, false), lead));
        assert!(
            !Broker::due(&t(Some(60), false, false), lead),
            "nothing to refresh with"
        );
        assert!(!Broker::due(&t(None, true, false), lead), "no expiry");
        assert!(
            !Broker::due(&t(Some(60), true, true), lead),
            "needs a new login"
        );
    }

    #[test]
    fn token_set_debug_hides_tokens() {
        let t = TokenSet {
            access_token: "AT-secret".into(),
            refresh_token: Some("RT-secret".into()),
            expires_at: Some(1),
            scope: None,
            token_type: "bearer".into(),
            needs_reauth: false,
        };
        let d = format!("{t:?}");
        assert!(!d.contains("AT-secret") && !d.contains("RT-secret"), "{d}");
    }
}
