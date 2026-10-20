//! `mcpgw-vault` — envelope encryption for secrets (ADR-0003).
//!
//! Each secret is encrypted with its own random 256-bit data key (DEK)
//! using XChaCha20-Poly1305; the DEK is wrapped with the vault's key
//! (KEK). The KEK comes from a [`KeySource`]:
//!
//! - **keychain**: a random KEK kept in the OS credential store (macOS
//!   Keychain, Windows Credential Manager, Linux Secret Service);
//! - **passphrase**: Argon2id over a passphrase, for headless machines and
//!   CI (`MCPGW_VAULT_PASSPHRASE`).
//!
//! The vault file (`vault.json`, mode 0600, written atomically) holds only
//! ciphertext, nonces, wrapped keys and KDF parameters, never a value. Every
//! ciphertext is bound to the vault id and secret name through AEAD
//! associated data, so it can't be moved to another name or vault.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Environment variable holding the vault passphrase.
pub const PASSPHRASE_ENV: &str = "MCPGW_VAULT_PASSPHRASE";
/// Keychain service name.
pub const KEYCHAIN_SERVICE: &str = "mcpgw";

const VERSION: u32 = 1;
const CHECK_PLAINTEXT: &[u8] = b"mcpgw-vault-check";

/// Vault errors.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// Reading or writing the vault file failed.
    #[error("vault file {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// The file isn't a vault, or is from a newer mcpgw.
    #[error("vault file {0} is not readable: {1}")]
    Format(PathBuf, String),
    /// The key is wrong (bad passphrase, or a different keychain entry).
    #[error("cannot unlock the vault: {0}")]
    Locked(String),
    /// A ciphertext failed authentication (tampered or corrupted).
    #[error("secret {0:?} is corrupted or was tampered with")]
    Corrupt(String),
    /// The OS keychain is not usable here.
    #[error("OS keychain unavailable ({0}); set {PASSPHRASE_ENV} to use a passphrase instead")]
    Keychain(String),
    /// A secret name is invalid.
    #[error("{0}")]
    Name(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, VaultError>;

/// Where the vault key comes from.
#[derive(Clone)]
pub enum KeySource {
    /// A random key in the OS keychain.
    Keychain,
    /// A key derived from this passphrase.
    Passphrase(SecretString),
}

impl KeySource {
    /// `MCPGW_VAULT_PASSPHRASE` if set, else the keychain.
    pub fn from_env() -> Self {
        match std::env::var(PASSPHRASE_ENV) {
            Ok(p) if !p.is_empty() => Self::Passphrase(SecretString::from(p)),
            _ => Self::Keychain,
        }
    }
}

/// Argon2id parameters, stored in the file so they can be raised later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_kib: u32,
    /// Iterations.
    pub t: u32,
    /// Parallelism.
    pub p: u32,
}

impl Default for KdfParams {
    /// 64 MiB, 3 passes, 1 lane (ADR-0003).
    fn default() -> Self {
        Self {
            m_kib: 64 * 1024,
            t: 3,
            p: 1,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "source", rename_all = "lowercase")]
enum KekInfo {
    Keychain,
    Passphrase {
        kdf: String,
        #[serde(flatten)]
        params: KdfParams,
        salt: String,
    },
}

#[derive(Serialize, Deserialize, Clone)]
struct Sealed {
    nonce: String,
    ct: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    dek: Sealed,
    value: Sealed,
    created_at: u64,
    updated_at: u64,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    id: String,
    kek: KekInfo,
    check: Sealed,
    #[serde(default)]
    secrets: BTreeMap<String, Entry>,
}

type Key = Zeroizing<[u8; 32]>;

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random number generator");
    b
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn seal(key: &[u8; 32], plaintext: &[u8], aad: &str) -> Sealed {
    let nonce = random::<24>();
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .expect("encryption cannot fail for in-memory data");
    Sealed {
        nonce: B64.encode(nonce),
        ct: B64.encode(ct),
    }
}

fn open(key: &[u8; 32], s: &Sealed, aad: &str) -> Option<Zeroizing<Vec<u8>>> {
    let nonce = B64.decode(&s.nonce).ok()?;
    let ct = B64.decode(&s.ct).ok()?;
    if nonce.len() != 24 {
        return None;
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: aad.as_bytes(),
            },
        )
        .ok()
        .map(Zeroizing::new)
}

