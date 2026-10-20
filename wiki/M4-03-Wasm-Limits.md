# M4-03 — Wasm limits: time budget and memory cap

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M4-01
**Done when:** an infinite-loop tool is stopped within its time budget.
**Status:** ✅ Done: a 300 ms budget stops a spin at 300.6 ms

## Using it

```toml
[[server]]
name = "notes"
transport = "wasm"
path = "/opt/tools/notes.wasm"
time_budget = "5s"                 # per call; default 30s
limits = { memory = "64MiB" }      # per instance; default and maximum 256MiB
```

```sh
mcpgw add notes --wasm ./notes.wasm --time-budget 5s --memory 64MiB
```

`cpus` and `pids` don't apply to Wasm servers and are refused.

## How (`mcpgw_wasm::Limits`)

- **Time: epoch interruption.** The engine has `epoch_interruption` on,
  and a thread advances the epoch every **10 ms**. Each store's deadline
  callback runs at every tick:
  - within the budget it **yields** to the async executor, so a call never
    holds a thread, and a caller's timeout (dropping the call) stops the
    guest at the next tick;
  - past the budget it **interrupts**, which traps the guest.
- **Memory: a resource limiter.** It refuses `memory.grow` beyond the cap
  and records that it did, so the trap that follows (Rust's allocation
  failure) is reported as running out of memory. The pooling allocator's
  slots are 256 MiB, the upper bound for any cap.
- **Reported, not fatal.** A stopped call comes back as a **tool error**
  (`isError`) the model sees: `stopped: it ran past its 300ms time budget`,
  or `stopped: out of memory at its 32MiB limit`. A trapped component
  instance can't be entered again, so the server builds a fresh instance
  (microseconds) for the next call. The connection, pins and status are
  unaffected.

## Tests

The echo fixture gained `alloc {mib}`; `spin` loops forever.

- `crates/mcpgw-wasm/tests/limits.rs`:
  - **Done-when:** under a 300 ms budget `spin` is stopped at 300–800 ms
    (measured 300.6 ms), as a tool error naming the budget, and the next
    call answers.
  - A 32 MiB cap holds `alloc 8` and stops `alloc 64` with "out of memory
    at its 32MiB limit". The next call answers, and a cap above 256 MiB is
    refused.
  - With a 60 s budget, a caller's 200 ms timeout cancels `spin` almost
    immediately, because the guest yields every tick.
- `crates/mcpgw-runtime/tests/wasm.rs`: `time_budget` and `limits.memory`
  apply under the manager. Stopped calls are tool errors and the server
  stays connected and keeps serving.
- `crates/mcpgw/tests/wasm.rs`: `mcpgw add … --time-budget 300ms`, and
  `spin` through the daemon is stopped with the budget message.
- Core: memory caps are accepted for Wasm servers, and `cpus`/`pids` are
  refused.
