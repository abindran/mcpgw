# Changelog

All notable changes. The project follows [Semantic Versioning](https://semver.org/);
each config or API change comes with an upgrade note in
[docs/upgrading.md](docs/upgrading.md).

## [1.0.0] — M4: extensions and v1.0

### Added

- **Wasm tool servers** (`transport = "wasm"`, `mcpgw add --wasm`).
  - Components run in-process on wasmtime (component model, WASI 0.2,
    pooling allocator), and an instance starts in about 40 µs.
  - Grants are deny-by-default: network destinations, directories (read
    or read-write) and environment.
  - Each call has a time budget (epoch interruption) and a memory cap.
  - The `mcpgw-tool` SDK and a `cargo generate` template take a new tool
    from template to a working call in under a minute.
- **OpenAPI APIs as tools** (`transport = "openapi"`, `mcpgw add --openapi`).
  - Every operation of an OpenAPI 3.x spec (JSON or YAML, file or URL)
    becomes a schema-checked tool, served by mcpgw itself.
  - Security schemes (API key, HTTP basic and bearer, OAuth 2,
    OpenID Connect) are bound to vault secrets and OAuth logins (`auth`,
    `--auth`).
- **Cedar policies**, an optional policy engine:
  - `policy = { cedar = "file.cedar" }` on a profile or the default
    endpoint;
  - `mcpgw policy convert` writes Cedar from glob rules, with the same
    decisions;
  - `mcpgw policy check` shows what a policy decides;
  - `mcpgw doctor` reports Cedar files that don't load.
- **Long calls:**
  - backends' progress notifications reach clients;
  - cancelling a request cancels the tool at the backend;
  - tasks (SEP-2663) for clients that declare the extension, with backend
    tasks followed;
  - `gateway.call_timeout` and `gateway.max_call_time`.
- **Project servers:** a `.mcpgw.toml` at a repository's root lists
  servers for that project only.
  - `mcpgw connect` finds it from its working directory and serves the
    project's endpoint (`/mcp/project/<id>`): the project's servers with
    the global ones, the project's winning a name clash. No other endpoint
    sees them.
  - Nothing in the file runs until that exact content is trusted
    (`mcpgw project trust`, or the macOS app). Any edit turns its servers
    off until it's trusted again, and until then the endpoint lists a
    `project_not_trusted` tool saying what to do.
  - `mcpgw project init|trust|untrust|ls`, `/api/v1/projects`, a
    `mcpgw doctor` check, and `schema/project.schema.json`.
- **A macOS app** (`mcpgw.app`, SwiftUI) with the `mcpgw` binary bundled
  inside. It manages servers, tools, approvals, profiles, secrets, the
  audit log and clients, and has a menu bar item for approvals. See
  [docs/guides/macos-app.md](docs/guides/macos-app.md).
- **`mcpgw dev`:** run a server under a private gateway, restart it on
  every edit, and log each call.
- **The web UI's Inspector** calls any tool, through the new
  `POST /api/v1/tools/{name}/call`.
- **Documentation:** guides, an upgrade guide, and CLI, configuration and
  HTTP API references generated from the code (`docs/`, built as an
  mdBook).
- **Releases.** Each release has:
  - archives for five targets, with checksums, Sigstore signatures and
    build attestations;
  - an SBOM;
  - a container image pinned by digest;
  - a shell installer and a Homebrew formula.
- **Examples:** `examples/wasm-text-tools`, a Wasm tool server, and
  `examples/openapi-petstore.toml`, an OpenAPI API.
- **The upgrade from M3 builds is tested**, against a store, vault and config
  written by the M3 binary.

### Changed

- **Call timeout.** It now counts from the last sign of life (a result or
  progress), not from the start.
- **JSON key order.** Generated JSON keeps declaration order: the workspace
  enables `serde_json/preserve_order`, which cedar-policy needs.
- **YAML specs** are read with `serde-saphyr`. It replaces `serde_yaml_ng`,
  whose `unsafe-libyaml` is unmaintained.

### Fixed

- **Health checks of 2026-07-28 servers.** Their "method not found" answer
  to `ping` was taken for a dead server, so they were restarted every 30 s,
  even under a running call. Health checks now probe such servers with
  `server/discover`, and any answer counts as alive.
- **Two tests that had passed for the wrong reason.** The hanging-ping test
  never hung, and the SDK template test left its daemon running.

## Earlier milestones (pre-releases)

- **M3, isolation and performance:**
  - container servers with sandbox profiles, egress allowlists and secret
    files;
  - memory caps for processes, warm pools and predictive prewarming;
  - the npm/PyPI package cache and BM25 tool search;
  - the tool-poisoning scanner, OpenTelemetry export and `mcpgw doctor`.
- **M2, auth and control:**
  - the OAuth broker, token refresh, per-account bindings, and remote
    HTTP/SSE servers with upstream OAuth;
  - profiles, tool policy, approvals and live change propagation;
  - the admin REST API, the web UI, the hash-chained audit log and MCP
    Registry installs.
- **M1, local MVP:**
  - the loopback HTTP front door and the stdio shim;
  - the SQLite store, the stdio launcher and the lifecycle supervisor;
  - the idle reaper and `max_running`;
  - pinned manifests with drift checks, the aggregating router and
    meta-tools;
  - the vault, secret injection and redaction;
  - the CLI, and client install/import.
