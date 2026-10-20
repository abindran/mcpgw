# Servers

A server is anything mcpgw gets tools from. Each kind is a `transport`.
`mcpgw add` registers a server in the store; `[[server]]` in the config
file declares one, and the config file wins over the store for the same
name. Every option is in the [configuration reference](../reference/config.md)
and the [CLI reference](../reference/cli.md#mcpgw-add).

| Transport | Add it with | Runs as |
|---|---|---|
| `stdio` | `mcpgw add fs --command npx -- -y @modelcontextprotocol/server-filesystem ~/notes` | a child process (its own process group or Job Object) |
| `http` | `mcpgw add docs --url https://mcp.example.com/mcp` | a remote MCP server (Streamable HTTP, or a legacy `…/sse` endpoint) |
| `container` | `mcpgw add gh --image ghcr.io/github/github-mcp-server` | Docker or Podman, pinned by digest, sandboxed |
| `wasm` | `mcpgw add hello --wasm ./hello.wasm` | in-process with wasmtime ([Wasm tools](wasm.md)) |
| `openapi` | `mcpgw add pets --openapi https://petstore3.swagger.io/api/v3/openapi.json` | in-process, an HTTP API as tools ([OpenAPI APIs](openapi.md)) |
| from the Registry | `mcpgw add io.github.github/github-mcp-server` | whatever the entry offers, at a pinned version |

## Pins and drift

`mcpgw add` starts the server once and reads its tools. They are pinned:
the manifest's SHA-256 is stored.

- When a later start reports different tools, the server is
  **quarantined**. Its calls are refused until you review the change:
  `mcpgw status` shows the diff, and `mcpgw pin <server>` accepts it.
- Tool descriptions are also scanned for prompt-injection signs (hidden
  Unicode, "ignore previous instructions", exfiltration links). High-risk
  findings quarantine the server, even on the first capture. Review them
  with `mcpgw scan <server>`.

## Lifecycle

- Servers start on their first call and stop after `idle_timeout`.
  `stateful = true` servers are never stopped for idleness.
- `max_running` caps how many run at once; the least recently used idle
  server makes room.
- A crashed or unresponsive server is restarted with exponential backoff.
  After `max_failures` failed starts it is marked `failed`;
  `mcpgw restart <server>` resets it.
- `warm_pool = { min = 1 }` keeps a server started, so calls never wait for
  a cold start.
- In meta mode, `search_tools` prewarms the servers behind its top results
  (`gateway.prewarm`).
- npm and PyPI servers (`npx`, `uvx`) can be installed once into a package
  cache with `mcpgw runtimes install`. Later starts don't download anything.

## Containers

```sh
mcpgw add gh --image ghcr.io/github/github-mcp-server \
  --secret GITHUB_PERSONAL_ACCESS_TOKEN=github_token \
  --egress api.github.com:443 --memory 256MiB
```

- **Sandbox.** Each profile sets its own flags and default limits, and the
  server's `limits` override the defaults.

  | Profile | Flags | Defaults |
  |---|---|---|
  | `standard` (the default) | `--cap-drop=ALL --security-opt=no-new-privileges` | 1 GiB memory, 512 processes |
  | `strict` | standard plus a read-only root, a small `/tmp` tmpfs, and the `nobody` user | 512 MiB, 1 CPU, 128 processes |
  | `off` | none | only the server's own `limits` |

  Memory comes with no swap, so going over the limit is an OOM kill, not a
  slowdown.
- **Egress.** `--egress host:port` allows only those destinations, through
  a proxy sidecar on an internal network. `--no-network` allows none.
- **Secret files.** `--secret-file /run/secrets/sa.json=gcp_sa` mounts a
  vault secret as a read-only tmpfs file, gone when the server stops.
- **Killed and reported.** A container that dies is inspected before it is
  removed. An out-of-memory kill is reported with the limit it hit, and
  any other exit with its exit code. Containers left behind by a crashed
  daemon are swept at the next start.

## Resource limits for processes

`limits = { memory = "256MiB" }` caps a stdio server's process tree. It is
enforced by `RLIMIT_DATA` on Linux, a Job Object on Windows, and an RSS
watchdog on macOS and Linux. Going over stops that server and reports it;
the gateway and other servers carry on.
