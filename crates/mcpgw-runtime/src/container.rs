//! Container launcher (M3-01): Docker or Podman.
//!
//! A `container` server runs as `<runtime> run -i --name mcpgw-<server>-<id>`
//! under its sandbox profile (M3-02), with MCP over the attached stdin/stdout, so it goes through the same
//! stdio path (handshake, stderr capture, crash detection) as a local
//! process. Each start gets a fresh name. When the server stops (idle,
//! eviction, shutdown, failed handshake) the CLI is killed and the
//! container is removed with `rm -f`. Containers carry `mcpgw.*` labels so a
//! daemon can remove ones a crashed predecessor left behind ([`sweep`]).
//!
//! Images are pinned by digest when one is known (`repo:tag@sha256:…`) and
//! pulled before the handshake, so a slow download never times it out.
//!
//! Containers run without `--rm`: mcpgw always removes them itself, and a
//! container that died is first inspected ([`post_mortem`]) so a kill at
//! its memory limit is reported as such.

use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

use mcpgw_core::{Limits, Sandbox};
use mcpgw_proto::{BackendConn, ProtoError, StderrSink, StdioSpec};

/// Label on every container mcpgw starts.
pub const MANAGED_LABEL: &str = "mcpgw.managed";
/// Label naming the server.
pub const SERVER_LABEL: &str = "mcpgw.server";
/// Label with the gateway's store (see [`set_instance`]).
pub const INSTANCE_LABEL: &str = "mcpgw.instance";
/// Label with the launching process id.
pub const PID_LABEL: &str = "mcpgw.pid";

/// Longest a pull may take.
const PULL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

static INSTANCE: OnceLock<String> = OnceLock::new();

/// Tag containers started by this process with `mcpgw.instance=<id>` (the
/// daemon uses a hash of its store path), so [`sweep`] can find leftovers.
pub fn set_instance(id: &str) {
    let _ = INSTANCE.set(id.to_string());
}

/// The runtime to use: `wanted`, or the first of docker and podman found.
pub fn pick_runtime(wanted: Option<&str>) -> Result<String, String> {
    match wanted {
        Some(r) if mcpgw_core::on_path(r) => Ok(r.to_string()),
        Some(r) => Err(format!("container runtime {r} is not installed")),
        None => mcpgw_core::CONTAINER_RUNTIMES
            .iter()
            .find(|r| mcpgw_core::on_path(r))
            .map(|r| r.to_string())
            .ok_or_else(|| "no container runtime found: install Docker or Podman".into()),
    }
}

/// A running container, as shown in status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContainerInfo {
    /// `docker` or `podman`.
    pub runtime: String,
    /// Container name.
    pub name: String,
    /// Image reference it runs (pinned by digest when known).
    pub image: String,
    /// Whether the runtime itself runs without root (rootless Docker,
    /// Podman as a user); `None` if it couldn't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootless: Option<bool>,
}

/// Whether `runtime` runs rootless (asked once per runtime).
pub async fn rootless(runtime: &str) -> Option<bool> {
    static CACHE: OnceLock<std::sync::Mutex<BTreeMap<String, Option<bool>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(runtime).copied()) {
        return v;
    }
    let format = if runtime == "podman" {
        "{{.Host.Security.Rootless}}"
    } else {
        "{{json .SecurityOptions}}"
    };
    let v = match output(runtime, &["info", "--format", format]).await {
        Ok(o) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout);
            Some(if runtime == "podman" {
                t.trim() == "true"
            } else {
                t.contains("name=rootless")
            })
        }
        _ => None,
    };
    if let Ok(mut c) = cache.lock() {
        c.insert(runtime.to_string(), v);
    }
    v
}

