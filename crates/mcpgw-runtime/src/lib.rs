//! `mcpgw-runtime` — supervisor, lifecycle state machine and launchers.
//!
//! Connects out to backend tool servers as an MCP client and supervises their
//! lifecycle. It never imports `rmcp` directly; all protocol access goes
//! through `mcpgw-proto`.

pub mod approvals;
pub mod calls;
pub mod catalog;
pub mod container;
pub mod manager;
pub mod openapi;
pub mod packages;
pub mod state;
pub mod wasm;

pub use approvals::{ApprovalView, Approvals};
pub use calls::{CallLog, CallRecord};
pub use catalog::{Catalog, Persist, introspect, introspect_with_secrets};
pub use container::ContainerInfo;
pub use manager::{BackendManager, BackendStatus, ManagerOptions, SecretResolver};
pub use mcpgw_proto::{JsonObject, ProtoError};
pub use state::State;
