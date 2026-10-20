//! Test tool server: `echo` returns its `text`, `reverse` reverses it,
//! `spin` loops forever (for the M4-03 time budget).

wit_bindgen::generate!({
    path: "../../crates/mcpgw-wasm/wit",
    world: "tool-server",
});


struct Echo;

fn text_arg(arguments: &str) -> String {
    // tiny JSON lookup for {"text": "..."}, enough for a test fixture
    let Some(i) = arguments.find("\"text\"") else {
        return String::new();
    };
    let rest = &arguments[i + 6..];
    let Some(start) = rest.find('"') else {
        return String::new();
    };
    let rest = &rest[start + 1..];
    let mut out = String::new();
    let mut esc = false;
    for c in rest.chars() {
        match (esc, c) {
            (true, c) => {
                out.push(c);
                esc = false;
            }
            (false, '\\') => esc = true,
            (false, '"') => break,
            (false, c) => out.push(c),
        }
    }
    out
}

impl Guest for Echo {
    fn list_tools() -> Vec<Tool> {
        let schema = r#"{"type":"object","properties":{"text":{"type":"string"}}}"#;
        vec![
            Tool { name: "echo".into(), description: "Echo text back".into(), input_schema: schema.into() },
            Tool { name: "reverse".into(), description: "Reverse text".into(), input_schema: schema.into() },
            Tool { name: "spin".into(), description: "Never returns".into(), input_schema: r#"{"type":"object"}"#.into() },
        ]
    }

    fn call_tool(name: String, arguments: String) -> Result<Output, String> {
        match name.as_str() {
            "echo" => Ok(Output { text: text_arg(&arguments), is_error: false }),
            "reverse" => Ok(Output { text: text_arg(&arguments).chars().rev().collect(), is_error: false }),
            "spin" => {
                let mut n: u64 = 0;
                loop {
                    n = n.wrapping_add(1);
                    core::hint::black_box(n);
                }
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

export!(Echo);
