//! `mcpgw auth connect|ls|rm` (M2-01).

use std::{path::Path, time::Duration};

use mcpgw_config::Config;

use crate::{AnyResult, control::Control, daemon};

/// Open `url` in the default browser; false if that failed.
pub fn open_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn control(config: &Config, path: &Path) -> AnyResult<Control> {
    if let Some(c) = Control::connect(config)? {
        return Ok(c);
    }
    daemon::start(config, path, Duration::from_secs(60))?;
    Control::connect(config)?.ok_or_else(|| "the daemon did not come up".into())
}

/// Sign in to an OAuth app in the browser and store the tokens.
pub fn connect(
    config: &Config,
    path: &Path,
    provider: &str,
    account: &str,
    browser: bool,
) -> AnyResult<()> {
    let c = control(config, path)?;
    let (state, url, account) = c.oauth_start(provider, account)?;
    println!("Sign in to {provider} ({account}) in your browser:\n  {url}");
    if browser && !open_browser(&url) {
        println!("(could not open a browser; open the URL yourself)");
    }
    let deadline = std::time::Instant::now() + mcpgw_auth::oauth::FLOW_TTL;
    while std::time::Instant::now() < deadline {
        let s = c.oauth_flow(&state)?;
        match s["status"].as_str() {
            Some("done") => {
                println!("connected {provider} ({account})");
                return Ok(());
            }
            Some("error") => {
                return Err(s["message"]
                    .as_str()
                    .unwrap_or("login failed")
                    .to_string()
                    .into());
            }
            _ => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    Err("timed out waiting for the browser login".into())
}

/// List connected accounts.
pub fn ls(config: &Config) -> AnyResult<()> {
    let c = Control::connect(config)?.ok_or("the daemon is not running")?;
    let list = c.oauth_connections()?;
    if list.is_empty() {
        println!("no connected accounts");
    }
    for x in list {
        let status = if x["needs_reauth"] == true {
            "needs login".to_string()
        } else {
            match x["expires_at"].as_u64() {
                Some(t) => format!("expires {t}"),
                None => "no expiry".to_string(),
            }
        };
        let refresh = if x["refreshable"] == true {
            "\trefreshable"
        } else {
            ""
        };
        println!(
            "{}\t{}\t{status}{refresh}",
            x["provider"].as_str().unwrap_or("?"),
            x["account"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// Forget a connected account.
pub fn rm(config: &Config, provider: &str, account: &str) -> AnyResult<()> {
    let c = Control::connect(config)?.ok_or("the daemon is not running")?;
    c.oauth_disconnect(provider, account)?;
    println!("disconnected {provider} ({account})");
    Ok(())
}
