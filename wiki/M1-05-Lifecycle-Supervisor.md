# M1-05 — Lifecycle supervisor

**Story:** US4 · **Estimate:** 5 days · **Depends on:** M1-04
**Done when:** state machine tests cover every transition; concurrent first calls spawn once.
**Status:** ✅ Done

## Design (`mcpgw-runtime`)

### State machine (`state.rs`)

`step(state, event, failures, max_failures) -> (next, action)` is a pure
function. States: `Cold`, `Starting`, `Ready`, `Busy{inflight}`, `Idle`,
`Draining{inflight}`, `Backoff{attempt}`, `Failed`, `Disabled`. Events:
`Demand`, `StartOk`, `StartErr`, `CallStart`, `CallEnd`, `Lost`, `Stop`,
`RetryDue`, `Reset`. Actions: `Spawn`, `Close`, `Retry` (close and arm the
backoff timer).

| From | Event | To |
|------|-------|----|
| Cold | Demand | Starting (spawn) |
| Starting | StartOk / StartErr / Stop | Ready / Backoff{1} or Failed / Draining{0} |
| Ready, Idle | CallStart / Stop / Lost | Busy{1} / Cold (close) / Backoff{1} |
| Busy{n} | CallStart / CallEnd / Stop / Lost | Busy{n+1} / Busy{n-1} or Idle / Draining{n} / Backoff{1} |
| Draining{n} | CallEnd / StartOk, Lost / StartErr | Draining{n-1} or Cold (close) / Cold (close) / Cold |
| Backoff | RetryDue, Stop / Demand | Cold / Backoff (call queues) |
| Failed | Reset / Demand | Cold / Failed (call refused) |
| Disabled | anything | Disabled |

Every other pair is `Invalid` and ignored with a debug log (a stale event,
such as a call ending after its connection was already lost).

### One actor per server (`manager.rs`)

- Callers send `Call`, `Ensure`, `Stop`, `Reset` and `Shutdown` commands
  over a channel. Spawns, tool calls and health pings run in their own tasks
  and report back as internal events. The actor never blocks, so status reads
  and other calls stay responsive while a server starts.
- Calls to a server that isn't connected **queue**, and one `Demand` starts
  it. Concurrent first calls therefore cause exactly one spawn.
- A queued call whose caller gave up (the call timed out) is dropped rather
  than run late, so a side-effecting tool never runs after the client stopped
  waiting.
- A failed start fails the queued calls at once with the error. After
  `max_failures` (default 5) consecutive failures the server is `Failed`;
  calls get "failed to start N times (last error …); run `mcpgw restart`"
  until `reset`.
- `Stop` drains: in-flight calls finish, no new ones start, then the
  connection closes (killing the tree). Calls that arrive while draining
  start the server again afterwards.
- `eager` (default on for now) starts every enabled server at boot and
  restarts it after a crash. M1-06 switches the default to lazy.
- Per-server `startup_timeout` overrides the connect deadline.
- Status: `state`, `restarts`, `protocol_version`, `last_error`, `pid`,
  `last_used`.

## Tests

- `state.rs`: a table of the expected result for **all 12 × 9
  (state, event) pairs**, failure counting into `Failed`, `Reset`, and a full
  session walk.
- `lifecycle.rs` against `fake-mcp`:
  - **10 concurrent calls to a cold server → one spawn** (`FAKE_SPAWN_LOG`)
    and ten correct answers
  - lazy start on first call (no pid before)
  - stop drains an in-flight call, then a new call restarts the server
  - repeated start failures end in `Failed`; `reset` clears it
  - a lazy start failure is reported to the caller
  - crash restart, health-check restart, disabled and unsupported servers,
    the slow-legacy-start regression, process-tree cleanup, stderr capture

## Fix (found in M4-09)

A health-check `ping` answered with a JSON-RPC **error** counts as alive. 2026-07-28
servers may not implement `ping` and reply -32601; before the fix that marked them
lost, so every such server was restarted every 30 s, even in the middle of a call (it
broke cancelling a 10-minute call). Only timeouts and transport errors fail a health
check now. Test: `lifecycle.rs::a_ping_answered_with_an_error_is_alive`.

The fix also showed that `failed_health_check_triggers_restart` had been passing
for the wrong reason. rmcp answers `ping` before any handler runs, so
`FAKE_HANG_PING` hung nothing; the restart came from the -32601 above. The fake now
drops `ping` requests on its stdin, so the gateway's health check really times out.

