//! `mcpgw __reference cli` (M4-11): the CLI reference as Markdown, built
//! from clap's own definitions so `docs/reference/cli.md` can't drift from
//! the binary (`tests/reference.rs` checks it).

use clap::{ArgAction, Command};

/// Markdown for every visible command, option and argument.
pub fn cli(root: &Command) -> String {
    let mut root = root.clone().bin_name("mcpgw");
    root.build();
    let mut out = String::from(
        "# CLI reference\n\n\
         <!-- Generated from `mcpgw __reference cli` by `cargo xtask schema`; don't edit. -->\n\n\
         Global options go before the command: `mcpgw --config ./config.toml list`.\n",
    );
    command(&root, "mcpgw", 2, &mut out);
    out
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn command(cmd: &Command, path: &str, level: usize, out: &mut String) {
    out.push_str(&format!("\n{} `{path}`\n\n", "#".repeat(level.min(4))));
    if let Some(about) = cmd.get_long_about().or(cmd.get_about()) {
        out.push_str(&format!("{about}\n\n"));
    }
    let aliases: Vec<&str> = cmd.get_visible_aliases().collect();
    if !aliases.is_empty() {
        out.push_str(&format!(
            "Aliases: {}\n\n",
            aliases
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let usage = cmd.clone().render_usage().to_string();
    let usage = usage.trim_start_matches("Usage: ").trim();
    out.push_str(&format!("```\n{usage}\n```\n"));
    let args: Vec<_> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version"))
        // global options are listed once, under `mcpgw`
        .filter(|a| path == "mcpgw" || !a.is_global_set())
        .collect();
    if !args.is_empty() {
        out.push_str("\n| Argument | Description |\n|---|---|\n");
        for a in args {
            let value = a
                .get_value_names()
                .map(|v| {
                    v.iter()
                        .map(|n| format!("<{n}>"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_else(|| format!("<{}>", a.get_id().as_str().to_uppercase()));
            let takes_value = !matches!(
                a.get_action(),
                ArgAction::SetTrue
                    | ArgAction::SetFalse
                    | ArgAction::Count
                    | ArgAction::Help
                    | ArgAction::Version
            );
            let name = if a.is_positional() {
                value
            } else {
                let mut n = Vec::new();
                if let Some(s) = a.get_short() {
                    n.push(format!("-{s}"));
                }
                if let Some(l) = a.get_long() {
                    n.push(format!("--{l}"));
                }
                let n = n.join(", ");
                if takes_value {
                    format!("{n} {value}")
                } else {
                    n
                }
            };
            let mut desc = a
                .get_long_help()
                .or(a.get_help())
                .map(|h| h.to_string())
                .unwrap_or_default();
            if !desc.is_empty() && !desc.ends_with('.') {
                desc.push('.');
            }
            let possible: Vec<String> = a
                .get_possible_values()
                .iter()
                .filter(|p| !p.is_hide_set())
                .map(|p| format!("`{}`", p.get_name()))
                .collect();
            if !possible.is_empty() && takes_value {
                desc.push_str(&format!(" One of: {}.", possible.join(", ")));
            }
            let defaults: Vec<String> = a
                .get_default_values()
                .iter()
                .map(|d| d.to_string_lossy().into_owned())
                .filter(|d| !d.is_empty())
                .collect();
            if !defaults.is_empty() && takes_value {
                desc.push_str(&format!(" Default: `{}`.", defaults.join(" ")));
            }
            if matches!(a.get_action(), ArgAction::Append) {
                desc.push_str(" Repeatable.");
            }
            if a.is_required_set() {
                desc.push_str(" Required.");
            }
            out.push_str(&format!("| `{}` | {} |\n", cell(&name), cell(desc.trim())));
        }
    }
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
        if sub.get_name() == "help" {
            continue;
        }
        command(sub, &format!("{path} {}", sub.get_name()), level + 1, out);
    }
}
