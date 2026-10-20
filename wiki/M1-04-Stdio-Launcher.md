# M1-04 — stdio launcher

**Story:** US4 · **Estimate:** 4 days · **Depends on:** M0-10
**Done when:** killing a server removes its whole process tree, including `npx` children.
**Status:** ✅ Done

## What was built (`mcpgw-proto::backend`)

`BackendConn::connect_stdio(spec, timeout, stderr_sink)` launches the server
through `process-wrap`:

| Concern | Unix | Windows |
|---------|------|---------|
| Isolation | own process group (`ProcessGroup::leader`), pgid = server pid | Job Object |
| Tree kill | `TreeGuard` sends `killpg(pgid, SIGKILL)` when the connection is dropped or shut down | `KILL_ON_JOB_CLOSE`: closing the job handle kills every process in it |
| Env | `env` from the spec, plus `cwd` | same |
| stderr | piped, read line by line, sent to the sink (or the debug log) | same |

### Why the extra guard

rmcp's graceful shutdown closes the server's stdin and waits for it to exit.
Well-behaved servers do exit, so rmcp never signals the rest of the group
and children like the `node` process behind `npx` are left running. This was
observed in a test before the guard was added. The guard runs after the
service is torn down. The group id can't be recycled while any member is
alive, so killing an already-empty group only returns `ESRCH`.

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
