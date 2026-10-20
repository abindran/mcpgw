Test tool server for the Wasm capability grants (M4-02). `wit/deps/` holds
the WASI 0.2.12 WIT from wasmtime-wasi 49 (Apache-2.0 WITH LLVM-exception)
and a copy of `crates/mcpgw-wasm/wit/tool.wit`, which `cargo xtask wasm`
checks is current. Built by `cargo xtask wasm`.
