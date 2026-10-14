//! OAuth 2.1 access-token validation on `boxscore-exact serve` (RFC 7662
//! introspection). A mock authorization server runs as an axum router on an
//! ephemeral loopback port: it counts introspection calls, checks the HTTP
//! Basic client credentials and answers per token. For the browser
//! workbench's sign-in it also has `authorize` and `token` endpoints: a
//! public client's authorization code flow with PKCE (S256), as strict as a
//! real IdP where it matters (a state is required, the redirect URI must be
//! registered, a code is bound to its client and expires, and the token
//! endpoint answers only a registered page origin, with CORS), whose access
//! tokens its introspection then reports active. The real `router` is
//! driven with `oneshot` or on a socket; three tests start the real binary.
//! Synthetic data only.

use axum::{
    body::{to_bytes, Body},
    extract::Query,
    extract::State,
    http::{header, HeaderMap, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{self, get},
    Form, Json, Router,
};
use boxscore::exact::{
    mcp_http::{self, Config, ServeOptions},
    oauth::{self, IntrospectionSettings, IntrospectionValidator},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

const PUBLIC_URL: &str = "https://boxscore.example.test";
const AUDIENCE: &str = "https://boxscore.example.test";
const CLIENT_ID: &str = "boxscore-resource-server";
const CLIENT_SECRET: &str = "synthetic-introspection-secret-0123456789";
const API_KEY: &str = "synthetic-test-secret-0123456789abcdef";
const ACCEPT: &str = "application/json, text/event-stream";

const ACTIVE: &str = "synthetic-oauth-token-active-user";
const INACTIVE: &str = "synthetic-oauth-token-inactive";
const EXPIRED: &str = "synthetic-oauth-token-expired";
const WRONG_AUD: &str = "synthetic-oauth-token-wrong-audience";
const NO_SCOPE: &str = "synthetic-oauth-token-no-scope";
/// The public client the workbench page signs in as.
const BROWSER_CLIENT: &str = "boxscore-workbench";
/// Another public client of the mock IdP: its codes are not the page's.
const OTHER_CLIENT: &str = "synthetic-other-public-client";
/// How long a code can wait before it is redeemed, unless a test changes it.
const CODE_TTL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Mock authorization server
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Reply {
    Json(Value),
    Raw(u16, &'static str),
    Delayed(Duration, Value),
}

struct MockState {
    calls: AtomicUsize,
    rejected_clients: AtomicUsize,
    replies: Mutex<HashMap<String, Reply>>,
    metadata: Mutex<Value>,
    /// Further GET documents by path (status, body); any other path is 404.
    documents: Mutex<HashMap<String, (u16, Value)>>,
    expected_authorization: String,
    /// Authorization codes not yet redeemed.
    codes: Mutex<HashMap<String, Grant>>,
    issued: AtomicUsize,
    /// Registered redirect URIs: `authorize` refuses any other.
    redirect_uris: Mutex<Vec<String>>,
    /// Page origins the token endpoint answers (CORS): any other is refused.
    page_origins: Mutex<Vec<String>>,
    code_ttl: Mutex<Duration>,
}

/// What `authorize` bound a code to.
struct Grant {
    client_id: String,
    challenge: String,
    redirect_uri: String,
    issued_at: std::time::Instant,
}

struct Idp {
    base: String,
    state: Arc<MockState>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What reqwest sends for HTTP Basic with these credentials.
fn basic(user: &str, password: &str) -> String {
    reqwest::Client::new()
        .get("http://127.0.0.1/")
        .basic_auth(user, Some(password))
        .build()
        .unwrap()
        .headers()["authorization"]
        .to_str()
        .unwrap()
        .to_string()
}

async fn introspect(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let authorized = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == state.expected_authorization);
    if !authorized {
        state.rejected_clients.fetch_add(1, Ordering::SeqCst);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    assert_eq!(
        form.get("token_type_hint").map(String::as_str),
        Some("access_token")
    );
    let token = form.get("token").cloned().unwrap_or_default();
    let reply = state.replies.lock().unwrap().get(&token).cloned();
    match reply.unwrap_or(Reply::Json(json!({"active": false}))) {
        Reply::Json(body) => Json(body).into_response(),
        Reply::Raw(status, body) => (StatusCode::from_u16(status).unwrap(), body).into_response(),
        Reply::Delayed(delay, body) => {
            tokio::time::sleep(delay).await;
            Json(body).into_response()
        }
    }
}

fn oauth_error(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

/// RFC 4648 section 5 base64url without padding, as RFC 7636 uses it.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

fn s256(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// `GET /authorize`: no login screen; a request from a known public client
/// with a registered redirect URI, a state and an S256 challenge is
/// answered with a redirect to that URI carrying a fresh code and the
/// state. An unregistered redirect URI is answered here, never followed.
async fn authorize(
    State(state): State<Arc<MockState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let param = |name: &str| query.get(name).cloned().unwrap_or_default();
    if param("response_type") != "code"
        || ![BROWSER_CLIENT, OTHER_CLIENT].contains(&param("client_id").as_str())
    {
        return oauth_error("unauthorized_client", "unknown client or response type");
    }
    if !state
        .redirect_uris
        .lock()
        .unwrap()
        .contains(&param("redirect_uri"))
    {
        return oauth_error("invalid_request", "redirect_uri is not registered");
    }
    if param("state").is_empty() {
        return oauth_error("invalid_request", "state is required");
    }
    if param("code_challenge_method") != "S256" || param("code_challenge").len() != 43 {
        return oauth_error("invalid_request", "PKCE with S256 is required");
    }
    if param("scope").split(' ').all(|s| s != "boxscore:calc") {
        return oauth_error("invalid_scope", "boxscore:calc was not requested");
    }
    let n = state.issued.fetch_add(1, Ordering::SeqCst);
    let code = format!("synthetic-code-{n}");
    state.codes.lock().unwrap().insert(
        code.clone(),
        Grant {
            client_id: param("client_id"),
            challenge: param("code_challenge"),
            redirect_uri: param("redirect_uri"),
            issued_at: std::time::Instant::now(),
        },
    );
    let mut location = reqwest::Url::parse(&param("redirect_uri")).unwrap();
    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &param("state"));
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

/// `POST /token`: from a registered page origin only (answered with
/// `Access-Control-Allow-Origin` for it), a public client (no client
/// authentication) redeems a code once, within the code TTL, as the client
/// it was issued to, with the verifier whose S256 is the challenge
/// `authorize` saw and the same redirect URI. The access token is then
/// active at introspection with this server's audience and scope.
async fn token(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    form: Form<HashMap<String, String>>,
) -> Response {
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !state.page_origins.lock().unwrap().contains(&origin) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "invalid_request", "error_description": "origin not allowed"})),
        )
            .into_response();
    }
    let mut response = redeem(&state, &headers, &form);
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.parse().unwrap());
    headers.insert(header::VARY, "Origin".parse().unwrap());
    response
}

