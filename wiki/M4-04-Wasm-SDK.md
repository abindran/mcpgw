# M4-04 — Wasm tool SDK and template (Rust)

**Story:** US1 · **Estimate:** 3 days · **Depends on:** M4-02
**Done when:** a new tool goes from template to working call in under 15 minutes.
**Status:** ✅ Done: **19 s** from template to a working call (16.7 s of it the first build). The crate is ready to publish: `cargo publish --dry-run` passes; the upload is left to the maintainer.

## From template to call

```sh
rustup target add wasm32-unknown-unknown                       # once
cargo generate --git https://github.com/abindran/mcpgw sdk/template --name hello-tool
cd hello-tool
cargo build --release --target wasm32-unknown-unknown
mcpgw add hello-tool --wasm target/wasm32-unknown-unknown/release/hello_tool.wasm
# → added server hello-tool (1 tools, pin …); call hello-tool__greet {"name": "mcpgw"} → "Hello, mcpgw!"
```

There's no separate componentize step: `mcpgw add --wasm` (via
`mcpgw_wasm::componentize`) turns the core module that `cargo build`
produces into a component, using the WIT that wit-bindgen embedded. Builds
for `wasm32-wasip2`, which are already components, load as they are.

## The SDK: `mcpgw-tool` (`crates/mcpgw-tool`)

```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GreetArgs {
    /// Who to greet.            ← becomes the parameter's description
    name: String,
}

fn tools() -> mcpgw_tool::Tools {
    mcpgw_tool::Tools::new()
        .tool("greet", "Say hello to someone", |a: GreetArgs| Ok(format!("Hello, {}!", a.name)))
}

mcpgw_tool::export!(tools);
```

- `Tools::tool(name, description, handler)` takes a typed handler, whose
  argument type gives the listed JSON Schema (schemars; `$schema` and
  `title` dropped). It returns anything `Into<Output>` (`String`, `&str`,
  `serde_json::Value`).
- The handler's `Err(message)` is a **tool error** (`isError`) the model
  sees. An unknown tool, arguments that aren't JSON, or arguments that
  don't fit the type are protocol errors.
- `export!(fn)` implements the `mcpgw:tool/tool-server` world through
  wit-bindgen (`pub_export_macro`). The set is built once.
- `mcpgw-tool` is a workspace member, so its logic is unit-tested on the
  host. A test keeps its `wit/tool.wit` identical to the host's.

## Template (`sdk/template`)

A cargo-generate template (`{{project-name}}`, `{{crate_name}}`):
- a `cdylib`, size-optimized in release;
- a `greet` example that shows a typed argument and a tool error;
- a README covering build, add, grants and time budget.

It depends on the SDK from git until the crate is published; switch it to
`mcpgw-tool = "0.1"` then.

## Tests

- `crates/mcpgw/tests/sdk_template.rs`, the **done-when** test: render the
  template, build it for `wasm32-unknown-unknown` against the in-repo SDK,
  then `mcpgw add --wasm` the raw build output. It checks the listed schema
  (`name` described "Who to greet."), the call through the daemon ("Hello,
  mcpgw!"), and a tool error for an empty name. It asserts under 15 minutes
  and measures 19 s. It skips without the wasm target, except on the Linux
  glibc CI job, which installs the target and sets `MCPGW_TEST_WASM=1`.
- `mcpgw-tool` unit tests: schemas and descriptions, calls, tool errors vs
  protocol errors, and the WIT staying in sync.
- `crates/mcpgw-wasm/tests/instantiate.rs`: a core module from `cargo
  build` is componentized on load and answers.

## Still for a person

- Publish the crate: `cargo publish -p mcpgw-tool` (crates.io account
  needed), then point the template at the version.