/// How to start a container server (secrets already filled in).
#[derive(Clone)]
pub struct ContainerTemplate {
    /// Server name (for the container name and label).
    pub server: String,
    /// Runtime, if the spec names one.
    pub runtime: Option<String>,
    /// Image reference to run.
    pub image: String,
    /// Entrypoint arguments.
    pub args: Vec<String>,
    /// Environment inside the container.
    pub env: BTreeMap<String, String>,
    /// Extra `run` flags.
    pub run_args: Vec<String>,
    /// Confinement profile.
    pub sandbox: Sandbox,
    /// The server's own limits (unset ones come from the profile).
    pub limits: Limits,
    /// Network allowlist (`host:port`); `Some([])` is no network.
    pub egress: Option<Vec<String>>,
    /// Secret files: container path → vault secret name, replaced by the
    /// value when secrets are resolved.
    pub files: BTreeMap<String, String>,
}

impl std::fmt::Debug for ContainerTemplate {
    // env and file contents may hold secrets: show names only
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerTemplate")
            .field("server", &self.server)
            .field("runtime", &self.runtime)
            .field("image", &self.image)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("files", &self.files.keys().collect::<Vec<_>>())
            .field("sandbox", &self.sandbox)
            .field("egress", &self.egress)
            .finish_non_exhaustive()
    }
}

fn container_name(server: &str) -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let clean: String = server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("mcpgw-{clean}-{id}")
}

impl ContainerTemplate {
    /// The `run` invocation for one start, named `name`.
    pub fn stdio(&self, runtime: &str, name: &str) -> StdioSpec {
        self.stdio_with(runtime, name, &[])
    }

    /// [`stdio`](Self::stdio) with extra network flags (from the egress
    /// setup) before the user's run flags.
    fn stdio_with(&self, runtime: &str, name: &str, network: &[String]) -> StdioSpec {
        let mut args: Vec<String> = ["run", "-i", "--name", name].map(String::from).into();
        args.extend(labels(&self.server));
        // values travel in the CLI's environment, not on its command line
        for k in self.env.keys() {
            args.push("-e".into());
            args.push(k.clone());
        }
        let limits = self.limits.or(&self.sandbox.default_limits());
        args.extend(self.sandbox.run_flags(&limits));
        if self.egress.as_ref().is_some_and(|e| e.is_empty()) {
            args.push("--network=none".into());
        }
        args.extend(network.iter().cloned());
        args.extend(self.run_args.iter().cloned());
        args.push(self.image.clone());
        args.extend(self.args.iter().cloned());
        StdioSpec {
            command: runtime.to_string(),
            args,
            env: self.env.clone(),
            cwd: None,
            cleanup: vec![vec![runtime.into(), "rm".into(), "-f".into(), name.into()]],
            teardown: Vec::new(),
        }
    }

    /// Pull the image if needed, then start the container and connect.
    pub async fn connect(
        &self,
        timeout: Duration,
        sink: Option<StderrSink>,
    ) -> Result<(BackendConn, ContainerInfo), ProtoError> {
        let runtime = pick_runtime(self.runtime.as_deref()).map_err(ProtoError::Connect)?;
        ensure_image(&runtime, &self.image, sink.as_ref())
            .await
            .map_err(ProtoError::Connect)?;
        let name = container_name(&self.server);
        let mut flags = Vec::new();
        let mut teardown = Vec::new();
        if !self.files.is_empty() {
            let f = SecretFiles::write(&runtime, &name, &self.files, sink.as_ref())
                .await
                .map_err(ProtoError::Connect)?;
            flags.extend(f.flags);
            teardown.extend(f.teardown);
        }
        if let Some(list) = self.egress.as_deref().filter(|l| !l.is_empty()) {
            match Egress::start(&runtime, &self.server, &name, list, sink.as_ref()).await {
                Ok(gw) => {
                    flags.extend(gw.server_flags);
                    // the proxy and network go before the secret files
                    teardown.splice(0..0, gw.teardown);
                }
                Err(e) => {
                    run_all(&teardown).await;
                    return Err(ProtoError::Connect(e));
                }
            }
        }
        let mut spec = self.stdio_with(&runtime, &name, &flags);
        spec.teardown = teardown;
        let conn = BackendConn::connect_stdio(&spec, timeout, sink).await?;
        let rootless = rootless(&runtime).await;
        Ok((
            conn,
            ContainerInfo {
                runtime,
                name,
                image: self.image.clone(),
                rootless,
            },
        ))
    }
}

