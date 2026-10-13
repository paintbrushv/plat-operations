//! The streamable-HTTP MCP server, the /v1/calc routes, authentication and
//! the published OpenAPI document. Most tests drive the axum router with
//! `oneshot`; one starts the real binary on a loopback socket. Synthetic data
//! only.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use boxscore::exact::{
    catalog, mcp as mcp_server,
    mcp_http::{self, Config, ServeOptions, StaticKeys},
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
};
use tower::ServiceExt;

const SECRET: &str = "synthetic-test-secret-0123456789abcdef";
const OTHER_SECRET: &str = "synthetic-test-secret-zyxwvutsrqponmlk";
const PUBLIC_URL: &str = "https://boxscore.example.test";
const ACCEPT: &str = "application/json, text/event-stream";

fn config(database: Option<PathBuf>) -> Config {
    // A trailing newline and a CRLF line ending, as editors leave them.
    let keys = StaticKeys::parse(&format!("ci:{SECRET}\r\nops:{OTHER_SECRET}\n\n")).unwrap();
    Config {
        database,
        validator: Some(Arc::new(keys)),
        allowed_origins: vec!["https://claude.ai".into()],
        public_url: PUBLIC_URL.into(),
        authorization_servers: vec!["https://idp.example.test".into()],
        rate_limit_per_minute: 60,
        scope: mcp_http::SCOPE.into(),
        oauth_enabled: false,
    }
}