fn redeem(
    state: &MockState,
    headers: &HeaderMap,
    Form(form): &Form<HashMap<String, String>>,
) -> Response {
    let param = |name: &str| form.get(name).cloned().unwrap_or_default();
    if headers.contains_key("authorization") || form.contains_key("client_secret") {
        return oauth_error("invalid_client", "the browser client is public");
    }
    if param("grant_type") != "authorization_code"
        || ![BROWSER_CLIENT, OTHER_CLIENT].contains(&param("client_id").as_str())
    {
        return oauth_error("unauthorized_client", "unknown client or grant type");
    }
    let Some(grant) = state.codes.lock().unwrap().remove(&param("code")) else {
        return oauth_error("invalid_grant", "unknown or used code");
    };
    if grant.client_id != param("client_id") {
        return oauth_error("invalid_grant", "the code was issued to another client");
    }
    if grant.issued_at.elapsed() >= *state.code_ttl.lock().unwrap() {
        return oauth_error("invalid_grant", "the code has expired");
    }
    if grant.redirect_uri != param("redirect_uri") {
        return oauth_error("invalid_grant", "redirect_uri does not match");
    }
    if s256(&param("code_verifier")) != grant.challenge {
        return oauth_error("invalid_grant", "PKCE verification failed");
    }
    let access_token = format!("synthetic-browser-token-{}", param("code"));
    let mut granted = active("alice", unix_now() + 300);
    granted["client_id"] = json!(BROWSER_CLIENT);
    state
        .replies
        .lock()
        .unwrap()
        .insert(access_token.clone(), Reply::Json(granted));
    Json(json!({
        "access_token": access_token, "token_type": "Bearer", "expires_in": 300,
        "scope": "openid boxscore:calc",
    }))
    .into_response()
}

async fn metadata(State(state): State<Arc<MockState>>) -> Json<Value> {
    Json(state.metadata.lock().unwrap().clone())
}

async fn document(State(state): State<Arc<MockState>>, uri: Uri) -> Response {
    match state.documents.lock().unwrap().get(uri.path()).cloned() {
        Some((status, body)) => (StatusCode::from_u16(status).unwrap(), Json(body)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

impl Idp {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(MockState {
            calls: AtomicUsize::new(0),
            rejected_clients: AtomicUsize::new(0),
            replies: Mutex::new(HashMap::new()),
            metadata: Mutex::new(json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "introspection_endpoint": format!("{base}/introspect"),
            })),
            documents: Mutex::new(HashMap::new()),
            expected_authorization: basic(CLIENT_ID, CLIENT_SECRET),
            codes: Mutex::new(HashMap::new()),
            issued: AtomicUsize::new(0),
            redirect_uris: Mutex::new(vec![format!("{PUBLIC_URL}/")]),
            page_origins: Mutex::new(vec![PUBLIC_URL.to_string()]),
            code_ttl: Mutex::new(CODE_TTL),
        });
        let app = Router::new()
            .route("/introspect", routing::post(introspect))
            .route("/authorize", get(authorize))
            .route("/token", routing::post(token))
            .route("/.well-known/oauth-authorization-server", get(metadata))
            .fallback(document)
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let idp = Self { base, state };
        let exp = unix_now() + 3600;
        idp.reply(ACTIVE, Reply::Json(active("user-1", exp)));
        idp.reply(INACTIVE, Reply::Json(json!({"active": false})));
        idp.reply(
            EXPIRED,
            Reply::Json(active("user-1", unix_now().saturating_sub(10))),
        );
        let mut wrong = active("user-1", exp);
        wrong["aud"] = json!("https://other.example.test");
        idp.reply(WRONG_AUD, Reply::Json(wrong));
        let mut unscoped = active("user-1", exp);
        unscoped["scope"] = json!("openid profile");
        idp.reply(NO_SCOPE, Reply::Json(unscoped));
        idp
    }

    fn endpoint(&self) -> String {
        format!("{}/introspect", self.base)
    }

    fn reply(&self, token: &str, reply: Reply) {
        self.state
            .replies
            .lock()
            .unwrap()
            .insert(token.to_string(), reply);
    }

    fn calls(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }
}

fn active(sub: &str, exp: u64) -> Value {
    json!({
        "active": true, "sub": sub, "client_id": "claude-connector",
        "aud": [AUDIENCE], "scope": "openid boxscore:calc", "exp": exp,
        "iat": unix_now(), "nbf": unix_now().saturating_sub(5),
        "iss": "https://idp.example.test", "token_type": "Bearer",
    })
}

// ---------------------------------------------------------------------------
// Server fixtures
// ---------------------------------------------------------------------------

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

fn private_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    set_mode(&path, 0o600);
    path
}

fn oauth_options(idp: &Idp, dir: &Path) -> ServeOptions {
    ServeOptions {
        public_url: Some(PUBLIC_URL.into()),
        authorization_servers: vec![idp.base.clone()],
        introspection_endpoint: Some(idp.endpoint()),
        oauth_client_id: Some(CLIENT_ID.into()),
        oauth_client_secret_file: Some(private_file(
            dir,
            "client-secret",
            &format!("{CLIENT_SECRET}\n"),
        )),
        oauth_audience: Some(AUDIENCE.into()),
        ..ServeOptions::default()
    }
}

fn app_from(options: &ServeOptions) -> Router {
    let (_, config) = mcp_http::prepare(options).unwrap();
    mcp_http::router(config)
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }

    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("no {name} header"))
            .to_str()
            .unwrap()
    }
}

async fn send(app: &Router, request: Request<Body>) -> Answer {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Answer {
        status,
        headers,
        body,
    }
}

fn post(path: &str, token: Option<&str>, body: impl Into<Body>) -> Request<Body> {
    let mut builder = Request::post(path)
        .header("content-type", "application/json")
        .header("accept", ACCEPT);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(body.into()).unwrap()
}

fn ping() -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).unwrap()
}

async fn ping_with(app: &Router, token: &str) -> Answer {
    send(app, post("/mcp", Some(token), ping())).await
}

/// The 401 challenge on `/mcp`: it names the `/mcp`-suffixed metadata.
fn challenge() -> String {
    format!("Bearer resource_metadata=\"{PUBLIC_URL}/.well-known/oauth-protected-resource/mcp\"")
}

fn assert_challenge(answer: &Answer) {
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(answer.header("www-authenticate"), challenge());
}

// ---------------------------------------------------------------------------
// Red team
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejected_tokens_get_the_401_challenge() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let missing = send(&app, post("/mcp", None, ping())).await;
    assert_challenge(&missing);
    assert_eq!(idp.calls(), 0, "no token, no call");
    for token in [INACTIVE, EXPIRED, WRONG_AUD, "synthetic-unknown-token"] {
        let answer = ping_with(&app, token).await;
        assert_challenge(&answer);
        // Identical to a missing token: nothing says why.
        assert_eq!(answer.body, missing.body, "{token}");
    }
    assert_eq!(idp.calls(), 4);
    assert_eq!(idp.state.rejected_clients.load(Ordering::SeqCst), 0);

    // An active token whose nbf is still in the future is refused.
    let not_yet = "synthetic-oauth-token-not-yet-valid";
    let mut future = active("user-1", unix_now() + 3600);
    future["nbf"] = json!(unix_now() + 600);
    idp.reply(not_yet, Reply::Json(future));
    assert_challenge(&ping_with(&app, not_yet).await);

    // A token outside the RFC 6750 b64token alphabet is refused unasked.
    let mut odd = post("/mcp", None, ping());
    odd.headers_mut()
        .insert("authorization", "Bearer a\"b".parse().unwrap());
    let before = idp.calls();
    assert_challenge(&send(&app, odd).await);
    assert_eq!(idp.calls(), before, "not a b64token: no call");
}

