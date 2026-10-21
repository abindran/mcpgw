//! Project servers: a `.mcpgw.toml` at a repository's root lists servers
//! for that project only. `mcpgw connect`, started by a client inside the
//! project, finds the file by walking up from its working directory.
//!
//! The file uses the config's `[[server]]` syntax. Relative paths in it
//! resolve against the project's root, and stdio servers run there unless
//! they set `cwd`. Each server gets an internal name, `<name>-p<id>`, so
//! two projects (or a project and the global config) can both have a `db`.
//!
//! Nothing in the file runs until the user trusts that exact content (see
//! `mcpgw project trust`): a cloned repository could otherwise start any
//! command, or bind any vault secret to its own server.

use std::path::{Path, PathBuf};

use mcpgw_core::{ServerSpec, Transport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ConfigError, Origin};

/// The project file's name.
pub const PROJECT_FILE: &str = ".mcpgw.toml";

/// Longest server name in a project file (the internal name adds 10).
pub const MAX_NAME: usize = 54;

/// A project's `.mcpgw.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    /// About the project.
    #[serde(default, skip_serializing_if = "ProjectMeta::is_empty")]
    pub project: ProjectMeta,
    /// The project's servers, as in the config's `[[server]]`. Relative
    /// paths resolve against the project's root.
    #[serde(default, rename = "server", skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerSpec>,
}

/// `[project]` in a project file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    /// Shown in listings and messages. Default: the directory's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ProjectMeta {
    fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

/// JSON Schema of the project file.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ProjectFile)).expect("schema serialises")
}

/// A project file read from disk, its servers ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// The directory holding `.mcpgw.toml`.
    pub root: PathBuf,
    /// Stable id, from the root's path: the endpoint is `/mcp/project/<id>`.
    pub id: String,
    /// `[project] name`, else the directory's name.
    pub name: String,
    /// SHA-256 of the file's bytes, hex: what trusting it records.
    pub sha256: String,
    /// The servers as written (names and paths as in the file).
    pub declared: Vec<ServerSpec>,
    /// The servers to run: internal names, the written name as tool prefix,
    /// and paths resolved against the root.
    pub servers: Vec<ServerSpec>,
}

/// The nearest directory at or above `start` holding a project file. The
/// home directory itself never counts: a `.mcpgw.toml` there would make
/// every directory below it one project.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok());
    let start = start.canonicalize().ok()?;
    start
        .ancestors()
        .take_while(|d| home.as_deref() != Some(*d))
        .find(|d| d.join(PROJECT_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The id of the project rooted at `root` (an absolute, canonical path):
/// eight hex digits of the SHA-256 of the path.
pub fn project_id(root: &Path) -> String {
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// The internal name of project `id`'s server `name`.
pub fn internal_name(name: &str, id: &str) -> String {
    format!("{name}-p{id}")
}

/// SHA-256 of `bytes`, hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read and check the project at `root` (canonicalised here).
pub fn load(root: &Path) -> Result<Project, ConfigError> {
    let root = root
        .canonicalize()
        .map_err(|e| ConfigError::new(Origin::File(root.into()), format!("cannot open: {e}")))?;
    let path = root.join(PROJECT_FILE);
    let bytes = std::fs::read(&path)
        .map_err(|e| ConfigError::new(Origin::File(path.clone()), format!("cannot read: {e}")))?;
    let src = String::from_utf8(bytes.clone())
        .map_err(|_| ConfigError::new(Origin::File(path.clone()), "not UTF-8 text"))?;
    let file = crate::load::parse_project(&src, &path)?;
    let id = project_id(&root);
    let name = file.project.name.clone().unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| id.clone())
    });
    let servers = file
        .servers
        .iter()
        .map(|s| resolve(s, &root, &id))
        .collect();
    Ok(Project {
        sha256: sha256_hex(&bytes),
        declared: file.servers,
        root,
        id,
        name,
        servers,
    })
}

/// A declared server as it runs: internal name, its written name (or
/// alias) as the tool prefix, and relative paths under `root`.
fn resolve(s: &ServerSpec, root: &Path, id: &str) -> ServerSpec {
    let mut out = s.clone();
    out.name = internal_name(&s.name, id);
    out.alias = Some(s.prefix().to_string());
    let under = |p: &Path| -> PathBuf {
        if p.is_relative() {
            join_clean(root, p)
        } else {
            p.to_path_buf()
        }
    };
    match &mut out.transport {
        Transport::Stdio { command, cwd, .. } => {
            // `./bin/server` or `tools/server`; a bare `npx` stays a PATH lookup
            if command.contains('/') && Path::new(command.as_str()).is_relative() {
                *command = join_clean(root, Path::new(command.as_str()))
                    .to_string_lossy()
                    .into_owned();
            }
            *cwd = Some(
                cwd.as_deref()
                    .map(under)
                    .unwrap_or_else(|| root.to_path_buf()),
            );
        }
        Transport::Wasm {
            path, allow_dirs, ..
        } => {
            *path = under(path);
            for d in allow_dirs {
                d.path = under(&d.path);
            }
        }
        Transport::OpenApi { spec, .. } => {
            if !spec.starts_with("http://") && !spec.starts_with("https://") {
                *spec = under(Path::new(spec.as_str()))
                    .to_string_lossy()
                    .into_owned();
            }
        }
        Transport::Http { .. } | Transport::Container { .. } => {}
    }
    out
}

