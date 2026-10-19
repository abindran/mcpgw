# M1-03 — Backend process lifecycle

**Crates:** `mcpgw-proto` (`src/backend.rs`), `mcpgw-client` (`src/manager.rs`) · **Status:** ✅ Done

## Protocol seam (`mcpgw-proto::backend`)
- `BackendConn::connect_stdio(spec, timeout)` spawns the child (rmcp `TokioChildProcess`), stdout = protocol, stderr inherited, child killed on drop.
- Lifecycle `Auto { preferred: [2026-07-28, 2025-11-25], legacy: 2025-11-25 }`: modern servers get `server/discover`, legacy servers that answer `-32601` fall back to `initialize`. (Both versions must be listed — M0-05 finding.)
- `list_tools`, `call_tool` (name + arguments only, no front `_meta`), `ping`, `shutdown`, `protocol_version`, `is_closed`.
- The API is **rmcp-free**: tools/results are `serde_json` objects and errors are `ProtoError { Rpc{code,message,data}, Timeout, Closed, Connect, Unavailable, Other }`, so other crates keep obeying the dependency rule.

## Supervisor (`mcpgw-client::BackendManager`)
One tokio task per enabled stdio backend:

```
Starting ──connect+tools/list ok──▶ Running ──ping fails / pipe closed / call saw Closed──▶ Restarting
    │                                  ▲                                                      │
    └──fail──▶ Restarting(attempt n) ──┴──── backoff: initial·2ⁿ, capped at max ◀─────────────┘
shutdown() ─▶ Stopped (child terminated)
```

- The tool list is cached per backend and refreshed on every (re)connect.
- A tool call that hits a closed transport wakes the supervisor at once, so it does not wait for the next health tick.
- `http` servers and `enabled = false` servers report `Unsupported(..)` (HTTP lands in M2).
- `ManagerOptions` defaults: connect 30 s, call 120 s, health every 30 s, ping 10 s, backoff 0.5 s → 30 s.
- Calls to a server that isn't connected fail fast with `ProtoError::Unavailable`.

## Test fixture
`testdata/fixture_server.py` is a dependency-free **legacy 2025-11-25** stdio server (it rejects `server/discover` with `-32601`, like real pre-2026 servers). Tools: `add`, `echo` (uses `$FIXTURE_PREFIX`), `crash` (exits), `sleep`. `FIXTURE_HANG_PING=1` makes it ignore pings. Tests need `python3` (present on GitHub's `ubuntu-latest`).

## Tests
`crates/mcpgw-client/tests/lifecycle.rs` runs 5 tests against real child processes: legacy negotiation plus a call, env passthrough and RPC errors, restart after a crash, restart after a hung health check, and bad-command backoff together with the unsupported/disabled/unknown-server cases. There is also a unit test for the backoff curve. Five back-to-back runs passed without flakes, and no child processes were left behind.
