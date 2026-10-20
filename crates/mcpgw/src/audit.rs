//! `mcpgw audit verify|export|tail` (M2-13), and the CLI's own entries.

use std::io::Write;

use mcpgw_config::Config;
use mcpgw_core::AuditEvent;
use mcpgw_store::{SqliteStore, Store};
use serde_json::Value;

use crate::AnyResult;

/// Record a change made by the CLI. The audit log must not stop the
/// command, so failures only warn.
pub fn record(config: &Config, kind: &str, subject: &str, detail: Value) {
    let r = SqliteStore::open(config.gateway.store_path())
        .and_then(|s| s.append_audit(&AuditEvent::new(kind, "cli", subject, detail)));
    if let Err(e) = r {
        eprintln!("mcpgw: warning: could not write the audit log: {e}");
    }
}

fn store(config: &Config) -> AnyResult<SqliteStore> {
    Ok(SqliteStore::open(config.gateway.store_path())?)
}

/// Check the whole chain; fails on the first broken entry.
pub fn verify(config: &Config) -> AnyResult<()> {
    match store(config)?.verify_audit()? {
        Ok(n) => {
            println!("audit log OK: {n} entries, chain intact");
            Ok(())
        }
        Err(b) => Err(b.to_string().into()),
    }
}

/// Write entries after `after` as JSON lines.
pub fn export(config: &Config, after: u64) -> AnyResult<()> {
    let s = store(config)?;
    let out = std::io::stdout();
    let mut out = out.lock();
    let mut next = after;
    loop {
        let page = s.audit_entries(next, 1000)?;
        let Some(last) = page.last() else { break };
        next = last.seq;
        for e in &page {
            writeln!(out, "{}", serde_json::to_string(e)?)?;
        }
    }
    Ok(())
}

/// The last `n` entries, readable.
pub fn tail(config: &Config, n: usize) -> AnyResult<()> {
    let s = store(config)?;
    let total = match s.verify_audit()? {
        Ok(t) => t,
        Err(b) => {
            eprintln!("mcpgw: warning: {b}");
            b.seq.saturating_sub(1)
        }
    };
    let entries = s.audit_entries(total.saturating_sub(n as u64), n)?;
    if entries.is_empty() {
        println!("the audit log is empty");
    }
    for e in entries {
        let digest = e
            .event
            .args_digest
            .as_deref()
            .map(|d| format!("\targs {}", &d[..12]))
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}{digest}",
            e.seq, e.at_ms, e.event.kind, e.event.actor, e.event.subject, e.event.detail
        );
    }
    Ok(())
}
