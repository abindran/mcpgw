//! `cargo xtask` — build, release, conformance and e2e helpers.

use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask <ci|schema|wasm>

  ci      run the local CI gates (fmt, clippy, test, dependency rule, canary scan)
  schema  regenerate the JSON Schemas in schema/ and docs/openapi.json
  wasm    build the test Wasm tools in wasm/ and write them as components to
          crates/mcpgw-wasm/tests/fixtures/ (needs the wasm32-unknown-unknown target)";

fn run(cmd: &str, args: &[&str]) -> bool {
    eprintln!("$ {cmd} {}", args.join(" "));
    Command::new(cmd)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ci() -> bool {
    run("cargo", &["fmt", "--all", "--", "--check"])
        && run(
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        )
        && run("cargo", &["test", "--workspace"])
        && run("bash", &["scripts/check-deps.sh"])
        && run("bash", &["scripts/canary-scan.sh"])
}

fn schema() -> bool {
    let regen = |pkg: &str, test: &str| {
        Command::new("cargo")
            .args(["test", "-q", "-p", pkg, "--test", test])
            .env("MCPGW_UPDATE_SCHEMA", "1")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let ok = regen("mcpgw-config", "schema") && regen("mcpgw-api", "openapi");
    ok && run("git", &["status", "--short", "schema", "docs/openapi.json"])
}

/// Build each guest crate in `wasm/` for wasm32-unknown-unknown and encode
/// the core module (with its embedded WIT) as a component.
fn wasm() -> bool {
    let out = std::path::Path::new("crates/mcpgw-wasm/tests/fixtures");
    let Ok(dirs) = std::fs::read_dir("wasm") else {
        eprintln!("no wasm/ directory");
        return false;
    };
    let mut dirs: Vec<_> = dirs
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();
    dirs.sort();
    let canonical = std::fs::read_to_string("crates/mcpgw-wasm/wit/tool.wit").unwrap_or_default();
    for dir in dirs {
        // guests with their own WIT keep a copy of the tool world: it must be current
        let copy = dir.join("wit/deps/tool.wit");
        if copy.is_file() && std::fs::read_to_string(&copy).unwrap_or_default() != canonical {
            eprintln!(
                "{} is out of date; copy crates/mcpgw-wasm/wit/tool.wit over it",
                copy.display()
            );
            return false;
        }
        let manifest = dir.join("Cargo.toml");
        let manifest = manifest.to_string_lossy();
        if !run(
            "cargo",
            &[
                "build",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "--manifest-path",
                &manifest,
            ],
        ) {
            return false;
        }
        let name = dir.file_name().unwrap().to_string_lossy().replace('-', "_");
        let core = dir.join(format!("target/wasm32-unknown-unknown/release/{name}.wasm"));
        let bytes = match std::fs::read(&core) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", core.display());
                return false;
            }
        };
        let component = wit_component::ComponentEncoder::default()
            .module(&bytes)
            .and_then(|e| e.validate(true).encode());
        match component {
            Ok(c) => {
                let file = out.join(format!(
                    "{}.wasm",
                    dir.file_name().unwrap().to_string_lossy()
                ));
                if let Err(e) = std::fs::write(&file, &c) {
                    eprintln!("{}: {e}", file.display());
                    return false;
                }
                eprintln!("wrote {} ({} bytes)", file.display(), c.len());
            }
            Err(e) => {
                eprintln!("encoding {}: {e:#}", core.display());
                return false;
            }
        }
    }
    true
}

fn main() -> ExitCode {
    let ok = match std::env::args().nth(1).as_deref() {
        Some("ci") => ci(),
        Some("schema") => schema(),
        Some("wasm") => wasm(),
        _ => {
            eprintln!("{USAGE}");
            false
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