#[tokio::test]
async fn missing_scope_is_403_insufficient_scope() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let answer = ping_with(&app, NO_SCOPE).await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert_eq!(
        answer.header("www-authenticate"),
        format!(
            "Bearer error=\"insufficient_scope\", scope=\"boxscore:calc\", resource_metadata=\"{PUBLIC_URL}/.well-known/oauth-protected-resource/mcp\""
        )
    );
    assert_eq!(answer.json()["error"]["code"], "INSUFFICIENT_SCOPE");
    // Cached like any negative verdict.
    assert_eq!(
        ping_with(&app, NO_SCOPE).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(idp.calls(), 1);

    // A configured scope is the one required and the one named.
    let mut custom = oauth_options(&idp, dir.path());
    custom.oauth_scope = Some("boxscore:custom".into());
    let custom = app_from(&custom);
    let refused = ping_with(&custom, ACTIVE).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused
        .header("www-authenticate")
        .contains("scope=\"boxscore:custom\""));
    let mut both = active("user-2", unix_now() + 3600);
    both["scope"] = json!("boxscore:custom");
    idp.reply("synthetic-custom-scope", Reply::Json(both));
    assert_eq!(
        ping_with(&custom, "synthetic-custom-scope").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn active_token_is_accepted_and_traced_as_http_oauth_subject() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let mut options = oauth_options(&idp, dir.path());
    options.database = Some(db);
    let app = app_from(&options);

    let ok = ping_with(&app, ACTIVE).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json(), json!({"jsonrpc": "2.0", "id": 1, "result": {}}));

    // /v1/calc records the caller ...
    let described = send(&app, post("/v1/calc/describe", Some(ACTIVE), "")).await;
    assert_eq!(described.status, StatusCode::OK);
    let trace_id = described.json()["trace_id"].as_str().unwrap().to_string();
    // ... and so does an MCP tool call.
    let call = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "describe", "arguments": {}}}))
    .unwrap();
    let mcp = send(&app, post("/mcp", Some(ACTIVE), call)).await;
    assert_eq!(mcp.status, StatusCode::OK);
    let mcp_trace = mcp.json()["result"]["structuredContent"]["trace_id"]
        .as_str()
        .unwrap()
        .to_string();
    for id in [trace_id, mcp_trace] {
        let provenance = send(
            &app,
            post(
                "/v1/calc/get_provenance",
                Some(ACTIVE),
                serde_json::to_vec(&json!({"trace_id": id})).unwrap(),
            ),
        )
        .await;
        let envelope = provenance.json();
        assert_eq!(provenance.status, StatusCode::OK, "{envelope}");
        assert_eq!(
            envelope["result"]["trace"]["caller"], "http:oauth:user-1",
            "{envelope}"
        );
    }
    // Four requests, one introspection: the positive verdict is cached.
    assert_eq!(idp.calls(), 1);

    // No sub: the client id names the caller.
    let mut service = active("ignored", unix_now() + 3600);
    service.as_object_mut().unwrap().remove("sub");
    idp.reply("synthetic-client-credentials", Reply::Json(service));
    assert_eq!(
        ping_with(&app, "synthetic-client-credentials").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn introspection_failures_are_503_with_retry_after() {
    // Nothing listens on this port.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut down = oauth_options(&idp, dir.path());
    down.introspection_endpoint = Some(format!("http://127.0.0.1:{closed}/introspect"));
    let app = app_from(&down);
    for _ in 0..2 {
        let answer = ping_with(&app, ACTIVE).await;
        assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(answer.header("retry-after"), "5");
        assert_eq!(answer.json()["error"]["code"], "AUTH_UNAVAILABLE");
        assert!(answer.headers.get("www-authenticate").is_none());
    }

    // Unusable answers from a live endpoint are 503 too, and never cached.
    let app = app_from(&oauth_options(&idp, dir.path()));
    let cases = [
        (
            "synthetic-reply-not-json",
            Reply::Raw(200, "<html>oops</html>"),
        ),
        (
            "synthetic-reply-no-active",
            Reply::Json(json!({"sub": "x"})),
        ),
        (
            "synthetic-reply-string-active",
            Reply::Json(json!({"active": "true"})),
        ),
        ("synthetic-reply-array", Reply::Json(json!([true]))),
        ("synthetic-reply-500", Reply::Raw(500, "{}")),
        ("synthetic-reply-302", Reply::Raw(302, "")),
    ];
    for (token, reply) in &cases {
        idp.reply(token, reply.clone());
    }
    for (token, _) in &cases {
        let before = idp.calls();
        for _ in 0..2 {
            let answer = ping_with(&app, token).await;
            assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE, "{token}");
            assert_eq!(answer.header("retry-after"), "5");
        }
        assert_eq!(idp.calls(), before + 2, "{token}: failures are not cached");
    }

    // Wrong client credentials at the IdP are a server misconfiguration: 503.
    let mut wrong_secret = oauth_options(&idp, dir.path());
    wrong_secret.oauth_client_secret_file = Some(private_file(
        dir.path(),
        "wrong-secret",
        "synthetic-wrong-introspection-secret",
    ));
    let answer = ping_with(&app_from(&wrong_secret), ACTIVE).await;
    assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(idp.state.rejected_clients.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn introspection_has_a_timeout() {
    let idp = Idp::start().await;
    idp.reply(
        "synthetic-slow-token",
        Reply::Delayed(Duration::from_secs(3), active("user-1", unix_now() + 3600)),
    );
    let mut settings = IntrospectionSettings::new(
        reqwest::Url::parse(&idp.endpoint()).unwrap(),
        CLIENT_ID.into(),
        CLIENT_SECRET.into(),
        AUDIENCE.into(),
        mcp_http::SCOPE.into(),
    );
    assert_eq!(settings.timeout, Duration::from_secs(5));
    settings.timeout = Duration::from_millis(300);
    let app = mcp_http::router(Config {
        database: None,
        validator: Some(Arc::new(IntrospectionValidator::new(settings).unwrap())),
        allowed_origins: Vec::new(),
        public_url: PUBLIC_URL.into(),
        authorization_servers: vec![idp.base.clone()],
        rate_limit_per_minute: 60,
        scope: mcp_http::SCOPE.into(),
        oauth_enabled: true,
        loopback_port: None,
        api_keys: false,
        browser_oauth: None,
    });
    let started = std::time::Instant::now();
    let answer = ping_with(&app, "synthetic-slow-token").await;
    assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn negative_verdicts_are_cached_without_recalling() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    for token in [INACTIVE, EXPIRED, WRONG_AUD] {
        let before = idp.calls();
        for _ in 0..3 {
            assert_challenge(&ping_with(&app, token).await);
        }
        assert_eq!(idp.calls(), before + 1, "{token}");
    }
    // Within 30 seconds, even a token the IdP now calls active stays refused.
    idp.reply(INACTIVE, Reply::Json(active("user-1", unix_now() + 3600)));
    assert_challenge(&ping_with(&app, INACTIVE).await);
    assert_eq!(idp.calls(), 3);
}

#[tokio::test]
async fn cached_positive_verdict_ends_at_exp() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let token = "synthetic-oauth-token-short-lived";
    idp.reply(token, Reply::Json(active("user-1", unix_now() + 2)));
    assert_eq!(ping_with(&app, token).await.status, StatusCode::OK);
    assert_eq!(ping_with(&app, token).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 1, "second request served from the cache");
    tokio::time::sleep(Duration::from_millis(3100)).await;
    // The cached verdict is gone; the endpoint is asked again and the exp
    // it reports is past.
    assert_challenge(&ping_with(&app, token).await);
    assert_eq!(idp.calls(), 2);
}

#[tokio::test]
async fn oversized_tokens_are_refused_without_a_call() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let long = "a".repeat(4097);
    assert_challenge(&ping_with(&app, &long).await);
    assert_eq!(idp.calls(), 0);
    let limit = "b".repeat(4096);
    assert_challenge(&ping_with(&app, &limit).await);
    assert_eq!(idp.calls(), 1, "4 KiB exactly is still introspected");
}

