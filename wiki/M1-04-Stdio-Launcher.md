# M1-04 — stdio launcher

**Story:** US4 · **Estimate:** 4 days · **Depends on:** M0-10
**Done when:** killing a server removes its whole process tree, including `npx` children.
**Status:** ✅ Done

## What was built (`mcpgw-proto::backend`)

`BackendConn::connect_stdio(spec, timeout, stderr_sink)` launches the server
through `process-wrap`:

| Concern | Unix | Windows |
|---------|------|---------|
| Isolation | own process group (`ProcessGroup::leader`), pgid = server pid | Job Object (`JobObject` + `KillOnDrop` → `KILL_ON_JOB_CLOSE`) |
| Tree kill | `start_kill` on the group → `killpg(SIGKILL)` | `start_kill` on the job → `TerminateJobObject` |
| Env | `env` from the spec, plus `cwd` | same |
| stderr | piped, read line by line, sent to the sink (or the debug log) | same |

`mcpgw-proto` spawns the wrapped child itself and builds the MCP transport
from its stdin and stdout pipes, so `BackendConn` owns the process.
`shutdown()` closes the session, gives the server 2 s to exit on its own,
then kills what is left of the tree. Dropping a connection (a crash, or a
failed handshake) kills the tree immediately; the child is then reaped in
the background, so no zombies are left.

### Why not rmcp's child transport

rmcp's `TokioChildProcess` only waits for the server to exit after closing
stdin. A server that exits by itself, or crashes, leaves its children (the
`node` process behind `npx`) running. That was observed on macOS for both
stop and crash, and on Windows for crash, before this design. Killing the
group or job covers every case, even after the leader is gone: a process
group id can't be reused while any member is alive, and a job outlives its
first process.

### stderr capture

`mcpgw-runtime` keeps the last 200 stderr lines per server across restarts
(`BackendManager::stderr_tail(name)`) and logs each line at debug level
under target `mcpgw::backend` with the server name. `mcpgw logs` (M1-13)
reads from it. The pipe is always drained, so a chatty server never blocks on
a full stderr pipe.

## Tests (`mcpgw-runtime/tests/lifecycle.rs`)

- `stopping_a_server_kills_its_process_tree`: the fake's `spawn` tool starts
  an idle grandchild; after `shutdown` the grandchild is gone.
- `restart_after_crash_cleans_up_grandchildren`: the server crashes and
  leaves a grandchild behind; the supervisor's restart path removes it.
- `captures_stderr_lines`: a startup line and a line written during a tool
  call both appear in `stderr_tail`.

These tests use long health-check intervals, so a heavily loaded machine
can't trigger a restart (which would rightly kill the tree) before the test
checks it.
