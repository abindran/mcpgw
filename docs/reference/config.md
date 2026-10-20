# Configuration reference

<!-- Generated from `schema/config.schema.json` by `cargo xtask schema`; don't edit. -->

The config file is TOML: `~/.config/mcpgw/config.toml`, `$MCPGW_CONFIG` or `--config`.
Editors can validate it with the JSON Schema in `schema/config.schema.json`.

### Top level

Root configuration.

| Key | Type | Description |
|---|---|---|
| `gateway` | GatewayConfig | Gateway-wide behaviour. Default: `{"allow_agent_add_servers":false,"store_path":null,"token_path":null,"mode":"meta"}`. |
| `server` | array of ServerSpec | Backend tool servers. |
| `oauth` | array of OAuthProvider | OAuth apps for `mcpgw auth connect`. |
| `profile` | array of Profile | Profiles: named endpoints at `/mcp/p/<name>` (M2-07). |

### `GatewayConfig`

Gateway-wide settings.

| Key | Type | Description |
|---|---|---|
| `allow_agent_add_servers` | boolean | Whether the agent may add new backend servers itself. Default: `false`. |
| `store_path` | string or null | Path to the SQLite store. Default: `<data dir>/mcpgw/mcpgw.db`. |
| `token_path` | string or null | Path of the static loopback auth token file. Default: `<data dir>/mcpgw/token`. |
| `vault_path` | string or null | Path of the secret vault. Default: `vault.json` next to the store. |
| `client_metadata_url` | string or null | HTTPS URL of a client ID metadata document describing mcpgw, used as the OAuth client id for remote MCP servers that accept them (otherwise mcpgw registers itself dynamically). |
| `listen` | string or null | Address of the HTTP front door; must be a loopback address. Default: `127.0.0.1:7777`. |
| `mode` | Mode | How tools are presented to clients: `meta` (default: only `search_tools`, `describe_tool`, `call_tool`), `direct` (every tool) or `hybrid` (meta-tools plus `pinned_tools`). Default: `"meta"`. |
| `accounts` | map of string | Account per server on the default endpoint, for servers whose secrets use `{account}`: `accounts = { github = "work" }`. Profiles (M2-07) set their own. Default: `default`. |
| `policy` | Policy | Tool policy for the default endpoint (profiles have their own): `policy = { deny = ["*__delete_*"], approve = ["gh__merge_*"] }`. |
| `approval_timeout` | Duration or null | How long a call to an `approve`-listed tool waits for `mcpgw approve` when the client can't be asked. Default: `"2m"`. |
| `pinned_tools` | array of string | Public tool names always listed in `hybrid` mode. |
| `oauth_refresh_interval` | Duration or null | How often the daemon checks for OAuth tokens to refresh. Default: `"60s"`. |
| `idle_timeout` | Duration or null | Stop a server after this long without calls (per-server `idle_timeout` overrides it). Default: `"10m"`. |
| `call_timeout` | Duration or null | How long a tool call may go without a sign of life (its result or a progress notification) before it fails. Default: `"2m"`. |
| `max_call_time` | Duration or null | The longest a tool call may run, progress or not. Default: `"1h"`. |
| `max_running` | integer or null | Most servers running at once; starting one more stops the least recently used idle server. Default: 16. |
| `allowed_origins` | array of string | Extra browser origins allowed to call the daemon, e.g. `"http://localhost:5173"`. The daemon's own origin is always allowed. |
| `prewarm` | boolean or null | Start servers in the background when `search_tools` returns their tools, hiding their cold start from the call that usually follows. Default: `true`. |
| `otlp_endpoint` | string or null | OTLP/HTTP endpoint for traces, metrics and logs of the daemon, e.g. `http://127.0.0.1:4318` (M3-11). `OTEL_EXPORTER_OTLP_ENDPOINT` overrides it. Default: no export. |
| `runtimes_path` | string or null | Package cache for npm and PyPI servers (M3-08). Default: `runtimes` next to the store. |
| `registry_url` | string or null | MCP Registry used by `mcpgw add <name>` and `mcpgw registry search`. Default: `https://registry.modelcontextprotocol.io`. |

### `Policy`

