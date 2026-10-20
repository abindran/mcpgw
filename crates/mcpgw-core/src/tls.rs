//! TLS setup shared by every HTTP client in the process.
//!
//! reqwest is built with rustls but without a bundled crypto provider, so
//! the provider is chosen here: *ring*, which builds without cmake or a C
//! toolchain on every target (aws-lc-rs would need both). Certificates are
//! checked against the OS trust store (rustls-platform-verifier), so
//! corporate CAs work.

/// Install the process-wide crypto provider. Call before building any HTTP
/// client; calling it again is harmless.
pub fn install() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
