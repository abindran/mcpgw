# M1-06 — Idle reaper and `max_running` cap

**Story:** US4 · **Estimate:** 2 days · **Depends on:** M1-05
**Done when:** with 10 servers enabled and none used, zero backend processes run; an idle server stops after `idle_timeout`.
**Status:** ✅ Done

## Behaviour

```toml
[gateway]
idle_timeout = "10m"   # default; per-server idle_timeout overrides it
max_running  = 16      # default

[[server]]
name = "browser"
stateful = true        # never stopped for idleness, never evicted
```

- **Idle reaper.** Each server's actor arms a timer while the server is
  `Ready` or `Idle`, from its last start, call start or call end. When it
  fires the server drains and stops (its process tree is killed). The next
  call starts it again.
- **Cap.** Before an actor spawns its server, it counts the other servers
  that are starting or connected. At `max_running` it stops the **least
  recently used idle** one. The cap is soft: if every running server is busy
  or stateful, the new one starts anyway with a warning, rather than
  failing the call.
- `stateful` servers are exempt from both, so browser or database sessions
  survive.
- Status gains `stateful`; `last_used` now also counts starts and call ends.

## Tests

- Runtime (`lifecycle.rs`):
  - `ten_unused_servers_run_nothing`: ten enabled servers with manifests list
    their tools, stay `Cold`, and the spawn log is never created
  - `idle_server_stops_after_idle_timeout`: after 300 ms idle the server is
    `Cold` and its process is gone; the next call restarts it
  - `per_server_idle_timeout_and_stateful_servers`
  - `max_running_evicts_least_recently_used_idle_server`: with a cap of 2,
    starting `c` evicts `a`, then starting `a` evicts `b`
  - `stateful_servers_are_never_evicted` (soft cap)
- Binary (`crates/mcpgw/tests/lazy.rs`):
  - 10 `add-server`s (10 introspection spawns, all exited), then `mcpgw
    serve`: no new spawn, no live server process, `tools/list` returns 70
    tools, and one call starts exactly one server
  - `idle_timeout = "500ms"` in the config: the server's process exits by
    itself after a call
