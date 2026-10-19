# ADR-0004: Process model

**Status:** Accepted (19 Oct 2026) · **Work items:** M1-01, M1-02, M1-04 – M1-06

## Context

Today every client that runs `mcpgw serve` gets its own gateway process and
its own copy of every backend. The plan wants one configuration for all
clients, servers that start on first use and stop when idle, and a cap on
running servers. That needs one shared gateway per user.

## Decision

- **One daemon per user.** `mcpgw serve` runs it in the foreground; `mcpgw
  up` starts it detached, `mcpgw down` stops it, `mcpgw status` reports it.
  The daemon writes `<data dir>/mcpgw/daemon.json` (pid, URL, version,
  start time; mode 0600) while running.
- **Front doors.**
  - Streamable HTTP on `127.0.0.1:7777/mcp` (configurable `gateway.listen`,
    loopback only). It requires the loopback bearer token and checks `Host`
    and `Origin` against loopback names, which blocks DNS rebinding and
    browser pages.
  - `mcpgw connect`: a stdio shim for clients that only launch commands. It
    finds the daemon through `daemon.json`, starts it if needed, and relays
    newline-delimited JSON-RPC between stdio and the HTTP endpoint without
    interpreting it.
  - `mcpgw serve --stdio` keeps the original single-client, in-process mode
    for tests and constrained setups.
- **Backends are children of the daemon.** Each stdio server runs in its own
  process group (Unix) or Job Object (Windows), so stopping it removes the
  whole tree, including `npx` grandchildren. stderr is captured into a
  per-server ring buffer and the log.
- **One actor per server.** A task owns each server's lifecycle state machine
  (`Cold → Starting → Ready ⇄ Busy → Idle → Draining → Cold`, plus `Backoff`,
  `Failed`, `Quarantined`) and receives commands over a channel. Concurrent
  first calls queue behind a single spawn.
- **Lazy by default.** `tools/list` is answered from stored manifests; a
  server starts on its first call and stops after `idle_timeout`. A global
  `max_running` cap evicts the least recently used idle server. `stateful`
  servers are never stopped for idleness.
- **Runtime:** one multi-threaded tokio runtime.

## Consequences

- Clients share backends and state; a crash in one backend never takes down
  the daemon.
- The daemon is a single point of failure for all clients; `mcpgw connect`
  restarts it on demand.
- A stale `daemon.json` (crash) is detected by a failed health check and
  replaced.
- Running as a separate local process keeps a future cloud version
  possible: the HTTP edge is already stateless for 2026-07-28 clients.
