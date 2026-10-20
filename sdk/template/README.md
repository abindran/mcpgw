# {{project-name}}

An mcpgw Wasm tool server.

```sh
rustup target add wasm32-unknown-unknown        # once
cargo build --release --target wasm32-unknown-unknown
mcpgw add {{project-name}} --wasm target/wasm32-unknown-unknown/release/{{crate_name}}.wasm
```

Then call `{{project-name}}__greet` from any MCP client connected to mcpgw.
The component runs with no network, files or environment unless you grant
them (`--allow-network`, `--allow-dir`, `--env`), and each call has a time
budget (`--time-budget`, default 30s).
