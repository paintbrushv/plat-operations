//! OAuth 2.1 access-token validation for `boxscore-exact serve`, by RFC 7662
//! token introspection against the configured authorization server.
//!
//! The server is an OAuth resource server only. Sign-in, consent, client
//! registration and token issuance are the authorization server's job (the
//! customer's identity provider). For each bearer token that is not a static
//! API key, the server asks the introspection endpoint whether the token is
//! active, then checks `exp`, `nbf`, the audience and the scope itself.
//!
//! - The token is sent only in the POST body to the introspection endpoint,
//!   with HTTP Basic client authentication. It is never logged or stored:
//!   the verdict cache is keyed by the token's SHA-256.
//! - A positive verdict is cached until the token's `exp`, at most 5 minutes;
//!   a negative one for 30 seconds. The cache holds at most 10 000 entries.
//!   When it is full, expired entries go first, then negative verdicts
//!   oldest first, and positive verdicts only after those, so a flood of
//!   unknown tokens cannot push real users' verdicts out.
//! - Uncached introspection calls are bounded, because any request with an
//!   unknown bearer token would otherwise cost one call to the authorization
//!   server: at most [`MAX_IN_FLIGHT`] (16) are in flight at once
//!   (`IntrospectionSettings::max_in_flight`) and at most
//!   [`CALLS_PER_MINUTE`] (120) are made per minute
//!   (`IntrospectionSettings::calls_per_minute`). A request beyond either
//!   limit is answered `503` without a call.
//! - A transport failure, a non-2xx answer, or an answer that is not a JSON
//!   object with a boolean `active` is `503` (not `401`), logged as one line
//!   on stderr that names the failure, never the token. At most one such
//!   line is printed every 10 seconds; the next printed line counts the
//!   failures suppressed in between.
//!
//! Local JWT validation against the issuer's JWKS (needed for Microsoft
//! Entra ID and Auth0, which offer no RFC 7662 endpoint) is out of scope: it
//! would be another [`TokenValidator`] behind the same seam.
use super::mcp_http::{
    self, read_private_file, sha256, AuthError, Principal, RateLimiter, ServeOptions, StartError,
    TokenValidator, SCOPE,
};
use reqwest::{header, Url};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt::Write as _,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Longest bearer token sent to the introspection endpoint; longer ones are
/// refused (`401`) without a call.
pub const MAX_TOKEN_BYTES: usize = 4096;
/// Whole-request timeout for introspection and metadata discovery.
pub const INTROSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest a positive verdict is cached, even when `exp` is later.
pub const POSITIVE_CACHE_CAP: Duration = Duration::from_secs(300);
/// How long a negative verdict (inactive, expired, wrong audience, missing
/// scope) is cached.
pub const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);
/// Most verdicts held; inserting beyond it evicts expired entries, then the
/// oldest negative verdict, then the oldest positive one.
pub const CACHE_CAPACITY: usize = 10_000;
/// Most introspection calls in flight at once; a request beyond it is `503`.
pub const MAX_IN_FLIGHT: usize = 16;
/// Most introspection calls started per minute (a token bucket with this
/// burst); a request beyond it is `503`.
pub const CALLS_PER_MINUTE: u32 = 120;
/// Shortest interval between two introspection failure lines on stderr.
pub const FAILURE_LOG_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SUBJECT_CHARS: usize = 256;
const MAX_CLIENT_ID_CHARS: usize = 256;
const MAX_AUDIENCE_CHARS: usize = 1024;
const MAX_SECRET_BYTES: usize = 4096;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// The OAuth part of [`ServeOptions`], checked for consistency. Building it
/// does no I/O; the client secret is read by [`OAuthOptions::client_secret`].
#[derive(Debug, Clone)]
pub struct OAuthOptions {
    /// The issuer URL without a trailing slash, as advertised in the
    /// protected-resource metadata.
    pub issuer: String,
    pub introspection_endpoint: Option<Url>,
    pub client_id: String,
    pub client_secret_file: PathBuf,
    pub audience: String,
    pub scope: String,
}

/// `https`, or `http` to a loopback host (a local mock or test IdP): the
/// client secret and tokens must not cross a network in clear text.
fn secure_url(raw: &str, what: &str) -> Result<Url, StartError> {
    let url = mcp_http::http_url(raw, what)?;
    if url.scheme() != "https" && !mcp_http::is_loopback_url(url.as_str()) {
        return Err(StartError::new(format!(
            "{what} must be https (http is accepted for a loopback host only): {raw}"
        )));
    }
    Ok(url)
}