/// The `--label` flags every mcpgw container and network gets.
fn labels(server: &str) -> Vec<String> {
    let mut v = vec![
        format!("{MANAGED_LABEL}=1"),
        format!("{SERVER_LABEL}={server}"),
        format!("{PID_LABEL}={}", std::process::id()),
    ];
    if let Some(i) = INSTANCE.get() {
        v.push(format!("{INSTANCE_LABEL}={i}"));
    }
    v.into_iter()
        .flat_map(|l| ["--label".to_string(), l])
        .collect()
}

/// Image of the egress proxy sidecar (official nginx, stream module). It
/// also serves as the small helper that writes secret files.
pub const EGRESS_IMAGE: &str = "docker.io/library/nginx:1.27-alpine";

/// Where secret files live on the container host: its `/dev/shm`, a
/// tmpfs on every Linux host (and inside Docker Desktop's or colima's VM).
pub const SECRETS_ROOT: &str = "/dev/shm/mcpgw-secrets";

fn instance_dir() -> String {
    INSTANCE.get().cloned().unwrap_or_else(|| "local".into())
}

/// A helper container with the host's `/dev/shm` at `/shm`, running `script`.
fn helper(runtime: &str, script: &str) -> Vec<String> {
    let mut v: Vec<String> = [
        runtime,
        "run",
        "--rm",
        "-i",
        "--network=none",
        "--memory=32m",
    ]
    .map(String::from)
    .into();
    v.extend(
        [
            "--label",
            &format!("{MANAGED_LABEL}=1"),
            "-v",
            "/dev/shm:/shm",
            EGRESS_IMAGE,
            "sh",
            "-c",
            script,
        ]
        .map(String::from),
    );
    v
}

/// Run each command, ignoring failures (best-effort teardown).
async fn run_all(commands: &[Vec<String>]) {
    for argv in commands {
        if let Some((cmd, args)) = argv.split_first() {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let _ = output(cmd, &a).await;
        }
    }
}

/// Secret files in place on the container host's tmpfs.
struct SecretFiles {
    /// `--mount` flags for the server container.
    flags: Vec<String>,
    /// Removes the files.
    teardown: Vec<Vec<String>>,
}

impl SecretFiles {
    /// Write each value (through a helper's stdin, never argv or env) to
    /// `SECRETS_ROOT/<instance>/<container>/f<n>` (dirs 0700, files 0444)
    /// and mount it read-only at its path.
    async fn write(
        runtime: &str,
        name: &str,
        files: &BTreeMap<String, String>,
        sink: Option<&StderrSink>,
    ) -> Result<SecretFiles, String> {
        ensure_image(runtime, EGRESS_IMAGE, sink).await?;
        let rel = format!("{}/{name}", instance_dir());
        let inner = format!("/shm/mcpgw-secrets/{rel}");
        let teardown = vec![helper(runtime, &format!("rm -rf {inner}"))];
        let mut flags = Vec::new();
        for (i, (target, value)) in files.iter().enumerate() {
            let script = format!(
                "umask 077 && mkdir -p {inner} && cat > {inner}/f{i} && chmod 0444 {inner}/f{i}"
            );
            let argv = helper(runtime, &script);
            if let Err(e) = run_with_stdin(&argv, value.as_bytes()).await {
                run_all(&teardown).await;
                return Err(format!("writing the secret file {target}: {e}"));
            }
            flags.push(format!(
                "--mount=type=bind,source={SECRETS_ROOT}/{rel}/f{i},target={target},readonly"
            ));
        }
        Ok(SecretFiles { flags, teardown })
    }
}

/// Run `argv` with `input` on stdin.
async fn run_with_stdin(argv: &[String], input: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let (cmd, args) = argv.split_first().ok_or("empty command")?;
    let mut child = tokio::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).await.map_err(|e| e.to_string())?;
    }
    let o = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(first_line(&o.stderr))
    }
}

