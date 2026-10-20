//! Profile endpoints, resolved per request (M2-07, M2-11).
//!
//! `/mcp/p/{name}` is served from a registry instead of fixed routes, so
//! profiles created or changed through the API take effect at once. Each
//! profile's MCP service is built on first use and kept until the profile
//! changes (which ends its sessions; clients reconnect).

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, RwLock},
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use mcpgw_core::Profile;
use serde_json::json;

/// Builds the MCP service for a profile.
pub type ProfileService = Arc<dyn Fn(&Profile) -> Router + Send + Sync>;

/// Where a profile is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The config file (read-only through the API).
    Config,
    /// The store (managed through the API).
    Store,
}

impl Source {
    /// `config` or `store`.
    pub fn name(self) -> &'static str {
        match self {
            Source::Config => "config",
            Source::Store => "store",
        }
    }
}

/// The daemon's profiles and their MCP services.
pub struct Profiles {
    list: RwLock<BTreeMap<String, (Profile, Source)>>,
    cache: Mutex<HashMap<String, Router>>,
    build: ProfileService,
}

impl Profiles {
    /// Profiles from the config file and the store (the file wins).
    pub fn new(config: Vec<Profile>, store: Vec<Profile>, build: ProfileService) -> Arc<Self> {
        let mut list = BTreeMap::new();
        for p in store {
            list.insert(p.name.clone(), (p, Source::Store));
        }
        for p in config {
            list.insert(p.name.clone(), (p, Source::Config));
        }
        Arc::new(Self {
            list: RwLock::new(list),
            cache: Mutex::new(HashMap::new()),
            build,
        })
    }

    /// Every profile, by name.
    pub fn all(&self) -> Vec<(Profile, Source)> {
        self.list
            .read()
            .expect("profiles")
            .values()
            .cloned()
            .collect()
    }

    /// One profile.
    pub fn get(&self, name: &str) -> Option<(Profile, Source)> {
        self.list.read().expect("profiles").get(name).cloned()
    }

    /// Add or replace a store profile.
    pub fn put(&self, p: Profile) {
        self.cache.lock().expect("profile cache").remove(&p.name);
        self.list
            .write()
            .expect("profiles")
            .insert(p.name.clone(), (p, Source::Store));
    }

    /// Remove a profile; returns whether it existed.
    pub fn remove(&self, name: &str) -> bool {
        self.cache.lock().expect("profile cache").remove(name);
        self.list.write().expect("profiles").remove(name).is_some()
    }

    fn service(&self, name: &str) -> Option<Router> {
        let (p, _) = self.get(name)?;
        let mut cache = self.cache.lock().expect("profile cache");
        Some(
            cache
                .entry(name.to_string())
                .or_insert_with(|| (self.build)(&p))
                .clone(),
        )
    }

    /// Routes `/p/{name}` (and below) to the profile's MCP service; mount
    /// inside the `/mcp` endpoint.
    pub fn routes(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/p/{name}", any(dispatch))
            .route("/p/{name}/{*rest}", any(dispatch))
            .with_state(self.clone())
    }
}

async fn dispatch(State(p): State<Arc<Profiles>>, req: Request) -> Response {
    let name = req
        .uri()
        .path()
        .split('/')
        .skip_while(|s| *s != "p")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    match p.service(&name) {
        Some(svc) => {
            use tower::ServiceExt;
            match svc.oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no profile named {name:?}")})),
        )
            .into_response(),
    }
}