fn app() -> Router {
    mcp_http::router(config(None))
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {}",
                String::from_utf8_lossy(&self.body[..self.body.len().min(500)])
            )
        })
    }
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Reply {
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

fn rpc(id: i64, method: &str, params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .unwrap()
}

async fn mcp(app: &Router, id: i64, method: &str, params: Value) -> Value {
    let reply = send(app, post("/mcp", Some(SECRET), rpc(id, method, params))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.headers["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    let response = reply.json();
    assert_eq!(response["id"], id, "{response}");
    response
}

fn line(code: &str, category: &str, amount: &str) -> Value {
    json!({"account_code": code, "account_name": code, "category": category, "amount": amount})
}

fn variance_input() -> Value {
    json!({
        "currency": "USD", "expense_convention": "positive_costs",
        "actuals": [line("4000", "rental income", "100.10"), line("5000", "payroll", "30.00")],
        "budgets": [line("4000", "rental income", "90.05"), line("5000", "payroll", "25.00")],
    })
}

/// The 401 challenge for a request to `path`: on `/mcp` it names the
/// `/mcp`-suffixed metadata document, elsewhere the unsuffixed one.
fn assert_challenge(reply: &Reply, path: &str) {
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let metadata = if path == "/mcp" {
        "/.well-known/oauth-protected-resource/mcp"
    } else {
        "/.well-known/oauth-protected-resource"
    };
    assert_eq!(
        reply.headers["www-authenticate"],
        format!("Bearer resource_metadata=\"{PUBLIC_URL}{metadata}\"").as_str()
    );
}

/// Run the same tools/call through the stdio binary.
fn stdio_call(name: &str, arguments: &Value) -> Value {
    stdio_request("tools/call", &json!({"name": name, "arguments": arguments}))["result"].clone()
}

/// Send one request after the handshake through the stdio binary and return
/// the whole response.
fn stdio_request(method: &str, params: &Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .arg("mcp")
        .env_remove("BOXSCORE_MCP_DEBUG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params}),
        ] {
            serde_json::to_writer(&mut stdin, &message).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    let lines: Vec<Value> = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    lines[1].clone()
}

#[tokio::test]
async fn http_session_matches_the_stdio_path() {
    let app = app();
    let init = mcp(
        &app,
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "http-test", "version": "0"}}),
    )
    .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "boxscore-exact");

    // The initialized notification is accepted with an empty body.
    let note =
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .unwrap();
    let mut request = post("/mcp", Some(SECRET), note);
    request
        .headers_mut()
        .insert("mcp-protocol-version", "2025-06-18".parse().unwrap());
    let accepted = send(&app, request).await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    assert!(accepted.body.is_empty());

    let listed = mcp(&app, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let catalog_names: Vec<&str> = catalog::CATALOG.iter().map(|c| c.name).collect();
    assert_eq!(names, catalog_names);

    let arguments = variance_input();
    let called = mcp(
        &app,
        3,
        "tools/call",
        json!({"name": "noi_variance", "arguments": arguments}),
    )
    .await;
    let http = &called["result"];
    assert_eq!(http["isError"], false);
    let stdio = stdio_call("noi_variance", &arguments);
    assert_eq!(stdio["isError"], false);
    let (h, s) = (&http["structuredContent"], &stdio["structuredContent"]);
    assert_eq!(h["result"], s["result"]);
    assert_eq!(h["output_sha256"], s["output_sha256"]);
    assert_eq!(h["input_sha256"], s["input_sha256"]);
    assert_eq!(h["function"], "noi_variance");
    assert_eq!(h["result"]["noi_bridge"]["noi_variance"], "5.05");

    // Prompts go through the same dispatcher: both are listed, and start_here
    // returns the same text over HTTP as over stdio.
    let prompts = mcp(&app, 4, "prompts/list", json!({})).await;
    assert_eq!(prompts["result"]["prompts"], json!(mcp_server::prompts()));
    let start = mcp(&app, 5, "prompts/get", json!({"name": "start_here"})).await;
    let text = start["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(text, mcp_server::start_here_text());
    for question in mcp_server::START_HERE_QUESTIONS {
        assert!(text.contains(question), "{question}");
    }
    let stdio_prompt = stdio_request("prompts/get", &json!({"name": "start_here"}));
    assert_eq!(stdio_prompt["result"], start["result"]);
    let unknown = mcp(&app, 6, "prompts/get", json!({"name": "no_such_prompt"})).await;
    assert_eq!(unknown["error"]["code"], -32602);

    // The /v1/calc route returns the same numbers and the same output hash.
    let direct = send(
        &app,
        post(
            "/v1/calc/noi_variance",
            Some(OTHER_SECRET),
            serde_json::to_vec(&arguments).unwrap(),
        ),
    )
    .await;
    assert_eq!(direct.status, StatusCode::OK);
    let envelope = direct.json();
    assert_eq!(envelope["result"], s["result"]);
    assert_eq!(envelope["output_sha256"], s["output_sha256"]);
    assert_eq!(envelope["status"], "calculated");
}

#[tokio::test]
async fn authentication_challenges_and_public_routes() {
    let app = app();
    let missing = send(&app, post("/mcp", None, rpc(1, "ping", json!({})))).await;
    assert_challenge(&missing, "/mcp");
    let wrong = send(
        &app,
        post(
            "/mcp",
            Some("synthetic-wrong-secret-0123456789abcd"),
            rpc(1, "ping", json!({})),
        ),
    )
    .await;
    assert_challenge(&wrong, "/mcp");
    // Missing and wrong tokens get byte-identical responses.
    assert_eq!(missing.body, wrong.body);
    let mut basic = post("/v1/calc/describe", None, "");
    basic
        .headers_mut()
        .insert("authorization", format!("Basic {SECRET}").parse().unwrap());
    assert_challenge(&send(&app, basic).await, "/v1/calc/describe");
    // A key id is not a secret.
    assert_challenge(
        &send(&app, post("/v1/calc/describe", Some("ci"), "")).await,
        "/v1/calc/describe",
    );

    let ok = send(&app, post("/mcp", Some(SECRET), rpc(1, "ping", json!({})))).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json(), json!({"jsonrpc": "2.0", "id": 1, "result": {}}));

    let health = send(&app, Request::get("/health").body(Body::empty()).unwrap()).await;
    assert_eq!(health.status, StatusCode::OK);
    let health = health.json();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["producer"], catalog::producer());

    let metadata = send(
        &app,
        Request::get("/.well-known/oauth-protected-resource")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(metadata.status, StatusCode::OK);
    assert_eq!(
        metadata.json(),
        json!({
            "resource": PUBLIC_URL,
            "authorization_servers": ["https://idp.example.test"],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["boxscore:calc"],
        })
    );

    let openapi = send(
        &app,
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(openapi.status, StatusCode::OK);

    let get_mcp = send(
        &app,
        Request::get("/mcp")
            .header("authorization", format!("Bearer {SECRET}"))
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(get_mcp.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(get_mcp.headers["allow"], "POST");

    let nowhere = send(&app, Request::get("/admin").body(Body::empty()).unwrap()).await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn origin_allowlist_blocks_rebinding() {
    let app = app();
    for (origin, expected) in [
        ("https://evil.example", StatusCode::FORBIDDEN),
        ("null", StatusCode::FORBIDDEN),
        ("https://claude.ai.evil.example", StatusCode::FORBIDDEN),
        ("https://claude.ai", StatusCode::OK),
        ("HTTPS://CLAUDE.AI:443", StatusCode::OK),
    ] {
        let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        let reply = send(&app, request).await;
        assert_eq!(reply.status, expected, "{origin}");
    }
    // Public routes too, and before authentication.
    let evil_health = send(
        &app,
        Request::get("/health")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(evil_health.status, StatusCode::FORBIDDEN);
    let mut unauthenticated = post("/mcp", None, rpc(1, "ping", json!({})));
    unauthenticated
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert_eq!(
        send(&app, unauthenticated).await.status,
        StatusCode::FORBIDDEN
    );

    // With no allowlist, any Origin header is refused.
    let mut closed = config(None);
    closed.allowed_origins.clear();
    let closed = mcp_http::router(closed);
    let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
    request
        .headers_mut()
        .insert("origin", "https://claude.ai".parse().unwrap());
    assert_eq!(send(&closed, request).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn transport_refusals() {
    let app = app();

    // 3 MiB body: 413 on both POST routes.
    let mut huge =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{\"pad\":\"".to_vec();
    huge.extend(std::iter::repeat_n(b'a', 3 * 1024 * 1024));
    huge.extend_from_slice(b"\"}}");
    let too_big = send(&app, post("/mcp", Some(SECRET), huge.clone())).await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(too_big.json()["error"]["code"], -32600);
    let too_big = send(&app, post("/v1/calc/noi_variance", Some(SECRET), huge)).await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);

    // Batches are refused as the stdio server refuses them.
    let batch = serde_json::to_vec(&json!([
        {"jsonrpc": "2.0", "id": 1, "method": "ping"},
        {"jsonrpc": "2.0", "id": 2, "method": "ping"}
    ]))
    .unwrap();
    let refused = send(&app, post("/mcp", Some(SECRET), batch)).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.json()["error"]["code"], -32600);
    assert_eq!(refused.json()["id"], Value::Null);

    let parse = send(&app, post("/mcp", Some(SECRET), "{\"jsonrpc\":")).await;
    assert_eq!(parse.status, StatusCode::BAD_REQUEST);
    assert_eq!(parse.json()["error"]["code"], -32700);

    // Unknown MCP-Protocol-Version header: 400 with a JSON body.
    let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
    request
        .headers_mut()
        .insert("mcp-protocol-version", "1999-01-01".parse().unwrap());
    let bad_version = send(&app, request).await;
    assert_eq!(bad_version.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_version.json()["error"]["code"], -32600);
    let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
    request
        .headers_mut()
        .insert("mcp-protocol-version", "2025-03-26".parse().unwrap());
    assert_eq!(send(&app, request).await.status, StatusCode::OK);

    // A client that accepts only SSE cannot be served.
    let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
    request
        .headers_mut()
        .insert("accept", "text/event-stream".parse().unwrap());
    assert_eq!(send(&app, request).await.status, StatusCode::NOT_ACCEPTABLE);

    let mut request = post("/mcp", Some(SECRET), rpc(1, "ping", json!({})));
    request
        .headers_mut()
        .insert("content-type", "text/plain".parse().unwrap());
    assert_eq!(
        send(&app, request).await.status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
}

#[tokio::test]
async fn tool_arguments_never_supply_a_database() {
    let app = app();
    let smuggled = mcp(
        &app,
        1,
        "tools/call",
        json!({"name": "list_revisions", "arguments": {"database": "/tmp/elsewhere.sqlite"}}),
    )
    .await;
    assert_eq!(smuggled["result"]["isError"], true);
    assert_eq!(
        smuggled["result"]["structuredContent"]["error"]["code"],
        "INVALID_INPUT"
    );
    let direct = send(
        &app,
        post(
            "/v1/calc/list_revisions",
            Some(SECRET),
            r#"{"database":"/tmp/elsewhere.sqlite"}"#,
        ),
    )
    .await;
    assert_eq!(direct.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(direct.json()["error"]["code"], "INVALID_INPUT");
}

#[tokio::test]
async fn calc_routes() {
    let app = app();
    let nope = send(&app, post("/v1/calc/nope", Some(SECRET), "{}")).await;
    assert_eq!(nope.status, StatusCode::NOT_FOUND);
    assert_eq!(nope.json()["error"]["code"], "UNKNOWN_FUNCTION");

    let stateful = send(
        &app,
        post(
            "/v1/calc/review_period",
            Some(SECRET),
            r#"{"revision_id":"r1"}"#,
        ),
    )
    .await;
    assert_eq!(stateful.status, StatusCode::UNPROCESSABLE_ENTITY);
    let envelope = stateful.json();
    assert_eq!(envelope["status"], "refused");
    assert_eq!(envelope["function"], "review_period");
    assert_eq!(envelope["error"]["code"], "DATABASE_REQUIRED");

    // An empty body is {} and hashes as the canonical {}.
    let described = send(&app, post("/v1/calc/describe", Some(SECRET), "")).await;
    assert_eq!(described.status, StatusCode::OK);
    let described = described.json();
    assert_eq!(described["status"], "calculated");
    assert_eq!(
        described["input_sha256"],
        boxscore::exact::digest(b"{}").as_str()
    );

    // input_sha256 covers the body bytes as received.
    let body = b"{ \"currency\": \"USD\", \"expense_convention\": \"positive_costs\", \"actuals\": [], \"budgets\": [] }";
    let hashed = send(
        &app,
        post("/v1/calc/noi_variance", Some(SECRET), body.to_vec()),
    )
    .await;
    assert_eq!(
        hashed.json()["input_sha256"],
        boxscore::exact::digest(body).as_str()
    );

    let pinned = send(
        &app,
        post("/v1/calc/describe?version=9.9.9", Some(SECRET), ""),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(pinned.json()["error"]["code"], "VERSION_MISMATCH");

    let bad_query = send(&app, post("/v1/calc/describe?path=/etc", Some(SECRET), "")).await;
    assert_eq!(bad_query.status, StatusCode::BAD_REQUEST);

    let not_json = send(&app, post("/v1/calc/describe", Some(SECRET), "{nope")).await;
    assert_eq!(not_json.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn database_round_trip_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(config(Some(db)));
    let dataset = json!({
        "property": "Synthetic Oak", "period": "2026-05",
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [line("4000", "rental income", "100.10"), line("5000", "payroll", "30.00")],
        "budgets": [line("4000", "rental income", "90.05"), line("5000", "payroll", "25.00")],
        "snapshot": null
    });
    let imported = send(
        &app,
        post(
            "/v1/calc/import_period",
            Some(SECRET),
            serde_json::to_vec(&json!({"dataset": dataset})).unwrap(),
        ),
    )
    .await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.json());
    let revision = imported.json()["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reviewed = mcp(
        &app,
        2,
        "tools/call",
        json!({"name": "review_period", "arguments": {"revision_id": revision}}),
    )
    .await;
    let envelope = &reviewed["result"]["structuredContent"];
    assert_eq!(reviewed["result"]["isError"], false, "{envelope}");
    assert_eq!(
        envelope["result"]["variance"]["noi_bridge"]["noi_variance"],
        "5.05"
    );
    assert_eq!(envelope["result"]["human_approval"], "not_granted");
}

#[tokio::test]
async fn rate_limit_per_key() {
    let app = app();
    let mut statuses = Vec::new();
    for id in 0..70 {
        statuses.push(
            send(&app, post("/mcp", Some(SECRET), rpc(id, "ping", json!({}))))
                .await
                .status,
        );
    }
    assert!(statuses[..60].iter().all(|s| *s == StatusCode::OK));
    assert!(statuses[60..]
        .iter()
        .all(|s| *s == StatusCode::TOO_MANY_REQUESTS));
    let limited = send(&app, post("/mcp", Some(SECRET), rpc(71, "ping", json!({})))).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = limited.headers["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&retry));
    // Another key has its own bucket; public routes are not limited.
    let other = send(
        &app,
        post("/mcp", Some(OTHER_SECRET), rpc(1, "ping", json!({}))),
    )
    .await;
    assert_eq!(other.status, StatusCode::OK);
    let health = send(&app, Request::get("/health").body(Body::empty()).unwrap()).await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn openapi_document_is_the_catalog() {
    let app = app();
    let reply = send(
        &app,
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await;
    let doc = reply.json();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["servers"][0]["url"], PUBLIC_URL);
    let paths = doc["paths"].as_object().unwrap();
    for calc in catalog::CATALOG {
        let path = &paths[&format!("/v1/calc/{}", calc.name)]["post"];
        assert_eq!(path["operationId"], calc.name);
        assert_eq!(
            path["requestBody"]["content"]["application/json"]["schema"],
            calc.input_schema(),
            "{}",
            calc.name
        );
        assert_eq!(
            path["responses"]["200"]["content"]["application/json"]["schema"]["allOf"][1]
                ["properties"]["result"],
            calc.output_schema()
        );
        assert!(path["responses"]["422"].is_object());
        assert!(path["responses"]["401"].is_object());
    }
    assert_eq!(
        paths.keys().filter(|k| k.starts_with("/v1/calc/")).count(),
        catalog::CATALOG.len()
    );
    assert!(paths["/mcp"]["post"].is_object());
    assert_eq!(paths["/health"]["get"]["security"], json!([]));
    assert_eq!(
        doc["components"]["securitySchemes"]["bearer"]["scheme"],
        "bearer"
    );
    // Every $ref resolves within the document.
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for reference in refs {
        let pointer = reference.strip_prefix('#').unwrap();
        assert!(doc.pointer(pointer).is_some(), "{reference}");
    }
    // An API-key-only server serves the configuration-independent document,
    // apart from servers.
    assert_eq!(mcp_http::openapi(PUBLIC_URL), doc);
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("$ref", Value::String(r)) => out.push(r.clone()),
                    _ => collect_refs(child, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_refs(i, out)),
        _ => {}
    }
}

#[tokio::test]
async fn start_up_refusals() {
    let dir = tempfile::tempdir().unwrap();

    // Non-loopback without a key file.
    let exposed = ServeOptions {
        bind: "0.0.0.0:3819".into(),
        ..ServeOptions::default()
    };
    let error = mcp_http::prepare(&exposed).unwrap_err().to_string();
    assert!(error.contains("without authentication"), "{error}");
    let output = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["serve", "--bind", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("without authentication"));

    // Loopback without keys is allowed (IPv4 and IPv6).
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let (_, config) = mcp_http::prepare(&ServeOptions {
            bind: bind.into(),
            ..ServeOptions::default()
        })
        .unwrap();
        assert!(config.validator.is_none());
    }
    // ... but not when the public URL says it is reachable from elsewhere
    // (a same-host reverse proxy).
    let proxied = mcp_http::prepare(&ServeOptions {
        public_url: Some("https://boxscore.example.test".into()),
        ..ServeOptions::default()
    })
    .unwrap_err()
    .to_string();
    assert!(proxied.contains("without authentication"), "{proxied}");
    for local in ["http://localhost:3819", "http://[::1]:3819"] {
        assert!(mcp_http::prepare(&ServeOptions {
            public_url: Some(local.into()),
            ..ServeOptions::default()
        })
        .is_ok());
    }
    let (_, default) = mcp_http::prepare(&ServeOptions::default()).unwrap();
    assert_eq!(default.public_url, "http://127.0.0.1:3819");

    // Key file with a trailing newline, mode 0600: accepted, even non-loopback.
    let keys = dir.path().join("keys");
    std::fs::write(&keys, format!("ci:{SECRET}\n")).unwrap();
    set_mode(&keys, 0o600);
    let (addr, config) = mcp_http::prepare(&ServeOptions {
        bind: "0.0.0.0:3819".into(),
        api_key_file: Some(keys.clone()),
        public_url: Some("https://boxscore.example.test/".into()),
        ..ServeOptions::default()
    })
    .unwrap();
    assert!(!addr.ip().is_loopback());
    assert_eq!(config.public_url, PUBLIC_URL);
    let validator = config.validator.unwrap();
    assert_eq!(validator.validate(SECRET).await.unwrap().subject, "ci");
    assert_eq!(validator.validate(SECRET).await.unwrap().method, "api_key");

    // World-readable key file refuses to start.
    #[cfg(unix)]
    {
        set_mode(&keys, 0o644);
        let error = mcp_http::prepare(&ServeOptions {
            api_key_file: Some(keys.clone()),
            ..ServeOptions::default()
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("other users"), "{error}");
        assert!(!error.contains(SECRET));
        set_mode(&keys, 0o600);
    }

    for bad in [
        ServeOptions {
            bind: "localhost:3819".into(),
            ..ServeOptions::default()
        },
        ServeOptions {
            rate_limit: 0,
            ..ServeOptions::default()
        },
        ServeOptions {
            allow_origins: vec!["https://claude.ai/path".into()],
            ..ServeOptions::default()
        },
        ServeOptions {
            public_url: Some("ftp://example.test".into()),
            ..ServeOptions::default()
        },
        ServeOptions {
            authorization_servers: vec!["not a url".into()],
            ..ServeOptions::default()
        },
        ServeOptions {
            api_key_file: Some(dir.path().join("missing")),
            ..ServeOptions::default()
        },
    ] {
        assert!(mcp_http::prepare(&bad).is_err(), "{bad:?}");
    }
}

fn set_mode(path: &std::path::Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[cfg(unix)]
#[tokio::test]
async fn real_socket_serves_and_stops_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys");
    std::fs::write(&keys, format!("ci:{SECRET}\n")).unwrap();
    set_mode(&keys, 0o600);
    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--api-key-file",
            keys.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    let base = banner
        .split_whitespace()
        .find(|w| w.starts_with("http://127.0.0.1:"))
        .unwrap_or_else(|| panic!("no address in {banner:?}"))
        .to_string();

    let client = reqwest::Client::new();
    let health: Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");

    let unauthenticated = client
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status().as_u16(), 401);
    let challenge = unauthenticated.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        challenge,
        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"")
    );

    let init: Value = client
        .post(format!("{base}/mcp"))
        .bearer_auth(SECRET)
        .header("accept", ACCEPT)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "socket-test", "version": "0"}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "boxscore-exact");

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("server did not stop on SIGTERM");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(status.code(), Some(0), "graceful shutdown exits 0");
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stderr, &mut rest).unwrap();
    assert!(rest.contains("stopped"), "{rest}");
    // Nothing secret reaches the log.
    assert!(!banner.contains(SECRET) && !rest.contains(SECRET));
}