/// The nginx config for an allowlist: one listener per port. A port with a
/// single host passes connections straight through; a port shared by
/// several hosts routes TLS by SNI and refuses unknown names.
pub fn egress_conf(allow: &[(String, u16)]) -> String {
    let mut by_port: BTreeMap<u16, Vec<&str>> = BTreeMap::new();
    for (h, p) in allow {
        let hosts = by_port.entry(*p).or_default();
        if !hosts.contains(&h.as_str()) {
            hosts.push(h);
        }
    }
    let mut c = String::from(
        "worker_processes 1;\npid /tmp/nginx.pid;\nerror_log /dev/stderr notice;\n\
         events { worker_connections 512; }\nstream {\n\
         \x20 log_format egress '$remote_addr -> $ssl_preread_server_name:$server_port via $upstream_addr $status';\n\
         \x20 access_log /dev/stdout egress;\n\
         \x20 proxy_connect_timeout 10s;\n\
         \x20 upstream mcpgw_deny { server 127.0.0.1:1; }\n",
    );
    for (port, hosts) in &by_port {
        if let [host] = hosts[..] {
            c += &format!("  server {{ listen {port}; proxy_pass {host}:{port}; }}\n");
            continue;
        }
        let mut map = format!("  map $ssl_preread_server_name $p{port} {{\n");
        for (i, h) in hosts.iter().enumerate() {
            c += &format!("  upstream p{port}_{i} {{ server {h}:{port}; }}\n");
            map += &format!("    {h} p{port}_{i};\n");
        }
        map += "    default mcpgw_deny;\n  }\n";
        c += &map;
        c += &format!("  server {{ listen {port}; ssl_preread on; proxy_pass $p{port}; }}\n");
    }
    c + "}\n"
}

/// A server's egress allowlist in place: an internal network and the
/// proxy sidecar on it.
struct Egress {
    /// `--network` and `--add-host` flags for the server container.
    server_flags: Vec<String>,
    /// Removes the sidecar and the network.
    teardown: Vec<Vec<String>>,
}

impl Egress {
    async fn start(
        runtime: &str,
        server: &str,
        name: &str,
        list: &[String],
        sink: Option<&StderrSink>,
    ) -> Result<Egress, String> {
        let allow: Vec<(String, u16)> = list
            .iter()
            .map(|e| mcpgw_core::parse_egress(e))
            .collect::<Result<_, _>>()?;
        let (net, side) = (format!("{name}-net"), format!("{name}-egress"));
        let teardown = vec![
            vec![runtime.into(), "rm".into(), "-f".into(), side.clone()],
            vec![runtime.into(), "network".into(), "rm".into(), net.clone()],
        ];
        let r = Self::setup(runtime, server, &net, &side, &allow, sink).await;
        match r {
            Ok(ip) => {
                let mut server_flags = vec![format!("--network={net}")];
                let mut seen = std::collections::BTreeSet::new();
                for (h, _) in &allow {
                    if seen.insert(h) {
                        server_flags.push(format!("--add-host={h}:{ip}"));
                    }
                }
                Ok(Egress {
                    server_flags,
                    teardown,
                })
            }
            Err(e) => {
                for argv in &teardown {
                    let a: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
                    let _ = output(runtime, &a).await;
                }
                Err(format!("egress proxy for {server}: {e}"))
            }
        }
    }

