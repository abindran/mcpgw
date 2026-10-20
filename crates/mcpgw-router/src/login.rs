//! Login links for calls that need a credential (M2-06).
//!
//! When a call fails because an OAuth login is missing or expired, the
//! gateway starts the login and answers with the sign-in URL instead of a
//! dead end:
//!
//! - clients that support URL-mode elicitation get JSON-RPC error `-32042`
//!   ("URL elicitation required") with the URL, which they show to the user;
//! - other clients get a tool error whose text contains the link, so the
//!   model can pass it on.
//!
//! After the browser login the daemon resets the affected servers, so the
//! retried call starts them with the new token.

use std::sync::Arc;

use mcpgw_proto::front::{BoxFuture, CallContext, URL_ELICITATION_REQUIRED};
use mcpgw_runtime::{JsonObject, ProtoError};
use serde_json::json;

/// Start a login for `(provider, account)`; returns `(flow id, URL)`.
pub type LoginLink = Arc<
    dyn Fn(String, String) -> BoxFuture<'static, Result<(String, String), String>> + Send + Sync,
>;

/// Whether `e` means "the user has to sign in".
pub fn needs_login(e: &ProtoError) -> bool {
    let m = e.to_string();
    m.contains("requires authentication")
        || m.contains("login has expired")
        || (m.contains("oauth/") && m.contains("is not connected"))
}

/// The `(provider, account)` to log in to: named in the error as
/// `oauth/<provider>@<account>`, else the server's own login.
pub fn login_target(e: &ProtoError, server: &str, account: Option<&str>) -> (String, String) {
    let m = e.to_string();
    if let Some(i) = m.find("oauth/") {
        let rest = &m[i + "oauth/".len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || "_.@-".contains(c)))
            .unwrap_or(rest.len());
        if let Some((p, a)) = rest[..end].split_once('@')
            && !p.is_empty()
            && !a.is_empty()
        {
            return (p.to_string(), a.to_string());
        }
    }
    (
        server.to_string(),
        account.unwrap_or(mcpgw_core::DEFAULT_ACCOUNT).to_string(),
    )
}

/// The answer to give instead of `e`.
pub fn respond(
    ctx: &CallContext,
    provider: &str,
    account: &str,
    id: &str,
    url: &str,
) -> Result<JsonObject, ProtoError> {
    let who = if account == mcpgw_core::DEFAULT_ACCOUNT {
        provider.to_string()
    } else {
        format!("{provider} ({account})")
    };
    let message = format!("Sign in to {who} to use this tool.");
    if ctx.url_elicitation {
        return Err(ProtoError::Rpc {
            code: URL_ELICITATION_REQUIRED,
            message: message.clone(),
            data: Some(json!({
                "elicitations": [{
                    "mode": "url",
                    "elicitationId": id,
                    "url": url,
                    "message": message,
                }]
            })),
        });
    }
    let text = format!(
        "{message} Ask the user to open this link and sign in, then call the tool again:\n{url}"
    );
    Ok(
        json!({"content": [{"type": "text", "text": text}], "isError": true})
            .as_object()
            .cloned()
            .expect("object"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_logins_and_targets() {
        let expired = ProtoError::Connect(
            "the oauth/github@work login has expired; run `mcpgw auth connect github --account work`".into(),
        );
        assert!(needs_login(&expired));
        assert_eq!(
            login_target(&expired, "gh", None),
            ("github".into(), "work".into())
        );
        let remote =
            ProtoError::Connect("http://x/mcp requires authentication (HTTP 401/403)".into());
        assert!(needs_login(&remote));
        assert_eq!(
            login_target(&remote, "atl", Some("team")),
            ("atl".into(), "team".into())
        );
        let missing = ProtoError::Connect("oauth/jira@default is not connected; run …".into());
        assert_eq!(
            login_target(&missing, "j", None),
            ("jira".into(), "default".into())
        );
        assert!(!needs_login(&ProtoError::Connect(
            "spawn x: not found".into()
        )));
    }

    #[test]
    fn link_as_elicitation_or_text() {
        let ctx = CallContext {
            url_elicitation: true,
            ..Default::default()
        };
        let Err(ProtoError::Rpc { code, data, .. }) =
            respond(&ctx, "atl", "default", "s1", "https://l")
        else {
            panic!()
        };
        assert_eq!(code, -32042);
        assert_eq!(data.unwrap()["elicitations"][0]["url"], "https://l");
        let r = respond(&CallContext::default(), "github", "work", "s1", "https://l").unwrap();
        assert_eq!(r["isError"], true);
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(
            t.contains("Sign in to github (work)") && t.ends_with("https://l"),
            "{t}"
        );
    }
}
