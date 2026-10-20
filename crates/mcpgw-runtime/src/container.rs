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
#[derive(Debug, Clone)]
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
        let mut args: Vec<String> = ["run", "-i", "--name", name].map(String::from).into();
        let mut label = |k: &str, v: &str| {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        };
        label(MANAGED_LABEL, "1");
        label(SERVER_LABEL, &self.server);
        label(PID_LABEL, &std::process::id().to_string());
        if let Some(i) = INSTANCE.get() {
            label(INSTANCE_LABEL, i);
        }
        // values travel in the CLI's environment, not on its command line
        for k in self.env.keys() {
            args.push("-e".into());
            args.push(k.clone());
        }
        let limits = self.limits.or(&self.sandbox.default_limits());
        args.extend(self.sandbox.run_flags(&limits));
        args.extend(self.run_args.iter().cloned());
        args.push(self.image.clone());
        args.extend(self.args.iter().cloned());
        StdioSpec {
            command: runtime.to_string(),
            args,
            env: self.env.clone(),
            cwd: None,
            cleanup: Some(vec![runtime.into(), "rm".into(), "-f".into(), name.into()]),
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
        let conn = BackendConn::connect_stdio(&self.stdio(&runtime, &name), timeout, sink).await?;
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
        assert_eq!(s.cleanup.unwrap(), ["docker", "rm", "-f", name.as_str()]);
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
