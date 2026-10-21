//! Project endpoints: the servers of a repository's `.mcpgw.toml`, served
//! at `/mcp/project/<id>` with the global ones.
//!
//! `mcpgw connect` registers the project it was started in
//! (`POST /api/v1/projects`) and relays to its endpoint. A project's servers
//! run only while its file's content is trusted: trusting records the
//! file's SHA-256 for its path, and any other content (a new file, an edit,
//! a different checkout) turns them off again until it's trusted. While
//! they're off the endpoint lists a `project_not_trusted` tool saying so.
//!
//! The file is checked again (a `stat`, and a read when that changed) on
//! every request to the endpoint, so edits apply without a restart.
//! Trusted projects are registered when the daemon starts.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, Weak},
    time::SystemTime,
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use mcpgw_config::project::{self, PROJECT_FILE};
use mcpgw_core::ServerSpec;
use mcpgw_runtime::BackendManager;
use mcpgw_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a project endpoint is built from.
#[derive(Clone)]
pub struct ProjectEndpoint {
    /// The project's id (its servers' owner in the manager).
    pub id: String,
    /// `project <name>`, for messages and the call log.
    pub label: String,
    /// The `project_not_trusted` notice while its servers are off.
    pub notice: mcpgw_router::Notice,
}

/// Builds the MCP service of a project endpoint.
pub type ProjectService = Arc<dyn Fn(ProjectEndpoint) -> Router + Send + Sync>;

/// How long turning a project's servers on waits for their tools to be
/// read (the first time only; they're pinned afterwards).
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Key of the trust records in the store's key-value table.
pub const TRUST_KEY: &str = "projects.trusted";

/// A trusted project file: its path's root and the content trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// SHA-256 of the trusted file, hex.
    pub sha256: String,
    /// The project's name when it was trusted.
    pub name: String,
    /// When, Unix seconds.
    pub trusted_at: u64,
    /// Per server (internal name), a SHA-256 of its definition as trusted.
    #[serde(default)]
    pub servers: BTreeMap<String, String>,
}

/// A fingerprint of a server's definition.
fn spec_hash(s: &ServerSpec) -> String {
    project::sha256_hex(&serde_json::to_vec(s).unwrap_or_default())
}

/// Trust `p`'s current content. Servers whose definition changed since the
/// last trust lose their pinned tools (in the store; returned, so a running
/// daemon can drop them too): trusting the new definition is consent to its
/// tools. Unchanged servers keep their pins, so their drift is still caught.
pub fn record_trust(store: &dyn Store, p: &project::Project) -> Result<Vec<String>, String> {
    let old = trusted(store)?.remove(&*p.root.to_string_lossy());
    let servers: BTreeMap<String, String> = p
        .servers
        .iter()
        .map(|s| (s.name.clone(), spec_hash(s)))
        .collect();
    let mut repin = Vec::new();
    if let Some(old) = &old {
        for (name, h) in &servers {
            if old.servers.get(name).is_some_and(|o| o != h) {
                let _ = store.remove_manifest(name);
                repin.push(name.clone());
            }
        }
    }
    set_trusted(
        store,
        &p.root,
        Some(TrustRecord {
            sha256: p.sha256.clone(),
            name: p.name.clone(),
            trusted_at: now(),
            servers,
        }),
    )?;
    Ok(repin)
}

/// Every trust record, by project root.
pub fn trusted(store: &dyn Store) -> Result<BTreeMap<String, TrustRecord>, String> {
    match store.get(TRUST_KEY).map_err(|e| e.to_string())? {
        None => Ok(BTreeMap::new()),
        Some(b) => serde_json::from_slice(&b).map_err(|e| format!("bad trust records: {e}")),
    }
}

/// Record (or with `None`, remove) the trust of the project at `root`.
pub fn set_trusted(
    store: &dyn Store,
    root: &Path,
    record: Option<TrustRecord>,
) -> Result<(), String> {
    let mut all = trusted(store)?;
    let key = root.to_string_lossy().into_owned();
    match record {
        Some(r) => {
            all.insert(key, r);
        }
        None => {
            all.remove(&key);
        }
    }
    let bytes = serde_json::to_vec(&all).map_err(|e| e.to_string())?;
    store.put(TRUST_KEY, &bytes).map_err(|e| e.to_string())
}

/// Where a project stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    /// Its file's content is trusted: its servers run.
    Trusted,
    /// Never trusted: its servers are off.
    Untrusted,
    /// Trusted once, but the file changed since: its servers are off.
    Changed,
    /// The file can't be read or has errors: its servers are off.
    Invalid,
    /// The file is gone: nothing to run.
    Missing,
}