/// RFC 6749 §3.3 `scope-token`: printable ASCII except space, `"` and `\`.
fn valid_scope_token(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 128
        && scope
            .bytes()
            .all(|b| b == 0x21 || (0x23..=0x5B).contains(&b) || (0x5D..=0x7E).contains(&b))
}

fn plain_text(value: &str, max_chars: usize) -> bool {
    let count = value.chars().count();
    count > 0 && count <= max_chars && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

impl OAuthOptions {
    /// `None` when OAuth is not configured. Refuses every inconsistent
    /// combination: an OAuth flag without `--authorization-server`, an
    /// issuer without an audience or client credentials, a client id without
    /// a secret file and the reverse, more than one issuer, a bad scope, or a
    /// non-https URL to a non-loopback host.
    pub fn from_serve(options: &ServeOptions) -> Result<Option<Self>, StartError> {
        let flags = [
            (
                "--introspection-endpoint",
                options.introspection_endpoint.is_some(),
            ),
            ("--oauth-client-id", options.oauth_client_id.is_some()),
            (
                "--oauth-client-secret-file",
                options.oauth_client_secret_file.is_some(),
            ),
            ("--oauth-audience", options.oauth_audience.is_some()),
            ("--oauth-scope", options.oauth_scope.is_some()),
        ];
        let raw_issuer = match options.authorization_servers.as_slice() {
            [] => {
                if let Some((flag, _)) = flags.iter().find(|(_, set)| *set) {
                    return Err(StartError::new(format!(
                        "{flag} needs --authorization-server (the OAuth issuer)"
                    )));
                }
                return Ok(None);
            }
            [one] => one,
            many => {
                return Err(StartError::new(format!(
                    "OAuth validation uses exactly one --authorization-server; {} were given",
                    many.len()
                )))
            }
        };
        let issuer = secure_url(raw_issuer, "--authorization-server")?
            .as_str()
            .trim_end_matches('/')
            .to_string();
        let audience = options.oauth_audience.clone().ok_or_else(|| {
            StartError::new(
                "--authorization-server needs --oauth-audience: the value the authorization server puts in the token's aud (or resource) claim for this server",
            )
        })?;
        if !plain_text(&audience, MAX_AUDIENCE_CHARS) {
            return Err(StartError::new(format!(
                "--oauth-audience must be 1-{MAX_AUDIENCE_CHARS} characters without spaces or control characters"
            )));
        }
        let (client_id, client_secret_file) = match (
            options.oauth_client_id.clone(),
            options.oauth_client_secret_file.clone(),
        ) {
            (Some(id), Some(file)) => (id, file),
            (None, Some(_)) => {
                return Err(StartError::new(
                    "--oauth-client-secret-file needs --oauth-client-id",
                ))
            }
            (Some(_), None) => {
                return Err(StartError::new(
                    "--oauth-client-id needs --oauth-client-secret-file",
                ))
            }
            (None, None) => {
                return Err(StartError::new(
                    "--authorization-server needs --oauth-client-id and --oauth-client-secret-file: the credentials this server presents to the introspection endpoint",
                ))
            }
        };
        if !plain_text(&client_id, MAX_CLIENT_ID_CHARS) {
            return Err(StartError::new(format!(
                "--oauth-client-id must be 1-{MAX_CLIENT_ID_CHARS} characters without spaces or control characters"
            )));
        }
        let scope = options.oauth_scope.clone().unwrap_or_else(|| SCOPE.into());
        if !valid_scope_token(&scope) {
            return Err(StartError::new(
                "--oauth-scope must be one OAuth scope token: 1-128 printable ASCII characters without spaces, quotes or backslashes",
            ));
        }
        let introspection_endpoint = options
            .introspection_endpoint
            .as_deref()
            .map(|raw| secure_url(raw, "--introspection-endpoint"))
            .transpose()?;
        Ok(Some(Self {
            issuer,
            introspection_endpoint,
            client_id,
            client_secret_file,
            audience,
            scope,
        }))
    }

    /// Read the client secret: one line in a file that other users cannot
    /// read (the API key file's rules). Errors never quote the file.
    pub fn client_secret(&self) -> Result<String, StartError> {
        let text = read_private_file(&self.client_secret_file, "OAuth client secret file")?;
        let secret = text.trim();
        if secret.is_empty()
            || secret.len() > MAX_SECRET_BYTES
            || secret.chars().any(char::is_control)
        {
            return Err(StartError::new(format!(
                "OAuth client secret file must hold one line, the client secret (1-{MAX_SECRET_BYTES} bytes)"
            )));
        }
        Ok(secret.to_string())
    }

    /// The validator, once the introspection endpoint is known.
    pub fn validator(&self) -> Result<IntrospectionValidator, StartError> {
        let endpoint = self.introspection_endpoint.clone().ok_or_else(|| {
            StartError::new(
                "OAuth needs an introspection endpoint: pass --introspection-endpoint, or start through serve, which reads it from the issuer's metadata",
            )
        })?;
        IntrospectionValidator::new(IntrospectionSettings::new(
            endpoint,
            self.client_id.clone(),
            self.client_secret()?,
            self.audience.clone(),
            self.scope.clone(),
        ))
    }
}

// ---------------------------------------------------------------------------
// HTTP client and discovery
// ---------------------------------------------------------------------------

/// A client with a whole-request timeout and no redirects (a redirect is an
/// error, so the client secret never follows one). The environment's
/// `HTTPS_PROXY`/`HTTP_PROXY` apply, except to a loopback target.
fn http_client(target: &Url, timeout: Duration) -> Result<reqwest::Client, StartError> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("boxscore-exact/", env!("CARGO_PKG_VERSION")));
    if mcp_http::is_loopback_url(target.as_str()) {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|_| StartError::new("cannot build the HTTP client for OAuth introspection"))
}

