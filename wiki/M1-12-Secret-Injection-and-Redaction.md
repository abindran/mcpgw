# M1-12 — Secret binding, injection and redaction

**Story:** US2 · **Estimate:** 2 days · **Depends on:** M1-11, M1-04
**Done when:** canary test: a tool that echoes its environment never returns the secret to the client.
**Status:** ✅ Done

## Binding secrets to a server

```toml
[[server]]
name = "github"
transport = "stdio"
command = "github-mcp-server"
args = ["stdio", "--host=${secret:gh_host}"]   # placeholders in args and env values
secrets = { GITHUB_TOKEN = "github_token" }     # env var = vault secret name
```

- `ServerSpec.secrets` maps environment variables to vault secret names.
  `${secret:<name>}` placeholders work in `args`, `env` values and (from
  M2-04) HTTP headers. `secret_refs()` collects every referenced name; names
  and variables are validated.
- **At each spawn** (never earlier) the actor resolves the referenced
  secrets through a `SecretResolver`, off the actor thread. The daemon's
  `VaultResolver` opens `gateway.vault_path` (default `vault.json` next to the
  store) on first use with `MCPGW_VAULT_PASSPHRASE` or the keychain, and
  re-reads it when the file changes. A missing secret fails the start with
  "secret \"github_token\" is not set; add it with `mcpgw secret set
  github_token`"; a server that needs secrets with no vault configured says so.
- Install-time introspection (`add-server`) resolves secrets the same way.

## Redaction (`mcpgw_vault::Redactor`)

Each connection gets a redactor for the values injected into it. Every
string the server sends back passes through it before reaching a client or
a log:

- tool results (every string in the JSON, including object keys)
- error messages and error `data`
- stderr lines (the ring buffer and the debug log)

Values are matched as is, in standard and URL-safe base64 (with and without
padding), and percent-encoded. The longest match wins, and values under 4
characters are not redacted. A match becomes `[REDACTED:<secret name>]`.

## Tests

- **Runtime canary**, `bound_secret_is_injected_but_never_returned`: the
  fake's `env` tool returns `GITHUB_TOKEN`. The whole result JSON never
  contains the canary, and the text is `[REDACTED:gh_token]`. An env
  placeholder is filled and redacted; base64 of the secret written to stderr
  is redacted in `stderr_tail`; status output doesn't contain it.
- **End-to-end canary** (`crates/mcpgw/tests/serve.rs`): a real vault file
  with the canary, a config binding it, `mcpgw serve --stdio` with
  `MCPGW_VAULT_PASSPHRASE`, and an MCP client calling `gh__env` gets only
  the marker.
- Missing secret and no-vault errors carry their hints.
- Redactor units: encodings, nested JSON and keys, longest match first.
- Canary strings are built at runtime, so `scripts/canary-scan.sh` (which
  forbids committed canaries) stays green.
