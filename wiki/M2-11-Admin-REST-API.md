# M2-11 — Admin REST API v1 with OpenAPI

**Story:** US3 · **Estimate:** 4 days · **Depends on:** M2-08
**Done when:** OpenAPI spec published; every endpoint has an integration test.
**Status:** ✅ Done

The spec is **[docs/openapi.json](../docs/openapi.json)** (OpenAPI 3.1), and
the daemon serves it at `GET /api/v1/openapi.json`. All routes sit behind the
same edge as `/mcp`: loopback `Host`, `Origin` check, and `Authorization:
Bearer $(mcpgw token)`. Errors are `{"error": "…"}` with 400 (bad input),
404 (unknown), or 409 (conflict, or read-only because it comes from the
config file).

| Method & path | Does |
|---------------|------|
| `GET /api/v1/runtime` | version, pid, uptime, servers total/running, pending approvals, endpoints |
| `GET /api/v1/servers` · `POST` | list with live state, pin and tool count · add (stored; starts on first use) |
| `GET /api/v1/servers/{name}` · `DELETE` | one server with its spec · remove (store, manifest, runtime) |
| `POST …/{name}/enable` · `/disable` · `/restart` · `/pin` | lifecycle; enable/disable is persisted |
| `GET …/{name}/logs?lines=` · `/tools` | redacted stderr · pinned tools and pin |
| `GET /api/v1/tools?profile=` | public tools (as a profile sees them: servers, policy) with their server |
| `GET /api/v1/profiles` · `GET`/`PUT`/`DELETE /api/v1/profiles/{name}` | profiles with source and endpoint URL; `PUT` creates or replaces |
| `GET /api/v1/secrets` · `PUT`/`DELETE /api/v1/secrets/{name}` | metadata only; values are **write-only** |
| `GET /api/v1/approvals` · `POST …/{id}/approve` · `/deny` | M2-09 approvals |

Config-file servers and profiles are read-only through the API (409 with
"change it there").

## Profiles take effect at once

`/mcp/p/{name}` is no longer a fixed set of routes. `mcpgw_api::profiles`
looks up the profile per request and builds its MCP service on first use, so
a profile created with `PUT /api/v1/profiles/{name}` is live immediately,
and a deleted one answers 404. Changing a profile drops its cached service,
which ends its sessions, and clients reconnect.

The CLI keeps using the internal `/admin` routes; v1 is the stable, documented
surface. The web UI (M2-12) builds on it.

## Tests

- `crates/mcpgw/tests/api.rs::every_endpoint_works_and_is_documented`
  drives a real daemon through all **23 operations on 18 paths**: success
  paths, 400/404/409 cases, config read-only, a server added through the API
  working over MCP (and its tools captured), a profile created through the
  API usable at `/mcp/p/web` right away and gone after delete, secret values
  never returned, approvals approved and denied, removal persisted, 401
  without the token. The test records every `(method, path template)` it
  called and **asserts that set equals the paths in the OpenAPI document**.
  An undocumented route or an untested operation fails it.
- `crates/mcpgw-api/tests/openapi.rs`: `docs/openapi.json` must match the code
  (`cargo xtask schema` regenerates it with the JSON Schemas), and every
  operation has a summary and responses.
