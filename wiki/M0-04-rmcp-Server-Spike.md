# M0-04 — Spike: `rmcp` as MCP server to a client

**Status:** ✅ Done (spike; code is throwaway)

## Verdict

`rmcp` **can** be the client-facing server. Use it directly behind the `mcpgw-proto` seam; no wrapping or fork needed.

## Pinned version

- `rmcp = "3.5.1"`, only in `mcpgw-proto`. Features: `server`, `client`, `transport-io`, `macros`, `transport-child-process`.
- `check-deps.sh` still passes (rmcp confined to `mcpgw-proto`).

## What was built

- `crates/mcpgw-proto/src/spike_server.rs` — `EchoServer` with one `echo` tool (`#[tool_router]` / `#[tool_handler]`).
- `crates/mcpgw-proto/tests/spike_server.rs` — in-memory (duplex) end-to-end tests with an `rmcp` client.

## Findings

| Question | Result |
|----------|--------|
| `initialize` + `tools/list` + `tools/call` | Works (legacy handshake test) |
| Protocol `2026-07-28` | **Supported.** `ProtocolVersion::LATEST` is `2026-07-28`; client `ClientLifecycleMode::Discover` (`server/discover`) works against the server (test) |
| Backend version `2025-11-25` | Supported (`LATEST_WITH_INITIALIZE`); relevant to M0-05 |
| Versions known to rmcp | 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25, 2026-07-28 |
| stdio transport | Provided by feature `transport-io` (server) / `transport-child-process` (client); not run over real stdio in this spike (tests use in-memory duplex) |
| Streamable HTTP | Available via `transport-streamable-http-server` (not enabled or tested here) |

## Notes

- `2026-07-28` has **no `initialize`**: it uses `server/discover` and per-request metadata. The gateway must support both lifecycles for older clients.
- rmcp 3.x API differs from older tutorials: `ServerConfig` (alias of `InitializeResult`), `ServerConfig::new(capabilities)`, `CallToolResponse`.
- Reusable idea: `rmcp` ships `Auto` lifecycle mode that probes `server/discover` and falls back to `initialize`.

## Follow-ups

- Enable and test Streamable HTTP in a later item (gateway needs it for loopback-token auth, M1+).
- M0-05: verify the client side against a `2025-11-25` backend and design the bridge.
