//! Pending approvals for tools a policy marks `approve` (M2-09).
//!
//! An approval is created for one tool call, bound to a SHA-256 digest of
//! the tool name and its exact arguments, and is single-use: once a call
//! goes through on it, it's gone. Approvals expire after [`APPROVAL_TTL`].
//! They are decided by the client (elicitation, MRTR) or by a person
//! (`mcpgw approve <id>`, the web UI later).

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mcpgw_proto::JsonObject;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

/// How long an undecided approval lives.
pub const APPROVAL_TTL: Duration = Duration::from_secs(600);

/// One approval, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalView {
    /// Short id (`mcpgw approve <id>`).
    pub id: String,
    /// Public tool name.
    pub tool: String,
    /// Arguments, shortened for display.
    pub arguments: String,
    /// Where the call came from (`profile frontend`).
    pub endpoint: String,
    /// Client name, if known.
    pub client: Option<String>,
    /// Creation time, Unix seconds.
    pub created_at: u64,
    /// `pending`, `approved` or `denied`.
    pub status: &'static str,
}

struct Pending {
    view: ApprovalView,
    digest: String,
    decided: Option<bool>,
    waiters: Vec<oneshot::Sender<bool>>,
    created: Instant,
}

/// All approvals of the daemon.
#[derive(Default)]
pub struct Approvals {
    inner: Mutex<HashMap<String, Pending>>,
}

/// Digest binding an approval to one tool and its arguments.
pub fn call_digest(tool: &str, args: &Option<JsonObject>) -> String {
    let mut canon = String::new();
    let v = serde_json::json!({"tool": tool, "arguments": args});
    mcpgw_core::manifest::canonical_json(&v, &mut canon);
    Sha256::digest(canon.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn short(args: &Option<JsonObject>) -> String {
    let s = args
        .as_ref()
        .map(|a| serde_json::Value::Object(a.clone()).to_string())
        .unwrap_or_else(|| "{}".into());
    if s.chars().count() > 300 {
        format!("{}…", s.chars().take(300).collect::<String>())
    } else {
        s
    }
}

impl Approvals {
    fn sweep(map: &mut HashMap<String, Pending>) {
        map.retain(|_, p| p.created.elapsed() < APPROVAL_TTL);
    }

    /// Open an approval for this call; returns its id.
    pub fn request(
        &self,
        tool: &str,
        args: &Option<JsonObject>,
        endpoint: &str,
        client: Option<String>,
    ) -> String {
        let mut b = [0u8; 6];
        getrandom::fill(&mut b).expect("OS random number generator");
        let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let view = ApprovalView {
            id: id.clone(),
            tool: tool.to_string(),
            arguments: short(args),
            endpoint: endpoint.to_string(),
            client,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            status: "pending",
        };
        let mut map = self.inner.lock().expect("approvals");
        Self::sweep(&mut map);
        map.insert(
            id.clone(),
            Pending {
                view,
                digest: call_digest(tool, args),
                decided: None,
                waiters: Vec::new(),
                created: Instant::now(),
            },
        );
        tracing::info!(%id, tool, endpoint, "approval requested");
        id
    }

    /// Decide an approval (person or client). Wakes anyone waiting.
    pub fn decide(&self, id: &str, approve: bool) -> Result<(), String> {
        let mut map = self.inner.lock().expect("approvals");
        Self::sweep(&mut map);
        let p = map
            .get_mut(id)
            .ok_or_else(|| format!("no pending approval {id}"))?;
        if p.decided.is_some() {
            return Err(format!("approval {id} was already decided"));
        }
        p.decided = Some(approve);
        p.view.status = if approve { "approved" } else { "denied" };
        for w in p.waiters.drain(..) {
            let _ = w.send(approve);
        }
        tracing::info!(id, approve, tool = %p.view.tool, "approval decided");
        Ok(())
    }

    /// Use an approval for a call: it must exist, be for exactly this tool
    /// and arguments, and be approved. It is removed either way.
    pub fn redeem(&self, id: &str, tool: &str, args: &Option<JsonObject>) -> Result<(), String> {
        let mut map = self.inner.lock().expect("approvals");
        Self::sweep(&mut map);
        let p = map
            .remove(id)
            .ok_or_else(|| format!("approval {id} is unknown or expired"))?;
        if p.digest != call_digest(tool, args) {
            return Err(format!("approval {id} was for a different call"));
        }
        match p.decided {
            Some(true) => Ok(()),
            Some(false) => Err(format!("{tool} was not approved")),
            None => Err(format!("approval {id} is still pending")),
        }
    }

    /// Wait for a decision, up to `timeout`; `None` on timeout or expiry.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Option<bool> {
        let rx = {
            let mut map = self.inner.lock().expect("approvals");
            let p = map.get_mut(id)?;
            if let Some(d) = p.decided {
                return Some(d);
            }
            let (tx, rx) = oneshot::channel();
            p.waiters.push(tx);
            rx
        };
        tokio::time::timeout(timeout, rx).await.ok()?.ok()
    }

    /// Forget an approval.
    pub fn cancel(&self, id: &str) {
        self.inner.lock().expect("approvals").remove(id);
    }

    /// Approvals not yet redeemed, oldest first.
    pub fn list(&self) -> Vec<ApprovalView> {
        let mut map = self.inner.lock().expect("approvals");
        Self::sweep(&mut map);
        let mut v: Vec<_> = map.values().map(|p| p.view.clone()).collect();
        v.sort_by_key(|a| a.created_at);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: serde_json::Value) -> Option<JsonObject> {
        v.as_object().cloned()
    }

    #[test]
    fn bound_to_the_exact_call_and_single_use() {
        let a = Approvals::default();
        let id = a.request(
            "gh__merge_pull_request",
            &args(json!({"pr": 7})),
            "profile x",
            None,
        );
        assert_eq!(a.list()[0].status, "pending");
        assert!(
            a.redeem(&id, "gh__merge_pull_request", &args(json!({"pr": 7})))
                .unwrap_err()
                .contains("pending")
        );

        let id = a.request(
            "gh__merge_pull_request",
            &args(json!({"pr": 7})),
            "profile x",
            None,
        );
        a.decide(&id, true).unwrap();
        assert!(a.decide(&id, false).is_err(), "decided twice");
        assert!(
            a.redeem(&id, "gh__merge_pull_request", &args(json!({"pr": 8})))
                .unwrap_err()
                .contains("different call")
        );

        let id = a.request("t", &args(json!({"x": 1, "y": 2})), "e", None);
        a.decide(&id, true).unwrap();
        // key order doesn't matter, the content does
        a.redeem(&id, "t", &args(json!({"y": 2, "x": 1}))).unwrap();
        assert!(
            a.redeem(&id, "t", &args(json!({"y": 2, "x": 1}))).is_err(),
            "single use"
        );

        let id = a.request("t", &None, "e", None);
        a.decide(&id, false).unwrap();
        assert!(
            a.redeem(&id, "t", &None)
                .unwrap_err()
                .contains("not approved")
        );
    }

    #[tokio::test]
    async fn waiters_wake_on_decision() {
        let a = std::sync::Arc::new(Approvals::default());
        let id = a.request("t", &None, "e", Some("cli".into()));
        let w = {
            let (a, id) = (a.clone(), id.clone());
            tokio::spawn(async move { a.wait(&id, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.decide(&id, true).unwrap();
        assert_eq!(w.await.unwrap(), Some(true));
        let id2 = a.request("t", &None, "e", None);
        assert_eq!(
            a.wait(&id2, Duration::from_millis(50)).await,
            None,
            "timeout"
        );
    }
}