    /// Create the network and the sidecar; returns the sidecar's address on
    /// the internal network once nginx is up.
    async fn setup(
        runtime: &str,
        server: &str,
        net: &str,
        side: &str,
        allow: &[(String, u16)],
        sink: Option<&StderrSink>,
    ) -> Result<String, String> {
        let check = |what: &str, o: std::process::Output| {
            if o.status.success() {
                Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                Err(format!("{what}: {}", first_line(&o.stderr)))
            }
        };
        ensure_image(runtime, EGRESS_IMAGE, sink).await?;
        let l = labels(server);
        let mut a: Vec<&str> = vec!["network", "create", "--internal"];
        a.extend(l.iter().map(String::as_str));
        a.push(net);
        check("network create", output(runtime, &a).await?)?;
        // the config travels in the environment and is written at start
        let script = "printf '%s' \"$MCPGW_EGRESS_CONF\" > /tmp/nginx.conf && exec nginx -c /tmp/nginx.conf -g 'daemon off;'";
        let mut a: Vec<&str> = vec!["run", "-d", "--name", side];
        a.extend(l.iter().map(String::as_str));
        a.extend([
            "--memory=64m",
            "--pids-limit=64",
            "-e",
            "MCPGW_EGRESS_CONF",
            EGRESS_IMAGE,
            "sh",
            "-c",
            script,
        ]);
        let conf = egress_conf(allow);
        let o = tokio::process::Command::new(runtime)
            .args(&a)
            .env("MCPGW_EGRESS_CONF", &conf)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("{runtime}: {e}"))?;
        check("starting the proxy", o)?;
        check(
            "network connect",
            output(runtime, &["network", "connect", net, side]).await?,
        )?;
        let format = format!("{{{{(index .NetworkSettings.Networks \"{net}\").IPAddress}}}}");
        let ip = check(
            "inspect",
            output(runtime, &["inspect", "--format", &format, side]).await?,
        )?;
        if ip.is_empty() {
            return Err("the proxy has no address on its network".into());
        }
        // ready once nginx says so; a bad upstream name stops it instead
        for _ in 0..100 {
            let logs = output(runtime, &["logs", side]).await?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            );
            if text.contains("start worker process") {
                return Ok(ip);
            }
            if text.contains("[emerg]") {
                let why = text
                    .lines()
                    .find(|l| l.contains("[emerg]"))
                    .unwrap_or("")
                    .to_string();
                return Err(why);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("the proxy did not start within 10 s".into())
    }
}

async fn output(runtime: &str, args: &[&str]) -> Result<std::process::Output, String> {
    tokio::process::Command::new(runtime)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("{runtime}: {e}"))
}

fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Make sure `image` is present locally, pulling it (progress to `sink`)
/// if not.
pub async fn ensure_image(
    runtime: &str,
    image: &str,
    sink: Option<&StderrSink>,
) -> Result<(), String> {
    let o = output(runtime, &["image", "inspect", "--format", "{{.Id}}", image]).await?;
    if o.status.success() {
        return Ok(());
    }
    let daemon_down = |err: &str| {
        err.contains("Cannot connect")
            || err.contains("daemon running")
            || err.contains("connection refused")
    };
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    if daemon_down(&err) {
        return Err(format!("{runtime} is not running: {}", err.trim()));
    }
    tracing::info!(%runtime, %image, "pulling image");
    if let Some(s) = sink {
        s(&format!("mcpgw: pulling {image}"));
    }
    let pull_args = ["pull", image];
    let o = match tokio::time::timeout(PULL_TIMEOUT, output(runtime, &pull_args)).await {
        Ok(o) => o?,
        Err(_) => return Err(format!("pulling {image} took longer than {PULL_TIMEOUT:?}")),
    };
    if o.status.success() {
        if let Some(s) = sink {
            s(&format!("mcpgw: pulled {image}: {}", first_line(&o.stdout)));
        }
        Ok(())
    } else {
        Err(format!(
            "{runtime} pull {image} failed: {}",
            first_line(&o.stderr)
        ))
    }
}

/// Why a container stopped, if it says: `out of memory (killed at its
/// 256MiB limit)`, `exited with code 3`. Inspects the (stopped, not yet
/// removed) container.
pub async fn post_mortem(info: &ContainerInfo) -> Option<String> {
    let o = output(
        &info.runtime,
        &[
            "inspect",
            "--format",
            "{{.State.Status}}|{{.State.OOMKilled}}|{{.State.ExitCode}}|{{.HostConfig.Memory}}",
            &info.name,
        ],
    )
    .await
    .ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let parts: Vec<&str> = text.split('|').collect();
    let [status, oom, code, memory] = parts[..] else {
        return None;
    };
    if status == "running" {
        return None;
    }
    let limit = memory
        .parse::<u64>()
        .ok()
        .filter(|m| *m > 0)
        .map(|m| format!(" at its {} limit", mcpgw_core::ByteSize(m)))
        .unwrap_or_default();
    match (oom, code) {
        ("true", _) => Some(format!("out of memory: the container was killed{limit}")),
        (_, "137") => Some(format!("the container was killed (SIGKILL){limit}")),
        (_, "0") => None,
        (_, c) => Some(format!("the container exited with code {c}")),
    }
}

