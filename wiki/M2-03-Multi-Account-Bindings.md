# M2-03 — Multi-account bindings

**Story:** US2 · **Estimate:** 2 days · **Depends on:** M2-01
**Done when:** two profiles call the same server with different accounts.
**Status:** ✅ Done (endpoint-level; profiles supply the account map from M2-07)

## Binding a server to "the caller's account"

```toml
[[server]]
name = "github"
transport = "stdio"
command = "github-mcp-server"
args = ["stdio"]
secrets = { GITHUB_TOKEN = "oauth/github@{account}" }

[gateway]
accounts = { github = "work" }   # the default endpoint's choice; else "default"
```

```sh
mcpgw auth connect github --account work
mcpgw auth connect github --account personal
mcpgw add github --command github-mcp-server --secret 'GITHUB_TOKEN=oauth/github@{account}' -- stdio
```

`{account}` may appear in `secrets` values and inside `${secret:…}`
placeholders in args, env and headers (the placeholder parser steps over the
nested braces).

## How it runs (`mcpgw-runtime`)

- A server whose secrets use `{account}` is **account-scoped**
  (`ServerSpec::account_scoped`). The server itself serves the `default`
  account.
- `BackendManager::call_tool_as(server, account, …)` routes another account
  to an **instance** keyed `server@account`, created on first use from
  `ServerSpec::for_account(account)`. Each instance is its own actor and
  process with its own injected token, so accounts never share a process or
  credential. Instances:
  - share the base server's pinned manifest (`catalog_key`), so drift checks
    and listings use one pin
  - are not listed in `names()` (tools appear once), but do appear in
    `status` as `github@work` with their own pid
  - count toward `max_running`, idle out like any server, and go away when
    the server is disabled or removed (they come back on demand)
- Introspection at `add` time uses the `default` account.
- Servers without `{account}` ignore the account: no instances.

## Routing (`mcpgw-router`)

`GatewayOptions.accounts` (server → account) is applied to every call,
direct or through `call_tool`. The default `/mcp` endpoint takes it from
`gateway.accounts`; each profile endpoint (M2-07) will take it from the
profile's `accounts`.

## Tests

- `crates/mcpgw-router/tests/accounts.rs`, **three endpoints over one
  manager** (`github → work`, `github → personal`, none): the server's
  `GITHUB_TOKEN` is `oauth/github@work`, `…@personal` and `…@default`
  respectively (seen through redaction markers), in either order. Status
  shows `github`, `github@personal` and `github@work` with **three distinct
  pids**; both listings are identical. Disable stops every account instance;
  after enabling, accounts come back; remove clears everything.
- A server without `{account}` ignores the account map.
- Core: `account_scoped`, `for_account`, validation of `{account}` names.
