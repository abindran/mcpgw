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
mcpgw serve                      # run the daemon: http://127.0.0.1:7777/mcp
mcpgw token [--rotate]           # the loopback bearer token clients must send
mcpgw remove-server gh
```

Point an HTTP-capable MCP client at the daemon with the token, for example
Claude Code:

```sh
claude mcp add --transport http mcpgw http://127.0.0.1:7777/mcp \
  --header "Authorization: Bearer $(mcpgw token)"
```

Clients that can only launch a command (Cursor, VS Code, Claude Desktop) use
the stdio shim, which finds the daemon, starts it if it isn't running, and
relays messages:

```json
{ "mcpServers": { "mcpgw": { "command": "mcpgw", "args": ["connect"] } } }
```

`mcpgw serve --stdio` runs a separate single-client gateway on stdin/stdout
instead of using the daemon.

The daemon only listens on loopback, refuses requests whose `Host` is not a
loopback name or whose `Origin` is a foreign web page (403), and requires the
token on `/mcp` (401).

By default (`gateway.mode = "meta"`) clients see three tools, `search_tools`,
`describe_tool` and `call_tool`, and find backend tools by search. That keeps
the client's context small and picks up servers enabled at runtime without
a restart. Set `mode = "direct"` to list every tool, or `"hybrid"` for the
meta-tools plus `pinned_tools`.

Tools appear as `<server>__<tool>` (for example `fs__read_file`). Servers can also be declared in `~/.config/mcpgw/config.toml` (or `$MCPGW_CONFIG`, or `--config`); see [examples/config.toml](examples/config.toml). Config-file entries take precedence over stored ones with the same name. Logs go to stderr; `MCPGW_LOG` sets the level (default `warn`) and `--log-format pretty|compact|json` (or `MCPGW_LOG_FORMAT`) the format.

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
