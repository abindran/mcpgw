# M2-10 — Direct-mode change propagation

**Story:** US3 · **Estimate:** 2 days · **Depends on:** M2-07
**Done when:** a client that supports notifications sees new tools without reconnecting.
**Status:** ✅ Done

## What happens when servers change

`Catalog` now carries a change counter (`tokio::sync::watch`). It goes up
when a manifest is recorded with a new pin or removed, and when the manager
adds, removes, enables or disables a server: every way `mcpgw add`,
`enable`, `disable`, `pin`, `client import` or the admin API can change what
a listing shows.

In **direct and hybrid** mode the front end:

- advertises `capabilities.tools.listChanged = true`;
- on **2025-11-25** sessions, sends `notifications/tools/list_changed` on the
  session after each change (one task per session that ends when the
  session does);
- on **2026-07-28**, accepts `subscriptions/listen` for `toolsListChanged`
  and sends the notification on that subscription;
- sets `ttlMs: 5000` on `tools/list`, so stateless clients that only cache
  also refresh within seconds.

In **meta** mode the listing is always the three meta-tools, so there is
nothing to announce: no `listChanged`, and `ttlMs` is one hour. New servers
show up in `search_tools` at once (M1-10).

`ToolProvider` gained `changes()` and `list_ttl_ms()` (both optional) to
carry this across the protocol seam.

## Tests (`crates/mcpgw-router/tests/changes.rs`)

- `clients_see_new_tools_without_reconnecting`: a 2025-11-25 client and a
  2026-07-28 client (with `subscriptions/listen` open) on one direct-mode
  endpoint. `listChanged` is advertised and `ttlMs` is 5000. A server added at
  runtime gives the legacy client a `list_changed` notification and the 2026
  subscription a `tools/list_changed`, and both then list `extra__add` on the
  **same connection**. Disabling the server notifies both again, and the tool
  is gone.
- `meta_mode_needs_no_notifications`: no `listChanged`, `ttlMs` = 3 600 000.

testkit: `McpClient::listen_tools()` (2026 subscriptions),
`list_changed_count()` (legacy notifications), `list_raw()`,
`server_capabilities()`.
