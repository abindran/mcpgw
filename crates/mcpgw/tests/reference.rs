//! M4-11 done-when: the docs cover every CLI command, config key and API
//! endpoint. The three references are generated, from the binary's clap
//! definitions, `schema/config.schema.json` and `docs/openapi.json`, so
//! they can't drift: this test fails when a committed file is stale.
//! Regenerate with `cargo xtask schema` (sets `MCPGW_UPDATE_SCHEMA=1`).

use std::{collections::BTreeSet, path::PathBuf, process::Command};

use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Compare `text` with the committed file, or write it.
fn check(rel: &str, text: &str) {
    let path = root().join(rel);
    if std::env::var_os("MCPGW_UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == text,
        "{rel} is stale; run `cargo xtask schema`"
    );
}

fn json(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(rel)).unwrap()).unwrap()
}

#[test]
fn cli_reference_covers_every_command() {
    let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .args(["__reference", "cli"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let md = String::from_utf8(o.stdout).unwrap();
    // every command `mcpgw --help` lists has a section
    let help = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout).into_owned();
    let commands: Vec<&str> = help
        .split("Commands:")
        .nth(1)
        .unwrap()
        .split("Options:")
        .next()
        .unwrap()
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .collect();
    assert!(commands.len() > 20, "{commands:?}");
    for c in commands {
        assert!(md.contains(&format!("`mcpgw {c}`")), "no section for {c}");
    }
    check("docs/reference/cli.md", &md);
}

// --- config ---------------------------------------------------------------

/// Follow a `$ref` into `$defs`.
fn resolve<'a>(schema: &'a Value, s: &'a Value) -> &'a Value {
    match s.get("$ref").and_then(Value::as_str) {
        Some(r) => schema["$defs"]
            .get(r.trim_start_matches("#/$defs/"))
            .unwrap_or(s),
        None => s,
    }
}

/// A short type: `string`, `integer or null`, `array of string`, `Duration`.
fn type_of(s: &Value) -> String {
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        return r.trim_start_matches("#/$defs/").to_string();
    }
    if let Some(any) = s
        .get("anyOf")
        .or_else(|| s.get("oneOf"))
        .and_then(Value::as_array)
    {
        let parts: Vec<String> = any.iter().map(type_of).collect();
        return parts.join(" or ");
    }
    if let Some(c) = s.get("const") {
        return format!("`{c}`");
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        return e
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(", ");
    }
    match &s["type"] {
        Value::String(t) if t == "array" => format!("array of {}", type_of(&s["items"])),
        Value::String(t)
            if t == "object" && s.get("additionalProperties").is_some_and(|a| a.is_object()) =>
        {
            format!("map of {}", type_of(&s["additionalProperties"]))
        }
        Value::String(t) => t.clone(),
        Value::Array(ts) => ts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" or "),
        _ => "any".into(),
    }
}

fn describe(s: &Value) -> String {
    let mut d = s["description"]
        .as_str()
        .unwrap_or_default()
        .replace('\n', " ");
    if let Some(def) = s
        .get("default")
        .filter(|v| !v.is_null() && !d.contains("Default"))
    {
        d.push_str(&format!(" Default: `{def}`."));
    }
    d.replace('|', "\\|")
}

/// One table per object, then its object-typed children; `seen` stops
/// recursion and repeats.
fn table(
    schema: &Value,
    title: &str,
    obj: &Value,
    out: &mut String,
    keys: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) {
    let obj = resolve(schema, obj);
    let Some(props) = obj["properties"].as_object() else {
        return;
    };
    out.push_str(&format!("\n### {title}\n\n"));
    if let Some(d) = obj["description"].as_str() {
        out.push_str(&format!("{}\n\n", d.replace('\n', " ")));
    }
    out.push_str("| Key | Type | Description |\n|---|---|---|\n");
    let mut nested = Vec::new();
    for (k, v) in props {
        keys.insert(k.clone());
        out.push_str(&format!(
            "| `{k}` | {} | {} |\n",
            type_of(v).replace('|', "\\|"),
            describe(v)
        ));
        // the types a key holds: directly, as an option, a list or a map
        let mut refs = Vec::new();
        collect_refs(v, &mut refs);
        for r in refs {
            let name = r.trim_start_matches("#/$defs/").to_string();
            if seen.insert(name.clone()) {
                nested.push((name, json!({"$ref": r})));
            }
        }
    }
    for (name, s) in nested {
        variants_or_table(schema, &name, &s, out, keys, seen);
    }
}

