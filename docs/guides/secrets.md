# Secrets and OAuth

## The vault

Secrets live in an encrypted vault (`vault.json` next to the store).

- **Keys.** Each value is encrypted with a fresh key of its own
  (XChaCha20-Poly1305, bound to the secret's name), and that key is wrapped
  by a key-encryption key.
  - The key-encryption key is kept in the OS keychain (macOS Keychain,
    Windows Credential Manager, Linux Secret Service).
  - Without a keychain it is derived from `MCPGW_VAULT_PASSPHRASE` with
    Argon2id (see [ADR 0003](../adr/0003-vault-crypto.md)).
  - Losing the keychain entry or the passphrase makes the vault unreadable.
- **Never in plain text.** Values never reach the store, the config, logs or
  the API: `mcpgw secret ls` and `GET /api/v1/secrets` show names and
  descriptions only.

```sh
mcpgw secret set github_token --description "GitHub PAT"   # prompts without echo; or pipe it in
mcpgw secret ls
mcpgw secret rm github_token
```

## Binding secrets to servers

```sh
mcpgw add gh --command github-mcp-server --secret GITHUB_TOKEN=github_token -- stdio
mcpgw add docs --url https://mcp.example.com/mcp --header 'x-api-key=${secret:docs_key}'
mcpgw add notes --image ghcr.io/example/notes --secret-file /run/secrets/key=notes_key
```

- **Forms.** `secrets = { VAR = "name" }` injects an environment variable.
  `${secret:name}` can also appear in args, env values and HTTP headers.
  Containers can get secrets as files (`files`).
- **At start only.** Values are injected when the server starts, never
  stored with it.
- **Redaction.** Any injected value the server sends back is replaced by
  `[REDACTED:name]`: in results, errors, logs and audit.
- **Missing secrets.** A secret that isn't set stops the start with a hint:
  "add it with `mcpgw secret set name`".

## OAuth

mcpgw ships no client credentials: register an OAuth app with the provider.
The redirect URI is `http://127.0.0.1:7777/oauth/callback`.

```toml
[[oauth]]
name = "github"
preset = "github"                     # github, google or atlassian
client_id = "Iv1.0123456789abcdef"
client_secret = "github_oauth_secret" # a vault secret name
```

```sh
mcpgw auth connect github --account work     # opens the browser (PKCE)
mcpgw auth ls
mcpgw add gh --command github-mcp-server --secret 'GITHUB_TOKEN=oauth/github@{account}' -- stdio
```

- **Storage.** Tokens are stored in the vault as `oauth/<provider>@<account>`.
- **Refresh.** The daemon refreshes tokens before they expire. A refused
  refresh shows the server as needing a login.
- **Remote servers.** Remote MCP servers that ask for OAuth themselves are
  discovered, registered dynamically and logged into the same way. A client
  that can show a link gets one, through URL elicitation, when a login is
  needed.

## Accounts

`{account}` in a binding makes a server account-scoped. Each account gets
its own instance (`server@account`), with its own process and token.

- The default endpoint uses `gateway.accounts` (`{ github = "work" }`).
- Each profile uses its own `accounts`.
- Without either, the account is `default`.

## OpenAPI security schemes

An OpenAPI server binds secrets to the spec's security schemes by name (see
[OpenAPI APIs](openapi.md)):

```sh
mcpgw add pets --openapi ./petstore.yaml --auth api_key=petstore_key
```
