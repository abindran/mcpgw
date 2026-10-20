//! Audit events (M2-13). Stored hash-chained by `mcpgw-store`.
//!
//! An event names who did what to what. Tool calls record a SHA-256
//! **digest** of their arguments, never the arguments themselves, so the
//! log proves what was called without holding the data.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Records audit events (the daemon appends them to the store).
pub type Auditor = std::sync::Arc<dyn Fn(AuditEvent) + Send + Sync>;

/// Something worth recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    /// What happened: `tool.call`, `server.add`, `approval.decide`, …
    pub kind: String,
    /// Who: an endpoint and client (`profile web / Cursor`), `cli`, `api`.
    pub actor: String,
    /// What it was done to: a tool, server, profile, secret name, approval id.
    pub subject: String,
    /// SHA-256 of the canonical JSON of the arguments, for tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_digest: Option<String>,
    /// Small, non-secret details (outcome, duration, pin, …).
    #[serde(default)]
    pub detail: Value,
}

impl AuditEvent {
    /// An event with no argument digest.
    pub fn new(kind: &str, actor: &str, subject: &str, detail: Value) -> Self {
        Self {
            kind: kind.into(),
            actor: actor.into(),
            subject: subject.into(),
            args_digest: None,
            detail,
        }
    }
}

/// One stored, chained entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEntry {
    /// Position in the chain, from 1.
    pub seq: u64,
    /// Time, Unix milliseconds.
    pub at_ms: u64,
    /// The event.
    #[serde(flatten)]
    pub event: AuditEvent,
    /// Hash of the previous entry (64 zeros for the first).
    pub prev_hash: String,
    /// This entry's hash.
    pub hash: String,
}

/// The `prev_hash` of the first entry.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// SHA-256 over `prev_hash` and the canonical JSON of the entry's content.
pub fn entry_hash(prev_hash: &str, seq: u64, at_ms: u64, e: &AuditEvent) -> String {
    use sha2::{Digest, Sha256};
    let content = serde_json::json!({
        "seq": seq,
        "at_ms": at_ms,
        "kind": e.kind,
        "actor": e.actor,
        "subject": e.subject,
        "args_digest": e.args_digest,
        "detail": e.detail,
    });
    let mut canon = String::new();
    crate::manifest::canonical_json(&content, &mut canon);
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(b"\n");
    h.update(canon.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Digest of tool arguments (canonical JSON, so key order doesn't matter).
pub fn args_digest(args: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut canon = String::new();
    crate::manifest::canonical_json(args, &mut canon);
    Sha256::digest(canon.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where the first broken link is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBreak {
    /// The entry that doesn't verify.
    pub seq: u64,
    /// Why.
    pub reason: String,
}

impl std::fmt::Display for ChainBreak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "audit chain broken at entry {}: {}",
            self.seq, self.reason
        )
    }
}

/// Check a chain of entries in order: sequence numbers are consecutive from
/// the first, every `prev_hash` links to the entry before, and every hash
/// matches the content. Returns the number of entries checked.
pub fn verify_chain<'a>(
    entries: impl IntoIterator<Item = &'a AuditEntry>,
) -> Result<u64, ChainBreak> {
    let mut v = ChainVerifier::default();
    for e in entries {
        v.push(e)?;
    }
    Ok(v.checked())
}

/// Verifies a chain one entry at a time (for logs read in pages).
#[derive(Debug, Clone)]
pub struct ChainVerifier {
    prev: String,
    next_seq: u64,
}

impl Default for ChainVerifier {
    fn default() -> Self {
        Self {
            prev: GENESIS_HASH.to_string(),
            next_seq: 1,
        }
    }
}

impl ChainVerifier {
    /// Check the next entry.
    pub fn push(&mut self, e: &AuditEntry) -> Result<(), ChainBreak> {
        if e.seq != self.next_seq {
            return Err(ChainBreak {
                seq: e.seq,
                reason: format!(
                    "expected entry {} (an entry is missing or out of order)",
                    self.next_seq
                ),
            });
        }
        if e.prev_hash != self.prev {
            return Err(ChainBreak {
                seq: e.seq,
                reason: "does not link to the previous entry".into(),
            });
        }
        if entry_hash(&e.prev_hash, e.seq, e.at_ms, &e.event) != e.hash {
            return Err(ChainBreak {
                seq: e.seq,
                reason: "content does not match its hash (modified)".into(),
            });
        }
        self.prev = e.hash.clone();
        self.next_seq += 1;
        Ok(())
    }

    /// Entries checked so far.
    pub fn checked(&self) -> u64 {
        self.next_seq - 1
    }

    /// Hash of the last checked entry (the chain head).
    pub fn head(&self) -> &str {
        &self.prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chain(n: u64) -> Vec<AuditEntry> {
        let mut out: Vec<AuditEntry> = Vec::new();
        for seq in 1..=n {
            let prev = out
                .last()
                .map(|e| e.hash.clone())
                .unwrap_or_else(|| GENESIS_HASH.into());
            let event = AuditEvent::new(
                "tool.call",
                "default",
                &format!("fx__t{seq}"),
                json!({"outcome": "ok"}),
            );
            let hash = entry_hash(&prev, seq, 1000 + seq, &event);
            out.push(AuditEntry {
                seq,
                at_ms: 1000 + seq,
                event,
                prev_hash: prev,
                hash,
            });
        }
        out
    }

    #[test]
    fn verifies_and_detects_every_kind_of_tampering() {
        let good = chain(5);
        assert_eq!(verify_chain(&good), Ok(5));

        let mut edited = good.clone();
        edited[2].event.subject = "fx__other".into();
        assert_eq!(verify_chain(&edited).unwrap_err().seq, 3);

        let mut deleted = good.clone();
        deleted.remove(1);
        assert_eq!(verify_chain(&deleted).unwrap_err().seq, 3);

        let mut swapped = good.clone();
        swapped.swap(1, 2);
        assert!(verify_chain(&swapped).is_err());

        let mut rehashed = good.clone(); // edit and recompute that one hash
        rehashed[1].event.detail = json!({"outcome": "error"});
        rehashed[1].hash = entry_hash(
            &rehashed[1].prev_hash,
            2,
            rehashed[1].at_ms,
            &rehashed[1].event,
        );
        let b = verify_chain(&rehashed).unwrap_err();
        assert_eq!(
            (b.seq, b.reason.as_str()),
            (3, "does not link to the previous entry")
        );

        let mut truncated_front = good;
        truncated_front.remove(0);
        assert_eq!(verify_chain(&truncated_front).unwrap_err().seq, 2);
    }

    #[test]
    fn digests_ignore_key_order() {
        assert_eq!(
            args_digest(&json!({"a": 1, "b": [1, 2]})),
            args_digest(&json!({"b": [1, 2], "a": 1}))
        );
        assert_ne!(args_digest(&json!({"a": 1})), args_digest(&json!({"a": 2})));
    }
}