/// `root` joined with relative `p`, without its `.` parts
/// (`./bin/server` → `<root>/bin/server`).
fn join_clean(root: &Path, p: &Path) -> PathBuf {
    let mut out = root.to_path_buf();
    for c in p.components() {
        if c != std::path::Component::CurDir {
            out.push(c);
        }
    }
    out
}

/// What trusting `project` lets run: per server, its name and what it runs
/// (the command or URL, and the vault secrets it gets).
pub fn describe(project: &Project) -> Vec<(String, String)> {
    project
        .servers
        .iter()
        .zip(&project.declared)
        .map(|(s, d)| {
            let what = match &s.transport {
                Transport::Stdio { command, args, .. } => {
                    let mut c = vec![command.clone()];
                    c.extend(args.iter().cloned());
                    format!("runs {}", c.join(" "))
                }
                Transport::Http { url, .. } => format!("connects to {url}"),
                Transport::Container { image, .. } => format!("runs container {image}"),
                Transport::Wasm { path, .. } => format!("runs Wasm {}", path.display()),
                Transport::OpenApi { spec, .. } => format!("calls the API in {spec}"),
            };
            let secrets = s.secret_refs();
            let secrets = if secrets.is_empty() {
                String::new()
            } else {
                let mut v: Vec<_> = secrets.into_iter().collect();
                v.sort();
                format!("; secrets: {}", v.join(", "))
            };
            (d.name.clone(), format!("{what}{secrets}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mcpgw-proj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub/deeper")).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn loads_resolves_and_names_servers() {
        let d = dir("load");
        std::fs::write(
            d.join(PROJECT_FILE),
            r#"
[project]
name = "shop"

[[server]]
name = "db"
transport = "stdio"
command = "./bin/db-server"
args = ["--readonly"]
secrets = { DATABASE_URL = "shop_db" }

[[server]]
name = "npx-one"
transport = "stdio"
command = "npx"
cwd = "sub"

[[server]]
name = "api"
transport = "openapi"
spec = "openapi.yaml"
"#,
        )
        .unwrap();
        let p = load(&d.join("sub/..")).unwrap();
        assert_eq!(p.root, d);
        assert_eq!(p.name, "shop");
        assert_eq!(p.id, project_id(&d));
        assert_eq!(p.id.len(), 8);
        assert_eq!(
            p.sha256,
            sha256_hex(&std::fs::read(d.join(PROJECT_FILE)).unwrap())
        );
        let db = &p.servers[0];
        assert_eq!(db.name, format!("db-p{}", p.id));
        assert_eq!(db.prefix(), "db");
        match &db.transport {
            Transport::Stdio { command, cwd, .. } => {
                assert_eq!(command, &d.join("bin/db-server").to_string_lossy());
                assert_eq!(cwd.as_deref(), Some(d.as_path()));
            }
            t => panic!("{t:?}"),
        }
        match &p.servers[1].transport {
            Transport::Stdio { command, cwd, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(cwd.as_deref(), Some(d.join("sub").as_path()));
            }
            t => panic!("{t:?}"),
        }
        match &p.servers[2].transport {
            Transport::OpenApi { spec, .. } => {
                assert_eq!(spec, &d.join("openapi.yaml").to_string_lossy())
            }
            t => panic!("{t:?}"),
        }
        let lines = describe(&p);
        assert_eq!(
            lines[0],
            (
                "db".to_string(),
                format!(
                    "runs {} --readonly; secrets: shop_db",
                    d.join("bin/db-server").display()
                )
            )
        );
        assert!(
            lines[2].1.starts_with("calls the API in "),
            "{:?}",
            lines[2]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_the_nearest_root() {
        let d = dir("find");
        assert_eq!(find_root(&d.join("sub/deeper")), None);
        std::fs::write(d.join(PROJECT_FILE), "").unwrap();
        assert_eq!(
            find_root(&d.join("sub/deeper")).as_deref(),
            Some(d.as_path())
        );
        std::fs::write(d.join("sub").join(PROJECT_FILE), "").unwrap();
        assert_eq!(find_root(&d.join("sub/deeper")), Some(d.join("sub")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn errors_point_at_the_problem() {
        let d = dir("err");
        std::fs::write(
            d.join(PROJECT_FILE),
            "[[server]]\nname = \"db\"\ntransport = \"stdio\"\ncomand = \"x\"\n",
        )
        .unwrap();
        let e = load(&d).unwrap_err().to_string();
        assert!(e.contains(".mcpgw.toml:4:1") && e.contains("comand"), "{e}");
        let long = "x".repeat(MAX_NAME + 1);
        std::fs::write(
            d.join(PROJECT_FILE),
            format!("[[server]]\nname = \"{long}\"\ntransport = \"stdio\"\ncommand = \"x\"\n"),
        )
        .unwrap();
        let e = load(&d).unwrap_err().to_string();
        assert!(e.contains("at most 54"), "{e}");
        std::fs::write(d.join(PROJECT_FILE), "[project]\nnmae = \"x\"\n").unwrap();
        assert!(load(&d).unwrap_err().to_string().contains("nmae"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
