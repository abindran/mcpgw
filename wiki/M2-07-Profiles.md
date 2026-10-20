# M2-07 — Profiles

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M1-10
**Done when:** Cursor on the frontend profile sees only its three servers.
**Status:** ✅ Done

## Config

```toml
[[profile]]
name = "frontend"                 # → http://127.0.0.1:7777/mcp/p/frontend
servers = ["fs", "github", "figma"]   # empty = every server
mode = "direct"                   # meta (default), direct, hybrid
pinned_tools = []                 # hybrid: listed next to the meta-tools
accounts = { github = "work" }    # over gateway.accounts (M2-03)
# policy = { … }                  # M2-08
```

Profiles come from the config file and the store (the file wins on a name
clash). Names are validated (they are URL segments), duplicates are
rejected, and a profile naming an unknown server gets a warning at daemon
start.

## Endpoints and clients

- HTTP: the daemon mounts one MCP endpoint per profile at
  **`/mcp/p/<name>`**, behind the same Host, Origin and token checks as
  `/mcp`. An unknown profile gets **404**, never the default endpoint.
- stdio: `mcpgw connect --profile frontend` (the shim relays to
  `/mcp/p/frontend`) and `mcpgw serve --stdio --profile frontend`.
- `mcpgw client install cursor --profile frontend` writes `… connect
  --profile frontend`.
- `mcpgw profiles` lists each profile with its URL, mode and servers.

## Scoping (`mcpgw-router::Scope`)

Every listing, `search_tools` / `describe_tool`, name resolution and call
goes through a `Scope { servers, accounts }`. A server outside the profile is
**invisible**: it isn't listed, search doesn't find it, and calling its tool
by name, directly or through `call_tool`, is "tool not found". Login links
(M2-06) and account instances (M2-03) work per profile.

## Tests (`crates/mcpgw/tests/profiles.rs`)

Five servers; `frontend` = fs, gh, figma (direct); `backend` = db (hybrid,
pinned `db__add`).

- The default endpoint lists all five servers.
- **Cursor-style stdio `mcpgw connect --profile frontend` lists exactly fs,
  gh and figma**; calls `gh__add`; `db__add` is "tool not found".
- `backend` lists `search_tools`, `describe_tool`, `call_tool`, `db__add`;
  searching "add" finds only `db__add`; `call_tool` on `fs__add` is an error.
- `/mcp/p/nope` refuses the connection (404).
- `mcpgw profiles` output; `client install --profile`; invalid profile
  names are rejected by the config loader.
