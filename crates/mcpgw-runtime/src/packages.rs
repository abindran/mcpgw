//! Package cache for npm and PyPI servers (M3-08).
//!
//! A server launched as `npx -y <pkg>[@version] args…` or `uvx
//! <pkg>[==version] args…` is installed once into the cache
//! (`<data>/runtimes`, see [`set_dir`]), and from then on it starts the
//! installed entry point directly, as `node <cache>/…/bin.js args…` or
//! `<cache>/…/venv/bin/<cmd> args…`. No `npx -y` resolution, download or
//! network happens at call time. Unpinned specs stay at the version
//! installed until the server is added (or `mcpgw runtimes install`ed)
//! again.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Use `dir` as the package cache (set once, at startup).
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// The package cache, if one is set.
pub fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// Package ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `npx -y <pkg>`.
    Npm,
    /// `uvx <pkg>`.
    Pypi,
}

/// A package a server's command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRef {
    /// Ecosystem.
    pub kind: Kind,
    /// The spec as written: `@scope/pkg@1.2.3`, `pkg==1.0`, a tarball path.
    pub spec: String,
    /// The server's own arguments, after the spec.
    pub rest: Vec<String>,
}

/// What an installed package runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// Ecosystem.
    pub kind: Kind,
    /// The spec it was installed from.
    pub spec: String,
    /// Package name.
    pub name: String,
    /// Installed version.
    pub version: String,
    /// Entry point, relative to the cache slot.
    pub entry: String,
}

const META: &str = "mcpgw-package.json";

/// The package behind `command args`, if it's an `npx`/`uvx` launch.
pub fn detect(command: &str, args: &[String]) -> Option<PackageRef> {
    let base = Path::new(command)
        .file_stem()?
        .to_str()?
        .to_ascii_lowercase();
    let (kind, flags): (Kind, &[&str]) = match base.as_str() {
        "npx" => (Kind::Npm, &["-y", "--yes", "-q", "--quiet"]),
        "uvx" => (Kind::Pypi, &["-q", "--quiet"]),
        _ => return None,
    };
    let i = args
        .iter()
        .take_while(|a| flags.contains(&a.as_str()))
        .count();
    let spec = args.get(i)?;
    // other launcher options (--package, --from, …) aren't handled
    if spec.starts_with('-') {
        return None;
    }
    Some(PackageRef {
        kind,
        spec: spec.clone(),
        rest: args[i + 1..].to_vec(),
    })
}

/// The cache slot for a package (by spec, so `pkg` and `pkg@1` differ).
pub fn slot(cache: &Path, p: &PackageRef) -> PathBuf {
    let clean: String = p
        .spec
        .chars()
        .rev()
        .take(48)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash: String = Sha256::digest(p.spec.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();
    let kind = match p.kind {
        Kind::Npm => "npm",
        Kind::Pypi => "pypi",
    };
    cache
        .join(kind)
        .join(format!("{}-{hash}", clean.trim_start_matches(['_', '@'])))
}

/// What's installed for `p`, if anything (and its entry point exists).
pub fn installed(cache: &Path, p: &PackageRef) -> Option<(PathBuf, Installed)> {
    let slot = slot(cache, p);
    let meta: Installed = serde_json::from_slice(&std::fs::read(slot.join(META)).ok()?).ok()?;
    (meta.spec == p.spec && slot.join(&meta.entry).is_file()).then_some((slot, meta))
}

/// How to launch `command args` from the cache, if its package is there:
/// `(program, args)`.
pub fn cached_launch(command: &str, args: &[String]) -> Option<(String, Vec<String>)> {
    let p = detect(command, args)?;
    let (slot, meta) = installed(dir()?, &p)?;
    let entry = slot.join(&meta.entry).to_string_lossy().into_owned();
    Some(match meta.kind {
        Kind::Npm => (
            "node".to_string(),
            std::iter::once(entry)
                .chain(p.rest.iter().cloned())
                .collect(),
        ),
        Kind::Pypi => (entry, p.rest.clone()),
    })
}

fn tool(name: &str) -> String {
    if cfg!(windows) && matches!(name, "npm") {
        format!("{name}.cmd")
    } else {
        name.to_string()
    }
}

async fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut c = tokio::process::Command::new(program);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = tokio::time::timeout(std::time::Duration::from_secs(600), c.output())
        .await
        .map_err(|_| format!("{program} took longer than 10 minutes"))?
        .map_err(|e| format!("{program}: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        Err(format!(
            "{program} {}: {last}",
            args.first().copied().unwrap_or_default()
        ))
    }
}

