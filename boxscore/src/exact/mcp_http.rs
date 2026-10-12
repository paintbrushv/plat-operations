//! MCP Streamable HTTP (2025-06-18) and a plain JSON API for the calculation
//! catalog, on axum, in the `boxscore-exact` binary.
//!
//! The transport is deliberately thin and reuses the stdio dispatcher
//! ([`mcp::Server::handle`]) unchanged, so the HTTP and stdio servers answer
//! the same message with the same envelope:
//!
//! - `POST /mcp` takes one JSON-RPC message and answers `application/json`
//!   (no SSE streaming, no sessions); a notification gets `202 Accepted`.
//! - `POST /v1/calc/{function}` runs one catalog function with the request
//!   body as its input and returns the envelope (200, or 422 when refused).
//! - `GET /health`, `GET /openapi.json` and
//!   `GET /.well-known/oauth-protected-resource` (RFC 9728) are public.
//!
//! Everything else needs `Authorization: Bearer <token>`, checked by a
//! [`TokenValidator`]: [`StaticKeys`] (API keys for scripts) and, for
//! claude.ai and ChatGPT connectors, OAuth 2.1 access tokens checked by RFC
//! 7662 introspection against the configured authorization server
//! ([`oauth::IntrospectionValidator`]). When both are configured the static
//! keys are tried first ([`ValidatorChain`]). A server without a validator
//! answers loopback only: [`prepare`] refuses to bind anything else.
//!
//! The database path comes from the command line, never from a request. The
//! server writes no artifacts and logs no request content, token or principal.
use super::{catalog, digest, mcp, oauth, protocol::MAX_REQUEST_BYTES};
use axum::{
    body::Bytes,
    extract::{rejection::BytesRejection, DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const DEFAULT_BIND: &str = "127.0.0.1:3819";
pub const DEFAULT_RATE_LIMIT: u32 = 60;
pub const SCOPE: &str = "boxscore:calc";
pub const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
/// The RFC 9728 path-suffixed variant for clients that use the MCP endpoint
/// (`<public-url>/mcp`) as the resource identifier.
pub const METADATA_MCP_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
/// Seconds a client is asked to wait when the authorization server cannot be
/// reached (`503 Retry-After`).
pub const AUTH_UNAVAILABLE_RETRY_SECONDS: u64 = 5;
/// Shortest accepted API-key secret, in characters.
pub const MIN_SECRET_LEN: usize = 32;
const MAX_KEY_FILE_BYTES: u64 = 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Who made a request. Never logged; carried to the handler so the call
/// record can name the caller ([`Principal::caller`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub subject: String,
    /// `"api_key"` for [`StaticKeys`], `"oauth"` for an introspected access
    /// token; `"none"` when the server runs without a validator on loopback.
    pub method: &'static str,
}

impl Principal {
    /// The trace caller and rate-limit key: `http:<key_id>` for an API key
    /// (`http:local` without a validator), `http:oauth:<subject>` for an
    /// OAuth access token. Key ids cannot contain `:`, so the two never
    /// collide.
    pub fn caller(&self) -> String {
        match self.method {
            "oauth" => format!("http:oauth:{}", self.subject),
            _ => format!("http:{}", self.subject),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// No `Authorization: Bearer` header.
    Missing,
    /// A token was presented and not accepted.
    Invalid,
    /// The token is valid but lacks the required scope (`403`).
    InsufficientScope,
    /// The token could not be checked: the authorization server was
    /// unreachable or answered something unusable (`503`, not `401`).
    Unavailable,
}

/// Turns a bearer token into a [`Principal`]. Implemented by [`StaticKeys`]
/// and [`oauth::IntrospectionValidator`]. Local JWT validation against the
/// authorization server's JWKS (what Microsoft Entra ID and Auth0 need) would
/// be a third implementation behind this seam; it is not written.
#[allow(clippy::double_must_use)] // async_trait adds must_use to an already must_use Future.
#[async_trait::async_trait]
pub trait TokenValidator: Send + Sync {
    async fn validate(&self, token: &str) -> Result<Principal, AuthError>;
}

/// Several validators tried in order: the first that accepts the token wins.
/// When none does, the last validator's error is returned, so with
/// `[StaticKeys, IntrospectionValidator]` a token that is not an API key gets
/// the introspection verdict (`401`, `403` or `503`).
pub struct ValidatorChain(pub Vec<Arc<dyn TokenValidator>>);

#[async_trait::async_trait]
impl TokenValidator for ValidatorChain {
    async fn validate(&self, token: &str) -> Result<Principal, AuthError> {
        let mut last = AuthError::Invalid;
        for validator in &self.0 {
            match validator.validate(token).await {
                Ok(principal) => return Ok(principal),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}

struct StaticKey {
    id: String,
    secret_sha256: [u8; 32],
}

/// API keys for scripts, loaded from a `key_id:secret` file. Secrets are
/// held only as SHA-256 digests and compared in constant time; every key is
/// compared on every request, so timing reveals neither which key matched
/// nor how much of a guess was right.
pub struct StaticKeys {
    keys: Vec<StaticKey>,
}

impl std::fmt::Debug for StaticKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.keys.iter().map(|k| k.id.as_str()).collect();
        f.debug_struct("StaticKeys").field("key_ids", &ids).finish()
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Constant-time equality for equal-length byte strings. Different lengths
/// return `false` immediately; callers compare fixed-length digests only.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff = std::hint::black_box(diff | (x ^ y));
    }
    diff == 0
}

fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// RFC 6750 `b64token` characters, so the secret can be sent as a bearer token.
fn valid_secret(secret: &str) -> bool {
    secret.len() >= MIN_SECRET_LEN
        && secret.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
        })
}

impl StaticKeys {
    /// Parse `key_id:secret` lines. Blank lines and lines starting with `#`
    /// are skipped; surrounding whitespace (including a trailing `\r`) is
    /// ignored. Errors name the line number, never the secret.
    pub fn parse(text: &str) -> Result<Self, StartError> {
        let mut keys: Vec<StaticKey> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line_no = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((id, secret)) = line.split_once(':') else {
                return Err(StartError::new(format!(
                    "API key file line {line_no}: expected key_id:secret"
                )));
            };
            let (id, secret) = (id.trim(), secret.trim());
            if !valid_key_id(id) {
                return Err(StartError::new(format!(
                    "API key file line {line_no}: key_id must be 1-64 characters of A-Z a-z 0-9 . _ -"
                )));
            }
            if !valid_secret(secret) {
                return Err(StartError::new(format!(
                    "API key file line {line_no}: secret must be at least {MIN_SECRET_LEN} characters of A-Z a-z 0-9 - . _ ~ + / ="
                )));
            }
            let secret_sha256 = sha256(secret.as_bytes());
            if keys.iter().any(|k| k.id == id) {
                return Err(StartError::new(format!(
                    "API key file line {line_no}: duplicate key_id {id}"
                )));
            }
            if keys
                .iter()
                .any(|k| constant_time_eq(&k.secret_sha256, &secret_sha256))
            {
                return Err(StartError::new(format!(
                    "API key file line {line_no}: secret reused by another key"
                )));
            }
            keys.push(StaticKey {
                id: id.to_string(),
                secret_sha256,
            });
        }
        if keys.is_empty() {
            return Err(StartError::new("API key file contains no keys"));
        }
        Ok(Self { keys })
    }

    /// Load a key file. On unix the file must not be accessible to other
    /// users (mode 0600 expected); a world-readable file refuses to start.
    pub fn from_file(path: &FsPath) -> Result<Self, StartError> {
        Self::parse(&read_private_file(path, "API key file")?)
    }

    /// The constant-time check behind [`TokenValidator::validate`]: every key
    /// is compared on every call.
    pub fn check(&self, token: &str) -> Result<Principal, AuthError> {
        let presented = sha256(token.as_bytes());
        let mut matched: Option<&str> = None;
        for key in &self.keys {
            if constant_time_eq(&key.secret_sha256, &presented) {
                matched = Some(&key.id);
            }
        }
        matched
            .map(|id| Principal {
                subject: id.to_string(),
                method: "api_key",
            })
            .ok_or(AuthError::Invalid)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[async_trait::async_trait]
impl TokenValidator for StaticKeys {
    async fn validate(&self, token: &str) -> Result<Principal, AuthError> {
        self.check(token)
    }
}

/// Read a secret-bearing file; `label` names it in errors, which never quote
/// its contents. On unix the file must not be accessible to other users
/// (mode 0600 expected; a world-readable file refuses to start) and group
/// access prints a warning. At most 1 MiB of UTF-8 text.
pub(crate) fn read_private_file(path: &FsPath, label: &str) -> Result<String, StartError> {
    use std::io::Read;
    let unreadable = || StartError::new(format!("{label} cannot be read"));
    // Refuse a FIFO or device before opening it (opening a FIFO blocks);
    // the checks that matter are repeated on the open handle below.
    if !std::fs::metadata(path).map_err(|_| unreadable())?.is_file() {
        return Err(StartError::new(format!("{label} is not a regular file")));
    }
    // Open first, then check the open handle's metadata, so the mode check
    // and the read refer to the same file even if the path is swapped.
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.is_file() {
        return Err(StartError::new(format!("{label} is not a regular file")));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o007 != 0 {
            return Err(StartError::new(format!(
                "{label} is accessible to other users (mode {:o}); run chmod 600 on it",
                mode & 0o777
            )));
        }
        if mode & 0o070 != 0 {
            eprintln!(
                "boxscore-exact serve: warning: {label} is group-accessible (mode {:o}); 0600 is expected",
                mode & 0o777
            );
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
        return Err(StartError::new(format!("{label} exceeds 1 MiB")));
    }
    String::from_utf8(bytes).map_err(|_| StartError::new(format!("{label} is not UTF-8 text")))
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AuthError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(AuthError::Missing)?
        .to_str()
        .map_err(|_| AuthError::Invalid)?;
    let (scheme, token) = value.trim().split_once(' ').ok_or(AuthError::Invalid)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthError::Invalid);
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(AuthError::Invalid);
    }
    Ok(token)
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// A token bucket per principal: `per_minute` requests of burst, refilled
/// continuously at `per_minute` per minute.
pub struct RateLimiter {
    capacity: f64,
    per_second: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        let capacity = f64::from(per_minute.max(1));
        Self {
            capacity,
            per_second: capacity / 60.0,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `key` at `now`; `Err` carries the wait until the
    /// next token.
    pub fn check_at(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: self.capacity,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_second).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64(
                (1.0 - bucket.tokens) / self.per_second,
            ))
        }
    }

    pub fn check(&self, key: &str) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }
}

