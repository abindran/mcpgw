//! The daemon's secrets: one vault file, opened on first use and re-read
//! when another process (`mcpgw secret set`) changes it. It backs both
//! secret injection ([`VaultResolver`]) and OAuth tokens ([`VaultTokens`]).

use std::{path::PathBuf, sync::Arc, sync::Mutex, time::SystemTime};

use mcpgw_auth::oauth::{TokenSet, TokenStore};
use mcpgw_runtime::SecretResolver;
use mcpgw_vault::{KeySource, Vault};
use secrecy::{ExposeSecret, SecretString};

/// A vault shared by everything in the daemon.
pub struct SharedVault {
    path: PathBuf,
    source: KeySource,
    open: Mutex<Option<(Vault, Option<SystemTime>)>>,
}

impl SharedVault {
    /// The vault at `path`, unlocked with `source` when first needed.
    pub fn new(path: PathBuf, source: KeySource) -> Arc<Self> {
        Arc::new(Self {
            path,
            source,
            open: Mutex::new(None),
        })
    }

    fn mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
    }

    /// Run `f` on the up-to-date vault. With `create`, a missing vault is
    /// created; otherwise `f` gets `None`.
    pub fn with<T>(
        &self,
        create: bool,
        f: impl FnOnce(Option<&mut Vault>) -> Result<T, String>,
    ) -> Result<T, String> {
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
            None if !self.path.exists() && !create => return f(None),
            None => {
                let v =
                    Vault::open_or_create(&self.path, &self.source).map_err(|e| e.to_string())?;
                *guard = Some((v, self.mtime()));
            }
        }
        let (v, seen) = guard.as_mut().expect("opened above");
        let out = f(Some(v));
        *seen = self.mtime();
        out
    }
}

/// Secret injection from the vault. `oauth/<provider>@<account>` names
/// resolve to that connection's current access token.
pub struct VaultResolver {
    vault: Arc<SharedVault>,
    /// Refreshes a due OAuth token before it is injected (daemon only).
    refresher: Option<(Arc<mcpgw_auth::oauth::Broker>, tokio::runtime::Handle)>,
}

impl VaultResolver {
    /// A resolver over an existing shared vault.
    pub fn shared(vault: Arc<SharedVault>) -> Self {
        Self {
            vault,
            refresher: None,
        }
    }

    /// Refresh OAuth tokens that are about to expire through `broker`.
    /// `resolve` must then run off the async threads (it does: the runtime
    /// calls it from `spawn_blocking`).
    pub fn with_refresh(
        mut self,
        broker: Arc<mcpgw_auth::oauth::Broker>,
        rt: tokio::runtime::Handle,
    ) -> Self {
        self.refresher = Some((broker, rt));
        self
    }
}

