//! Backend server specification.

use crate::{Duration, Error, Limits, Result, Sandbox};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Separator between server name and tool name in aggregated tool names.
/// Server names may not contain it (see [`validate_server_name`]).
pub const TOOL_SEPARATOR: &str = "__";

/// How to reach a backend server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum Transport {
    /// Spawn a local child process speaking MCP over stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Extra environment variables for the child.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// Working directory. Default: the gateway's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    /// Connect to a remote Streamable HTTP endpoint.
    Http {
        /// Endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    /// Run an OCI image with Docker or Podman, speaking MCP over the
    /// container's stdin/stdout (`run -i`, attached). The container is
    /// removed when the server stops.
    Container {
        /// Image, e.g. `ghcr.io/github/github-mcp-server:2.0.2`.
        image: String,
        /// Pinned content digest (`sha256:…`): the container always runs
        /// exactly this image, pulled by digest. `mcpgw add` fills it in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
        /// `docker` or `podman`. Default: whichever is installed, Docker first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime: Option<String>,
        /// Arguments for the image's entrypoint.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Environment variables set inside the container (values are passed
        /// through the runtime's environment, never on its command line).
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// Extra `run` flags, e.g. `["-p", "127.0.0.1:8085:8085"]`. They
        /// come after the sandbox's flags, so they can add to them
        /// (`--cap-add=…`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        run_args: Vec<String>,
        /// Confinement: `standard` (default), `strict` or `off`.
        #[serde(default, skip_serializing_if = "Sandbox::is_default")]
        sandbox: Sandbox,
        /// Network allowlist, `["api.github.com:443"]`: the container can
        /// reach these host:port pairs and nothing else. `[]` means no
        /// network at all. Unset: unrestricted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress: Option<Vec<String>>,
    },
    /// A Wasm component exporting the `mcpgw:tool` world, run in-process
    /// with wasmtime (M4-01).
    Wasm {
        /// The component file (`.wasm`).
        path: PathBuf,
        /// TCP destinations it may connect to, `["api.github.com:443"]`, or
        /// `["*"]` for any. Default: no network at all (M4-02).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow_network: Vec<String>,
        /// Host directories it may use. Default: none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow_dirs: Vec<DirAccess>,
        /// Environment variables it sees (secrets bind through `secrets`).
        /// Default: none; the host's environment never leaks in.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// Longest one call may run before it's stopped (M4-03). Default:
        /// 30s. The memory cap is `limits.memory` (default and maximum
        /// 256MiB).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_budget: Option<Duration>,
    },
    /// An HTTP API described by an OpenAPI 3.x spec, served as tools by
    /// mcpgw itself, no MCP server needed (M4-05).
    #[serde(rename = "openapi")]
    OpenApi {
        /// The spec: a file path or an http(s) URL (JSON or YAML).
        spec: String,
        /// Where requests go. Default: the spec's first server URL
        /// (relative ones resolve against the spec's URL).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        /// Headers on every request; values may use `${secret:<name>}`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// A host directory granted to a Wasm server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirAccess {
    /// The directory on the host.
    pub path: PathBuf,
    /// Where the component sees it. Default: the same path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<String>,
    /// May create and change files. Default: read-only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
}

/// Split an egress entry `host:port`, checking both parts.
pub fn parse_egress(entry: &str) -> std::result::Result<(String, u16), String> {
    let (host, port) = entry
        .rsplit_once(':')
        .ok_or_else(|| format!("egress {entry:?} must be host:port"))?;
    let port: u16 = port
        .parse()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| format!("egress {entry:?} has a bad port"))?;
    let host = host.to_ascii_lowercase();
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if host.len() > 253 || !host.split('.').all(label_ok) {
        return Err(format!("egress {entry:?}: {host:?} is not a host name"));
    }
    if host
        .split('.')
        .all(|l| l.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "egress {entry:?}: give a host name, not an IP address (names are what the allowlist routes)"
        ));
    }
    Ok((host, port))
}

/// Container runtimes mcpgw drives.
pub const CONTAINER_RUNTIMES: [&str; 2] = ["docker", "podman"];

