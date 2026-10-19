//! `mcpgw-testkit` — fake MCP servers for integration tests.
//!
//! [`fake_mcp`] locates the `fake-mcp` binary (building it on first use),
//! [`stdio`] makes a `ServerSpec` that launches it, and [`HttpFake`] runs
//! it as a Streamable HTTP server. See `src/bin/fake-mcp.rs` for the knobs.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::OnceLock,
};

use mcpgw_core::{ServerSpec, Transport};

/// Path to the `fake-mcp` binary, built once per test process.
///
/// Tests live in `target/<profile>/deps/`, so the binary sits one level up.
pub fn fake_mcp() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().expect("current exe");
        let dir = exe
            .parent()
            .and_then(|d| {
                if d.ends_with("deps") {
                    d.parent()
                } else {
                    Some(d)
                }
            })
            .expect("target dir")
            .to_path_buf();
        let bin = dir.join(format!("fake-mcp{}", std::env::consts::EXE_SUFFIX));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args([
            "build",
            "--quiet",
            "--package",
            "mcpgw-testkit",
            "--bin",
            "fake-mcp",
        ]);
        if dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        // Cross builds put artifacts in target/<triple>/<profile>.
        let triple = dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| n.matches('-').count() >= 2);
        if let Some(triple) = triple {
            cmd.args(["--target", triple]);
        }
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        assert!(ok || bin.exists(), "could not build fake-mcp");
        bin
    })
    .clone()
}

/// A stdio `ServerSpec` named `name` that launches `fake-mcp` with `env`.
pub fn stdio(name: &str, env: &[(&str, &str)]) -> ServerSpec {
    ServerSpec::new(
        name,
        Transport::Stdio {
            command: fake_mcp().to_string_lossy().into_owned(),
            args: vec![],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            cwd: None,
        },
    )
}

/// A `fake-mcp` serving Streamable HTTP on a random loopback port.
/// The process is killed on drop.
pub struct HttpFake {
    child: Child,
    /// Endpoint URL, e.g. `http://127.0.0.1:53211/mcp`.
    pub url: String,
}

impl HttpFake {
    /// Start one with the given environment.
    pub fn start(env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(fake_mcp())
            .args(["--http", "127.0.0.1:0"])
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fake-mcp");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("read listening line");
        let url = line
            .trim()
            .strip_prefix("listening ")
            .unwrap_or_else(|| panic!("unexpected fake-mcp output: {line:?}"))
            .to_string();
        Self { child, url }
    }

    /// Process id of the server.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for HttpFake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