impl SecretResolver for VaultResolver {
    fn resolve(&self, name: &str) -> Result<Option<SecretString>, String> {
        let value = self.vault.with(false, |v| match v {
            Some(v) => v.get(name).map_err(|e| e.to_string()),
            None => Ok(None),
        })?;
        if !name.starts_with("oauth/") {
            return Ok(value);
        }
        let Some(v) = value else { return Ok(None) };
        let t: TokenSet = serde_json::from_str(v.expose_secret())
            .map_err(|e| format!("stored OAuth tokens for {name} are unreadable: {e}"))?;
        let relogin = || {
            format!(
                "the {name} login has expired; run `mcpgw auth connect {}`",
                name.trim_start_matches("oauth/")
                    .replace('@', " --account ")
            )
        };
        if t.needs_reauth {
            return Err(relogin());
        }
        use mcpgw_auth::oauth::{Broker, TokenError};
        if let Some((broker, rt)) = &self.refresher
            && Broker::due(&t, std::time::Duration::from_secs(60))
        {
            let (provider, account) = name
                .trim_start_matches("oauth/")
                .split_once('@')
                .ok_or_else(|| format!("bad OAuth secret name {name}"))?;
            match rt.block_on(broker.refresh(provider, account)) {
                Ok(fresh) => return Ok(Some(SecretString::from(fresh.access_token))),
                Err(TokenError::InvalidGrant(_)) => return Err(relogin()),
                Err(TokenError::Other(e)) => {
                    let expired = t.expires_at.is_some_and(|at| at <= now_secs());
                    if expired {
                        return Err(format!(
                            "the {name} token expired and refreshing it failed: {e}"
                        ));
                    }
                    tracing::warn!(secret = name, error = %e, "token refresh failed; using the current token");
                }
            }
        }
        Ok(Some(SecretString::from(t.access_token)))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// OAuth tokens kept in the vault as `oauth/<provider>@<account>`.
pub struct VaultTokens {
    vault: Arc<SharedVault>,
}

impl VaultTokens {
    /// Tokens in `vault`.
    pub fn new(vault: Arc<SharedVault>) -> Self {
        Self { vault }
    }
}

impl TokenStore for VaultTokens {
    fn save(&self, provider: &str, account: &str, tokens: &TokenSet) -> Result<(), String> {
        let json = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
        let name = mcpgw_core::oauth_secret_name(provider, account);
        self.vault.with(true, |v| {
            v.expect("created")
                .set(&name, &SecretString::from(json))
                .map_err(|e| e.to_string())
        })
    }

    fn load(&self, provider: &str, account: &str) -> Result<Option<TokenSet>, String> {
        let name = mcpgw_core::oauth_secret_name(provider, account);
        let v = self.vault.with(false, |v| match v {
            Some(v) => v.get(&name).map_err(|e| e.to_string()),
            None => Ok(None),
        })?;
        v.map(|s| serde_json::from_str(s.expose_secret()).map_err(|e| e.to_string()))
            .transpose()
    }

    fn list(&self) -> Result<Vec<(String, String)>, String> {
        self.vault.with(false, |v| {
            Ok(v.map(|v| {
                v.list()
                    .into_iter()
                    .filter_map(|(n, ..)| {
                        let rest = n.strip_prefix("oauth/")?;
                        let (p, a) = rest.split_once('@')?;
                        Some((p.to_string(), a.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default())
        })
    }

    fn remove(&self, provider: &str, account: &str) -> Result<bool, String> {
        let name = mcpgw_core::oauth_secret_name(provider, account);
        self.vault.with(false, |v| match v {
            Some(v) => v.remove(&name).map_err(|e| e.to_string()),
            None => Ok(false),
        })
    }
}

/// Secrets for the admin API: values into the vault, metadata in the store.
pub struct ApiSecrets {
    vault: Arc<SharedVault>,
    store: Arc<mcpgw_store::SqliteStore>,
}

impl ApiSecrets {
    /// Over `vault` and `store`.
    pub fn new(vault: Arc<SharedVault>, store: Arc<mcpgw_store::SqliteStore>) -> Self {
        Self { vault, store }
    }
}

impl mcpgw_api::v1::SecretBackend for ApiSecrets {
    fn list(&self) -> Result<Vec<mcpgw_core::SecretMeta>, String> {
        use mcpgw_store::Store;
        let names = self
            .vault
            .with(false, |v| Ok(v.map(|v| v.list()).unwrap_or_default()))?;
        Ok(names
            .into_iter()
            .map(|(name, created_at, updated_at)| {
                let stored = self.store.secret_meta(&name).ok().flatten();
                mcpgw_core::SecretMeta {
                    description: stored.as_ref().and_then(|m| m.description.clone()),
                    last_used_at: stored.and_then(|m| m.last_used_at),
                    name,
                    created_at,
                    updated_at,
                }
            })
            .collect())
    }

    fn set(&self, name: &str, value: &str, description: Option<String>) -> Result<(), String> {
        use mcpgw_store::Store;
        let (created_at, updated_at) = self.vault.with(true, |v| {
            let v = v.expect("created");
            v.set(name, &SecretString::from(value.to_string()))
                .map_err(|e| e.to_string())?;
            Ok(v.list()
                .into_iter()
                .find(|(n, ..)| n == name)
                .map(|(_, c, u)| (c, u))
                .unwrap_or_default())
        })?;
        let prev = self.store.secret_meta(name).map_err(|e| e.to_string())?;
        self.store
            .upsert_secret_meta(&mcpgw_core::SecretMeta {
                name: name.to_string(),
                description: description.or(prev.and_then(|p| p.description)),
                created_at,
                updated_at,
                last_used_at: None,
            })
            .map_err(|e| e.to_string())
    }

    fn remove(&self, name: &str) -> Result<bool, String> {
        use mcpgw_store::Store;
        let existed = self.vault.with(false, |v| match v {
            Some(v) => v.remove(name).map_err(|e| e.to_string()),
            None => Ok(false),
        })?;
        let _ = self.store.remove_secret_meta(name);
        Ok(existed)
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
