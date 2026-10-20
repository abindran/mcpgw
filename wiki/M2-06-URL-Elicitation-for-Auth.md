# M2-06 — URL-mode elicitation for auth

**Story:** US2 · **Estimate:** 2 days · **Depends on:** M2-05
**Done when:** a first call to an unconnected app returns a link; after login the retry succeeds.
**Status:** ✅ Done

## Behaviour

When a tool call fails because a login is missing or expired, the gateway
starts that login and answers with the sign-in URL instead of an error:

| Client | Answer |
|--------|--------|
| declares `elicitation.url` (MCP 2025-11-25) | JSON-RPC error **-32042** "URL elicitation required", `data.elicitations = [{mode: "url", elicitationId, url, message}]`; the client shows the link |
| anything else | a tool result with `isError: true`: "Sign in to atl to use this tool. Ask the user to open this link and sign in, then call the tool again:\n<url>" |

Both work in direct mode and through the meta-tools' `call_tool`.

What counts as "needs a login" (`mcpgw_router::login`):

- a remote server answered 401/403 (M2-04/M2-05), so the login is the
  server's own, as the caller's account;
- a bound `oauth/<app>@<account>` is not connected, or its login has expired
  (M2-01/M2-02), so the login is that app and account.

The link is the provider's authorization URL with a fresh PKCE flow, valid
for 10 minutes. For a remote server seen for the first time, getting the link
also runs discovery and registration. When the browser login completes, the
daemon **resets** every server that uses that login. `Reset` is now also
valid from `Backoff`: it clears the retry timer and failure count. So the
client's retry starts the server with the new token at once instead of
waiting out a backoff.

Supporting changes:

- `mcpgw-proto`: `ToolProvider::call_tool` receives a `CallContext`
  (`url_elicitation`, `client_name`), read from the request's client
  capabilities (2026-07-28 per-request metadata, or the legacy session).
- A missing OAuth binding now says "oauth/github@work is not connected; run
  `mcpgw auth connect github --account work`".
- `mcpgw-api::oauth::begin_login` is shared by `auth connect` and the
  router; an `on_login` hook runs after each completed login.
- stdio mode has no callback endpoint, so it keeps the plain error and
  hint.

## Tests (`crates/mcpgw/tests/upstream.rs`)

- `first_call_returns_a_login_link_via_url_elicitation`: a client declaring
  URL elicitation calls an OAuth-protected remote server it never logged in
  to. It gets `-32042` with `mode: url`, the message, and a URL on the
  provider's `/authorize`. Opening it ends on "connected to atl", and the
  retry returns `3`.
- `clients_without_elicitation_get_the_link_as_text`, in **meta** mode
  through `call_tool`: `isError` text with the link on its last line; open,
  retry, `10`.
- The M2-05 test now sees the link on its first call and still registers
  only once.
- Units: login detection and target extraction (`oauth/<p>@<a>` in the
  message, or the server and account); the two answer shapes.
