# mcpgw-tool

Write [mcpgw](https://github.com/abindran/mcpgw) Wasm tool servers in Rust.

```rust
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct GreetArgs {
    /// Who to greet.
    name: String,
}

fn tools() -> mcpgw_tool::Tools {
    mcpgw_tool::Tools::new()
        .tool("greet", "Say hello to someone", |a: GreetArgs| Ok(format!("Hello, {}!", a.name)))
}

mcpgw_tool::export!(tools);
```

```sh
cargo build --release --target wasm32-unknown-unknown
mcpgw add hello --wasm target/wasm32-unknown-unknown/release/hello_tool.wasm
```

Each tool takes a typed argument struct (its JSON Schema is generated with
`schemars`) and returns `Ok(text)` or `Err(message)` (a tool error the model
sees). `mcpgw add --wasm` accepts the plain `cargo build` output: it turns the
module into a component itself. Start from the template in
`sdk/template` (`cargo generate`).
