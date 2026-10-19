# M0-09 — Architecture decision records

**Story:** X · **Estimate:** 1 day · **Depends on:** M0-05
**Done when:** four ADRs merged and reviewed.
**Status:** ✅ Done

ADRs live in [`docs/adr/`](../docs/adr/README.md):

| ADR | Decision in one line |
|-----|----------------------|
| [0001 Protocol](../docs/adr/0001-protocol.md) | rmcp 3.5.x behind `mcpgw-proto` only; 2026-07-28 primary, 2025-11-25 accepted; backends use `Auto` with both versions listed; calls are rebuilt from name + arguments |
| [0002 Storage](../docs/adr/0002-storage.md) | SQLite through `rusqlite` behind the `Store` trait, WAL, `user_version` migrations, JSON columns; no `sqlx` |
| [0003 Vault crypto](../docs/adr/0003-vault-crypto.md) | Per-secret XChaCha20-Poly1305 DEKs wrapped by a KEK from the OS keychain or an Argon2id passphrase; `vault.json` at 0600 |
| [0004 Process model](../docs/adr/0004-process-model.md) | One daemon per user on `127.0.0.1:7777`, `mcpgw connect` stdio shim, backends in process groups / Job Objects, one actor per server, lazy start and idle stop |

ADR-0001 also covers M0-05's "findings written up in
`docs/adr/0001-protocol.md`".

Deviations from the plan's key dependencies, each explained in its ADR:
`rusqlite` instead of `sqlx`, Argon2id + XChaCha20-Poly1305 instead of
`age`, and (from M0-07) `toml` with spans instead of `figment`.
