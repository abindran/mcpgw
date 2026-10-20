//! M4-07 done-when: existing allow/deny/approve rules convert to Cedar and
//! give the same decisions.

use mcpgw_cedar::{Cedar, convert};
use mcpgw_core::{Decision, Policy, PolicyEngine};

fn policy(allow: &[&str], deny: &[&str], approve: &[&str]) -> Policy {
    let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
    Policy {
        allow: v(allow),
        deny: v(deny),
        approve: v(approve),
        ..Default::default()
    }
}

const TOOLS: &[&str] = &[
    "fs__read_file",
    "fs__read_dir",
    "fs__write_file",
    "fs__delete_file",
    "github__list_issues",
    "github__create_issue",
    "github__delete_repo",
    "github__merge_pull_request",
    "github__merge_",
    "gh__crash",
    "gh__spawn",
    "gh__add",
    "gh__echo",
    "slack__post_message",
    "slack__delete_message",
    "a__b",
    "x",
    "",
    "weird__na\"me",
    "back\\slash__t",
    "ctl__a\u{7}b",
    "uni__caf\u{e9}",
];

#[test]
fn converted_rules_give_the_same_decisions() {
    let cases = [
        // every shape from the M2-08 docs and tests
        policy(&[], &[], &[]),
        policy(&[], &["*__delete_*"], &[]),
        policy(&[], &["gh__crash", "*__spawn"], &[]),
        policy(&["gh__add", "gh__echo"], &["gh__echo"], &[]),
        policy(
            &["fs__read_*", "github__*"],
            &["github__delete_*"],
            &["github__merge_*"],
        ),
        // approve outside the allow list still asks (approve beats allow)
        policy(&["fs__*"], &[], &["github__merge_*", "*__post_*"]),
        // deny beats approve; the first matching pattern is named
        policy(&[], &["*__delete_*", "github__*"], &["github__*", "*"]),
        policy(&["*"], &["*"], &["*"]),
        policy(&["*__*"], &[], &[]),
        policy(&["weird__*", "back\\slash__*"], &["*\"*"], &[]),
        policy(&["nothing_matches"], &[], &[]),
    ];
    let mut checked = 0;
    for (i, p) in cases.iter().enumerate() {
        let text = convert(p).unwrap();
        let cedar = Cedar::parse(&text, "frontend").unwrap_or_else(|e| panic!("{e}\n{text}"));
        for tool in TOOLS {
            assert_eq!(
                cedar.decide(tool),
                p.decide(tool),
                "case {i}, tool {tool:?}\n{text}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, cases.len() * TOOLS.len());
}

#[test]
fn question_marks_cannot_convert() {
    let e = convert(&policy(&["fs__read_?"], &[], &[])).unwrap_err();
    assert!(e.contains("`?`"), "{e}");
}

#[test]
fn hand_written_policies() {
    let cedar = Cedar::parse(
        r#"
        @id("readers")
        permit (principal == Mcpgw::Endpoint::"frontend", action, resource in Mcpgw::Server::"fs")
        when { resource.tool like "read_*" };

        @id("merges need a human")
        @approve
        permit (principal, action == Mcpgw::Action::"call", resource)
        when { resource.server == "github" && resource.tool like "merge_*" };

        @id("no deletes")
        forbid (principal, action, resource) when { resource.tool like "delete_*" };
        "#,
        "frontend",
    )
    .unwrap();
    assert_eq!(cedar.decide("fs__read_file"), Decision::Allow);
    assert_eq!(
        cedar.decide("fs__write_file"),
        Decision::Deny { pattern: None }
    );
    assert_eq!(
        cedar.decide("github__merge_pr"),
        Decision::Approve {
            pattern: "merges need a human".into()
        }
    );
    assert_eq!(
        cedar.decide("fs__delete_file"),
        Decision::Deny {
            pattern: Some("no deletes".into())
        }
    );
    // the principal is the endpoint: another profile gets nothing from "readers"
    let other = Cedar::parse(
        r#"permit (principal == Mcpgw::Endpoint::"frontend", action, resource);"#,
        "backend",
    )
    .unwrap();
    assert_eq!(
        other.decide("fs__read_file"),
        Decision::Deny { pattern: None }
    );
}

#[test]
fn mistakes_are_load_errors() {
    // a typo in an attribute: caught by the schema, not a silent deny
    let e = Cedar::parse(
        r#"permit (principal, action, resource) when { resource.nmae == "x" };"#,
        "default",
    )
    .unwrap_err();
    assert!(e.contains("nmae"), "{e}");
    // an unknown entity type
    let e = Cedar::parse(
        r#"permit (principal == User::"bob", action, resource);"#,
        "default",
    )
    .unwrap_err();
    assert!(e.contains("User"), "{e}");
    // a syntax error
    assert!(Cedar::parse("permit (principal, action, resource", "default").is_err());
}

#[test]
fn attach_loads_files_and_fails_closed() {
    let dir = std::env::temp_dir().join(format!("mcpgw-cedar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.cedar");
    std::fs::write(
        &file,
        r#"permit (principal, action, resource) when { resource.server == "fs" };"#,
    )
    .unwrap();
    let mut p = Policy {
        cedar: Some(file.clone()),
        ..Default::default()
    };
    // not attached yet: everything is denied
    assert!(matches!(p.decide("fs__read"), Decision::Deny { .. }));
    mcpgw_cedar::attach(&mut p, "default").unwrap();
    assert_eq!(p.decide("fs__read"), Decision::Allow);
    assert!(!p.visible("gh__add"));
    assert!(!p.is_empty());

    // a broken file: the error is returned and the policy denies everything
    let broken = dir.join("broken.cedar");
    std::fs::write(&broken, "permit (").unwrap();
    let mut b = Policy {
        cedar: Some(broken),
        ..Default::default()
    };
    assert!(mcpgw_cedar::attach(&mut b, "default").is_err());
    assert!(matches!(b.decide("fs__read"), Decision::Deny { .. }));
    let _ = std::fs::remove_dir_all(&dir);
}
