//! The web UI (M2-12): three static files under `/ui`.
//!
//! The page holds no secrets, so it is served without the token (behind the
//! loopback Host and Origin checks). It reads the token from the URL
//! fragment (`mcpgw ui` opens `/ui#token=…`; fragments never reach the
//! server) and calls `/api/v1` with it. A strict CSP allows only these
//! files and same-origin requests.

use axum::{
    Router,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

const INDEX: &str = include_str!("ui/index.html");
const CSS: &str = include_str!("ui/app.css");
const JS: &str = include_str!("ui/app.js");

/// Content-Security-Policy of the UI.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                       img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        body,
    )
        .into_response()
}

/// `/ui`, `/ui/app.css`, `/ui/app.js` (public routes).
pub fn routes() -> Router {
    Router::new()
        .route(
            "/ui",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/ui/",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/ui/app.css",
            get(|| async { asset("text/css; charset=utf-8", CSS) }),
        )
        .route(
            "/ui/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", JS) }),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_only_calls_documented_endpoints() {
        // every `api("METHOD", "/path…")` and template in app.js must exist in v1
        let spec = crate::v1::openapi();
        let paths: Vec<String> = spec["paths"].as_object().unwrap().keys().cloned().collect();
        let js = super::JS;
        for needle in [
            "/runtime",
            "/servers",
            "/approvals",
            "/calls",
            "disable",
            "enable",
            "/restart",
            "/approve",
            "/deny",
            "/tools",
            "/call",
        ] {
            assert!(js.contains(needle), "{needle} not used");
            assert!(
                paths.iter().any(|p| p.contains(needle)),
                "{needle} not in the API"
            );
        }
        assert!(
            !super::INDEX.contains("<script>") && !super::INDEX.contains("style="),
            "inline code breaks the CSP"
        );
    }
}
