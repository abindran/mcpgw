//! `mcpgw project init|trust|untrust|ls`, and finding the project that
//! `mcpgw connect` runs in.

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
};

use mcpgw_api::projects::{self as reg, TrustRecord};
use mcpgw_config::{
    Config,
    clients::{self, Client},
    project::{self, PROJECT_FILE},
};
use mcpgw_store::SqliteStore;
use serde_json::{Value, json};

use crate::{AnyResult, audit, control::Control};

const TEMPLATE: &str = r#"# Servers for this project only. mcpgw serves them, with your global ones,
# to clients started here (`mcpgw connect`). They stay off until you run
# `mcpgw project trust` here, and again after every change to this file.
#
# Same keys as [[server]] in mcpgw's config. Relative paths are relative
# to this directory, and stdio servers run here. Bind credentials with
# vault secret names (`mcpgw secret set <name>`), never values: this file
# is meant to be committed.

[project]
name = "@NAME@"

# [[server]]
# name = "db"
# transport = "stdio"
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-postgres"]
# secrets = { DATABASE_URL = "@NAME@_db_url" }
"#;

fn here(dir: Option<PathBuf>) -> AnyResult<PathBuf> {
    Ok(match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    })
}

/// The project at or above `dir` (default: the working directory).
fn root_of(dir: Option<PathBuf>) -> AnyResult<PathBuf> {
    let start = here(dir)?;
    project::find_root(&start).ok_or_else(|| {
        format!(
            "no {PROJECT_FILE} in {} or above it (`mcpgw project init` writes one)",
            start.display()
        )
        .into()
    })
}

/// `mcpgw project init`: write a `.mcpgw.toml` and, per `--client`, the
/// client's project-level entry for mcpgw.
pub fn init(dir: Option<PathBuf>, name: Option<String>, clients_: &[Client]) -> AnyResult<()> {
    let root = here(dir)?;
    let file = root.join(PROJECT_FILE);
    if file.exists() {
        println!("{} exists; leaving it", file.display());
    } else {
        let name = name.unwrap_or_else(|| {
            root.canonicalize()
                .ok()
                .and_then(|r| r.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "project".into())
        });
        let name = name.replace(['"', '\\'], "");
        std::fs::write(&file, TEMPLATE.replace("@NAME@", &name))?;
        println!("wrote {}", file.display());
    }
    for c in clients_ {
        let Some(path) = c.project_config(&root) else {
            return Err(format!(
                "{} has no project-level config; `mcpgw client install {}` connects it for every project",
                c.name(),
                c.name()
            )
            .into());
        };
        let mut cfg = clients::read(&path)?;
        clients::install_gateway(*c, &mut cfg, c.project_entry());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
        println!("added the mcpgw entry to {}", path.display());
    }
    println!("add servers to {PROJECT_FILE}, then run `mcpgw project trust` to turn them on");
    Ok(())
}

fn store(config: &Config) -> AnyResult<SqliteStore> {
    Ok(SqliteStore::open(config.gateway.store_path())?)
}

/// Print what the project runs, and where its trust stands.
fn show(p: &project::Project, record: Option<&TrustRecord>) {
    println!("project {} ({})", p.name, p.root.display());
    println!("  {PROJECT_FILE} sha256 {}", &p.sha256[..12]);
    match record {
        Some(r) if r.sha256 == p.sha256 => println!("  trusted (this content)"),
        Some(r) => println!(
            "  CHANGED since it was trusted (then sha256 {})",
            &r.sha256[..12.min(r.sha256.len())]
        ),
        None => println!("  not trusted yet"),
    }
    if p.servers.is_empty() {
        println!("  no servers");
    }
    for (name, runs) in project::describe(p) {
        println!("  - {name}: {runs}");
    }
}

/// `mcpgw project trust`: show what the file runs, ask, and record its
/// content as trusted (through the daemon when it runs, so the servers
/// start at once).
pub fn trust(config: &Config, dir: Option<PathBuf>, yes: bool) -> AnyResult<()> {
    let root = root_of(dir)?;
    let p = project::load(&root)?;
    let records = reg::trusted(&store(config)?)?;
    let record = records.get(&*p.root.to_string_lossy());
    show(&p, record);
    if record.is_some_and(|r| r.sha256 == p.sha256) {
        return Ok(());
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err("pass --yes to trust it without asking".into());
        }
        if !crate::registry::confirm("Trust this file and run these servers?")? {
            return Err("not trusted".into());
        }
    }
    match Control::connect(config)? {
        Some(c) => {
            let id = register(&c, &p.root)?["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            c.call(
                reqwest::Method::POST,
                &format!("/api/v1/projects/{id}/trust"),
                Some(json!({ "sha256": p.sha256 })),
            )?;
        }
        None => {
            reg::record_trust(&store(config)?, &p)?;
            audit::record(
                config,
                "project.trust",
                &p.root.to_string_lossy(),
                json!({"sha256": p.sha256}),
            );
        }
    }
    println!(
        "trusted {}: its servers run for clients in this project",
        p.name
    );
    Ok(())
}

