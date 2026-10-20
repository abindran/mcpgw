//! The published `docs/openapi.json` must match the API.
//! Regenerate with `cargo xtask schema` (sets `MCPGW_UPDATE_SCHEMA=1`).

use std::path::PathBuf;

#[test]
fn published_openapi_is_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json");
    let text = serde_json::to_string_pretty(&mcpgw_api::v1::openapi()).unwrap() + "\n";
    if std::env::var_os("MCPGW_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == text,
        "docs/openapi.json is stale; run `cargo xtask schema`"
    );
}

#[test]
fn every_operation_is_described() {
    let spec = mcpgw_api::v1::openapi();
    for (path, ops) in spec["paths"].as_object().unwrap() {
        assert!(path.starts_with("/api/v1/"), "{path}");
        for (method, op) in ops.as_object().unwrap() {
            assert!(
                op["summary"].as_str().is_some_and(|s| !s.is_empty()),
                "{method} {path}"
            );
            assert!(
                op["responses"].as_object().is_some_and(|r| !r.is_empty()),
                "{method} {path}"
            );
        }
    }
    for s in [
        "ServerSpec",
        "Profile",
        "SecretMeta",
        "Server",
        "Runtime",
        "Approval",
        "Error",
    ] {
        assert!(spec["components"]["schemas"][s].is_object(), "{s}");
    }
}
