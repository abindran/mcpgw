# M2-02 — Token refresh

**Story:** US2 · **Estimate:** 2 days · **Depends on:** M2-01
**Done when:** an expiring token is refreshed before use; a revoked grant shows "needs login".
**Status:** ✅ Done

## Behaviour

- **`Broker::refresh(provider, account)`** posts `grant_type=refresh_token`
  and stores the new `TokenSet`. If the provider doesn't rotate refresh
  tokens (it omits one), the old refresh token is kept. One async lock per
  connection, so concurrent refreshes don't race; a waiter that finds the
  token already refreshed returns it.
- **`invalid_grant`** (revoked, or the refresh token itself expired) marks the
  connection `needs_reauth` in the vault. It is never retried; `mcpgw auth
  ls` shows `needs login`. Other failures (network, 5xx) are left for the
  next attempt.
- **Background refresher** in the daemon: every `gateway.oauth_refresh_interval`
  (default 60 s), connections with a refresh token that expire within 5
  minutes (`REFRESH_LEAD`) are refreshed.
- **Before injection:** when a server starts and binds `oauth/<p>@<a>`, a
  token due within 60 s is refreshed first. Secret resolution runs on a
  blocking thread, so it waits on the refresh there. If that refresh fails
  but the token hasn't expired yet, the current token is used with a
  warning. A connection that needs a login fails the start with "the
  oauth/github@work login has expired; run `mcpgw auth connect github
  --account work`".
- Servers already running keep their token until they restart. Refreshing
  the token of a running server (restart, or `${secret:}` re-injection for
  HTTP backends) comes with M2-04 and M2-05.

## Tests

- `tokens_refresh_and_revocation_needs_a_new_login` (`crates/mcpgw/tests/oauth.rs`,
  with a fake provider whose tokens expire after 1 s and a 200 ms check
  interval):
  background refreshes happen; a server bound with `--secret
  GITHUB_TOKEN=oauth/fake@default` receives a current token (shown to the
  client only as `[REDACTED:oauth/fake@default]`); after the provider revokes
  the grant, `auth ls` shows `needs login`, refresh attempts **stop**, and
  after `mcpgw restart fx` the next call fails with the `mcpgw auth connect
  fake --account default` hint.
- `long_lived_tokens_are_not_refreshed_early`: a 1-hour token is not
  refreshed.
- Unit: `due` only near expiry, only with a refresh token, never with no
  expiry or when a login is needed.
