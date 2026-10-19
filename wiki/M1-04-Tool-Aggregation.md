# M1-04 — Tool aggregation with server-name prefixes

**Crate:** `mcpgw-client` (`src/aggregate.rs`) · **Status:** ✅ Done

## Naming scheme
- Public name: `<server>__<tool>`, for example `fs__read_file`. The separator is `mcpgw_core::TOOL_SEPARATOR`.
- Server names may not contain `__` or start or end with `_` (M1-01 validation), so splitting on the **first** `__` is always unambiguous, even when the tool name itself contains `__` (`fs__a__b` → `fs` / `a__b`).
- Descriptions get a `[server]` prefix so agents can tell sources apart. The input and output schemas, annotations and other fields pass through unchanged.
- A tool is dropped (with a warning) if it has no name or if its prefixed name is longer than 128 characters (`MAX_TOOL_NAME`).
- Order: servers by name, and each server's tools in the order the backend returned them. Duplicate names from the same backend are removed.

## API
- `public_name`, `split_name`, `prefix_tool`, `aggregate`: pure functions.
- `BackendManager::list_all_tools()`: the aggregated list from each backend's cached tools (refreshed on reconnect, per M1-03).
- `BackendManager::call_public(name, args)`: routes a call to the owning backend. A name without a prefix, or with an unknown server, returns JSON-RPC `-32602 tool not found: …`, the same shape a backend uses for its own unknown tools.

## Tests
4 unit tests (name round-trip, prefixing, dropped tools, the same tool name on two servers) and 1 integration test with two real fixture backends, which checks the aggregated list, routing via `$FIXTURE_PREFIX`, and the unknown-prefix errors.
