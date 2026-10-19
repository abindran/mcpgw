# M1-07 — `mcpgw` CLI

**Crate:** `mcpgw` (`src/main.rs`, clap) · **Status:** ✅ Done

| Command | What it does |
|---------|--------------|
| `mcpgw serve` | Loads the config, merges config and store servers, creates the token if it's missing, starts all backends, waits up to 30 s for them to settle, then serves MCP on stdio until the client disconnects or Ctrl-C. Backends are shut down on exit. |
| `mcpgw add-server NAME --command CMD [--env K=V]... [--disabled] [-- ARGS...]` | Adds or updates a stdio server in the store. |
| `mcpgw add-server NAME --url URL [--disabled]` | Adds or updates an HTTP server in the store (it is connected from M2 on). |
| `mcpgw remove-server NAME` | Removes a server from the store. |
| `mcpgw list-servers [--check]` | Prints tab-separated `name  transport  source  command/url [disabled]`. With `--check`, it also starts each backend and appends `ok (N tools, <protocol>)`, `error: …`, or `skipped: …`. |
| `mcpgw token [--rotate]` | Prints (and creates, if needed) the loopback token, or rotates it. |

Global flag: `--config PATH` (default `$MCPGW_CONFIG`, then `~/.config/mcpgw/config.toml`).

## Rules
- Servers defined in the config file can't be added or removed through the CLI. You get an error that points at the file.
- Names are validated the same way as in the config (no `__`, and so on).
- **stdout is reserved for MCP** in `serve`. Logs go to stderr via `tracing-subscriber`, with the level set by `MCPGW_LOG` (default `warn`).
- Errors print as `mcpgw: <message>` with exit code 1.

## Tests
`crates/mcpgw/tests/cli.rs` runs 4 tests against the built binary, each with an isolated config, store and token directory: add/list/remove (including protected config names and invalid names), `list-servers --check` with one working and one broken backend, token create and rotate, and `--version` plus a bad config.
