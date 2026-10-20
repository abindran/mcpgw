//! `mcpgw policy …` (M4-07): write Cedar from glob rules, and check what an
//! endpoint's policy decides.

use std::path::Path;

use mcpgw_config::Config;
use mcpgw_core::{Decision, Policy};
use mcpgw_store::SqliteStore;

use crate::{AnyResult, serve};

/// Attach the Cedar engine `policy` names, if any. On an error the policy
/// stays unloaded, so it denies everything.
pub fn attach(policy: &mut Policy, endpoint: &str) -> Result<(), String> {
    #[cfg(feature = "cedar")]
    return mcpgw_cedar::attach(policy, endpoint);
    #[cfg(not(feature = "cedar"))]
    match (&policy.cedar, endpoint) {
        (Some(_), _) => Err("this mcpgw was built without Cedar support (feature `cedar`)".into()),
        (None, _) => Ok(()),
    }
}

/// `policy` ready to serve `endpoint`: with its Cedar engine, or denying
/// everything (and saying why) when the file doesn't load.
pub fn loaded(policy: &Policy, endpoint: &str) -> Policy {
    let mut p = policy.clone();
    if let Err(e) = attach(&mut p, endpoint) {
        tracing::error!(endpoint, error = %e, "Cedar policy not loaded: every tool is denied");
    }
    p
}

/// The policy of a profile (from the config file or the store), or of the
/// default endpoint, with the endpoint's name.
fn policy_of(config: &Config, profile: Option<&str>) -> AnyResult<(String, Policy)> {
    let Some(name) = profile else {
        return Ok(("default".into(), config.gateway.policy.clone()));
    };
    let store = SqliteStore::open(config.gateway.store_path())?;
    let p = serve::profiles(config, &store)?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("no profile named {name:?}"))?;
    Ok((p.name, p.policy))
}

/// `mcpgw policy convert`: the endpoint's allow/deny/approve lists as Cedar.
pub fn convert(config: &Config, profile: Option<&str>, output: Option<&Path>) -> AnyResult<()> {
    let (_, p) = policy_of(config, profile)?;
    #[cfg(feature = "cedar")]
    let text = mcpgw_cedar::convert(&p)?;
    #[cfg(not(feature = "cedar"))]
    let text: String = {
        let _ = p;
        return Err("this mcpgw was built without Cedar support (feature `cedar`)".into());
    };
    match output {
        Some(f) => {
            std::fs::write(f, &text)?;
            let at = match profile {
                Some(name) => format!("profile {name}'s "),
                None => "gateway.".into(),
            };
            println!(
                "wrote {}; to use it, set {at}policy = {{ cedar = {:?} }}",
                f.display(),
                f.display().to_string()
            );
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn describe(d: &Decision) -> String {
    match d {
        Decision::Allow => "allow".into(),
        Decision::Approve { pattern } => format!("approve ({pattern})"),
        Decision::Deny { pattern: Some(p) } => format!("deny ({p})"),
        Decision::Deny { pattern: None } => "deny (nothing allows it)".into(),
    }
}

/// `mcpgw policy check`: load the endpoint's policy and print what it
/// decides for each tool.
pub fn check(config: &Config, profile: Option<&str>, tools: &[String]) -> AnyResult<()> {
    let (endpoint, mut p) = policy_of(config, profile)?;
    attach(&mut p, &endpoint)?;
    match &p.cedar {
        Some(f) => println!("{}: Cedar policies load and validate", f.display()),
        None if p.is_empty() => println!("no policy: every tool is allowed"),
        None => println!(
            "glob rules: {} allow, {} deny, {} approve",
            p.allow.len(),
            p.deny.len(),
            p.approve.len()
        ),
    }
    for t in tools {
        println!("{t}\t{}", describe(&p.decide(t)));
    }
    Ok(())
}
