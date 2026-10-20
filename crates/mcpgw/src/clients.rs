//! `mcpgw client install|import` (M1-14).

use std::path::{Path, PathBuf};

use mcpgw_config::{
    Config,
    clients::{self, Client},
};
use mcpgw_store::{SqliteStore, Store, effective_servers};
use mcpgw_vault::{KeySource, Vault};
use secrecy::SecretString;

use crate::{AnyResult, control::Control};

fn target(client: Client, path: Option<PathBuf>) -> AnyResult<PathBuf> {
    path.or_else(|| client.user_config())
        .ok_or_else(|| format!("cannot find {}'s config; pass --path", client.name()).into())
}

fn entry(client: Client, explicit_config: Option<&Path>) -> AnyResult<serde_json::Value> {
    let exe = std::env::current_exe()?;
    let config = explicit_config.map(std::fs::canonicalize).transpose()?;
    Ok(client.gateway_entry(&exe.to_string_lossy(), config.as_deref()))
}

fn write(path: &Path, value: &serde_json::Value) -> AnyResult<Option<PathBuf>> {
    let backup = if path.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut b = path.as_os_str().to_owned();
        b.push(format!(".mcpgw-backup-{stamp}"));
        let b = PathBuf::from(b);
        std::fs::copy(path, &b)?;
        Some(b)
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        None
    };
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(backup)
}

/// Add the gateway entry to a client, keeping its other servers.
pub fn install(
    client: Client,
    path: Option<PathBuf>,
    explicit_config: Option<&Path>,
    dry_run: bool,
) -> AnyResult<()> {
    let path = target(client, path)?;
    let mut cfg = clients::read(&path)?;
    clients::install_gateway(client, &mut cfg, entry(client, explicit_config)?);
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
        return Ok(());
    }
    let backup = write(&path, &cfg)?;
    println!("installed the mcpgw entry in {}", path.display());
    if let Some(b) = backup {
        println!("backup: {}", b.display());
    }
    Ok(())
}

/// Move a client's servers into mcpgw and leave only the gateway entry.
pub fn import(
    config: &Config,
    client: Client,
    path: Option<PathBuf>,
    explicit_config: Option<&Path>,
    dry_run: bool,
    keep_env: bool,
) -> AnyResult<()> {
    let path = target(client, path)?;
    let mut cfg = clients::read(&path)?;
    let found = clients::entries(client, &cfg);
    if found.is_empty() {
        println!("no servers to import in {}", path.display());
        return Ok(());
    }
    let store = SqliteStore::open(config.gateway.store_path())?;
    let mut taken: Vec<String> = effective_servers(&config.servers, &store)?
        .into_iter()
        .map(|s| s.name)
        .collect();
    let mut plan = Vec::new();
    for (raw, e) in &found {
        let mut spec = match clients::to_spec(raw, e) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("mcpgw: skipping {raw:?}: {err}");
                continue;
            }
        };
        // unique names: fs, fs-2, fs-3 ...
        let base = spec.name.clone();
        let mut n = 2;
        while taken.contains(&spec.name) {
            spec.name = format!("{base}-{n}");
            n += 1;
        }
        taken.push(spec.name.clone());
        let secrets = if keep_env {
            Vec::new()
        } else {
            clients::extract_secrets(&mut spec)
        };
        spec.validate()?;
        plan.push((raw.clone(), spec, secrets));
    }

    for (raw, spec, secrets) in &plan {
        let moved = if secrets.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = secrets.iter().map(|(n, _)| n.as_str()).collect();
            format!(" (secrets to vault: {})", names.join(", "))
        };
        println!("{raw} -> {} [{}]{moved}", spec.name, spec.transport.kind());
    }
    if dry_run {
        println!("dry run: nothing changed");
        return Ok(());
    }

    let any_secret = plan.iter().any(|(_, _, s)| !s.is_empty());
    if any_secret {
        let mut vault = Vault::open_or_create(config.gateway.vault_path(), &KeySource::from_env())
            .map_err(|e| {
                format!("{e}; or run with --keep-env to leave credentials in the server env")
            })?;
        for (_, _, secrets) in &plan {
            for (name, value) in secrets {
                vault.set(name, &SecretString::from(value.clone()))?;
            }
        }
    }
    let control = Control::connect(config)?;
    for (_, spec, _) in &plan {
        store.upsert_server(spec)?;
        if let Some(c) = &control {
            c.add(spec, None)?;
        }
    }
    clients::replace_with_gateway(client, &mut cfg, entry(client, explicit_config)?);
    let backup = write(&path, &cfg)?;
    println!(
        "imported {} server(s) from {}; it now has only the mcpgw entry",
        plan.len(),
        path.display()
    );
    if let Some(b) = backup {
        println!("backup: {}", b.display());
    }
    Ok(())
}
