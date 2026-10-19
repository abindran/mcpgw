//! `mcpgw-client` — the MCP client side of the gateway.
//!
//! Connects out to backend tool servers as an MCP client. It never imports
//! `rmcp` directly; all protocol access goes through `mcpgw-proto`.

/// Backend-facing protocol version this side speaks (via `mcpgw-proto`).
pub fn backend_facing_version() -> &'static str {
    mcpgw_proto::version::BACKEND_FACING
}