/// Every `$ref` in `v`, through `anyOf`, `oneOf`, `items` and maps.
fn collect_refs(v: &Value, out: &mut Vec<String>) {
    if let Some(r) = v.get("$ref").and_then(Value::as_str) {
        out.push(r.to_string());
    }
    for k in ["anyOf", "oneOf", "allOf"] {
        if let Some(xs) = v.get(k).and_then(Value::as_array) {
            xs.iter().for_each(|x| collect_refs(x, out));
        }
    }
    for k in ["items", "additionalProperties"] {
        if let Some(x) = v.get(k).filter(|x| x.is_object()) {
            collect_refs(x, out);
        }
    }
}

/// Objects, and tagged enums (one table per variant).
fn variants_or_table(
    schema: &Value,
    name: &str,
    s: &Value,
    out: &mut String,
    keys: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) {
    let d = resolve(schema, s);
    if d.get("properties").is_some() {
        table(schema, &format!("`{name}`"), d, out, keys, seen);
    }
    if let Some(vs) = d
        .get("oneOf")
        .or_else(|| d.get("anyOf"))
        .and_then(Value::as_array)
    {
        for v in vs {
            let v = resolve(schema, v);
            let tag = v["properties"]
                .as_object()
                .and_then(|p| p.iter().find(|(_, x)| x.get("const").is_some()))
                .map(|(k, x)| format!("{k} = {}", x["const"]));
            if let Some(tag) = tag {
                table(schema, &format!("`{name}`: `{tag}`"), v, out, keys, seen);
            }
        }
    }
}

