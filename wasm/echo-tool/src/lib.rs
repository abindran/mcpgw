//! Test tool server: `echo` returns its `text`, `reverse` reverses it,
//! `spin` loops forever and `alloc` holds `mib` MiB (for the M4-03 time
//! budget and memory cap).

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
            Tool { name: "alloc".into(), description: "Hold mib MiB of memory".into(), input_schema: r#"{"type":"object","properties":{"mib":{"type":"number"}}}"#.into() },
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
            "alloc" => {
                let mib: usize = arguments
                    .split("\"mib\"")
                    .nth(1)
                    .and_then(|r| r.trim_start_matches([':', ' ']).split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                let mut held: Vec<Vec<u8>> = Vec::new();
                for _ in 0..mib {
                    held.push(vec![1u8; 1 << 20]);
                }
                Ok(Output { text: format!("held {} MiB", held.len()), is_error: false })
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

export!(Echo);