/// A failure category for the log; never the error's own text, which could
/// carry a URL or other request detail.
fn transport_reason(error: &reqwest::Error) -> String {
    let reason = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_redirect() {
        "redirect refused"
    } else if error.is_body() || error.is_decode() {
        "response body could not be read"
    } else {
        "request failed"
    };
    reason.to_string()
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport_reason(&e))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("response exceeds 64 KiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The metadata URLs to try for `issuer`, in order. RFC 8414 §3.1 inserts
/// the well-known segment between the host and the issuer's path
/// (`https://host/.well-known/oauth-authorization-server/tenant`); many
/// providers serve it appended instead (`https://host/tenant/.well-known/...`),
/// which is the only form for an issuer without a path.
fn metadata_urls(issuer: &str) -> Result<Vec<Url>, StartError> {
    const WELL_KNOWN: &str = "/.well-known/oauth-authorization-server";
    let what = "authorization server metadata URL";
    let appended = secure_url(&format!("{issuer}{WELL_KNOWN}"), what)?;
    let issuer_url = secure_url(issuer, "--authorization-server")?;
    let path = issuer_url.path().trim_end_matches('/');
    if path.is_empty() {
        return Ok(vec![appended]);
    }
    let mut inserted = issuer_url.clone();
    inserted.set_path(&format!("{WELL_KNOWN}{path}"));
    Ok(vec![inserted, appended])
}

/// GET one metadata URL. `Ok(None)` is a `404`, the only answer after which
/// the next form is tried.
async fn fetch_metadata(client: &reqwest::Client, url: &Url) -> Result<Option<Vec<u8>>, String> {
    let response = client
        .get(url.clone())
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| transport_reason(&e))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("HTTP status {}", status.as_u16()));
    }
    read_capped(response).await.map(Some)
}

