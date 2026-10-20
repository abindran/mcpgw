//! `mcpgw-runtime` — supervisor, lifecycle state machine and launchers.
//!
//! Connects out to backend tool servers as an MCP client and supervises their
//! lifecycle. It never imports `rmcp` directly; all protocol access goes
//! through `mcpgw-proto`.

pub mod manager;
pub mod state;

pub use manager::{BackendManager, BackendStatus, ManagerOptions};
pub use mcpgw_proto::{JsonObject, ProtoError};
pub use state::State;
