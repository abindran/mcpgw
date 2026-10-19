# mcpgw

A single-binary, local **MCP gateway**. `mcpgw` is simultaneously:

- an **MCP server** to your LLM clients, and
- an **MCP client** to your backend tool servers,

proxying and bridging between them (including across protocol versions).

> Status: **early development**, following the implementation plan (see the [wiki](wiki/Home.md)).

## Workspace layout

This is a Cargo workspace (edition 2024, MSRV 1.98). Crates are split so
that protocol churn stays contained behind a single seam.

| Crate | Role |
|-------|------|
| `mcpgw` | CLI entry and local daemon. |
| `mcpgw-proto` | MCP adapter. **The only crate allowed to depend on `rmcp`.** |
| `mcpgw-core` | Domain types: servers, tools, profiles, manifests, policies, errors. |
| `mcpgw-config` | TOML load, validation, hot reload, import from client configs. |
| `mcpgw-store` | Local persistence in SQLite behind a trait. |
| `mcpgw-vault` | Envelope encryption, OS keychain and passphrase keys, redaction. |
| `mcpgw-router` | Aggregation, namespacing, meta-tools, policy checks, result pipeline. |
| `mcpgw-runtime` | Supervisor, lifecycle state machine, launchers. |
| `mcpgw-auth` | Loopback client token, OAuth broker, upstream OAuth. |
| `mcpgw-api` | Admin REST API, SSE events, static web UI. |
| `mcpgw-telemetry` | Tracing, metrics, audit log writer. |
| `xtask` | Build, release, conformance and e2e helpers (`cargo xtask ci`). |

### Dependency rule

No crate **except `mcpgw-proto`** may import `rmcp`. This isolates all
protocol handling to one place so protocol version churn has a single blast
radius. This rule is enforced automatically in CI (work item **M0-02**).

## Protocol versions

MCP `2026-07-28` is the primary protocol on both sides. `2025-11-25` is
accepted from older clients and servers.

## Decisions locked in

- **Auth:** static loopback token (localhost daemon).
- **Agent self-adding servers:** **off by default** (supply-chain / tool-poisoning safeguard).
- **Embeddings search:** not built; tool search is BM25 only.

## Build & run

```sh
cargo build --release            # binary at target/release/mcpgw
cargo test --workspace
```

## Quick start

```sh
# Register backends (stored in ~/.local/share/mcpgw/mcpgw.db)
mcpgw add-server fs --command npx -- -y @modelcontextprotocol/server-filesystem /tmp
mcpgw add-server gh --command github-mcp --env GITHUB_TOKEN=...

mcpgw list-servers --check       # start each backend and report status and tool count
mcpgw serve                      # run the gateway on stdio
mcpgw token [--rotate]           # loopback token (used by the HTTP front end from M2)
mcpgw remove-server gh
```

Point your MCP client at the gateway, for example:

```json
{ "mcpServers": { "mcpgw": { "command": "mcpgw", "args": ["serve"] } } }
```

Tools appear as `<server>__<tool>` (for example `fs__read_file`). Servers can also be declared in `~/.config/mcpgw/config.toml` (or `$MCPGW_CONFIG`, or `--config`); see [config loading](wiki/history/M1-01-Config-Loading.md). Config-file entries take precedence over stored ones with the same name. Logs go to stderr and are controlled by `MCPGW_LOG` (default `warn`).

Backends speaking `2025-11-25` or `2026-07-28` are both supported. Crashed or unresponsive backends are restarted with exponential backoff.

Requires Rust `1.98+` (edition 2024).

## CI & local checks

GitHub Actions (`.github/workflows/ci.yml`) runs fmt, clippy (`-D warnings`), build, test, the `rmcp` dependency-rule check, and a canary-secret scan. Run the rule checks locally:

```sh
bash scripts/check-deps.sh
bash scripts/canary-scan.sh
```

## Roadmap

| Milestone | Theme |
|-----------|-------|
| M0 | Foundations: workspace, CI, config, telemetry, protocol spike |
| M1 | Local MVP: one-config gateway with lazy servers and a vault |
| M2 | Auth and control: OAuth, remote backends, profiles, approvals, admin API |
| M3 | Isolation and performance: containers, sandboxing, warm pools, search, OTel |
| M4 | Extensions and v1.0: Wasm tools, OpenAPI adapter, dev loop |

See the [wiki](wiki/Home.md) for the work-item tracker.
