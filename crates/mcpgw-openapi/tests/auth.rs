//! M4-06: each security scheme type — apiKey in a header, query parameter
//! and cookie; HTTP bearer and basic; oauth2; openIdConnect — is tested
//! against a mock API that checks the credential, plus OR and AND
//! requirements, an open operation, and the hint when credentials are
//! missing.

use axum::{
    Router,
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
};
use base64::Engine;
use mcpgw_openapi::{Credential, Credentials, RequestExtras, api, parse};
use serde_json::{Map, Value, json};

fn spec() -> Value {
    let op = |sec: Value| json!({"get": {"operationId": null, "security": sec, "responses": {"200": {"description": "ok"}}}});
    let mut paths = Map::new();
    for (path, id, sec) in [
        ("/key-header", "keyHeader", json!([{"hdr": []}])),
        ("/key-query", "keyQuery", json!([{"qry": []}])),
        ("/key-cookie", "keyCookie", json!([{"ck": []}])),
        ("/bearer", "bearer", json!([{"bearer": []}])),
        ("/basic", "basic", json!([{"basic": []}])),
        ("/oauth", "oauth", json!([{"oauth": ["read"]}])),
        ("/oidc", "oidc", json!([{"oidc": ["openid"]}])),
        ("/either", "either", json!([{"hdr": []}, {"bearer": []}])),
        ("/both", "both", json!([{"hdr": [], "bearer": []}])),
        ("/open", "open", json!([])),
    ] {
        let mut o = op(sec);
        o["get"]["operationId"] = json!(id);
        paths.insert(path.into(), o);
    }
    parse(
        &json!({
            "openapi": "3.0.3",
            "info": {"title": "secure", "version": "1"},
            "paths": paths,
            "components": {"securitySchemes": {
                "hdr": {"type": "apiKey", "in": "header", "name": "X-API-Key"},
                "qry": {"type": "apiKey", "in": "query", "name": "api_key"},
                "ck": {"type": "apiKey", "in": "cookie", "name": "session"},
                "bearer": {"type": "http", "scheme": "bearer"},
                "basic": {"type": "http", "scheme": "basic"},
                "oauth": {"type": "oauth2", "flows": {"authorizationCode": {
                    "authorizationUrl": "https://auth.example/authorize", "tokenUrl": "https://auth.example/token", "scopes": {"read": "read"}}}},
                "oidc": {"type": "openIdConnect", "openIdConnectUrl": "https://auth.example/.well-known/openid-configuration"}
            }}
        })
        .to_string(),
    )
    .unwrap()
}

async fn check(uri: Uri, headers: HeaderMap) -> impl IntoResponse {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let q = uri.query().unwrap_or_default().to_string();
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("alice:s3cret")
    );
    let ok = match uri.path() {
        "/key-header" => h("x-api-key") == "k-hdr",
        "/key-query" => q == "api_key=k-qry",
        "/key-cookie" => h("cookie") == "session=k-ck",
        "/bearer" => h("authorization") == "Bearer t-bearer",
        "/basic" => h("authorization") == basic,
        "/oauth" => h("authorization") == "Bearer at-oauth",
        "/oidc" => h("authorization") == "Bearer at-oidc",
        "/either" => h("x-api-key") == "k-hdr" || h("authorization") == "Bearer t-bearer",
        "/both" => h("x-api-key") == "k-hdr" && h("authorization") == "Bearer t-bearer",
        "/open" => true,
        _ => false,
    };
    if ok {
        (StatusCode::OK, format!("ok {}", uri.path()))
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized".to_string())
    }
}

async fn mock() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, Router::new().fallback(check)).await;
    });
    base
}

fn secret(s: &str) -> Credential {
    Credential::Secret(s.into())
}

#[tokio::test(flavor = "multi_thread")]
async fn every_scheme_type_against_a_mock() {
    mcpgw_core::tls::install();
    let base = mock().await;
    let a = api(&spec(), Some(&base), None).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let all: Credentials = [
        ("hdr", secret("k-hdr")),
        ("qry", secret("k-qry")),
        ("ck", secret("k-ck")),
        ("bearer", secret("t-bearer")),
        (
            "basic",
            Credential::Basic {
                username: "alice".into(),
                password: "s3cret".into(),
            },
        ),
        ("oauth", secret("at-oauth")),
        ("oidc", secret("at-oidc")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let call = |tool: &'static str, creds: Credentials| {
        let (a, http) = (&a, &http);
        async move {
            let r = a
                .call_with(http, tool, &Map::new(), &RequestExtras::default(), &creds)
                .await
                .unwrap();
            (
                r.get("isError") == Some(&Value::Bool(true)),
                r["content"][0]["text"].as_str().unwrap().to_string(),
            )
        }
    };
    // each scheme type, with its credential: accepted
    for tool in [
        "keyHeader",
        "keyQuery",
        "keyCookie",
        "bearer",
        "basic",
        "oauth",
        "oidc",
        "either",
        "both",
        "open",
    ] {
        let (err, text) = call(tool, all.clone()).await;
        assert!(!err && text.starts_with("ok /"), "{tool}: {text}");
    }
    // without it: refused, with a hint naming the scheme
    let (err, text) = call("keyHeader", Credentials::new()).await;
    assert!(
        err && text.starts_with("HTTP 401") && text.contains("needs credentials for hdr"),
        "{text}"
    );
    // OR: either alternative is enough
    let only_bearer: Credentials = [("bearer".to_string(), secret("t-bearer"))].into();
    assert!(!call("either", only_bearer.clone()).await.0);
    // AND: both schemes are needed
    let (err, text) = call("both", only_bearer).await;
    assert!(
        err && text.contains("needs credentials for hdr + bearer"),
        "{text}"
    );
    // an open operation needs nothing
    assert!(!call("open", Credentials::new()).await.0);
    // wrong credential: refused
    let wrong: Credentials = [("bearer".to_string(), secret("nope"))].into();
    assert!(call("bearer", wrong).await.0);
}
