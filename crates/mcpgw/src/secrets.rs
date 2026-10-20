//! The daemon's secret source: the vault file, opened on first use and
//! re-read when another process (`mcpgw secret set`) changes it.

use std::{path::PathBuf, sync::Mutex, time::SystemTime};

use mcpgw_runtime::SecretResolver;
use mcpgw_vault::{KeySource, Vault};
use secrecy::SecretString;

/// Resolves secrets from a vault file.
pub struct VaultResolver {
    path: PathBuf,
    source: KeySource,
    open: Mutex<Option<(Vault, Option<SystemTime>)>>,
}

impl VaultResolver {
    /// A resolver for the vault at `path`, unlocked with `source`.
    pub fn new(path: PathBuf, source: KeySource) -> Self {
        Self {
            path,
            source,
            open: Mutex::new(None),
        }
    }

    fn mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
    }
}

impl SecretResolver for VaultResolver {
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String> {
        let mut guard = self
            .open
            .lock()
            .map_err(|_| "vault lock poisoned".to_string())?;
        let now = self.mtime();
        match guard.as_mut() {
            Some((v, seen)) if *seen != now => {
                v.refresh().map_err(|e| e.to_string())?;
                *seen = now;
            }
            Some(_) => {}
            None => {
                if !self.path.exists() {
                    return Ok(None);
                }
                let v = Vault::open(&self.path, &self.source).map_err(|e| e.to_string())?;
                *guard = Some((v, now));
            }
        }
        let (v, _) = guard.as_ref().expect("opened above");
        v.get(name).map_err(|e| e.to_string())
    }
}
