# M4-06 — Adapter auth mapping

**Story:** US1 · **Depends on:** M4-05, M1-11 (vault), M1-12 (redaction), M2-05 (OAuth)
**Done when:** each security scheme type is tested with a mock API.
**Status:** ✅ Done: apiKey (header, query, cookie), HTTP basic, HTTP bearer, OAuth 2 and OpenID Connect are each tested against a mock API, and end to end through the vault

## Using it

Bind a vault secret to each of the spec's security schemes, by scheme name:

```sh
mcpgw secret set petstore_key
mcpgw add pets --openapi ./petstore.yaml --auth api_key=petstore_key
mcpgw add jira --openapi ./jira.json --auth basic=jira_token --auth-username basic=bob@example.com
mcpgw add gh --openapi https://…/api.github.com.json --auth oauth=oauth/github@{account}
```

```toml
[[server]]
name = "pets"
transport = "openapi"
spec = "/opt/specs/petstore.yaml"
[server.auth.api_key]
secret = "petstore_key"
[server.auth.petstore_auth]
secret = "oauth/petstore@{account}"     # an OAuth login (M2-05), refreshed when it expires
[server.auth.basic]
secret = "jira_token"
username = "bob@example.com"             # basic: the secret is the password
```

## How a scheme becomes a credential

| Scheme in the spec | What is sent |
|---|---|
| `apiKey`, `in: header` | header `<name>: <secret>` |
| `apiKey`, `in: query` | query parameter `<name>=<secret>` |
| `apiKey`, `in: cookie` | cookie `<name>=<secret>` |
| `http`, `scheme: basic` | `Authorization: Basic base64(user:secret)`. Without a `username`, the secret holds `user:password` |
| `http`, `scheme: bearer` | `Authorization: Bearer <secret>` |
| `oauth2`, `openIdConnect` | `Authorization: Bearer <access token>` |

- **Per operation.** Each operation's `security` (else the spec's
  top-level `security`) lists alternatives. Each alternative is OR'ed, and
  the schemes inside one are AND'ed. The first alternative whose schemes are
  all bound is used. An operation with no security, or with an empty
  alternative (`{}`), gets no credentials. A bound key is never sent to an
  operation that doesn't ask for it.
- **Missing credentials.** When nothing is bound, the request still goes out
  (the API may be open). If it answers 401 or 403, the tool error adds
  "this operation needs credentials for X + Y; bind them in the server's
  `auth`".
- **Typos are caught.** A binding for a scheme the spec doesn't define stops
  the start with "has no security scheme "apikey" (it has: api_key,
  petstore_auth)". `mcpgw add` shows this as a warning.
- **Secrets stay secret:**
  - Bindings hold vault names, never values: the store and config contain
    only `petstore_key`.
  - Values are resolved at each start. `oauth/…` names resolve to a current
    access token.
  - Every value goes into the start's redactor, so tool results, logs and
    audit show `[REDACTED:petstore_key]`.
  - `{account}` in a binding makes the server account-scoped (M2-03).
- `headers` with `${secret:…}` still work for APIs whose spec has no
  security schemes.

## Code

- `mcpgw-openapi`:
  - `Credential::{Secret, Basic}` and `Credentials` (scheme name →
    credential).
  - `Api::call_with(http, tool, args, extras, creds)`.
  - `auth_for` picks the alternative and builds the headers, query and
    cookies.
- `mcpgw-core`: `Transport::OpenApi { auth: BTreeMap<String, AuthBinding{secret, username}> }`.
  The bindings count in `secret_refs`, and `for_account` fills them in.
- `mcpgw-runtime`:
  - `Launch::resolve` swaps the binding names for their values.
  - `OpenApiTemplate::connect` checks the scheme names and builds the
    `Credentials`.
- CLI: `--auth SCHEME=SECRET` and `--auth-username SCHEME=USER`, which go with
  `--openapi`.

## Tests

- `crates/mcpgw-openapi/tests/auth.rs` runs a mock axum API with one
  operation per scheme type, plus OR, AND and open operations. It checks:
  - the exact header, query or cookie for each operation;
  - that nothing leaks to other operations;
  - the missing-credentials hint, including AND order, in the spec's order (`hdr + bearer`).
- `crates/mcpgw/tests/openapi.rs::security_schemes_from_the_vault`:
  - vault secrets go to the fake Petstore as `api_key` (getInventory) and
    `Bearer` (addPet, oauth2), and nothing goes to logoutUser;
  - secrets are not in the store;
  - `--auth-username` without `--auth` is refused;
  - an unknown scheme gives the warning.
- `mcpgw-core` `openapi_auth_bindings`: TOML form, `deny_unknown_fields`,
  `{account}` filling, secret refs.
