//! `mcpgw-store` — persistence trait + local SQLite implementation (ADR-0002).
//!
//! The [`Store`] trait keeps persistence pluggable (a future cloud backend can
//! implement it without changing callers). [`SqliteStore`] is the local
//! implementation. Domain objects are stored as JSON next to their key
//! columns; the schema evolves through numbered [`MIGRATIONS`].

use std::path::Path;
use std::sync::Mutex;

use mcpgw_core::{
    Error, Profile, Result, SecretMeta, ServerSpec, ToolManifest, validate_secret_name,
};
use rusqlite::{Connection, OptionalExtension, params};

/// Schema steps, applied in order; step `i` brings `user_version` to `i + 1`.
/// Never edit a released step: append a new one.
pub const MIGRATIONS: &[&str] = &[
    // 1: key/value and the server registry
    "CREATE TABLE IF NOT EXISTS kv (
         key   TEXT PRIMARY KEY NOT NULL,
         value BLOB NOT NULL
     );
     CREATE TABLE IF NOT EXISTS servers (
         name   TEXT PRIMARY KEY NOT NULL,
         config TEXT NOT NULL
     );",
    // 2: tool manifests, profiles, secret metadata
    "CREATE TABLE manifests (
         server      TEXT PRIMARY KEY NOT NULL,
         sha256      TEXT NOT NULL,
         captured_at INTEGER NOT NULL,
         manifest    TEXT NOT NULL
     );
     CREATE TABLE profiles (
         name    TEXT PRIMARY KEY NOT NULL,
         profile TEXT NOT NULL
     );
     CREATE TABLE secrets (
         name       TEXT PRIMARY KEY NOT NULL,
         updated_at INTEGER NOT NULL,
         meta       TEXT NOT NULL
     );",
];

/// Current schema version.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Pluggable persistence backend for gateway state.
pub trait Store: Send + Sync {
    /// Persist a raw value under `key`.
    fn put(&self, key: &str, value: &[u8]) -> Result<()>;
    /// Load a previously persisted value for `key`, if present.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Insert or replace a backend server (keyed by name).
    fn upsert_server(&self, server: &ServerSpec) -> Result<()>;
    /// Remove a server; returns whether it existed.
    fn remove_server(&self, name: &str) -> Result<bool>;
    /// Look up one server.
    fn server(&self, name: &str) -> Result<Option<ServerSpec>>;
    /// All registered servers, ordered by name.
    fn servers(&self) -> Result<Vec<ServerSpec>>;

    /// Insert or replace a server's tool manifest.
    fn put_manifest(&self, manifest: &ToolManifest) -> Result<()>;
    /// A server's manifest.
    fn manifest(&self, server: &str) -> Result<Option<ToolManifest>>;
    /// All manifests, ordered by server.
    fn manifests(&self) -> Result<Vec<ToolManifest>>;
    /// Remove a server's manifest; returns whether it existed.
    fn remove_manifest(&self, server: &str) -> Result<bool>;

    /// Insert or replace a profile.
    fn upsert_profile(&self, profile: &Profile) -> Result<()>;
    /// Remove a profile; returns whether it existed.
    fn remove_profile(&self, name: &str) -> Result<bool>;
    /// Look up one profile.
    fn profile(&self, name: &str) -> Result<Option<Profile>>;
    /// All profiles, ordered by name.
    fn profiles(&self) -> Result<Vec<Profile>>;

    /// Insert or replace secret metadata.
    fn upsert_secret_meta(&self, meta: &SecretMeta) -> Result<()>;
    /// Remove secret metadata; returns whether it existed.
    fn remove_secret_meta(&self, name: &str) -> Result<bool>;
    /// Look up one secret's metadata.
    fn secret_meta(&self, name: &str) -> Result<Option<SecretMeta>>;
    /// All secret metadata, ordered by name.
    fn secret_metas(&self) -> Result<Vec<SecretMeta>>;
}

fn db_err(e: impl std::fmt::Display) -> Error {
    Error::Store(e.to_string())
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(db_err)
}

fn from_json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(db_err)
}

/// SQLite-backed [`Store`].
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (creating if needed) a database file, including parent dirs, and
    /// run pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        // WAL lets CLI readers run while the daemon writes.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(db_err)?;
        Self::init(conn)
    }

    /// Open a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The schema version of the open database.
    pub fn schema_version(&self) -> Result<i64> {
        self.with(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("store mutex poisoned".into()))?;
        f(&conn).map_err(db_err)
    }

    fn put_json(&self, sql: &str, key: &str, extra: Option<i64>, json: String) -> Result<()> {
        self.with(|c| match extra {
            Some(x) => c.execute(sql, params![key, x, json]).map(drop),
            None => c.execute(sql, params![key, json]).map(drop),
        })
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, sql: &str, key: &str) -> Result<Option<T>> {
        let json: Option<String> =
            self.with(|c| c.query_row(sql, [key], |r| r.get(0)).optional())?;
        json.map(|j| from_json(&j)).transpose()
    }

    fn all_json<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let rows: Vec<String> = self.with(|c| {
            let mut st = c.prepare(sql)?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        rows.iter().map(|j| from_json(j)).collect()
    }

    fn delete(&self, sql: &str, key: &str) -> Result<bool> {
        self.with(|c| c.execute(sql, [key])).map(|n| n > 0)
    }
}