/// Fetch the issuer's RFC 8414 metadata once and return its
/// `introspection_endpoint`. For an issuer with a path the RFC 8414 form is
/// tried first and the appended form only after a `404`. The metadata's
/// `issuer` must be the configured one; a missing endpoint refuses to start.
pub async fn discover_introspection_endpoint(issuer: &str) -> Result<Url, StartError> {
    let metadata_error = |url: &Url, why: String| {
        StartError::new(format!(
            "cannot read authorization server metadata from {url}: {why}; pass --introspection-endpoint"
        ))
    };
    let candidates = metadata_urls(issuer)?;
    let mut found = None;
    for (index, url) in candidates.iter().enumerate() {
        let client = http_client(url, INTROSPECTION_TIMEOUT)?;
        match fetch_metadata(&client, url)
            .await
            .map_err(|why| metadata_error(url, why))?
        {
            Some(body) => {
                found = Some((url, body));
                break;
            }
            None if index + 1 < candidates.len() => {}
            None => return Err(metadata_error(url, "HTTP status 404".into())),
        }
    }
    let Some((url, body)) = found else {
        return Err(StartError::new(
            "cannot read authorization server metadata; pass --introspection-endpoint",
        ));
    };
    let failed = |why: String| metadata_error(url, why);
    let metadata: Value =
        serde_json::from_slice(&body).map_err(|_| failed("response is not JSON".into()))?;
    let advertised = metadata
        .get("issuer")
        .and_then(Value::as_str)
        .map(|s| s.trim_end_matches('/'));
    if advertised != Some(issuer) {
        return Err(failed(format!(
            "its issuer is {advertised:?}, not {issuer:?} (RFC 8414 section 3.3)"
        )));
    }
    let endpoint = metadata
        .get("introspection_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("it has no introspection_endpoint".into()))?;
    secure_url(
        endpoint,
        "introspection_endpoint in the authorization server metadata",
    )
}

// ---------------------------------------------------------------------------
// The validator
// ---------------------------------------------------------------------------

/// Everything an [`IntrospectionValidator`] needs. [`IntrospectionSettings::new`]
/// sets the documented timeout and cache limits; tests may shorten them.
#[derive(Clone)]
pub struct IntrospectionSettings {
    pub endpoint: Url,
    pub client_id: String,
    pub client_secret: String,
    pub audience: String,
    pub scope: String,
    pub timeout: Duration,
    pub positive_ttl_cap: Duration,
    pub negative_ttl: Duration,
    pub cache_capacity: usize,
    /// Most introspection calls in flight at once ([`MAX_IN_FLIGHT`]).
    pub max_in_flight: usize,
    /// Most introspection calls started per minute ([`CALLS_PER_MINUTE`]).
    pub calls_per_minute: u32,
}

impl IntrospectionSettings {
    pub fn new(
        endpoint: Url,
        client_id: String,
        client_secret: String,
        audience: String,
        scope: String,
    ) -> Self {
        Self {
            endpoint,
            client_id,
            client_secret,
            audience,
            scope,
            timeout: INTROSPECTION_TIMEOUT,
            positive_ttl_cap: POSITIVE_CACHE_CAP,
            negative_ttl: NEGATIVE_CACHE_TTL,
            cache_capacity: CACHE_CAPACITY,
            max_in_flight: MAX_IN_FLIGHT,
            calls_per_minute: CALLS_PER_MINUTE,
        }
    }
}

impl std::fmt::Debug for IntrospectionSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntrospectionSettings")
            .field("endpoint", &self.endpoint.as_str())
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("audience", &self.audience)
            .field("scope", &self.scope)
            .field("timeout", &self.timeout)
            .field("positive_ttl_cap", &self.positive_ttl_cap)
            .field("negative_ttl", &self.negative_ttl)
            .field("cache_capacity", &self.cache_capacity)
            .field("max_in_flight", &self.max_in_flight)
            .field("calls_per_minute", &self.calls_per_minute)
            .finish()
    }
}

type Verdict = Result<(Principal, u64), AuthError>;

struct Entry {
    verdict: Verdict,
    valid_until: Instant,
    seq: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<[u8; 32], Entry>,
    next_seq: u64,
}

/// When the last failure line was printed, and how many were suppressed
/// since.
#[derive(Default)]
struct FailureLog {
    last: Option<Instant>,
    suppressed: u64,
}

/// One introspection call in flight; dropping it (also when the request is
/// cancelled) frees the slot.
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The single rate-limiter key: the limit is on all uncached calls together.
const LIMITER_KEY: &str = "introspection";

/// Validates OAuth access tokens by RFC 7662 introspection, with a verdict
/// cache keyed by the token's SHA-256 and bounded outbound calls.
pub struct IntrospectionValidator {
    settings: IntrospectionSettings,
    basic_user: String,
    basic_password: String,
    client: reqwest::Client,
    cache: Mutex<Cache>,
    in_flight: AtomicUsize,
    limiter: RateLimiter,
    failure_log: Mutex<FailureLog>,
}

impl std::fmt::Debug for IntrospectionValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntrospectionValidator")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// `application/x-www-form-urlencoded`, as RFC 6749 §2.3.1 requires for the
/// client id and secret inside HTTP Basic credentials.
fn form_urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// RFC 6750 `b64token`. Anything else is not a bearer token and is refused
/// without a call.
fn is_b64token(token: &str) -> bool {
    let body = token.trim_end_matches('=');
    !body.is_empty()
        && body.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
}

/// Seconds since the epoch; a clock before 1970 reads as the end of time,
/// so every token is expired rather than every token valid.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs())
}

/// An RFC 7519 NumericDate: a non-negative number of seconds.
fn numeric_date(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            // Truncation is the intent: whole seconds.
            .map(|f| f as u64)
    })
}

fn names(value: Option<&Value>, audience: &str) -> bool {
    match value {
        Some(Value::String(s)) => s == audience,
        Some(Value::Array(items)) => items.iter().any(|i| i.as_str() == Some(audience)),
        _ => false,
    }
}

/// A member that is present and not `null`.
fn present<'a>(response: &'a Value, key: &str) -> Option<&'a Value> {
    response.get(key).filter(|v| !v.is_null())
}

