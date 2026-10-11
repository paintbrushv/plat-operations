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
//! [`TokenValidator`]. Stage 2a ships [`StaticKeys`] (API keys for scripts);
//! OAuth 2.1 access tokens from an external authorization server (stage 2b)
//! slot in behind the same trait. A server without a validator answers
//! loopback only: [`prepare`] refuses to bind anything else.
//!
//! The database path comes from the command line, never from a request. The
//! server writes no artifacts and logs no request content, token or principal.
use super::{catalog, digest, mcp, protocol::MAX_REQUEST_BYTES};
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
/// Shortest accepted API-key secret, in characters.
pub const MIN_SECRET_LEN: usize = 32;
const MAX_KEY_FILE_BYTES: u64 = 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Who made a request. Never logged; carried to the handler only so the
/// call record can name the caller once the catalog has a `caller` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub subject: String,
    /// `"api_key"` for [`StaticKeys`]; `"none"` when the server runs without
    /// a validator on loopback.
    pub method: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// No `Authorization: Bearer` header.
    Missing,
    /// A token was presented and not accepted.
    Invalid,
}

/// Turns a bearer token into a [`Principal`]. Stage 2b adds an OAuth 2.1
/// access-token validator behind this same seam.
pub trait TokenValidator: Send + Sync {
    fn validate(&self, token: &str) -> Result<Principal, AuthError>;
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

fn sha256(bytes: &[u8]) -> [u8; 32] {
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
        use std::io::Read;
        let metadata =
            std::fs::metadata(path).map_err(|_| StartError::new("API key file cannot be read"))?;
        if !metadata.is_file() {
            return Err(StartError::new("API key file is not a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            if mode & 0o007 != 0 {
                return Err(StartError::new(format!(
                    "API key file is accessible to other users (mode {:o}); run chmod 600 on it",
                    mode & 0o777
                )));
            }
            if mode & 0o070 != 0 {
                eprintln!(
                    "boxscore-exact serve: warning: API key file is group-accessible (mode {:o}); 0600 is expected",
                    mode & 0o777
                );
            }
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(MAX_KEY_FILE_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|_| StartError::new("API key file cannot be read"))?;
        if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
            return Err(StartError::new("API key file exceeds 1 MiB"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| StartError::new("API key file is not UTF-8 text"))?;
        Self::parse(&text)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl TokenValidator for StaticKeys {
    fn validate(&self, token: &str) -> Result<Principal, AuthError> {
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
    /// `None` only for a loopback server started without `--api-key-file`.
    pub validator: Option<Arc<dyn TokenValidator>>,
    /// Serialized origins (`scheme://host[:port]`) allowed to send `Origin`.
    pub allowed_origins: Vec<String>,
    /// The resource identifier, without a trailing slash.
    pub public_url: String,
    pub authorization_servers: Vec<String>,
    pub rate_limit_per_minute: u32,
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
            .finish()
    }
}

/// The command-line options of `boxscore-exact serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: String,
    pub database: Option<PathBuf>,
    pub api_key_file: Option<PathBuf>,
    pub allow_origins: Vec<String>,
    pub public_url: Option<String>,
    pub authorization_servers: Vec<String>,
    pub rate_limit: u32,
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
        }
    }
}

fn http_url(raw: &str, what: &str) -> Result<reqwest::Url, StartError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| StartError::new(format!("{what} is not a URL: {raw}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(StartError::new(format!(
            "{what} must be an http or https URL: {raw}"
        )));
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(StartError::new(format!(
            "{what} must not carry credentials, a query or a fragment: {raw}"
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

fn is_loopback_url(raw: &str) -> bool {
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

/// Validate the options and load keys. Refuses a non-loopback bind, or a
/// non-loopback `--public-url`, without `--api-key-file`, so a server
/// without authentication is never exposed.
pub fn prepare(options: &ServeOptions) -> Result<(SocketAddr, Config), StartError> {
    let addr: SocketAddr = options.bind.parse().map_err(|_| {
        StartError::new(format!(
            "--bind must be an IP address and port such as {DEFAULT_BIND}: {}",
            options.bind
        ))
    })?;
    if !addr.ip().is_loopback() && options.api_key_file.is_none() {
        return Err(StartError::new(format!(
            "refusing to bind {addr} without authentication; pass --api-key-file or bind a loopback address"
        )));
    }
    if options.rate_limit == 0 {
        return Err(StartError::new("--rate-limit must be at least 1"));
    }
    let validator: Option<Arc<dyn TokenValidator>> = match &options.api_key_file {
        Some(path) => Some(Arc::new(StaticKeys::from_file(path)?)),
        None => None,
    };
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
            "refusing to serve {public_url} without authentication; pass --api-key-file"
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
        },
    ))
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
}

/// The full application: public routes, authenticated routes, the origin
/// guard and the 2 MiB body cap.
pub fn router(config: Config) -> Router {
    let state = Arc::new(AppState {
        mcp: mcp::Server::new(config.database.clone()),
        limiter: RateLimiter::new(config.rate_limit_per_minute),
        openapi: openapi(&config.public_url),
        metadata: protected_resource_metadata(&config),
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

fn unauthorized(state: &AppState) -> Response {
    let challenge = format!(
        "Bearer resource_metadata=\"{}{METADATA_PATH}\"",
        state.config.public_url
    );
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
            match bearer_token(request.headers()).and_then(|t| validator.validate(t)) {
                Ok(principal) => principal,
                Err(_) => return unauthorized(&state),
            }
        }
    };
    if let Err(wait) = state.limiter.check(&principal.subject) {
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

fn protected_resource_metadata(config: &Config) -> Value {
    json!({
        "resource": config.public_url,
        "authorization_servers": config.authorization_servers,
        "bearer_methods_supported": ["header"],
        "scopes_supported": [SCOPE],
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
    let server = state.mcp.with_caller(format!("http:{}", principal.subject));
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
        caller: format!("http:{}", principal.subject),
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

fn auth_responses() -> Value {
    json!({
        "401": {
            "description": "Missing or invalid bearer token",
            "headers": {"WWW-Authenticate": {"schema": {"type": "string"},
                "description": "Bearer resource_metadata=\"<public-url>/.well-known/oauth-protected-resource\""}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        },
        "403": error_response("Origin header not on the allowlist"),
        "413": error_response("Body exceeds 2 MiB"),
        "429": {
            "description": "Rate limit for this key exceeded",
            "headers": {"Retry-After": {"schema": {"type": "integer"}}},
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}
        }
    })
}

fn calc_path(calc: &catalog::Calculation) -> Value {
    let mut responses = auth_responses();
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
/// the written contract for the HTTP API.
pub fn openapi(public_url: &str) -> Value {
    let mut paths = serde_json::Map::new();
    for calc in catalog::CATALOG {
        paths.insert(format!("/v1/calc/{}", calc.name), calc_path(calc));
    }
    let mut mcp_responses = auth_responses();
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
            "responses": {"200": json_response("Metadata", json!({
                "type": "object",
                "required": ["resource", "authorization_servers", "bearer_methods_supported", "scopes_supported"],
                "properties": {
                    "resource": {"type": "string"},
                    "authorization_servers": {"type": "array", "items": {"type": "string"}},
                    "bearer_methods_supported": {"type": "array", "items": {"const": "header"}},
                    "scopes_supported": {"type": "array", "items": {"type": "string"}}
                }
            }))}
        }}),
    );
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
        "security": [{"bearer": []}],
        "tags": [
            {"name": "calculations", "description": "One operation per catalog function"},
            {"name": "mcp", "description": "Model Context Protocol, Streamable HTTP"},
            {"name": "service", "description": "Unauthenticated service metadata"}
        ],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearer": {
                    "type": "http", "scheme": "bearer",
                    "description": "An API key from --api-key-file (scripts), or an OAuth 2.1 access token from the advertised authorization server once configured"
                }
            },
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
    let (addr, mut config) = prepare(&options)?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| StartError::new(format!("cannot bind {addr}: {e}")))?;
    let local = listener
        .local_addr()
        .map_err(|e| StartError::new(format!("cannot read bound address: {e}")))?;
    if options.public_url.is_none() {
        config.public_url = public_url_or_default(None, local.port())?;
    }
    let auth = match &options.api_key_file {
        Some(_) => "bearer API keys",
        None => "none (loopback only)",
    };
    eprintln!("boxscore-exact serve: listening on http://{local} (auth: {auth})");
    if !config.authorization_servers.is_empty() && options.api_key_file.is_none() {
        eprintln!(
            "boxscore-exact serve: note: --authorization-server is advertised in metadata only; OAuth token validation is not yet implemented"
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
        assert_eq!(keys.validate(&secret).unwrap().subject, "ci");
        assert_eq!(keys.validate(&format!("{secret}x")).unwrap().subject, "ops");
        assert_eq!(keys.validate("nope"), Err(AuthError::Invalid));
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