/// The repository part of an image reference (no tag or digest).
pub fn repository(image: &str) -> &str {
    let no_digest = image.split('@').next().unwrap_or(image);
    match no_digest.rfind(':') {
        Some(i) if !no_digest[i..].contains('/') => &no_digest[..i],
        _ => no_digest,
    }
}

/// Pull `image` and return its content digest (`sha256:…`) for pinning.
/// `None` for images that only exist locally (no registry digest).
pub async fn resolve_digest(runtime: &str, image: &str) -> Result<Option<String>, String> {
    ensure_image(runtime, image, None).await?;
    let o = output(
        runtime,
        &[
            "image",
            "inspect",
            "--format",
            "{{json .RepoDigests}}",
            image,
        ],
    )
    .await?;
    if !o.status.success() {
        return Err(format!(
            "{runtime} image inspect {image}: {}",
            first_line(&o.stderr)
        ));
    }
    let digests: Vec<String> = serde_json::from_slice(&o.stdout).unwrap_or_default();
    let repo = repository(image);
    // Docker Hub images are listed without the docker.io/library prefix
    let short = |r: &str| {
        r.trim_start_matches("docker.io/")
            .trim_start_matches("library/")
            .to_string()
    };
    Ok(digests
        .iter()
        .filter_map(|d| d.split_once('@'))
        .find(|(r, _)| short(r) == short(repo) || r.ends_with(&format!("/{repo}")))
        .or_else(|| digests.first().and_then(|d| d.split_once('@')))
        .map(|(_, d)| d.to_string()))
}