#[derive(Debug, Clone)]
struct Entry {
    root: PathBuf,
    id: String,
    name: String,
    state: ProjectState,
    error: Option<String>,
    sha256: Option<String>,
    trusted_sha256: Option<String>,
    /// `(written name, internal name, what it runs)` per server.
    servers: Vec<(String, String, String)>,
    /// The file's modification time and length when last read.
    stamp: Option<(SystemTime, u64)>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn stamp(root: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(root.join(PROJECT_FILE)).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The daemon's projects and their endpoints.
pub struct Projects {
    me: Weak<Projects>,
    manager: Arc<BackendManager>,
    store: Arc<dyn Store>,
    list: RwLock<BTreeMap<String, Entry>>,
    cache: Mutex<HashMap<String, Router>>,
    build: ProjectService,
    audit: Option<mcpgw_core::audit::Auditor>,
    /// The MCP base URL (`http://127.0.0.1:7777/mcp`).
    mcp_url: String,
    /// One reload at a time: they add and remove servers.
    applying: tokio::sync::Mutex<()>,
}

impl Projects {
    /// An empty registry; [`Projects::start`] registers the trusted ones.
    pub fn new(
        manager: Arc<BackendManager>,
        store: Arc<dyn Store>,
        build: ProjectService,
        audit: Option<mcpgw_core::audit::Auditor>,
        mcp_url: String,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            manager,
            store,
            list: RwLock::new(BTreeMap::new()),
            cache: Mutex::new(HashMap::new()),
            build,
            audit,
            mcp_url,
            applying: tokio::sync::Mutex::new(()),
        })
    }

