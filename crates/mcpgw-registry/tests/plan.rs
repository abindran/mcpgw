//! Install plans from registry entries, including the real GitHub entry.

use std::collections::BTreeMap;

use mcpgw_core::{RegistryRef, Transport};
use mcpgw_registry::{PlanOptions, ServerJson, default_server_name, plan, secret_name};

fn github() -> ServerJson {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/github-mcp-server.json")).unwrap();
    serde_json::from_value(v["server"].clone()).unwrap()
}

fn opts<'a>(
    has_secret: &'a dyn Fn(&str) -> bool,
    available: &'a dyn Fn(&str) -> bool,
) -> PlanOptions<'a> {
    PlanOptions {
        server_name: "github-mcp-server".into(),
        prefer_remote: false,
        package_type: None,
        values: BTreeMap::new(),
        has_secret,
        runtime_available: available,
    }
}

fn stdio(t: &Transport) -> (&str, Vec<&str>, &BTreeMap<String, String>) {
    match t {
        Transport::Stdio {
            command, args, env, ..
        } => (command, args.iter().map(String::as_str).collect(), env),
        other => panic!("not stdio: {other:?}"),
    }
}

#[test]
fn github_oci_package_is_pinned_and_skips_the_unset_optional_token() {
    let s = github();
    assert_eq!(default_server_name(&s.name), "github-mcp-server");
    let p = plan(&s, &opts(&|_| false, &|_| true)).unwrap();
    let (cmd, args, env) = stdio(&p.spec.transport);
    assert_eq!(cmd, "docker");
    assert_eq!(
        args,
        [
            "run",
            "-i",
            "--rm",
            "-p",
            "127.0.0.1:8085:8085",
            "-e",
            "GITHUB_OAUTH_CALLBACK_PORT=8085",
            "ghcr.io/github/github-mcp-server:2.0.2"
        ]
    );
    assert!(env.is_empty());
    assert_eq!(p.method, "oci ghcr.io/github/github-mcp-server:2.0.2");
    assert_eq!(p.skipped, ["github-mcp-server_token"]);
    assert!(p.secrets.is_empty() && p.spec.secrets.is_empty());
    assert_eq!(
        p.spec.registry,
        Some(RegistryRef {
            name: "io.github.github/github-mcp-server".into(),
            version: "2.0.2".into()
        })
    );
}

#[test]
fn github_token_once_set_is_injected_not_written_into_argv() {
    let p = plan(
        &github(),
        &opts(&|n| n == "github-mcp-server_token", &|_| true),
    )
    .unwrap();
    let (_, args, _) = stdio(&p.spec.transport);
    assert!(
        args.windows(2)
            .any(|w| w == ["-e", "GITHUB_PERSONAL_ACCESS_TOKEN"]),
        "{args:?}"
    );
    assert!(!args.iter().any(|a| a.contains("secret")), "{args:?}");
    assert_eq!(
        p.spec
            .secrets
            .get("GITHUB_PERSONAL_ACCESS_TOKEN")
            .map(String::as_str),
        Some("github-mcp-server_token")
    );
    assert_eq!(p.secrets.len(), 1);
    assert!(!p.secrets[0].required);
    // the image is the last docker argument
    assert_eq!(args.last(), Some(&"ghcr.io/github/github-mcp-server:2.0.2"));
}

#[test]
fn github_falls_back_to_the_remote_without_docker() {
    let p = plan(&github(), &opts(&|_| false, &|c| c != "docker")).unwrap();
    let Transport::Http { url, headers } = &p.spec.transport else {
        panic!("not http: {:?}", p.spec.transport)
    };
    assert_eq!(url, "https://api.githubcopilot.com/mcp/");
    // optional secret header, unset: left out (log in with OAuth instead)
    assert!(headers.is_empty(), "{headers:?}");
    assert_eq!(p.skipped, ["github-mcp-server_authorization"]);
    assert!(
        p.notes[0].contains("docker is not installed"),
        "{:?}",
        p.notes
    );

    let mut o = opts(&|_| true, &|_| true);
    o.prefer_remote = true;
    let p = plan(&github(), &o).unwrap();
    let Transport::Http { headers, .. } = &p.spec.transport else {
        panic!()
    };
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer ${secret:github-mcp-server_authorization}")
    );
    assert_eq!(p.method, "remote https://api.githubcopilot.com/mcp/");
}

