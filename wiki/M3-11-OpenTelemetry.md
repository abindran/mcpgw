# M3-11 — OpenTelemetry OTLP export

**Story:** X · **Estimate:** 2 days · **Depends on:** M0-08
**Done when:** a Jaeger or Grafana stack shows one trace from client call to backend.
**Status:** ✅ Done (verified against Jaeger 2.11 in Docker; Linux CI runs it)

## Using it

```toml
[gateway]
otlp_endpoint = "http://127.0.0.1:4318"   # OTLP over HTTP
```

or the standard variables, which win: `OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_EXPORTER_OTLP_PROTOCOL` (`http/protobuf`, the default, or
`http/json`), `OTEL_SERVICE_NAME` (default `mcpgw`), plus the SDK's own
(`OTEL_BSP_SCHEDULE_DELAY`, `OTEL_METRIC_EXPORT_INTERVAL`, …). Only the
daemon (`mcpgw serve`, including `--stdio`) exports, and it flushes at
exit. What goes to OTLP is filtered by `MCPGW_OTEL_FILTER` (default
`info`), separately from the stderr log (`MCPGW_LOG`). A collector on
loopback is reached directly, never through `HTTP(S)_PROXY`.

## What's exported (`mcpgw-telemetry::otel`)

Built on tracing-opentelemetry 0.34 and opentelemetry 0.33 (OTLP/HTTP
exporters, batch spans, periodic metrics, the log bridge).

**Traces.** One trace runs from the client's call to the backend:

```
client span (00f067aa0ba902b7)            ← the client's traceparent
└─ tools/call fx__echo        SERVER      ← the gateway's request span
   └─ backend tools/call fx   CLIENT      ← its traceparent goes to the backend in _meta
      └─ (the backend's own spans, if it traces)
```

- The client's W3C `traceparent`/`tracestate` is read from the request's
  `_meta`, or else the `traceparent` HTTP header.
- The backend call carries `_meta.traceparent` of the `backend` span, for
  stdio and HTTP backends alike (proto `call_tool_with_meta`, runtime
  `call_tool_meta`).
- Failed requests get `otel.status_code = ERROR`.

**Metrics** come from `tracing` events through the metrics layer, so other
crates stay free of OpenTelemetry:

| Metric | Kind | Attributes |
|--------|------|------------|
| `mcpgw_tool_calls` | counter | tool, endpoint, outcome (`ok`, `tool_error`, `error`) |
| `mcpgw_tool_call_duration_ms` | histogram | tool, endpoint, outcome |
| `mcpgw_request_duration_ms` | histogram | method, tool |
| `mcpgw_server_start_ms` | histogram | server, **runtime** (`node`, `npx`, `uvx`, `docker`, `podman`, `http`, …) |
| `mcpgw_cold_starts` | counter | server, runtime: calls that waited for a start |

`mcpgw_server_start_ms` by `runtime` is the M3 exit's "cold-start metrics
published per runtime".

**Logs:** every `tracing` event past the filter, with its span's trace
context.

## Verified

- **Jaeger 2.11** (`crates/mcpgw/tests/jaeger.rs`, Docker): the daemon
  exported over OTLP/HTTP protobuf. Jaeger's query API returned trace
  `5ad16b3a…` with `tools/call fx__echo`, a child of the client's span
  `1122334455667788`, and `backend tools/call fx` under it.
- `crates/mcpgw/tests/otel.rs`, a fake OTLP collector (JSON protocol, every
  OS):
  - the spans' trace and parent ids, and their kinds (SERVER, CLIENT);
  - the backend's received `_meta.traceparent`, which equals the backend
    span's id;
  - the metrics `mcpgw_tool_calls`, `mcpgw_tool_call_duration_ms` and
    `mcpgw_server_start_ms`;
  - log records.
- Unit test: continuing a remote trace and propagating it to a child span,
  against an in-memory exporter.
- Manual: `mcpgw serve --stdio` with a collector received
  `mcpgw_server_start_ms{runtime="fake-mcp"}` and `mcpgw_cold_starts`.

`mcpgw down` returns when the daemon has stopped serving; its exporters
flush a moment later, as the process exits.
