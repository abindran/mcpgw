# mcpgw

mcpgw is a single-binary, local **MCP gateway**. To your LLM clients
(Claude Code, Cursor, VS Code, Claude Desktop…) it is one MCP server. Behind
it are all your tool servers:

- local processes, containers and remote MCP servers;
- Wasm components;
- plain HTTP APIs described by OpenAPI.

It runs on your machine, and only listens on loopback.

- **One config.** Add a server once and every client sees it. Servers start
  on their first call and stop when idle.
- **Safe by default.**
  - Secrets live in an encrypted vault and are redacted from everything a
    server returns.
  - Tools are pinned, and changes are quarantined until you look.
  - Containers are sandboxed and Wasm tools get only what they are granted.
- **Control.**
  - Profiles give each client its own servers and view.
  - Policies (glob lists or Cedar) hide or refuse tools, and some calls
    can wait for a human.
  - Everything is audited.
- **Built for real work.**
  - BM25 tool search keeps the client's context small.
  - Warm pools and prewarming hide cold starts.
  - Long calls report progress and can be cancelled or run as tasks.
  - OAuth logins refresh themselves, and OpenTelemetry traces show each
    call end to end.

Both MCP `2026-07-28` and `2025-11-25` are supported, on both sides.

| Start here | |
|---|---|
| [Getting started](getting-started.md) | install, add servers, connect a client |
| [Upgrading to v1.0](upgrading.md) | from earlier builds; what changed |
| [Guides](guides/servers.md) | servers, secrets, policy, Wasm, OpenAPI, long calls, the dev loop, troubleshooting |
| [Reference](reference/cli.md) | every [CLI command](reference/cli.md), [config key](reference/config.md) and [API endpoint](reference/api.md), generated from the code |

The documentation builds as a book with [mdBook](https://rust-lang.github.io/mdBook/):
`mdbook build docs` (CI builds it on every push).
