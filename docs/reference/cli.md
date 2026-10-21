# CLI reference

<!-- Generated from `mcpgw __reference cli` by `cargo xtask schema`; don't edit. -->

Global options go before the command: `mcpgw --config ./config.toml list`.

## `mcpgw`

MCP gateway: many MCP servers behind one

```
mcpgw [OPTIONS] <COMMAND>
```

| Argument | Description |
|---|---|
| `--config <CONFIG>` | Config file (default: $MCPGW_CONFIG or ~/.config/mcpgw/config.toml). |
| `--log-format <FORMAT>` | Log format on stderr: pretty, compact or json (default: $MCPGW_LOG_FORMAT or compact). The level comes from $MCPGW_LOG (default: warn). |

### `mcpgw serve`

Run the gateway daemon in the foreground (HTTP on gateway.listen)

```
mcpgw serve [OPTIONS]
```

| Argument | Description |
|---|---|
| `--stdio` | Serve one client on stdin/stdout instead of HTTP. |
| `--profile <PROFILE>` | With --stdio: serve this profile's view. |

### `mcpgw up`

Start the daemon in the background

```
mcpgw up [OPTIONS]
```

### `mcpgw down`

Stop the running daemon

```
mcpgw down [OPTIONS]
```

### `mcpgw status`

Show the daemon and the state of every server

```
mcpgw status [OPTIONS]
```

### `mcpgw stats`

CPU, memory and process counts of running container servers

```
mcpgw stats [OPTIONS]
```

### `mcpgw doctor`

Find and explain misconfigurations: config, vault, secrets, commands, Docker, ports, the daemon, logins, profiles and client configs. Fails if anything is broken

```
mcpgw doctor [OPTIONS]
```

| Argument | Description |
|---|---|
| `--json` | Print the checks as JSON. |

### `mcpgw scan`

Check servers' tools for poisoning signs (hidden characters, injection phrases, instructions about other tools)

```
mcpgw scan [OPTIONS] [SERVER]
```

| Argument | Description |
|---|---|
| `<SERVER>` | Only this server (read live if it has no pinned tools). |

### `mcpgw connect`

Relay stdin/stdout to the daemon, starting it if needed. Use this as the command in clients that only launch stdio servers

```
mcpgw connect [OPTIONS]
```

| Argument | Description |
|---|---|
| `--profile <PROFILE>` | Use this profile's endpoint (/mcp/p/<name>). |
| `--project <PROJECT>` | Serve the project (`.mcpgw.toml`) at or above this directory. Default: the one the working directory is in, if any. |
| `--no-project` | Ignore any project and serve the default endpoint. |

### `mcpgw profiles`

List profiles and their endpoints

```
mcpgw profiles [OPTIONS]
```

### `mcpgw add`

Register or update a server, pin its tools, and tell the daemon. With a registry name (`mcpgw add io.github.github/github-mcp-server`) it installs that entry from the MCP Registry at a pinned version, shows its tools and asks before adding

