//! Container test helpers: a tiny MCP server in Node that runs in a stock
//! `node` image, and Docker probes.
//!
//! Container tests skip when no Linux-container Docker is available, unless
//! `MCPGW_TEST_CONTAINERS=1` (set on the Linux CI jobs), where a missing
//! Docker is a failure.

use std::process::{Command, Stdio};

use mcpgw_core::{ServerSpec, Transport};

/// Image the test server runs in.
pub const IMAGE: &str = "node:22-alpine";

/// An MCP server over stdio (line-delimited JSON-RPC) with tools `echo`,
/// `env` (read a variable), `alloc` (hold `mib` more MiB of touched memory),
/// `write` (create a file at `path`) and `uid`. It logs "node-fake ready" to
/// stderr and exits when stdin closes.
pub const SERVER_JS: &str = r#"
const rl = require('readline').createInterface({ input: process.stdin });
const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
const tools = [
  { name: 'echo', description: 'Echo text back', inputSchema: { type: 'object', properties: { text: { type: 'string' } } } },
  { name: 'env', description: 'Read an environment variable', inputSchema: { type: 'object', properties: { name: { type: 'string' } } } },
  { name: 'alloc', description: 'Hold more memory', inputSchema: { type: 'object', properties: { mib: { type: 'number' } } } },
  { name: 'write', description: 'Create a file', inputSchema: { type: 'object', properties: { path: { type: 'string' } } } },
  { name: 'uid', description: 'The user id', inputSchema: { type: 'object' } },
];
const held = [];
console.error('node-fake ready');
rl.on('line', (l) => {
  let m;
  try { m = JSON.parse(l); } catch { return; }
  if (m.id === undefined) return;
  const ok = (result) => send({ jsonrpc: '2.0', id: m.id, result });
  const err = (code, message) => send({ jsonrpc: '2.0', id: m.id, error: { code, message } });
  const a = (m.params && m.params.arguments) || {};
  switch (m.method) {
    case 'initialize':
      return ok({ protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'node-fake', version: '1.0.0' } });
    case 'tools/list': return ok({ tools });
    case 'tools/call':
      if (m.params.name === 'echo') return ok({ content: [{ type: 'text', text: String(a.text) }] });
      if (m.params.name === 'env') return ok({ content: [{ type: 'text', text: String(process.env[a.name] ?? '') }] });
      if (m.params.name === 'alloc') {
        for (let i = 0; i < a.mib; i++) held.push(Buffer.alloc(1 << 20, 1));
        return ok({ content: [{ type: 'text', text: `holding ${held.length} MiB` }] });
      }
      if (m.params.name === 'write') {
        try { require('fs').writeFileSync(a.path, 'x'); return ok({ content: [{ type: 'text', text: 'written' }] }); }
        catch (e) { return ok({ content: [{ type: 'text', text: e.code || String(e) }] }); }
      }
      if (m.params.name === 'uid') return ok({ content: [{ type: 'text', text: String(process.getuid()) }] });
      return err(-32602, 'unknown tool');
    case 'ping': return ok({});
    default: return err(-32601, 'method not found');
  }
});
rl.on('close', () => process.exit(0));
"#;

/// Whether Docker can run Linux containers here.
pub fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.OSType}}"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "linux")
}

/// Whether to run container tests: true with Docker; false (skip) without,
/// unless `MCPGW_TEST_CONTAINERS=1` demands them.
pub fn containers_or_skip(test: &str) -> bool {
    if docker_available() {
        return true;
    }
    if std::env::var("MCPGW_TEST_CONTAINERS").as_deref() == Ok("1") {
        panic!("{test}: MCPGW_TEST_CONTAINERS=1 but Docker can't run Linux containers");
    }
    eprintln!("{test}: skipped (no Docker for Linux containers)");
    false
}

/// A container server running [`SERVER_JS`] in [`IMAGE`].
pub fn node_server(name: &str) -> ServerSpec {
    ServerSpec::new(
        name,
        Transport::Container {
            image: IMAGE.into(),
            digest: None,
            runtime: Some("docker".into()),
            args: vec!["node".into(), "-e".into(), SERVER_JS.into()],
            env: Default::default(),
            run_args: Vec::new(),
            sandbox: Default::default(),
        },
    )
}

/// Whether Podman can run containers here.
pub fn podman_available() -> bool {
    Command::new("podman")
        .args(["info", "--format", "{{.Host.OS}}"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "linux")
}

/// Names of Docker containers (running or not) labelled
/// `mcpgw.server=<server>`.
pub fn containers_of(server: &str) -> Vec<String> {
    containers_in("docker", server)
}

/// [`containers_of`] for a given runtime.
pub fn containers_in(runtime: &str, server: &str) -> Vec<String> {
    let o = Command::new(runtime)
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label=mcpgw.server={server}"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

/// `docker <args>`, returning trimmed stdout; panics on failure.
pub fn docker(args: &[&str]) -> String {
    let o = Command::new("docker").args(args).output().expect("docker");
    assert!(
        o.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
