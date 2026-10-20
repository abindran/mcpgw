//! M3-06 end to end: a server added with `--warm` is started by the daemon
//! itself, before any call, and shows as warm in `mcpgw status`.

use std::{path::PathBuf, process::Command, time::Duration};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mcpgw-warm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[gateway]\nmode = \"direct\"\nidle_timeout = \"1s\"\nstore_path = {:?}\ntoken_path = {:?}\nlisten = \"127.0.0.1:0\"\n",
                dir.join("db.sqlite"),
                dir.join("token")
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .args(args)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "mcpgw {args:?}: {}{out}",
            String::from_utf8_lossy(&o.stderr)
        );
        out
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_mcpgw"))
            .arg("--config")
            .arg(self.dir.join("config.toml"))
            .arg("down")
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn daemon_starts_warm_servers() {
    let e = Env::new();
    let fake = mcpgw_testkit::fake_mcp().to_string_lossy().into_owned();
    e.ok(&["add", "hot", "--warm", "--command", &fake]);
    e.ok(&["add", "lazy", "--command", &fake]);
    e.ok(&["up"]);
    let mut st = String::new();
    for _ in 0..100 {
        st = e.ok(&["status"]);
        if st.contains("hot\tidle (warm)") || st.contains("hot\tready (warm)") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(st.contains("(warm)\t7 tools\tpid "), "{st}");
    assert!(st.contains("lazy\tcold\t7 tools\t-"), "{st}");
    // past the idle timeout it is still running
    std::thread::sleep(Duration::from_millis(1500));
    assert!(e.ok(&["status"]).contains("(warm)\t7 tools\tpid "));
}
