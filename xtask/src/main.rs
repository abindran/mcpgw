//! `cargo xtask` — build, release, conformance and e2e helpers.

use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask <ci|schema>

  ci      run the local CI gates (fmt, clippy, test, dependency rule, canary scan)
  schema  regenerate the JSON Schemas in schema/";

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
    let ok = Command::new("cargo")
        .args(["test", "-q", "-p", "mcpgw-config", "--test", "schema"])
        .env("MCPGW_UPDATE_SCHEMA", "1")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok && run("git", &["status", "--short", "schema"])
}

fn main() -> ExitCode {
    let ok = match std::env::args().nth(1).as_deref() {
        Some("ci") => ci(),
        Some("schema") => schema(),
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
