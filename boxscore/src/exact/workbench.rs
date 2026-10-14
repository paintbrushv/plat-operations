//! The browser workbench `serve` offers at `/`: one HTML page, one
//! stylesheet and one script, embedded in the binary, so the page loads with
//! no network beyond the server itself (the image runs with `--network none`
//! everywhere but `serve`).
//!
//! The page is a thin view over the authenticated `/v1/calc`
//! routes: it never computes a figure, it shows the strings the envelopes
//! carry. Loading the page needs no bearer token; every data call it makes
//! goes through the same authentication, origin check and rate limit as any
//! other client. It is deliberately left out of `/openapi.json`, which is
//! the API contract.
//!
//! Every response carries a strict Content-Security-Policy (`self` only, no
//! inline script or style, no framing, no form submission), `nosniff`,
//! `no-referrer` and `no-store`.
//!
//! `GET /workbench/auth.json` tells the page how this server authenticates
//! ([`auth_document`]). With `--oauth-browser-client-id` the page signs in
//! through the identity provider itself (authorization code with PKCE, as a
//! public client): it navigates to the issuer's authorization endpoint and
//! POSTs the code to its token endpoint, so `connect-src` also names that
//! endpoint's exact URL (not its origin, so the rest of the identity
//! provider's API stays out of reach), and nothing else changes.
use super::mcp_http::{normalize_origin, BrowserOAuth, Config, StartError};
use axum::{
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};

/// The page's Content-Security-Policy: same-origin script, style, fetch and
/// images only; nothing inline, no `<base>`, no form submission, no framing.
/// With browser sign-in, [`content_security_policy`] adds the token
/// endpoint's exact URL to `connect-src`.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Where `serve` answers [`auth_document`].
pub const AUTH_PATH: &str = "/workbench/auth.json";

/// The token endpoint, which the page POSTs the authorization code to, as a
/// CSP source expression naming that one URL: scheme, host, the port when
/// it is not the default, and the path; never a query or fragment. `None`
/// unless the host is an IP literal or LDH labels (ASCII letters, digits,
/// `-` and `.`): a WHATWG host may carry `;`, `,` or `'`, which would end
/// the directive, split the source list or start a keyword. In the path
/// those three are percent-encoded (CSP compares paths percent-decoded).
pub fn token_endpoint_source(token_endpoint: &str) -> Option<String> {
    let url = reqwest::Url::parse(token_endpoint).ok()?;
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let ip_literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok();
    let ldh = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if !ip_literal && !ldh {
        return None;
    }
    let mut source = normalize_origin(&url.origin().ascii_serialization())?;
    for c in url.path().chars() {
        match c {
            ';' => source.push_str("%3B"),
            ',' => source.push_str("%2C"),
            '\'' => source.push_str("%27"),
            c => source.push(c),
        }
    }
    Some(source)
}

/// The page's Content-Security-Policy header: [`CONTENT_SECURITY_POLICY`],
/// with `connect-src 'self' <token endpoint URL>` when the page signs in
/// through the identity provider. Refused, naming the rule, when the token
/// endpoint cannot be named ([`token_endpoint_source`]) or the result is not
/// a valid header value.
pub fn content_security_policy(browser: Option<&BrowserOAuth>) -> Result<HeaderValue, StartError> {
    let Some(browser) = browser else {
        return Ok(HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    };
    let source = token_endpoint_source(&browser.token_endpoint).ok_or_else(|| {
        StartError::new(format!(
            "the token endpoint {} cannot be named in the workbench page's connect-src: its host must be an IP address or DNS labels of ASCII letters, digits, '-' and '.', with no query or fragment",
            browser.token_endpoint
        ))
    })?;
    let policy = CONTENT_SECURITY_POLICY.replacen(
        "connect-src 'self'",
        &format!("connect-src 'self' {source}"),
        1,
    );
    HeaderValue::from_str(&policy).map_err(|_| {
        StartError::new(
            "the workbench page's Content-Security-Policy would not be a valid header value",
        )
    })
}

/// `GET /workbench/auth.json`: how the page authenticates. `mode` is what
/// the validators accept: `none` (a loopback server without one), `api_key`,
/// `oauth`, or `api_key_or_oauth`. With browser sign-in it also carries the
/// issuer, its authorization and token endpoints, the public client id, the
/// scope to request (`openid` and the server's), the audience and the
/// redirect URI (`<public-url>/`). Identifiers only, never a secret.
pub fn auth_document(config: &Config) -> Value {
    let mode = match (
        config.validator.is_some(),
        config.api_keys,
        config.oauth_enabled,
    ) {
        (false, _, _) => "none",
        (true, true, true) => "api_key_or_oauth",
        (true, false, true) => "oauth",
        (true, _, false) => "api_key",
    };
    let mut document = json!({ "mode": mode });
    if let Some(browser) = &config.browser_oauth {
        document["issuer"] = json!(browser.issuer);
        document["authorization_endpoint"] = json!(browser.authorization_endpoint);
        document["token_endpoint"] = json!(browser.token_endpoint);
        document["client_id"] = json!(browser.client_id);
        document["scope"] = json!(browser.scope);
        document["audience"] = json!(browser.audience);
        document["redirect_uri"] = json!(format!("{}/", config.public_url));
    }
    document
}

pub const INDEX_HTML: &str = include_str!("workbench/index.html");
pub const APP_CSS: &str = include_str!("workbench/app.css");
pub const APP_JS: &str = include_str!("workbench/app.js");

/// `GET /`, `GET /app.css`, `GET /app.js` and `GET /workbench/auth.json`,
/// for any router state. `csp` and `auth` are built once, with the router.
pub(crate) fn routes<S>(csp: HeaderValue, auth: &Value) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let auth = auth.to_string();
    let page = |content_type: &'static str, body: &'static str, csp: HeaderValue| {
        get(move || {
            let csp = csp.clone();
            async move { asset(content_type, body, csp) }
        })
    };
    let auth_csp = csp.clone();
    Router::new()
        .route(
            "/",
            page("text/html; charset=utf-8", INDEX_HTML, csp.clone()),
        )
        .route(
            "/app.css",
            page("text/css; charset=utf-8", APP_CSS, csp.clone()),
        )
        .route(
            "/app.js",
            page("text/javascript; charset=utf-8", APP_JS, csp),
        )
        .route(
            AUTH_PATH,
            get(move || {
                let (csp, body) = (auth_csp.clone(), auth.clone());
                async move { asset("application/json", body, csp) }
            }),
        )
}

fn asset(content_type: &'static str, body: impl IntoResponse, csp: HeaderValue) -> Response {
    let headers: [(HeaderName, HeaderValue); 5] = [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (header::CONTENT_SECURITY_POLICY, csp),
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
