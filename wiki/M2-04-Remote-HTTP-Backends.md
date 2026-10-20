# M2-04 — Remote HTTP backends and the legacy SSE bridge

**Story:** US1, US5 · **Estimate:** 4 days · **Depends on:** M1-09
**Done when:** a remote MCP URL works like a local server, including on/off and pinning.
**Status:** ✅ Done

## Usage

```sh
mcpgw secret set api_key
mcpgw add docs --url https://mcp.example.com/mcp --header 'x-api-key=${secret:api_key}'
mcpgw add old  --url https://legacy.example.com/sse        # HTTP+SSE (2024-11-05)
```

```toml
[[server]]
name = "docs"
transport = "http"
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${secret:docs_token}" }
```

For HTTP servers, `secrets = { Header-Name = "secret" }` bindings become
headers, the counterpart of env bindings for stdio servers.

## How it works

- **`mcpgw-proto::http`**: `BackendConn::connect_http(HttpSpec, timeout)`.
  - **Streamable HTTP** through rmcp's client transport, with our reqwest
    client and the headers as `custom_headers` (marked sensitive). The same
    lifecycle as stdio backends: 2026-07-28 via `server/discover`, falling
    back to 2025-11-25 `initialize`.
  - **Legacy SSE bridge**, written here because rmcp 3.5 has no SSE client.
    It `GET`s the event stream, waits for the `endpoint` event, and refuses
    an endpoint on another origin. Server `message` events become rmcp
    inbound messages, and outbound messages are `POST`ed to the endpoint, all
    with the configured headers. It is used for URLs ending in `/sse`, and as
    a fallback when the Streamable HTTP handshake fails for any reason other
    than auth. Handshake: plain `initialize`; 2024-11-05 servers are accepted.
    The bridge's reader and writer tasks are owned by the connection and
    aborted with it, so stopping a server closes its stream.
  - A 401/403 is reported as "`<url>` requires authentication (HTTP 401/403);
    check the server's headers or connect its OAuth login" and never falls
    back to SSE. Upstream OAuth arrives in M2-05.
- **`mcpgw-runtime`**: launches are now a `Template` (stdio or HTTP), so
  remote servers go through the **same actor**: lazy start on the first call,
  idle stop (closes the session), `max_running`, health pings, backoff,
  enable/disable, **pinning and drift quarantine**, `{account}` instances,
  and secret injection with **redaction** of header values in everything
  sent back. The old "http arrives in M2-04" disabled state is gone.
  Install-time introspection works for HTTP servers.
- **CLI**: `add --url` takes `--header NAME=VALUE` (repeatable).
  `client import` brings in remote `url` and `type: "sse"` entries, and they
  now run.

## One HTTP stack

rmcp uses reqwest 0.13; the workspace moved from 0.12 to 0.13 so there is a
single reqwest. TLS is rustls with the **ring** provider, which builds
without cmake or a C toolchain on musl and Windows, unlike aws-lc-rs. It
checks certificates against the **OS trust store**
(rustls-platform-verifier). `mcpgw_core::tls::install()` registers the
provider and is called wherever a client is built (daemon, CLI, broker,
proto, testkit, and tests that use reqwest directly).

## Tests

- `crates/mcpgw-runtime/tests/remote.rs`:
  - **Streamable HTTP like a local server**: `fake-mcp --http` requires an
    `x-api-key` header (new `FAKE_REQUIRE_HEADER`) filled from a secret;
    install-time pin (7 tools, 2026-07-28); listing without connecting; lazy
    start; idle stop and reconnect; disable (refused, tools hidden) and
    enable.
  - **Drift**: a remote server whose description changed is quarantined.
  - **Missing header**: an auth error, not an SSE retry.
  - **Legacy SSE** (`mcpgw_testkit::sse::SseFake`, a 2024-11-05 HTTP+SSE
    server): echo works at protocol 2024-11-05; the injected header reached
    the server and comes back as `[REDACTED:api_key]`; the manifest is
    captured; RPC errors pass through; stopping closes the stream; a new
    call opens a new one.
  - SSE without the header gives a clear auth error.
- `crates/mcpgw/tests/remote.rs`: `secret set` → `add --url --header
  'x-api-key=${secret:api_key}'` (pinned, 7 tools) → `list` → the store has
  no key → `serve --stdio` → `api__add` = 42.
- Units: SSE parsing across chunks (CRLF, comments, multi-line data); auth
  error detection.