/// Whether `command` is an existing path or is found on `PATH` (with
/// `PATHEXT` on Windows).
pub fn on_path(command: &str) -> bool {
    let p = std::path::Path::new(command);
    if p.components().count() > 1 {
        return p.is_file();
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_string)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|d| {
            exts.iter()
                .any(|x| d.join(format!("{command}{x}")).is_file())
        })
    })
}

/// `image` pinned to `digest` (`repo:tag@sha256:…`), or `image` as given.
pub fn pinned_image_ref(image: &str, digest: Option<&str>) -> String {
    match digest {
        Some(d) if !image.contains('@') => format!("{image}@{d}"),
        _ => image.to_string(),
    }
}

impl Transport {
    /// A stdio transport with no extra env or working directory.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::Stdio {
            command: command.into(),
            args,
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    /// An HTTP transport with no extra headers.
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http {
            url: url.into(),
            headers: BTreeMap::new(),
        }
    }

    /// A container transport with no extra args, env or flags.
    pub fn container(image: impl Into<String>) -> Self {
        Self::Container {
            image: image.into(),
            digest: None,
            runtime: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            run_args: Vec::new(),
            sandbox: Sandbox::default(),
            egress: None,
        }
    }

    /// A Wasm component, granted nothing.
    pub fn wasm(path: impl Into<PathBuf>) -> Self {
        Self::Wasm {
            path: path.into(),
            allow_network: Vec::new(),
            allow_dirs: Vec::new(),
            env: BTreeMap::new(),
            time_budget: None,
        }
    }

    /// One line saying what runs: the command line, the URL, or the image
    /// (pinned by digest) and its arguments.
    pub fn target(&self) -> String {
        match self {
            Self::Stdio { command, args, .. } => std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            Self::Http { url, .. } => url.clone(),
            Self::Container {
                image,
                digest,
                args,
                ..
            } => std::iter::once(pinned_image_ref(image, digest.as_deref()))
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            Self::Wasm { path, .. } => path.display().to_string(),
            Self::OpenApi { spec, base_url, .. } => match base_url {
                Some(b) => format!("{spec} → {b}"),
                None => spec.clone(),
            },
        }
    }

    /// Short kind name: `stdio`, `http`, `container` or `wasm`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http { .. } => "http",
            Self::Container { .. } => "container",
            Self::Wasm { .. } => "wasm",
            Self::OpenApi { .. } => "openapi",
        }
    }
}

/// One backend tool server (`[[server]]` in the config file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerSpec {
    /// Unique server name.
    pub name: String,
    /// Shorter prefix for its tools' public names (`<alias>__<tool>`).
    /// Default: the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Connection details.
    #[serde(flatten)]
    pub transport: Transport,
    /// Whether the gateway may start this server. Default: `true`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Stop the server after this long without calls. Default: gateway-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<Duration>,
    /// The server keeps user state (browser, database session): it is never
    /// stopped for idleness or evicted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stateful: bool,
    /// Keep this server started: `{ min = 1 }` starts it with the gateway,
    /// never stops it for idleness or evicts it, and restarts it after a
    /// crash, so calls never wait for a cold start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_pool: Option<WarmPool>,
    /// Deadline for start-up and the MCP handshake. Default: gateway-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout: Option<Duration>,
    /// Secrets injected at spawn: environment variable → vault secret name,
    /// e.g. `{ GITHUB_TOKEN = "github_token" }`. Args and env values may
    /// also contain `${secret:<name>}`. Injected values are redacted from
    /// everything the server sends back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
    /// Secrets written to files (container servers): container path →
    /// vault secret, e.g. `{ "/run/secrets/sa.json" = "gcp_sa" }`. Each is
    /// a read-only file in tmpfs, gone when the server stops.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    /// Resource limits: `{ memory = "256MiB", cpus = 0.5, pids = 64 }`.
    /// Container servers take all three (unset ones come from the sandbox
    /// profile); stdio servers take `memory`, a cap on the process tree.
    #[serde(default, skip_serializing_if = "Limits::is_empty")]
    pub limits: Limits,
    /// Where it was installed from, when `mcpgw add` took it from the MCP
    /// Registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<RegistryRef>,
}

