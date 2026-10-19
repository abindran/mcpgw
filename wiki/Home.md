# mcpgw Wiki

Local single-binary **MCP gateway**: an MCP server to LLM clients (Claude Code,
Cursor, VS Code) and an MCP client to backend tool servers.

Work follows the **mcpgw Implementation Plan** (19 Oct 2026). Every item has a
page here named `<ID>-<Title>.md` and one commit `<ID>: <summary>` on `main`.

## Decisions

| Decision | Outcome |
|----------|---------|
| Local client auth | Static loopback bearer token (no local OAuth server) |
| Agents adding servers themselves | Off by default |
| Embeddings search | **Dropped** — BM25 only (plan item M4-08 is not built) |
| Licence | Dual `MIT OR Apache-2.0` |
| MCP Apps (UI resources) | Open |

## Work-item tracker

Legend: ✅ done · 🟡 in progress · ⏳ to do · ⛔ dropped

### M0 — Foundations

| ID | Item | Status |
|----|------|--------|
| [M0-01](M0-01-Workspace-and-Crate-Layout.md) | Workspace with all crates, MSRV 1.88, edition 2024 | ✅ |
| [M0-02](M0-02-CI-Matrix.md) | CI matrix: macOS arm64/x86, Linux glibc/musl, Windows | ✅ |
| M0-03 | Supply-chain gates: cargo deny, cargo audit, licences, SBOM | ⏳ |
| M0-04 | Protocol spike: forward one `tools/call` | ✅ ([history](history/M0-04-rmcp-Server-Spike.md)) |
| M0-05 | Version negotiation spike, both directions | ✅ ([history](history/M0-05-rmcp-Client-and-Bridging-Spike.md)); ADR in M0-09 |
| M0-06 | Core domain types + JSON Schema | ⏳ |
| M0-07 | Config loader: env overrides, line-numbered errors | ⏳ |
| M0-08 | Telemetry baseline: JSON/pretty, request IDs | ⏳ |
| M0-09 | ADRs: protocol, storage, vault crypto, process model | ⏳ |
| [M0-10](M0-10-Test-Harness.md) | Test harness: fake MCP servers (stdio + HTTP) | ✅ |

### M1 — Local MVP

| ID | Item | Status |
|----|------|--------|
| M1-01 | Streamable HTTP front door on `127.0.0.1:7777/mcp` | ⏳ |
| M1-02 | stdio shim `mcpgw connect` | ⏳ |
| M1-03 | SQLite store: servers, manifests, profiles, secrets metadata | ⏳ |
| M1-04 | stdio launcher: process groups / Job Objects, stderr capture | ⏳ |
| M1-05 | Lifecycle supervisor state machine | ⏳ |
| M1-06 | Idle reaper and `max_running` cap | ⏳ |
| M1-07 | Manifest capture with SHA-256 pin | ⏳ |
| M1-08 | Drift check and quarantine | ⏳ |
| M1-09 | Aggregating router, 64-char names with hash suffix | ⏳ |
| M1-10 | Meta-tools mode | ⏳ |
| M1-11 | Vault v1 | ⏳ |
| M1-12 | Secret binding, env injection, redaction | ⏳ |
| M1-13 | CLI: up, down, status, add, enable, disable, secret, logs | ⏳ |
| M1-14 | `client install` and `import` | ⏳ |

### M2 — Auth and control

| ID | Item | Status |
|----|------|--------|
| M2-01 | OAuth broker (auth code + PKCE) | ⏳ |
| M2-02 | Token refresher | ⏳ |
| M2-03 | Multi-account bindings | ⏳ |
| M2-04 | Remote HTTP backends, legacy SSE bridge | ⏳ |
| M2-05 | Upstream OAuth for remote MCP servers | ⏳ |
| M2-06 | URL-mode elicitation for auth | ⏳ |
| M2-07 | Profiles | ⏳ |
| M2-08 | Tool policy | ⏳ |
| M2-09 | Approvals | ⏳ |
| M2-10 | Direct-mode change propagation | ⏳ |
| M2-11 | Admin REST API v1 with OpenAPI | ⏳ |
| M2-12 | Minimal web UI | ⏳ |
| M2-13 | Hash-chained audit log | ⏳ |
| M2-14 | MCP Registry integration | ⏳ |

### M3 — Isolation and performance

| ID | Item | Status |
|----|------|--------|
| M3-01 | Container launcher (Docker/Podman) | ⏳ |
| M3-02 | Sandbox profiles | ⏳ |
| M3-03 | Egress allowlist | ⏳ |
| M3-04 | File and tmpfs secret injection | ⏳ |
| M3-05 | Resource limits for stdio servers | ⏳ |
| M3-06 | Warm pools | ⏳ |
| M3-07 | Predictive prewarm | ⏳ |
| M3-08 | Package pre-install cache | ⏳ |
| M3-09 | BM25 tool search | ⏳ |
| M3-10 | Tool description scanner | ⏳ |
| M3-11 | OpenTelemetry OTLP export | ⏳ |
| M3-12 | `mcpgw doctor` | ⏳ |

### M4 — Extensions and v1.0

| ID | Item | Status |
|----|------|--------|
| M4-01 | Wasm launcher | ⏳ |
| M4-02 | Wasm capability grants | ⏳ |
| M4-03 | Wasm limits | ⏳ |
| M4-04 | Wasm tool SDK and template | ⏳ |
| M4-05 | OpenAPI-to-MCP adapter | ⏳ |
| M4-06 | Adapter auth mapping | ⏳ |
| M4-07 | Cedar policies | ⏳ |
| M4-08 | Embeddings search | ⛔ dropped |
| M4-09 | Tasks extension pass-through | ⏳ |
| M4-10 | `mcpgw dev` | ⏳ |
| M4-11 | v1.0 docs | ⏳ |

## Earlier numbering

Work before the plan used its own IDs. The pages are kept in
[history/](history/); this is how they map to plan items:

| Old ID | Old item | Plan item |
|--------|----------|-----------|
| M0-01 | Workspace skeleton | M0-01 |
| M0-02 | CI + rmcp dependency rule | M0-02 (partial) |
| M0-03 | Core error + config types | M0-06 / M0-07 (partial) |
| M0-04 | rmcp server spike | M0-04 |
| M0-05 | rmcp client + bridging spike | M0-05 |
| M1-01 | Config loading | M0-07 (partial) |
| M1-02 | SQLite server registry | M1-03 (partial) |
| M1-03 | Backend process lifecycle | M1-04 / M1-05 (partial) |
| M1-04 | Tool aggregation | M1-09 (partial) |
| M1-05 | Gateway stdio server | stdio front used by M1-02 |
| M1-06 | Loopback token | M1-01 (token part) |
| M1-07 | CLI | M1-13 (partial) |
