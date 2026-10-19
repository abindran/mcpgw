# M1-01 — Streamable HTTP front door

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M0-05
**Done when:** a bad Origin gets 403; a missing token gets 401; Claude Code connects with the token.
**Status:** ✅ Done

## What was built

`mcpgw serve` now runs the **daemon**: MCP over Streamable HTTP at
`http://127.0.0.1:7777/mcp` (`gateway.listen`). `mcpgw serve --stdio` keeps
the single-client stdio gateway.

| Layer | Crate | Role |
|-------|-------|------|
| `front::http_service` | `mcpgw-proto` | rmcp `StreamableHttpService` as an axum router: 2026-07-28 requests stateless, legacy clients get sessions |
| `http_router` | `mcpgw-router` | mounts the gateway provider (aggregated tools, request spans) on it |
| `edge` | `mcpgw-api` | the HTTP edge: Host and Origin checks on everything, bearer token on `/mcp`, `/healthz` |

### Checks, in order

1. **Host** must be `localhost`, `127.0.0.1` or `[::1]` (any port), else
   **403**. This blocks DNS rebinding. rmcp's own loopback check stays on
   behind it.
2. **Origin**, if present, must be the daemon's own origin or listed in
   `gateway.allowed_origins`, else **403**. `Origin: null` is refused.
3. `/mcp` needs `Authorization: Bearer <token>` (the file from
   `mcpgw token`), else **401** with `WWW-Authenticate: Bearer realm="mcpgw"`.
   `/healthz` needs no token (the M1-02 shim uses it to find the daemon).

### Config

```toml
[gateway]
listen = "127.0.0.1:7777"                    # loopback only; validated
allowed_origins = ["http://localhost:5173"]   # optional
```

## Connecting Claude Code

```sh
claude mcp add --transport http mcpgw http://127.0.0.1:7777/mcp \
  --header "Authorization: Bearer $(mcpgw token)"
```

## Tests

- `mcpgw-api/tests/edge.rs`: 401 without or with a wrong token; 403 for a
  foreign or `null` Origin even with the token; own and configured origins
  allowed; 403 for a non-loopback Host; `/healthz` without a token.
- `mcpgw/tests/http.rs` runs the real binary with `listen = 127.0.0.1:0` and
  drives it with `mcpgw_testkit::McpClient`, which is rmcp's Streamable HTTP
  client, the same kind of client Claude Code uses:
  - with the token: negotiates 2026-07-28, lists `fx__*`, calls `fx__add`
  - as a 2025-11-25 client: gets a session, calls `fx__echo`
  - without or with a wrong token: the connection is refused
- `mcpgw-config`: `listen` must be loopback; `allowed_origins` must be
  origins.
