# M3 exit check

**Exit criteria (plan):** cold-start metrics published per runtime;
container isolation tests pass; the search benchmark meets its target.

## Cold-start metrics per runtime: met

Every start emits `mcpgw_server_start_ms`, a histogram labelled with
`server` and `runtime`, exported over OTLP (M3-11). Calls that had to wait
count in `mcpgw_cold_starts`. Each server's `last_start_ms` and
`cold_starts` also appear in `GET /api/v1/servers` and the admin status
(M3-06). Measured on macOS arm64 (20 Oct 2026), first start from cold,
through the handshake and `tools/list`:

| Runtime | Servers | Start |
|---------|---------|-------|
| native binary | fake-mcp | 16–52 ms |
| `node` (from the M3-08 package cache) | filesystem, memory, everything, playwright, context7 | 143–472 ms |
| `docker` (colima, image cached) | node test server, github-mcp-server 2.0.2 | 281–373 ms |
| `python` (cached venv) | mcp-server-git, mcp-server-time, mcp-server-fetch | 548–917 ms |
| `npx -y`, uncached, for comparison | server-memory | 3.1 s (download), 1.25 s (npx's own cache warm) |

Warm pools (M3-06) and prewarm on search (M3-07) hide these times from
calls. In the e2e suite the cold start was hidden in 10 of 10
search-then-call sequences.

## Container isolation tests pass: met

The Linux CI jobs (glibc and musl) run with `MCPGW_TEST_CONTAINERS=1`, so
Docker tests *must* run there. They have passed on every push since M3-01:

- launcher: start on first call, removal when idle, pull by digest, sweep
  (M3-01); also with **Podman**, required on Linux CI;
- sandbox: an OOM kill at 256 MiB is reported, not the gateway; strict and
  off profiles (M3-02);
- egress: only `api.github.com:443` is reachable, SNI routing works, and
  `[]` means no network (M3-03);
- secret files in tmpfs, gone after stop (M3-04);
- Jaeger tracing (M3-11).

## Search benchmark meets target: met

On 227 real tools from 18 servers, with 124 task queries, top-5 accuracy is
**96.8%** against a target of 90%; top-1 is 81.5% (M3-09).

## Also in M3

- stdio memory caps on all three OSes (M3-05);
- a poisoning scanner at install: 15/15 known samples flagged, 0 false
  positives on the 227 real tools (M3-10);
- `mcpgw doctor`, which explains 10 of 10 seeded misconfigurations (M3-12).

**Fixes found along the way:**

- `Ready` was published before the protocol version and tool list were
  stored (a Windows-only test failure);
- background connection closes could be cut off at shutdown;
- the flaky audit tamper test, the prewarm think time and an idle-reaper
  test were made robust on CI.

## Still needs a person

- The same cold-start table on Linux and Windows desktops.
- Docker Desktop on Windows (CI's Windows runner has Windows containers
  only, so the container tests skip there).
