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
| [M0-01](M0-01-Workspace-and-Crate-Layout.md) | Workspace with all crates, MSRV 1.98, edition 2024 | ✅ |
| [M0-02](M0-02-CI-Matrix.md) | CI matrix: macOS arm64/x86, Linux glibc/musl, Windows | ✅ |
| [M0-03](M0-03-Supply-Chain-Gates.md) | Supply-chain gates: cargo deny, cargo audit, licences, SBOM | ✅ |
| [M0-04](M0-04-Protocol-Spike.md) | Protocol spike: forward one `tools/call` | ✅ |
| M0-05 | Version negotiation spike, both directions | ✅ ([history](history/M0-05-rmcp-Client-and-Bridging-Spike.md), [ADR-0001](../docs/adr/0001-protocol.md)) |
| [M0-06](M0-06-Core-Domain-Types.md) | Core domain types + JSON Schema | ✅ |
| [M0-07](M0-07-Config-Loader.md) | Config loader: env overrides, line-numbered errors | ✅ |
| [M0-08](M0-08-Telemetry-Baseline.md) | Telemetry baseline: JSON/pretty, request IDs | ✅ |
| [M0-09](M0-09-ADRs.md) | ADRs: protocol, storage, vault crypto, process model | ✅ |
| [M0-10](M0-10-Test-Harness.md) | Test harness: fake MCP servers (stdio + HTTP) | ✅ |

### M1 — Local MVP

| ID | Item | Status |
|----|------|--------|
| [M1-01](M1-01-HTTP-Front-Door.md) | Streamable HTTP front door on `127.0.0.1:7777/mcp` | ✅ |
| [M1-02](M1-02-Stdio-Shim.md) | stdio shim `mcpgw connect` | ✅ |
| [M1-03](M1-03-SQLite-Store.md) | SQLite store: servers, manifests, profiles, secrets metadata | ✅ |
| [M1-04](M1-04-Stdio-Launcher.md) | stdio launcher: process groups / Job Objects, stderr capture | ✅ |
| [M1-05](M1-05-Lifecycle-Supervisor.md) | Lifecycle supervisor state machine | ✅ |
| [M1-06](M1-06-Idle-Reaper-and-Cap.md) | Idle reaper and `max_running` cap | ✅ |
| [M1-07](M1-07-Manifest-Capture.md) | Manifest capture with SHA-256 pin | ✅ |
| [M1-08](M1-08-Drift-Check.md) | Drift check and quarantine | ✅ |
| [M1-09](M1-09-Aggregating-Router.md) | Aggregating router, 64-char names with hash suffix | ✅ |
| [M1-10](M1-10-Meta-Tools.md) | Meta-tools mode | ✅ |
| [M1-11](M1-11-Vault.md) | Vault v1 | ✅ |
| [M1-12](M1-12-Secret-Injection-and-Redaction.md) | Secret binding, env injection, redaction | ✅ |
| [M1-13](M1-13-CLI.md) | CLI: up, down, status, add, enable, disable, secret, logs | ✅ |
| [M1-14](M1-14-Client-Install-and-Import.md) | `client install` and `import` | ✅ |

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
