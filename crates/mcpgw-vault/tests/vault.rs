//! Vault behaviour: encryption at rest, passphrase and keychain keys,
//! tamper detection.

use std::path::PathBuf;

use mcpgw_vault::{KdfParams, KeySource, Vault, VaultError};
use secrecy::{ExposeSecret, SecretString};

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mcpgw-vault-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("vault.json")
}

fn pass(p: &str) -> KeySource {
    KeySource::Passphrase(SecretString::from(p.to_string()))
}

/// Cheap KDF so tests stay fast; the default parameters are tested once.
fn quick() -> KdfParams {
    KdfParams {
        m_kib: 1024,
        t: 1,
        p: 1,
    }
}

fn canary() -> String {
    // Built at runtime so the source never contains a scannable canary.
    format!("mcpgw-canary-{}", "Q7".repeat(12))
}

#[test]
fn file_on_disk_contains_no_plaintext() {
    let path = temp("plain");
    let mut v = Vault::create(&path, &pass("correct horse"), quick()).unwrap();
    let secret = canary();
    v.set("github_token", &SecretString::from(secret.clone()))
        .unwrap();
    v.set("other", &SecretString::from("hunter2".to_string()))
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    for needle in [secret.as_str(), "hunter2", "correct horse"] {
        assert!(!text.contains(needle), "{needle:?} found in the vault file");
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&secret);
    assert!(!text.contains(&b64[..20]), "base64 of the secret found");
    assert_eq!(
        v.get("github_token").unwrap().unwrap().expose_secret(),
        secret
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

/// The headless-Linux path: a passphrase, default (64 MiB) Argon2id.
#[test]
fn passphrase_vault_reopens_with_the_right_passphrase_only() {
    let path = temp("reopen");
    {
        let mut v = Vault::open_or_create(&path, &pass("s3cret phrase")).unwrap();
        assert_eq!(v.key_source(), "passphrase");
        v.set("db/password", &SecretString::from("pg-pass".to_string()))
            .unwrap();
    }
    let v = Vault::open(&path, &pass("s3cret phrase")).unwrap();
    assert_eq!(
        v.get("db/password").unwrap().unwrap().expose_secret(),
        "pg-pass"
    );
    assert_eq!(v.list().len(), 1);
    assert!(v.get("missing").unwrap().is_none());

    let err = Vault::open(&path, &pass("wrong")).unwrap_err();
    assert!(
        matches!(&err, VaultError::Locked(m) if m == "wrong passphrase"),
        "{err}"
    );
    let err = Vault::open(&path, &KeySource::Keychain).unwrap_err();
    assert!(err.to_string().contains("MCPGW_VAULT_PASSPHRASE"), "{err}");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn update_remove_and_names() {
    let path = temp("crud");
    let mut v = Vault::create(&path, &pass("p"), quick()).unwrap();
    v.set("a", &SecretString::from("1".to_string())).unwrap();
    let created = v.list()[0].1;
    v.set("a", &SecretString::from("2".to_string())).unwrap();
    assert_eq!(v.get("a").unwrap().unwrap().expose_secret(), "2");
    assert_eq!(v.list()[0].1, created, "created_at is kept on update");
    assert!(v.remove("a").unwrap());
    assert!(!v.remove("a").unwrap());
    assert!(matches!(
        v.set("bad name", &SecretString::from("x".to_string())),
        Err(VaultError::Name(_))
    ));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn ciphertext_cannot_be_moved_or_tampered_with() {
    let path = temp("tamper");
    let mut v = Vault::create(&path, &pass("p"), quick()).unwrap();
    v.set("a", &SecretString::from("alpha".to_string()))
        .unwrap();
    v.set("b", &SecretString::from("bravo".to_string()))
        .unwrap();
    drop(v);

    // swap a's entry into b: the AEAD binding to the name must catch it
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let a = file["secrets"]["a"].clone();
    file["secrets"]["b"] = a;
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let v = Vault::open(&path, &pass("p")).unwrap();
    assert_eq!(v.get("a").unwrap().unwrap().expose_secret(), "alpha");
    assert!(matches!(v.get("b"), Err(VaultError::Corrupt(n)) if n == "b"));

    // flip one ciphertext character
    let ct = file["secrets"]["a"]["value"]["ct"]
        .as_str()
        .unwrap()
        .to_string();
    let mut chars: Vec<char> = ct.chars().collect();
    chars[4] = if chars[4] == 'A' { 'B' } else { 'A' };
    file["secrets"]["a"]["value"]["ct"] = serde_json::json!(chars.into_iter().collect::<String>());
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let v = Vault::open(&path, &pass("p")).unwrap();
    assert!(matches!(v.get("a"), Err(VaultError::Corrupt(_))));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn rejects_files_that_are_not_vaults() {
    let path = temp("format");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{}").unwrap();
    assert!(matches!(
        Vault::open(&path, &pass("p")),
        Err(VaultError::Format(..))
    ));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

/// Uses the real OS keychain, so it only runs when asked
/// (`MCPGW_TEST_KEYCHAIN=1`, set in CI on macOS and Windows).
#[test]
fn keychain_vault_roundtrip() {
    if std::env::var("MCPGW_TEST_KEYCHAIN").as_deref() != Ok("1") {
        eprintln!("skipped: set MCPGW_TEST_KEYCHAIN=1 to test the OS keychain");
        return;
    }
    let path = temp("keychain");
    {
        let mut v = Vault::create(&path, &KeySource::Keychain, KdfParams::default()).unwrap();
        assert_eq!(v.key_source(), "keychain");
        v.set("k", &SecretString::from("from keychain".to_string()))
            .unwrap();
    }
    let v = Vault::open(&path, &KeySource::Keychain).unwrap();
    assert_eq!(
        v.get("k").unwrap().unwrap().expose_secret(),
        "from keychain"
    );
    v.forget_keychain_key().unwrap();
    assert!(
        Vault::open(&path, &KeySource::Keychain).is_err(),
        "key was deleted"
    );
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
