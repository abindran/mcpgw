# M0-06 — Core domain types

**Story:** X · **Estimate:** 2 days · **Depends on:** M0-01
**Done when:** types serialise to and from JSON and TOML; JSON Schema generated.
**Status:** ✅ Done

## Types (`mcpgw-core`)

| Type | Purpose |
|------|---------|
| `ServerSpec` (was `ServerConfig`) | One `[[server]]`: name, transport, `enabled`, `idle_timeout`, `stateful`, `startup_timeout` |
| `Transport` | `stdio` (`command`, `args`, `env`, `cwd`) or `http` (`url`, `headers`) |
| `ToolDef` | One MCP tool: typed `name`, `title`, `description`, `inputSchema`, `outputSchema`, `annotations`; unknown fields kept in `extra` so tools round-trip exactly |
| `ToolManifest` | A server's tools at capture time plus a SHA-256 `pin` |
| `Profile` | Named view: servers, `mode`, pinned tools, account bindings, policy |
| `Mode` | `meta` (default), `direct`, `hybrid` |
| `Policy` | `allow` / `deny` / `approve` glob lists (evaluated from M2-08) |
| `Duration` | `"500ms"`, `"30s"`, `"10m"`, `"2h"`, `"1d"` or whole seconds |
| `Error` | Crate-wide error enum |

### Manifest pin

`manifest::pin(tools)` hashes the **canonical JSON** of the tools: tools sorted
by name and object keys sorted at every level, with no whitespace. Reordering
tools or keys keeps the pin; changing any description, schema or annotation
changes it. It does not depend on serde_json's `preserve_order` feature, which
a dependency could switch on. `ToolManifest::verify()` recomputes it to detect
tampering in storage.

## JSON Schema

`schemars` derives the schemas. They are committed in `schema/`:

- `config.schema.json`: the whole config file
- `manifest.schema.json`
- `profile.schema.json`

`crates/mcpgw-config/tests/schema.rs` fails when a committed schema no longer
matches the types; `cargo xtask schema` regenerates them.

## Also

- The dev profile now keeps `target/` small (`debug = "line-tables-only"`,
  no incremental cache, no debug info for dependencies): about 1.3 GB instead
  of 6 GB, after the development disk filled up.
- `StdioSpec` in `mcpgw-proto` gained `cwd`.
