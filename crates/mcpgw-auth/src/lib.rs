//! `mcpgw-auth` — the static loopback token, and the OAuth broker
//! ([`oauth`]).
//!
//! The loopback token is 256 random bits in a file only the owner can read.
//! The HTTP front door requires it as `Authorization: Bearer <token>`. The
//! stdio front needs none: only the parent process can reach the pipe.

pub mod oauth;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use mcpgw_core::{Error, Result};

/// Token length in bytes (hex-encoded to twice that).
pub const TOKEN_BYTES: usize = 32;

/// A loaded token. `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(***)")
    }
}

impl Token {
    /// Fresh random token.
    pub fn generate() -> Result<Self> {
        let mut buf = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut buf).map_err(|e| Error::Config(format!("rng failure: {e}")))?;
        Ok(Self(buf.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// The secret string (for writing into client configs).
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison against a presented token.
    pub fn verify(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.trim().as_bytes());
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// Check an HTTP `Authorization` header value (`Bearer <token>`).
    pub fn verify_bearer(&self, header: &str) -> bool {
        match header.trim().split_once(' ') {
            Some((scheme, tok)) if scheme.eq_ignore_ascii_case("bearer") => self.verify(tok),
            _ => false,
        }
    }

    fn parse(s: &str, path: &Path) -> Result<Self> {
        let s = s.trim();
        if s.len() != TOKEN_BYTES * 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Config(format!(
                "token file {} is malformed (expected {} hex chars)",
                path.display(),
                TOKEN_BYTES * 2
            )));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    /// Load an existing token file.
    ///
    /// On Unix, refuses files readable or writable by group/other.
    pub fn load(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path)?.permissions().mode();
            if mode & 0o077 != 0 {
                return Err(Error::Config(format!(
                    "token file {} has insecure permissions {:o}; run chmod 600",
                    path.display(),
                    mode & 0o777
                )));
            }
        }
        Self::parse(&fs::read_to_string(path)?, path)
    }

    /// Load the token, creating it (mode 0600) if the file does not exist.
    ///
    /// Safe against concurrent callers: if another process creates the file
    /// first, its token is loaded.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let t = Self::generate()?;
        match t.write_new(path) {
            Ok(()) => Ok(t),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Give the winner a moment to finish writing.
                for _ in 0..50 {
                    if let Ok(t) = Self::load(path) {
                        return Ok(t);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Self::load(path)
            }
            Err(e) => Err(e),
        }
    }

    /// Replace the token file with a fresh token.
    pub fn rotate(path: &Path) -> Result<Self> {
        let t = Self::generate()?;
        let tmp = tmp_path(path);
        let _ = fs::remove_file(&tmp);
        t.write_new(&tmp)?;
        fs::rename(&tmp, path)?;
        Ok(t)
    }

    fn write_new(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(path)?;
        writeln!(f, "{}", self.0)?;
        f.sync_all()?;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".tmp");
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mcpgw-auth-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn generate_verify_and_debug() {
        let t = Token::generate().unwrap();
        assert_eq!(t.expose().len(), 64);
        assert_ne!(t, Token::generate().unwrap());
        assert!(t.verify(t.expose()));
        assert!(t.verify(&format!(" {}\n", t.expose())));
        assert!(!t.verify(&t.expose()[..63]));
        assert!(!t.verify(""));
        assert!(t.verify_bearer(&format!("Bearer {}", t.expose())));
        assert!(t.verify_bearer(&format!("bearer {}", t.expose())));
        assert!(!t.verify_bearer(t.expose()));
        assert!(!t.verify_bearer(&format!("Basic {}", t.expose())));
        assert_eq!(format!("{t:?}"), "Token(***)");
    }

    #[test]
    fn create_load_rotate() {
        let d = dir("crud");
        let p = d.join("sub/token");
        let a = Token::load_or_create(&p).unwrap();
        assert_eq!(Token::load_or_create(&p).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let b = Token::rotate(&p).unwrap();
        assert_ne!(a, b);
        assert_eq!(Token::load(&p).unwrap(), b);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rejects_malformed_and_insecure() {
        let d = dir("bad");
        fs::create_dir_all(&d).unwrap();
        let p = d.join("token");
        fs::write(&p, "not-a-token").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(
                Token::load(&p)
                    .unwrap_err()
                    .to_string()
                    .contains("malformed")
            );
            fs::write(&p, "a".repeat(64)).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                Token::load(&p)
                    .unwrap_err()
                    .to_string()
                    .contains("insecure")
            );
        }
        fs::remove_dir_all(&d).unwrap();
    }
}
