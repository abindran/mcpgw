//! {{project-name}}: an mcpgw Wasm tool server.
//!
//! Each tool takes a typed argument struct (its JSON Schema is what the
//! model sees) and returns `Ok(text)` or `Err(message)`. Add tools with more
//! `.tool(...)` calls.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct GreetArgs {
    /// Who to greet.
    name: String,
}

fn tools() -> mcpgw_tool::Tools {
    mcpgw_tool::Tools::new().tool("greet", "Say hello to someone", |a: GreetArgs| {
        if a.name.trim().is_empty() {
            return Err("give a name".to_string());
        }
        Ok(format!("Hello, {}!", a.name))
    })
}

mcpgw_tool::export!(tools);
