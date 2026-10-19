# M1-05 — Gateway stdio server

**Crates:** `mcpgw-proto` (`src/front.rs`), `mcpgw-server` · **Status:** ✅ Done

## Shape
```
LLM client ──stdio──▶ mcpgw-proto::front (rmcp server)
                         │  ToolProvider trait (plain JSON)
                         ▼
                 mcpgw-server::GatewayProvider
                         │
                         ▼
                 mcpgw-client::BackendManager ──stdio──▶ backends
```

- `mcpgw_proto::front::ToolProvider`: `list_tools() -> Vec<JsonObject>` and `call_tool(name, args) -> Result<JsonObject, ProtoError>`. This is how the server crate plugs in without touching `rmcp`, which keeps the dependency rule intact.
- `front::serve_io` / `serve_stdio`: an rmcp server that advertises the `tools` capability, `serverInfo { name: "mcpgw", version }`, and instructions explaining the `<server>__<tool>` naming.
- **Both client generations work.** `2026-07-28` clients use `server/discover`; `2025-11-25` clients use `initialize`.
- Error mapping: `ProtoError::Rpc` is passed through with its original code (for example `-32602 tool not found`), and every other error becomes `-32603` with a readable message (`backend unavailable: …`, `timed out …`, `backend connection closed`).
- Malformed tools from a provider are skipped with a warning instead of failing the whole list.
- Front-session `_meta` is **not** forwarded to backends (as decided in M0-05).

## `mcpgw-server`
`GatewayProvider` wraps `Arc<BackendManager>`: `tools/list` returns the aggregated list (M1-04), and `tools/call` goes through `call_public` (M1-04). `serve_io` / `serve_stdio` are the entry points the binary uses (M1-07).

## Tests
- `mcpgw-proto/tests/front.rs` (rmcp clients): a modern client using Discover, plus a legacy client with error mapping and the malformed tool skipped.
- `mcpgw-server/tests/gateway.rs`: raw JSON-RPC lines → gateway → a real fixture child process (`initialize`, `tools/list`, `tools/call`, unknown tool and unknown prefix).
