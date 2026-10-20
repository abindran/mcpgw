# Getting started

## Install

```sh
curl -fsSL https://github.com/abindran/mcpgw/releases/latest/download/install.sh | sh   # macOS and Linux
cargo install --locked --git https://github.com/abindran/mcpgw mcpgw                    # from source, Rust 1.98+
```

Each release has archives for Linux (glibc and musl), macOS (arm64 and
x86_64) and Windows. With them come a `SHA256SUMS` file, an SBOM, Sigstore
signatures (`*.sigstore.json`), GitHub build attestations, a Homebrew
formula (`mcpgw.rb`) and a container image pinned by digest. To check an
archive:

```sh
cosign verify-blob mcpgw-1.0.0-aarch64-apple-darwin.tar.gz \
  --bundle mcpgw-1.0.0-aarch64-apple-darwin.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github.com/abindran/mcpgw/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify mcpgw-1.0.0-aarch64-apple-darwin.tar.gz --repo abindran/mcpgw
```

The container serves one client on stdio:
`docker run -i --rm -v ~/.config/mcpgw:/config ghcr.io/abindran/mcpgw:1.0.0`. On Linux, the keychain is used through the Secret
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