#[test]
fn config_reference_covers_every_key() {
    let schema = json("schema/config.schema.json");
    let mut out = String::from(
        "# Configuration reference\n\n\
         <!-- Generated from `schema/config.schema.json` by `cargo xtask schema`; don't edit. -->\n\n\
         The config file is TOML: `~/.config/mcpgw/config.toml`, `$MCPGW_CONFIG` or `--config`.\n\
         Editors can validate it with the JSON Schema in `schema/config.schema.json`.\n",
    );
    let mut keys = BTreeSet::new();
    let mut seen = BTreeSet::new();
    table(
        &schema,
        "Top level",
        &schema,
        &mut out,
        &mut keys,
        &mut seen,
    );
    // every key of every object in the schema is documented
    fn all_keys(v: &Value, acc: &mut BTreeSet<String>) {
        match v {
            Value::Object(o) => {
                if let Some(p) = o.get("properties").and_then(Value::as_object) {
                    acc.extend(p.keys().cloned());
                }
                for x in o.values() {
                    all_keys(x, acc);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| all_keys(x, acc)),
            _ => {}
        }
    }
    let mut every = BTreeSet::new();
    all_keys(&schema, &mut every);
    let missing: Vec<_> = every.difference(&keys).collect();
    assert!(missing.is_empty(), "undocumented config keys: {missing:?}");
    check("docs/reference/config.md", &out);
}

// --- API ------------------------------------------------------------------

#[test]
fn api_reference_covers_every_endpoint() {
    let spec = json("docs/openapi.json");
    let mut out = String::from(
        "# HTTP API reference\n\n\
         <!-- Generated from `docs/openapi.json` by `cargo xtask schema`; don't edit. -->\n\n\
         The admin API is served by the daemon on loopback. Every request needs\n\
         `Authorization: Bearer <token>` (`mcpgw token`). The OpenAPI document is\n\
         `docs/openapi.json`, also served at `GET /api/v1/openapi.json`.\n",
    );
    let mut count = 0;
    let mut by_tag: std::collections::BTreeMap<String, Vec<(String, String, Value)>> =
        Default::default();
    for (path, ops) in spec["paths"].as_object().unwrap() {
        for (method, op) in ops.as_object().unwrap() {
            let tag = op["tags"][0].as_str().unwrap_or("other").to_string();
            by_tag
                .entry(tag)
                .or_default()
                .push((method.to_uppercase(), path.clone(), op.clone()));
        }
    }
    for (tag, ops) in &by_tag {
        out.push_str(&format!("\n## {tag}\n"));
        for (method, path, op) in ops {
            count += 1;
            out.push_str(&format!(
                "\n### `{method} {path}`\n\n{}\n",
                op["summary"].as_str().unwrap_or_default()
            ));
            if let Some(ps) = op["parameters"].as_array().filter(|p| !p.is_empty()) {
                out.push_str("\n| Parameter | In | Description |\n|---|---|---|\n");
                for p in ps {
                    out.push_str(&format!(
                        "| `{}` | {} | {} |\n",
                        p["name"].as_str().unwrap_or_default(),
                        p["in"].as_str().unwrap_or_default(),
                        p["description"]
                            .as_str()
                            .unwrap_or_default()
                            .replace('|', "\\|")
                    ));
                }
            }
            if op.get("requestBody").is_some() {
                out.push_str("\nRequest body: JSON (see `docs/openapi.json`).\n");
            }
            if let Some(rs) = op["responses"].as_object() {
                let codes: Vec<String> = rs
                    .iter()
                    .map(|(c, r)| {
                        format!("`{c}` {}", r["description"].as_str().unwrap_or_default())
                    })
                    .collect();
                out.push_str(&format!("\nResponses: {}.\n", codes.join("; ")));
            }
        }
    }
    let paths: usize = spec["paths"]
        .as_object()
        .unwrap()
        .values()
        .map(|o| o.as_object().unwrap().len())
        .sum();
    assert_eq!(count, paths);
    check("docs/reference/api.md", &out);
}

// --- links ----------------------------------------------------------------

/// Every relative link in `docs/` and the README points at a file that
/// exists (and every page is in the book's SUMMARY).
#[test]
fn docs_links_resolve() {
    let docs = root().join("docs");
    let mut pages = Vec::new();
    fn walk(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    walk(&docs, &mut pages);
    pages.push(root().join("README.md"));
    let link = regex::Regex::new(r"\]\(([^)\s]+)\)").unwrap();
    let mut broken = Vec::new();
    for page in &pages {
        let text = std::fs::read_to_string(page).unwrap();
        // links inside code blocks are examples, not links
        let mut in_code = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                continue;
            }
            for c in link.captures_iter(line) {
                let target = &c[1];
                if target.contains("://")
                    || target.starts_with('#')
                    || target.starts_with("mailto:")
                {
                    continue;
                }
                let path = target.split('#').next().unwrap();
                if !page.parent().unwrap().join(path).exists() {
                    broken.push(format!(
                        "{}: {target}",
                        page.strip_prefix(root()).unwrap().display()
                    ));
                }
            }
        }
    }
    assert!(broken.is_empty(), "broken links: {broken:#?}");
    let summary = std::fs::read_to_string(docs.join("SUMMARY.md")).unwrap();
    for page in pages.iter().filter(|p| p.starts_with(&docs)) {
        let rel = page
            .strip_prefix(&docs)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let in_adr = rel.starts_with("adr/") && rel != "adr/README.md";
        if rel != "SUMMARY.md" && !in_adr {
            assert!(
                summary.contains(&format!("({rel})")),
                "{rel} isn't in docs/SUMMARY.md"
            );
        }
    }
}
