//! `mcpgw-core` — domain types shared by every crate.
//!
//! Servers, tools, profiles, manifests, policies and the crate-wide
//! [`Error`] / [`Result`]. Nothing here performs I/O.

pub mod duration;
pub mod manifest;
pub mod oauth;
pub mod profile;
pub mod secret;
pub mod server;

pub use duration::Duration;
pub use manifest::{ManifestDiff, ToolDef, ToolManifest};
pub use oauth::{OAuthEndpoints, OAuthProvider, oauth_secret_name};
pub use profile::{Mode, Policy, Profile};
pub use secret::{SecretMeta, validate_secret_name};
pub use server::{
    SECRET_PLACEHOLDER, ServerSpec, TOOL_SEPARATOR, Transport, fill_placeholders,
    secret_placeholders, validate_server_name,
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