fn entry(json: serde_json::Value) -> ServerJson {
    serde_json::from_value(json).unwrap()
}

#[test]
fn npm_and_pypi_are_pinned_and_inputs_are_filled() {
    let s = entry(serde_json::json!({
        "name": "io.example/notes",
        "version": "1.4.0",
        "packages": [
            {
                "registryType": "pypi", "identifier": "notes-mcp", "version": "1.4.0",
                "transport": {"type": "stdio"}
            },
            {
                "registryType": "npm", "identifier": "@example/notes-mcp", "version": "1.4.0",
                "transport": {"type": "stdio"},
                "packageArguments": [
                    {"type": "positional", "valueHint": "dir", "value": "{dir}",
                     "variables": {"dir": {"default": "/tmp/notes"}}},
                    {"type": "named", "name": "--mode", "default": "ro"}
                ],
                "environmentVariables": [
                    {"name": "NOTES_API_KEY", "isSecret": true, "isRequired": true,
                     "description": "API key"},
                    {"name": "NOTES_REGION", "default": "eu"}
                ]
            }
        ]
    }));
    let mut o = opts(&|_| false, &|_| true);
    o.server_name = "notes".into();
    o.values.insert("dir".into(), "/data".into());
    let p = plan(&s, &o).unwrap();
    let (cmd, args, env) = stdio(&p.spec.transport);
    assert_eq!(cmd, "npx");
    assert_eq!(
        args,
        ["-y", "@example/notes-mcp@1.4.0", "/data", "--mode", "ro"]
    );
    assert_eq!(env.get("NOTES_REGION").map(String::as_str), Some("eu"));
    assert_eq!(
        p.spec.secrets.get("NOTES_API_KEY").map(String::as_str),
        Some("notes_notes_api_key")
    );
    assert_eq!(p.secrets.len(), 1);
    assert!(p.secrets[0].required);
    assert_eq!(p.secrets[0].description, "API key");
    assert_eq!(secret_name("notes", "NOTES_API_KEY"), "notes_notes_api_key");

    o.package_type = Some("pypi".into());
    let p = plan(&s, &o).unwrap();
    let (cmd, args, _) = stdio(&p.spec.transport);
    assert_eq!((cmd, args), ("uvx", vec!["notes-mcp==1.4.0"]));

    o.package_type = Some("oci".into());
    assert!(plan(&s, &o).unwrap_err().contains("no oci package"));
}

#[test]
fn missing_required_values_and_unrunnable_entries_are_explained() {
    let s = entry(serde_json::json!({
        "name": "io.example/db",
        "version": "0.2.0",
        "packages": [{
            "registryType": "npm", "identifier": "db-mcp", "version": "0.2.0",
            "environmentVariables": [{"name": "DB_URL", "isRequired": true, "description": "connection string"}]
        }]
    }));
    let e = plan(&s, &opts(&|_| false, &|_| true)).unwrap_err();
    assert!(
        e.contains("DB_URL is required (connection string)") && e.contains("--env DB_URL="),
        "{e}"
    );

    let s = entry(serde_json::json!({
        "name": "io.example/dotnet",
        "version": "1.0.0",
        "packages": [{"registryType": "nuget", "identifier": "Example.Mcp", "version": "1.0.0"}]
    }));
    let e = plan(&s, &opts(&|_| false, &|_| true)).unwrap_err();
    assert!(e.contains("no package mcpgw can run (nuget)"), "{e}");

    // a package whose runtime is missing and no remote: kept, with a note
    let s = entry(serde_json::json!({
        "name": "io.example/py",
        "version": "3.0.0",
        "packages": [{"registryType": "pypi", "identifier": "py-mcp", "version": "3.0.0"}]
    }));
    let p = plan(&s, &opts(&|_| false, &|_| false)).unwrap();
    assert!(p.notes[0].contains("uvx is not installed"), "{:?}", p.notes);
}

#[test]
fn default_names_are_valid_server_names() {
    for (reg, want) in [
        ("io.github.github/github-mcp-server", "github-mcp-server"),
        ("com.example/My.Server", "My-Server"),
        ("io.x/a__b", "a_b"),
    ] {
        let n = default_server_name(reg);
        assert_eq!(n, want);
        mcpgw_core::validate_server_name(&n).unwrap();
    }
}
