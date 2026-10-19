# mcpgw

A single-binary, local **MCP gateway**. `mcpgw` is simultaneously:

- an **MCP server** to your LLM clients, and
- an **MCP client** to your backend tool servers,

proxying and bridging between them (including across protocol versions).

> Status: **early development** — currently at milestone **M0 (foundations & risk spikes)**.

## Workspace layout

This is a Cargo workspace. Crates are split so that protocol churn stays
contained behind a single seam.

| Crate | Role |
|-------|------|
| `mcpgw-proto` | MCP protocol seam. **The only crate allowed to depend on `rmcp`.** Owns framing, endpoints, and bidirectional protocol-version bridging. |
| `mcpgw-core` | Shared types, `Error`/`Result`, and configuration. |
| `mcpgw-store` | `Store` persistence trait + local implementation. |
| `mcpgw-server` | MCP **server** side — faces LLM clients. |
| `mcpgw-client` | MCP **client** side — faces backend tool servers. |
| `mcpgw` | The gateway binary that wires everything together. |

### Dependency rule

No crate **except `mcpgw-proto`** may import `rmcp`. This isolates all
protocol handling to one place so protocol version churn has a single blast
radius. This rule is enforced automatically in CI (work item **M0-02**).

## Protocol versions

The gateway bridges two protocol versions:

- Client-facing (to LLM clients): `2026-07-28`
- Backend-facing (to tool servers): `2025-11-25`

## Decisions locked in

- **Auth:** static loopback token (localhost daemon).
- **Agent self-adding servers:** **off by default** (supply-chain / tool-poisoning safeguard).
- **Embeddings search:** **enabled by default**.

## Build & run

```sh
# Build the whole workspace
cargo build --workspace

# Run the gateway binary
cargo run --bin mcpgw

# Run all tests
cargo test --workspace
```

Requires Rust `1.85+` (edition 2021).

## Roadmap

| Milestone | Theme |
|-----------|-------|
| **M0** | Foundations & risk spikes (workspace, CI, config, proxy spikes) |
| M1 | Core runtime, store, process lifecycle |
| M2 | OAuth, remote backends, profiles, policy, approvals, admin API/UI |
| M3 | Discovery, search (BM25 + embeddings), performance |
| M4 | Extensions, polish, v1.0 docs |

See the [project wiki](../../wiki) for detailed work-item tracking.
