# M0-05 — Spike: `rmcp` as MCP client + version bridging

**Status:** ✅ Done (spike; code is throwaway)

## Verdict

`rmcp` **can** be the backend-facing client, and a gateway can bridge a `2026-07-28` front client to a `2025-11-25` backend. In this spike the bridge needed **no message translation layer**: `rmcp` handles each side's lifecycle independently, and the bridge only forwards `tools/list` and `tools/call`.

## What was built

- `crates/mcpgw-proto/src/spike_bridge.rs`
  - `LegacyBackend`: backend fixture with an `add` tool. It overrides `supported_protocol_versions()` to `ProtocolVersion::known_up_to(V_2025_11_25)`, so it cannot speak `2026-07-28`.
  - `BackendClient`: the gateway's client handler toward the backend.
  - `Bridge`: a `ServerHandler` that forwards `list_tools` / `call_tool` to a backend `Peer<RoleClient>`.
- `crates/mcpgw-proto/tests/spike_bridge.rs`: 6 in-memory (duplex) tests.

## Findings

| Question | Result |
|----------|--------|
| Client negotiates `2025-11-25` with the legacy backend | Yes. The default `Initialize` lifecycle gives `peer_info().protocol_version == 2025-11-25` |
| Front client on `Discover` (`2026-07-28`) through the bridge to a legacy backend | Works for `tools/list` and `tools/call` |
| Strict `Discover` for `2026-07-28` against the legacy backend | Fails with an error, as expected |
| `Auto` with `preferred_versions = [2026-07-28]` only | **Hard error:** "no compatible protocol version". The backend answers `server/discover` with its supported list, so `Auto` does not fall back to `initialize` |
| `Auto` with `preferred_versions = [2026-07-28, 2025-11-25]` | Connects. The gateway must list the legacy version as acceptable |
| Unknown tool through the bridge | The backend's `invalid_params: tool not found` reaches the front client intact |

## Design implications for the gateway

- **Backend lifecycle:** use `Auto` with both versions in `preferred_versions` (newest first). Using `2026-07-28` alone would break legacy backends.
- **Forwarding `_meta`:** the bridge builds fresh `CallToolRequestParams` with only `name` and `arguments`. Per-request `_meta` from the front session (for example `2026-07-28` protocol metadata) must not leak to the backend.
- **Errors:** `ServiceError::McpError` maps back to an `ErrorData` unchanged. Transport-level failures become `internal_error`.
- **Pagination:** the bridge uses `list_all_tools()` and returns a single page. A production gateway needs a decision on pagination and caching.
- **Lifetime:** the bridge must own the backend `RunningService`, or the backend session ends.
- **API note:** `ListToolsResult` is `#[non_exhaustive]`, so it is built with `default()` and field assignment (clippy needs an `allow`).

## Not covered

- Real stdio or child-process backends (tests use in-memory duplex).
- Streamable HTTP on either side.
- Backend disconnect mid-call, and notifications (progress, `list_changed`).
- Fields that differ between versions beyond `tools/list` and `tools/call` (resources, prompts, sampling, the `input_required` MRTR flow).
