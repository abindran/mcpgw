# M4-09 — Tasks extension pass-through for long-running tools

**Story:** US1 · **Estimate:** 3 days · **Depends on:** M0-05
**Done when:** a 10-minute tool call reports progress and can be cancelled.
**Status:** ✅ Done: a 10-minute call through the daemon delivers all 1,200 progress notifications, and the same call cancelled near the end stops at the backend (`ten_minute_call`, run on demand). CI runs the same paths at a few seconds each.

## What a client sees

| Client | Long call |
|---|---|
| Any | `notifications/progress` under its own `progressToken`, while it waits |
| Any | `notifications/cancelled` for the request stops the tool **at the backend** |
| Declares `io.modelcontextprotocol/tasks` (SEP-2663) | a call still running after 1 s returns a task (`resultType: "task"`) to poll with `tasks/get`; `tasks/cancel` stops it |

```toml
[gateway]
call_timeout = "2m"     # the most a call may go without a sign of life (result or progress)
max_call_time = "1h"    # the most a call may run at all
```

A call fails when its backend goes silent for `call_timeout`.

- **Progress resets the clock.** A backend that reports progress can run up
  to `max_call_time`; before M4-09 every call had a flat 2 minutes.
- **Tasks:**
  - the gateway advertises the tasks extension;
  - a task's `statusMessage` follows the call's progress ("step 3 of 6
    (3/6)");
  - a finished task carries the `CallToolResult`, or the JSON-RPC `error`;
  - tasks live for 1 hour (`ttlMs`), with a 500 ms suggested
    `pollIntervalMs`;
  - task ids are 128 random bits, and a task is visible only on the endpoint
    (default or profile) that created it.
  - `tasks/update` is refused: approvals are asked before a call starts, so
    a gateway task never waits for input.

## Pass-through, both ways

```
client ──tools/call (progressToken T)──▶ mcpgw ──tools/call (token t)──▶ backend
client ◀──progress T── mcpgw ◀──progress t── backend
client ──cancelled──▶ mcpgw  ── drops the call ──▶ notifications/cancelled ──▶ backend
client ──tasks/cancel──▶ mcpgw ── aborts the call ──▶ notifications/cancelled / tasks/cancel ──▶ backend
```

- **Progress.**
  - The front gives each call a `ProgressSink`, scoped with a tokio
    task-local (`mcpgw_proto::progress::with_progress`). The router needs no
    new parameters.
  - The manager reads the sink (`progress::current()`) and passes it, with
    the call, to the backend actor.
  - The backend connection registers the request's progress token, and its
    client handler routes `notifications/progress` by token.
  - A notification can beat the registration, so the latest early one per
    token is kept and delivered when the token registers.
- **Cancellation is dropping futures:**
  - the front aborts the spawned call when the request is cancelled or the
    client goes away;
  - the manager's call task notices its caller is gone (`reply.closed()`);
  - the backend request's `CancelOnDrop` guard sends
    `notifications/cancelled`.
- **Backend tasks.**
  - mcpgw declares the tasks extension to backends.
  - A backend that answers `tools/call` with a task is polled
    (`tasks/get`, at its `pollIntervalMs`, between 50 ms and 5 s) until the
    task ends. Its status messages become progress.
  - Dropping the call sends `tasks/cancel`.
  - So clients see one model whether the gateway or the backend runs the
    task.
  - A backend task that asks for input (`input_required`) fails the call
    with a clear error: relaying in-task input is not built.
- **Timeouts:**
  - rmcp's `reset_timeout_on_progress` and `max_total_timeout` set the idle
    and total limits on the backend request;
  - the manager's own backstop becomes `connect_timeout + max_call_time`;
  - a silent call still fails ("timed out after 1s" in the tests).

## Found on the way

The full 10-minute run found a bug from M1-05. Health checks treated a
`ping` answered with "method not found" (-32601, a 2026-07-28 server
without `ping`) as a dead server, so the server was restarted every 30 s.
Restarts under a running call broke its cancellation (CI's short calls
never lasted 30 s). Fixed in its own commit (M1-05).

## Code

- `mcpgw-proto`:
  - `progress.rs`: `Progress`, `ProgressSink`, `with_progress`/`current`,
    and per-connection `Routes`;
  - `tasks.rs`: the process-wide task store (create, progress,
    complete/fail, get, cancel, scope, TTL);
  - `front.rs`:
    - `Front::run` runs each call attempt as a spawned job, forwards
      progress, honours cancellation and turns into a task after
      `TASK_AFTER`;
    - `get_task`/`cancel_task`/`update_task`;
    - `ToolProvider::task_scope`;
  - `backend.rs`:
    - `CallLimits`, `call_tool_with`, `follow_task`;
    - the cancel-on-drop guards;
    - the `GatewayClient` progress routing and tasks capability.
- `mcpgw-runtime`: `ManagerOptions::max_call_time`, the sink in `Cmd::Call`,
  cancel on caller gone.
- `mcpgw-config`: `gateway.call_timeout`, `gateway.max_call_time`.
- `mcpgw-router`: the task scope is the endpoint label.
- `fake-mcp`:
  - `sleep` with `steps` reports progress and stops on cancellation,
    logging `sleep cancelled at <n>` or `sleep done`;
  - `FAKE_TASKS=1` runs it as an rmcp `TaskManager` task.
- `mcpgw-testkit`: `ClientKind.tasks`, `progress()`, `start_call` →
  `RunningCall::{cancel, finish}`, `call_once`, `get_task`, `cancel_task`.

## Tests

- `crates/mcpgw-runtime/tests/long.rs`, with a 1 s call timeout:
  - 4 s in 8 progress steps succeeds and passes all 8 on, while 3 s of
    silence times out;
  - a dropped call is cancelled at the backend, and the server keeps
    working;
  - a backend task is followed to its result, with status messages as
    progress, and dropping it sends `tasks/cancel`.
  - health checks every 300 ms leave a long call alone (no restart), and
    it can still be cancelled.
- `crates/mcpgw/tests/long_calls.rs`, through the daemon over Streamable
  HTTP (2026-07-28, stateless):
  - progress reaches the client, and silence still times out;
  - `notifications/cancelled` stops the backend tool;
  - for a tasks client: the capability is advertised, and quick calls are
    answered directly;
  - for both the gateway-run (`gw`) and backend-run (`bt`) cases: a task
    handle, the status message, completion with the result, and
    `tasks/cancel` stopping the backend;
  - unknown task ids are refused;
  - `ten_minute_call` (ignored in CI) is the done-when at full length.
- Unit tests: progress routing (including early notifications), the scoped
  sink, and the task store (scope, cancel, terminal states stick).
