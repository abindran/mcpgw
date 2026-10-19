//! `mcpgw-proto` — MCP protocol seam.
//!
//! This crate is the **only** place in the workspace that is permitted to
//! depend on `rmcp`. It owns all protocol framing, the server/client
//! endpoints, and bidirectional protocol-version bridging. Keeping it the
//! sole consumer of `rmcp` contains protocol churn to a single crate
//! (enforced in CI, see M0-02).

pub mod backend;
pub mod front;
pub mod spike_bridge;
pub mod spike_server;

/// Protocol versions the gateway bridges between.
///
/// The gateway speaks a newer protocol to LLM clients and may need to talk an
/// older protocol to backend tool servers; version bridging is proven in the
/// M0-04 / M0-05 spikes.
pub use backend::{BackendConn, JsonObject, ProtoError, StdioSpec};

pub mod version {
    /// Protocol version presented to LLM clients (server side).
    pub const CLIENT_FACING: &str = "2026-07-28";
    /// Protocol version spoken to backend tool servers (client side).
    pub const BACKEND_FACING: &str = "2025-11-25";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_distinct() {
        assert_ne!(version::CLIENT_FACING, version::BACKEND_FACING);
    }
}