/// How many instances of a server to keep started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WarmPool {
    /// Instances kept warm: 0 (start on demand) or 1. Calls share one
    /// connection per server, so larger pools aren't supported.
    pub min: u32,
}

impl ServerSpec {
    /// Whether the server is kept warm (`warm_pool.min >= 1`).
    pub fn warm(&self) -> bool {
        self.warm_pool.is_some_and(|w| w.min >= 1)
    }
}

/// A pinned MCP Registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegistryRef {
    /// Registry name, e.g. `io.github.github/github-mcp-server`.
    pub name: String,
    /// Installed version.
    pub version: String,
}

/// Stands for the caller's account in secret names:
/// `secrets = { GITHUB_TOKEN = "oauth/github@{account}" }`.
pub const ACCOUNT_PLACEHOLDER: &str = "{account}";

/// Account used when a caller names none.
pub const DEFAULT_ACCOUNT: &str = "default";

/// Prefix of a secret placeholder in args and env values.
pub const SECRET_PLACEHOLDER: &str = "${secret:";

/// Where a `${secret:` placeholder's name ends (its closing `}`), stepping
/// over a nested `{account}`.
fn placeholder_end(after: &str) -> Option<usize> {
    let mut i = 0;
    while i < after.len() {
        if after[i..].starts_with(ACCOUNT_PLACEHOLDER) {
            i += ACCOUNT_PLACEHOLDER.len();
        } else if after[i..].starts_with('}') {
            return Some(i);
        } else {
            i += after[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// Names referenced by `${secret:<name>}` placeholders in `s`.
pub fn secret_placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(SECRET_PLACEHOLDER) {
        let after = &rest[i + SECRET_PLACEHOLDER.len()..];
        match placeholder_end(after) {
            Some(j) => {
                out.push(after[..j].to_string());
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    out
}

/// Replace `${secret:<name>}` placeholders using `lookup`.
pub fn fill_placeholders(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(SECRET_PLACEHOLDER) {
        out.push_str(&rest[..i]);
        let after = &rest[i + SECRET_PLACEHOLDER.len()..];
        match placeholder_end(after) {
            Some(j) => {
                out.push_str(&lookup(&after[..j]).unwrap_or_default());
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl ServerSpec {
    /// Prefix of this server's public tool names: the alias, else the name.
    pub fn prefix(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }

    /// A spec with defaults for everything but name and transport.
    pub fn new(name: impl Into<String>, transport: Transport) -> Self {
        Self {
            name: name.into(),
            alias: None,
            transport,
            enabled: true,
            idle_timeout: None,
            stateful: false,
            warm_pool: None,
            startup_timeout: None,
            secrets: BTreeMap::new(),
            files: BTreeMap::new(),
            limits: Limits::default(),
            registry: None,
        }
    }

    /// Whether this server's secrets depend on the caller's account.
    pub fn account_scoped(&self) -> bool {
        self.secret_refs()
            .iter()
            .any(|r| r.contains(ACCOUNT_PLACEHOLDER))
    }

    /// This server with `{account}` filled in, for one account.
    pub fn for_account(&self, account: &str) -> ServerSpec {
        let fill = |s: &str| s.replace(ACCOUNT_PLACEHOLDER, account);
        let mut s = self.clone();
        for v in s.secrets.values_mut().chain(s.files.values_mut()) {
            *v = fill(v);
        }
        match &mut s.transport {
            Transport::Stdio { args, env, .. } | Transport::Container { args, env, .. } => {
                for a in args.iter_mut() {
                    *a = fill(a);
                }
                for v in env.values_mut() {
                    *v = fill(v);
                }
            }
            Transport::Http { headers, .. } | Transport::OpenApi { headers, .. } => {
                for v in headers.values_mut() {
                    *v = fill(v);
                }
            }
            Transport::Wasm { env, .. } => {
                for v in env.values_mut() {
                    *v = fill(v);
                }
            }
        }
        s
    }

    /// Every secret this server uses: bindings and placeholders.
    pub fn secret_refs(&self) -> std::collections::BTreeSet<String> {
        let mut refs: std::collections::BTreeSet<String> = self
            .secrets
            .values()
            .chain(self.files.values())
            .cloned()
            .collect();
        if let Transport::Wasm { env, .. } = &self.transport {
            for s in env.values() {
                refs.extend(secret_placeholders(s));
            }
        }
        if let Transport::Stdio { args, env, .. } | Transport::Container { args, env, .. } =
            &self.transport
        {
            for s in args.iter().chain(env.values()) {
                refs.extend(secret_placeholders(s));
            }
        }
        if let Transport::Http { headers, .. } | Transport::OpenApi { headers, .. } =
            &self.transport
        {
            for s in headers.values() {
                refs.extend(secret_placeholders(s));
            }
        }
        refs
    }

    /// Check this server's own invariants: valid name, non-empty
    /// command/URL, http(s) URL.
    pub fn validate(&self) -> Result<()> {
        validate_server_name(&self.name)?;
        for (var, secret) in &self.secrets {
            let ok_var = !var.is_empty()
                && !var.starts_with(|c: char| c.is_ascii_digit())
                && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok_var {
                return Err(Error::Config(format!(
                    "server {}: {var:?} is not an environment variable name",
                    self.name
                )));
            }
            crate::validate_secret_name(&secret.replace(ACCOUNT_PLACEHOLDER, DEFAULT_ACCOUNT))?;
        }
        for r in self.secret_refs() {
            crate::validate_secret_name(&r.replace(ACCOUNT_PLACEHOLDER, DEFAULT_ACCOUNT)).map_err(
                |_| {
                    Error::Config(format!(
                        "server {}: bad secret placeholder name {r:?}",
                        self.name
                    ))
                },
            )?;
        }
        if let Some(a) = &self.alias {
            validate_server_name(a).map_err(|_| {
                Error::Config(format!("invalid alias {a:?} for server {}", self.name))
            })?;
        }
        if let Some(w) = self.warm_pool
            && w.min > 1
        {
            return Err(Error::Config(format!(
                "server {}: warm_pool.min = {} is not supported; use 0 or 1 (calls share one connection per server)",
                self.name, w.min
            )));
        }
        let empty = match &self.transport {
            Transport::Stdio { command, .. } => command.trim().is_empty(),
            Transport::Http { url, .. } => url.trim().is_empty(),
            Transport::Container { image, .. } => image.trim().is_empty(),
            Transport::Wasm { path, .. } => path.as_os_str().is_empty(),
            Transport::OpenApi { spec, .. } => spec.trim().is_empty(),
        };
        if empty {
            return Err(Error::Config(format!(
                "server {} has an empty command/url/image",
                self.name
            )));
        }
        self.limits
            .validate()
            .map_err(|m| Error::Config(format!("server {}: {m}", self.name)))?;
        match &self.transport {
            Transport::Http { .. } | Transport::OpenApi { .. } if !self.limits.is_empty() => {
                return Err(Error::Config(format!(
                    "server {}: limits apply to local servers (stdio or container), not remote ones",
                    self.name
                )));
            }
            Transport::Stdio { .. } if self.limits.cpus.is_some() || self.limits.pids.is_some() => {
                return Err(Error::Config(format!(
                    "server {}: stdio servers take a memory limit only; cpus and pids need a container server",
                    self.name
                )));
            }
            Transport::Wasm { .. } if self.limits.cpus.is_some() || self.limits.pids.is_some() => {
                return Err(Error::Config(format!(
                    "server {}: Wasm servers take a memory limit (and time_budget); cpus and pids don't apply",
                    self.name
                )));
            }
            _ => {}
        }
        if !self.files.is_empty() && !matches!(self.transport, Transport::Container { .. }) {
            return Err(Error::Config(format!(
                "server {}: secret files are for container servers only",
                self.name
            )));
        }
        for (path, secret) in &self.files {
            let ok = path.starts_with('/')
                && path.len() > 1
                && !path.ends_with('/')
                && !path.split('/').any(|c| c == ".." || c == ".")
                && !path.contains(['\0', ',', '='])
                && !matches!(path.as_str(), "/proc" | "/sys" | "/dev")
                && !["/proc/", "/sys/", "/dev/"]
                    .iter()
                    .any(|p| path.starts_with(p));
            if !ok {
                return Err(Error::Config(format!(
                    "server {}: secret file path {path:?} must be an absolute file path (no .., commas or =; not under /proc, /sys or /dev)",
                    self.name
                )));
            }
            crate::validate_secret_name(&secret.replace(ACCOUNT_PLACEHOLDER, DEFAULT_ACCOUNT))?;
        }
        if let Transport::Wasm { allow_network, .. } = &self.transport {
            for e in allow_network.iter().filter(|e| e.as_str() != "*") {
                let ok = parse_egress(e).is_ok()
                    || e.rsplit_once(':').is_some_and(|(h, p)| {
                        h.parse::<std::net::IpAddr>().is_ok()
                            && p.parse::<u16>().is_ok_and(|p| p > 0)
                    });
                if !ok {
                    return Err(Error::Config(format!(
                        "server {}: allow_network entry {e:?} must be host:port, ip:port or \"*\"",
                        self.name
                    )));
                }
            }
        }
        if let Transport::Container {
            digest,
            runtime,
            run_args,
            env,
            egress,
            ..
        } = &self.transport
        {
            for e in egress.iter().flatten() {
                parse_egress(e).map_err(|m| Error::Config(format!("server {}: {m}", self.name)))?;
            }
            let network_flag = |a: &String| {
                let f = a.split('=').next().unwrap_or(a);
                matches!(f, "--network" | "--net" | "--add-host" | "--dns")
            };
            if egress.is_some() && run_args.iter().any(network_flag) {
                return Err(Error::Config(format!(
                    "server {}: --network, --add-host and --dns can't be combined with egress",
                    self.name
                )));
            }
            let bad = |m: String| Err(Error::Config(format!("server {}: {m}", self.name)));
            if let Some(d) = digest {
                let hex = d.strip_prefix("sha256:").unwrap_or("");
                if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return bad(format!("digest {d:?} is not sha256:<64 hex digits>"));
                }
            }
            if let Some(r) = runtime
                && !CONTAINER_RUNTIMES.contains(&r.as_str())
            {
                return bad(format!("container runtime {r:?} is not docker or podman"));
            }
            for a in run_args {
                let flag = a.split('=').next().unwrap_or(a);
                if matches!(flag, "-d" | "--detach" | "--name" | "-t" | "--tty") {
                    return bad(format!(
                        "run flag {a:?} is managed by mcpgw (containers run attached, without a TTY, under their own name)"
                    ));
                }
                if a.contains(SECRET_PLACEHOLDER) {
                    return bad(
                        "secrets can't go in run_args (they'd be on the command line); use env"
                            .into(),
                    );
                }
            }
            for var in env.keys().chain(self.secrets.keys()) {
                if var.is_empty() || var.contains('=') {
                    return bad(format!("{var:?} is not an environment variable name"));
                }
            }
        }
        if let Transport::Http { url, .. } = &self.transport
            && !(url.starts_with("http://") || url.starts_with("https://"))
        {
            return Err(Error::Config(format!(
                "server {} url must start with http:// or https://",
                self.name
            )));
        }
        Ok(())
    }
}

/// Validate a server name: 1-64 chars of `[A-Za-z0-9_-]`, no `__`.
pub fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(Error::Config(format!(
            "server name must be 1-64 characters: {name:?}"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Config(format!(
            "server name may only contain [A-Za-z0-9_-]: {name:?}"
        )));
    }
    if name.contains(TOOL_SEPARATOR) || name.starts_with('_') || name.ends_with('_') {
        return Err(Error::Config(format!(
            "server name may not contain {TOOL_SEPARATOR:?} or start/end with '_': {name:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_name_rules() {
        for ok in ["fs", "my-server", "a_b", "X1"] {
            validate_server_name(ok).unwrap();
        }
        for bad in ["", "a__b", "_a", "a_", "a b", "a.b", &"x".repeat(65)] {
            assert!(validate_server_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn container_transport() {
        let toml_src = r#"
            name = "gh"
            transport = "container"
            image = "ghcr.io/github/github-mcp-server:2.0.2"
            digest = "sha256:ffced0d76e77428532a2ced185516992d77fd146d69eec2f244e6237d1a97796"
            run_args = ["-p", "127.0.0.1:8085:8085"]
            env = { GITHUB_TOOLSETS = "repos" }
            secrets = { GITHUB_PERSONAL_ACCESS_TOKEN = "github_token" }
        "#;
        let s: ServerSpec = toml::from_str(toml_src).unwrap();
        s.validate().unwrap();
        assert_eq!(s.transport.kind(), "container");
        let back: ServerSpec = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let Transport::Container { image, digest, .. } = &s.transport else {
            panic!()
        };
        assert_eq!(
            pinned_image_ref(image, digest.as_deref()),
            "ghcr.io/github/github-mcp-server:2.0.2@sha256:ffced0d76e77428532a2ced185516992d77fd146d69eec2f244e6237d1a97796"
        );
        assert_eq!(
            pinned_image_ref("a@sha256:1", Some("sha256:2")),
            "a@sha256:1"
        );
        for (field, bad, needle) in [
            ("digest", r#""sha256:abc""#, "not sha256:"),
            ("runtime", r#""lxc""#, "not docker or podman"),
            ("run_args", r#"["-d"]"#, "managed by mcpgw"),
            ("run_args", r#"["--name=x"]"#, "managed by mcpgw"),
            (
                "run_args",
                r#"["-e", "T=${secret:t}"]"#,
                "can't go in run_args",
            ),
        ] {
            let src = format!(
                "name = \"c\"\ntransport = \"container\"\nimage = \"x\"\n{field} = {bad}\n"
            );
            let s: ServerSpec = toml::from_str(&src).unwrap();
            let e = s.validate().unwrap_err().to_string();
            assert!(e.contains(needle), "{field}: {e}");
        }
        for (entry, ok) in [
            ("api.github.com:443", true),
            ("API.GitHub.com:443", true),
            ("localhost:8080", true),
            ("api.github.com", false),
            ("api.github.com:0", false),
            ("api.github.com:99999", false),
            ("1.1.1.1:443", false),
            ("*.github.com:443", false),
            ("-bad.com:443", false),
        ] {
            assert_eq!(parse_egress(entry).is_ok(), ok, "{entry}");
        }
        assert_eq!(
            parse_egress("API.GitHub.com:443").unwrap(),
            ("api.github.com".into(), 443)
        );
        let mut s = ServerSpec::new("c", Transport::container("x"));
        if let Transport::Container { egress, .. } = &mut s.transport {
            *egress = Some(vec!["api.github.com:443".into()]);
        }
        s.validate().unwrap();
        if let Transport::Container { run_args, .. } = &mut s.transport {
            run_args.push("--network=host".into());
        }
        assert!(
            s.validate()
                .unwrap_err()
                .to_string()
                .contains("can't be combined with egress")
        );
        let mut f = ServerSpec::new("c", Transport::container("x"));
        f.files
            .insert("/run/secrets/sa.json".into(), "gcp_sa".into());
        f.validate().unwrap();
        assert!(f.secret_refs().contains("gcp_sa"));
        for bad in [
            "run/x",
            "/run/../etc/x",
            "/run/x/",
            "/proc/self/x",
            "/a,b",
            "/",
        ] {
            let mut g = f.clone();
            g.files = [(bad.to_string(), "gcp_sa".to_string())].into();
            assert!(
                g.validate()
                    .unwrap_err()
                    .to_string()
                    .contains("absolute file path"),
                "{bad}"
            );
        }
        let mut stdio = ServerSpec::new("s", Transport::stdio("x", vec![]));
        stdio.files = f.files.clone();
        assert!(
            stdio
                .validate()
                .unwrap_err()
                .to_string()
                .contains("container servers only")
        );
        let empty = ServerSpec::new("c", Transport::container(" "));
        assert!(empty.validate().is_err());
    }

    #[test]
    fn wasm_transport_and_grants() {
        let src = r#"
            name = "notes"
            transport = "wasm"
            path = "/opt/notes.wasm"
            allow_network = ["api.example.com:443", "127.0.0.1:8080", "*"]
            allow_dirs = [{ path = "/data", write = true }, { path = "/srv/docs", guest = "/docs" }]
            env = { REGION = "eu", KEY = "${secret:notes_key}" }
        "#;
        let s: ServerSpec = toml::from_str(src).unwrap();
        s.validate().unwrap();
        assert_eq!(
            (s.transport.kind(), s.transport.target().as_str()),
            ("wasm", "/opt/notes.wasm")
        );
        assert!(s.secret_refs().contains("notes_key"));
        assert_eq!(
            toml::from_str::<ServerSpec>(&toml::to_string(&s).unwrap()).unwrap(),
            s
        );
        let mut capped = s.clone();
        capped.limits.memory = Some(crate::ByteSize::mib(64));
        capped.validate().unwrap();
        capped.limits.cpus = Some(1.0);
        assert!(
            capped
                .validate()
                .unwrap_err()
                .to_string()
                .contains("don't apply")
        );
        for bad in ["api.example.com", "1.2.3.4:0", "*.example.com:443"] {
            let mut t = s.clone();
            if let Transport::Wasm { allow_network, .. } = &mut t.transport {
                *allow_network = vec![bad.into()];
            }
            assert!(
                t.validate()
                    .unwrap_err()
                    .to_string()
                    .contains("allow_network"),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_transport_fields() {
        let mut s = ServerSpec::new("a", Transport::stdio(" ", vec![]));
        assert!(s.validate().is_err());
        s.transport = Transport::http("ftp://x");
        assert!(s.validate().is_err());
        s.transport = Transport::http("https://x/mcp");
        s.validate().unwrap();
    }

    #[test]
    fn secret_bindings_and_placeholders() {
        let mut s = ServerSpec::new(
            "gh",
            Transport::Stdio {
                command: "gh-mcp".into(),
                args: vec!["--token=${secret:gh_pat}".into(), "--x".into()],
                env: [("URL".to_string(), "https://${secret:host}/api".to_string())].into(),
                cwd: None,
            },
        );
        s.secrets
            .insert("GITHUB_TOKEN".into(), "github_token".into());
        s.validate().unwrap();
        assert_eq!(
            s.secret_refs().into_iter().collect::<Vec<_>>(),
            ["gh_pat", "github_token", "host"]
        );
        let filled = fill_placeholders("a ${secret:x} b ${secret:y} ${secret:", &|n| {
            (n == "x").then(|| "X".to_string())
        });
        assert_eq!(filled, "a X b  ${secret:");
        s.secrets.insert("1BAD".into(), "k".into());
        assert!(s.validate().is_err());
    }

    #[test]
    fn account_placeholders() {
        let mut s = ServerSpec::new(
            "gh",
            Transport::Stdio {
                command: "gh-mcp".into(),
                args: vec!["--token=${secret:oauth/github@{account}}".into()],
                env: Default::default(),
                cwd: None,
            },
        );
        assert!(!ServerSpec::new("x", Transport::stdio("x", vec![])).account_scoped());
        assert!(s.account_scoped());
        s.secrets
            .insert("GITHUB_TOKEN".into(), "oauth/github@{account}".into());
        s.validate().unwrap();
        let w = s.for_account("work");
        assert!(!w.account_scoped());
        assert_eq!(w.secrets["GITHUB_TOKEN"], "oauth/github@work");
        assert!(w.secret_refs().contains("oauth/github@work"));
        assert_eq!(w.name, "gh");
    }

    #[test]
    fn toml_and_json_roundtrip() {
        let toml_src = r#"
            name = "gh"
            transport = "stdio"
            command = "github-mcp"
            args = ["--ro"]
            env = { LOG = "1" }
            idle_timeout = "5m"
            stateful = true
        "#;
        let s: ServerSpec = toml::from_str(toml_src).unwrap();
        assert_eq!(s.idle_timeout, Some(Duration::from_secs(300)));
        assert!(s.stateful && s.enabled);
        let back: ServerSpec = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["transport"], "stdio");
        assert_eq!(json["idle_timeout"], "5m");
        assert!(
            json.get("enabled").is_none(),
            "defaults are omitted: {json}"
        );
        assert_eq!(serde_json::from_value::<ServerSpec>(json).unwrap(), s);
    }
}
