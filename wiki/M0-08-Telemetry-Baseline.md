# M0-08 — Telemetry baseline

**Story:** X · **Estimate:** 1 day · **Depends on:** M0-01
**Done when:** every request logs one span with method, tool and duration.
**Status:** ✅ Done

## What was built (`mcpgw-telemetry`)

- `init(format, default_filter)` installs the global subscriber on stderr
  (stdout carries MCP in `serve`).
  - Format: `--log-format pretty|compact|json`, else `MCPGW_LOG_FORMAT`, else
    `compact`. Colours only when stderr is a terminal; never in JSON.
  - Level filter: `MCPGW_LOG` (`EnvFilter` syntax), default `warn`.
- `request_id()` returns 16 hex characters: a random per-process prefix plus
  a counter, so ids are unique in a process and differ across processes.
- `instrument_request(method, tool, future)` runs a request inside a
  `request` span (`request_id`, `method`, `tool`) and emits **one** `request`
  event when it finishes, with `duration_ms` and `outcome` (`ok`, or `error`
  plus the error at WARN).
- The router's `GatewayProvider` wraps every `tools/list` and `tools/call`.

A JSON line looks like:

```json
{"timestamp":"…","level":"INFO","message":"request","duration_ms":0.412,"outcome":"ok",
 "target":"mcpgw_telemetry","span":{"method":"tools/call","request_id":"7d55f3ad00000003","tool":"fx__add","name":"request"}}
```

## Tests

- Unit: JSON output has exactly one line per request with method, tool,
  request id, duration and outcome; compact output has no colour codes off a
  terminal; ids are unique; formats parse.
- End to end (`crates/mcpgw/tests/serve.rs`): runs `mcpgw serve` against a
  fake backend over real stdio and checks that stderr has exactly three
  `request` lines for list, a good call and a failing call. This is also the
  first test of the shipped binary as an MCP server.
