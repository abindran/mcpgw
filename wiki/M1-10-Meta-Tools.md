# M1-10 — Meta-tools mode

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M1-09
**Done when:** enabling a server makes its tools findable within 1 s with no client restart.
**Status:** ✅ Done

## Modes (`gateway.mode`)

| Mode | `tools/list` returns |
|------|----------------------|
| `meta` (**default**) | `search_tools`, `describe_tool`, `call_tool` |
| `direct` | every backend tool (`<prefix>__<tool>`) |
| `hybrid` | the meta-tools plus `gateway.pinned_tools` |

In every mode any tool can also be called directly by its public name.
Meta is the default as the plan's risk table recommends: the list never
changes, so clients that ignore `list_changed` still see servers that are
enabled or added at runtime, and the model's context stays small.

## The meta-tools (`mcpgw-router::meta`)

- **`search_tools {query, limit = 10 (max 50)}`**: keyword match. Each query
  word scores 3 if a name token starts with it, 2 if a parameter name
  contains it, and 1 if a description word starts with it. Ties sort by name.
  An empty query lists tools. Returns text lines (`name - description`) and
  `structuredContent.tools`. BM25 replaces the scoring in M3-09.
- **`describe_tool {name}`**: the full public tool definition (description,
  input schema, annotations) as text and as `structuredContent`.
- **`call_tool {name, arguments}`**: runs the tool and returns its result
  unchanged.
- Problems the model can fix come back as tool errors (`isError: true`)
  that point it at `search_tools`, not as JSON-RPC errors: an unknown tool, a
  backend failure, or `call_tool` called on itself.
- Request logs record the inner tool name for `call_tool`.

## Runtime changes to the server set (`mcpgw-runtime`)

The backend set is now dynamic: `add_server`, `remove_server`, and
`set_enabled(name, bool)`. Disabling drains and stops the server and fails
queued calls with "server X was disabled". New state-machine events
`Enable` (`Disabled → Cold`) and `Disable` (from a stopped state; the actor
sends `Stop` first) bring the exhaustive table to 13 × 12 pairs. The CLI
commands that drive these on a running daemon come in M1-13.

## Tests

- `crates/mcpgw-router/tests/meta.rs`, with a real MCP client over HTTP:
  - **disabled `zoo` server: "zebra" finds nothing → `set_enabled(zoo)` →
    within 1 s the same session finds `zoo__zebra_facts`**, describes it and
    calls it through `call_tool`; the listing is still the three meta-tools;
    disabling hides it again
  - a server added at runtime is findable; meta errors are tool errors;
    direct calls work in meta mode
  - hybrid lists the meta-tools plus `fx__add`
- `meta.rs` unit tests: ranking, parameter matches, empty query, limits.
- `disable_enable_and_remove_at_runtime` (runtime): a disabled server's
  process is gone and calls are refused; re-enable; add, reject duplicates,
  remove.
- End-to-end tests of the binary set `mode = "direct"` where they list
  backend tools.
