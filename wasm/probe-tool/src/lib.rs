//! Probe tools: `connect {host, port}` (a TCP connect to an IPv4 address),
//! `lookup {host}` (DNS), `read {path}` / `write {path}` (in a preopened
//! directory), `env {name}`. Each answers `ok …` or `denied: <reason>`.

wit_bindgen::generate!({ path: "wit", world: "probe", generate_all });

use wasi::cli::environment;
use wasi::filesystem::{preopens, types as fs};
use wasi::sockets::{instance_network, ip_name_lookup, network, tcp_create_socket};

struct Probe;

fn arg(arguments: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let Some(i) = arguments.find(&needle) else {
        return String::new();
    };
    let rest = arguments[i + needle.len()..].trim_start().trim_start_matches(':').trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        r.split('"').next().unwrap_or("").to_string()
    } else {
        rest.chars().take_while(|c| c.is_ascii_digit()).collect()
    }
}

fn ok(text: String) -> Result<Output, String> {
    Ok(Output { text, is_error: false })
}

fn connect(host: &str, port: u16) -> String {
    let parts: Vec<u8> = host.split('.').filter_map(|p| p.parse().ok()).collect();
    let [a, b, c, d] = parts[..] else {
        return "denied: give an IPv4 address".into();
    };
    let net = instance_network::instance_network();
    let sock = match tcp_create_socket::create_tcp_socket(network::IpAddressFamily::Ipv4) {
        Ok(s) => s,
        Err(e) => return format!("denied: {e:?}"),
    };
    let addr = network::IpSocketAddress::Ipv4(network::Ipv4SocketAddress { port, address: (a, b, c, d) });
    if let Err(e) = sock.start_connect(&net, addr) {
        return format!("denied: {e:?}");
    }
    let ready = sock.subscribe();
    loop {
        match sock.finish_connect() {
            Ok(_) => return "ok connected".into(),
            Err(network::ErrorCode::WouldBlock) => ready.block(),
            Err(e) => return format!("denied: {e:?}"),
        }
    }
}

fn lookup(host: &str) -> String {
    let net = instance_network::instance_network();
    match ip_name_lookup::resolve_addresses(&net, host) {
        Err(e) => format!("denied: {e:?}"),
        Ok(stream) => {
            let ready = stream.subscribe();
            loop {
                match stream.resolve_next_address() {
                    Ok(Some(_)) => return "ok resolved".into(),
                    Ok(None) => return "denied: no addresses".into(),
                    Err(network::ErrorCode::WouldBlock) => ready.block(),
                    Err(e) => return format!("denied: {e:?}"),
                }
            }
        }
    }
}

/// The preopened directory containing `path`, and the rest of the path.
fn preopen(path: &str) -> Result<(fs::Descriptor, String), String> {
    let dirs = preopens::get_directories();
    if dirs.is_empty() {
        return Err("denied: no directories granted".into());
    }
    for (d, mount) in dirs {
        let m = mount.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(m) {
            return Ok((d, rest.trim_start_matches('/').to_string()));
        }
    }
    Err(format!("denied: {path} is outside the granted directories"))
}

fn read(path: &str) -> String {
    let (dir, rel) = match preopen(path) {
        Ok(x) => x,
        Err(e) => return e,
    };
    match dir.open_at(fs::PathFlags::empty(), &rel, fs::OpenFlags::empty(), fs::DescriptorFlags::READ) {
        Err(e) => format!("denied: {e:?}"),
        Ok(f) => match f.read(4096, 0) {
            Ok((bytes, _)) => format!("ok {}", String::from_utf8_lossy(&bytes)),
            Err(e) => format!("denied: {e:?}"),
        },
    }
}

fn write(path: &str) -> String {
    let (dir, rel) = match preopen(path) {
        Ok(x) => x,
        Err(e) => return e,
    };
    match dir.open_at(fs::PathFlags::empty(), &rel, fs::OpenFlags::CREATE, fs::DescriptorFlags::WRITE) {
        Err(e) => format!("denied: {e:?}"),
        Ok(f) => match f.write(b"written by wasm", 0) {
            Ok(_) => "ok written".into(),
            Err(e) => format!("denied: {e:?}"),
        },
    }
}

impl Guest for Probe {
    fn list_tools() -> Vec<Tool> {
        let t = |name: &str, d: &str| Tool { name: name.into(), description: d.into(), input_schema: r#"{"type":"object"}"#.into() };
        vec![
            t("connect", "TCP connect to host:port"),
            t("lookup", "Resolve a host name"),
            t("read", "Read a file"),
            t("write", "Write a file"),
            t("env", "Read an environment variable"),
        ]
    }

    fn call_tool(name: String, arguments: String) -> Result<Output, String> {
        match name.as_str() {
            "connect" => ok(connect(&arg(&arguments, "host"), arg(&arguments, "port").parse().unwrap_or(0))),
            "lookup" => ok(lookup(&arg(&arguments, "host"))),
            "read" => ok(read(&arg(&arguments, "path"))),
            "write" => ok(write(&arg(&arguments, "path"))),
            "env" => {
                let want = arg(&arguments, "name");
                ok(environment::get_environment()
                    .into_iter()
                    .find(|(k, _)| *k == want)
                    .map(|(_, v)| format!("ok {v}"))
                    .unwrap_or_else(|| "denied: not set".into()))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

export!(Probe);
