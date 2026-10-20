# M3-07 — Predictive prewarm

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M3-06, M1-10
**Done when:** cold start is hidden in more than 80% of search-then-call sequences in the e2e suite.
**Status:** ✅ Done: hidden in 10 of 10 sequences (100%)

## What it does

In meta mode the model looks for a tool with `search_tools`, then calls it
with `call_tool` a moment later. That moment is its "think time", usually a
second or more. When `search_tools` answers, the gateway starts, in the
background, the cold servers behind the **top 3 hits**
(`meta::PREWARM_TOP`). By the time the call comes the server is usually
ready, so the call doesn't wait for a cold start.

`BackendManager::prewarm(server, account)`:

- only starts a server that is **cold** (not disabled, failing, quarantined
  or already up);
- is **speculative**, so it never evicts anything: it starts only when fewer
  than `max_running` servers are running or starting;
- uses the account the endpoint maps that server to (M2-03), so the right
  `server@account` instance starts;
- counts each start in the server's `prewarms`, next to M3-06's
  `cold_starts` and `last_start_ms` (admin status, `GET /api/v1/servers`).

A prewarmed server that isn't called stops after the usual idle timeout.
Turn the feature off with:

```toml
[gateway]
prewarm = false
```

## Measured (e2e, macOS arm64)

There were ten fake servers, each taking 700 ms to start and each with one
tool on its own topic. Through the daemon in meta mode, each sequence ran
`search_tools {query: <topic>}`, waited a 1 s think time, then ran
`call_tool` on the hit.

| | Cold start hidden | Call latency |
|---|---|---|
| prewarm on | **10 / 10 (100%)** (`cold_starts = 0` everywhere) | 4–9 ms |
| prewarm off | 0 / 3 (`cold_starts = 1` each) | ~750 ms |

## Tests

- `crates/mcpgw/tests/prewarm.rs` (end to end):
  - `search_then_call_hides_the_cold_start` checks the done-when through
    the v1 API: more than 80% of servers have `cold_starts = 0`, and fewer
    than 20% of calls waited a full start. It prints the ratio and the
    latencies.
  - `without_prewarm_every_call_waits` is the baseline with `prewarm =
    false`: every call waits 700 ms or more and counts a cold start.
- `crates/mcpgw-runtime/tests/warm.rs::prewarm_only_with_room`: with
  `max_running = 1` and another server running, prewarm does nothing. Once
  there's room it starts the server once. Running and unknown servers are
  left alone.
