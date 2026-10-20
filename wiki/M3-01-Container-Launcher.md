# M3-01 — Container launcher (Docker and Podman)

**Story:** US4, US5 · **Estimate:** 4 days · **Depends on:** M1-05
**Done when:** a containerised server starts on first call and is removed when idle.
**Status:** ✅ Done (verified with Docker 27.4; Podman is tested where CI has it)

## Using it

```sh
mcpgw add notes --image ghcr.io/example/notes-mcp:1.4 --env LOG=info \
  --secret API_TOKEN=notes_token --run-arg=-v --run-arg=/data:/data:ro -- --read-only
# pulling ghcr.io/example/notes-mcp:1.4 to pin its digest
# pinned ghcr.io/example/notes-mcp:1.4 to sha256:…
# added server notes (5 tools, pin …)
mcpgw status     # notes  idle  5 tools  docker mcpgw-notes-1a2b3c4d
mcpgw stats      # server  container  cpu  memory  pids
mcpgw logs notes # the container's stderr
```

```toml
[[server]]
name = "notes"
transport = "container"
image = "ghcr.io/example/notes-mcp:1.4"
digest = "sha256:…"            # optional; `mcpgw add` fills it in
runtime = "podman"              # optional; default: docker, else podman
args = ["--read-only"]          # entrypoint arguments
env = { LOG = "info" }
run_args = ["-v", "/data:/data:ro"]
secrets = { API_TOKEN = "notes_token" }
```

`mcpgw add io.github.github/github-mcp-server` (M2-14) now installs OCI
entries this way as well.

## How it runs (`mcpgw-runtime::container`)

- **stdio over attach:** each start runs `<runtime> run -i --name
  mcpgw-<server>-<8 hex> --label mcpgw.managed=1 --label
  mcpgw.server=<server> --label mcpgw.pid=<pid> [--label
  mcpgw.instance=<id>] -e NAME… <sandbox flags> <run_args>
  <image>[@digest] <args>` as the stdio child. The sandbox flags were added
  in [M3-02](M3-02-Sandbox-Profiles.md). Handshake, stderr capture, health pings, crash detection,
  idle stop, LRU eviction and drift quarantine are the same as for a local
  process.
- **Pull by digest:** `mcpgw add` pulls the image and pins its registry
  digest (`RepoDigests`), so `repo:tag@sha256:…` always runs the same bits.
  Before each start the launcher checks the image is present and pulls it if
  not, *before* the handshake clock starts (15 min limit, progress in the
  server's log). A stopped daemon is reported as "docker is not running".
- **Env and secrets:** values are put in the runtime CLI's environment and
  passed with `-e NAME`, never on a command line. Injected secrets are
  redacted from results as usual. Secret placeholders are refused in
  `run_args`. `-d`, `--detach`, `--name`, `-t` and `--tty` are refused
  there too, since mcpgw manages them.
- **Removal:** containers run without `--rm`, and mcpgw always removes
  them itself, so a dead one can be inspected first (M3-02). When a server
  stops (idle, eviction, disable, shutdown) the CLI is closed, then killed,
  then `rm -f <name>` runs. Each server's actor waits for its removals
  before it exits, so `mcpgw down` leaves nothing behind. A
  failed handshake removes its container before any retry starts. A
  dropped connection removes it in the background. Each start gets a fresh
  name, so a slow removal can never hit a newer container.
- **Leftovers:** the daemon labels its containers `mcpgw.instance=<hash of
  its store path>`. On start it removes containers with its instance label
  that another process started (a crashed daemon). Containers from other
  stores or from `mcpgw add` introspection are left alone.
- **Status and stats:** `BackendStatus.container` = {runtime, name, image}
  appears in `/admin/status`, `GET /api/v1/servers` (`container`) and
  `mcpgw status`. `mcpgw stats` asks `docker stats` / `podman stats` for
  CPU, memory and process counts.

## Tests

The test server is a small MCP server in Node (`testkit::container::SERVER_JS`,
with `echo` and `env` tools) running in the stock `node:22-alpine` image.
Container tests skip without Docker for Linux containers (macOS and Windows
CI), and **must** run where `MCPGW_TEST_CONTAINERS=1` (the Linux CI jobs).

- `crates/mcpgw-runtime/tests/container.rs`:
  - **Done-when:** nothing runs before the first call. The call starts one
    labelled container. Env reaches it, and the injected secret reaches it
    too (redacted on the way back). Its stderr is captured. When idle it is
    stopped and the container is removed. The next call starts a fresh
    container, and shutdown removes it.
  - The digest resolves to `sha256:…` and the container runs
    `node:22-alpine@sha256:…`. A container that exits before the handshake
    leaves nothing behind. The sweep removes only this instance's orphan,
    not another instance's container.
  - The same flow with Podman, where Podman is installed.
- `crates/mcpgw/tests/container.rs` (end to end): `add --image` pins the
  digest, captures 2 tools and leaves no introspection container. `list`
  shows `container … node:22-alpine@sha256:`. `--run-arg=-d` is refused.
  Through the daemon the first call starts it; `status`, `stats` and `logs`
  show it. It's removed when idle, and a running one is removed by `mcpgw
  down`.
- Unit tests: the `run` invocation (labels, `-e NAME` without values,
  cleanup command), repository parsing, and validating container specs.

Verified locally on macOS arm64 against Docker 27.4 (colima). The real GitHub
registry entry installed as a container server pinned to
`sha256:ffced0d7…`, with 46 tools.
