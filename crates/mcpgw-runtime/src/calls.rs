//! Recent tool calls, for the web UI and `GET /api/v1/calls` (M2-12).
//! Only names, outcomes and timings are kept, never arguments or results.

use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

/// Calls kept.
pub const CALL_LOG_LEN: usize = 200;

/// One finished call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRecord {
    /// When it finished, Unix milliseconds.
    pub at_ms: u64,
    /// Endpoint (`the default endpoint`, `profile web`).
    pub endpoint: String,
    /// Public tool name (the target of `call_tool` in meta mode).
    pub tool: String,
    /// `ok`, `tool_error` (the tool answered `isError`) or `error`.
    pub outcome: &'static str,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// The client, if it said.
    pub client: Option<String>,
}

/// The most recent [`CALL_LOG_LEN`] calls.
#[derive(Default)]
pub struct CallLog {
    buf: Mutex<VecDeque<CallRecord>>,
}

impl CallLog {
    /// Record a call.
    pub fn record(
        &self,
        endpoint: &str,
        tool: &str,
        outcome: &'static str,
        duration_ms: u64,
        client: Option<String>,
    ) {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut b = self.buf.lock().expect("call log");
        if b.len() == CALL_LOG_LEN {
            b.pop_front();
        }
        b.push_back(CallRecord {
            at_ms,
            endpoint: endpoint.to_string(),
            tool: tool.to_string(),
            outcome,
            duration_ms,
            client,
        });
    }

    /// Up to `limit` calls, newest first.
    pub fn recent(&self, limit: usize) -> Vec<CallRecord> {
        self.buf
            .lock()
            .expect("call log")
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest() {
        let l = CallLog::default();
        for i in 0..(CALL_LOG_LEN + 5) {
            l.record("e", &format!("t{i}"), "ok", 1, None);
        }
        let r = l.recent(3);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].tool, format!("t{}", CALL_LOG_LEN + 4));
        assert_eq!(l.recent(1000).len(), CALL_LOG_LEN);
    }
}
