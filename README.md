# mcpgw

A single-binary, local **MCP gateway**. `mcpgw` is simultaneously:

- an **MCP server** to your LLM clients, and
- an **MCP client** to your backend tool servers,

proxying and bridging between them (including across protocol versions).

> Status: **early development**. M0 (foundations) and M1 (core runtime) are done: stdio backends, tool aggregation, a stdio front end, and a CLI.

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
cargo build --release            # binary at target/release/mcpgw
cargo test --workspace           # tests need python3 for the fixture backend
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

Tools appear as `<server>__<tool>` (for example `fs__read_file`). Servers can also be declared in `~/.config/mcpgw/config.toml` (or `$MCPGW_CONFIG`, or `--config`); see [M1-01](wiki/M1-01-Config-Loading.md). Config-file entries take precedence over stored ones with the same name. Logs go to stderr and are controlled by `MCPGW_LOG` (default `warn`).

Backends speaking `2025-11-25` or `2026-07-28` are both supported. Crashed or unresponsive backends are restarted with exponential backoff.

Requires Rust `1.85+` (edition 2021).

## CI & local checks

GitHub Actions (`.github/workflows/ci.yml`) runs fmt, clippy (`-D warnings`), build, test, the `rmcp` dependency-rule check, and a canary-secret scan. Run the rule checks locally:

```sh
bash scripts/check-deps.sh
bash scripts/canary-scan.sh
```

## Roadmap

| Milestone | Theme |
|-----------|-------|
| M0 ✅ | Foundations & risk spikes (workspace, CI, config, proxy spikes) |
| M1 ✅ | Core runtime, store, process lifecycle |
| M2 | OAuth, remote backends, profiles, policy, approvals, admin API/UI |
| M3 | Discovery, search (BM25 + embeddings), performance |
| M4 | Extensions, polish, v1.0 docs |

See the [project wiki](../../wiki) for detailed work-item tracking.
