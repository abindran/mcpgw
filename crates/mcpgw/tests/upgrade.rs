//! M4 exit: upgrading from an M3 build. `tests/fixtures/m3/` holds a
//! config, store and vault written by the M3 binary (c8eee8c, made by
//! `scripts/make-m3-fixture.sh`): a stored stdio server bound to a vault
//! secret, its tools pinned, and a profile with a policy. v1.0 must read all
//! of it: the server is listed and starts without being quarantined, the
//! secret decrypts, is injected and redacted, and the profile still applies.

use std::{path::PathBuf, process::Command};

use mcpgw_testkit::McpClient;
use serde_json::{Value, json};

const PASSPHRASE: &str = "m3-fixture-passphrase";

struct Env {
    dir: PathBuf,
    path: std::ffi::OsString,
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/m3");
        for f in ["db.sqlite", "vault.json"] {
            std::fs::copy(fixture.join(f), dir.join(f)).unwrap();
        }
        let cfg = std::fs::read_to_string(fixture.join("config.toml")).unwrap();
        // the fixture's paths point at the directory it was made in
        let here = dir.to_string_lossy().replace('\\', "/");
        std::fs::write(dir.join("config.toml"), cfg.replace("@DIR@", &here)).unwrap();
        // the stored server runs `fake-mcp` from PATH, as it did under M3
        let fake = mcpgw_testkit::fake_mcp();
        let name = if cfg!(windows) {
            "fake-mcp.exe"
        } else {
            "fake-mcp"
        };
        std::fs::copy(&fake, dir.join("bin").join(name)).unwrap();
        let mut paths = vec![dir.join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        Self { dir, path }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mcpgw"));
        c.arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .env("PATH", &self.path)
            .env("MCPGW_VAULT_PASSPHRASE", PASSPHRASE)
            .env("HOME", &self.dir)
            .env("USERPROFILE", &self.dir);
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.cmd(&["down"]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn v1_runs_on_an_m3_store_vault_and_config() {
    let e = Env::new();
    let m3 = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/m3/m3-version.txt"),
    )
    .unwrap();
    assert!(m3.starts_with("mcpgw "), "{m3}");

    // the stored server, the vault's secret and the config's profile
    let list = e.ok(&["list"]);
    assert!(list.contains("echo\tstdio\tstore"), "{list}");
    assert!(e.ok(&["secret", "ls"]).contains("m3_secret"));
    assert!(e.ok(&["profiles"]).contains("narrow"));
    assert!(e.ok(&["audit", "verify"]).contains("chain intact"));

    // nothing the doctor considers broken
    let doctor = e.cmd(&["doctor", "--json"]).output().unwrap();
    let checks: Vec<Value> = serde_json::from_slice(&doctor.stdout).unwrap();
    let failed: Vec<&Value> = checks.iter().filter(|c| c["level"] == "fail").collect();
    assert!(failed.is_empty(), "{failed:#?}");

    // the server starts against its M3 pin, with the secret injected and
    // redacted on the way back
    e.ok(&["up"]);
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(e.dir.join("daemon.json")).unwrap()).unwrap();
    let url = info["url"].as_str().unwrap().to_string();
    let token = e.ok(&["token"]).trim().to_string();
    let c = McpClient::http(&url, Some(&token)).await.unwrap();
    let r = c
        .call_text("echo__env", json!({"name": "FAKE_FROM_VAULT"}))
        .await
        .unwrap();
    assert_eq!(r, "[REDACTED:m3_secret]");
    let status = e.ok(&["status"]);
    assert!(!status.contains("quarantined"), "{status}");

    // the profile's policy (written by M3) still applies
    let narrow = McpClient::http(&format!("{url}/p/narrow"), Some(&token))
        .await
        .unwrap();
    let names = narrow.tool_names().await.unwrap();
    assert!(names.contains(&"echo__add".to_string()), "{names:?}");
    assert!(!names.contains(&"echo__crash".to_string()), "{names:?}");
}
