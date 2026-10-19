//! Host, Origin and token checks on the HTTP edge.

use axum::{Router, routing::post};
use mcpgw_api::{EdgeOptions, edge};
use mcpgw_auth::Token;
use reqwest::StatusCode;

async fn start() -> (u16, Token) {
    let token = Token::generate().unwrap();
    let mcp = Router::new().route("/", post(|| async { "mcp ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = edge(
        mcp,
        EdgeOptions {
            token: token.clone(),
            port,
            allowed_origins: vec!["http://localhost:5173".into()],
        },
    );
    tokio::spawn(mcpgw_api::serve(listener, app, std::future::pending()));
    (port, token)
}

#[tokio::test]
async fn status_codes() {
    let (port, token) = start().await;
    let c = reqwest::Client::new();
    let bearer = format!("Bearer {}", token.expose());
    let mcp = format!("http://127.0.0.1:{port}/mcp");
    let post = |origin: Option<&str>, host: Option<&str>, auth: Option<&str>| {
        let mut r = c.post(&mcp);
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        if let Some(h) = host {
            r = r.header("host", h);
        }
        if let Some(a) = auth {
            r = r.header("authorization", a);
        }
        r.send()
    };

    let r = post(None, None, None).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()["www-authenticate"], "Bearer realm=\"mcpgw\"");
    let r = post(None, None, Some("Bearer nope")).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let r = post(None, None, Some(&bearer)).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.text().await.unwrap(), "mcp ok");

    // a web page on another origin is refused even with the token
    let r = post(Some("https://evil.example"), None, Some(&bearer))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = post(Some("null"), None, Some(&bearer)).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let own = format!("http://127.0.0.1:{port}");
    for origin in [own.as_str(), "http://localhost:5173"] {
        let r = post(Some(origin), None, Some(&bearer)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{origin}");
    }

    // DNS rebinding: a non-loopback Host is refused before auth
    let r = post(None, Some("evil.example:7777"), Some(&bearer))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = post(None, Some(&format!("localhost:{port}")), Some(&bearer))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // health check needs no token, but still a loopback Host
    let health = format!("http://127.0.0.1:{port}/healthz");
    assert_eq!(
        c.get(&health).send().await.unwrap().status(),
        StatusCode::OK
    );
    let r = c
        .get(&health)
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}