fn derive(passphrase: &SecretString, salt: &[u8], p: KdfParams) -> Result<Key> {
    let params = argon2::Params::new(p.m_kib, p.t, p.p, Some(32))
        .map_err(|e| VaultError::Locked(format!("bad KDF parameters: {e}")))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    a.hash_password_into(passphrase.expose_secret().as_bytes(), salt, key.as_mut())
        .map_err(|e| VaultError::Locked(format!("key derivation failed: {e}")))?;
    Ok(key)
}

/// Run a keychain operation on a plain thread: some credential stores
/// drive their own async runtime, which must not nest inside tokio's.
fn keychain<T: Send + 'static>(
    f: impl FnOnce() -> keyring::Result<T> + Send + 'static,
) -> Result<T> {
    std::thread::spawn(f)
        .join()
        .map_err(|_| VaultError::Keychain("keychain thread panicked".into()))?
        .map_err(|e| VaultError::Keychain(e.to_string()))
}

fn keychain_account(id: &str) -> String {
    format!("vault-kek-{id}")
}

/// Whether the OS keychain can be used on this machine.
pub fn keychain_available() -> bool {
    keychain(|| match keyring::Entry::store_status() {
        Ok(()) => Ok(()),
        Err(e) => Err(keyring::Error::NoStorageAccess(e.to_string().into())),
    })
    .is_ok()
}