/// Apply every pending migration, each in its own transaction.
fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Store(format!(
            "database schema {version} is newer than supported {SCHEMA_VERSION}; upgrade mcpgw"
        )));
    }
    for (i, step) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute_batch(step).map_err(db_err)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
    }
    Ok(())
}

impl Store for SqliteStore {
    fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO kv(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(drop)
        })
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.with(|c| {
            c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    fn upsert_server(&self, server: &ServerSpec) -> Result<()> {
        server.validate()?;
        self.put_json(
            "INSERT INTO servers(name, config) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET config = excluded.config",
            &server.name,
            None,
            to_json(server)?,
        )
    }

    fn remove_server(&self, name: &str) -> Result<bool> {
        self.delete("DELETE FROM servers WHERE name = ?1", name)
    }

    fn server(&self, name: &str) -> Result<Option<ServerSpec>> {
        self.get_json("SELECT config FROM servers WHERE name = ?1", name)
    }

    fn servers(&self) -> Result<Vec<ServerSpec>> {
        self.all_json("SELECT config FROM servers ORDER BY name")
    }

    fn put_manifest(&self, m: &ToolManifest) -> Result<()> {
        let json = to_json(m)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO manifests(server, sha256, captured_at, manifest)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(server) DO UPDATE SET sha256 = excluded.sha256,
                     captured_at = excluded.captured_at, manifest = excluded.manifest",
                params![m.server, m.sha256, m.captured_at as i64, json],
            )
            .map(drop)
        })
    }

    fn manifest(&self, server: &str) -> Result<Option<ToolManifest>> {
        self.get_json("SELECT manifest FROM manifests WHERE server = ?1", server)
    }

    fn manifests(&self) -> Result<Vec<ToolManifest>> {
        self.all_json("SELECT manifest FROM manifests ORDER BY server")
    }

    fn remove_manifest(&self, server: &str) -> Result<bool> {
        self.delete("DELETE FROM manifests WHERE server = ?1", server)
    }

    fn upsert_profile(&self, p: &Profile) -> Result<()> {
        mcpgw_core::validate_server_name(&p.name)
            .map_err(|_| Error::Config(format!("invalid profile name {:?}", p.name)))?;
        self.put_json(
            "INSERT INTO profiles(name, profile) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET profile = excluded.profile",
            &p.name,
            None,
            to_json(p)?,
        )
    }

    fn remove_profile(&self, name: &str) -> Result<bool> {
        self.delete("DELETE FROM profiles WHERE name = ?1", name)
    }

    fn profile(&self, name: &str) -> Result<Option<Profile>> {
        self.get_json("SELECT profile FROM profiles WHERE name = ?1", name)
    }

    fn profiles(&self) -> Result<Vec<Profile>> {
        self.all_json("SELECT profile FROM profiles ORDER BY name")
    }

    fn upsert_secret_meta(&self, m: &SecretMeta) -> Result<()> {
        validate_secret_name(&m.name)?;
        self.put_json(
            "INSERT INTO secrets(name, updated_at, meta) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET updated_at = excluded.updated_at, meta = excluded.meta",
            &m.name,
            Some(m.updated_at as i64),
            to_json(m)?,
        )
    }

    fn remove_secret_meta(&self, name: &str) -> Result<bool> {
        self.delete("DELETE FROM secrets WHERE name = ?1", name)
    }

    fn secret_meta(&self, name: &str) -> Result<Option<SecretMeta>> {
        self.get_json("SELECT meta FROM secrets WHERE name = ?1", name)
    }

    fn secret_metas(&self) -> Result<Vec<SecretMeta>> {
        self.all_json("SELECT meta FROM secrets ORDER BY name")
    }
}

