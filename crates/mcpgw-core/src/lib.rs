//! `mcpgw-core` — shared types, errors, and configuration.
//!
//! Fully fleshed out in M0-03. This skeleton provides the crate-wide
//! `Error` / `Result` aliases that the rest of the workspace builds on.

use thiserror::Error;

/// Crate-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level gateway error type. Expanded in M0-03.
#[derive(Debug, Error)]
pub enum Error {
    /// Configuration could not be loaded or validated.
    #[error("configuration error: {0}")]
    Config(String),
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
