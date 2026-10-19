# mcpgw Wiki

Local single-binary **MCP gateway** — an MCP server to LLM clients and an MCP
client to backend tool servers, bridging between them.

## Locked decisions

- **Auth:** static loopback token.
- **Agent self-adding servers:** off by default.
- **Embeddings search:** enabled by default.

## Work-item tracker

### Milestone M0 — Foundations & Risk Spikes

| ID | Item | Status |
|----|------|--------|
| [M0-01](M0-01-Workspace-and-Crate-Skeleton) | Cargo workspace + crate skeleton | ✅ Done |
| [M0-02](M0-02-CI-and-Dependency-Rule) | CI pipeline + rmcp dependency-rule enforcement | ✅ Implemented (remote proof pending) |
| M0-03 | Core error + config types (incl. locked decisions) | ✅ Done |
| [M0-04](M0-04-rmcp-Server-Spike) | Spike: rmcp as MCP **server** to a client | ✅ Done |
| [M0-05](M0-05-rmcp-Client-and-Bridging-Spike) | Spike: rmcp as MCP **client** + version bridging | ✅ Done |

### Milestone M1 — Core runtime, store, process lifecycle

| ID | Item | Status |
|----|------|--------|
| [M1-01](M1-01-Config-Loading) | Config file loading, defaults, validation (`mcpgw-core`) | ✅ Done |
| [M1-02](M1-02-SQLite-Server-Registry) | SQLite-backed server registry (`mcpgw-store`) | ✅ Done |
| [M1-03](M1-03-Backend-Process-Lifecycle) | Backend process lifecycle: spawn, health, restart, shutdown | ✅ Done |
| [M1-04](M1-04-Tool-Aggregation) | Tool aggregation with server-name prefixes | ✅ Done |
| [M1-05](M1-05-Gateway-Stdio-Server) | Gateway stdio server forwarding `tools/list` / `tools/call` | ✅ Done |
| [M1-06](M1-06-Loopback-Token-Auth) | Static loopback-token auth | ✅ Done |
| M1-07 | `mcpgw` CLI: `serve`, `add-server`, `list-servers` | ⏳ Todo |
| M1-08 | End-to-end test through the gateway binary | ⏳ Todo |

### Later milestones

- **M2** — OAuth, remote backends, profiles, policy, approvals, admin API/UI
- **M3** — Discovery, search (BM25 + embeddings), performance
- **M4** — Extensions, polish, v1.0 docs