Tool policy: glob lists matched against public tool names (`<server>__<tool>`). `*` matches any run of characters, `?` one.  Precedence: `deny` beats `approve`, which beats `allow`. When `allow` is empty every tool not denied is allowed.  `cedar` names a Cedar policy file instead (M4-07); the lists must then be empty.

| Key | Type | Description |
|---|---|---|
| `allow` | array of string | Tools that may be called. Empty means all. |
| `deny` | array of string | Tools that are never called. |
| `approve` | array of string | Tools that need a human approval per call. |
| `cedar` | string or null | A Cedar policy file (M4-07) in place of the lists; relative to the config file. `mcpgw policy convert` writes one from the lists. |

### `ServerSpec`

One backend tool server (`[[server]]` in the config file).

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique server name. |
| `alias` | string or null | Shorter prefix for its tools' public names (`<alias>__<tool>`). Default: the name. |
| `enabled` | boolean | Whether the gateway may start this server. Default: `true`. |
| `idle_timeout` | Duration or null | Stop the server after this long without calls. Default: gateway-wide. |
| `stateful` | boolean | The server keeps user state (browser, database session): it is never stopped for idleness or evicted. |
| `warm_pool` | WarmPool or null | Keep this server started: `{ min = 1 }` starts it with the gateway, never stops it for idleness or evicts it, and restarts it after a crash, so calls never wait for a cold start. |
| `startup_timeout` | Duration or null | Deadline for start-up and the MCP handshake. Default: gateway-wide. |
| `secrets` | map of string | Secrets injected at spawn: environment variable → vault secret name, e.g. `{ GITHUB_TOKEN = "github_token" }`. Args and env values may also contain `${secret:<name>}`. Injected values are redacted from everything the server sends back. |
| `files` | map of string | Secrets written to files (container servers): container path → vault secret, e.g. `{ "/run/secrets/sa.json" = "gcp_sa" }`. Each is a read-only file in tmpfs, gone when the server stops. |
| `limits` | Limits | Resource limits: `{ memory = "256MiB", cpus = 0.5, pids = 64 }`. Container servers take all three (unset ones come from the sandbox profile); stdio servers take `memory`, a cap on the process tree. |
| `registry` | RegistryRef or null | Where it was installed from, when `mcpgw add` took it from the MCP Registry. |

### `WarmPool`

How many instances of a server to keep started.

| Key | Type | Description |
|---|---|---|
| `min` | integer | Instances kept warm: 0 (start on demand) or 1. Calls share one connection per server, so larger pools aren't supported. |

### `Limits`

Resource limits for a server.

| Key | Type | Description |
|---|---|---|
| `memory` | ByteSize or null | Memory cap; exceeding it kills the server (no swap). |
| `cpus` | number or null | CPU cap in cores, e.g. `0.5`. |
| `pids` | integer or null | Most processes (and threads) at once. |

### `RegistryRef`

A pinned MCP Registry entry.

| Key | Type | Description |
|---|---|---|
| `name` | string | Registry name, e.g. `io.github.github/github-mcp-server`. |
| `version` | string | Installed version. |

### `ServerSpec`: `transport = "stdio"`

Spawn a local child process speaking MCP over stdio.

| Key | Type | Description |
|---|---|---|
| `transport` | `"stdio"` |  |
| `command` | string | Executable to run. |
| `args` | array of string | Arguments. |
| `env` | map of string | Extra environment variables for the child. |
| `cwd` | string or null | Working directory. Default: the gateway's. |

### `ServerSpec`: `transport = "http"`

Connect to a remote Streamable HTTP endpoint.

| Key | Type | Description |
|---|---|---|
| `transport` | `"http"` |  |
| `url` | string | Endpoint URL. |
| `headers` | map of string | Extra request headers. |

### `ServerSpec`: `transport = "container"`

Run an OCI image with Docker or Podman, speaking MCP over the container's stdin/stdout (`run -i`, attached). The container is removed when the server stops.

