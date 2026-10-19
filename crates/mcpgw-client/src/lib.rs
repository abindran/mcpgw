//! `mcpgw-client` — the MCP client side of the gateway.
//!
//! Connects out to backend tool servers as an MCP client and supervises their
//! lifecycle. It never imports `rmcp` directly; all protocol access goes
//! through `mcpgw-proto`.

pub mod aggregate;
pub mod manager;

pub use aggregate::{public_name, split_name};
pub use manager::{BackendManager, BackendState, BackendStatus, ManagerOptions};
pub use mcpgw_proto::{JsonObject, ProtoError};

/// Backend-facing protocol version this side speaks (via `mcpgw-proto`).
pub fn backend_facing_version() -> &'static str {
    mcpgw_proto::version::BACKEND_FACING
}
