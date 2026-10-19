# ADR-0002: Local storage

**Status:** Accepted (19 Oct 2026) · **Work items:** M1-03 (and every item that persists state)

## Context

The gateway keeps a small amount of durable state on one machine: registered
servers, captured tool manifests and pins, profiles, secret metadata, OAuth
token records and the audit log. There is one writer, the daemon, plus
short-lived CLI processes. A later cloud version (out of scope for v1) would
use Postgres.

The plan named `sqlx`.

## Decision

- **SQLite** in `<data dir>/mcpgw/mcpgw.db`, opened with **`rusqlite`**
  (bundled SQLite; no system library needed on any target).
- All access goes through the `Store` trait in `mcpgw-store`. Callers never
  see SQL, so another backend can be added without changing them.
- **WAL** journal mode and a 5 s busy timeout, so the CLI can read while the
  daemon writes.
- **Migrations:** numbered SQL steps applied in order on open, tracked in
  `PRAGMA user_version`; a database newer than the binary is refused.
- Domain objects are stored as JSON in a `TEXT` column next to the indexed
  key columns. This is the same serde form as the config file, so there is
  no second mapping to keep in sync.
- Secret **values** never go in this database (see ADR-0003); only metadata
  does.

`sqlx` was not chosen: the data set is tiny and local, async database access
buys nothing here, and the `sqlx` macros need either a live database or
committed query metadata at build time. rusqlite is synchronous, so the
daemon calls it from short critical sections (or `spawn_blocking` for bulk
work).

## Consequences

- One file to back up or delete; `mcpgw doctor` can check it.
- A Postgres backend for a future cloud mode means a second `Store`
  implementation, not a rewrite.
- Concurrent writers are serialised by SQLite. That is fine for one daemon.