#[tokio::test]
async fn static_keys_and_oauth_together() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = oauth_options(&idp, dir.path());
    options.bind = "0.0.0.0:3819".into();
    options.api_key_file = Some(private_file(dir.path(), "keys", &format!("ci:{API_KEY}\n")));
    let app = app_from(&options);
    // The API key is matched first, without calling the IdP.
    assert_eq!(ping_with(&app, API_KEY).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 0);
    assert_eq!(ping_with(&app, ACTIVE).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 1);
    // A token that is neither gets the introspection verdict.
    assert_challenge(&ping_with(&app, INACTIVE).await);
    assert_eq!(
        ping_with(&app, NO_SCOPE).await.status,
        StatusCode::FORBIDDEN
    );
    // The two kinds of principal are rate-limited separately.
    let (_, config) = mcp_http::prepare(&options).unwrap();
    let validator = config.validator.unwrap();
    let key = validator.validate(API_KEY).await.unwrap();
    let user = validator.validate(ACTIVE).await.unwrap();
    assert_eq!((key.method, key.caller()), ("api_key", "http:ci".into()));
    assert_eq!(
        (user.method, user.caller()),
        ("oauth", "http:oauth:user-1".into())
    );
}

/// Settings for the mock IdP, for tests that change the limits.
fn settings(idp: &Idp) -> IntrospectionSettings {
    IntrospectionSettings::new(
        reqwest::Url::parse(&idp.endpoint()).unwrap(),
        CLIENT_ID.into(),
        CLIENT_SECRET.into(),
        AUDIENCE.into(),
        mcp_http::SCOPE.into(),
    )
}

fn app_with(idp: &Idp, settings: IntrospectionSettings) -> Router {
    mcp_http::router(Config {
        database: None,
        validator: Some(Arc::new(IntrospectionValidator::new(settings).unwrap())),
        allowed_origins: Vec::new(),
        public_url: PUBLIC_URL.into(),
        authorization_servers: vec![idp.base.clone()],
        rate_limit_per_minute: 1_000,
        scope: mcp_http::SCOPE.into(),
        oauth_enabled: true,
        loopback_port: None,
        api_keys: false,
        browser_oauth: None,
    })
}

#[tokio::test]
async fn a_flood_of_unknown_tokens_is_bounded_and_spares_cached_users() {
    let idp = Idp::start().await;
    let mut limited = settings(&idp);
    assert_eq!(limited.max_in_flight, 16);
    assert_eq!(limited.calls_per_minute, 120);
    limited.calls_per_minute = 10;
    limited.cache_capacity = 4;
    let app = app_with(&idp, limited);
    assert_eq!(ping_with(&app, ACTIVE).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 1);

    let mut refused = 0;
    let mut unavailable = 0;
    for i in 0..40 {
        let answer = ping_with(&app, &format!("synthetic-flood-token-{i}")).await;
        match answer.status {
            StatusCode::UNAUTHORIZED => refused += 1,
            StatusCode::SERVICE_UNAVAILABLE => {
                assert_eq!(answer.header("retry-after"), "5");
                unavailable += 1;
            }
            other => panic!("unexpected {other}"),
        }
    }
    // Ten calls per minute in all: the first (ACTIVE) and nine flood tokens.
    assert_eq!(idp.calls(), 10, "the IdP sees at most the configured calls");
    assert_eq!((refused, unavailable), (9, 31));
    // The flood filled the 4-entry cache with refusals, yet the accepted
    // token is still served from the cache, without a call.
    assert_eq!(ping_with(&app, ACTIVE).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 10);
}

#[tokio::test]
async fn introspection_calls_in_flight_are_capped() {
    let idp = Idp::start().await;
    let slow =
        |sub: &str| Reply::Delayed(Duration::from_millis(600), active(sub, unix_now() + 3600));
    for token in ["synthetic-slow-a", "synthetic-slow-b", "synthetic-slow-c"] {
        idp.reply(token, slow("user-1"));
    }
    let mut capped = settings(&idp);
    capped.max_in_flight = 2;
    let app = app_with(&idp, capped);
    let third = async {
        // Starts while the first two calls are held open at the IdP.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = std::time::Instant::now();
        let answer = ping_with(&app, "synthetic-slow-c").await;
        (answer, started.elapsed())
    };
    let (a, b, (c, waited)) = tokio::join!(
        ping_with(&app, "synthetic-slow-a"),
        ping_with(&app, "synthetic-slow-b"),
        third
    );
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(b.status, StatusCode::OK);
    assert_eq!(c.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(c.header("retry-after"), "5");
    assert!(
        waited < Duration::from_millis(300),
        "refused without waiting"
    );
    assert_eq!(idp.calls(), 2, "the third token was not sent");
    // The slots are released: the third token now gets its call.
    assert_eq!(
        ping_with(&app, "synthetic-slow-c").await.status,
        StatusCode::OK
    );
    assert_eq!(idp.calls(), 3);
}

#[tokio::test]
async fn challenges_name_the_metadata_for_the_path() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let suffixed = format!("{PUBLIC_URL}/.well-known/oauth-protected-resource/mcp");
    let plain = format!("{PUBLIC_URL}/.well-known/oauth-protected-resource");

    let on_mcp = send(&app, post("/mcp", None, ping())).await;
    assert_eq!(on_mcp.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        on_mcp.header("www-authenticate"),
        format!("Bearer resource_metadata=\"{suffixed}\"")
    );
    // The document it names has resource <public-url>/mcp.
    let path = suffixed.strip_prefix(PUBLIC_URL).unwrap();
    let document = send(&app, Request::get(path).body(Body::empty()).unwrap())
        .await
        .json();
    assert_eq!(document["resource"], format!("{PUBLIC_URL}/mcp"));

    let on_calc = send(&app, post("/v1/calc/describe", None, "")).await;
    assert_eq!(on_calc.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        on_calc.header("www-authenticate"),
        format!("Bearer resource_metadata=\"{plain}\"")
    );
    let document = send(
        &app,
        Request::get(plain.strip_prefix(PUBLIC_URL).unwrap())
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .json();
    assert_eq!(document["resource"], PUBLIC_URL);

    // The 403 insufficient_scope challenge follows the same rule.
    let scoped_mcp = ping_with(&app, NO_SCOPE).await;
    assert_eq!(scoped_mcp.status, StatusCode::FORBIDDEN);
    assert!(scoped_mcp
        .header("www-authenticate")
        .ends_with(&format!("resource_metadata=\"{suffixed}\"")));
    let scoped_calc = send(&app, post("/v1/calc/describe", Some(NO_SCOPE), "")).await;
    assert_eq!(scoped_calc.status, StatusCode::FORBIDDEN);
    assert!(scoped_calc
        .header("www-authenticate")
        .ends_with(&format!("resource_metadata=\"{plain}\"")));
}

#[tokio::test]
async fn discovery_tries_the_rfc_8414_form_before_the_appended_form() {
    let idp = Idp::start().await;
    let issuer = format!("{}/tenant", idp.base);
    let inserted = "/.well-known/oauth-authorization-server/tenant";
    let appended = "/tenant/.well-known/oauth-authorization-server";
    let metadata = json!({"issuer": issuer, "introspection_endpoint": idp.endpoint()});
    let serve_only = |documents: &[(&str, u16)]| {
        let mut map = idp.state.documents.lock().unwrap();
        map.clear();
        for (path, status) in documents {
            map.insert((*path).to_string(), (*status, metadata.clone()));
        }
    };

    serve_only(&[(inserted, 200)]);
    let endpoint = oauth::discover_introspection_endpoint(&issuer)
        .await
        .unwrap();
    assert_eq!(endpoint.as_str(), idp.endpoint());

    // A 404 on the RFC 8414 form falls back to the appended form.
    serve_only(&[(appended, 200)]);
    let endpoint = oauth::discover_introspection_endpoint(&issuer)
        .await
        .unwrap();
    assert_eq!(endpoint.as_str(), idp.endpoint());

    // Any other failure of the first form is the error.
    serve_only(&[(inserted, 500), (appended, 200)]);
    let error = oauth::discover_introspection_endpoint(&issuer)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP status 500"), "{error}");
    assert!(error.contains(inserted), "{error}");

    serve_only(&[]);
    let error = oauth::discover_introspection_endpoint(&issuer)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP status 404"), "{error}");

    // The issuer check is unchanged: the document must name this issuer.
    idp.state.documents.lock().unwrap().insert(
        inserted.to_string(),
        (
            200,
            json!({"issuer": idp.base, "introspection_endpoint": idp.endpoint()}),
        ),
    );
    let error = oauth::discover_introspection_endpoint(&issuer)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("RFC 8414 section 3.3"), "{error}");
}

