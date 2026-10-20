//! `mcpgw add <registry name>` and `mcpgw registry …`: install servers from
//! the MCP Registry with pinned versions (M2-14).

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, IsTerminal, Write},
    path::Path,
    time::Duration,
};

use mcpgw_config::Config;
use mcpgw_core::ToolManifest;
use mcpgw_registry::{OFFICIAL, PlanOptions, Registry, ServerJson};
use mcpgw_vault::{KeySource, Vault};

use crate::{AnyResult, secrets, serve, servers};

/// Choices for a registry install.
pub struct Install {
    /// `io.github.github/github-mcp-server`, optionally `@<version>`.
    pub entry: String,
    /// Server name (default: the entry's last path segment).
    pub server_name: Option<String>,
    /// Use the hosted remote.
    pub remote: bool,
    /// Only this package type.
    pub package: Option<String>,
    /// Values for the entry's non-secret inputs.
    pub values: BTreeMap<String, String>,
    /// Add without asking.
    pub yes: bool,
    /// Shorter tool prefix.
    pub alias: Option<String>,
    /// Never stopped for idleness.
    pub stateful: bool,
    /// Register but don't start it.
    pub disabled: bool,
    /// Don't read its tools now.
    pub no_introspect: bool,
}

fn registry(config: &Config) -> AnyResult<Registry> {
    Ok(Registry::new(
        config.gateway.registry_url.as_deref().unwrap_or(OFFICIAL),
    )?)
}

/// `name@version` → (name, Some(version)); names contain `/`, versions don't.
pub fn split_version(entry: &str) -> (&str, Option<&str>) {
    match entry.rsplit_once('@') {
        Some((n, v)) if n.contains('/') && !v.contains('/') && !v.is_empty() => (n, Some(v)),
        _ => (entry, None),
    }
}

fn vault_secrets(config: &Config) -> BTreeSet<String> {
    let path = config.gateway.vault_path();
    if !path.exists() {
        return BTreeSet::new();
    }
    Vault::open(&path, &KeySource::from_env())
        .map(|v| v.list().into_iter().map(|(n, ..)| n).collect())
        .unwrap_or_default()
}

fn first_line(s: &str, max: usize) -> String {
    let l = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if l.chars().count() > max {
        format!("{}…", l.chars().take(max - 1).collect::<String>())
    } else {
        l.to_string()
    }
}

