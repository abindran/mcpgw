# M3-06 — Warm pools

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M1-05
**Done when:** with `min = 1`, the first call has no cold start.
**Status:** ✅ Done

## Using it

```toml
[[server]]
name = "github"
transport = "container"
image = "ghcr.io/github/github-mcp-server:2.0.2"
warm_pool = { min = 1 }
```

```sh
mcpgw add fs --warm --command npx -- -y @modelcontextprotocol/server-filesystem /tmp
mcpgw status    # fs  idle (warm)  14 tools  pid 4242
```

A warm server:

- **starts with the gateway**, in the background, instead of on its first
  call;
- is **never stopped for idleness** and **never evicted** by `max_running`
  (like `stateful`, warm servers count toward the soft cap but are never
  picked to make room);
- **comes back by itself** after a crash, after the usual backoff, without
  waiting for a call;
- stays off while disabled.

`min` is 0 (the default: start on demand) or 1. Calls to a server share one
multiplexed connection, so a larger pool would need several instances per
server. Values above 1 are refused with that reason.

## Cold-start numbers

Every server's status now has:

- `cold_starts`: calls that had to wait for the server to start;
- `last_start_ms`: how long the last start took, from spawn or connect
  through the handshake and `tools/list`.

They appear in `/admin/status`, `GET /api/v1/servers` (`warm`,
`cold_starts`, `last_start_ms`) and `mcpgw status` (`(warm)`), and they
feed M3-07's prewarm measurements and the M3 exit metrics.

## Tests

- `crates/mcpgw-runtime/tests/warm.rs`: two fake servers take 1.5 s to
  start. The warm one is connected before any call, with `last_start_ms`
  at 1500 or more; the other stays cold.
  - **Done-when:** the warm server's first call finishes in well under the
    start delay, with `cold_starts = 0`. The cold server's first call waits
    1.5 s or more, with `cold_starts = 1`.
  - Past the idle timeout the cold server stops and the warm one stays up.
  - After a `crash` the warm server is back on its own, with a new pid and
    `restarts ≥ 1`, and answers without a cold start.
  - `min = 2` is refused, and `warm_pool = { min = 1 }` parses from TOML.
- `crates/mcpgw/tests/warm.rs` (end to end): after `mcpgw add hot --warm`
  and `mcpgw up`, `hot` shows `(warm)` with a pid before any call, while
  `lazy` stays `cold`. `hot` is still running past the idle timeout.