/// `mcpgw project untrust`: its servers stop.
pub fn untrust(config: &Config, dir: Option<PathBuf>) -> AnyResult<()> {
    let root = root_of(dir)?;
    let root = root.canonicalize()?;
    match Control::connect(config)? {
        Some(c) => {
            let id = project::project_id(&root);
            register(&c, &root)?;
            c.call(
                reqwest::Method::POST,
                &format!("/api/v1/projects/{id}/untrust"),
                None,
            )?;
        }
        None => {
            reg::set_trusted(&store(config)?, &root, None)?;
            audit::record(
                config,
                "project.untrust",
                &root.to_string_lossy(),
                json!({}),
            );
        }
    }
    println!(
        "{} is no longer trusted; its servers are off",
        root.display()
    );
    Ok(())
}

/// `mcpgw project ls`: trusted projects, and what the daemon has
/// registered.
pub fn ls(config: &Config) -> AnyResult<()> {
    let records = reg::trusted(&store(config)?)?;
    let live: Vec<Value> = match Control::connect(config)? {
        Some(c) => c
            .call(reqwest::Method::GET, "/api/v1/projects", None)?
            .and_then(|v| v["projects"].as_array().cloned())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut roots: Vec<String> = records.keys().cloned().collect();
    for p in &live {
        if let Some(r) = p["root"].as_str()
            && !roots.iter().any(|x| x == r)
        {
            roots.push(r.to_string());
        }
    }
    if roots.is_empty() {
        println!("no projects (`mcpgw project init` starts one)");
    }
    roots.sort();
    for root in roots {
        let rp = Path::new(&root);
        let (name, state, servers) = match project::load(rp) {
            Ok(p) => {
                let state = match records.get(&root) {
                    Some(r) if r.sha256 == p.sha256 => "trusted",
                    Some(_) => "changed",
                    None => "untrusted",
                };
                (p.name.clone(), state, p.declared.len())
            }
            Err(_) if !rp.join(PROJECT_FILE).exists() => (
                records
                    .get(&root)
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
                "missing",
                0,
            ),
            Err(_) => (String::new(), "invalid", 0),
        };
        println!("{name}\t{state}\t{servers} servers\t{root}");
    }
    Ok(())
}

fn register(c: &Control, root: &Path) -> AnyResult<Value> {
    Ok(c.call(
        reqwest::Method::POST,
        "/api/v1/projects",
        Some(json!({ "path": root })),
    )?
    .unwrap_or_default())
}

/// For `mcpgw connect`: the endpoint of the project at or above `dir`
/// (`--project`, else the working directory), registered with the running
/// daemon, and a line about it for the client's log. `None` outside a
/// project.
pub fn endpoint(config: &Config, dir: Option<PathBuf>) -> AnyResult<Option<(String, String)>> {
    let start = here(dir)?;
    let Some(root) = project::find_root(&start) else {
        return Ok(None);
    };
    let c = Control::connect(config)?.ok_or("the daemon is not running")?;
    let p = register(&c, &root)?;
    let url = p["url"].as_str().unwrap_or_default().to_string();
    let name = p["name"].as_str().unwrap_or_default();
    let n = p["servers"].as_array().map(Vec::len).unwrap_or(0);
    let line = match p["state"].as_str().unwrap_or_default() {
        "trusted" => format!("project {name} ({}): {n} server(s)", root.display()),
        "changed" => format!(
            "project {name}: {PROJECT_FILE} changed since it was trusted; its servers are off until `mcpgw project trust` in {}",
            root.display()
        ),
        "invalid" => format!(
            "project {name}: {PROJECT_FILE} has errors: {}",
            p["error"].as_str().unwrap_or_default()
        ),
        _ => format!(
            "project {name}: not trusted; its {n} server(s) are off until `mcpgw project trust` in {}",
            root.display()
        ),
    };
    Ok(Some((url, line)))
}
