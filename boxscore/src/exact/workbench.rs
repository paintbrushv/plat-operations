//! The browser workbench `serve` offers at `/`: one HTML page, one
//! stylesheet and one script, embedded in the binary, so the page loads with
//! no network beyond the server itself (the image runs with `--network none`
//! everywhere but `serve`).
//!
//! The page is a thin, read-only view over the authenticated `/v1/calc`
//! routes: it never computes a figure, it shows the strings the envelopes
//! carry. Loading the page needs no bearer token; every data call it makes
//! goes through the same authentication, origin check and rate limit as any
//! other client. It is deliberately left out of `/openapi.json`, which is
//! the API contract.
//!
//! Every response carries a strict Content-Security-Policy (`self` only, no
//! inline script or style, no framing, no form submission), `nosniff`,
//! `no-referrer` and `no-store`.
use axum::{
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

/// The page's Content-Security-Policy: same-origin script, style, fetch and
/// images only; nothing inline, no `<base>`, no form submission, no framing.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub const INDEX_HTML: &str = include_str!("workbench/index.html");
pub const APP_CSS: &str = include_str!("workbench/app.css");
pub const APP_JS: &str = include_str!("workbench/app.js");

/// `GET /`, `GET /app.css` and `GET /app.js`, for any router state.
pub(crate) fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(stylesheet))
        .route("/app.js", get(script))
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    let headers: [(HeaderName, HeaderValue); 5] = [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
    ];
    (StatusCode::OK, headers, body).into_response()
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

async fn stylesheet() -> Response {
    asset("text/css; charset=utf-8", APP_CSS)
}

async fn script() -> Response {
    asset("text/javascript; charset=utf-8", APP_JS)
}
