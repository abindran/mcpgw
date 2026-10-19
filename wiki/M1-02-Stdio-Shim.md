# M1-02 — stdio shim `mcpgw connect`

**Story:** US3, US4 · **Estimate:** 3 days · **Depends on:** M1-01
**Done when:** Cursor with a stdio entry works; the daemon auto-starts if it isn't running.
**Status:** ✅ Done

## Usage

```json
{ "mcpServers": { "mcpgw": { "command": "mcpgw", "args": ["connect"] } } }
```

## How it works

1. **Find the daemon.** A running daemon writes `daemon.json` next to the
   store (`pid`, `url`, `version`, `started_at`; mode 0600, written
   atomically) and removes it on clean shutdown, but only if the file is
   still its own. `connect` trusts the file only if `GET /healthz` at that
   address answers `200 ok`.
2. **Start it if needed.** Otherwise `connect` launches `mcpgw --config <same
   file> serve` detached: its own process group on Unix,
   `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on Windows, stdin and stdout
   closed, stderr appended to `daemon.log`. It then polls until the daemon is
   healthy (60 s limit). If two shims race, the loser's daemon fails to bind
   and exits, and both shims use the winner. A stale `daemon.json` from a
   crash fails the health check and is replaced.
3. **Relay.** `mcpgw_proto::relay::stdio_to_http` pumps whole JSON-RPC
   messages between rmcp's stdio transport and its Streamable HTTP client
   transport, with the bearer token. Messages are not interpreted. rmcp
   produces the right HTTP for each protocol: sessions for 2025-11-25, and
   `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` headers for
   2026-07-28. A byte-level relay would have had to reimplement those rules
   outside the protocol crate.

`Token::load_or_create` is now safe when the shim and the daemon create the
token file at the same moment.

## Tests (`crates/mcpgw/tests/connect.rs`)

- A stdio client (rmcp, 2026-07-28) runs `mcpgw connect` with no daemon
  running → the daemon starts, `daemon.json` appears, tools list and calls
  work.
- A second client, as 2025-11-25, reuses the **same** daemon (same pid) and
  gets a session through the relay.
- After both disconnect the daemon stays up; a third client reuses it.
- A stale `daemon.json` (dead pid, closed port) is replaced by a new daemon.