// ---------------------------------------------------------------------------
// Configuration and start-up checks
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartError(String);

impl StartError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Everything the router needs. Built by [`prepare`] from the command line,
/// or directly by tests.
#[derive(Clone)]
pub struct Config {
    pub database: Option<PathBuf>,
    /// `None` only for a loopback server started without `--api-key-file`
    /// or OAuth.
    pub validator: Option<Arc<dyn TokenValidator>>,
    /// Serialized origins (`scheme://host[:port]`) allowed to send `Origin`.
    pub allowed_origins: Vec<String>,
    /// The resource identifier, without a trailing slash.
    pub public_url: String,
    pub authorization_servers: Vec<String>,
    pub rate_limit_per_minute: u32,
    /// The OAuth scope a token must carry: advertised in the metadata and
    /// the OpenAPI document and named in the `insufficient_scope` challenge.
    /// [`SCOPE`] unless `--oauth-scope` says otherwise.
    pub scope: String,
    /// OAuth access-token validation is configured: the OpenAPI document
    /// then advertises the `oauth2` security scheme.
    pub oauth_enabled: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database", &self.database)
            .field("authenticated", &self.validator.is_some())
            .field("allowed_origins", &self.allowed_origins)
            .field("public_url", &self.public_url)
            .field("authorization_servers", &self.authorization_servers)
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("scope", &self.scope)
            .field("oauth_enabled", &self.oauth_enabled)
            .finish()
    }
}

