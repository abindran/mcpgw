# M0-01 — Workspace and crate layout

**Story:** X · **Estimate:** 1 day · **Depends on:** —
**Done when:** `cargo build` produces `mcpgw` on macOS, Linux and Windows.
**Status:** ✅ Done (Windows/Linux builds proven by the M0-02 matrix)

## What changed

- Edition **2024**, MSRV **1.88**, workspace resolver 3.
- The workspace now has every crate from the plan:

| Crate | Responsibility | Code today |
|-------|----------------|------------|
| `mcpgw` (bin) | CLI entry and local daemon | CLI |
| `mcpgw-proto` | MCP adapter; the only crate that imports `rmcp` | backend client, stdio front, spikes |
| `mcpgw-core` | Domain types and errors | `ServerConfig`, `Transport`, `Error` |
| `mcpgw-config` | TOML load and validation | `Config` (moved from core) |
| `mcpgw-store` | SQLite persistence behind a trait | server registry |
| `mcpgw-vault` | Encryption, keys, redaction | stub (M1-11) |
| `mcpgw-router` | Aggregation, namespacing, meta-tools, policy | aggregation + `GatewayProvider` (was `mcpgw-server`) |
| `mcpgw-runtime` | Supervisor and launchers | `BackendManager` (was `mcpgw-client`) |
| `mcpgw-auth` | Loopback token, OAuth | `Token` (moved from core) |
| `mcpgw-api` | Admin REST API and web UI | stub (M2-11) |
| `mcpgw-telemetry` | Tracing, metrics, audit log | stub (M0-08) |
| `xtask` | Build and test helpers | `cargo xtask ci` |

- `ServerConfig::validate()` lives in core, so the store validates servers
  without depending on the config crate. `effective_servers` now takes the
  config file's server list.
- The `[search]` section and its `embeddings` flag are removed: embeddings
  search is dropped from the project.

## Layering

```
core ← config, store, auth, vault, telemetry
core ← proto(rmcp) ← runtime ← router ← mcpgw (bin)
```

## Tests

All existing tests moved with their code (`mcpgw-runtime/tests/lifecycle.rs`,
`mcpgw-router/tests/gateway.rs`). `cargo xtask ci` runs the local gates.
