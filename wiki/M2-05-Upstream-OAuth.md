# M2-05 — Upstream OAuth for remote MCP servers

**Story:** US2 · **Estimate:** 4 days · **Depends on:** M2-01, M2-04
**Done when:** Atlassian remote MCP connects with tokens held only by mcpgw.
**Status:** ✅ Done (verified against fakes; a real Atlassian login needs an account)

## Usage

```sh
mcpgw add atlassian --url https://mcp.atlassian.com/v1/sse
# mcpgw: atlassian requires a login; sign in with `mcpgw auth connect atlassian`
mcpgw auth connect atlassian            # browser login; tokens stay in mcpgw's vault
```

No `[[oauth]]` app and no client id are needed: the server tells mcpgw where
to log in.

## Flow (`mcpgw-auth::discovery`, MCP authorization spec)

1. **Protected resource metadata** (RFC 9728): mcpgw POSTs a ping to the
   server and reads `resource_metadata` from the 401 `WWW-Authenticate`
   header, else tries `/.well-known/oauth-protected-resource{path}` and then
   without the path.
2. **Authorization server metadata** (RFC 8414; OIDC discovery as fallback,
   path-aware) for the first listed authorization server. The `issuer` must
   match, PKCE S256 must be supported if methods are listed, and every
   endpoint must be https (loopback http only for tests).
3. **Client identity:**
   - a **client ID metadata document**, when the server advertises
     `client_id_metadata_document_supported` and `gateway.client_metadata_url`
     is set (an https URL describing mcpgw that you host); otherwise
   - **dynamic client registration** (RFC 7591) as a public client
     (`token_endpoint_auth_method = none`, redirect to the daemon's
     `/oauth/callback`, code + refresh grants). A secret, if issued, goes to
     the vault;
   - neither → a clear error suggesting an `[[oauth]]` app.
4. The result is an ordinary `OAuthProvider` named after the server, with
   `resource` (RFC 8707) sent on the authorization **and** token requests.
   It is saved in the vault as `oauth-client/<server>` and reloaded at daemon
   start, so restarts don't register again. The M2-01 flow, M2-02 refresh and
   M2-03 accounts all apply unchanged.

## Token injection

For an HTTP server with no `Authorization` header configured, every start
looks up `oauth/<server>@<account>` and, if present, sends `Authorization:
Bearer <token>`. The token is refreshed first if due (M2-02) and redacted
from everything sent back. Without a login the server's 401 becomes
"<url> requires authentication…; sign in with `mcpgw auth connect <server>`"
(with `--account` for account instances), both at `add` time and on calls.
Clients only ever hold the loopback token; the remote token never leaves
mcpgw.

## Tests (`crates/mcpgw/tests/upstream.rs`)

New fixtures: `FakeOAuth` serves RFC 8414 metadata and `POST /register`,
records `resource` parameters, and can advertise metadata-document support;
`fake-mcp --http` with `FAKE_AUTH_SERVER` requires `Bearer at-…`, answers 401
with `resource_metadata`, and serves its protected resource metadata.

- **End to end**: `add` prints the login hint; a call before login fails
  with it; `auth connect atl` performs discovery and **one** DCR (client name
  `mcpgw`, redirect to `/oauth/callback`), authorizes as `dcr-client-1`, and
  sends `resource` to both endpoints. The call then returns `4`, tools are
  listed, `auth ls` shows the login, and the vault file has no token or
  client id in plaintext. After `down` / `up` the call works **without** a
  second registration.
- **Metadata document**: with `client_metadata_url` set and support
  advertised, no registration happens and the URL is the client id.
- **No metadata**: a server without OAuth metadata gives a clear error.
- Units: `resource_metadata` parsing (quoted and bare), path-aware
  well-known URLs, https enforcement.
