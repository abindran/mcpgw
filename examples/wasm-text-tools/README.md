# text-tools: an example Wasm tool server

Two tools, `count_words` and `slugify`, built with the
[`mcpgw-tool`](../../crates/mcpgw-tool) SDK. The component runs inside mcpgw
with no grants (no network, files or environment).

```sh
rustup target add wasm32-unknown-unknown        # once
cargo build --release --target wasm32-unknown-unknown
mcpgw add text-tools --wasm target/wasm32-unknown-unknown/release/text_tools.wasm
# or, while editing: mcpgw dev target/wasm32-unknown-unknown/release/text_tools.wasm
```

`text-tools__slugify {"title": "Hello, Wasm World!"}` returns
`hello-wasm-world`. Start your own from the template: see
[Wasm tools](../../docs/guides/wasm.md).