```
mcpgw add [OPTIONS] <NAME> [-- <ARGS>...]
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name ([A-Za-z0-9_-], no "__"), or an MCP Registry entry (`io.github.github/github-mcp-server`, optionally `@<version>`). Required. |
| `--command <COMMAND>` | Stdio command to launch. |
| `--image <IMAGE>` | Container image to run with Docker or Podman (MCP over its stdio); its digest is pinned when added. |
| `--wasm <FILE>` | A Wasm component exporting the `mcpgw:tool` world, run in-process with wasmtime. |
| `--openapi <SPEC>` | An OpenAPI 3.x spec (file or URL, JSON or YAML): its operations become tools, served by mcpgw itself. |
| `--base-url <URL>` | For --openapi: where requests go (default: the spec's server URL). |
| `--auth <SCHEME=SECRET>` | For --openapi: bind a vault secret to one of the spec's security schemes, SCHEME=SECRET (repeatable): `--auth api_key=petstore_key`, `--auth oauth=oauth/github@{account}`. Repeatable. |
| `--auth-username <SCHEME=USER>` | For --openapi: the user name for an HTTP basic scheme, SCHEME=USER. Repeatable. |
| `--allow-network <HOST:PORT>` | Let a Wasm server connect to HOST:PORT (repeatable; `*` for any). Default: no network. Repeatable. |
| `--allow-dir <DIR>` | Let a Wasm server read this directory (repeatable). Repeatable. |
| `--allow-dir-rw <DIR>` | Let a Wasm server read and write this directory (repeatable). Repeatable. |
| `--time-budget <DURATION>` | Stop a Wasm server's call after this long (e.g. 5s; default 30s). |
| `--runtime <RUNTIME>` | Container runtime for --image: docker or podman (default: whichever is installed, Docker first). |
| `--run-arg <RUN_ARGS>` | Extra `run` flag for --image (repeatable), e.g. `--run-arg=-p --run-arg=127.0.0.1:8085:8085`. Repeatable. |
| `--sandbox <SANDBOX>` | Sandbox for --image: standard (default), strict or off. |
| `--egress <HOST:PORT>` | Let the container reach only this host:port (repeatable), e.g. `--egress api.github.com:443`. Repeatable. |
| `--secret-file <PATH=SECRET>` | Mount a vault secret as a read-only tmpfs file in the container, PATH=SECRET (repeatable), e.g. `--secret-file /run/secrets/sa.json=gcp_sa`. Repeatable. |
| `--no-network` | Give the container no network at all. |
| `--memory <MEMORY>` | Memory cap for the server, e.g. 256MiB (stdio or container servers). |
| `--cpus <CPUS>` | CPU limit in cores, e.g. 0.5 (container servers). |
| `--pids <PIDS>` | Process limit (container servers). |
| `--url <URL>` | Remote MCP URL: Streamable HTTP, or a legacy `…/sse` endpoint. |
| `--header <HEADERS>` | HTTP header for a remote server, NAME=VALUE (repeatable). Values may use `${secret:<name>}`, e.g. `Authorization=Bearer ${secret:api_key}`. Repeatable. |
| `--env <ENV>` | Environment variable for the command, KEY=VALUE (repeatable). Repeatable. |
| `--secret <SECRETS>` | Inject a vault secret as an environment variable, VAR=SECRET (repeatable). Repeatable. |
| `--alias <ALIAS>` | Shorter prefix for its tools (`<alias>__<tool>`). |
| `--stateful` | Keep its processes running even when idle (browser or database sessions). |
| `--warm` | Keep it warm: start it with the gateway and restart it after a crash, so calls never wait for a cold start (`warm_pool = { min = 1 }`). |
| `--disabled` | Register but do not start it. |
| `--no-introspect` | Don't start the server now to read and pin its tools; they are captured on its first start instead. |
| `--no-cache` | Don't install an npx/uvx server's package into the cache; it then starts through npx/uvx each time. |
| `--accept-risk` | Add it even if its tool descriptions look poisoned (hidden characters, injection phrases); see `mcpgw scan`. |
| `--as <NAME>` | Registry installs: the server name to use (default: the entry's last path segment). |
| `--remote` | Registry installs: use the entry's hosted remote, not a package. |
| `--package <TYPE>` | Registry installs: use this package type (npm, pypi or oci). |
| `-y, --yes` | Registry installs: add without asking after showing the tools. |
| `<ARGS>` | Arguments for the command or the image's entrypoint (after `--`). Repeatable. |

### `mcpgw remove`

Remove a server

```
mcpgw remove [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name. Required. |

### `mcpgw enable`

Enable a server

```
mcpgw enable [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name. Required. |

### `mcpgw disable`

Disable a server: it stops and its tools are hidden

```
mcpgw disable [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name. Required. |

### `mcpgw list`

List configured servers (config file + store)

```
mcpgw list [OPTIONS]
```

| Argument | Description |
|---|---|
| `--check` | Also start each server and report its status and tool count. |

### `mcpgw logs`

Show a server's recent stderr, or the daemon's log

```
mcpgw logs [OPTIONS] [NAME]
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name (omit for the daemon log). |
| `-n, --lines <LINES>` | Number of lines. Default: `50`. |

### `mcpgw pin`

Trust a quarantined server's changed tools as its new pin

```
mcpgw pin [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name. Required. |

### `mcpgw restart`

Clear a failed server; it starts fresh on its next call

```
mcpgw restart [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Server name. Required. |

### `mcpgw ui`

Open the web UI in a browser (starts the daemon if needed)

```
mcpgw ui [OPTIONS]
```

| Argument | Description |
|---|---|
| `--print` | Print the address instead of opening a browser. |

### `mcpgw approvals`

List tool calls waiting for approval

```
mcpgw approvals [OPTIONS]
```

### `mcpgw approve`

Approve a waiting tool call (id from `mcpgw approvals`)

```
mcpgw approve [OPTIONS] <ID>
```

| Argument | Description |
|---|---|
| `<ID>` | Approval id. Required. |

### `mcpgw deny`

Refuse a waiting tool call

```
mcpgw deny [OPTIONS] <ID>
```

| Argument | Description |
|---|---|
| `<ID>` | Approval id. Required. |

### `mcpgw audit`

Inspect the hash-chained audit log

```
mcpgw audit [OPTIONS] <COMMAND>
```

#### `mcpgw audit verify`

Check that no entry was changed, removed or reordered

```
mcpgw audit verify [OPTIONS]
```

#### `mcpgw audit export`

Write entries as JSON lines (to stdout)

```
mcpgw audit export [OPTIONS]
```

| Argument | Description |
|---|---|
| `--after <AFTER>` | Only entries after this sequence number. Default: `0`. |

#### `mcpgw audit tail`

Show the latest entries

```
mcpgw audit tail [OPTIONS]
```

| Argument | Description |
|---|---|
| `-n, --lines <LINES>` | Number of entries. Default: `20`. |

### `mcpgw registry`

Search the MCP Registry (gateway.registry_url)

```
mcpgw registry [OPTIONS] <COMMAND>
```

#### `mcpgw registry search`

Find servers by name or description

```
mcpgw registry search [OPTIONS] <QUERY>...
```

| Argument | Description |
|---|---|
| `<QUERY>` | Search text. Repeatable. Required. |
| `--limit <LIMIT>` | Most results. Default: `20`. |

#### `mcpgw registry show`

Show an entry's packages, remotes and inputs

```
mcpgw registry show [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Entry name, optionally `@<version>`. Required. |

### `mcpgw dev`

Work on a server: run it under a private gateway, restart it on every edit, log each call, and open the inspector

```
mcpgw dev [OPTIONS] [PATH] [-- <COMMAND>...]
```

| Argument | Description |
|---|---|
| `<PATH>` | The server: a directory (package.json, server.py, main.py or Cargo.toml), a `.wasm` component, or an OpenAPI spec. |
| `--name <NAME>` | Server name (default: from the path). |
| `--port <PORT>` | Port for the session's gateway (default: any free port). Default: `0`. |
| `--watch <PATH>` | Also restart when this file or directory changes (repeatable). Repeatable. |
| `--env <ENV>` | Environment variable for the server, KEY=VALUE (repeatable). Repeatable. |
| `--base-url <URL>` | For an OpenAPI spec: where requests go. |
| `<COMMAND>` | How to run the server, when it isn't detected (`mcpgw dev ./srv -- deno run main.ts`). Repeatable. |

### `mcpgw policy`

Tool policies: write Cedar from glob rules, check decisions

```
mcpgw policy [OPTIONS] <COMMAND>
```

#### `mcpgw policy convert`

Print an endpoint's allow/deny/approve rules as Cedar policies that give the same decisions

```
mcpgw policy convert [OPTIONS]
```

| Argument | Description |
|---|---|
| `--profile <PROFILE>` | The profile (default: the default endpoint). |
| `-o, --output <OUTPUT>` | Write to this file instead of stdout. |

#### `mcpgw policy check`

Load an endpoint's policy (Cedar files are validated) and show what it decides for each tool given

```
mcpgw policy check [OPTIONS] [TOOLS]...
```

| Argument | Description |
|---|---|
| `--profile <PROFILE>` | The profile (default: the default endpoint). |
| `<TOOLS>` | Public tool names (`<server>__<tool>`). Repeatable. |

### `mcpgw runtimes`

The package cache for npm and PyPI servers

```
mcpgw runtimes [OPTIONS] <COMMAND>
```

#### `mcpgw runtimes install`

Install (again) the packages of npx/uvx servers, so they start without npx/uvx and without the network

```
mcpgw runtimes install [OPTIONS] [SERVER]
```

| Argument | Description |
|---|---|
| `<SERVER>` | Only this server. |

#### `mcpgw runtimes ls`

List cached packages

```
mcpgw runtimes ls [OPTIONS]
```

### `mcpgw project`

Project servers: a `.mcpgw.toml` in a repository lists servers for clients started there

```
mcpgw project [OPTIONS] <COMMAND>
```

#### `mcpgw project init`

Write a `.mcpgw.toml` here, and with --client the client's project-level entry for mcpgw

```
mcpgw project init [OPTIONS] [DIR]
```

| Argument | Description |
|---|---|
| `<DIR>` | Directory (default: the working directory). |
| `--name <NAME>` | Project name (default: the directory's). |
| `--client <CLIENTS>` | Also add mcpgw to this client's project config: claude-code (.mcp.json), cursor (.cursor/mcp.json) or vscode (.vscode/mcp.json). Repeatable. Repeatable. |

#### `mcpgw project trust`

Show what the project's `.mcpgw.toml` runs and trust that content: its servers start. Any later change needs trusting again

```
mcpgw project trust [OPTIONS] [DIR]
```

| Argument | Description |
|---|---|
| `<DIR>` | A directory in the project (default: the working directory). |
| `-y, --yes` | Don't ask. |

#### `mcpgw project untrust`

Stop trusting the project: its servers stop

```
mcpgw project untrust [OPTIONS] [DIR]
```

| Argument | Description |
|---|---|
| `<DIR>` | A directory in the project (default: the working directory). |

#### `mcpgw project ls`

Projects: trusted ones, and those the daemon has seen

```
mcpgw project ls [OPTIONS]
```

### `mcpgw secret`

Manage secrets in the vault

```
mcpgw secret [OPTIONS] <COMMAND>
```

#### `mcpgw secret set`

Store a secret; the value is read hidden from the terminal, or from stdin

```
mcpgw secret set [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Secret name, e.g. github_token. Required. |
| `--description <DESCRIPTION>` | A note shown by `secret ls`. |

#### `mcpgw secret ls`

List secret names (never values)

```
mcpgw secret ls [OPTIONS]
```

#### `mcpgw secret rm`

Delete a secret

```
mcpgw secret rm [OPTIONS] <NAME>
```

| Argument | Description |
|---|---|
| `<NAME>` | Secret name. Required. |

### `mcpgw auth`

Connect accounts at OAuth apps (`[[oauth]]` in the config)

```
mcpgw auth [OPTIONS] <COMMAND>
```

#### `mcpgw auth connect`

Sign in to an OAuth app in the browser; tokens go to the vault

```
mcpgw auth connect [OPTIONS] <PROVIDER>
```

| Argument | Description |
|---|---|
| `<PROVIDER>` | The `[[oauth]]` app name, e.g. github. Required. |
| `--account <ACCOUNT>` | Account label, for several accounts at one provider (github@work). Default: `default`. |
| `--no-browser` | Print the sign-in URL instead of opening a browser. |

#### `mcpgw auth ls`

List connected accounts

```
mcpgw auth ls [OPTIONS]
```

#### `mcpgw auth rm`

Forget a connected account

```
mcpgw auth rm [OPTIONS] <PROVIDER>
```

| Argument | Description |
|---|---|
| `<PROVIDER>` | Provider. Required. |
| `--account <ACCOUNT>` | Account label. Default: `default`. |

### `mcpgw client`

Connect MCP clients (Claude Code, Claude Desktop, Cursor, VS Code)

```
mcpgw client [OPTIONS] <COMMAND>
```

#### `mcpgw client install`

Add the mcpgw entry to a client's config, keeping its other servers

```
mcpgw client install [OPTIONS] <CLIENT>
```

| Argument | Description |
|---|---|
| `<CLIENT>` | claude-code, claude-desktop, cursor or vscode. Required. |
| `--path <PATH>` | Config file to edit (default: the client's user config). |
| `--dry-run` | Print the result instead of writing it. |
| `--profile <PROFILE>` | Point the entry at this profile. |

#### `mcpgw client import`

Move a client's servers into mcpgw and replace them with one mcpgw entry. Credentials in their env move to the vault. The original file is backed up

```
mcpgw client import [OPTIONS] <CLIENT>
```

| Argument | Description |
|---|---|
| `<CLIENT>` | claude-code, claude-desktop, cursor or vscode. Required. |
| `--path <PATH>` | Config file to read and rewrite (default: the client's user config). |
| `--dry-run` | Show what would be imported without changing anything. |
| `--keep-env` | Leave credential-looking env values in place instead of moving them to the vault. |

### `mcpgw token`

Print the loopback token (creating it if needed)

```
mcpgw token [OPTIONS]
```

| Argument | Description |
|---|---|
| `--rotate` | Replace the token with a new one. |
