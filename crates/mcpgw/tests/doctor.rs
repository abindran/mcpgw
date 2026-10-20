//! M3-12: `mcpgw doctor` finds and explains each of ten seeded
//! misconfigurations (one per scenario), with a fix for each, and passes on
//! a healthy setup. Every run gets its own HOME, so the real client configs
//! are never read.

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

struct Env {
    dir: PathBuf,
    passphrase: String,
    path_env: Option<String>,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-doctor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = Self {
            dir,
            passphrase: "right pass".into(),
            path_env: None,
        };
        e.config("");
        e
    }

    fn config(&self, extra: &str) {
        std::fs::write(
            self.dir.join("config.toml"),
            format!(
                "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n{extra}",
                self.dir.join("db.sqlite"),
                self.dir.join("token")
            ),
        )
        .unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("HOME", &self.dir)
            .env("USERPROFILE", &self.dir)
            .env("APPDATA", self.dir.join("AppData"))
            .env("XDG_CONFIG_HOME", self.dir.join(".config"))
            .env("MCPGW_VAULT_PASSPHRASE", &self.passphrase);
        if let Some(p) = &self.path_env {
            c.env("PATH", p);
        }
        c
    }

    fn secret(&self, name: &str, value: &str) {
        let mut c = self
            .cmd(&["secret", "set", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(value.as_bytes()).unwrap();
        assert!(c.wait().unwrap().success());
    }

    fn add(&self, args: &[&str]) {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Doctor's checks, and whether it passed.
    fn doctor(&self) -> (bool, Vec<Value>) {
        let o = self.cmd(&["doctor", "--json"]).output().unwrap();
        let checks: Vec<Value> = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| {
            panic!(
                "doctor json: {}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        });
        (o.status.success(), checks)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fake() -> String {
    mcpgw_testkit::fake_mcp().to_string_lossy().into_owned()
}

/// The check with `id`, which must explain itself and say how to fix it.
fn found<'a>(checks: &'a [Value], id: &str, level: &str) -> &'a Value {
    let c = checks
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("doctor missed {id}: {checks:#?}"));
    assert_eq!(c["level"], level, "{c}");
    assert!(
        c["message"].as_str().is_some_and(|m| m.len() > 15),
        "unexplained: {c}"
    );
    assert!(
        c["fix"].as_str().is_some_and(|f| !f.is_empty()),
        "no fix: {c}"
    );
    c
}

fn seed(n: usize) -> (Env, &'static str, &'static str) {
    let e = Env::new(&format!("s{n}"));
    let id = match n {
        // 1. the config file doesn't load
        1 => {
            std::fs::write(e.dir.join("config.toml"), "[gateway]\nlisten = 7777\n").unwrap();
            "config_invalid"
        }
        // 2. a server's command isn't installed
        2 => {
            e.add(&[
                "add",
                "ghost",
                "--no-introspect",
                "--command",
                "mcpgw-no-such-program-xyz",
            ]);
            "command_missing"
        }
        // 3. a server uses a secret that isn't in the vault
        3 => {
            e.secret("other", "x");
            e.add(&[
                "add",
                "gh",
                "--no-introspect",
                "--command",
                &fake(),
                "--secret",
                "GITHUB_TOKEN=gh_token",
            ]);
            "secret_missing"
        }
        // 4. the listen port is taken by another program
        4 => {
            let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = held.local_addr().unwrap().port();
            std::mem::forget(held); // stays bound until the test process ends
            std::fs::write(
                e.dir.join("config.toml"),
                format!(
                    "[gateway]\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:{port}\"\n",
                    e.dir.join("db.sqlite"),
                    e.dir.join("token")
                ),
            )
            .unwrap();
            "port_in_use"
        }
        // 5. a daemon file left by a daemon that's gone
        5 => {
            std::fs::write(
                e.dir.join("daemon.json"),
                json!({"pid": 999_999_999u32, "url": "http://127.0.0.1:9/mcp", "version": "0", "started_at": 0}).to_string(),
            )
            .unwrap();
            "stale_daemon_file"
        }
        // 6. a container server without Docker or Podman
        6 => {
            e.add(&[
                "add",
                "boxed",
                "--no-introspect",
                "--image",
                "ghcr.io/example/x:1",
            ]);
            let empty = e.dir.join("emptybin");
            std::fs::create_dir_all(&empty).unwrap();
            let mut e = e;
            e.path_env = Some(empty.to_string_lossy().into_owned());
            return (e, "container_runtime_missing", "fail");
        }
        // 7. an OAuth login that expired with no way to renew it
        7 => {
            e.secret(
                "oauth/github@default",
                &json!({"access_token": "at-old", "expires_at": 1, "token_type": "Bearer"})
                    .to_string(),
            );
            "oauth_expired"
        }
        // 8. a profile naming a server that doesn't exist
        8 => {
            e.config("\n[[profile]]\nname = \"web\"\nservers = [\"ghost\"]\n");
            return (e, "profile_unknown_server", "warn");
        }
        // 9. a client config whose mcpgw entry points nowhere
        9 => {
            let cursor = e.dir.join(".cursor");
            std::fs::create_dir_all(&cursor).unwrap();
            std::fs::write(
                cursor.join("mcp.json"),
                json!({"mcpServers": {"mcpgw": {"command": "/no/such/dir/mcpgw", "args": ["connect"]}}}).to_string(),
            )
            .unwrap();
            "client_entry_broken"
        }
        // 10. the vault passphrase is wrong
        10 => {
            e.secret("a", "b");
            let mut e = e;
            e.passphrase = "wrong pass".into();
            return (e, "vault_unreadable", "fail");
        }
        _ => unreachable!(),
    };
    let level = if id == "stale_daemon_file" {
        "warn"
    } else {
        "fail"
    };
    (e, id, level)
}

#[test]
fn doctor_finds_and_explains_ten_misconfigurations() {
    for n in 1..=10 {
        let (e, id, level) = seed(n);
        let (ok, checks) = e.doctor();
        let c = found(&checks, id, level);
        println!(
            "{n:2}. {id}: {}\n     fix: {}",
            c["message"].as_str().unwrap(),
            c["fix"].as_str().unwrap()
        );
        if level == "fail" {
            assert!(!ok, "doctor passed despite {id}");
        }
    }
}

#[test]
fn healthy_setup_passes() {
    let e = Env::new("ok");
    e.secret("gh_token", "t");
    e.add(&[
        "add",
        "fine",
        "--command",
        &fake(),
        "--secret",
        "GITHUB_TOKEN=gh_token",
    ]);
    let (ok, checks) = e.doctor();
    let bad: Vec<&Value> = checks.iter().filter(|c| c["level"] == "fail").collect();
    assert!(ok && bad.is_empty(), "{bad:#?}");
    assert!(
        checks
            .iter()
            .any(|c| c["id"] == "vault" && c["level"] == "ok")
    );
    assert!(
        checks
            .iter()
            .any(|c| c["id"] == "command" && c["subject"] == "server fine")
    );
    // the human format names the problem count
    let o = e.cmd(&["doctor"]).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("0 problem(s)"));
}