    /// Register every trusted project whose file is still there.
    pub async fn start(&self) {
        let records = match trusted(&*self.store) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read trusted projects");
                return;
            }
        };
        for root in records.keys() {
            let root = PathBuf::from(root);
            if root.join(PROJECT_FILE).is_file()
                && let Err(e) = self.register(&root).await
            {
                tracing::warn!(root = %root.display(), error = %e, "cannot register project");
            }
        }
    }

    fn record(&self, kind: &str, subject: &str, detail: Value) {
        if let Some(a) = &self.audit {
            a(mcpgw_core::AuditEvent::new(kind, "api", subject, detail));
        }
    }

    /// Register the project at `root` (or read it again) and describe it.
    pub async fn register(&self, root: &Path) -> Result<Value, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        if !root.join(PROJECT_FILE).is_file() {
            return Err(format!("no {PROJECT_FILE} in {}", root.display()));
        }
        let id = self.reload(&root).await;
        self.get(&id).ok_or_else(|| "project vanished".into())
    }

    /// Read the project at `root` again and run what its trust allows.
    /// Returns its id.
    async fn reload(&self, root: &Path) -> String {
        let _one = self.applying.lock().await;
        let id = project::project_id(root);
        let st = stamp(root);
        let trust = trusted(&*self.store)
            .ok()
            .and_then(|t| t.get(&*root.to_string_lossy()).cloned());
        let (entry, desired) = match project::load(root) {
            Ok(p) => {
                let state = match &trust {
                    Some(t) if t.sha256 == p.sha256 => ProjectState::Trusted,
                    Some(_) => ProjectState::Changed,
                    None => ProjectState::Untrusted,
                };
                let lines = project::describe(&p);
                let servers = p
                    .declared
                    .iter()
                    .zip(&p.servers)
                    .zip(lines)
                    .map(|((d, s), (_, l))| (d.name.clone(), s.name.clone(), l))
                    .collect();
                let desired = if state == ProjectState::Trusted {
                    p.servers.clone()
                } else {
                    Vec::new()
                };
                let e = Entry {
                    root: root.to_path_buf(),
                    id: id.clone(),
                    name: p.name,
                    state,
                    error: None,
                    sha256: Some(p.sha256),
                    trusted_sha256: trust.map(|t| t.sha256),
                    servers,
                    stamp: st,
                };
                (e, desired)
            }
            Err(err) => {
                let missing = !root.join(PROJECT_FILE).exists();
                let e = Entry {
                    root: root.to_path_buf(),
                    id: id.clone(),
                    name: trust.as_ref().map(|t| t.name.clone()).unwrap_or_else(|| {
                        root.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| id.clone())
                    }),
                    state: if missing {
                        ProjectState::Missing
                    } else {
                        ProjectState::Invalid
                    },
                    error: (!missing).then(|| err.to_string()),
                    sha256: None,
                    trusted_sha256: trust.map(|t| t.sha256),
                    servers: Vec::new(),
                    stamp: st,
                };
                (e, Vec::new())
            }
        };
        let before = self
            .list
            .read()
            .expect("projects")
            .get(&id)
            .map(|e| e.state);
        if before != Some(entry.state) {
            tracing::info!(project = %entry.name, root = %root.display(), state = ?entry.state, "project");
        }
        self.list
            .write()
            .expect("projects")
            .insert(id.clone(), entry);
        self.apply(&id, desired).await;
        id
    }

    /// Make the manager run exactly `desired` for project `id`.
    async fn apply(&self, id: &str, desired: Vec<ServerSpec>) {
        let mut changed = false;
        for name in self.manager.project_servers(id) {
            let keep = desired
                .iter()
                .any(|d| d.name == name && self.manager.spec(&name).as_ref() == Some(d));
            if !keep {
                if let Err(e) = self.manager.remove_server(&name).await {
                    tracing::warn!(server = %name, error = %e, "cannot stop project server");
                }
                changed = true;
            }
        }
        let mut capture = Vec::new();
        for spec in desired {
            if self.manager.spec(&spec.name).is_none() {
                match self.manager.add_project_server(&spec, id) {
                    Ok(()) if spec.enabled && !self.manager.catalog().contains(&spec.name) => {
                        capture.push(spec.name.clone())
                    }
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!(server = %spec.name, error = %e, "cannot add project server")
                    }
                }
                changed = true;
            }
        }
        // like the config's servers at startup: start and stop each one never
        // seen, so its tools are pinned and can be listed
        let started: Vec<_> = capture
            .into_iter()
            .map(|name| {
                let mgr = self.manager.clone();
                tokio::spawn(async move {
                    if mgr.ensure(&name).await.is_ok() {
                        let _ = mgr.stop(&name).await;
                    }
                })
            })
            .collect();
        let _ = tokio::time::timeout(CAPTURE_TIMEOUT, async {
            for t in started {
                let _ = t.await;
            }
        })
        .await;
        if changed {
            self.manager.catalog().bump();
        }
    }

    /// Read the file again if it changed on disk since it was last read.
    pub async fn refresh(&self, id: &str) {
        let Some((root, old)) = self
            .list
            .read()
            .expect("projects")
            .get(id)
            .map(|e| (e.root.clone(), e.stamp))
        else {
            return;
        };
        if stamp(&root) != old {
            self.reload(&root).await;
        }
    }

    /// Trust the project's current file. With `sha256`, only if that's
    /// still its content (what the user was shown).
    pub async fn trust(&self, id: &str, sha256: Option<&str>) -> Result<Value, String> {
        self.refresh(id).await;
        let e = self
            .list
            .read()
            .expect("projects")
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no project {id:?}"))?;
        let Some(current) = e.sha256.clone() else {
            return Err(match e.error {
                Some(err) => format!("its {PROJECT_FILE} has errors: {err}"),
                None => format!("{} has no {PROJECT_FILE}", e.root.display()),
            });
        };
        if let Some(want) = sha256
            && want != current
        {
            return Err(format!(
                "{PROJECT_FILE} changed since it was shown (now {}); look at it again",
                &current[..12]
            ));
        }
        let p = project::load(&e.root).map_err(|err| err.to_string())?;
        if p.sha256 != current {
            return Err(format!(
                "{PROJECT_FILE} changed while being trusted; look at it again"
            ));
        }
        for name in record_trust(&*self.store, &p)? {
            self.manager.catalog().remove(&name);
        }
        self.record(
            "project.trust",
            &e.root.to_string_lossy(),
            json!({"sha256": current, "servers": e.servers.iter().map(|s| &s.0).collect::<Vec<_>>()}),
        );
        self.reload(&e.root).await;
        self.get(id).ok_or_else(|| "project vanished".into())
    }

    /// Stop trusting the project: its servers stop.
    pub async fn untrust(&self, id: &str) -> Result<Value, String> {
        let root = self.root(id)?;
        set_trusted(&*self.store, &root, None)?;
        self.record("project.untrust", &root.to_string_lossy(), json!({}));
        self.reload(&root).await;
        self.get(id).ok_or_else(|| "project vanished".into())
    }

    /// Untrust and forget the project; returns whether it was known.
    pub async fn forget(&self, id: &str) -> Result<bool, String> {
        let Ok(root) = self.root(id) else {
            return Ok(false);
        };
        set_trusted(&*self.store, &root, None)?;
        {
            let _one = self.applying.lock().await;
            self.apply(id, Vec::new()).await;
            self.list.write().expect("projects").remove(id);
        }
        self.cache.lock().expect("project cache").remove(id);
        self.record("project.forget", &root.to_string_lossy(), json!({}));
        Ok(true)
    }

    fn root(&self, id: &str) -> Result<PathBuf, String> {
        self.list
            .read()
            .expect("projects")
            .get(id)
            .map(|e| e.root.clone())
            .ok_or_else(|| format!("no project {id:?}"))
    }

    /// The endpoint URL of project `id`.
    pub fn url(&self, id: &str) -> String {
        format!("{}/project/{id}", self.mcp_url)
    }

    fn json(&self, e: &Entry) -> Value {
        let servers: Vec<Value> = e
            .servers
            .iter()
            .map(|(name, internal, runs)| {
                let st = self.manager.status_of(internal);
                json!({
                    "name": name,
                    "server": internal,
                    "runs": runs,
                    "state": st.as_ref().map(|s| s.state.name()),
                    "tools": self.manager.catalog().get(internal).map(|m| m.tools.len()).unwrap_or(0),
                })
            })
            .collect();
        json!({
            "id": e.id,
            "name": e.name,
            "root": e.root,
            "state": e.state,
            "error": e.error,
            "sha256": e.sha256,
            "trusted_sha256": e.trusted_sha256,
            "url": self.url(&e.id),
            "servers": servers,
        })
    }

    /// One project, described.
    pub fn get(&self, id: &str) -> Option<Value> {
        let e = self.list.read().expect("projects").get(id).cloned()?;
        Some(self.json(&e))
    }

    /// Every registered project, described.
    pub fn all(&self) -> Vec<Value> {
        let list: Vec<Entry> = self
            .list
            .read()
            .expect("projects")
            .values()
            .cloned()
            .collect();
        list.iter().map(|e| self.json(e)).collect()
    }

    /// What the endpoint tells clients while the servers are off.
    fn notice(&self, id: &str) -> Option<String> {
        let e = self.list.read().expect("projects").get(id).cloned()?;
        let root = e.root.display();
        let n = e.servers.len();
        let how = format!(
            "run `mcpgw project trust` in {root}, or approve it in the mcpgw app's Projects"
        );
        match e.state {
            ProjectState::Trusted | ProjectState::Missing => None,
            ProjectState::Untrusted if n == 0 => None,
            ProjectState::Untrusted => Some(format!(
                "This project's {PROJECT_FILE} ({root}) lists {n} server(s) that stay off until \
                 the user trusts it: {how}."
            )),
            ProjectState::Changed => Some(format!(
                "This project's {PROJECT_FILE} ({root}) changed since it was trusted, so its \
                 servers are off until it's trusted again: {how}."
            )),
            ProjectState::Invalid => Some(format!(
                "This project's {PROJECT_FILE} ({root}) has errors, so its servers are off: {}",
                e.error.unwrap_or_default()
            )),
        }
    }

    /// What project `id`'s endpoint is built from: its scope, label and
    /// notice. The API's tool list and inspector use it to act as that
    /// endpoint does.
    pub fn endpoint(&self, id: &str) -> Option<ProjectEndpoint> {
        let name = self.list.read().expect("projects").get(id)?.name.clone();
        let me = self.me.clone();
        let pid = id.to_string();
        Some(ProjectEndpoint {
            id: id.to_string(),
            label: format!("project {name}"),
            notice: Arc::new(move || me.upgrade().and_then(|p| p.notice(&pid))),
        })
    }

    fn service(&self, id: &str) -> Option<Router> {
        let ep = self.endpoint(id)?;
        let mut cache = self.cache.lock().expect("project cache");
        Some(
            cache
                .entry(id.to_string())
                .or_insert_with(|| (self.build)(ep))
                .clone(),
        )
    }

    /// Routes `/project/{id}` (and below) to the project's MCP service;
    /// mount inside the `/mcp` endpoint.
    pub fn routes(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/project/{id}", any(dispatch))
            .route("/project/{id}/{*rest}", any(dispatch))
            .with_state(self.clone())
    }
}

async fn dispatch(State(p): State<Arc<Projects>>, req: Request) -> Response {
    let id = req
        .uri()
        .path()
        .split('/')
        .skip_while(|s| *s != "project")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    p.refresh(&id).await;
    match p.service(&id) {
        Some(svc) => {
            use tower::ServiceExt;
            match svc.oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no project {id:?}; `mcpgw connect` registers the project it runs in")})),
        )
            .into_response(),
    }
}
