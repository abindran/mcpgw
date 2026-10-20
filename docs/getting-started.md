# Getting started

## Install

```sh
cargo install --locked --git https://github.com/abindran/mcpgw mcpgw
```

Rust 1.98 or later. On Linux, the keychain is used through the Secret
Service; set `MCPGW_VAULT_PASSPHRASE` where there is none (servers, CI).

## Five minutes

```sh
mcpgw secret set github_token                     # into the encrypted vault (OS keychain)
mcpgw add fs --command npx -- -y @modelcontextprotocol/server-filesystem ~/notes
mcpgw add io.github.github/github-mcp-server      # from the MCP Registry, pinned
mcpgw up                                          # the daemon: http://127.0.0.1:7777/mcp
mcpgw client install cursor                       # or claude-code, claude-desktop, vscode
mcpgw ui                                          # servers, approvals, recent calls, inspector
```

`mcpgw add` starts each server once, reads its tools and **pins** them. A
later change to a server's tools quarantines it until you look
(`mcpgw pin`).

Clients see one MCP server, mcpgw:

- By default (`mode = "meta"`) they get three tools, `search_tools`,
  `describe_tool` and `call_tool`, and search for the rest.
- `mode = "direct"` lists every tool, as `<server>__<tool>`.

Servers start on their first call and stop when idle. A bound secret is
injected at start and redacted from everything the server sends back.

## Next

- Run servers as containers, Wasm components or OpenAPI APIs:
  [Servers](guides/servers.md).
- Give each client its own view: [Profiles and policy](guides/policy.md).
- Work on your own server with hot reload: [Developing a
  server](guides/dev.md).
- Everything the CLI, config and API accept is in the reference: the
  [CLI](reference/cli.md), [configuration](reference/config.md) and [HTTP
  API](reference/api.md).
- When something is off, `mcpgw doctor` checks the config, vault, servers
  and clients, and explains each problem.