/// The command-line options of `boxscore-exact serve`. Holds paths and
/// identifiers only, never a secret.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: String,
    pub database: Option<PathBuf>,
    pub api_key_file: Option<PathBuf>,
    pub allow_origins: Vec<String>,
    pub public_url: Option<String>,
    /// The OAuth issuer. With the `oauth_*` options it also enables OAuth
    /// access-token validation by introspection.
    pub authorization_servers: Vec<String>,
    pub rate_limit: u32,
    /// RFC 7662 endpoint; when absent, [`resolve`] discovers it from the
    /// issuer's RFC 8414 metadata once at start-up.
    pub introspection_endpoint: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret_file: Option<PathBuf>,
    pub oauth_audience: Option<String>,
    pub oauth_scope: Option<String>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.into(),
            database: None,
            api_key_file: None,
            allow_origins: Vec::new(),
            public_url: None,
            authorization_servers: Vec::new(),
            rate_limit: DEFAULT_RATE_LIMIT,
            introspection_endpoint: None,
            oauth_client_id: None,
            oauth_client_secret_file: None,
            oauth_audience: None,
            oauth_scope: None,
        }
    }
}

/// Parse an `http`/`https` URL option. Errors name the option, never the
/// URL itself, which could carry a password.
pub(crate) fn http_url(raw: &str, what: &str) -> Result<reqwest::Url, StartError> {
    let url =
        reqwest::Url::parse(raw).map_err(|_| StartError::new(format!("{what} is not a URL")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(StartError::new(format!(
            "{what} must be an http or https URL"
        )));
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(StartError::new(format!(
            "{what} must not carry credentials (user name or password), a query or a fragment"
        )));
    }
    Ok(url)
}

/// Canonical form of an allowed origin: `scheme://host[:port]`, lowercase,
/// default port dropped.
pub fn normalize_origin(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
    {
        return None;
    }
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

fn public_url_or_default(raw: Option<&str>, port: u16) -> Result<String, StartError> {
    match raw {
        None => Ok(format!("http://127.0.0.1:{port}")),
        Some(raw) => {
            let url = http_url(raw, "--public-url")?;
            Ok(url.as_str().trim_end_matches('/').to_string())
        }
    }
}

pub(crate) fn is_loopback_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Validate the options, load the API keys and the OAuth client secret.
/// Refuses a non-loopback bind, or a non-loopback `--public-url`, without a
/// validator (`--api-key-file` or OAuth), so a server without authentication
/// is never exposed, and OAuth on a non-loopback bind without
/// `--public-url`. With OAuth, the introspection endpoint must be known
/// here: [`resolve`], which `serve` runs, discovers it first when
/// `--introspection-endpoint` is absent.
pub fn prepare(options: &ServeOptions) -> Result<(SocketAddr, Config), StartError> {
    let addr: SocketAddr = options.bind.parse().map_err(|_| {
        StartError::new(format!(
            "--bind must be an IP address and port such as {DEFAULT_BIND}: {}",
            options.bind
        ))
    })?;
    let oauth = oauth::OAuthOptions::from_serve(options)?;
    let authenticated = options.api_key_file.is_some() || oauth.is_some();
    if !addr.ip().is_loopback() && !authenticated {
        return Err(StartError::new(format!(
            "refusing to bind {addr} without authentication; pass --api-key-file, configure OAuth (--authorization-server with --oauth-audience, --oauth-client-id and --oauth-client-secret-file), or bind a loopback address"
        )));
    }
    if oauth.is_some() && !addr.ip().is_loopback() && options.public_url.is_none() {
        return Err(StartError::new(format!(
            "OAuth on a non-loopback bind ({addr}) needs --public-url: remote connectors find the authorization server through the URL in the 401 challenge and the protected-resource metadata, which would otherwise name a loopback address"
        )));
    }
    if options.rate_limit == 0 {
        return Err(StartError::new("--rate-limit must be at least 1"));
    }
    // Static keys first: constant time, no network.
    let mut validators: Vec<Arc<dyn TokenValidator>> = Vec::new();
    if let Some(path) = &options.api_key_file {
        validators.push(Arc::new(StaticKeys::from_file(path)?));
    }
    if let Some(oauth) = &oauth {
        validators.push(Arc::new(oauth.validator()?));
    }
    let validator: Option<Arc<dyn TokenValidator>> = match validators.len() {
        0 | 1 => validators.pop(),
        _ => Some(Arc::new(ValidatorChain(validators))),
    };
    let scope = oauth
        .as_ref()
        .map_or_else(|| SCOPE.to_string(), |o| o.scope.clone());
    let mut allowed_origins = Vec::new();
    for raw in &options.allow_origins {
        let origin = normalize_origin(raw).ok_or_else(|| {
            StartError::new(format!(
                "--allow-origin must be scheme://host[:port] with no path: {raw}"
            ))
        })?;
        allowed_origins.push(origin);
    }
    let mut authorization_servers = Vec::new();
    for raw in &options.authorization_servers {
        let url = http_url(raw, "--authorization-server")?;
        authorization_servers.push(url.as_str().trim_end_matches('/').to_string());
    }
    let public_url = public_url_or_default(options.public_url.as_deref(), addr.port())?;
    // A loopback bind behind a same-host reverse proxy is still exposed; a
    // public URL that is not loopback says so.
    if validator.is_none() && !is_loopback_url(&public_url) {
        return Err(StartError::new(format!(
            "refusing to serve {public_url} without authentication; pass --api-key-file or configure OAuth"
        )));
    }
    Ok((
        addr,
        Config {
            database: options.database.clone(),
            validator,
            allowed_origins,
            public_url,
            authorization_servers,
            rate_limit_per_minute: options.rate_limit,
            scope,
            oauth_enabled: oauth.is_some(),
        },
    ))
}

/// [`prepare`], after discovering the introspection endpoint from the
/// issuer's `/.well-known/oauth-authorization-server` when OAuth is
/// configured without `--introspection-endpoint`. Inconsistent flags and an
/// unreadable client secret are refused before any network call; metadata
/// without an `introspection_endpoint` refuses to start.
pub async fn resolve(options: &ServeOptions) -> Result<(SocketAddr, Config), StartError> {
    prepare(&with_introspection_endpoint(options).await?)
}

/// `options`, with `introspection_endpoint` discovered when OAuth is
/// configured without one.
async fn with_introspection_endpoint(options: &ServeOptions) -> Result<ServeOptions, StartError> {
    let mut options = options.clone();
    if let Some(oauth) = oauth::OAuthOptions::from_serve(&options)? {
        if oauth.introspection_endpoint.is_none() {
            oauth.client_secret()?;
            let endpoint = oauth::discover_introspection_endpoint(&oauth.issuer).await?;
            options.introspection_endpoint = Some(endpoint.to_string());
        }
    }
    Ok(options)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

struct AppState {
    config: Config,
    mcp: mcp::Server,
    limiter: RateLimiter,
    openapi: Value,
    metadata: Value,
    metadata_mcp: Value,
}

/// The full application: public routes, authenticated routes, the origin
/// guard and the 2 MiB body cap.
pub fn router(config: Config) -> Router {
    let state = Arc::new(AppState {
        mcp: mcp::Server::new(config.database.clone()),
        limiter: RateLimiter::new(config.rate_limit_per_minute),
        openapi: openapi_with_scope(
            &config.public_url,
            config.oauth_enabled.then_some(config.scope.as_str()),
        ),
        metadata: protected_resource_metadata(&config, &config.public_url),
        metadata_mcp: protected_resource_metadata(&config, &format!("{}/mcp", config.public_url)),
        config,
    });
    let protected = Router::new()
        .route(
            "/mcp",
            post(mcp_post).get(mcp_not_allowed).delete(mcp_not_allowed),
        )
        .route("/v1/calc/:function", post(calc))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_document))
        .route(METADATA_PATH, get(resource_metadata))
        .route(METADATA_MCP_PATH, get(resource_metadata_mcp))
        .merge(protected)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), check_origin))
        .with_state(state)
}

