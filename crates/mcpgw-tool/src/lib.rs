//! `mcpgw-tool` — write mcpgw Wasm tool servers in Rust (M4-04).
//!
//! Build a [`Tools`] set from typed handlers and export it with
//! [`export!`]. The crate implements the `mcpgw:tool/tool-server` world
//! (`wit/tool.wit`) for you: tool listing with JSON Schemas generated from
//! the argument types, argument parsing, and results.
//!
//! ```ignore
//! #[derive(serde::Deserialize, schemars::JsonSchema)]
//! struct GreetArgs { name: String }
//!
//! fn tools() -> mcpgw_tool::Tools {
//!     mcpgw_tool::Tools::new()
//!         .tool("greet", "Say hello", |a: GreetArgs| Ok(format!("Hello, {}!", a.name)))
//! }
//! mcpgw_tool::export!(tools);
//! ```

use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use schemars;
pub use serde_json;

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "tool-server",
        pub_export_macro: true,
        export_macro_name: "__mcpgw_export",
        default_bindings_module: "mcpgw_tool::bindings",
    });
}

/// What a tool returns: text, and whether it is an error the model should
/// see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The text content.
    pub text: String,
    /// A tool-level failure (`isError`).
    pub is_error: bool,
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        Output {
            text,
            is_error: false,
        }
    }
}

impl From<&str> for Output {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}

impl From<Value> for Output {
    fn from(v: Value) -> Self {
        v.to_string().into()
    }
}

type Handler = Box<dyn Fn(Value) -> Result<Output, String> + Send + Sync>;

struct Entry {
    name: String,
    description: String,
    schema: Value,
    handler: Handler,
}

/// A set of tools.
#[derive(Default)]
pub struct Tools(Vec<Entry>);

/// A tool as listed.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
}

fn schema_of<A: JsonSchema>() -> Value {
    let mut v = schemars::schema_for!(A).to_value();
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.entry("type")
            .or_insert_with(|| Value::String("object".into()));
    }
    v
}

impl Tools {
    /// No tools yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tool. `f` gets the arguments parsed into `A` (whose JSON Schema
    /// is listed) and returns `Ok(output)` or `Err(message)`: a tool error
    /// the model sees.
    pub fn tool<A, R, F>(mut self, name: &str, description: &str, f: F) -> Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        R: Into<Output>,
        F: Fn(A) -> Result<R, String> + Send + Sync + 'static,
    {
        let tool = name.to_string();
        let handler: Handler = Box::new(move |args| {
            let a: A = serde_json::from_value(args)
                .map_err(|e| format!("invalid arguments for {tool}: {e}"))?;
            Ok(match f(a) {
                Ok(r) => r.into(),
                Err(text) => Output {
                    text,
                    is_error: true,
                },
            })
        });
        self.0.push(Entry {
            name: name.to_string(),
            description: description.to_string(),
            schema: schema_of::<A>(),
            handler,
        });
        self
    }

    /// The tools, as listed.
    pub fn list(&self) -> Vec<ToolInfo> {
        self.0
            .iter()
            .map(|e| ToolInfo {
                name: e.name.clone(),
                description: e.description.clone(),
                input_schema: e.schema.clone(),
            })
            .collect()
    }

    /// Call one with JSON `arguments`. `Err` is a protocol-level failure
    /// (unknown tool, arguments that don't parse).
    pub fn call(&self, name: &str, arguments: &str) -> Result<Output, String> {
        let entry = self
            .0
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let args: Value = if arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(arguments).map_err(|e| format!("arguments are not JSON: {e}"))?
        };
        (entry.handler)(args)
    }
}

#[doc(hidden)]
pub mod __run {
    use super::*;

    fn tools(make: fn() -> Tools) -> &'static Tools {
        static TOOLS: OnceLock<Tools> = OnceLock::new();
        TOOLS.get_or_init(make)
    }

    pub fn list(make: fn() -> Tools) -> Vec<bindings::Tool> {
        tools(make)
            .list()
            .into_iter()
            .map(|t| bindings::Tool {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema.to_string(),
            })
            .collect()
    }

    pub fn call(
        make: fn() -> Tools,
        name: &str,
        arguments: &str,
    ) -> Result<bindings::Output, String> {
        tools(make).call(name, arguments).map(|o| bindings::Output {
            text: o.text,
            is_error: o.is_error,
        })
    }
}

/// Export a tool set as this component's `mcpgw:tool/tool-server`.
/// `$make` is a `fn() -> Tools`, called once.
#[macro_export]
macro_rules! export {
    ($make:path) => {
        struct __McpgwToolServer;

        impl $crate::bindings::Guest for __McpgwToolServer {
            fn list_tools() -> ::std::vec::Vec<$crate::bindings::Tool> {
                $crate::__run::list($make)
            }

            fn call_tool(
                name: ::std::string::String,
                arguments: ::std::string::String,
            ) -> ::std::result::Result<$crate::bindings::Output, ::std::string::String> {
                $crate::__run::call($make, &name, &arguments)
            }
        }

        $crate::bindings::__mcpgw_export!(__McpgwToolServer with_types_in $crate::bindings);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, JsonSchema)]
    struct Add {
        /// First.
        a: i64,
        b: i64,
    }

    fn tools() -> Tools {
        Tools::new()
            .tool("add", "Add two numbers", |x: Add| {
                Ok((x.a + x.b).to_string())
            })
            .tool("fail", "Always fails", |_: Add| -> Result<String, String> {
                Err("no".into())
            })
    }

    #[test]
    fn lists_with_schemas_and_calls() {
        let t = tools();
        let l = t.list();
        assert_eq!(l[0].name, "add");
        assert_eq!(l[0].input_schema["type"], "object");
        assert_eq!(
            l[0].input_schema["properties"]["a"]["description"],
            "First."
        );
        assert!(l[0].input_schema.get("$schema").is_none());
        assert_eq!(
            t.call("add", r#"{"a": 2, "b": 3}"#).unwrap(),
            Output::from("5")
        );
        assert_eq!(
            t.call("fail", r#"{"a": 1, "b": 1}"#).unwrap(),
            Output {
                text: "no".into(),
                is_error: true
            }
        );
        assert!(
            t.call("add", r#"{"a": "x"}"#)
                .unwrap_err()
                .contains("invalid arguments for add")
        );
        assert!(t.call("nope", "{}").unwrap_err().contains("unknown tool"));
        assert!(t.call("add", "not json").unwrap_err().contains("not JSON"));
    }

    #[test]
    fn wit_matches_the_host() {
        assert_eq!(
            include_str!("../wit/tool.wit"),
            include_str!("../../mcpgw-wasm/wit/tool.wit")
        );
    }
}
