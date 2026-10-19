//! Finding, starting and describing the local daemon (ADR-0004).
//!
//! A running daemon writes `daemon.json` (next to the store) with its pid and
//! URL, and removes it on clean shutdown. `mcpgw connect` trusts the file
//! only if `/healthz` answers; otherwise it starts a new daemon.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// Contents of `daemon.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// Process id.
    pub pid: u32,
    /// MCP endpoint, e.g. `http://127.0.0.1:7777/mcp`.
    pub url: String,
    /// mcpgw version.
    pub version: String,
    /// Start time, Unix seconds.
    pub started_at: u64,
}

impl DaemonInfo {
    /// Info for this process serving at `addr`.
    pub fn current(addr: SocketAddr) -> Self {
        Self {
            pid: std::process::id(),
            url: format!("http://{addr}{}", mcpgw_api::MCP_PATH),
            version: env!("CARGO_PKG_VERSION").into(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// The `host:port` part of `url`.
    pub fn addr(&self) -> Option<SocketAddr> {
        self.url
            .strip_prefix("http://")?
            .split('/')
            .next()?
            .parse()
            .ok()
    }

    /// Write atomically with owner-only permissions.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(tmp, path)
    }

    /// Read the file, if present and well-formed.
    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }
}

/// Path of `daemon.json` for a config: next to the store database.
pub fn info_path(config: &mcpgw_config::Config) -> PathBuf {
    config.gateway.store_path().with_file_name("daemon.json")
}

/// Path of the detached daemon's log.
pub fn log_path(config: &mcpgw_config::Config) -> PathBuf {
    config.gateway.store_path().with_file_name("daemon.log")
}

/// Whether something answering like mcpgw listens at `addr`.
pub fn healthy(addr: SocketAddr) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.take(4096).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    text.starts_with("HTTP/1.1 200") && text.ends_with("ok")
}

/// The running daemon, if `daemon.json` points at a healthy one.
pub fn running(config: &mcpgw_config::Config) -> Option<DaemonInfo> {
    let info = DaemonInfo::read(&info_path(config))?;
    healthy(info.addr()?).then_some(info)
}

/// Start a detached daemon for `config_path` and wait until it is healthy.
pub fn start(
    config: &mcpgw_config::Config,
    config_path: &Path,
    timeout: Duration,
) -> Result<DaemonInfo, String> {
    let log = log_path(config);
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("--config")
        .arg(config_path)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log_file);
    detach(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start daemon: {e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(info) = running(config) {
            return Ok(info);
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Lost a start race to another shim, or failed outright.
            if let Some(info) = running(config) {
                return Ok(info);
            }
            return Err(format!(
                "daemon exited ({status}) before it was ready; see {}",
                log.display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon not ready after {timeout:?}; see {}",
                log.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Keep the daemon out of the caller's terminal session and console.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_roundtrip_and_addr() {
        let dir = std::env::temp_dir().join(format!("mcpgw-dinfo-{}", std::process::id()));
        let p = dir.join("daemon.json");
        let info = DaemonInfo::current("127.0.0.1:7777".parse().unwrap());
        info.write(&p).unwrap();
        assert_eq!(DaemonInfo::read(&p).unwrap(), info);
        assert_eq!(info.url, "http://127.0.0.1:7777/mcp");
        assert_eq!(info.addr().unwrap().port(), 7777);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!healthy("127.0.0.1:1".parse().unwrap()));
    }
}
