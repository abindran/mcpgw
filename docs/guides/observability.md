# Observability and troubleshooting

## When something is wrong: `mcpgw doctor`

`mcpgw doctor` checks each of these and explains what it finds, with a fix:

- the config file, the store and the token file's permissions;
- the vault, and secrets that bindings name but that aren't set;
- OAuth logins that are missing or expired;
- servers: commands on `PATH`, uncached npm and PyPI packages, the
  container runtime, and failing, quarantined or disabled servers;
- profiles that name unknown servers;
- the daemon and its port (and a stale daemon file);
- client configs that don't use mcpgw, or have a broken entry;
- Cedar policy files;
- the OTLP endpoint.

`--json` prints the checks for scripts. The exit status is non-zero when
something fails.

## Status and logs

```sh
mcpgw status            # daemon and per-server state, restarts, last error, cold starts
mcpgw stats             # CPU, memory and process counts of running container servers
mcpgw logs              # the daemon's log
mcpgw logs gh           # a server's recent stderr
mcpgw scan gh           # the tool-poisoning scan of a server's tools
```

Logs go to stderr. `MCPGW_LOG` sets the level (default `warn`), and
`--log-format pretty|compact|json` sets the format.

## Audit

Changes and tool calls go into a hash-chained, append-only audit log.

- **Recorded:** server, profile and secret changes (CLI and API), approval
  decisions, logins, and every tool call.
- **Tool calls** are recorded with their outcome, duration and a SHA-256 of
  their arguments. Arguments, results and secret values are never stored.

```sh
mcpgw audit verify      # "audit log OK: N entries, chain intact", or the entry where it breaks
mcpgw audit tail -n 20
mcpgw audit export --after 100   # JSON lines
```

## OpenTelemetry

```toml
[gateway]
otlp_endpoint = "http://127.0.0.1:4318"   # OTLP over HTTP; OTEL_EXPORTER_OTLP_ENDPOINT wins
```

The daemon exports traces, metrics and logs.

- **One trace per call**, from the client's `traceparent` (in `_meta` or
  the HTTP header) through the gateway to the backend, which gets its own
  `traceparent` in `_meta`.
- **Metrics:** tool calls and their durations, cold starts, and start
  times per runtime.
- **Filtering.** `MCPGW_OTEL_FILTER` (default `info`) filters what is
  exported, separately from the stderr log.
