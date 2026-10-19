//! `mcpgw-core` — shared types, errors, and configuration.
//!
//! Provides the crate-wide [`Error`] / [`Result`] and the gateway [`Config`],
//! including the locked defaults:
//! - agent self-adding of servers is **disabled** by default;
//! - embeddings-based tool search is **enabled** by default.

pub mod auth;
pub mod config;

pub use auth::Token;
pub use config::{
    data_dir, validate_server_name, Config, GatewayConfig, SearchConfig, ServerConfig, Transport,
    CONFIG_ENV, TOOL_SEPARATOR,
};

use thiserror::Error;

/// Crate-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level gateway error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Configuration could not be loaded or validated.
    #[error("configuration error: {0}")]
    Config(String),
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse failure.
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    /// A named backend server was not found.
    #[error("unknown server: {0}")]
    UnknownServer(String),
    /// Persistence failure.
    #[error("store error: {0}")]
    Store(String),
    /// A protocol-level failure reported by another crate.
    #[error("protocol error: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays() {
        let e = Error::Config("missing field".into());
        assert_eq!(e.to_string(), "configuration error: missing field");
    }
}
