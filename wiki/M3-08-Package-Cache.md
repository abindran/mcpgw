# M3-08 — Package pre-install cache for npm and PyPI servers

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M1-04
**Done when:** a Node server starts in under 2 s with no network access.
**Status:** ✅ Done: a cached Node server starts in about 65 ms offline, and a cached PyPI server in 0.75 s

## Using it

```sh
mcpgw add fs --command npx -- -y @modelcontextprotocol/server-filesystem@2025.8.21 /tmp
# installing @modelcontextprotocol/server-filesystem@2025.8.21 into the package cache
# installed @modelcontextprotocol/server-filesystem@2025.8.21 for fs (starts without npx)
mcpgw add time --command uvx -- mcp-server-time
mcpgw list            # … uvx mcp-server-time [cached mcp-server-time@2026.10.19]
mcpgw runtimes ls     # kind, name@version, spec, cache path
mcpgw runtimes install [server]   # (re)install, e.g. for servers added earlier
```

The cache is `gateway.runtimes_path`, by default `runtimes` next to the
store (the plan's "~/.mcpgw/runtimes"). `mcpgw add --no-cache` leaves a
server to npx or uvx. Registry installs (M2-14) of npm and PyPI entries are
cached the same way.

## How it works (`mcpgw-runtime::packages`)

- **Detection:** the command is `npx` (optionally `-y`, `--yes`, `-q`) or
  `uvx` (optionally `-q`) followed by a package spec, as written:
  `@scope/pkg@1.2.3`, `pkg`, a tarball path, or `pkg==1.0`. Launcher options
  like `--package` or `--from` aren't handled and fall back to npx or uvx.
- **Install:** each spec gets its own slot,
  `<cache>/<npm|pypi>/<spec>-<hash>`.
  - npm: `npm install --prefix <slot> --omit=dev`. The package's `bin` is
    found from its `package.json` (a string, a single entry, or the entry
    named after the package).
  - PyPI: `uv venv` plus `uv pip install` if `uv` is installed, else
    `python3 -m venv` plus pip. The command is the one named after the
    package, as `uvx` runs it. `pkg@1.0` is turned into `pkg==1.0`.
  - `mcpgw-package.json` (kind, spec, name, version, entry) is written last
    and marks a complete install; a failed install removes the slot.
- **Launch:** when the cache holds the server's package, `Launch` rewrites
  `npx -y pkg args…` to `node <slot>/node_modules/<pkg>/<bin> args…` and
  `uvx pkg args…` to `<slot>/venv/bin/<pkg> args…`. Nothing is resolved or
  downloaded at call time, and an unpinned spec stays at the version
  installed. The config keeps the original command, so `--no-cache` or a
  removed cache falls back cleanly.

## Verified

| Server | First start, from the cache, network unreachable |
|--------|---------------------------------------------------|
| Node test server (`npx -y node-fake-mcp-1.0.0.tgz`) | 64 ms (the test asserts < 2 s) |
| `mcp-server-time` 2026.10.19 from PyPI (`uvx mcp-server-time`, venv plus pip) | 0.75 s for `mcpgw list --check`, 2 tools |

"Unreachable" means `HTTP(S)_PROXY`, `npm_config_registry` and the pip
index all point at `127.0.0.1:9`. (`mcp-server-time==0.6.2` installed fine
but crashes on import against the current `mcp` SDK. That's upstream drift,
and `uvx` fails the same way.)

## Tests

- `crates/mcpgw-runtime/tests/packages.rs`: **done-when.** The Node test
  server is packed with `npm pack` and installed into the cache. The launch
  becomes `node …/node_modules/node-fake-mcp/server.js`, and the first call
  finishes in under 2 s with every proxy and the npm registry dead.
- `crates/mcpgw/tests/packages.rs`: `mcpgw add … --command npx -- -y <tgz>`
  installs and reads the tools from the cache. `list` shows `[cached
  node-fake-mcp@1.0.0]`, `runtimes ls` lists it, `runtimes install nf`
  reinstalls it, and `--no-cache` installs nothing.
- Unit tests: npx and uvx detection (flags, paths, unsupported options),
  slot naming, and PyPI name parsing.
- Both skip where npm isn't installed (CI runners have it).
