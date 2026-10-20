# M4-01 — Wasm launcher (wasmtime, component model, WASI p2, pooling)

**Story:** US4 · **Estimate:** 5 days · **Depends on:** M1-05
**Done when:** a Wasm tool instantiates in under 10 ms.
**Status:** ✅ Done: median **39 µs** (worst 0.4 ms over 50 instances); a full server start in under 1 ms

## Using it

```sh
mcpgw add notes --wasm ./notes-tool.wasm     # stored as an absolute path
mcpgw list                                    # notes  wasm  store  /…/notes-tool.wasm
```

```toml
[[server]]
name = "notes"
transport = "wasm"
path = "/opt/tools/notes-tool.wasm"
```

## The tool interface (`crates/mcpgw-wasm/wit/tool.wit`)

A Wasm tool server is a component exporting world `mcpgw:tool/tool-server`:

```wit
record tool   { name: string, description: string, input-schema: string }  // schema as JSON text
record output { text: string, is-error: bool }
export list-tools: func() -> list<tool>;
export call-tool:  func(name: string, arguments: string) -> result<output, string>;  // arguments: JSON
```

JSON text keeps the interface small and stable while MCP itself evolves. An
`err` is a protocol-level failure (unknown tool, bad arguments); `is-error`
is a failure the model should see. The SDK and template come in M4-04.

## How it runs

- **`mcpgw-wasm` crate** (wasmtime 49 with `component-model`,
  `pooling-allocator`, `cranelift` and `async`, plus wasmtime-wasi p2):
  - *one engine per process*: the pooling allocator (64 component instances,
    256 MiB memory cap, small reservations), falling back to on-demand
    allocation if the system won't grant the address space;
  - **WASI p2** is linked, with nothing granted: no files, network or
    environment. Grants come in M4-02;
  - `load(path)` compiles and links once into a `ToolServerPre`, cached in
    memory per file version and **on disk** by content hash
    (`runtimes/wasm/*.cwasm` next to the store). A restart deserializes
    instead of recompiling;
  - `instantiate()` makes a fresh store and instance from the pool.
- **Runtime:** a `wasm` server's start loads the component (compiling off
  the async threads), instantiates it, and serves it through
  **`BackendConn::connect_local`**, a new in-memory MCP session in proto.
  So a Wasm tool has the same handshake, pin and drift check, policy,
  approvals, audit, idle stop and status as every other server, and rmcp
  stays inside proto. Its runtime label for metrics is `wasm`.

## Measured (macOS arm64)

| Step | Time |
|------|------|
| compile and link (first use; cached afterwards) | 87 ms |
| **instantiate** from the pre-linked component | median **39 µs**, max 0.4 ms (50 held at once) |
| full server start: instantiate, MCP handshake, `tools/list` (`last_start_ms`) | under 1 ms |

## Test fixtures

`wasm/echo-tool` (outside the workspace) is a Rust guest written with
wit-bindgen 0.63. Its tools are `echo`, `reverse`, and `spin`, an endless
loop for M4-03. `cargo xtask wasm` builds every guest in `wasm/` for
`wasm32-unknown-unknown` and encodes it as a component with
`wit-component`, into `crates/mcpgw-wasm/tests/fixtures/` (committed; about
32 KB; `.gitattributes` marks `*.wasm` binary).

## Tests

- `crates/mcpgw-wasm/tests/instantiate.rs`:
  - **Done-when:** the median of 50 instantiations is under 10 ms.
  - Its tools are listed with their schemas, `echo` and `reverse` answer,
    and an unknown tool is an error.
  - Loading again is cached.
  - Files that aren't components are rejected.
- `crates/mcpgw-runtime/tests/wasm.rs`: under the manager, the server is
  introspected and pinned, cold until called, connected without a pid, and
  starts in under 200 ms (it measures 0 ms). It stops when idle, a fresh
  instance answers, and a guest error leaves it running.
- `crates/mcpgw/tests/wasm.rs` (end to end): `mcpgw add wt --wasm …` pins 3
  tools, `list` shows `wasm`, the compiled code is cached on disk, and a call
  through the daemon works. `status` shows `idle  3 tools  -`.
