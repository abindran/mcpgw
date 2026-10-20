# M3-05 — Memory limits for stdio servers

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M1-04
**Done when:** stdio servers respect memory caps on all three OSes.
**Status:** ✅ Done (the test runs on macOS arm64 and x86_64, Linux glibc and musl, and Windows in CI)

## Using it

```toml
[[server]]
name = "fs"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem@2025.8.21", "/tmp"]
limits = { memory = "256MiB" }
```

```sh
mcpgw add fs --memory 256MiB --command npx -- -y @modelcontextprotocol/server-filesystem /tmp
```

Stdio servers take `memory` only. `cpus` and `pids` need a container server
([M3-02](M3-02-Sandbox-Profiles.md)), and remote servers take no limits.

## How each OS enforces it (`mcpgw-proto::limits`)

| OS | Enforced by | Scope |
|----|-------------|-------|
| Linux | `RLIMIT_DATA` set in the child before `exec` (kernel), plus the watchdog | each process (rlimit); the process group's resident total (watchdog) |
| macOS | the watchdog: the process group's resident memory every 250 ms, then `killpg(SIGKILL)` | the process group |
| Windows | a nested Job Object with `JOB_OBJECT_LIMIT_JOB_MEMORY` (kernel) | the whole tree's committed memory |

- **Why not `RLIMIT_AS`:** V8 (Node) and other JIT runtimes reserve far more
  address space than they use. Under a 256 MiB `RLIMIT_AS`, Node doesn't
  even start, while under `RLIMIT_DATA` it starts and fails only when it
  really goes over (checked in a Linux container).
- **macOS** enforces no memory rlimit (`ulimit -d` doesn't stick, and 400
  MiB was allocated anyway), so the watchdog is what holds the cap there. It
  samples, so a very fast spike can briefly exceed the cap before the kill.
- **Windows:** process-wrap's own job (kill-on-close) holds the tree. The
  server is also put in a second job carrying the cap; Windows 8 and later
  nest jobs. Its peak use afterwards tells whether the cap was what stopped
  the server.

## Reported, not fatal

When a capped server dies, `last_error` says why: `connection closed: out
of memory: killed at its 128MiB limit (using 131MiB)` from the watchdog,
`… stopped at its 128MiB limit` from the Windows job, or `… out of memory at
its 128MiB limit` when the kernel refused an allocation and the server's
last stderr lines say so (Node's "heap out of memory", Rust's "memory
allocation of … failed", Python's `MemoryError`, `ENOMEM`). The same line
goes to the server's log as `mcpgw: …`. The call in flight fails. The
gateway and other servers carry on, and the server starts again on its next
call.

## Tests

- `crates/mcpgw-runtime/tests/limits.rs` (every CI OS): a 128 MiB fake-mcp
  holds 16 MiB fine, and an uncapped one holds 200 MiB. Allocating 400 MiB
  in steps fails that call, and the server is reported "out of memory …
  128MiB" in status and in its log. The other server keeps answering, and
  the capped one answers again on demand. Limits are checked by transport:
  memory only for stdio, none for remote servers.
- Unit tests: OOM line detection, and measuring a process group's resident
  memory (Linux and macOS).
- fake-mcp has a new `alloc` tool (enabled through `FAKE_TOOLS`).