fn error_body(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": code, "message": message}})),
    )
        .into_response()
}

/// Block DNS rebinding: a request carrying `Origin` must name an allowed
/// origin. Non-browser clients send none and pass.
async fn check_origin(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .and_then(normalize_origin)
            .is_some_and(|o| state.config.allowed_origins.contains(&o));
        if !allowed {
            return error_body(
                StatusCode::FORBIDDEN,
                "ORIGIN_NOT_ALLOWED",
                "Origin is not on this server's allowlist",
            );
        }
    }
    next.run(request).await
}

/// The metadata document a challenge names: the `/mcp`-suffixed one (whose
/// `resource` is `<public-url>/mcp`) for the MCP endpoint, so a client asks
/// the authorization server for the audience it uses there; the unsuffixed
/// one elsewhere.
fn metadata_url(state: &AppState, path: &str) -> String {
    let metadata = if path == "/mcp" || path.starts_with("/mcp/") {
        METADATA_MCP_PATH
    } else {
        METADATA_PATH
    };
    format!("{}{metadata}", state.config.public_url)
}

fn unauthorized(state: &AppState, path: &str) -> Response {
    let challenge = format!("Bearer resource_metadata=\"{}\"", metadata_url(state, path));
    let mut response = error_body(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "A valid bearer token is required",
    );
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// `403` for a valid token without the required scope (RFC 6750 §3.1).
fn insufficient_scope(state: &AppState, path: &str) -> Response {
    let challenge = format!(
        "Bearer error=\"insufficient_scope\", scope=\"{}\", resource_metadata=\"{}\"",
        state.config.scope,
        metadata_url(state, path)
    );
    let mut response = error_body(
        StatusCode::FORBIDDEN,
        "INSUFFICIENT_SCOPE",
        "The access token does not carry the required scope",
    );
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// `503` when the token could not be checked: not the client's fault, so
/// not a `401` that would send it back through sign-in.
fn auth_unavailable() -> Response {
    let mut response = error_body(
        StatusCode::SERVICE_UNAVAILABLE,
        "AUTH_UNAVAILABLE",
        "The authorization server could not check the token; retry later",
    );
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(AUTH_UNAVAILABLE_RETRY_SECONDS),
    );
    response
}

/// Authenticate, then rate-limit per principal. A missing and a wrong token
/// get the identical 401.
async fn authenticate(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = match &state.config.validator {
        None => Principal {
            subject: "local".into(),
            method: "none",
        },
        Some(validator) => {
            let outcome = match bearer_token(request.headers()) {
                Ok(token) => validator.validate(token).await,
                Err(error) => Err(error),
            };
            match outcome {
                Ok(principal) => principal,
                Err(AuthError::InsufficientScope) => {
                    return insufficient_scope(&state, request.uri().path())
                }
                Err(AuthError::Unavailable) => return auth_unavailable(),
                Err(AuthError::Missing | AuthError::Invalid) => {
                    return unauthorized(&state, request.uri().path())
                }
            }
        }
    };
    if let Err(wait) = state.limiter.check(&principal.caller()) {
        let mut response = error_body(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Too many requests for this key; retry later",
        );
        let seconds = wait.as_secs().saturating_add(1);
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

async fn not_found() -> Response {
    error_body(StatusCode::NOT_FOUND, "NOT_FOUND", "No such route")
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "producer": catalog::producer()}))
}

