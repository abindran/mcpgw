# M1-07 — Manifest capture

**Story:** US3, US4 · **Estimate:** 3 days · **Depends on:** M1-03, M1-05
**Done when:** `tools/list` answers without spawning any backend.
**Status:** ✅ Done

## How it works

- **At install.** `mcpgw add-server` starts the server once, reads its tools,
  stops it, and stores a `ToolManifest` with its SHA-256 pin:
  `added server fs (14 tools, pin 3f9a1c0b77de)`. `--no-introspect` skips
  this. If introspection fails, the server is still added and its tools are
  captured on first start. Re-adding a server drops the old pin;
  `remove-server` deletes it.
- **At daemon start.** Manifests are loaded from the store into the
  runtime's `Catalog`. Enabled servers with no manifest (for example ones
  declared only in `config.toml`) are started and stopped once to capture
  one, bounded to 30 s.
- **At first start.** A server that starts without a manifest records the
  tools it reports. New manifests are written back to the store through a
  persistence callback.
- **Listing.** `BackendManager::tools(name)` and the router's aggregated
  list read the catalog only, never a live connection, so `tools/list`
  starts nothing. Disabled servers list no tools. Tools come back sorted by
  name, as pinned.

The supervisor is now **lazy by default** (`ManagerOptions::eager = false`):
servers start on their first call. `list-servers --check` starts each server
explicitly and reports the result of that start.

## Tests

- `catalog_lists_tools_without_spawning` (runtime): introspection spawns
  once; with that manifest a lazy manager lists 7 tools, the server stays
  `Cold` and the spawn log shows no new start; the first call starts it.
- `tools_list_after_install_spawns_nothing` (binary, end to end): `mcpgw
  add-server` prints the pin and spawns once; `mcpgw serve --stdio` answers
  `tools/list` with **no** new spawn; `tools/call` starts the server.
- The manifest pin is checked in the existing tests (`verify()`, 64 hex
  characters).

## Also fixed here

- Windows: a crashed server's pipes can be reported closed only after the
  failing call returns, so the supervisor didn't notice the crash and its
  grandchildren survived. `BackendConn::is_closed` now also checks whether
  the process has exited, and a failed call triggers a recheck 250 ms later.
- Two status assertions waited for the actor's `CallEnd` (a test race seen
  on musl).
