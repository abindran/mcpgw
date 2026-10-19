# ADR-0003: Vault cryptography

**Status:** Accepted (19 Oct 2026) · **Work items:** M1-11, M1-12, M2-01

## Context

Secrets (API keys, OAuth tokens) must be usable by backend servers but never
visible to the LLM, never written in plaintext, and must work on desktops
with an OS keychain and on headless Linux or CI without one.

## Decision

**Envelope encryption** in `mcpgw-vault`:

| Layer | Choice |
|-------|--------|
| Data key (DEK) | Fresh random 256-bit key per secret, per write |
| Secret encryption | **XChaCha20-Poly1305** (`chacha20poly1305`), random 192-bit nonce, AAD = `mcpgw-vault/v1/<secret name>` so a ciphertext can't be moved to another name |
| Key-encryption key (KEK) | 256-bit; wraps each DEK with XChaCha20-Poly1305 (AAD binds the name too) |
| KEK provider: keychain | Random KEK stored in the OS keychain (`keyring`: macOS Keychain, Windows Credential Manager, Linux Secret Service), service `mcpgw`, account `vault-kek` |
| KEK provider: passphrase | **Argon2id** (m = 64 MiB, t = 3, p = 1, random 16-byte salt) over the passphrase from `MCPGW_VAULT_PASSPHRASE` or a prompt |
| Key check | The vault header stores a value encrypted under the KEK, so a wrong passphrase fails clearly instead of producing garbage |
| Storage | `<data dir>/mcpgw/vault.json`, mode 0600, written atomically (temp file + rename) |
| Memory | Keys and plaintexts in `Zeroizing` buffers; `secrecy::SecretString` in APIs; `Debug` never prints values |

**Rotation:** every write uses a new DEK and nonce. Changing the KEK
re-wraps the DEKs; the secrets themselves are not re-encrypted.

**Use:** secrets reach a backend only as environment variables (or, from
M3-04, files in tmpfs) at spawn. Bound values are redacted from tool
results, logs and API responses (M1-12).

The plan listed `age` for the passphrase path. Argon2id plus the same AEAD as
the keychain path gives one format and a memory-hard KDF; `age`'s scrypt
recipient would have meant two formats.

## Consequences

- Losing the keychain entry or the passphrase makes the vault unreadable.
  This is intended; `mcpgw doctor` explains it.
- Anyone who can read the vault file *and* the keychain entry as the user
  can decrypt it. The threat model is other local users, backups and the
  LLM, not malware running as the same user.
- Headless setups need `MCPGW_VAULT_PASSPHRASE` in the daemon's environment.