/// The response names a token that is not an access token: a `token_type`
/// other than `bearer`/`access_token`, or a `typ` naming a refresh or ID
/// token (Keycloak's `Refresh`, `ID`).
fn not_an_access_token(response: &Value) -> bool {
    let lower = |key: &str| present(response, key).map(|v| v.as_str().map(str::to_ascii_lowercase));
    let wrong_type = match lower("token_type") {
        None => false,
        Some(Some(t)) => t != "bearer" && t != "access_token",
        Some(None) => true,
    };
    let wrong_typ = matches!(
        lower("typ"),
        Some(Some(t)) if matches!(t.as_str(), "refresh" | "refresh_token" | "id" | "id_token")
    );
    wrong_type || wrong_typ
}

/// Judge an introspection response that has a boolean `active`. Accepted
/// only if `active` is true, the token is an access token (`token_type`,
/// if present, is `bearer` or `access_token`; `typ` does not name a refresh
/// or ID token), `exp` is present and in the future, `nbf` (if present) is
/// not in the future, `aud` contains the audience (`resource` is consulted
/// only when `aud` is absent or null), and `scope` contains the required scope (else
/// `InsufficientScope`). The subject is `sub`, else `client_id`.
fn evaluate(response: &Value, now: u64, audience: &str, scope: &str) -> Verdict {
    if response.get("active") != Some(&Value::Bool(true)) {
        return Err(AuthError::Invalid);
    }
    if not_an_access_token(response) {
        return Err(AuthError::Invalid);
    }
    let exp = response
        .get("exp")
        .and_then(numeric_date)
        .ok_or(AuthError::Invalid)?;
    if exp <= now {
        return Err(AuthError::Invalid);
    }
    if let Some(nbf) = response.get("nbf") {
        if numeric_date(nbf).is_none_or(|nbf| nbf > now) {
            return Err(AuthError::Invalid);
        }
    }
    // A present but wrong aud is a refusal, whatever resource says.
    let audience_claim = present(response, "aud").or_else(|| present(response, "resource"));
    if !names(audience_claim, audience) {
        return Err(AuthError::Invalid);
    }
    let granted = response.get("scope").and_then(Value::as_str).unwrap_or("");
    if !granted.split_ascii_whitespace().any(|s| s == scope) {
        return Err(AuthError::InsufficientScope);
    }
    let subject = ["sub", "client_id"]
        .iter()
        .filter_map(|key| response.get(*key).and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .ok_or(AuthError::Invalid)?;
    if subject.chars().count() > MAX_SUBJECT_CHARS || subject.chars().any(char::is_control) {
        return Err(AuthError::Invalid);
    }
    Ok((
        Principal {
            subject: subject.to_string(),
            method: "oauth",
        },
        exp,
    ))
}

impl IntrospectionValidator {
    pub fn new(settings: IntrospectionSettings) -> Result<Self, StartError> {
        let client = http_client(&settings.endpoint, settings.timeout)?;
        Ok(Self {
            basic_user: form_urlencode(&settings.client_id),
            basic_password: form_urlencode(&settings.client_secret),
            in_flight: AtomicUsize::new(0),
            limiter: RateLimiter::new(settings.calls_per_minute),
            failure_log: Mutex::new(FailureLog::default()),
            settings,
            client,
            cache: Mutex::new(Cache::default()),
        })
    }

    /// Take an in-flight slot, or `None` when [`IntrospectionSettings::max_in_flight`]
    /// calls are already running.
    fn admit(&self) -> Option<InFlight<'_>> {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.settings.max_in_flight {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(InFlight(&self.in_flight)),
                Err(actual) => current = actual,
            }
        }
    }

    /// The failure line to print at `now`, or `None` when one was printed
    /// less than [`FAILURE_LOG_INTERVAL`] ago (the failure is then counted
    /// and reported by the next printed line). Names the endpoint and the
    /// failure, never the token or the secret.
    fn failure_line(&self, reason: &str, now: Instant) -> Option<String> {
        let mut log = self.failure_log.lock().unwrap_or_else(|e| e.into_inner());
        if log
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < FAILURE_LOG_INTERVAL)
        {
            log.suppressed = log.suppressed.saturating_add(1);
            return None;
        }
        let suppressed = std::mem::take(&mut log.suppressed);
        log.last = Some(now);
        let note = if suppressed > 0 {
            format!(" ({suppressed} similar failures suppressed)")
        } else {
            String::new()
        };
        Some(format!(
            "boxscore-exact serve: OAuth introspection at {} failed ({reason}); answered 503{note}",
            self.settings.endpoint
        ))
    }

    fn log_failure(&self, reason: &str) {
        if let Some(line) = self.failure_line(reason, Instant::now()) {
            eprintln!("{line}");
        }
    }

    /// Number of cached verdicts (expired ones included until evicted).
    pub fn cached_entries(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    fn lookup(&self, key: &[u8; 32]) -> Option<Result<Principal, AuthError>> {
        let now = Instant::now();
        let now_unix = unix_now();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let entry = cache.entries.get(key)?;
        // A positive verdict also dies with the token's own exp.
        let live = entry.valid_until > now
            && entry
                .verdict
                .as_ref()
                .map_or(true, |(_, exp)| *exp > now_unix);
        if live {
            Some(entry.verdict.clone().map(|(principal, _)| principal))
        } else {
            cache.entries.remove(key);
            None
        }
    }

    fn store(&self, key: [u8; 32], verdict: &Verdict, now_unix: u64) {
        let ttl = match verdict {
            Ok((_, exp)) => Duration::from_secs(exp.saturating_sub(now_unix))
                .min(self.settings.positive_ttl_cap),
            Err(AuthError::Invalid | AuthError::InsufficientScope) => self.settings.negative_ttl,
            Err(AuthError::Missing | AuthError::Unavailable) => return,
        };
        let capacity = self.settings.cache_capacity;
        if ttl.is_zero() || capacity == 0 {
            return;
        }
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.entries.contains_key(&key) && cache.entries.len() >= capacity {
            cache.entries.retain(|_, entry| entry.valid_until > now);
            while cache.entries.len() >= capacity {
                // Negative verdicts (false < true) go before positive ones,
                // each oldest first.
                let oldest = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| (entry.verdict.is_ok(), entry.seq))
                    .map(|(k, _)| *k);
                match oldest {
                    Some(oldest) => {
                        cache.entries.remove(&oldest);
                    }
                    None => break,
                }
            }
        }
        let seq = cache.next_seq;
        cache.next_seq = seq.wrapping_add(1);
        cache.entries.insert(
            key,
            Entry {
                verdict: verdict.clone(),
                valid_until: now + ttl,
                seq,
            },
        );
    }

    /// One introspection call. `Err` is a failure category for the log.
    async fn introspect(&self, token: &str) -> Result<Value, String> {
        let response = self
            .client
            .post(self.settings.endpoint.clone())
            .basic_auth(&self.basic_user, Some(&self.basic_password))
            .header(header::ACCEPT, "application/json")
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|e| transport_reason(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP status {}", status.as_u16()));
        }
        let body = read_capped(response).await?;
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| "response is not JSON".to_string())?;
        if !value.get("active").is_some_and(Value::is_boolean) {
            return Err("response has no boolean active".into());
        }
        Ok(value)
    }
}

