//! text-tools: an example mcpgw Wasm tool server with two tools. It needs no
//! grants: no network, no files, no environment.
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown
//! mcpgw add text-tools --wasm target/wasm32-unknown-unknown/release/text_tools.wasm
//! ```

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct Text {
    /// The text to work on.
    text: String,
}

#[derive(Deserialize, JsonSchema)]
struct Slug {
    /// The title to turn into a URL slug.
    title: String,
    /// The longest slug to return, in characters.
    #[serde(default)]
    max_len: Option<usize>,
}

fn count_words(a: Text) -> Result<serde_json::Value, String> {
    let words = a.text.split_whitespace().count();
    let lines = a.text.lines().count();
    Ok(serde_json::json!({ "words": words, "lines": lines, "chars": a.text.chars().count() }))
}

fn slugify(a: Slug) -> Result<String, String> {
    let mut slug = String::new();
    for c in a.title.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        return Err("the title has no letters or digits".into());
    }
    if let Some(max) = a.max_len {
        slug = slug.chars().take(max).collect::<String>().trim_end_matches('-').to_string();
    }
    Ok(slug)
}

fn tools() -> mcpgw_tool::Tools {
    mcpgw_tool::Tools::new()
        .tool("count_words", "Count the words, lines and characters of a text", count_words)
        .tool("slugify", "Turn a title into a URL slug", slugify)
}

mcpgw_tool::export!(tools);