// ---------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------

fn refusal(options: &ServeOptions) -> String {
    mcp_http::prepare(options).unwrap_err().to_string()
}

#[tokio::test]
async fn start_up_refusals() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let good = oauth_options(&idp, dir.path());

    // OAuth alone authenticates a non-loopback bind.
    let mut exposed = good.clone();
    exposed.bind = "0.0.0.0:3819".into();
    let (_, config) = mcp_http::prepare(&exposed).unwrap();
    assert!(config.validator.is_some());
    assert!(config.oauth_enabled);
    assert_eq!(config.scope, "boxscore:calc");
    assert_eq!(config.authorization_servers, vec![idp.base.clone()]);

    // ... but only with --public-url: the challenge and the metadata must
    // name the address remote connectors use, not a loopback default.
    let mut no_public_url = exposed.clone();
    no_public_url.public_url = None;
    let error = refusal(&no_public_url);
    assert!(
        error.contains("needs --public-url") && error.contains("challenge"),
        "{error}"
    );
    // A loopback bind keeps its loopback default.
    let mut loopback = good.clone();
    loopback.public_url = None;
    assert!(mcp_http::prepare(&loopback).is_ok());

    // A URL with a password is refused, and the refusal does not echo it.
    for (flag, url) in [
        ("--authorization-server", "https://:s3cret@idp.example.test"),
        (
            "--introspection-endpoint",
            "https://:s3cret@idp.example.test",
        ),
        (
            "--authorization-server",
            "https://user:s3cret@idp.example.test",
        ),
    ] {
        let mut options = good.clone();
        if flag == "--authorization-server" {
            options.authorization_servers = vec![url.into()];
        } else {
            options.introspection_endpoint = Some(url.into());
        }
        let error = refusal(&options);
        assert!(
            error.contains(&format!("{flag} must not carry credentials")),
            "{error}"
        );
        assert!(!error.contains("s3cret"), "{error}");
    }

    #[cfg(unix)]
    {
        let secret = good.oauth_client_secret_file.clone().unwrap();
        set_mode(&secret, 0o644);
        let error = refusal(&good);
        assert!(
            error.contains("OAuth client secret file is accessible to other users"),
            "{error}"
        );
        assert!(!error.contains(CLIENT_SECRET));
        set_mode(&secret, 0o600);
    }

    let mut no_audience = good.clone();
    no_audience.oauth_audience = None;
    assert!(refusal(&no_audience).contains("--authorization-server needs --oauth-audience"));

    // An issuer alone is no longer an advertisement-only option.
    let issuer_only = ServeOptions {
        authorization_servers: vec!["https://idp.example.test".into()],
        ..ServeOptions::default()
    };
    assert!(refusal(&issuer_only).contains("--oauth-audience"));

    let mut no_client = good.clone();
    no_client.oauth_client_id = None;
    assert!(refusal(&no_client).contains("--oauth-client-secret-file needs --oauth-client-id"));
    let mut no_secret = good.clone();
    no_secret.oauth_client_secret_file = None;
    assert!(refusal(&no_secret).contains("--oauth-client-id needs --oauth-client-secret-file"));
    let mut neither = good.clone();
    neither.oauth_client_id = None;
    neither.oauth_client_secret_file = None;
    assert!(refusal(&neither).contains("needs --oauth-client-id and --oauth-client-secret-file"));

    // OAuth flags without an issuer.
    for (flag, options) in [
        (
            "--introspection-endpoint",
            ServeOptions {
                introspection_endpoint: Some(idp.endpoint()),
                ..ServeOptions::default()
            },
        ),
        (
            "--oauth-client-id",
            ServeOptions {
                oauth_client_id: Some(CLIENT_ID.into()),
                ..ServeOptions::default()
            },
        ),
        (
            "--oauth-client-secret-file",
            ServeOptions {
                oauth_client_secret_file: good.oauth_client_secret_file.clone(),
                ..ServeOptions::default()
            },
        ),
        (
            "--oauth-audience",
            ServeOptions {
                oauth_audience: Some(AUDIENCE.into()),
                ..ServeOptions::default()
            },
        ),
        (
            "--oauth-scope",
            ServeOptions {
                oauth_scope: Some("x".into()),
                ..ServeOptions::default()
            },
        ),
    ] {
        let error = refusal(&options);
        assert!(
            error.contains(&format!("{flag} needs --authorization-server")),
            "{error}"
        );
    }

    let mut two = good.clone();
    two.authorization_servers
        .push("https://idp2.example.test".into());
    assert!(refusal(&two).contains("exactly one --authorization-server"));

    let mut plain_http = good.clone();
    plain_http.introspection_endpoint = Some("http://idp.example.test/introspect".into());
    assert!(refusal(&plain_http).contains("must be https"));
    let mut plain_issuer = good.clone();
    plain_issuer.authorization_servers = vec!["http://idp.example.test".into()];
    assert!(refusal(&plain_issuer).contains("must be https"));

    let mut bad_scope = good.clone();
    bad_scope.oauth_scope = Some("two scopes".into());
    assert!(refusal(&bad_scope).contains("--oauth-scope"));
    let mut bad_audience = good.clone();
    bad_audience.oauth_audience = Some(String::new());
    assert!(refusal(&bad_audience).contains("--oauth-audience"));

    let mut empty_secret = good.clone();
    empty_secret.oauth_client_secret_file = Some(private_file(dir.path(), "empty", "\n"));
    assert!(refusal(&empty_secret).contains("must hold one line"));
    let mut missing_secret = good.clone();
    missing_secret.oauth_client_secret_file = Some(dir.path().join("missing"));
    assert!(refusal(&missing_secret).contains("cannot be read"));

    // prepare does not discover; resolve does.
    let mut discover = good.clone();
    discover.introspection_endpoint = None;
    assert!(refusal(&discover).contains("introspection endpoint"));
    let (_, config) = mcp_http::resolve(&discover).await.unwrap();
    let app = mcp_http::router(config);
    assert_eq!(ping_with(&app, ACTIVE).await.status, StatusCode::OK);
    assert_eq!(idp.calls(), 1, "the discovered endpoint is the mock's");

    // Metadata without introspection_endpoint, or naming another issuer,
    // refuses to start.
    idp.state
        .metadata
        .lock()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("introspection_endpoint");
    let error = mcp_http::resolve(&discover).await.unwrap_err().to_string();
    assert!(error.contains("no introspection_endpoint"), "{error}");
    *idp.state.metadata.lock().unwrap() = json!({
        "issuer": "https://elsewhere.example.test",
        "introspection_endpoint": idp.endpoint(),
    });
    let error = mcp_http::resolve(&discover).await.unwrap_err().to_string();
    assert!(error.contains("issuer"), "{error}");

    // An inconsistent combination is refused before any network call.
    let mut unreachable = discover.clone();
    unreachable.authorization_servers = vec!["https://idp.invalid".into()];
    unreachable.oauth_client_id = None;
    let error = mcp_http::resolve(&unreachable)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("--oauth-client-id"), "{error}");
}