async fn openapi_document(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.openapi.clone())
}

async fn resource_metadata(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.metadata.clone())
}

async fn resource_metadata_mcp(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.metadata_mcp.clone())
}

/// RFC 9728 metadata naming `resource`. Dynamic client registration is the
/// authorization server's business; nothing here advertises it.
fn protected_resource_metadata(config: &Config, resource: &str) -> Value {
    json!({
        "resource": resource,
        "authorization_servers": config.authorization_servers,
        "bearer_methods_supported": ["header"],
        "scopes_supported": [config.scope],
    })
}

async fn mcp_not_allowed() -> Response {
    let mut response = error_body(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "This server does not offer an SSE stream or sessions; POST one JSON-RPC message to /mcp",
    );
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

fn media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn is_json_content(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| media_type(v) == "application/json")
}

/// No `Accept` header, or one that admits `application/json`.
fn accepts_json(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::ACCEPT).iter().peekable();
    if values.peek().is_none() {
        return true;
    }
    values
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(media_type)
        .any(|m| matches!(m.as_str(), "application/json" | "application/*" | "*/*"))
}

async fn mcp_post(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Some(version) = headers.get("mcp-protocol-version") {
        let known = version
            .to_str()
            .is_ok_and(|v| mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&v.trim()));
        if !known {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                INVALID_REQUEST,
                &format!(
                    "Unsupported MCP-Protocol-Version; supported: {}",
                    mcp::SUPPORTED_PROTOCOL_VERSIONS.join(", ")
                ),
            );
        }
    }
    if !accepts_json(&headers) {
        return rpc_error(
            StatusCode::NOT_ACCEPTABLE,
            INVALID_REQUEST,
            "This server answers application/json only",
        );
    }
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(rejection) => {
            return rpc_error(
                rejection.status(),
                INVALID_REQUEST,
                "Invalid Request: message exceeds 2 MiB or could not be read",
            )
        }
    };
    if !is_json_content(&headers) {
        return rpc_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            INVALID_REQUEST,
            "Content-Type must be application/json",
        );
    }
    let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
        return rpc_error(StatusCode::BAD_REQUEST, PARSE_ERROR, "Parse error");
    };
    // Tool calls are traced with the authenticated principal as the caller.
    let server = state.mcp.with_caller(principal.caller());
    match server.handle(message).await {
        // A notification or a client response: accepted, nothing to say.
        None => StatusCode::ACCEPTED.into_response(),
        Some(response) => {
            // An error with no id answers something that was not a request
            // at all (a batch, a scalar, a null id): HTTP 400.
            let unidentified =
                response.get("id").is_none_or(Value::is_null) && response.get("error").is_some();
            let status = if unidentified {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::OK
            };
            (status, Json(response)).into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CalcQuery {
    version: Option<String>,
}

async fn calc(
    State(state): State<Arc<AppState>>,
    Path(function): Path<String>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<CalcQuery>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if !catalog::CATALOG.iter().any(|c| c.name == function) {
        return error_body(
            StatusCode::NOT_FOUND,
            "UNKNOWN_FUNCTION",
            "No such calculation; GET /openapi.json or POST /v1/calc/describe for the catalog",
        );
    }
    let Ok(Query(query)) = query else {
        return error_body(
            StatusCode::BAD_REQUEST,
            "INVALID_QUERY",
            "The only query parameter is version",
        );
    };
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(rejection) => {
            return error_body(
                rejection.status(),
                "INPUT_LIMIT",
                "Request body exceeds 2 MiB or could not be read",
            )
        }
    };
    let empty = bytes.iter().all(u8::is_ascii_whitespace);
    if !empty && !is_json_content(&headers) {
        return error_body(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Type must be application/json",
        );
    }
    let input = if empty {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(input) => input,
            Err(_) => {
                return error_body(
                    StatusCode::BAD_REQUEST,
                    "INVALID_JSON",
                    "Request body is not JSON",
                )
            }
        }
    };
    let call = catalog::Call {
        function,
        version: query.version,
        input,
        // The server's configured database, never anything from the body.
        database: state.config.database.clone(),
        caller: principal.caller(),
    };
    let envelope = if empty {
        catalog::invoke(&call).await
    } else {
        catalog::invoke_hashed(&call, digest(&bytes)).await
    };
    let status = if envelope.refused() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    (status, Json(envelope.to_value())).into_response()
}