/// Merge config-file servers with store-registered servers.
///
/// Config-file entries win on name clashes (the file is the operator's
/// explicit source of truth); the result is ordered by name.
pub fn effective_servers(config: &[ServerSpec], store: &dyn Store) -> Result<Vec<ServerSpec>> {
    let mut out: Vec<ServerSpec> = config.to_vec();
    for s in store.servers()? {
        if !out.iter().any(|c| c.name == s.name) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcpgw_core::{ToolDef, Transport};

    fn stdio(name: &str, cmd: &str) -> ServerSpec {
        ServerSpec::new(
            name,
            Transport::Stdio {
                command: cmd.into(),
                args: vec!["-x".into()],
                env: [("K".to_string(), "V".to_string())].into(),
                cwd: None,
            },
        )
    }

    fn manifest(server: &str, desc: &str) -> ToolManifest {
        let t = ToolDef::from_json(serde_json::json!({"name": "t", "description": desc})).unwrap();
        ToolManifest::new(server, Some("2026-07-28".into()), 100, vec![t])
    }

    fn secret(name: &str, at: u64) -> SecretMeta {
        SecretMeta {
            name: name.into(),
            description: None,
            created_at: 1,
            updated_at: at,
            last_used_at: None,
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpgw-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn kv_roundtrip() {
        let s = SqliteStore::open_in_memory().unwrap();
        assert_eq!(s.get("a").unwrap(), None);
        s.put("a", b"1").unwrap();
        s.put("a", b"2").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn server_crud() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.upsert_server(&stdio("b", "one")).unwrap();
        s.upsert_server(&stdio("a", "two")).unwrap();
        s.upsert_server(&stdio("b", "three")).unwrap();
        let all = s.servers().unwrap();
        assert_eq!(
            all.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(s.server("b").unwrap().unwrap(), stdio("b", "three"));
        assert!(s.remove_server("a").unwrap());
        assert!(!s.remove_server("a").unwrap());
        assert_eq!(s.server("a").unwrap(), None);
    }

    #[test]
    fn rejects_invalid_records() {
        let s = SqliteStore::open_in_memory().unwrap();
        assert!(s.upsert_server(&stdio("a__b", "x")).is_err());
        assert!(s.upsert_server(&stdio("a", " ")).is_err());
        assert!(s.upsert_secret_meta(&secret("bad name", 1)).is_err());
        let p = Profile {
            name: "no spaces".into(),
            ..toml::from_str("name = \"x\"").unwrap()
        };
        assert!(s.upsert_profile(&p).is_err());
        assert!(s.servers().unwrap().is_empty());
    }

    #[test]
    fn manifests_profiles_secrets() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.put_manifest(&manifest("fs", "v1")).unwrap();
        s.put_manifest(&manifest("fs", "v2")).unwrap();
        s.put_manifest(&manifest("db", "v1")).unwrap();
        let m = s.manifest("fs").unwrap().unwrap();
        assert_eq!(m.tools[0].description.as_deref(), Some("v2"));
        assert!(m.verify());
        assert_eq!(s.manifests().unwrap().len(), 2);
        assert!(s.remove_manifest("db").unwrap());

        let p: Profile = toml::from_str("name = \"frontend\"\nservers = [\"fs\"]").unwrap();
        s.upsert_profile(&p).unwrap();
        assert_eq!(s.profile("frontend").unwrap().unwrap(), p);
        assert_eq!(s.profiles().unwrap().len(), 1);
        assert!(s.remove_profile("frontend").unwrap());

        s.upsert_secret_meta(&secret("github_token", 5)).unwrap();
        s.upsert_secret_meta(&secret("github_token", 9)).unwrap();
        assert_eq!(
            s.secret_meta("github_token").unwrap().unwrap().updated_at,
            9
        );
        assert_eq!(s.secret_metas().unwrap().len(), 1);
        assert!(s.remove_secret_meta("github_token").unwrap());
        assert!(s.secret_metas().unwrap().is_empty());
    }

    #[test]
    fn survives_restart_uses_wal_and_rejects_future_schema() {
        let dir = temp("reopen");
        let path = dir.join("nested/db.sqlite");
        {
            let s = SqliteStore::open(&path).unwrap();
            s.upsert_server(&stdio("fs", "mcp-fs")).unwrap();
            s.put_manifest(&manifest("fs", "v1")).unwrap();
            s.upsert_secret_meta(&secret("k", 1)).unwrap();
            assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        }
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.servers().unwrap().len(), 1);
        assert!(s.manifest("fs").unwrap().is_some());
        assert!(s.secret_meta("k").unwrap().is_some());
        let mode: String = s
            .with(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(s);

        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(c);
        assert!(matches!(SqliteStore::open(&path), Err(Error::Store(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_a_version_1_database() {
        let dir = temp("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        {
            // What a v0.1 binary (before M1-03) left behind.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch("PRAGMA user_version = 1").unwrap();
            c.execute(
                "INSERT INTO servers(name, config) VALUES ('old', ?1)",
                [serde_json::to_string(&stdio("old", "x")).unwrap()],
            )
            .unwrap();
        }
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(s.server("old").unwrap().unwrap(), stdio("old", "x"));
        s.put_manifest(&manifest("old", "v1")).unwrap();
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_wins_on_merge() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.upsert_server(&stdio("fs", "from-store")).unwrap();
        s.upsert_server(&stdio("db", "from-store")).unwrap();
        let merged = effective_servers(&[stdio("fs", "from-config")], &s).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "db");
        assert_eq!(merged[1], stdio("fs", "from-config"));
    }
}