/// Ask on the terminal (or read a line from piped stdin).
fn confirm(question: &str) -> AnyResult<bool> {
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn show_tools(name: &str, m: Option<&ToolManifest>) {
    match m {
        Some(m) => {
            println!("{name} offers {} tools:", m.tools.len());
            for t in &m.tools {
                let d = t.description.as_deref().map(|d| first_line(d, 90));
                match d.filter(|d| !d.is_empty()) {
                    Some(d) => println!("  {}  {d}", t.name),
                    None => println!("  {}", t.name),
                }
            }
        }
        None => {
            println!("{name}: its tools could not be read now; they are pinned on its first start")
        }
    }
}

/// `mcpgw add <registry name>`.
pub fn add(config: &Config, path: &Path, i: Install) -> AnyResult<()> {
    let (name, version) = split_version(&i.entry);
    let reg = registry(config)?;
    let entry = serve::runtime()?.block_on(reg.get(name, version))?;
    let have = vault_secrets(config);
    let server_name = i
        .server_name
        .clone()
        .unwrap_or_else(|| mcpgw_registry::default_server_name(&entry.name));
    let plan = mcpgw_registry::plan(
        &entry,
        &PlanOptions {
            server_name,
            prefer_remote: i.remote,
            package_type: i.package.clone(),
            values: i.values.clone(),
            has_secret: &|s| have.contains(s),
            runtime_available: &mcpgw_registry::on_path,
        },
    )?;
    let mut spec = plan.spec;
    spec.alias = i.alias;
    spec.stateful = i.stateful;
    spec.enabled = !i.disabled;
    println!("{} {} via {}", entry.name, entry.version, plan.method);
    if let Some(d) = &entry.description {
        println!("  {}", first_line(d, 100));
    }
    println!("  runs: {}", servers::describe(&spec));
    for n in &plan.notes {
        println!("  note: {n}");
    }
    for s in &plan.skipped {
        println!(
            "  optional secret {s} is not set; to use it: `mcpgw secret set {s}`, then add again"
        );
    }
    for s in plan
        .secrets
        .iter()
        .filter(|s| s.required && !have.contains(&s.name))
    {
        if std::io::stdin().is_terminal() {
            println!("  {} needs {} ({})", spec.name, s.name, s.description);
            secrets::cmd::set(config, &s.name, Some(s.description.clone()))?;
        } else {
            return Err(format!(
                "{} needs the secret {} ({}); store it with `mcpgw secret set {}` and add again",
                spec.name, s.name, s.description, s.name
            )
            .into());
        }
    }
    // A first pull can take minutes and would time out the handshake.
    if let (Some(image), mcpgw_core::Transport::Stdio { command, .. }) =
        (&plan.image, &spec.transport)
        && spec.enabled
        && !i.no_introspect
        && mcpgw_registry::on_path(command)
    {
        eprintln!("pulling {image}");
        let ok = std::process::Command::new(command)
            .args(["pull", image.as_str()])
            .stdin(std::process::Stdio::null())
            // progress goes to our stderr; stdout stays for results
            .stdout(std::io::stderr())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("mcpgw: could not pull {image}; is the {command} daemon running?");
        }
    }
    let label = spec.name.clone();
    let yes = i.yes;
    let review = move |m: Option<&ToolManifest>| -> AnyResult<bool> {
        show_tools(&label, m);
        Ok(yes || confirm(&format!("add {label}?"))?)
    };
    // first runs download the package or image: allow for that
    servers::add_reviewed(
        config,
        path,
        spec,
        !i.no_introspect,
        Duration::from_secs(300),
        Some(&review),
    )?;
    Ok(())
}

fn kinds(s: &ServerJson) -> String {
    let mut k: Vec<String> = s.packages.iter().map(|p| p.registry_type.clone()).collect();
    if !s.remotes.is_empty() {
        k.push("remote".into());
    }
    k.dedup();
    k.join(",")
}

/// `mcpgw registry search <query>`.
pub fn search(config: &Config, query: &str, limit: usize) -> AnyResult<()> {
    let list = serve::runtime()?.block_on(registry(config)?.search(query, limit))?;
    if list.is_empty() {
        println!("no servers match {query:?}");
    }
    for s in list {
        let about = s
            .title
            .as_deref()
            .or(s.description.as_deref())
            .unwrap_or("");
        println!(
            "{}\t{}\t{}\t{}",
            s.name,
            s.version,
            kinds(&s),
            first_line(about, 80)
        );
    }
    Ok(())
}

/// `mcpgw registry show <name[@version]>`.
pub fn show(config: &Config, entry: &str) -> AnyResult<()> {
    let (name, version) = split_version(entry);
    let s = serve::runtime()?.block_on(registry(config)?.get(name, version))?;
    println!("{} {}", s.name, s.version);
    if let Some(t) = &s.title {
        println!("  title: {t}");
    }
    if let Some(d) = &s.description {
        println!("  {}", first_line(d, 200));
    }
    for p in &s.packages {
        println!(
            "  package: {} {} {}",
            p.registry_type,
            p.identifier,
            p.version.as_deref().unwrap_or("")
        );
        for v in &p.environment_variables {
            let what = if v.is_secret { "secret" } else { "value" };
            let req = if v.is_required {
                "required"
            } else {
                "optional"
            };
            println!("    env {} ({req} {what})", v.name);
        }
    }
    for r in &s.remotes {
        println!("  remote: {} {}", r.kind, r.url);
        for h in &r.headers {
            let req = if h.is_required {
                "required"
            } else {
                "optional"
            };
            println!(
                "    header {} ({req}{})",
                h.name,
                if h.is_secret { " secret" } else { "" }
            );
        }
    }
    println!("install: mcpgw add {}@{}", s.name, s.version);
    Ok(())
}