// ---------------------------------------------------------------------------
// OpenAPI 3.1
// ---------------------------------------------------------------------------

fn envelope_schema() -> Value {
    json!({
        "type": "object",
        "required": ["contract_version", "function", "version", "producer", "input_sha256",
                     "computed_at", "trace_id", "trace_store", "citation", "status"],
        "properties": {
            "contract_version": {"const": super::CONTRACT},
            "function": {"type": "string"},
            "version": {"type": ["string", "null"],
                        "description": "Served version; null only when the function is unknown"},
            "producer": {
                "type": "object",
                "required": ["name", "version", "arithmetic"],
                "properties": {
                    "name": {"const": catalog::PRODUCER_NAME},
                    "version": {"type": "string"},
                    "arithmetic": {"const": catalog::ARITHMETIC}
                }
            },
            "input_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$",
                             "description": "SHA-256 of the request body bytes as received (of `{}` for an empty body)"},
            "output_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$",
                              "description": "SHA-256 of the canonical JSON serialization of result"},
            "computed_at": {"type": "string"},
            "trace_id": {"type": "string",
                         "description": "UUID v4 naming this invocation's trace record"},
            "trace_store": {"enum": ["database", "jsonl", "none"],
                            "description": "Where the trace record went"},
            "citation": {"type": "string",
                         "description": "One pasteable line naming function@version, the trace id and the hashes"},
            "trace_warning": {"type": "string",
                              "description": "Why the trace record could not be written; the result is unaffected"},
            "status": {"enum": ["calculated", "review_required", "refused"]},
            "result": {},
            "error": {"$ref": "#/components/schemas/Refusal"}
        }
    })
}

fn success_schema(calc: &catalog::Calculation) -> Value {
    json!({
        "allOf": [
            {"$ref": "#/components/schemas/Envelope"},
            {
                "required": ["output_sha256", "result"],
                "properties": {
                    "status": {"enum": ["calculated", "review_required"]},
                    "result": calc.output_schema()
                }
            }
        ]
    })
}

fn refused_schema() -> Value {
    json!({
        "allOf": [
            {"$ref": "#/components/schemas/Envelope"},
            {"required": ["error"], "properties": {"status": {"const": "refused"}}}
        ]
    })
}

fn json_response(description: &str, schema: Value) -> Value {
    json!({"description": description, "content": {"application/json": {"schema": schema}}})
}

fn error_response(description: &str) -> Value {
    json_response(description, json!({"$ref": "#/components/schemas/Error"}))
}