fn read_json(p: &Path) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?)
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Install `p` into the cache (replacing what's there).
pub async fn install(cache: &Path, p: &PackageRef) -> Result<Installed, String> {
    let slot = slot(cache, p);
    let _ = std::fs::remove_dir_all(&slot);
    std::fs::create_dir_all(&slot).map_err(|e| format!("{}: {e}", slot.display()))?;
    let r = match p.kind {
        Kind::Npm => install_npm(&slot, &p.spec).await,
        Kind::Pypi => install_pypi(&slot, &p.spec).await,
    };
    match r {
        Ok(meta) => {
            let bytes = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
            // written last: its presence marks a complete install
            std::fs::write(slot.join(META), bytes).map_err(|e| e.to_string())?;
            Ok(meta)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&slot);
            Err(e)
        }
    }
}

async fn install_npm(slot: &Path, spec: &str) -> Result<Installed, String> {
    std::fs::write(
        slot.join("package.json"),
        r#"{"name":"mcpgw-runtime","private":true}"#,
    )
    .map_err(|e| e.to_string())?;
    let prefix = slot.to_string_lossy().into_owned();
    run(
        &tool("npm"),
        &[
            "install",
            "--prefix",
            &prefix,
            "--no-audit",
            "--no-fund",
            "--omit=dev",
            "--loglevel=error",
            spec,
        ],
        Some(slot),
    )
    .await?;
    let root = read_json(&slot.join("package.json"))?;
    let name = root["dependencies"]
        .as_object()
        .and_then(|d| d.keys().next().cloned())
        .ok_or("npm installed nothing")?;
    let pkg_dir = slot.join("node_modules").join(&name);
    let pkg = read_json(&pkg_dir.join("package.json"))?;
    let short = name.rsplit('/').next().unwrap_or(&name).to_string();
    let bin = match &pkg["bin"] {
        serde_json::Value::String(b) => Some(b.clone()),
        serde_json::Value::Object(m) if m.len() == 1 => m
            .values()
            .next()
            .and_then(|v| v.as_str())
            .map(str::to_string),
        serde_json::Value::Object(m) => m
            .get(&short)
            .or_else(|| m.get(&name))
            .or_else(|| m.values().next())
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    }
    .ok_or_else(|| format!("{name} has no command (no \"bin\" in its package.json)"))?;
    let entry = format!("node_modules/{name}/{}", bin.trim_start_matches("./"));
    if !slot.join(&entry).is_file() {
        return Err(format!("{name}'s command {bin} is missing"));
    }
    Ok(Installed {
        kind: Kind::Npm,
        spec: spec.to_string(),
        name,
        version: pkg["version"].as_str().unwrap_or("?").to_string(),
        entry,
    })
}

/// `pkg==1.0` / `pkg@1.0` / `pkg[extra]` → `pkg`.
fn pypi_name(spec: &str) -> &str {
    let end = spec
        .find(['=', '@', '[', '<', '>', '~', '!', ' '])
        .unwrap_or(spec.len());
    &spec[..end]
}

