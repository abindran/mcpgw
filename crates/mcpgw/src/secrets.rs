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

/// `mcpgw secret set|ls|rm`.
pub mod cmd {
    use std::io::{IsTerminal, Read};

    use mcpgw_config::Config;
    use mcpgw_core::SecretMeta;
    use mcpgw_store::{SqliteStore, Store};
    use mcpgw_vault::{KeySource, Vault};
    use secrecy::SecretString;

    use crate::AnyResult;

    fn vault(config: &Config) -> AnyResult<Vault> {
        Ok(Vault::open_or_create(
            config.gateway.vault_path(),
            &KeySource::from_env(),
        )?)
    }

    /// Store a secret read from the terminal (hidden) or stdin.
    pub fn set(config: &Config, name: &str, description: Option<String>) -> AnyResult<()> {
        mcpgw_core::validate_secret_name(name)?;
        let value = if std::io::stdin().is_terminal() {
            rpassword::prompt_password(format!("value for {name}: "))?
        } else {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s.trim_end_matches(['\r', '\n']).to_string()
        };
        if value.is_empty() {
            return Err("empty value; nothing stored".into());
        }
        let mut v = vault(config)?;
        v.set(name, &SecretString::from(value))?;
        let (_, created, updated) = v
            .list()
            .into_iter()
            .find(|(n, ..)| n == name)
            .expect("just stored");
        let store = SqliteStore::open(config.gateway.store_path())?;
        let prev = store.secret_meta(name)?;
        store.upsert_secret_meta(&SecretMeta {
            name: name.to_string(),
            description: description.or(prev.and_then(|p| p.description)),
            created_at: created,
            updated_at: updated,
            last_used_at: None,
        })?;
        println!("stored secret {name} (vault: {})", v.key_source());
        Ok(())
    }

    /// List secret names (never values).
    pub fn ls(config: &Config) -> AnyResult<()> {
        let path = config.gateway.vault_path();
        if !path.exists() {
            println!("no secrets");
            return Ok(());
        }
        let v = Vault::open(&path, &KeySource::from_env())?;
        let store = SqliteStore::open(config.gateway.store_path())?;
        let list = v.list();
        if list.is_empty() {
            println!("no secrets");
        }
        for (name, _, updated) in list {
            let desc = store
                .secret_meta(&name)?
                .and_then(|m| m.description)
                .map(|d| format!("\t{d}"))
                .unwrap_or_default();
            println!("{name}\tupdated {updated}{desc}");
        }
        Ok(())
    }

    /// Delete a secret.
    pub fn rm(config: &Config, name: &str) -> AnyResult<()> {
        let mut v = vault(config)?;
        let existed = v.remove(name)?;
        SqliteStore::open(config.gateway.store_path())?.remove_secret_meta(name)?;
        if existed {
            println!("removed secret {name}");
            Ok(())
        } else {
            Err(format!("no such secret: {name}").into())
        }
    }
}