/// The authentication responses; `metadata_path` is the document the
/// challenges name on this path ([`METADATA_MCP_PATH`] on `/mcp`).
fn auth_responses(metadata_path: &str) -> Value {
    json!({
        "401": {
            "description": "Missing or invalid bearer token",
            "headers": {"WWW-Authenticate": {"schema": {"type": "string"},
                "description": format!("Bearer resource_metadata=\"<public-url>{metadata_path}\"")}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        },
        "403": {
            "description": "Origin header not on the allowlist (ORIGIN_NOT_ALLOWED), or an OAuth access token without the required scope (INSUFFICIENT_SCOPE)",
            "headers": {"WWW-Authenticate": {"schema": {"type": "string"},
                "description": format!("With INSUFFICIENT_SCOPE only: Bearer error=\"insufficient_scope\", scope=\"<scope>\", resource_metadata=\"<public-url>{metadata_path}\"")}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        },
        "413": error_response("Body exceeds 2 MiB"),
        "429": {
            "description": "Rate limit for this key exceeded",
            "headers": {"Retry-After": {"schema": {"type": "integer"}}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        },
        "503": {
            "description": "OAuth only: the authorization server could not check the token (AUTH_UNAVAILABLE)",
            "headers": {"Retry-After": {"schema": {"type": "integer"}}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        }
    })
}

fn metadata_document_schema() -> Value {
    json!({
        "type": "object",
        "required": ["resource", "authorization_servers", "bearer_methods_supported", "scopes_supported"],
        "properties": {
            "resource": {"type": "string"},
            "authorization_servers": {"type": "array", "items": {"type": "string"}},
            "bearer_methods_supported": {"type": "array", "items": {"const": "header"}},
            "scopes_supported": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn calc_path(calc: &catalog::Calculation) -> Value {
    let mut responses = auth_responses(METADATA_PATH);
    responses["200"] = json_response(
        "Calculated (status calculated or review_required)",
        success_schema(calc),
    );
    responses["422"] = json_response(
        "Refused by the engine; the envelope carries a typed error",
        refused_schema(),
    );
    responses["400"] = error_response("Body is not JSON, or an unknown query parameter");
    responses["415"] = error_response("Content-Type is not application/json");
    json!({
        "post": {
            "operationId": calc.name,
            "summary": format!("{}@{}", calc.name, calc.version),
            "description": format!(
                "{}{}\n\n{}",
                calc.summary,
                if calc.stateful {
                    " Needs the database configured on the server (--database); refuses with DATABASE_REQUIRED otherwise."
                } else {
                    ""
                },
                mcp::GROUNDING_RULE
            ),
            "tags": ["calculations"],
            "parameters": [{
                "name": "version", "in": "query", "required": false,
                "description": "Pin the exact function version; a different served version refuses with VERSION_MISMATCH",
                "schema": {"type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+$"}
            }],
            "requestBody": {
                "required": false,
                "description": "The function's input. An empty body means {}.",
                "content": {"application/json": {"schema": calc.input_schema()}}
            },
            "responses": responses
        }
    })
}

/// The OpenAPI 3.1 document for this server, generated from the catalog:
/// the written contract for the HTTP API, for a server with API keys only
/// (no `oauth2` security scheme).
pub fn openapi(public_url: &str) -> Value {
    openapi_with_scope(public_url, None)
}

/// [`openapi`] for a server with OAuth configured and requiring `scope`
/// (`--oauth-scope`): it also advertises the `oauth2` security scheme and
/// the `{"oauth2": [scope]}` requirement. `None` is the API-key-only
/// document. Only `servers` and the OAuth scheme depend on configuration.
pub fn openapi_with_scope(public_url: &str, scope: Option<&str>) -> Value {
    let mut paths = serde_json::Map::new();
    for calc in catalog::CATALOG {
        paths.insert(format!("/v1/calc/{}", calc.name), calc_path(calc));
    }
    let mut mcp_responses = auth_responses(METADATA_MCP_PATH);
    mcp_responses["200"] = json_response(
        "The JSON-RPC response to a request",
        json!({"type": "object", "required": ["jsonrpc", "id"]}),
    );
    mcp_responses["202"] =
        json!({"description": "A notification or client response was accepted; empty body"});
    // Transport refusals on /mcp are JSON-RPC errors with a null id.
    let rpc_failure = |description: &str| {
        json_response(
            description,
            json!({"type": "object", "required": ["jsonrpc", "id", "error"],
                   "properties": {"id": {"type": "null"}}}),
        )
    };
    mcp_responses["400"] = rpc_failure(
        "Not one JSON-RPC message (parse error, batch, null id) or an unsupported MCP-Protocol-Version",
    );
    mcp_responses["406"] = rpc_failure("Accept does not admit application/json");
    mcp_responses["413"] = rpc_failure("Body exceeds 2 MiB");
    mcp_responses["415"] = rpc_failure("Content-Type is not application/json");
    paths.insert(
        "/mcp".into(),
        json!({
            "post": {
                "operationId": "mcp",
                "summary": "MCP Streamable HTTP endpoint (2025-06-18)",
                "description": "One JSON-RPC 2.0 message per request; batches are refused with -32600. Responses are application/json; SSE streaming and sessions are not offered. Tools are the catalog functions and return the same envelopes as /v1/calc.",
                "tags": ["mcp"],
                "parameters": [{
                    "name": "MCP-Protocol-Version", "in": "header", "required": false,
                    "schema": {"enum": mcp::SUPPORTED_PROTOCOL_VERSIONS}
                }],
                "requestBody": {
                    "required": true,
                    "content": {"application/json": {"schema": {
                        "type": "object", "required": ["jsonrpc"],
                        "properties": {"jsonrpc": {"const": "2.0"}, "id": {"type": ["string", "integer"]},
                                       "method": {"type": "string"}, "params": {"type": "object"}}
                    }}}
                },
                "responses": mcp_responses
            },
            "get": {
                "operationId": "mcp_get",
                "summary": "Not offered: no server-initiated SSE stream",
                "tags": ["mcp"],
                "responses": {"405": error_response("Method not allowed")}
            }
        }),
    );
    paths.insert(
        "/health".into(),
        json!({"get": {
            "operationId": "health", "summary": "Liveness and producer identity",
            "tags": ["service"], "security": [],
            "responses": {"200": json_response("Up", json!({
                "type": "object", "required": ["status", "producer"],
                "properties": {"status": {"const": "ok"}, "producer": {"type": "object"}}
            }))}
        }}),
    );
    paths.insert(
        "/openapi.json".into(),
        json!({"get": {
            "operationId": "openapi", "summary": "This document",
            "tags": ["service"], "security": [],
            "responses": {"200": json_response("OpenAPI 3.1 document", json!({"type": "object"}))}
        }}),
    );
    paths.insert(
        METADATA_PATH.into(),
        json!({"get": {
            "operationId": "oauth_protected_resource",
            "summary": "OAuth 2.0 Protected Resource Metadata (RFC 9728)",
            "tags": ["service"], "security": [],
            "responses": {"200": json_response("Metadata; resource is the public URL", metadata_document_schema())}
        }}),
    );
    paths.insert(
        METADATA_MCP_PATH.into(),
        json!({"get": {
            "operationId": "oauth_protected_resource_mcp",
            "summary": "RFC 9728 metadata for the resource <public-url>/mcp (path-suffixed variant)",
            "tags": ["service"], "security": [],
            "responses": {"200": json_response("Metadata; resource is <public-url>/mcp", metadata_document_schema())}
        }}),
    );
    let mut security = vec![json!({"bearer": []})];
    let mut security_schemes = serde_json::Map::new();
    if let Some(scope) = scope {
        security.push(json!({"oauth2": [scope]}));
        security_schemes.insert(
            "bearer".into(),
            json!({
                "type": "http", "scheme": "bearer",
                "description": "An API key from --api-key-file (scripts). An OAuth 2.1 access token is sent the same way; see oauth2."
            }),
        );
        security_schemes.insert(
            "oauth2".into(),
            json!({
                "type": "oauth2",
                "description": format!(
                    "An OAuth 2.1 access token issued by the authorization server listed in {METADATA_PATH} (RFC 9728), carrying scope {scope} and this server's audience, checked by RFC 7662 introspection. The authorization and token endpoints are that server's own (its RFC 8414 metadata), not this server's, so no flow URL is listed here; clients such as claude.ai and ChatGPT discover them from the 401 WWW-Authenticate challenge."
                ),
                "flows": {}
            }),
        );
    } else {
        security_schemes.insert(
            "bearer".into(),
            json!({
                "type": "http", "scheme": "bearer",
                "description": "An API key from --api-key-file (scripts)."
            }),
        );
    }
    json!({
        "openapi": "3.1.0",
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": "boxscore-exact",
            "version": env!("CARGO_PKG_VERSION"),
            "summary": "Named, versioned exact-cent USD operating calculations",
            "description": format!(
                "The written contract for the boxscore-exact HTTP API. Every calculation returns a provenance envelope ({contract}); money is a decimal string with at most two places, never a JSON number. The database is configured on the server and is never a request field.\n\n{rule}",
                contract = super::CONTRACT,
                rule = mcp::GROUNDING_RULE
            ),
            "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"}
        },
        "servers": [{"url": public_url}],
        "security": security,
        "tags": [
            {"name": "calculations", "description": "One operation per catalog function"},
            {"name": "mcp", "description": "Model Context Protocol, Streamable HTTP"},
            {"name": "service", "description": "Unauthenticated service metadata"}
        ],
        "paths": paths,
        "components": {
            "securitySchemes": security_schemes,
            "schemas": {
                "Envelope": envelope_schema(),
                "Refusal": {
                    "type": "object", "required": ["code", "message"],
                    "properties": {"code": {"type": "string"}, "message": {"type": "string"}}
                },
                "Error": {
                    "type": "object", "required": ["error"],
                    "properties": {"error": {"type": "object", "required": ["code", "message"],
                        "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}}
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// Resolves on SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}

/// Bind, announce on stderr, and serve until `shutdown` resolves.
pub async fn serve(
    options: ServeOptions,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), StartError> {
    let options = with_introspection_endpoint(&options).await?;
    let (addr, mut config) = prepare(&options)?;
    let oauth = oauth::OAuthOptions::from_serve(&options)?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| StartError::new(format!("cannot bind {addr}: {e}")))?;
    let local = listener
        .local_addr()
        .map_err(|e| StartError::new(format!("cannot read bound address: {e}")))?;
    if options.public_url.is_none() {
        config.public_url = public_url_or_default(None, local.port())?;
    }
    let auth = match (options.api_key_file.is_some(), oauth.is_some()) {
        (true, true) => "bearer API keys, then OAuth introspection",
        (true, false) => "bearer API keys",
        (false, true) => "OAuth introspection",
        (false, false) => "none (loopback only)",
    };
    eprintln!("boxscore-exact serve: listening on http://{local} (auth: {auth})");
    if let Some(oauth) = &oauth {
        // Identifiers only; the client secret is never printed.
        let endpoint = oauth
            .introspection_endpoint
            .as_ref()
            .map_or("", reqwest::Url::as_str);
        eprintln!(
            "boxscore-exact serve: OAuth issuer {}, introspection {endpoint}, audience {}, scope {}",
            oauth.issuer, oauth.audience, oauth.scope
        );
    }
    let app = router(config);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| StartError::new(format!("server error: {e}")))?;
    eprintln!("boxscore-exact serve: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_compares_whole_inputs() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn key_file_parsing_trims_and_refuses_weak_or_duplicate_keys() {
        let secret = "s".repeat(MIN_SECRET_LEN);
        let keys =
            StaticKeys::parse(&format!("# comment\n\nci:{secret}\r\nops : {secret}x \n")).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.check(&secret).unwrap().subject, "ci");
        assert_eq!(keys.check(&format!("{secret}x")).unwrap().subject, "ops");
        assert_eq!(keys.check("nope"), Err(AuthError::Invalid));
        assert!(StaticKeys::parse("").is_err());
        assert!(StaticKeys::parse("ci:short").is_err());
        assert!(StaticKeys::parse(&format!("no-colon{secret}")).is_err());
        assert!(StaticKeys::parse(&format!("bad id:{secret}")).is_err());
        assert!(StaticKeys::parse(&format!("a:{secret}\na:{secret}y")).is_err());
        assert!(StaticKeys::parse(&format!("a:{secret}\nb:{secret}")).is_err());
        let error = StaticKeys::parse(&format!("a:{}", "z".repeat(10)))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("zzzz"));
        assert!(!format!("{keys:?}").contains(&secret));
    }

    #[test]
    fn bucket_refills_over_time() {
        let limiter = RateLimiter::new(60);
        let start = Instant::now();
        for _ in 0..60 {
            assert!(limiter.check_at("k", start).is_ok());
        }
        let wait = limiter.check_at("k", start).unwrap_err();
        assert!(wait <= Duration::from_secs(1));
        assert!(limiter.check_at("other", start).is_ok());
        assert!(limiter
            .check_at("k", start + Duration::from_millis(1100))
            .is_ok());
        assert!(limiter
            .check_at("k", start + Duration::from_millis(1100))
            .is_err());
    }

    #[test]
    fn bearer_parsing() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), Err(AuthError::Missing));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer  tok "),
        );
        assert_eq!(bearer_token(&headers), Ok("tok"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic tok"));
        assert_eq!(bearer_token(&headers), Err(AuthError::Invalid));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert_eq!(bearer_token(&headers), Err(AuthError::Invalid));
    }

    #[test]
    fn origins_normalize() {
        assert_eq!(
            normalize_origin("HTTPS://Claude.AI:443").as_deref(),
            Some("https://claude.ai")
        );
        assert_eq!(
            normalize_origin("http://localhost:6274").as_deref(),
            Some("http://localhost:6274")
        );
        assert!(normalize_origin("https://claude.ai/path").is_none());
        assert!(normalize_origin("null").is_none());
    }
}
