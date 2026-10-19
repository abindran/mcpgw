# M0-01 — Cargo Workspace & Crate Skeleton

**Milestone:** M0 (Foundations & Risk Spikes)
**Status:** ✅ Done
**Dependencies:** none

## Goal

Convert the initial single-crate stub into a multi-crate Cargo workspace,
establishing the crate boundaries — most importantly the **protocol
isolation seam** (`mcpgw-proto`) that every later milestone relies on.

## What was built

A Cargo workspace (`resolver = "2"`) with six crates:

| Crate | Role |
|-------|------|
| `mcpgw-proto` | MCP protocol seam. **Only crate permitted to depend on `rmcp`.** Holds protocol versions and (later) framing + version bridging. |
| `mcpgw-core` | Shared `Error`/`Result` and configuration. |
| `mcpgw-store` | `Store` persistence trait + local impl. |
| `mcpgw-server` | MCP server side (faces LLM clients). |
| `mcpgw-client` | MCP client side (faces backend tool servers). |
| `mcpgw` | Gateway binary wiring everything together. |

### Key design points

- **Workspace inheritance** — version, edition, Rust version, license, and
  shared third-party deps are declared once under `[workspace.package]` /
  `[workspace.dependencies]` and inherited by each crate.
- **Dependency direction** — `mcpgw-server` and `mcpgw-client` depend on
  `mcpgw-proto` + `mcpgw-core`; the binary depends on all of them. Nothing
  outside `mcpgw-proto` references `rmcp` (enforced by M0-02).
- **Protocol versions** live in `mcpgw_proto::version`
  (`CLIENT_FACING = 2026-07-28`, `BACKEND_FACING = 2025-11-25`).

## Definition of Done — verification

| Criterion | Result |
|-----------|--------|
| Whole workspace compiles | ✅ `cargo build --workspace` finished clean |
| Each crate exists with a stub lib/bin | ✅ 6 crates created |
| Binary runs | ✅ prints version + both protocol versions |
| Tests pass | ✅ `cargo test --workspace` — all green (2 unit tests) |
| No LSP diagnostics | ✅ clean |

### Binary output

```
mcpgw 0.1.0 — client-facing proto 2026-07-28, backend-facing proto 2025-11-25
```

## Toolchain

- Rust stable `1.94` (edition 2021, MSRV pinned to `1.85`).
- `rmcp` is intentionally **not** added yet — it is pinned during the
  M0-04 / M0-05 spikes so the version is chosen against real proxy code.

## Next

- **M0-02** — CI pipeline + dependency-rule enforcement (fail CI if any
  crate other than `mcpgw-proto` imports `rmcp`).
