# M2-01 — OAuth broker

**Story:** US2 · **Estimate:** 4 days · **Depends on:** M1-11
**Done when:** `mcpgw auth connect github` completes in a browser and stores encrypted tokens.
**Status:** ✅ Done

## Setup

mcpgw ships no client credentials. Register an OAuth app with the provider,
using the redirect URI `http://127.0.0.1:7777/oauth/callback`, then:

```toml
[[oauth]]
name = "github"
preset = "github"                    # github, google or atlassian endpoints
client_id = "Iv1.0123456789abcdef"
client_secret = "github_oauth_secret" # vault secret name, for confidential apps
scopes = ["repo", "read:org"]         # default: the preset's
```

```sh
mcpgw secret set github_oauth_secret
mcpgw auth connect github --account work   # opens the browser
mcpgw auth ls
mcpgw auth rm github --account work
```

Presets fill in the authorize and token URLs, default scopes and extra
parameters (Google: `access_type=offline`, `prompt=consent`; Atlassian:
`audience=api.atlassian.com`). URLs must be https; loopback http is allowed
only for tests. App names must be unique; config validation checks
everything.

## Flow (`mcpgw-auth::oauth::Broker`)

1. `auth connect` (starting the daemon if needed) calls `POST
   /admin/oauth/{provider}/start`. The daemon creates a **PKCE** pair (43-char
   verifier, S256 challenge) and a random 192-bit `state`, keeps the flow for
   10 minutes, and returns the authorization URL (`response_type=code`,
   `client_id`, `redirect_uri`, `state`, `code_challenge`,
   `code_challenge_method=S256`, `scope`, extras).
2. The CLI opens the browser (`open`, `xdg-open`, `start`; or
   `--no-browser`) and polls `GET /admin/oauth/flows/{state}`.
3. The provider redirects to **`GET /oauth/callback?code&state`** on the
   daemon. This route is public (a browser can't send the bearer token) but
   still behind the loopback Host check. An unknown, expired or reused
   `state` is refused, and the code is useless without the verifier, which
   never leaves the daemon.
4. The daemon POSTs `grant_type=authorization_code` with the verifier (and
   the client secret from the vault) and stores the `TokenSet` (access,
   refresh, expiry, scope) **encrypted in the vault** as
   `oauth/<provider>@<account>`. The browser shows "mcpgw is connected to
   github (work)".
5. Provider errors (`error=access_denied`) end the flow with a clear message
   in the page and the CLI.

`TokenSet`'s `Debug` hides the tokens. A server can use a connection like any
secret: `secrets = { GITHUB_TOKEN = "oauth/github@work" }` injects the
current **access token**, not the JSON. A connection that needs a new login
fails the server's start with "run `mcpgw auth connect github --account
work`".

The daemon's vault is now one shared handle (`SharedVault`) for injection and
token writes, re-read when another process changes the file.

## Decisions

- No `oauth2` / `openidconnect` crates: the code flow with PKCE is about a
  hundred lines on `reqwest`, and the token logic stays fully under our
  control for refresh (M2-02) and upstream OAuth (M2-05).
- TLS: reqwest with **rustls + native roots** (OS trust store, so corporate
  CAs work; ring, no cmake). Adding it made `cargo deny` catch four rustls
  0.23.45 advisories, fixed by updating to 0.23.46.

## Tests

- `crates/mcpgw/tests/oauth.rs` with `mcpgw_testkit::oauth::FakeOAuth` (a
  fake authorization server whose token endpoint rejects a wrong PKCE
  verifier, client id or redirect URI):
  - `auth connect fake --account work`: the test plays the browser, follows
    the redirect to the daemon, and sees "connected to fake (work)". The CLI
    exits OK, the challenge method was S256, `auth ls` shows the account as
    refreshable, the vault file contains no token, a second account works,
    and `auth rm` removes one.
  - a refused login gives a 400 page and a CLI error; a forged `state` is
    refused; an unknown provider is named.
- Units: PKCE is S256 of the verifier; `TokenSet` debug output hides tokens;
  presets and https enforcement.

A real GitHub login needs a registered app; with one, the same command
completes in a real browser.
