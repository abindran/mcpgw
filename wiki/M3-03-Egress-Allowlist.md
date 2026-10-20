# M3-03 — Egress allowlist per server

**Story:** US2 · **Estimate:** 3 days · **Depends on:** M3-02
**Done when:** a GitHub server cannot reach any host except api.github.com:443.
**Status:** ✅ Done (verified with the real GitHub server under Docker; Linux CI runs the test)

## Using it

```toml
[[server]]
name = "github"
transport = "container"
image = "ghcr.io/github/github-mcp-server:2.0.2"
egress = ["api.github.com:443"]   # [] = no network; unset = unrestricted
secrets = { GITHUB_PERSONAL_ACCESS_TOKEN = "github_pat" }
```

```sh
mcpgw add github --image ghcr.io/github/github-mcp-server:2.0.2 \
  --egress api.github.com:443 --secret GITHUB_PERSONAL_ACCESS_TOKEN=github_pat
mcpgw add calc --image example/calc-mcp:1 --no-network
```

Entries are `host:port` with a host name. An IP address can't be listed,
because the allowlist routes by name. `--network`, `--add-host` and `--dns`
in `run_args` are refused when `egress` is set.

## How it works (network policy plus a proxy)

For a server with an allowlist, each start sets up three things:

1. **An internal network** `mcpgw-<server>-<id>-net` (`network create
   --internal`). It has no route out.
2. **A proxy sidecar** `…-egress` on the default network and the internal
   one, running the official `nginx:1.27-alpine` image (pinned like any
   image, 64 MiB, 64 processes). Its stream config comes from the
   environment:
   - a port with **one** allowed host passes connections straight through
     to it;
   - a port **shared** by several hosts reads the TLS SNI and routes by name.
     Unknown names go to a dead end.
3. **The server** runs only on the internal network, with `--add-host
   <allowed host>:<sidecar address>`.

So the allowed names resolve to the sidecar, and TLS passes through
untouched: certificates are verified end to end, and the server needs no
proxy settings. Every other name either doesn't resolve or has no route,
and every other port on the sidecar is closed. Startup waits for nginx to be
ready; a bad upstream name fails the start with nginx's error. The sidecar
logs each connection (`docker logs <name>-egress`).

`egress = []` runs the server with `--network=none`.

The sidecar and network carry the `mcpgw.*` labels. They are removed after
the server's container when it stops. A failed handshake keeps them for
the retry (proto's new `teardown`, which runs only when a connection ends
for good). The startup sweep also removes networks left by a crashed
daemon. This works with Podman too (`network create --internal`,
`--add-host`).

## Verified with the real GitHub server (macOS arm64, Docker 27.4)

Both servers were given a deliberately invalid placeholder PAT, so a
reachable GitHub answers 401:

| Allowlist | `get_me` result |
|-----------|-----------------|
| `api.github.com:443` | `GET https://api.github.com/user: 401 Bad credentials`: reached GitHub over verified TLS |
| `example.com:443` | `lookup api.github.com … server misbehaving`: no way out |

Without a token the server tries an OAuth device flow on `github.com`.
That was blocked under the `api.github.com:443` allowlist, as it should be.

## Tests

`crates/mcpgw-runtime/tests/egress.rs` (the Node test server's `connect`
tool opens TCP or verified TLS connections):

- An unrestricted server reaches `example.com:443`. This is the
  precondition; without internet the test skips, except on CI, where it
  fails.
- **Done-when:** with `["api.github.com:443"]`, a verified TLS connection to
  `api.github.com:443` works. `example.com:443`, `github.com:443`,
  `api.github.com:80`, `1.1.1.1:443` and a GitHub IP address are all
  unreachable, and the network is `Internal`.
- With `["api.github.com:443", "github.com:443"]` both names work over the
  shared port, and an unlisted SNI is refused.
- `[]`: nothing is reachable and `NetworkMode` is `none`.
- After shutdown no container (server or sidecar) or network is left.
- Unit tests: nginx config generation (pass-through and SNI map), egress
  entry parsing (host names only, ports 1 to 65535, no wildcards or IPs),
  and refusing network flags alongside egress. The sweep test now covers
  networks too.