#[async_trait::async_trait]
impl TokenValidator for IntrospectionValidator {
    async fn validate(&self, token: &str) -> Result<Principal, AuthError> {
        if token.len() > MAX_TOKEN_BYTES || !is_b64token(token) {
            return Err(AuthError::Invalid);
        }
        let key = sha256(token.as_bytes());
        if let Some(cached) = self.lookup(&key) {
            return cached;
        }
        // Bound the calls an unauthenticated caller can cause: a slot first,
        // then a token from the per-minute budget, so a refused slot costs
        // no budget.
        let Some(_slot) = self.admit() else {
            self.log_failure(&format!(
                "not called: {} calls already in flight",
                self.settings.max_in_flight
            ));
            return Err(AuthError::Unavailable);
        };
        if self.limiter.check(LIMITER_KEY).is_err() {
            self.log_failure(&format!(
                "not called: limit of {} calls per minute reached",
                self.settings.calls_per_minute
            ));
            return Err(AuthError::Unavailable);
        }
        let response = match self.introspect(token).await {
            Ok(response) => response,
            Err(reason) => {
                self.log_failure(&reason);
                return Err(AuthError::Unavailable);
            }
        };
        let now_unix = unix_now();
        let verdict = evaluate(
            &response,
            now_unix,
            &self.settings.audience,
            &self.settings.scope,
        );
        self.store(key, &verdict, now_unix);
        verdict.map(|(principal, _)| principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AUD: &str = "https://boxscore.example.test";

    fn token(extra: Value) -> Value {
        let mut base = json!({"active": true, "exp": 2_000, "sub": "user-1",
                              "aud": AUD, "scope": "openid boxscore:calc"});
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        base
    }

    fn judge(response: Value) -> Verdict {
        evaluate(&response, 1_000, AUD, SCOPE)
    }

    #[test]
    fn evaluation_rules() {
        let (principal, exp) = judge(token(json!({}))).unwrap();
        assert_eq!(principal.subject, "user-1");
        assert_eq!(principal.method, "oauth");
        assert_eq!(principal.caller(), "http:oauth:user-1");
        assert_eq!(exp, 2_000);
        assert_eq!(
            judge(token(json!({"active": false}))),
            Err(AuthError::Invalid)
        );
        assert_eq!(judge(token(json!({"exp": 1_000}))), Err(AuthError::Invalid));
        assert_eq!(judge(token(json!({"exp": null}))), Err(AuthError::Invalid));
        assert_eq!(judge(token(json!({"exp": -5}))), Err(AuthError::Invalid));
        assert!(judge(token(json!({"exp": 1_500.7}))).is_ok());
        assert_eq!(judge(token(json!({"nbf": 1_001}))), Err(AuthError::Invalid));
        assert_eq!(judge(token(json!({"nbf": "x"}))), Err(AuthError::Invalid));
        assert!(judge(token(json!({"nbf": 1_000}))).is_ok());
        assert_eq!(
            judge(token(json!({"aud": "https://other.example"}))),
            Err(AuthError::Invalid)
        );
        assert!(judge(token(json!({"aud": ["x", AUD]}))).is_ok());
        assert!(judge(token(json!({"aud": null, "resource": AUD}))).is_ok());
        let mut no_aud = token(json!({"resource": AUD}));
        no_aud.as_object_mut().unwrap().remove("aud");
        assert!(judge(no_aud).is_ok(), "resource is used when aud is absent");
        // A present but wrong aud is refused even when resource matches.
        assert_eq!(
            judge(token(
                json!({"aud": "https://other.example", "resource": AUD})
            )),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            judge(token(json!({"aud": ["x"], "resource": [AUD]}))),
            Err(AuthError::Invalid)
        );
        assert_eq!(judge(token(json!({"aud": null}))), Err(AuthError::Invalid));
        assert_eq!(
            judge(token(json!({"scope": "openid boxscore:calc:extra"}))),
            Err(AuthError::InsufficientScope)
        );
        assert_eq!(
            judge(token(json!({"scope": null}))),
            Err(AuthError::InsufficientScope)
        );
        // A wrong audience is a 401, even when the scope is also missing.
        assert_eq!(
            judge(token(json!({"aud": "x", "scope": ""}))),
            Err(AuthError::Invalid)
        );
        let (client, _) = judge(token(json!({"sub": "", "client_id": "svc"}))).unwrap();
        assert_eq!(client.subject, "svc");
        assert_eq!(judge(token(json!({"sub": null}))), Err(AuthError::Invalid));
        assert_eq!(
            judge(token(json!({"sub": "a\nb"}))),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            judge(token(json!({"sub": "s".repeat(257)}))),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn only_access_tokens_are_accepted() {
        for accepted in [
            json!({"token_type": "Bearer"}),
            json!({"token_type": "bearer"}),
            json!({"token_type": "ACCESS_TOKEN"}),
            json!({"token_type": null}),
            json!({"typ": "Bearer"}),
            json!({"typ": "at+jwt"}),
        ] {
            assert!(judge(token(accepted.clone())).is_ok(), "{accepted}");
        }
        for refused in [
            json!({"token_type": "refresh_token"}),
            json!({"token_type": "id_token"}),
            json!({"token_type": "DPoP"}),
            json!({"token_type": 1}),
            json!({"typ": "Refresh"}),
            json!({"typ": "refresh_token"}),
            json!({"typ": "ID"}),
            json!({"typ": "id_token"}),
            json!({"token_type": "Bearer", "typ": "Refresh"}),
        ] {
            assert_eq!(
                judge(token(refused.clone())),
                Err(AuthError::Invalid),
                "{refused}"
            );
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(form_urlencode("a-b_c.d*"), "a-b_c.d*");
        assert_eq!(form_urlencode("a b:c/é"), "a+b%3Ac%2F%C3%A9");
        assert!(is_b64token("abc.DEF-_~+/=="));
        assert!(!is_b64token("=="));
        assert!(!is_b64token("a b"));
        assert!(!is_b64token("a\"b"));
        assert!(valid_scope_token("boxscore:calc"));
        assert!(!valid_scope_token("a b"));
        assert!(!valid_scope_token("a\"b"));
        assert!(!valid_scope_token(""));
    }

    fn validator(capacity: usize) -> IntrospectionValidator {
        let mut settings = IntrospectionSettings::new(
            Url::parse("http://127.0.0.1:9/introspect").unwrap(),
            "client".into(),
            "synthetic-client-secret".into(),
            AUD.into(),
            SCOPE.into(),
        );
        settings.cache_capacity = capacity;
        settings.max_in_flight = 2;
        IntrospectionValidator::new(settings).unwrap()
    }

    fn positive(exp: u64) -> Verdict {
        Ok((
            Principal {
                subject: "u".into(),
                method: "oauth",
            },
            exp,
        ))
    }

    #[test]
    fn eviction_spares_positive_verdicts_until_no_negative_is_left() {
        let v = validator(3);
        let now = unix_now();
        v.store([1; 32], &positive(now + 60), now);
        v.store([2; 32], &Err(AuthError::Invalid), now);
        v.store([3; 32], &Err(AuthError::InsufficientScope), now);
        // A flood of refused tokens evicts only refused tokens.
        for i in 10..20 {
            v.store([i; 32], &Err(AuthError::Invalid), now);
        }
        assert_eq!(v.cached_entries(), 3);
        assert!(v.lookup(&[1; 32]).unwrap().is_ok(), "positive kept");
        assert!(v.lookup(&[2; 32]).is_none() && v.lookup(&[3; 32]).is_none());
        assert_eq!(v.lookup(&[19; 32]), Some(Err(AuthError::Invalid)));
        // With no negative verdict left, the oldest positive one goes.
        let v = validator(2);
        v.store([1; 32], &positive(now + 60), now);
        v.store([2; 32], &positive(now + 60), now);
        v.store([3; 32], &positive(now + 60), now);
        assert!(v.lookup(&[1; 32]).is_none());
        assert!(v.lookup(&[2; 32]).is_some() && v.lookup(&[3; 32]).is_some());
    }

    #[test]
    fn in_flight_slots_are_bounded_and_released() {
        let v = validator(2);
        let a = v.admit().unwrap();
        let b = v.admit().unwrap();
        assert!(v.admit().is_none(), "max_in_flight is 2");
        drop(a);
        let c = v.admit().unwrap();
        assert!(v.admit().is_none());
        drop((b, c));
        assert_eq!(v.in_flight.load(Ordering::Acquire), 0);
    }

    #[test]
    fn failure_lines_are_throttled() {
        let v = validator(2);
        let t0 = Instant::now();
        let first = v.failure_line("timed out", t0).unwrap();
        assert!(
            first.ends_with("failed (timed out); answered 503"),
            "{first}"
        );
        assert!(v
            .failure_line("timed out", t0 + Duration::from_secs(1))
            .is_none());
        assert!(v
            .failure_line("timed out", t0 + Duration::from_secs(9))
            .is_none());
        let next = v
            .failure_line("connection failed", t0 + Duration::from_secs(10))
            .unwrap();
        assert!(
            next.ends_with("(connection failed); answered 503 (2 similar failures suppressed)"),
            "{next}"
        );
        let quiet = v
            .failure_line("timed out", t0 + Duration::from_secs(25))
            .unwrap();
        assert!(!quiet.contains("suppressed"), "{quiet}");
    }

    #[test]
    fn metadata_url_forms() {
        let urls = |issuer: &str| -> Vec<String> {
            metadata_urls(issuer)
                .unwrap()
                .iter()
                .map(|u| u.to_string())
                .collect()
        };
        assert_eq!(
            urls("https://idp.example.test"),
            ["https://idp.example.test/.well-known/oauth-authorization-server"]
        );
        assert_eq!(
            urls("https://idp.example.test/realms/acme"),
            [
                "https://idp.example.test/.well-known/oauth-authorization-server/realms/acme",
                "https://idp.example.test/realms/acme/.well-known/oauth-authorization-server",
            ]
        );
    }

    #[test]
    fn cache_is_bounded() {
        let v = validator(2);
        let now = unix_now();
        let ok: Verdict = Ok((
            Principal {
                subject: "u".into(),
                method: "oauth",
            },
            now + 60,
        ));
        v.store([1; 32], &ok, now);
        v.store([2; 32], &Err(AuthError::Invalid), now);
        v.store([3; 32], &ok, now);
        assert_eq!(v.cached_entries(), 2);
        // The negative verdict goes before the older positive one.
        assert!(v.lookup(&[2; 32]).is_none(), "negative evicted first");
        assert_eq!(v.lookup(&[1; 32]).unwrap().unwrap().subject, "u");
        assert_eq!(v.lookup(&[3; 32]).unwrap().unwrap().subject, "u");
        // Transport failures and already-expired tokens are not cached.
        v.store([4; 32], &Err(AuthError::Unavailable), now);
        assert!(v.lookup(&[4; 32]).is_none());
        let expired: Verdict = Ok((
            Principal {
                subject: "u".into(),
                method: "oauth",
            },
            now,
        ));
        v.store([5; 32], &expired, now);
        assert!(v.lookup(&[5; 32]).is_none());
        assert!(!format!("{v:?}").contains("synthetic-client-secret"));
    }
}
