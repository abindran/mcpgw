# M4-02 — Wasm capability grants

**Story:** US2 · **Estimate:** 3 days · **Depends on:** M4-01
**Done when:** a component without a network grant cannot open a socket.
**Status:** ✅ Done

## Using it

```toml
[[server]]
name = "notes"
transport = "wasm"
path = "/opt/tools/notes.wasm"
allow_network = ["api.notes.example:443", "127.0.0.1:8080"]   # or ["*"] for any; default: none
allow_dirs = [{ path = "/srv/notes", write = true }, { path = "/srv/docs", guest = "/docs" }]
env = { REGION = "eu" }
secrets = { API_KEY = "notes_key" }    # injected into the component's environment
```

```sh
mcpgw add notes --wasm ./notes.wasm --allow-network api.notes.example:443 \
  --allow-dir /srv/docs --allow-dir-rw /srv/notes --env REGION=eu --secret API_KEY=notes_key
```

## Deny by default (`mcpgw_wasm::Grants`)

Each instance gets its own WASI context, built from its grants and
nothing else:

| Capability | Without a grant | With a grant |
|------------|-----------------|--------------|
| **TCP** | off: `create-tcp-socket` fails and connects are refused | connect only to the granted addresses. A `host:port` is resolved at each start; `*` means any destination |
| **DNS** (`ip-name-lookup`) | off | on when a grant names a host (or `*`) |
| **UDP, listening, accepting** | off | off: no grant opens them |
| **Files** | no preopened directories | each `allow_dirs` entry preopened at its guest path, read-only unless `write = true` |
| **Environment** | empty: the host's environment never leaks in | exactly `env`, plus `secrets` bindings |

Network checks run on every socket address the guest uses (wasmtime-wasi's
`socket_addr_check`). The only bind allowed is the implicit one a connect
makes. Secret values in a component's environment are redacted from what it
returns, as for every other server. `allow_network` entries must be
`host:port`, `ip:port` or `*` (validated at load).

## Tests

The probe component (`wasm/probe-tool`) imports `wasi:sockets`,
`wasi:filesystem` and `wasi:cli/environment`, using the WASI 0.2.12 WIT
vendored from wasmtime-wasi 49. Its tools are `connect`, `lookup`, `read`,
`write` and `env`, each answering `ok …` or `denied: …`.

- `crates/mcpgw-wasm/tests/grants.rs`:
  - **Done-when:** with no grants, connecting to a listening local port is
    `denied`, and so is DNS. An exact `127.0.0.1:<port>` grant connects. A
    grant for another port does not open this one. `*` connects anywhere,
    and DNS works only with a grant that allows it.
  - Files: with no grant there are no directories. A read-only grant reads
    but refuses writes, and nothing appears on the host. Paths outside the
    grant are refused. A read-write grant writes the host file.
  - Environment: empty by default, granted variables only, and the host's
    `PATH` never visible.
- `crates/mcpgw/tests/wasm.rs`, end to end: `--allow-network`,
  `--allow-dir`, `--env` and `--secret` on `mcpgw add` work through the
  daemon. The bound secret reaches the component and comes back as
  `[REDACTED:probe_key]`. A second server added with no grants is denied
  every one of them.
- Core: the TOML round trip, secret refs from `env`, and rejecting bad
  `allow_network` entries.

`cargo xtask wasm` now also checks that a guest's copy of the `mcpgw:tool`
WIT matches `crates/mcpgw-wasm/wit/tool.wit`.
