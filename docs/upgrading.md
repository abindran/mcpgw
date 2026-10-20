# Upgrading to v1.0

v1.0 is the M4 release. It reads everything earlier builds wrote: the
store, the vault, pins and the config file. An upgrade is a binary swap.

```sh
mcpgw down                 # stop the old daemon (clients reconnect later)
cargo install --locked --git https://github.com/abindran/mcpgw mcpgw   # the new binary
mcpgw doctor               # checks the config, vault, store, servers and clients
mcpgw up
```

## What carries over

| | |
|---|---|
| Store (`db.sqlite`) | Unchanged schema since M3; older stores migrate themselves on open (`PRAGMA user_version`). |
| Vault | Same format and keys (keychain or `MCPGW_VAULT_PASSPHRASE`). |
| Pins | Kept: servers aren't re-approved. |
| Config file | Every M3 key means the same; v1.0 only adds keys. |
| Clients | The same URL, token and `mcpgw connect` entry. |

## Behaviour changes to know about

- **Call timeout.** It counts from the last sign of life (M4-09). Before, a
  call had a flat 2 minutes.
  - A server that sends progress notifications can now run up to
    `gateway.max_call_time` (default 1 hour).
  - A silent call still fails after `gateway.call_timeout` (default 2
    minutes).
- **Health checks.** A `ping` answered with an error now counts as alive.
  2026-07-28 servers that don't implement `ping` were restarted every 30 s
  before; they now stay up. If you saw spurious restarts in
  `mcpgw status`, they stop.
- **Tasks.** The gateway advertises the tasks extension (SEP-2663). A client
  that declares it may receive a task for a call that takes more than 1 s.
  Clients that don't declare it are unaffected.
- **Progress** from backends now reaches clients, under the client's own
  progress token.
- **JSON key order.** Generated JSON (schemas, `docs/openapi.json`, API
  responses) keeps declaration order, not alphabetical order. Clients
  that compare JSON as text might notice; parsed JSON is the same.
- **The OpenAPI hint** for AND-ed security schemes lists them in the spec's
  order.

## New, all optional

| Area | What | Where |
|---|---|---|
| Wasm tools | `transport = "wasm"` (`mcpgw add --wasm`), deny-by-default grants, time and memory limits, the `mcpgw-tool` SDK and template | [Wasm tools](guides/wasm.md) |
| OpenAPI | `transport = "openapi"` (`mcpgw add --openapi`), security schemes bound to the vault (`auth`) | [OpenAPI APIs](guides/openapi.md) |
| Cedar | `policy = { cedar = "file.cedar" }`, `mcpgw policy convert`, `mcpgw policy check` | [Profiles and policy](guides/policy.md) |
| Long calls | `gateway.call_timeout`, `gateway.max_call_time`, tasks | [Long calls](guides/long-calls.md) |
| Dev loop | `mcpgw dev`, and the web UI's Inspector, which calls `POST /api/v1/tools/{name}/call` | [Developing a server](guides/dev.md) |

## Migration notes

- **Glob policies to Cedar** (optional; glob rules keep working):

  ```sh
  mcpgw policy convert --profile web -o policies/web.cedar
  # in the profile: policy = { cedar = "policies/web.cedar" }  (and remove allow/deny/approve)
  mcpgw policy check --profile web some__tool
  ```

  The converted policies give the same decisions, with the same messages.
  A `?` in a pattern can't convert; rewrite it with `*`.
- **`headers` with `${secret:…}` for OpenAPI servers** can move to `auth`,
  where each binding names the spec's security scheme. The scheme then
  decides where the credential goes (header, query or cookie).
- **Raised call timeouts.** If you raised a timeout to fit long tools, you
  can let the tool report progress instead and keep `call_timeout` short.

## Downgrading

A v1.0 store works with M3 builds: the schema is unchanged. Config keys added
in v1.0 make an M3 build refuse the file as having an unknown key. Remove
those keys, or keep a separate config, before going back.
