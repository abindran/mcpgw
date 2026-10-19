//! Secret metadata. Values live only in the vault (ADR-0003).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the gateway knows about a secret without decrypting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretMeta {
    /// Secret name, e.g. `github_token` or `oauth/github@work`.
    pub name: String,
    /// Free-text note shown in listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Creation time, Unix seconds.
    pub created_at: u64,
    /// Last change of the value, Unix seconds.
    pub updated_at: u64,
    /// Last time the value was injected into a server, Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
}

/// Validate a secret name: 1-128 chars of `[A-Za-z0-9_./@-]`.
pub fn validate_secret_name(name: &str) -> crate::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./@-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(crate::Error::Config(format!(
            "secret name must be 1-128 characters of [A-Za-z0-9_./@-]: {name:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["github_token", "oauth/github@work", "a.b-c"] {
            validate_secret_name(ok).unwrap();
        }
        for bad in ["", "has space", "x$", &"a".repeat(129)] {
            assert!(validate_secret_name(bad).is_err(), "{bad:?}");
        }
    }
}