| Key | Type | Description |
|---|---|---|
| `transport` | `"container"` |  |
| `image` | string | Image, e.g. `ghcr.io/github/github-mcp-server:2.0.2`. |
| `digest` | string or null | Pinned content digest (`sha256:…`): the container always runs exactly this image, pulled by digest. `mcpgw add` fills it in. |
| `runtime` | string or null | `docker` or `podman`. Default: whichever is installed, Docker first. |
| `args` | array of string | Arguments for the image's entrypoint. |
| `env` | map of string | Environment variables set inside the container (values are passed through the runtime's environment, never on its command line). |
| `run_args` | array of string | Extra `run` flags, e.g. `["-p", "127.0.0.1:8085:8085"]`. They come after the sandbox's flags, so they can add to them (`--cap-add=…`). |
| `sandbox` | Sandbox | Confinement: `standard` (default), `strict` or `off`. |
| `egress` | array or null | Network allowlist, `["api.github.com:443"]`: the container can reach these host:port pairs and nothing else. `[]` means no network at all. Unset: unrestricted. |

### `ServerSpec`: `transport = "wasm"`

A Wasm component exporting the `mcpgw:tool` world, run in-process with wasmtime (M4-01).

| Key | Type | Description |
|---|---|---|
| `transport` | `"wasm"` |  |
| `path` | string | The component file (`.wasm`). |
| `allow_network` | array of string | TCP destinations it may connect to, `["api.github.com:443"]`, or `["*"]` for any. Default: no network at all (M4-02). |
| `allow_dirs` | array of DirAccess | Host directories it may use. Default: none. |
| `env` | map of string | Environment variables it sees (secrets bind through `secrets`). Default: none; the host's environment never leaks in. |
| `time_budget` | Duration or null | Longest one call may run before it's stopped (M4-03). Default: 30s. The memory cap is `limits.memory` (default and maximum 256MiB). |

### `DirAccess`

A host directory granted to a Wasm server.

| Key | Type | Description |
|---|---|---|
| `path` | string | The directory on the host. |
| `guest` | string or null | Where the component sees it. Default: the same path. |
| `write` | boolean | May create and change files. Default: read-only. |

### `ServerSpec`: `transport = "openapi"`

An HTTP API described by an OpenAPI 3.x spec, served as tools by mcpgw itself, no MCP server needed (M4-05).

| Key | Type | Description |
|---|---|---|
| `transport` | `"openapi"` |  |
| `spec` | string | The spec: a file path or an http(s) URL (JSON or YAML). |
| `base_url` | string or null | Where requests go. Default: the spec's first server URL (relative ones resolve against the spec's URL). |
| `headers` | map of string | Headers on every request; values may use `${secret:<name>}`. |
| `auth` | map of AuthBinding | Credentials for the spec's security schemes, by scheme name (M4-06): `{ api_key = { secret = "petstore_key" } }`. OAuth logins use their vault name, `oauth/<provider>@{account}`. |

### `AuthBinding`

A vault secret bound to an OpenAPI security scheme.

| Key | Type | Description |
|---|---|---|
| `secret` | string | The secret: API key, token, password, or `oauth/<provider>@<account>`. |
| `username` | string or null | For HTTP basic: the user name (the secret is the password). |

### `OAuthProvider`

One OAuth app (`[[oauth]]` in the config file).

| Key | Type | Description |
|---|---|---|
| `name` | string | Name used in `mcpgw auth connect <name>` and secret names. |
| `preset` | string or null | Built-in endpoints to start from: `github`, `google` or `atlassian`. |
| `authorize_url` | string or null | Authorization endpoint (overrides the preset). |
| `token_url` | string or null | Token endpoint (overrides the preset). |
| `client_id` | string | Your OAuth app's client id. |
| `client_secret` | string or null | Vault secret holding the client secret, for confidential apps. |
| `scopes` | array of string | Scopes to request (the preset's when empty). |
| `extra_params` | map of string | Extra authorization parameters (`audience`, `prompt`, ...). |

### `Profile`

A named profile (`[[profile]]` in the config file).

| Key | Type | Description |
|---|---|---|
| `name` | string | Profile name (URL segment). |
| `servers` | array of string | Servers in this profile. Empty means every enabled server. |
| `mode` | Mode | Tool presentation. Default: `meta`. |
| `pinned_tools` | array of string | Public tool names always listed directly in `hybrid` mode. |
| `accounts` | map of string | Account bindings: server name → account label (`github` → `work`). |
| `policy` | Policy | Tool policy. Default: `{}`. |