/// An open, unlocked vault.
pub struct Vault {
    path: PathBuf,
    file: File,
    kek: Key,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("secrets", &self.file.secrets.len())
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// Open the vault at `path`, creating it (with `source` as its key) if
    /// the file doesn't exist.
    pub fn open_or_create(path: impl AsRef<Path>, source: &KeySource) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::open(path, source)
        } else {
            Self::create(path, source, KdfParams::default())
        }
    }

    /// Create a new, empty vault.
    pub fn create(path: impl AsRef<Path>, source: &KeySource, kdf: KdfParams) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let id: String = random::<8>().iter().map(|b| format!("{b:02x}")).collect();
        let (kek, info) = match source {
            KeySource::Keychain => {
                let kek: Key = Zeroizing::new(random::<32>());
                let encoded = Zeroizing::new(B64.encode(kek.as_ref()));
                let account = keychain_account(&id);
                keychain(move || {
                    keyring::Entry::new(KEYCHAIN_SERVICE, &account)?.set_password(&encoded)
                })?;
                (kek, KekInfo::Keychain)
            }
            KeySource::Passphrase(p) => {
                let salt = random::<16>();
                let kek = derive(p, &salt, kdf)?;
                (
                    kek,
                    KekInfo::Passphrase {
                        kdf: "argon2id".into(),
                        params: kdf,
                        salt: B64.encode(salt),
                    },
                )
            }
        };
        let check = seal(&kek, CHECK_PLAINTEXT, &format!("mcpgw-vault/v1/{id}/check"));
        let v = Self {
            path,
            file: File {
                version: VERSION,
                id,
                kek: info,
                check,
                secrets: BTreeMap::new(),
            },
            kek,
        };
        v.save()?;
        Ok(v)
    }

    /// Open and unlock an existing vault.
    pub fn open(path: impl AsRef<Path>, source: &KeySource) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path).map_err(|source| VaultError::Io {
            path: path.clone(),
            source,
        })?;
        let file: File = serde_json::from_str(&text)
            .map_err(|e| VaultError::Format(path.clone(), e.to_string()))?;
        if file.version > VERSION {
            return Err(VaultError::Format(
                path,
                format!("version {} is newer than supported {VERSION}", file.version),
            ));
        }
        let kek: Key = match (&file.kek, source) {
            (KekInfo::Keychain, _) => {
                let account = keychain_account(&file.id);
                let encoded = Zeroizing::new(keychain(move || {
                    keyring::Entry::new(KEYCHAIN_SERVICE, &account)?.get_password()
                })?);
                let bytes = Zeroizing::new(
                    B64.decode(encoded.as_bytes())
                        .map_err(|_| VaultError::Locked("keychain entry is malformed".into()))?,
                );
                let arr: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| VaultError::Locked("keychain entry is malformed".into()))?;
                Zeroizing::new(arr)
            }
            (KekInfo::Passphrase { params, salt, .. }, KeySource::Passphrase(p)) => {
                let salt = B64
                    .decode(salt)
                    .map_err(|e| VaultError::Format(path.clone(), e.to_string()))?;
                derive(p, &salt, *params)?
            }
            (KekInfo::Passphrase { .. }, KeySource::Keychain) => {
                return Err(VaultError::Locked(format!(
                    "this vault is protected by a passphrase; set {PASSPHRASE_ENV}"
                )));
            }
        };
        let aad = format!("mcpgw-vault/v1/{}/check", file.id);
        match open(&kek, &file.check, &aad) {
            Some(p) if p.as_slice() == CHECK_PLAINTEXT => {}
            _ => {
                return Err(VaultError::Locked(match file.kek {
                    KekInfo::Passphrase { .. } => "wrong passphrase".into(),
                    KekInfo::Keychain => "the keychain key does not match this vault".into(),
                }));
            }
        }
        Ok(Self { path, file, kek })
    }

    fn aad(&self, name: &str, part: &str) -> String {
        format!("mcpgw-vault/v1/{}/{name}/{part}", self.file.id)
    }

    /// Store or replace a secret. Every write uses a fresh DEK and nonces.
    pub fn set(&mut self, name: &str, value: &SecretString) -> Result<()> {
        mcpgw_core::validate_secret_name(name).map_err(|e| VaultError::Name(e.to_string()))?;
        let dek: Key = Zeroizing::new(random::<32>());
        let entry = Entry {
            dek: seal(&self.kek, dek.as_ref(), &self.aad(name, "dek")),
            value: seal(
                &dek,
                value.expose_secret().as_bytes(),
                &self.aad(name, "value"),
            ),
            created_at: self
                .file
                .secrets
                .get(name)
                .map(|e| e.created_at)
                .unwrap_or_else(now),
            updated_at: now(),
        };
        self.file.secrets.insert(name.to_string(), entry);
        self.save()
    }

    /// Decrypt a secret.
    pub fn get(&self, name: &str) -> Result<Option<SecretString>> {
        let Some(e) = self.file.secrets.get(name) else {
            return Ok(None);
        };
        let corrupt = || VaultError::Corrupt(name.to_string());
        let dek = open(&self.kek, &e.dek, &self.aad(name, "dek")).ok_or_else(corrupt)?;
        let dek: [u8; 32] = dek.as_slice().try_into().map_err(|_| corrupt())?;
        let dek = Zeroizing::new(dek);
        let value = open(&dek, &e.value, &self.aad(name, "value")).ok_or_else(corrupt)?;
        let text = String::from_utf8(value.to_vec()).map_err(|_| corrupt())?;
        Ok(Some(SecretString::from(text)))
    }

    /// Delete a secret; returns whether it existed.
    pub fn remove(&mut self, name: &str) -> Result<bool> {
        let existed = self.file.secrets.remove(name).is_some();
        if existed {
            self.save()?;
        }
        Ok(existed)
    }

    /// Secret names with creation and update times (Unix seconds).
    pub fn list(&self) -> Vec<(String, u64, u64)> {
        self.file
            .secrets
            .iter()
            .map(|(n, e)| (n.clone(), e.created_at, e.updated_at))
            .collect()
    }

    /// Where the vault key comes from.
    pub fn key_source(&self) -> &'static str {
        match self.file.kek {
            KekInfo::Keychain => "keychain",
            KekInfo::Passphrase { .. } => "passphrase",
        }
    }

    /// Path of the vault file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) -> Result<()> {
        let io = |source| VaultError::Io {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(io)?;
        let json = serde_json::to_vec_pretty(&self.file).expect("vault serialises");
        f.write_all(&json).map_err(io)?;
        f.sync_all().map_err(io)?;
        drop(f);
        std::fs::rename(&tmp, &self.path).map_err(io)
    }

    /// Delete the vault's keychain entry (when keychain-protected). Used when
    /// the vault file itself is deleted.
    pub fn forget_keychain_key(self) -> Result<()> {
        if let KekInfo::Keychain = self.file.kek {
            let account = keychain_account(&self.file.id);
            keychain(move || keyring::Entry::new(KEYCHAIN_SERVICE, &account)?.delete_credential())?;
        }
        Ok(())
    }
}
