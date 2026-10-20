# M1-11 — Vault v1

**Story:** US2 · **Estimate:** 4 days · **Depends on:** M1-03
**Done when:** the secret file on disk contains no plaintext; it works on headless Linux with a passphrase.
**Status:** ✅ Done

## Design (`mcpgw-vault`, ADR-0003)

- **Envelope encryption.** Each `set` makes a fresh random 256-bit data key
  (DEK) and encrypts the value with **XChaCha20-Poly1305** (random 192-bit
  nonce). The DEK is wrapped by the vault key (KEK) with the same AEAD.
- **Bindings.** The AEAD associated data is
  `mcpgw-vault/v1/<vault id>/<name>/{dek,value}`, so a ciphertext copied to
  another name or another vault fails to decrypt (`Corrupt`).
- **KEK sources** (`KeySource`):
  - `Keychain`: a random KEK in the OS credential store via `keyring` 4
    (macOS Keychain, Windows Credential Manager, Linux Secret Service over
    pure-Rust zbus, which also builds for musl). Entry `mcpgw` /
    `vault-kek-<vault id>`.
  - `Passphrase`: **Argon2id**, m = 64 MiB, t = 3, p = 1, random 16-byte
    salt. The parameters are stored in the file so they can be raised
    later. `KeySource::from_env()` uses `MCPGW_VAULT_PASSPHRASE` when it is
    set: the headless path.
- **Key check.** The header has a value encrypted under the KEK; opening
  fails clearly with "wrong passphrase" or "the keychain key does not match".
  A passphrase vault opened without one says to set `MCPGW_VAULT_PASSPHRASE`.
- **File.** `vault.json`: version, id, KEK info, check, and per secret the
  wrapped DEK, sealed value, and created/updated times. Mode 0600, written
  atomically (temp file + rename).
- **Memory.** Keys and plaintexts are held in `Zeroizing` buffers; values
  cross the API as `secrecy::SecretString`; `Debug` shows only the secret
  count.
- Keychain calls run on a plain thread, since some stores drive their own
  async runtime, which must not nest inside tokio.
- Argon2, BLAKE2 and ChaCha20-Poly1305 are optimised even in debug builds,
  so tests and debug binaries don't wait seconds per unlock.

## Supply chain

`keyring`'s Linux store pulls `zvariant`, `zvariant_derive` and `zbus_names`
(zbus 5). RustSec marks them **unmaintained** because they were merged into
zbus 6; there is no vulnerability. They are ignored with that reason in
`deny.toml` and the new `.cargo/audit.toml`, to be removed when
`secret-service` moves to zbus 6.

## Tests (`crates/mcpgw-vault/tests/vault.rs`)

- `file_on_disk_contains_no_plaintext`: a canary token (built at runtime, so
  the canary scan stays clean), another secret and the passphrase never
  appear in the file, in plain or base64 form; the file is 0600.
- `passphrase_vault_reopens_with_the_right_passphrase_only`: the
  headless-Linux path with **default** Argon2id parameters; a wrong
  passphrase gives "wrong passphrase"; the keychain source is refused with
  the env hint.
- Update keeps `created_at`; remove; invalid names.
- Swapping one secret's entry into another name, or flipping a ciphertext
  character, gives `Corrupt`.
- A non-vault file gives `Format`.
- `keychain_vault_roundtrip` uses the real OS keychain. It runs only with
  `MCPGW_TEST_KEYCHAIN=1`, which CI sets on macOS and Windows, so local runs
  never trigger keychain prompts. It deletes its entry afterwards.

The CLI (`mcpgw secret set/ls/rm`) arrives in M1-13; secrets reach servers
in M1-12.