/// Containers of this instance (see [`set_instance`]) started by other
/// processes, removed with `rm -f`: leftovers of a daemon that crashed.
/// Returns how many were removed.
pub async fn sweep() -> usize {
    let Some(instance) = INSTANCE.get() else {
        return 0;
    };
    let me = std::process::id().to_string();
    let mut removed = 0;
    for rt in mcpgw_core::CONTAINER_RUNTIMES {
        if !mcpgw_core::on_path(rt) {
            continue;
        }
        let filter = format!("label={INSTANCE_LABEL}={instance}");
        let format = format!("{{{{.Names}}}}\t{{{{.Label \"{PID_LABEL}\"}}}}");
        let Ok(o) = output(rt, &["ps", "-a", "--filter", &filter, "--format", &format]).await
        else {
            continue;
        };
        if !o.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let (name, pid) = line.split_once('\t').unwrap_or((line, ""));
            if name.is_empty() || pid == me {
                continue;
            }
            if output(rt, &["rm", "-f", name])
                .await
                .is_ok_and(|o| o.status.success())
            {
                tracing::info!(
                    runtime = rt,
                    container = name,
                    "removed a leftover container"
                );
                removed += 1;
            }
        }
        // then secret files of containers that no longer exist (only if the
        // helper image is here; a sweep never pulls)
        let have_helper = output(
            rt,
            &["image", "inspect", "--format", "{{.Id}}", EGRESS_IMAGE],
        )
        .await
        .is_ok_and(|o| o.status.success());
        if have_helper {
            let live = output(
                rt,
                &["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"],
            )
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
            let dir = format!("/shm/mcpgw-secrets/{instance}");
            let ls = helper(rt, &format!("ls {dir} 2>/dev/null || true"));
            let a: Vec<&str> = ls[1..].iter().map(String::as_str).collect();
            if let Ok(o) = output(rt, &a).await {
                for d in String::from_utf8_lossy(&o.stdout).lines() {
                    let ok_name = d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                    if ok_name && !d.is_empty() && !live.lines().any(|l| l == d) {
                        run_all(&[helper(rt, &format!("rm -rf {dir}/{d}"))]).await;
                    }
                }
            }
        }
        // then their egress networks
        let net_format = format!("{{{{.Name}}}}\t{{{{.Label \"{PID_LABEL}\"}}}}");
        let Ok(o) = output(
            rt,
            &[
                "network",
                "ls",
                "--filter",
                &filter,
                "--format",
                &net_format,
            ],
        )
        .await
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let (name, pid) = line.split_once('\t').unwrap_or((line, ""));
            if !name.is_empty() && pid != me {
                let _ = output(rt, &["network", "rm", name]).await;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_invocation() {
        let t = ContainerTemplate {
            server: "GitHub_x".into(),
            runtime: None,
            image: "ghcr.io/x/y:1@sha256:ab".into(),
            args: vec!["stdio".into()],
            env: [("TOKEN".to_string(), "s3cret".to_string())].into(),
            run_args: vec!["-p".into(), "127.0.0.1:1:1".into()],
            sandbox: Sandbox::Strict,
            limits: Limits {
                memory: Some(mcpgw_core::ByteSize::mib(256)),
                ..Default::default()
            },
            egress: None,
            files: BTreeMap::new(),
        };
        let name = container_name(&t.server);
        assert!(
            name.starts_with("mcpgw-github-x-") && name.len() == "mcpgw-github-x-".len() + 8,
            "{name}"
        );
        let s = t.stdio("docker", &name);
        assert_eq!(s.command, "docker");
        let args = s.args.join(" ");
        assert!(args.starts_with(&format!("run -i --name {name} --label mcpgw.managed=1 --label mcpgw.server=GitHub_x --label mcpgw.pid=")), "{args}");
        assert!(
            args.ends_with(
                "-e TOKEN --cap-drop=ALL --security-opt=no-new-privileges --read-only \
                 --tmpfs=/tmp:rw,nosuid,nodev,size=64m --user=65534:65534 \
                 --memory=268435456 --memory-swap=268435456 --cpus=1 --pids-limit=128 \
                 -p 127.0.0.1:1:1 ghcr.io/x/y:1@sha256:ab stdio"
            ),
            "{args}"
        );
        assert!(!args.contains("s3cret"));
        assert_eq!(s.env["TOKEN"], "s3cret");
        assert_eq!(s.cleanup, [["docker", "rm", "-f", name.as_str()]]);
        let mut none = t.clone();
        none.egress = Some(vec![]);
        assert!(
            none.stdio("docker", &name)
                .args
                .contains(&"--network=none".to_string())
        );
    }

    #[test]
    fn repositories() {
        assert_eq!(
            repository("ghcr.io/github/github-mcp-server:2.0.2"),
            "ghcr.io/github/github-mcp-server"
        );
        assert_eq!(repository("localhost:5000/x"), "localhost:5000/x");
        assert_eq!(
            repository("localhost:5000/x:1@sha256:ab"),
            "localhost:5000/x"
        );
        assert_eq!(repository("node"), "node");
    }
}

#[cfg(test)]
mod egress_tests {
    use super::*;

    #[test]
    fn nginx_config() {
        let allow = |v: &[(&str, u16)]| {
            v.iter()
                .map(|(h, p)| (h.to_string(), *p))
                .collect::<Vec<_>>()
        };
        let one = egress_conf(&allow(&[("api.github.com", 443), ("api.github.com", 443)]));
        assert!(
            one.contains("server { listen 443; proxy_pass api.github.com:443; }"),
            "{one}"
        );
        assert!(!one.contains("ssl_preread on"), "{one}");
        let two = egress_conf(&allow(&[
            ("api.github.com", 443),
            ("github.com", 443),
            ("db.example.com", 5432),
        ]));
        for want in [
            "upstream p443_0 { server api.github.com:443; }",
            "upstream p443_1 { server github.com:443; }",
            "    api.github.com p443_0;",
            "    default mcpgw_deny;",
            "server { listen 443; ssl_preread on; proxy_pass $p443; }",
            "server { listen 5432; proxy_pass db.example.com:5432; }",
        ] {
            assert!(two.contains(want), "{want}\n{two}");
        }
    }
}
