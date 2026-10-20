# M3-02 — Sandbox profiles and limits

**Story:** US2 · **Estimate:** 3 days · **Depends on:** M3-01
**Done when:** a server exceeding its 256 MiB limit is killed and reported, not the gateway.
**Status:** ✅ Done (verified with Docker 27.4; Linux CI runs the tests with Docker)

## Using it

```toml
[[server]]
name = "notes"
transport = "container"
image = "ghcr.io/example/notes-mcp:1.4"
sandbox = "strict"                                   # standard (default) | strict | off
limits = { memory = "256MiB", cpus = 0.5, pids = 64 } # override the profile's defaults
```

```sh
mcpgw add notes --image ghcr.io/example/notes-mcp:1.4 --sandbox strict --memory 256MiB --cpus 0.5
```

| Profile | Flags | Default limits |
|---------|-------|----------------|
| `standard` (default) | `--cap-drop=ALL --security-opt=no-new-privileges` | 1 GiB memory, 512 processes |
| `strict` | standard + `--read-only --tmpfs=/tmp:rw,nosuid,nodev,size=64m --user=65534:65534` | 512 MiB, 1 CPU, 128 processes |
| `off` | none | none (only the server's own `limits`) |

- Memory is set together with `--memory-swap` to the same value, so
  there's no swap: going over the limit is an OOM kill, not a slowdown.
- `run_args` come after the profile's flags, so an image that needs one
  capability can add it (`run_args = ["--cap-add=NET_BIND_SERVICE"]`).
- **Rootless:** `strict` runs the server as `nobody` inside the container.
  Whether the runtime itself is rootless (rootless Docker, or Podman as a
  user) is detected once per runtime and reported as `container.rootless`
  in status and the API.
- Stdio servers take a `memory` limit too ([M3-05](M3-05-Stdio-Limits.md));
  `cpus` and `pids` need a container. Remote servers take no limits.

## Killed and reported

mcpgw always removes containers itself (no `--rm`), so a container that died
can still be inspected first. When a container server's connection is lost,
the launcher reads `State.OOMKilled`, `ExitCode` and `HostConfig.Memory`
before it removes the container. Then:

- `last_error` becomes `connection closed: out of memory: the container was
  killed at its 256MiB limit` (other causes: `killed (SIGKILL)`, `exited
  with code N`), and shows in `mcpgw status`, the API and the web UI.
- The server's log (`mcpgw logs <server>`) gets `mcpgw: out of memory: …`,
  and the daemon log gets a warning.
- The call that was in flight fails. The gateway and its other servers carry
  on, and the server is started again (backoff) on its next call.

## Tests

`crates/mcpgw-runtime/tests/sandbox.rs`, using the Node test server, which
now has `alloc`, `write` and `uid` tools:

- **Done-when:** a 256 MiB server runs under the standard profile (inspect
  shows `Memory = MemorySwap = 268435456`, `CapDrop [ALL]`,
  `no-new-privileges`, `PidsLimit 512`) and holds 64 MiB. Allocating
  400 MiB more kills it, and the report says "out of memory: the container
  was killed at its 256MiB limit", in status and in its log. The container
  is removed. A second server keeps answering, and the killed one answers
  again on demand.
- `strict`: uid 65534, writing `/opt/x` gives `EROFS`, `/tmp/x` is
  writable, and inspect shows `ReadonlyRootfs true`, 512 MiB, 1 CPU, 128
  processes and user `65534:65534`. `off`: root, a writable filesystem, no
  limits.
- Unit tests: size parsing (`256MiB`, `512m`, `1 GiB`, bytes), limit
  validation and merging, and each profile's flags.

The real GitHub server (M2-14) starts and lists its 46 tools under the
default `standard` profile.

While building this, a shutdown bug turned up: closing a connection in a
spawned task could be cut off when the daemon's runtime ended, which `--rm`
had hidden for containers. Each actor now waits for its background closes
before it exits.
