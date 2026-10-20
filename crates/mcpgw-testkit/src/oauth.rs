//! A fake OAuth 2 authorization server for tests.
//!
//! `GET /authorize` immediately "consents" and redirects back with a code
//! (or `error=access_denied` when [`FakeOAuth::deny`] is set). `POST /token`
//! checks the PKCE verifier against the challenge and hands out
//! `at-<n>` / `rt-<n>` tokens; `grant_type=refresh_token` rotates them.
//! [`FakeOAuth::revoke_all`] makes every refresh fail with `invalid_grant`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Inner {
    codes: HashMap<String, (String, String, String)>, // code -> (challenge, client_id, redirect_uri)
    refresh: HashMap<String, bool>,                   // refresh token -> valid
    counter: u64,
    expires_in: u64,
    deny: bool,
    revoked: bool,
    token_requests: u64,
    refresh_requests: u64,
    last_challenge_method: Option<String>,
    last_scope: Option<String>,
}

/// A running fake authorization server (stops when dropped).
pub struct FakeOAuth {
    inner: Arc<Mutex<Inner>>,
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeOAuth {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type S = Arc<Mutex<Inner>>;

async fn authorize(State(s): State<S>, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut st = s.lock().unwrap();
    let redirect = q.get("redirect_uri").cloned().unwrap_or_default();
    let state = q.get("state").cloned().unwrap_or_default();
    st.last_challenge_method = q.get("code_challenge_method").cloned();
    st.last_scope = q.get("scope").cloned();
    let location = if st.deny {
        format!("{redirect}?error=access_denied&state={state}")
    } else {
        st.counter += 1;
        let code = format!("code-{}", st.counter);
        st.codes.insert(
            code.clone(),
            (
                q.get("code_challenge").cloned().unwrap_or_default(),
                q.get("client_id").cloned().unwrap_or_default(),
                redirect.clone(),
            ),
        );
        format!("{redirect}?code={code}&state={state}")
    };
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn bad(err: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": err}))).into_response()
}

fn issue(st: &mut Inner) -> Response {
    st.counter += 1;
    let rt = format!("rt-{}", st.counter);
    st.refresh.insert(rt.clone(), true);
    Json(json!({
        "access_token": format!("at-{}", st.counter),
        "refresh_token": rt,
        "expires_in": st.expires_in,
        "token_type": "bearer",
        "scope": st.last_scope.clone().unwrap_or_default(),
    }))
    .into_response()
}

async fn token(State(s): State<S>, Form(f): Form<HashMap<String, String>>) -> Response {
    let mut st = s.lock().unwrap();
    st.token_requests += 1;
    match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some((challenge, client, redirect)) =
                f.get("code").and_then(|c| st.codes.remove(c))
            else {
                return bad("invalid_grant");
            };
            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
            let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if expected != challenge {
                return bad("invalid_grant");
            }
            if f.get("client_id") != Some(&client) || f.get("redirect_uri") != Some(&redirect) {
                return bad("invalid_client");
            }
            issue(&mut st)
        }
        Some("refresh_token") => {
            st.refresh_requests += 1;
            let rt = f.get("refresh_token").cloned().unwrap_or_default();
            if st.revoked || st.refresh.get(&rt) != Some(&true) {
                return bad("invalid_grant");
            }
            st.refresh.insert(rt, false); // rotate
            issue(&mut st)
        }
        _ => bad("unsupported_grant_type"),
    }
}

impl FakeOAuth {
    /// Start one on a random loopback port; tokens last `expires_in` seconds.
    pub async fn start(expires_in: u64) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            expires_in,
            ..Default::default()
        }));
        let app = Router::new()
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { inner, base, task }
    }

    /// The next authorization is refused (`error=access_denied`).
    pub fn deny(&self, on: bool) {
        self.inner.lock().unwrap().deny = on;
    }

    /// Every refresh now fails with `invalid_grant`.
    pub fn revoke_all(&self) {
        self.inner.lock().unwrap().revoked = true;
    }

    /// Token endpoint calls so far (all grant types).
    pub fn token_requests(&self) -> u64 {
        self.inner.lock().unwrap().token_requests
    }

    /// Refresh-token grants so far.
    pub fn refresh_requests(&self) -> u64 {
        self.inner.lock().unwrap().refresh_requests
    }

    /// The `code_challenge_method` of the last authorization request.
    pub fn last_challenge_method(&self) -> Option<String> {
        self.inner.lock().unwrap().last_challenge_method.clone()
    }

    /// `[[oauth]]` config lines for this server.
    pub fn config(&self, name: &str) -> String {
        format!(
            "[[oauth]]\nname = \"{name}\"\nauthorize_url = \"{b}/authorize\"\ntoken_url = \"{b}/token\"\nclient_id = \"test-client\"\nscopes = [\"read\"]\n",
            b = self.base
        )
    }
}
