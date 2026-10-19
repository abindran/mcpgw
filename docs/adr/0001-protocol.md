# ADR-0001: MCP protocol handling

**Status:** Accepted (19 Oct 2026) · **Work items:** M0-04, M0-05

## Context

mcpgw is an MCP server to LLM clients and an MCP client to backend servers.
MCP 2026-07-28 replaces `initialize` with `server/discover` plus per-request
metadata and drops sessions on Streamable HTTP (SEP-2567). Most clients and
servers in the field still speak 2025-11-25. Protocol revisions and SDK
majors are the plan's largest schedule risk.

The M0-04 and M0-05 spikes used `rmcp` 3.5.1 on both sides:

| Finding | Consequence |
|---------|-------------|
| rmcp serves both lifecycles (`server/discover` and `initialize`) from one `ServerHandler` | One front implementation covers new and old clients |
| `ClientLifecycleMode::Auto` with only `[2026-07-28]` **fails** against a 2025-11-25 server ("no compatible protocol version"); the server answers `server/discover` with its list instead of erroring | The backend client must list both versions, newest first |
| A bridge needs no message translation for tools: each side negotiates independently | The gateway forwards by meaning (tool name + arguments), not by raw frames |
| Per-request `_meta` from the front session is version-specific | Never forwarded to backends; calls are rebuilt from name + arguments |
| `ListToolsResult` and other results are `#[non_exhaustive]` | Build with `default()` + assignment |

## Decision

1. **One adapter crate.** Only `mcpgw-proto` depends on `rmcp`
   (`scripts/check-deps.sh` enforces it in CI; the test-only `mcpgw-testkit`
   is the one exception). Its public API is rmcp-free: tools, arguments and
   results cross the seam as JSON objects, errors as `ProtoError`.
2. **Versions.** 2026-07-28 is primary on both sides; 2025-11-25 is accepted.
   - Front: answer `server/discover` and `initialize`.
   - Backends: `Auto` lifecycle, `preferred_versions = [2026-07-28,
     2025-11-25]`, `legacy_version = 2025-11-25`.
3. **Forwarding.** `tools/call` is re-issued as a fresh request with name
   and arguments only. Results come back as JSON and are reshaped (prefixes,
   redaction) outside the adapter.
4. **Pin rmcp** to `3.5.x` and upgrade deliberately, with the conformance and
   integration suites as the gate.
5. **Streamable HTTP.** 2026-07-28 requests are served statelessly; legacy
   clients get sessions (rmcp's `legacy_session_mode`).

## Consequences

- A protocol revision or rmcp major touches `mcpgw-proto` (and the fake
  servers) only.
- Features that need richer per-request context (elicitation, progress,
  cancellation, tasks) have to be added to the seam explicitly; they are not
  passed through by accident.
- Pagination: the gateway reads all backend pages and serves its own list.
