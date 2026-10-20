# Wasm tools

A Wasm tool server is a WebAssembly component that mcpgw runs in-process
with wasmtime (component model, WASI 0.2). It starts in microseconds, and it
can only reach what it is granted.

## From template to call

```sh
rustup target add wasm32-unknown-unknown                       # once
cargo generate --git https://github.com/abindran/mcpgw sdk/template --name hello-tool
cd hello-tool
cargo build --release --target wasm32-unknown-unknown
mcpgw add hello-tool --wasm target/wasm32-unknown-unknown/release/hello_tool.wasm
```

The template is a crate using the `mcpgw-tool` SDK:

```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
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

- **The argument type** gives the tool's input schema, and doc comments
  become parameter descriptions.
- **Errors.** `Err(message)` is a tool error the model sees.
- **No componentize step.** `mcpgw add --wasm` turns the core module
  `cargo build` produces into a component. `wasm32-wasip2` builds load as
  they are.
- **Other languages.** Any component exporting the `mcpgw:tool/tool-server`
  world (`crates/mcpgw-wasm/wit/tool.wit`) works: `list-tools` and
  `call-tool`, with JSON text.

While you work on it, `mcpgw dev ./hello-tool.wasm` reloads the component on
every rebuild ([Developing a server](dev.md)).

## Grants: deny by default

A component gets no network, no files and no environment unless granted:

```toml
[[server]]
name = "weather"
transport = "wasm"
path = "/opt/tools/weather.wasm"
allow_network = ["api.weather.example:443"]   # host:port, ip:port, or "*"
allow_dirs = [{ path = "/data/cache", write = true }, { path = "/data/ref", guest = "/ref" }]
env = { UNITS = "metric", API_KEY = "${secret:weather_key}" }
time_budget = "5s"
limits = { memory = "64MiB" }
```

```sh
mcpgw add weather --wasm ./weather.wasm --allow-network api.weather.example:443 \
  --allow-dir-rw /data/cache --time-budget 5s --memory 64MiB
```

- **Network.** Host names are resolved when the server starts, and only
  those addresses may be connected to. DNS lookups are allowed only with a
  name grant or `*`.
- **Directories** are mounted at their own path, or at `guest`, and are
  read-only unless `write = true`.

## Limits

- **Time.** Each call has a time budget (default 30 s). A call that runs
  past it is stopped (epoch interruption) with a tool error, and the next
  call gets a fresh instance.
- **Memory.** Linear memory is capped by `limits.memory` (default and
  maximum 256 MiB). Going over fails that call, not the gateway.
