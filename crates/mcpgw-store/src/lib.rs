//! `mcpgw-store` — persistence trait + local implementation.
//!
//! The `Store` trait keeps persistence pluggable (a future cloud backend can
//! implement it without changing callers). Fleshed out from M1-03 onward.

use mcpgw_core::Result;

/// Pluggable persistence backend for gateway state.
pub trait Store {
    /// Persist a raw value under `key`.
    fn put(&self, key: &str, value: &[u8]) -> Result<()>;
    /// Load a previously persisted value for `key`, if present.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
}