// ---------------------------------------------------------------------------
// Metadata and OpenAPI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metadata_documents() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let get_json = |path: &'static str| {
        let app = app.clone();
        async move {
            let answer = send(&app, Request::get(path).body(Body::empty()).unwrap()).await;
            assert_eq!(answer.status, StatusCode::OK, "{path}");
            answer.json()
        }
    };
    assert_eq!(
        get_json("/.well-known/oauth-protected-resource").await,
        json!({
            "resource": PUBLIC_URL,
            "authorization_servers": [idp.base],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["boxscore:calc"],
        })
    );
    assert_eq!(
        get_json("/.well-known/oauth-protected-resource/mcp").await,
        json!({
            "resource": format!("{PUBLIC_URL}/mcp"),
            "authorization_servers": [idp.base],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["boxscore:calc"],
        })
    );

    let mut custom = oauth_options(&idp, dir.path());
    custom.oauth_scope = Some("boxscore:custom".into());
    let custom = app_from(&custom);
    let answer = send(
        &custom,
        Request::get("/.well-known/oauth-protected-resource/mcp")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        answer.json()["scopes_supported"],
        json!(["boxscore:custom"])
    );
}

#[tokio::test]
async fn openapi_security_schemes() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let app = app_from(&oauth_options(&idp, dir.path()));
    let doc = send(
        &app,
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["bearer"]["type"], "http");
    assert_eq!(schemes["bearer"]["scheme"], "bearer");
    assert_eq!(schemes["oauth2"]["type"], "oauth2");
    assert!(schemes["oauth2"]["flows"].is_object());
    assert!(schemes["oauth2"]["description"]
        .as_str()
        .unwrap()
        .contains("RFC 7662"));
    assert_eq!(
        doc["security"],
        json!([{"bearer": []}, {"oauth2": ["boxscore:calc"]}])
    );
    let mcp = &doc["paths"]["/mcp"]["post"]["responses"];
    assert!(mcp["403"]["description"]
        .as_str()
        .unwrap()
        .contains("INSUFFICIENT_SCOPE"));
    assert!(mcp["503"]["headers"]["Retry-After"].is_object());
    let suffixed = &doc["paths"]["/.well-known/oauth-protected-resource/mcp"]["get"];
    assert_eq!(suffixed["security"], json!([]));
    assert_eq!(
        doc,
        mcp_http::openapi_with_scope(PUBLIC_URL, Some(mcp_http::SCOPE))
    );

    let mut custom = oauth_options(&idp, dir.path());
    custom.oauth_scope = Some("boxscore:custom".into());
    let custom_doc = send(
        &app_from(&custom),
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert_eq!(
        custom_doc["security"],
        json!([{"bearer": []}, {"oauth2": ["boxscore:custom"]}])
    );
    assert_eq!(
        custom_doc,
        mcp_http::openapi_with_scope(PUBLIC_URL, Some("boxscore:custom"))
    );

    // A server with API keys only advertises no oauth2 scheme.
    let keys_only = ServeOptions {
        public_url: Some(PUBLIC_URL.into()),
        api_key_file: Some(private_file(dir.path(), "keys", &format!("ci:{API_KEY}\n"))),
        ..ServeOptions::default()
    };
    let keys_doc = send(
        &app_from(&keys_only),
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert_eq!(keys_doc["security"], json!([{"bearer": []}]));
    let schemes = keys_doc["components"]["securitySchemes"]
        .as_object()
        .unwrap();
    assert_eq!(schemes.keys().collect::<Vec<_>>(), ["bearer"]);
    assert!(!keys_doc.to_string().contains("\"oauth2\""));
    assert_eq!(keys_doc, mcp_http::openapi(PUBLIC_URL));
}

// ---------------------------------------------------------------------------
// Real socket
// ---------------------------------------------------------------------------

struct Served {
    child: std::process::Child,
    stderr: BufReader<std::process::ChildStderr>,
    base: String,
    banner: String,
}

/// Start `serve` with `args`; the first stderr line names the address.
async fn start_serve(args: Vec<String>) -> Served {
    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .arg("serve")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    // Read off the runtime: the server may first call the mock, which runs
    // on this runtime.
    let (stderr, banner) = tokio::task::spawn_blocking(move || {
        let mut stderr = BufReader::new(stderr);
        let mut banner = String::new();
        stderr.read_line(&mut banner).unwrap();
        (stderr, banner)
    })
    .await
    .unwrap();
    let base = banner
        .split_whitespace()
        .find(|w| w.starts_with("http://127.0.0.1:"))
        .unwrap_or_else(|| panic!("no address in {banner:?}"))
        .to_string();
    Served {
        child,
        stderr,
        base,
        banner,
    }
}

impl Served {
    /// SIGTERM, wait for exit 0, return the rest of stderr.
    async fn stop(mut self) -> String {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not stop on SIGTERM");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(status.code(), Some(0));
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut self.stderr, &mut rest).unwrap();
        format!("{}{rest}", self.banner)
    }
}

fn oauth_args(idp: &Idp, secret: &Path) -> Vec<String> {
    [
        "--bind",
        "127.0.0.1:0",
        "--authorization-server",
        &idp.base,
        "--oauth-client-id",
        CLIENT_ID,
        "--oauth-client-secret-file",
        secret.to_str().unwrap(),
        "--oauth-audience",
        AUDIENCE,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_socket_oauth_initialize() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secret = private_file(dir.path(), "client-secret", CLIENT_SECRET);
    // No --introspection-endpoint: discovered from the mock's metadata.
    let served = start_serve(oauth_args(&idp, &secret)).await;
    assert!(
        served.banner.contains("OAuth introspection"),
        "{}",
        served.banner
    );
    let base = served.base.clone();
    let client = reqwest::Client::new();

    let unauthenticated = client
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status().as_u16(), 401);
    assert_eq!(
        unauthenticated.headers()["www-authenticate"],
        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"")
            .as_str()
    );
    let metadata: Value = client
        .get(format!("{base}/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["resource"], format!("{base}/mcp"));
    assert_eq!(metadata["authorization_servers"], json!([idp.base]));

    let init: Value = client
        .post(format!("{base}/mcp"))
        .bearer_auth(ACTIVE)
        .header("accept", ACCEPT)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "oauth-socket-test", "version": "0"}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "boxscore-exact");
    assert_eq!(idp.calls(), 1);

    let log = served.stop().await;
    assert!(
        log.contains(&format!("introspection {}", idp.endpoint())),
        "{log}"
    );
    assert!(
        !log.contains(ACTIVE) && !log.contains(CLIENT_SECRET),
        "{log}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_socket_unreachable_introspection_is_503_and_logs_no_token() {
    let idp = Idp::start().await;
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let secret = private_file(dir.path(), "client-secret", CLIENT_SECRET);
    let mut args = oauth_args(&idp, &secret);
    args.extend([
        "--introspection-endpoint".to_string(),
        format!("http://127.0.0.1:{closed}/introspect"),
    ]);
    let served = start_serve(args).await;
    let response = reqwest::Client::new()
        .post(format!("{}/mcp", served.base))
        .bearer_auth(ACTIVE)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(response.headers()["retry-after"], "5");
    let log = served.stop().await;
    let failures: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("OAuth introspection at"))
        .collect();
    assert_eq!(failures.len(), 1, "{log}");
    assert!(failures[0].contains("connection failed"), "{log}");
    assert!(!log.contains(ACTIVE), "no token in the log: {log}");
    assert!(!log.contains(CLIENT_SECRET), "{log}");
}