async fn install_pypi(slot: &Path, spec: &str) -> Result<Installed, String> {
    let name = pypi_name(spec).to_string();
    // uvx writes pkg@1.0; pip wants pkg==1.0
    let pip_spec = match spec.split_once('@') {
        Some((n, v)) if !v.contains('/') => format!("{n}=={v}"),
        _ => spec.to_string(),
    };
    let venv = slot.join("venv");
    let venv_s = venv.to_string_lossy().into_owned();
    let (bin, exe) = if cfg!(windows) {
        ("Scripts", ".exe")
    } else {
        ("bin", "")
    };
    let python = venv.join(bin).join(format!("python{exe}"));
    if mcpgw_core::on_path("uv") {
        run("uv", &["venv", "--quiet", &venv_s], None).await?;
        let py = python.to_string_lossy().into_owned();
        run(
            "uv",
            &["pip", "install", "--quiet", "--python", &py, &pip_spec],
            None,
        )
        .await?;
    } else {
        let host = if cfg!(windows) { "python" } else { "python3" };
        run(host, &["-m", "venv", &venv_s], None).await?;
        let py = python.to_string_lossy().into_owned();
        run(
            &py,
            &[
                "-m",
                "pip",
                "install",
                "--quiet",
                "--disable-pip-version-check",
                &pip_spec,
            ],
            None,
        )
        .await?;
    }
    let entry = format!("venv/{bin}/{name}{exe}");
    if !slot.join(&entry).is_file() {
        return Err(format!("{name} installed no command named {name}"));
    }
    let py = python.to_string_lossy().into_owned();
    let version = run(
        &py,
        &[
            "-c",
            &format!("import importlib.metadata as m; print(m.version({name:?}))"),
        ],
        None,
    )
    .await
    .map(|v| v.trim().to_string())
    .unwrap_or_else(|_| "?".into());
    Ok(Installed {
        kind: Kind::Pypi,
        spec: spec.to_string(),
        name,
        version,
        entry,
    })
}

/// Everything in the cache: `(slot, what's installed)`.
pub fn list(cache: &Path) -> Vec<(PathBuf, Installed)> {
    let mut out = Vec::new();
    for kind in ["npm", "pypi"] {
        let Ok(rd) = std::fs::read_dir(cache.join(kind)) else {
            continue;
        };
        for e in rd.flatten() {
            if let Ok(b) = std::fs::read(e.path().join(META))
                && let Ok(m) = serde_json::from_slice::<Installed>(&b)
            {
                out.push((e.path(), m));
            }
        }
    }
    out.sort_by(|a, b| a.1.spec.cmp(&b.1.spec));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn detects_npx_and_uvx() {
        let p = detect(
            "npx",
            &s(&[
                "-y",
                "@modelcontextprotocol/server-filesystem@2025.8.21",
                "/tmp",
            ]),
        )
        .unwrap();
        assert_eq!(
            (p.kind, p.spec.as_str(), p.rest.clone()),
            (
                Kind::Npm,
                "@modelcontextprotocol/server-filesystem@2025.8.21",
                s(&["/tmp"])
            )
        );
        assert_eq!(
            detect("/usr/local/bin/npx.cmd", &s(&["pkg"])).unwrap().spec,
            "pkg"
        );
        let u = detect(
            "uvx",
            &s(&["mcp-server-time==0.6.2", "--local-timezone", "UTC"]),
        )
        .unwrap();
        assert_eq!((u.kind, u.rest.len()), (Kind::Pypi, 2));
        assert!(detect("npx", &s(&["--package", "x", "y"])).is_none());
        assert!(detect("uvx", &s(&["--from", "x", "y"])).is_none());
        assert!(detect("node", &s(&["server.js"])).is_none());
        assert!(detect("npx", &s(&["-y"])).is_none());
    }

    #[test]
    fn slots_and_names() {
        let a = slot(
            Path::new("/c"),
            &detect("npx", &s(&["-y", "@scope/pkg@1.0.0"])).unwrap(),
        );
        let b = slot(
            Path::new("/c"),
            &detect("npx", &s(&["-y", "@scope/pkg"])).unwrap(),
        );
        assert_ne!(a, b);
        assert!(a.starts_with("/c/npm"), "{a:?}");
        assert!(
            a.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("scope_pkg@1.0.0-"),
            "{a:?}"
        );
        assert_eq!(pypi_name("mcp-server-time==0.6.2"), "mcp-server-time");
        assert_eq!(pypi_name("pkg[cli]@1.0"), "pkg");
        assert_eq!(pypi_name("pkg"), "pkg");
    }
}
