# M1-02 — SQLite-backed server registry

**Crate:** `mcpgw-store` · **Status:** ✅ Done

## What it adds
- `Store` trait (now `Send + Sync`): raw `put`/`get` plus `upsert_server`, `remove_server`, `server`, `servers`.
- `SqliteStore` (`rusqlite`, `bundled` SQLite — no system library needed):
  - `open(path)` creates parent directories; `open_in_memory()` for tests.
  - Tables `kv(key, value)` and `servers(name, config)`; the server row stores the `ServerConfig` as JSON so new config fields need no migration.
  - Schema version in `PRAGMA user_version` (currently `1`); opening a DB from a newer version fails with `Error::Store` instead of corrupting it.
  - A `Mutex<Connection>` with a 5 s busy timeout.
- `upsert_server` runs the same validation as the config file (name rules, non-empty command/URL, URL scheme).
- `effective_servers(config, store)` merges both sources. **The config file wins** on a name clash; the result is sorted by name.

## Design notes
- Config file = operator-declared servers; store = servers added at runtime (`mcpgw add-server`, later the admin API). Agent self-adding stays **off by default** (locked decision) and is not implemented in M1.
- New error variant: `mcpgw_core::Error::Store`.

## Tests
5 unit tests: kv round-trip/overwrite, server CRUD + ordering, invalid servers rejected, persistence across reopen + future-schema rejection, config-wins merge.