// ---------------------------------------------------------------------------
// Browser workbench sign-in (authorization code with PKCE)
// ---------------------------------------------------------------------------

fn browser_options(idp: &Idp, dir: &Path) -> ServeOptions {
    ServeOptions {
        oauth_browser_client_id: Some(BROWSER_CLIENT.into()),
        ..oauth_options(idp, dir)
    }
}

#[tokio::test]
async fn browser_sign_in_discovers_its_endpoints_or_refuses_to_start() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let options = browser_options(&idp, dir.path());

    // prepare does not discover: the endpoints come from the metadata.
    let error = refusal(&options);
    assert!(
        error.contains("needs the issuer's authorization_endpoint and token_endpoint"),
        "{error}"
    );
    // resolve reads them even though --introspection-endpoint is given.
    let (_, config) = mcp_http::resolve(&options).await.unwrap();
    assert_eq!(
        config.browser_oauth,
        Some(mcp_http::BrowserOAuth {
            issuer: idp.base.clone(),
            client_id: BROWSER_CLIENT.into(),
            authorization_endpoint: format!("{}/authorize", idp.base),
            token_endpoint: format!("{}/token", idp.base),
            scope: "openid boxscore:calc".into(),
            audience: AUDIENCE.into(),
        })
    );
    // A configured scope is the one requested.
    let mut custom = options.clone();
    custom.oauth_scope = Some("boxscore:custom".into());
    let (_, config) = mcp_http::resolve(&custom).await.unwrap();
    assert_eq!(
        config.browser_oauth.unwrap().scope,
        "openid boxscore:custom"
    );

    // Each endpoint must be present and https (http for loopback only).
    let good = idp.state.metadata.lock().unwrap().clone();
    for (field, value, expected) in [
        (
            "authorization_endpoint",
            None,
            "it has no authorization_endpoint",
        ),
        ("token_endpoint", None, "it has no token_endpoint"),
        (
            "authorization_endpoint",
            Some("http://idp.example.test/authorize"),
            "authorization_endpoint in the authorization server metadata must be https",
        ),
        (
            "token_endpoint",
            Some("http://idp.example.test/token"),
            "token_endpoint in the authorization server metadata must be https",
        ),
        (
            "token_endpoint",
            Some("https://idp.example.test/token?tenant=x"),
            "token_endpoint in the authorization server metadata must not carry",
        ),
    ] {
        let mut metadata = good.clone();
        match value {
            None => {
                metadata.as_object_mut().unwrap().remove(field);
            }
            Some(value) => metadata[field] = json!(value),
        }
        *idp.state.metadata.lock().unwrap() = metadata;
        let error = mcp_http::resolve(&options).await.unwrap_err().to_string();
        assert!(error.contains(expected), "{field}: {error}");
        assert!(
            error.contains("--oauth-browser-client-id needs the issuer's metadata"),
            "{error}"
        );
    }
    // With the introspection endpoint discovered too, a missing browser
    // endpoint points at the metadata, not at --introspection-endpoint.
    let mut discovering = options.clone();
    discovering.introspection_endpoint = None;
    let mut metadata = good.clone();
    metadata
        .as_object_mut()
        .unwrap()
        .remove("authorization_endpoint");
    *idp.state.metadata.lock().unwrap() = metadata;
    let error = mcp_http::resolve(&discovering)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("it has no authorization_endpoint"),
        "{error}"
    );
    assert!(
        error.contains("check the identity provider's authorization server metadata"),
        "{error}"
    );
    assert!(!error.contains("pass --introspection-endpoint"), "{error}");

    // PKCE: advertised methods must include S256; absent is accepted.
    for (methods, accepted) in [
        (json!(["S256", "plain"]), true),
        (json!(["S256"]), true),
        (json!(["plain"]), false),
        (json!([]), false),
        (json!("S256"), false),
    ] {
        let mut metadata = good.clone();
        metadata["code_challenge_methods_supported"] = methods.clone();
        *idp.state.metadata.lock().unwrap() = metadata;
        match mcp_http::resolve(&options).await {
            Ok(_) => assert!(accepted, "{methods}"),
            Err(error) => {
                assert!(!accepted, "{methods}: {error}");
                assert!(
                    error
                        .to_string()
                        .contains("code_challenge_methods_supported does not include S256"),
                    "{error}"
                );
            }
        }
    }
    *idp.state.metadata.lock().unwrap() = good.clone();
    assert!(mcp_http::resolve(&options).await.is_ok());

    // Without the browser client, an explicit introspection endpoint still
    // needs no metadata at all.
    idp.state
        .metadata
        .lock()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .clear();
    let mut plain = options.clone();
    plain.oauth_browser_client_id = None;
    let (_, config) = mcp_http::resolve(&plain).await.unwrap();
    assert!(config.browser_oauth.is_none());

    // The flag is valid only with OAuth configured, and is one client id.
    let mut no_audience = options.clone();
    no_audience.oauth_audience = None;
    assert!(refusal(&no_audience).starts_with("--oauth-browser-client-id needs OAuth configured"));
    let mut no_secret = options.clone();
    no_secret.oauth_client_secret_file = None;
    assert!(refusal(&no_secret).starts_with("--oauth-browser-client-id needs OAuth configured"));
    let mut spaced = options.clone();
    spaced.oauth_browser_client_id = Some("two words".into());
    assert!(refusal(&spaced).contains("--oauth-browser-client-id must be"));
}

