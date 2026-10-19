# M1-03 — SQLite store

**Story:** X · **Estimate:** 3 days · **Depends on:** M0-06
**Done when:** migrations run on start; the store survives a restart.
**Status:** ✅ Done

## Schema (`mcpgw-store`, ADR-0002)

| Table | Key | Holds |
|-------|-----|-------|
| `kv` | `key` | raw bytes (gateway state) |
| `servers` | `name` | `ServerSpec` JSON |
| `manifests` | `server` | `ToolManifest` JSON, plus `sha256` and `captured_at` columns |
| `profiles` | `name` | `Profile` JSON |
| `secrets` | `name` | `SecretMeta` JSON (never values; those live in the vault) plus `updated_at` |

`Store` trait methods: `put`/`get`, `upsert_server`/`remove_server`/`server`/
`servers`, `put_manifest`/`manifest`/`manifests`/`remove_manifest`,
`upsert_profile`/`remove_profile`/`profile`/`profiles`,
`upsert_secret_meta`/`remove_secret_meta`/`secret_meta`/`secret_metas`.
Records are validated before they are written.

New core type `SecretMeta` (name, description, created/updated/last-used
times) and `validate_secret_name` (`[A-Za-z0-9_./@-]{1,128}`, so names like
`oauth/github@work` work).

## Migrations

`MIGRATIONS` is an append-only list of SQL steps. On open, every step past
`PRAGMA user_version` runs in its own transaction, and the version is bumped
in the same transaction. A database newer than the binary is refused with
"upgrade mcpgw". Step 1 is the pre-M1-03 schema (`CREATE TABLE IF NOT
EXISTS`), so existing databases upgrade in place.

Files are opened in **WAL** mode (5 s busy timeout), so the CLI can read
while the daemon writes.

## Tests

- CRUD and validation for every table.
- `survives_restart_uses_wal_and_rejects_future_schema`: write servers,
  manifests and secrets, reopen, read them back; journal mode is `wal`; a
  `user_version` of 99 is refused.
- `migrates_a_version_1_database`: a database left by the pre-M1-03 binary
  (version 1, one server) opens at the current version with its data intact.
