//! The committed JSON Schemas in `schema/` must match the types.
//! Regenerate with `cargo xtask schema` (sets `MCPGW_UPDATE_SCHEMA=1`).

use std::path::PathBuf;

fn check(file: &str, schema: serde_json::Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .join(file);
    let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    if std::env::var_os("MCPGW_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == text,
        "schema/{file} is stale; run `cargo xtask schema`"
    );
}

#[test]
fn config_schema_is_current() {
    let s = mcpgw_config::json_schema();
    assert!(s["properties"]["server"].is_object(), "{s}");
    check("config.schema.json", s);
}

#[test]
fn manifest_schema_is_current() {
    let s = serde_json::to_value(schemars::schema_for!(mcpgw_core::ToolManifest)).unwrap();
    check("manifest.schema.json", s);
}

#[test]
fn profile_schema_is_current() {
    let s = serde_json::to_value(schemars::schema_for!(mcpgw_core::Profile)).unwrap();
    check("profile.schema.json", s);
}
