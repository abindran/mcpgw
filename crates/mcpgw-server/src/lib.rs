//! `mcpgw-server` — the MCP server side of the gateway.
//!
//! Exposes an MCP server endpoint to LLM clients. It never imports `rmcp`
//! directly; all protocol access goes through `mcpgw-proto`.

/// Client-facing protocol version this side presents (via `mcpgw-proto`).
pub fn client_facing_version() -> &'static str {
    mcpgw_proto::version::CLIENT_FACING
}
