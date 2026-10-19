# M0-10 — Test harness: fake MCP servers

**Story:** X · **Estimate:** 1 day · **Depends on:** M0-04
**Done when:** integration tests can start a fake server in under 50 ms.
**Status:** ✅ Done

## What was built

A test-only crate, `mcpgw-testkit`, with a `fake-mcp` binary and helpers.

`fake-mcp` serves over **stdio** by default or **Streamable HTTP** with
`--http ADDR` (it prints `listening <url>` once bound). Behaviour is set with
environment variables, so a `ServerConfig` can configure it:

| Variable | Effect |
|----------|--------|
| `FAKE_PROTOCOL` | `legacy` (≤ 2025-11-25), `modern` (2026-07-28 only) or `both` |
| `FAKE_TOOLS` | JSON array replacing the built-in tools (`name`, `description`, `inputSchema`, `reply`) |
| `FAKE_DESC_SUFFIX` | appended to every description (for drift tests) |
| `FAKE_PREFIX` | prefix for `echo` replies |
| `FAKE_START_DELAY_MS` | delay before serving (cold-start tests) |
| `FAKE_CRASH_AFTER` | exit after N tool calls |
| `FAKE_HANG_PING` | never answer `ping` |
| `FAKE_STDERR` | line written to stderr at startup |
| `FAKE_SPAWN_LOG` | file the process appends its pid to (spawn counting) |

Built-in tools: `add`, `echo`, `crash`, `sleep`, `env` (returns an env var —
used by the secret canary test), `spawn` (starts an idle grandchild and
returns its pid — used by process-tree tests) and `stderr`.

Helpers in `mcpgw_testkit`:

- `fake_mcp()` — path to the binary. It runs `cargo build -p mcpgw-testkit
  --bin fake-mcp` once per test process, so `cargo test -p <any crate>` works
  and the binary is never stale. Release and `--target` builds are followed.
- `stdio(name, env)` — a `ServerConfig` that launches it.
- `HttpFake::start(env)` — runs it on a random loopback port; killed on drop.

## Decisions

- The fakes are built on `rmcp`, so they speak both protocol versions exactly.
  `mcpgw-testkit` is the one exception to the dependency rule
  (`scripts/check-deps.sh`); it is `publish = false` and only a dev-dependency.
- The Python fixture (`testdata/fixture_server.py`) is removed. Tests no longer
  need `python3`, which also unblocks the Windows CI target (M0-02).

## Tests

`crates/mcpgw-testkit/tests/fake.rs`: every protocol mode negotiates the
expected version, built-in and scripted tools, crash-after, Streamable HTTP,
and start-up time (best of five under 50 ms; about 5 ms locally).
Runtime, router and CLI tests now use the fake.