/// The page's flow, by hand over HTTP: auth.json from the real router, the
/// mock's authorize redirect, the code exchange with the verifier, then the
/// token on `/mcp` and `/v1/calc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_sign_in_code_exchange_over_http() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let mut options = browser_options(&idp, dir.path());
    options.public_url = Some(server.clone());
    options.introspection_endpoint = None;
    // What IT registers for the page: its redirect URI and web origin.
    *idp.state.redirect_uris.lock().unwrap() = vec![format!("{server}/")];
    *idp.state.page_origins.lock().unwrap() = vec![server.clone()];
    let (_, config) = mcp_http::resolve(&options).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mcp_http::router(config))
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let page = client.get(format!("{server}/")).send().await.unwrap();
    let csp = page.headers()["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains(&format!("connect-src 'self' {}/token; ", idp.base)),
        "{csp}"
    );
    let auth: Value = client
        .get(format!("{server}/workbench/auth.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(auth["mode"], "oauth");
    assert_eq!(auth["redirect_uri"], format!("{server}/"));
    assert_eq!(auth["client_id"], BROWSER_CLIENT);
    let field = |name: &str| auth[name].as_str().unwrap().to_string();

    // What the page does on "Sign in": a verifier, its S256 challenge, a
    // state, and the authorize URL built on auth.json's endpoint. `changed`
    // replaces (or, with "", drops) one parameter, for the mock's refusals.
    let authorize_with = |verifier: &str, state: &str, changed: (&str, &str)| {
        let mut url = reqwest::Url::parse(&field("authorization_endpoint")).unwrap();
        let challenge = s256(verifier);
        let (client_id, redirect_uri, scope, audience) = (
            field("client_id"),
            field("redirect_uri"),
            field("scope"),
            field("audience"),
        );
        let pairs = [
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", scope.as_str()),
            ("state", state),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", audience.as_str()),
        ];
        for (name, value) in pairs {
            let value = if name == changed.0 { changed.1 } else { value };
            if !value.is_empty() {
                url.query_pairs_mut().append_pair(name, value);
            }
        }
        url
    };
    let authorize_url = |verifier: &str, state: &str| authorize_with(verifier, state, ("", ""));
    let redirected = |response: reqwest::Response| {
        assert_eq!(response.status().as_u16(), 302);
        let location =
            reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
        let mut landing = location.clone();
        landing.set_query(None);
        assert_eq!(landing.as_str(), format!("{server}/"));
        let pairs: HashMap<String, String> = location.query_pairs().into_owned().collect();
        (location, pairs)
    };
    let exchange_from = |code: String, verifier: String, origin: Option<String>| {
        let client = client.clone();
        let (token_endpoint, redirect_uri) = (field("token_endpoint"), field("redirect_uri"));
        async move {
            let mut post = client.post(token_endpoint);
            if let Some(origin) = origin {
                post = post.header("origin", origin);
            }
            post.form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("client_id", BROWSER_CLIENT),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .unwrap()
        }
    };
    let exchange =
        |code: String, verifier: String| exchange_from(code, verifier, Some(server.clone()));

    let verifier = "synthetic-pkce-verifier-0123456789abcdefghijklmnopqrstuvwxyz-_ABCD";
    assert_eq!(verifier.len(), 66);
    let response = client
        .get(authorize_url(verifier, "synthetic-state-1"))
        .send()
        .await
        .unwrap();
    let (location, pairs) = redirected(response);
    assert_eq!(pairs["state"], "synthetic-state-1");
    // The redirect lands on the page itself, query and all.
    let landed = client.get(location.clone()).send().await.unwrap();
    assert_eq!(landed.status().as_u16(), 200);
    assert_eq!(landed.headers()["referrer-policy"], "no-referrer");

    let granted = exchange(pairs["code"].clone(), verifier.to_string()).await;
    assert_eq!(granted.status().as_u16(), 200);
    // CORS for the page's origin, as a browser needs to read the answer.
    assert_eq!(
        granted.headers()["access-control-allow-origin"],
        server.as_str()
    );
    let granted: Value = granted.json().await.unwrap();
    assert_eq!(granted["token_type"], "Bearer");
    assert!(granted.get("refresh_token").is_none());
    let token = granted["access_token"].as_str().unwrap().to_string();

    // The token completes initialize and a /v1/calc call, from the page's
    // own origin; introspected once, then cached.
    let init: Value = client
        .post(format!("{server}/mcp"))
        .bearer_auth(&token)
        .header("accept", ACCEPT)
        .header("origin", &server)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "workbench-pkce-test", "version": "0"}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        init["result"]["serverInfo"]["name"], "boxscore-exact",
        "{init}"
    );
    let described = client
        .post(format!("{server}/v1/calc/describe"))
        .bearer_auth(&token)
        .header("origin", &server)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(described.status().as_u16(), 200);
    let described: Value = described.json().await.unwrap();
    assert_eq!(described["status"], "calculated", "{described}");
    assert_eq!(idp.calls(), 1);

    // The code is single use.
    let reused = exchange(pairs["code"].clone(), verifier.to_string()).await;
    assert_eq!(reused.status().as_u16(), 400);

    // A wrong verifier is refused by the IdP, and what comes back is the
    // IdP's own error, which the page shows as text.
    let response = client
        .get(authorize_url(verifier, "synthetic-state-2"))
        .send()
        .await
        .unwrap();
    let (_, pairs) = redirected(response);
    let refused = exchange(
        pairs["code"].clone(),
        "synthetic-wrong-verifier-0123456789abcdefghijklmnopqrstuvwxyz".to_string(),
    )
    .await;
    assert_eq!(refused.status().as_u16(), 400);
    let refused: Value = refused.json().await.unwrap();
    assert_eq!(
        refused,
        json!({"error": "invalid_grant", "error_description": "PKCE verification failed"})
    );
    // The mock is strict where a real IdP is, so the flow above proves the
    // page's requests are right. An unregistered redirect URI (localhost
    // for 127.0.0.1, say) is answered at the IdP, never redirected to.
    let localhost = server.replace("127.0.0.1", "localhost") + "/";
    let unregistered = client
        .get(authorize_with(
            verifier,
            "synthetic-state-3",
            ("redirect_uri", &localhost),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unregistered.status().as_u16(), 400);
    assert!(unregistered.headers().get("location").is_none());
    let unregistered: Value = unregistered.json().await.unwrap();
    assert_eq!(
        unregistered["error_description"],
        "redirect_uri is not registered"
    );
    // A request without state is refused.
    let stateless = client
        .get(authorize_with(verifier, "", ("", "")))
        .send()
        .await
        .unwrap();
    assert_eq!(stateless.status().as_u16(), 400);
    assert!(stateless.headers().get("location").is_none());
    // The token endpoint answers the page's origin only, with no CORS
    // header for any other.
    for origin in [None, Some("https://elsewhere.example.test".to_string())] {
        let response = client
            .get(authorize_url(verifier, "synthetic-state-4"))
            .send()
            .await
            .unwrap();
        let (_, pairs) = redirected(response);
        let refused = exchange_from(pairs["code"].clone(), verifier.to_string(), origin).await;
        assert_eq!(refused.status().as_u16(), 403);
        assert!(refused
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }
    // A code is bound to the client it was issued to.
    let response = client
        .get(authorize_with(
            verifier,
            "synthetic-state-5",
            ("client_id", OTHER_CLIENT),
        ))
        .send()
        .await
        .unwrap();
    let (_, pairs) = redirected(response);
    let refused: Value = exchange(pairs["code"].clone(), verifier.to_string())
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        refused["error_description"],
        "the code was issued to another client"
    );
    // A code expires.
    *idp.state.code_ttl.lock().unwrap() = Duration::ZERO;
    let response = client
        .get(authorize_url(verifier, "synthetic-state-6"))
        .send()
        .await
        .unwrap();
    let (_, pairs) = redirected(response);
    let refused: Value = exchange(pairs["code"].clone(), verifier.to_string())
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(refused["error_description"], "the code has expired");
    *idp.state.code_ttl.lock().unwrap() = CODE_TTL;

    // A token the IdP never issued is still the 401 challenge.
    let unknown = client
        .post(format!("{server}/v1/calc/describe"))
        .bearer_auth("synthetic-browser-token-never-issued")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status().as_u16(), 401);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_socket_browser_sign_in_banner_and_auth_json() {
    let idp = Idp::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secret = private_file(dir.path(), "client-secret", CLIENT_SECRET);
    let mut args = oauth_args(&idp, &secret);
    args.extend([
        "--oauth-browser-client-id".to_string(),
        BROWSER_CLIENT.to_string(),
    ]);
    let served = start_serve(args).await;
    let base = served.base.clone();
    let auth: Value = reqwest::get(format!("{base}/workbench/auth.json"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(auth["mode"], "oauth");
    assert_eq!(auth["redirect_uri"], format!("{base}/"));
    assert_eq!(auth["token_endpoint"], format!("{}/token", idp.base));
    let log = served.stop().await;
    assert!(
        log.contains(&format!(
            "boxscore-exact serve: workbench sign-in: OAuth via {} (client {BROWSER_CLIENT})\n",
            idp.base
        )),
        "{log}"
    );
    assert!(!log.contains(CLIENT_SECRET), "{log}");

    // Without OAuth the flag refuses to start, naming the rule.
    let refused = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--oauth-browser-client-id",
            BROWSER_CLIENT,
        ])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("--oauth-browser-client-id needs OAuth configured"),
        "{stderr}"
    );
}
