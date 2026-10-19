//! `mcpgw-store` — persistence trait + local SQLite implementation.
//!
//! The [`Store`] trait keeps persistence pluggable (a future cloud backend can
//! implement it without changing callers). [`SqliteStore`] is the local
//! implementation: a key/value table plus a registry of backend servers.

use std::path::Path;
use std::sync::Mutex;

use mcpgw_core::{Error, Result, ServerSpec};
use rusqlite::{Connection, OptionalExtension, params};

/// Current schema version, stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 1;

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
}

fn db_err(e: impl std::fmt::Display) -> Error {
    Error::Store(e.to_string())
}

/// SQLite-backed [`Store`].
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (creating if needed) a database file, including parent dirs.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path).map_err(db_err)?)
    }

    /// Open a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(Error::Store(format!(
                "database schema {version} is newer than supported {SCHEMA_VERSION}"
            )));
        }
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS kv (
                     key   TEXT PRIMARY KEY NOT NULL,
                     value BLOB NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS servers (
                     name   TEXT PRIMARY KEY NOT NULL,
                     config TEXT NOT NULL
                 );
                 PRAGMA user_version = 1;
                 COMMIT;",
            )
            .map_err(db_err)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("store mutex poisoned".into()))?;
        f(&conn).map_err(db_err)
    }
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
        let json = serde_json::to_string(server).map_err(db_err)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO servers(name, config) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET config = excluded.config",
                params![server.name, json],
            )
            .map(drop)
        })
    }

    fn remove_server(&self, name: &str) -> Result<bool> {
        self.with(|c| c.execute("DELETE FROM servers WHERE name = ?1", [name]))
            .map(|n| n > 0)
    }

    fn server(&self, name: &str) -> Result<Option<ServerSpec>> {
        let json: Option<String> = self.with(|c| {
            c.query_row("SELECT config FROM servers WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()
        })?;
        json.map(|j| serde_json::from_str(&j).map_err(db_err))
            .transpose()
    }

    fn servers(&self) -> Result<Vec<ServerSpec>> {
        let rows: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT config FROM servers ORDER BY name")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        rows.iter()
            .map(|j| serde_json::from_str(j).map_err(db_err))
            .collect()
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
    use mcpgw_core::Transport;

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
    fn rejects_invalid_server() {
        let s = SqliteStore::open_in_memory().unwrap();
        assert!(s.upsert_server(&stdio("a__b", "x")).is_err());
        assert!(s.upsert_server(&stdio("a", " ")).is_err());
        assert!(s.servers().unwrap().is_empty());
    }

    #[test]
    fn persists_across_reopen_and_rejects_future_schema() {
        let dir = std::env::temp_dir().join(format!("mcpgw-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/db.sqlite");
        {
            let s = SqliteStore::open(&path).unwrap();
            s.upsert_server(&stdio("fs", "mcp-fs")).unwrap();
        }
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.servers().unwrap().len(), 1);
        drop(s);

        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(c);
        assert!(matches!(SqliteStore::open(&path), Err(Error::Store(_))));
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
