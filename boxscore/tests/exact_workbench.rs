//! The browser workbench `serve` offers at `/`: the embedded page and its
//! headers, the implicit same-origin allowance its API calls rely on, the
//! source rules the page keeps (DOM only, no external URL), and that hostile
//! account names come back from the engine as plain JSON strings. The page
//! itself cannot run in cargo tests. Synthetic data only.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use boxscore::{
    exact::{
        mcp_http::{self, Config, ServeOptions, StaticKeys},
        workbench,
    },
    ontology::{account_class, AccountClass},
};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command, sync::Arc};
use tower::ServiceExt;

const SECRET: &str = "synthetic-test-secret-0123456789abcdef";
const PUBLIC_URL: &str = "https://boxscore.example.test";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const HOSTILE_NAME: &str = "<img src=x onerror=alert(1)>";
// Markup only: a control character in an account name is refused at import
// (see `hostile_account_names_come_back_as_plain_json_strings`).
const HOSTILE_SCRIPT: &str = "</td><script>alert(\"x\")</script>";

fn keyed_config(database: Option<PathBuf>) -> Config {
    Config {
        database,
        validator: Some(Arc::new(
            StaticKeys::parse(&format!("ci:{SECRET}\n")).unwrap(),
        )),
        allowed_origins: Vec::new(),
        public_url: PUBLIC_URL.into(),
        authorization_servers: Vec::new(),
        rate_limit_per_minute: 600,
        scope: mcp_http::SCOPE.into(),
        oauth_enabled: false,
        loopback_port: None,
        api_keys: true,
        browser_oauth: None,
    }
}

/// A loopback server without a validator, as `serve` builds it for
/// `--bind 127.0.0.1:<port>`.
fn loopback_config(port: u16) -> Config {
    Config {
        validator: None,
        public_url: format!("http://127.0.0.1:{port}"),
        loopback_port: Some(port),
        ..keyed_config(None)
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("no {name} header"))
            .to_str()
            .unwrap()
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

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn calc(function: &str, token: Option<&str>, origin: Option<&str>, input: &Value) -> Request<Body> {
    let mut builder = Request::post(format!("/v1/calc/{function}"))
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    builder
        .body(Body::from(serde_json::to_vec(input).unwrap()))
        .unwrap()
}

fn assert_page_headers(reply: &Reply, content_type: &str) {
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), content_type);
    assert_eq!(reply.header("content-security-policy"), CSP);
    assert_eq!(reply.header("x-content-type-options"), "nosniff");
    assert_eq!(reply.header("referrer-policy"), "no-referrer");
    assert_eq!(reply.header("cache-control"), "no-store");
}

#[tokio::test]
async fn page_and_assets_are_served_with_strict_headers() {
    assert_eq!(workbench::CONTENT_SECURITY_POLICY, CSP);
    let app = mcp_http::router(keyed_config(None));

    let page = send(&app, get("/")).await;
    assert_page_headers(&page, "text/html; charset=utf-8");
    let html = String::from_utf8(page.body.clone()).unwrap();
    assert_eq!(html, workbench::INDEX_HTML);
    assert!(html.contains(r#"<script src="app.js" defer></script>"#));
    assert!(html.contains(r#"<link rel="stylesheet" href="app.css">"#));

    let css = send(&app, get("/app.css")).await;
    assert_page_headers(&css, "text/css; charset=utf-8");
    assert_eq!(css.body, workbench::APP_CSS.as_bytes());

    let js = send(&app, get("/app.js")).await;
    assert_page_headers(&js, "text/javascript; charset=utf-8");
    assert_eq!(js.body, workbench::APP_JS.as_bytes());

    // Everything else still falls through to the JSON 404.
    for path in ["/nope", "/index.html", "/app.js/x", "/workbench/app.js"] {
        let missing = send(&app, get(path)).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(missing.json()["error"]["code"], "NOT_FOUND", "{path}");
    }
    // The page routes answer GET only.
    let posted = send(&app, Request::post("/").body(Body::empty()).unwrap()).await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);

    // Unchanged: /health is public JSON.
    let health = send(&app, get("/health")).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json()["status"], "ok");
    assert!(health.headers.get("content-security-policy").is_none());
}

#[tokio::test]
async fn the_page_needs_no_bearer_and_the_api_still_does() {
    let app = mcp_http::router(keyed_config(None));
    for path in ["/", "/app.css", "/app.js"] {
        assert_eq!(send(&app, get(path)).await.status, StatusCode::OK, "{path}");
    }
    let anonymous = send(&app, calc("describe", None, None, &json!({}))).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.json()["error"]["code"], "UNAUTHORIZED");
    let keyed = send(&app, calc("describe", Some(SECRET), None, &json!({}))).await;
    assert_eq!(keyed.status, StatusCode::OK);
    assert_eq!(keyed.json()["status"], "calculated");
}

#[tokio::test]
async fn api_responses_are_never_cached_or_sniffed() {
    let app = mcp_http::router(keyed_config(None));
    let keyed = send(&app, calc("describe", Some(SECRET), None, &json!({}))).await;
    assert_eq!(keyed.status, StatusCode::OK);
    assert!(keyed.header("content-type").starts_with("application/json"));
    assert_eq!(keyed.header("cache-control"), "no-store");
    assert_eq!(keyed.header("x-content-type-options"), "nosniff");
    // The refusal before the handler carries them too.
    let anonymous = send(&app, calc("describe", None, None, &json!({}))).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.header("cache-control"), "no-store");
    assert_eq!(anonymous.header("x-content-type-options"), "nosniff");
    assert!(anonymous.headers.get("www-authenticate").is_some());
}

#[tokio::test]
async fn the_page_is_not_part_of_the_api_contract() {
    let app = mcp_http::router(keyed_config(None));
    let document = send(&app, get("/openapi.json")).await.json();
    let paths = document["paths"].as_object().unwrap();
    for path in ["/", "/app.css", "/app.js"] {
        assert!(!paths.contains_key(path), "{path} is in /openapi.json");
    }
    assert_eq!(document, mcp_http::openapi(PUBLIC_URL));
}

#[tokio::test]
async fn the_servers_own_origin_is_allowed_implicitly() {
    // Public URL origin, with no --allow-origin at all.
    let app = mcp_http::router(keyed_config(None));
    for (origin, expected) in [
        (PUBLIC_URL, StatusCode::OK),
        ("HTTPS://BOXSCORE.EXAMPLE.TEST:443", StatusCode::OK),
        ("https://evil.example", StatusCode::FORBIDDEN),
        ("http://boxscore.example.test", StatusCode::FORBIDDEN),
        ("https://boxscore.example.test:8443", StatusCode::FORBIDDEN),
        (
            "https://boxscore.example.test.evil.example",
            StatusCode::FORBIDDEN,
        ),
        ("http://127.0.0.1:3819", StatusCode::FORBIDDEN),
        ("null", StatusCode::FORBIDDEN),
    ] {
        let reply = send(
            &app,
            calc("describe", Some(SECRET), Some(origin), &json!({})),
        )
        .await;
        assert_eq!(reply.status, expected, "{origin}");
        if expected == StatusCode::FORBIDDEN {
            assert_eq!(reply.json()["error"]["code"], "ORIGIN_NOT_ALLOWED");
        }
    }

    // A public URL behind a path prefix still has its origin.
    let prefixed = mcp_http::router(Config {
        public_url: format!("{PUBLIC_URL}/boxscore"),
        ..keyed_config(None)
    });
    let reply = send(
        &prefixed,
        calc("describe", Some(SECRET), Some(PUBLIC_URL), &json!({})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);

    // --allow-origin keeps working alongside.
    let allowed = mcp_http::router(Config {
        allowed_origins: vec!["https://claude.ai".into()],
        ..keyed_config(None)
    });
    for origin in ["https://claude.ai", PUBLIC_URL] {
        let reply = send(
            &allowed,
            calc("describe", Some(SECRET), Some(origin), &json!({})),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{origin}");
    }

    // A loopback bind: the three loopback spellings of the bound port.
    let local = mcp_http::router(loopback_config(3819));
    for (origin, expected) in [
        ("http://127.0.0.1:3819", StatusCode::OK),
        ("http://localhost:3819", StatusCode::OK),
        ("http://[::1]:3819", StatusCode::OK),
        ("http://127.0.0.1:3820", StatusCode::FORBIDDEN),
        ("https://127.0.0.1:3819", StatusCode::FORBIDDEN),
        ("http://localhost.evil.example:3819", StatusCode::FORBIDDEN),
        ("https://claude.ai", StatusCode::FORBIDDEN),
    ] {
        let reply = send(&local, calc("describe", None, Some(origin), &json!({}))).await;
        assert_eq!(reply.status, expected, "{origin}");
    }

    // A loopback bind behind a proxy: the public origin and the bound port.
    let proxied = mcp_http::Config {
        public_url: PUBLIC_URL.into(),
        loopback_port: Some(4000),
        ..keyed_config(None)
    };
    let origins = mcp_http::same_origins(&proxied);
    assert_eq!(
        origins,
        [
            PUBLIC_URL,
            "http://127.0.0.1:4000",
            "http://localhost:4000",
            "http://[::1]:4000"
        ]
    );
    // An unbound port 0 adds nothing.
    let unbound = Config {
        loopback_port: Some(0),
        ..keyed_config(None)
    };
    assert_eq!(mcp_http::same_origins(&unbound), [PUBLIC_URL]);
}

/// The same-origin entries are stored in the canonical form the request side
/// compares: the public URL's scheme for its loopback spellings, and no
/// default port.
#[tokio::test]
async fn loopback_public_urls_keep_their_scheme_and_default_port() {
    async fn statuses(public_url: &str, origins: &[&str]) -> Vec<StatusCode> {
        // A non-loopback bind, so only the public URL contributes.
        let app = mcp_http::router(Config {
            public_url: public_url.into(),
            ..keyed_config(None)
        });
        let mut found = Vec::new();
        for origin in origins {
            let reply = send(
                &app,
                calc("describe", Some(SECRET), Some(origin), &json!({})),
            )
            .await;
            found.push(reply.status);
        }
        found
    }
    let (ok, forbidden) = (StatusCode::OK, StatusCode::FORBIDDEN);

    // `--public-url http://localhost`: port 80, which a browser omits.
    assert_eq!(
        statuses(
            "http://localhost",
            &[
                "http://localhost",
                "http://127.0.0.1",
                "http://[::1]",
                "http://127.0.0.1:80",
                "https://127.0.0.1",
                "http://127.0.0.1:8080",
            ]
        )
        .await,
        [ok, ok, ok, ok, forbidden, forbidden]
    );
    assert_eq!(
        mcp_http::same_origins(&Config {
            public_url: "http://localhost".into(),
            ..keyed_config(None)
        }),
        ["http://localhost", "http://127.0.0.1", "http://[::1]"]
    );

    // A local TLS proxy: the loopback spellings are https on its port.
    assert_eq!(
        statuses(
            "https://localhost:8443",
            &[
                "https://localhost:8443",
                "https://127.0.0.1:8443",
                "https://[::1]:8443",
                "http://127.0.0.1:8443",
                "http://localhost:8443",
            ]
        )
        .await,
        [ok, ok, ok, forbidden, forbidden]
    );

    // `https://localhost` drops 443 the same way.
    assert_eq!(
        mcp_http::same_origins(&Config {
            public_url: "https://localhost".into(),
            ..keyed_config(None)
        }),
        ["https://localhost", "https://127.0.0.1", "https://[::1]"]
    );

    // Through prepare, as the command line sets it: the public URL's https
    // spellings, and the plain-http spellings of the loopback listener.
    let (_, config) = mcp_http::prepare(&ServeOptions {
        public_url: Some("https://localhost:8443".into()),
        ..ServeOptions::default()
    })
    .unwrap();
    assert_eq!(
        mcp_http::same_origins(&config),
        [
            "https://localhost:8443",
            "https://127.0.0.1:8443",
            "https://[::1]:8443",
            "http://127.0.0.1:3819",
            "http://localhost:3819",
            "http://[::1]:3819"
        ]
    );
}

#[tokio::test]
async fn prepare_records_a_loopback_bind() {
    let (_, config) = mcp_http::prepare(&ServeOptions::default()).unwrap();
    assert_eq!(config.loopback_port, Some(3819));
    assert!(mcp_http::same_origins(&config).contains(&"http://localhost:3819".to_string()));

    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys");
    std::fs::write(&keys, format!("ci:{SECRET}\n")).unwrap();
    set_private(&keys);
    let (_, exposed) = mcp_http::prepare(&ServeOptions {
        bind: "0.0.0.0:3819".into(),
        api_key_file: Some(keys),
        ..ServeOptions::default()
    })
    .unwrap();
    assert_eq!(exposed.loopback_port, None);
    // The default public URL is still the server's own origin.
    assert_eq!(
        mcp_http::same_origins(&exposed),
        [
            "http://127.0.0.1:3819",
            "http://localhost:3819",
            "http://[::1]:3819"
        ]
    );
}

fn set_private(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn source(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/exact/workbench")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every `on<letters>` followed by optional spaces and `=` (not `==`) right
/// after a character in `before`: an inline handler attribute in HTML
/// (`<a onclick=`) or a handler property in script (`node.onclick =`).
fn handler_assignments(text: &str, before: &[u8]) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for i in 1..bytes.len().saturating_sub(1) {
        if !before.contains(&bytes[i - 1])
            || !bytes[i].eq_ignore_ascii_case(&b'o')
            || !bytes[i + 1].eq_ignore_ascii_case(&b'n')
        {
            continue;
        }
        let mut j = i + 2;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j == i + 2 {
            continue;
        }
        let mut k = j;
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if bytes.get(k) == Some(&b'=') && bytes.get(k + 1) != Some(&b'=') {
            found.push(String::from_utf8_lossy(&bytes[i - 1..=k]).into_owned());
        }
    }
    found
}

#[test]
fn page_sources_build_the_dom_without_markup_or_external_urls() {
    let html = source("index.html");
    let js = source("app.js");
    let css = source("app.css");
    assert_eq!(html, workbench::INDEX_HTML);
    assert_eq!(js, workbench::APP_JS);
    assert_eq!(css, workbench::APP_CSS);

    for (name, text) in [("index.html", &html), ("app.js", &js), ("app.css", &css)] {
        // The two `new URL(` (the authorize request and the redirect URI's
        // origin, pinned by page_sign_in_follows_the_pkce_rules) are not a
        // CSS `url(`.
        let lower = text
            .to_ascii_lowercase()
            .replace("new url(", "new-url-constructor");
        for banned in [
            "innerhtml",
            "outerhtml",
            "insertadjacenthtml",
            "eval(",
            "new function",
            "document.write",
            "http://",
            "https://",
            "javascript:",
            "@import",
            "url(",
        ] {
            assert!(!lower.contains(banned), "{name} contains {banned}");
        }
    }

    // index.html: one script tag, the external one; no inline style or handler.
    let lower = html.to_ascii_lowercase();
    assert_eq!(lower.matches("<script").count(), 1);
    assert!(html.contains(r#"<script src="app.js" defer></script>"#));

    // Every URL is relative, so a server behind a path prefix
    // (`--public-url https://host/boxscore`) serves and calls itself there.
    for root_absolute in ["href=\"/", "src=\"/", "href='/", "src='/"] {
        assert!(
            !lower.contains(root_absolute),
            "index.html contains {root_absolute}"
        );
    }
    for root_absolute in ["\"/v1/", "'/v1/", "\"/.well-known", "'/.well-known"] {
        assert!(
            !js.contains(root_absolute),
            "app.js contains {root_absolute}"
        );
    }
    assert!(js.contains(r#"request("POST", "v1/calc/" + name, input, timeout)"#));
    assert!(js.contains(r#"request("GET", ".well-known/oauth-protected-resource")"#));
    assert!(js.contains(r#"request("GET", "workbench/auth.json")"#));

    // A pasted key is checked before it is stored: printable ASCII only.
    assert!(js.contains(r"var KEY_CHARS = /^[\x21-\x7e]+$/;"));
    assert!(js.contains("The key contains characters an API key cannot have"));
    assert!(!lower.contains("<style"));
    assert!(!lower.contains(" style="));
    assert_eq!(
        handler_assignments(&html, b" \t\r\n\"'/"),
        Vec::<String>::new()
    );

    // app.js: handlers only through addEventListener; no markup sink, no
    // string-evaluating timer.
    assert_eq!(handler_assignments(&js, b"."), Vec::<String>::new());
    for banned in [
        "setattribute(\"on",
        "setattribute('on",
        "settimeout(\"",
        "settimeout('",
        "setinterval(\"",
        "setinterval('",
        "parsefloat",
        "number(",
        "domparser",
        "createcontextualfragment",
        "srcdoc",
        "localstorage",
        "document.cookie",
    ] {
        assert!(
            !js.to_ascii_lowercase().contains(banned),
            "app.js contains {banned}"
        );
    }
    assert!(js.contains("sessionStorage"));

    // The scanner itself finds what it should.
    assert_eq!(
        handler_assignments(r#"<img src=x onerror=alert(1)><a ONCLICK = "x">"#, b" "),
        [" onerror=", " ONCLICK ="]
    );
    assert_eq!(
        handler_assignments("node.onclick = f; a.one == b; x.on = 1", b"."),
        [".onclick ="]
    );

    // The import screen reads each chosen file's bytes with
    // FileReader.readAsArrayBuffer through listeners (a handler property
    // would fail the scan above) and decodes them as strict UTF-8 that keeps
    // a leading BOM, so the text sent is the file's bytes exactly; a file
    // that is not UTF-8 is refused before anything is sent. The text is
    // never trimmed, parsed or kept. The page asks for no path, object URL or
    // other storage.
    assert_eq!(js.matches(".readAsText(").count(), 0);
    assert_eq!(js.matches(".readAsArrayBuffer(").count(), 1);
    assert!(js.contains("reader.readAsArrayBuffer(file);"));
    assert_eq!(js.matches("new TextDecoder(").count(), 1);
    assert!(js.contains(
        r#"resolve(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(reader.result));"#
    ));
    assert!(js.contains(r#"reject({ notUtf8: file.name });"#));
    assert!(js.contains(r#"e.notUtf8 + " is not UTF-8 text. Nothing was sent.""#));
    assert!(js.contains("files[p.name] = texts[i];"));
    assert!(js.contains("var payload = { profile: c.profile, files: files, options: c.options };"));
    // The body is measured as the server will see it, escaping included,
    // and refused over the cap before it is sent; the file sizes are the
    // first gate, before any file is read.
    assert!(js.contains("var size = new TextEncoder().encode(JSON.stringify(payload)).length;"));
    assert!(js.contains("if (size > MAX_FILE_BYTES) {"));
    assert!(js.contains("if (bytes > MAX_FILE_BYTES) {"));
    // An option with surrounding spaces is refused, not trimmed.
    assert!(js.contains(
        r#"return { problem: "Remove leading or trailing spaces from " + padded.join(", ") + "." };"#
    ));
    // Imports wait longer, and a timed-out one says it may have landed.
    assert!(js.contains("var IMPORT_TIMEOUT_MS = 60000;"));
    assert!(js.contains(r#"return calc("import_period", payload, IMPORT_TIMEOUT_MS);"#));
    assert!(js.contains("The server may already have stored the period. Reload the list"));
    assert!(js.contains(r#"calc("get_provenance", { trace_id: "#));
    assert!(js.contains("var MAX_FILE_BYTES = 2097152;"));
    // The trace view: caller-supplied provenance apart and labelled, the
    // files table and the raw JSON bounded.
    assert!(js.contains(r#""Supplied by the caller (not verified by the server)""#));
    assert!(js.contains("var MAX_FILE_ROWS = 50;"));
    assert!(js.contains("files.slice(0, MAX_FILE_ROWS)"));
    assert!(js.contains("var MAX_RAW_CHARS = 65536;"));
    assert!(js.contains("raw.slice(0, MAX_RAW_CHARS)"));
    assert!(js.contains("importSeq") && js.contains("traceSeq") && js.contains("issueSeq"));
    for banned in [
        "webkitrelativepath",
        "createobjecturl",
        "showopenfilepicker",
        "indexeddb",
        "formdata",
        "xmlhttprequest",
        "sendbeacon",
        "websocket",
    ] {
        assert!(
            !js.to_ascii_lowercase().contains(banned),
            "app.js contains {banned}"
        );
    }
    // The only thing done to a file input's selection is emptying it.
    assert!(js.contains(r#"f.input.value = "";"#));
    // Every file's text enters the payload in sendImport and nowhere else.
    assert_eq!(js.matches("texts").count(), 2);
    // The form is built from describe's adapter list, not from a fixed one.
    assert!(js.contains("result.adapters"));
    assert!(!js.contains("canonical.csv"));

    // Issuing a report from the review screen. The only request is
    // `issue_report` with exactly the revision id the confirmation showed,
    // sent from the confirm button's handler and nowhere else; the old
    // "not available here yet" wording is gone.
    assert_eq!(js.matches(r#"calc("issue_report""#).count(), 1);
    assert!(js.contains(r#"return calc("issue_report", { revision_id: ctx.id }).then("#));
    assert_eq!(
        js.matches("sendIssue(").count(),
        1,
        "defined, never called directly"
    );
    assert!(js.contains(r#"$("issue-confirm-yes").addEventListener("click", sendIssue);"#));
    assert!(js.contains(r#"$("issue-open").addEventListener("click", openIssueConfirm);"#));
    assert!(js.contains(r#"$("issue-confirm-no").addEventListener("click", cancelIssue);"#));
    assert!(js.contains("var ctx = state.issueConfirm;"));
    // Cancel sends nothing; the review's own refusal turns the button off and
    // review_required is left to the engine (no other status is tested).
    assert!(js.contains(r#"setStatus("Issue cancelled. Nothing was sent.");"#));
    assert!(js.contains(r#"$("issue-open").disabled = refused;"#));
    assert!(js.contains(r#"var refused = envelope.status === "refused";"#));
    assert!(!js.contains(r#"=== "review_required""#));
    // A stale response is dropped, the confirmation and button stay locked
    // while a request is in flight, and a failure is never a success.
    assert!(js.contains("if (seq === state.issueSeq) return finishIssue(seq, ctx.id, res);"));
    assert!(js.contains(r#"$("issue-lock").disabled = locked;"#));
    assert!(js.contains("|| state.issuing || state.issueConfirm ||"));
    assert_eq!(js.matches(r#"h3("Report issued")"#).count(), 1);
    assert!(js.contains(
        r#""issue_report was refused: " + text(error.code) + ": " + text(error.message)"#
    ));
    assert!(js.contains("The answer carried no report id"));
    // What is shown before and after: the empty state, the docs' wording
    // that an issued report is immutable and not sent anywhere, and the
    // citation through appendProvenance with its own element id.
    assert!(js.contains(r#""No report issued for this revision""#));
    assert!(js.contains("An issued report is an immutable record"));
    assert!(js.contains("and is not sent anywhere. Issuing is a record, not sign-off."));
    assert!(js.contains(r#"appendProvenance(out, body, { id: "issue-citation", heading: "Citation", lookup: true });"#));
    assert!(js.contains("hashCell(r.body_sha256, false)"));
    assert!(!js.contains("Issuing reports is not available"));
    assert!(!html.contains("Issuing reports is not available"));

    // The issue flow's warnings: a superseded revision is named from the
    // loaded list only (the confirmation and the Issued reports block), and
    // every failure but a refusal before the server acted may have issued
    // the report.
    assert!(js.contains(
        r#""This revision has been superseded by " + ids.join(", ") + " in the loaded list. The engine will refuse to issue a report for it (SUPERSEDED); issue the latest revision instead.""#
    ));
    assert!(js.contains("state.known[k].parent_id === id"));
    assert_eq!(js.matches("supersededNote(later)").count(), 2);
    assert!(js.contains(
        r#""The server may already have issued the report: reload the list before trying again.""#
    ));
    assert!(
        js.contains("if ([401, 403, 413, 429].indexOf(res.status) < 0) lines.push(MAYBE_ISSUED);")
    );
    assert!(js.contains(r#"failIssue(["The request ended unexpectedly.", MAYBE_ISSUED])"#));
    // After success, a failed reload leaves the success panel and adds a note.
    assert!(js.contains(r#""The list could not be reloaded: press Reload.""#));
    assert!(js.contains("Promise.resolve().then(loadRevisions).then(function (applied) {"));
    assert!(js.contains(r#""(list reload superseded; press Reload)""#));
    assert!(js.contains("state.queued = { id: id, note: loaded };"));

    // Under 88 KB together. Raised from 64 KB for slice 3's issue flow (the
    // superseded warning, the failure caveat and the queued selection), then
    // from 72 KB for sign-in through the identity provider (PKCE, the code
    // exchange, expiry and sign-out).
    assert!(html.len() + js.len() + css.len() < 88 * 1024);
}

/// Sign-in through the identity provider: what the page sends, keeps and
/// navigates to. The flow itself runs against the mock IdP in
/// exact_mcp_http_oauth.rs and against Keycloak by hand (docs).
#[test]
fn page_sign_in_follows_the_pkce_rules() {
    let js = source("app.js");
    let lower = js.to_ascii_lowercase();

    // One navigation, to a URL whose base is auth.json's
    // authorization_endpoint; it is the only URL the page builds from data.
    // The other `new URL(` only reads the redirect URI's origin, which is
    // compared, never navigated to or fetched.
    assert_eq!(js.matches("location.assign(").count(), 1);
    assert!(js.contains("      window.location.assign(url.toString());\n"));
    assert_eq!(js.matches("new URL(").count(), 2);
    assert!(js.contains("      var url = new URL(auth.authorization_endpoint);\n"));
    assert!(js.contains("      return new URL(raw).origin;\n"));
    assert_eq!(js.matches("originOf(").count(), 2);
    // Sign-in starts only at the redirect URI's origin (127.0.0.1 is not
    // localhost): the answer lands there, with this tab's state or not at all.
    assert!(js.contains("    if (originOf(auth.redirect_uri) !== window.location.origin) {\n"));
    assert!(js.contains(
        r#"showSignIn("Sign-in works only at " + text(auth.redirect_uri) + "; open the workbench there.");"#
    ));
    assert_eq!(js.matches("location.href").count(), 0);
    assert_eq!(js.matches("location.replace(").count(), 0);
    assert_eq!(js.matches("window.open(").count(), 0);
    // `auth` there is state.auth, which only the auth.json answer sets: no
    // envelope, result or other server body reaches the sign-in code.
    assert_eq!(js.matches("state.auth = ").count(), 1);
    assert!(js.contains(
        r#"if (!res.failure && res.status === 200 && res.body && typeof res.body.mode === "string") state.auth = res.body;"#
    ));
    let sign_in = section(&js, "  function signIn() {", "\n  }\n");
    assert!(sign_in.contains("var auth = state.auth;"));
    for banned in [
        "envelope", "result", "res.", "body", "token", "load(", "getKey",
    ] {
        assert!(!sign_in.contains(banned), "signIn uses {banned}");
    }
    // Exactly the authorize parameters (authorizeParams, which
    // scripts/workbench_unit_test.mjs also runs), set through
    // URLSearchParams.
    assert!(sign_in.contains(
        "authorizeParams(auth, nonce, encodeChallenge(hash)).forEach(function (p) { q.set(p[0], p[1]); });"
    ));
    assert_eq!(js.matches("q.set(").count(), 1);
    let params = section(
        &js,
        "  function authorizeParams(auth, nonce, challenge) {",
        "\n  }\n",
    );
    let pairs: Vec<&str> = params
        .lines()
        .filter(|l| l.trim_start().starts_with("[\""))
        .map(|l| l.trim().trim_end_matches(','))
        .collect();
    assert_eq!(
        pairs,
        [
            r#"["response_type", "code"]"#,
            r#"["client_id", auth.client_id]"#,
            r#"["redirect_uri", auth.redirect_uri]"#,
            r#"["scope", auth.scope]"#,
            r#"["state", nonce]"#,
            r#"["code_challenge", challenge]"#,
            r#"["code_challenge_method", "S256"]"#,
            r#"["resource", auth.audience]"#,
        ]
    );
    assert_eq!(js.matches("searchParams").count(), 1);
    assert!(!lower.contains("\"plain\""));

    // PKCE: a 64-character verifier from crypto.getRandomValues over 64
    // unreserved characters, S256 by crypto.subtle.digest, base64url.
    assert!(js.contains("var verifier = randomString(64);"));
    assert!(js.contains("window.crypto.getRandomValues(bytes);"));
    assert!(js.contains("out += UNRESERVED.charAt(bytes[i] & 63);"));
    assert!(js.contains(
        r#"var UNRESERVED = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";"#
    ));
    assert!(js
        .contains(r#"window.crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))"#));
    assert!(js.contains(r#".replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")"#));
    assert!(!lower.contains("math.random"));

    // The answer: any query with a code or an error leaves the address bar
    // and history before anything else (before the sign-in settings or the
    // state are looked at), the pending entry is dropped whatever happens,
    // and classifyCallback decides: no state, another tab's state, no
    // pending entry or an iss (RFC 9207) from another issuer is a mismatch.
    let classify = section(
        &js,
        "  function classifyCallback(params, pending, issuer, now) {",
        "\n  }\n",
    );
    assert!(classify.contains(r#"if (!params.has("state") || !pending || typeof pending.state !== "string" || pending.state !== params.get("state")) {"#));
    assert!(classify.contains(
        r#"if (params.has("iss") && issuerId(params.get("iss")) !== issuerId(issuer)) {"#
    ));
    assert_eq!(classify.matches("message: MISMATCH").count(), 2);
    assert!(js.contains(r#"return text(value).replace(/\/+$/, "");"#));
    assert!(
        js.contains(r#"var MISMATCH = "Sign-in response did not match this tab; start again.";"#)
    );
    assert!(js.contains("window.history.replaceState(null, \"\", window.location.pathname);"));
    let finish = section(&js, "  function finishSignIn() {", "\n  }\n");
    assert!(finish.contains(
        r#"    if (!params.has("code") && !params.has("error")) return Promise.resolve("");"#
    ));
    let cleaned = finish.find("history.replaceState").unwrap();
    for later in [
        "loadJson(PENDING_STORAGE)",
        "canSignIn()",
        "classifyCallback(",
        "exchange(",
    ] {
        assert!(cleaned < finish.find(later).unwrap(), "{later}");
    }
    assert!(
        finish.find(r#"save(PENDING_STORAGE, "");"#).unwrap() < finish.find("canSignIn()").unwrap()
    );
    // A callback while auth.json is unavailable says so.
    assert!(finish.contains("if (!canSignIn()) return Promise.resolve(UNAVAILABLE);"));
    assert!(js.contains(r#"var UNAVAILABLE = "Sign-in could not be completed because the server's sign-in settings could not be loaded; start again.";"#));

    // The exchange: a form POST to auth.json's token endpoint, no secret,
    // no cookies, the verifier in the body only.
    let exchange = section(&js, "  function exchange(code, verifier) {", "\n  }\n");
    assert!(exchange.contains("return window.fetch(auth.token_endpoint, {"));
    for field in [
        r#"form.set("grant_type", "authorization_code");"#,
        r#"form.set("code", code);"#,
        r#"form.set("redirect_uri", auth.redirect_uri);"#,
        r#"form.set("client_id", auth.client_id);"#,
        r#"form.set("code_verifier", verifier);"#,
    ] {
        assert!(exchange.contains(field), "{field}");
    }
    assert_eq!(exchange.matches("form.set(").count(), 5);
    assert!(exchange.contains(r#""Content-Type": "application/x-www-form-urlencoded""#));
    assert!(exchange.contains(r#"credentials: "omit","#));
    assert!(!lower.contains("client_secret"));
    assert!(!lower.contains("refresh_token"));
    assert_eq!(js.matches("code_verifier").count(), 1);
    // The token is kept in sessionStorage with its expiry, and sent only as
    // the bearer header (before the API key).
    assert!(js.contains(r#"var TOKEN_STORAGE = "boxscore.workbench.oauthToken";"#));
    assert!(js.contains(
        "var stored = { access_token: token, expires_at: typeof life === \"number\" && life > 0 ? now + life * 1000 : null };"
    ));
    assert!(js.contains("var record = tokenRecord(body, Date.now());"));
    assert!(js.contains(
        "    var api = path.indexOf(\"v1/\") === 0;\n    var held = api ? signInState() : {};\n"
    ));
    assert!(js.contains("var bearer = api ? token || getKey() : \"\";"));
    assert!(js.contains(r#"if (bearer) headers.Authorization = "Bearer " + bearer;"#));
    // Expiry mid-session: once this tab's sign-in has expired (or the server
    // refused its token), a request sends nothing, shows the sign-in panel
    // and resolves as a 401 to the token; the API key is used only when no
    // token was ever kept in this tab, or after Sign out.
    let request = section(
        &js,
        "  function request(method, path, payload, timeout) {",
        "\n  }\n",
    );
    let expired = request.find("    if (held.expired) {\n").unwrap();
    assert!(expired < request.find("window.fetch(").unwrap());
    assert!(expired < request.find("getKey()").unwrap());
    assert!(request.contains("      showSignIn(EXPIRED);\n"));
    assert!(request.contains(
        "      return Promise.resolve({ status: 401, body: null, retryAfter: null, oauth: true, notSent: true });\n"
    ));
    assert!(request.contains("    var token = held.token || \"\";\n"));
    assert!(js.contains(r#"if (res.notSent) return ["Nothing was sent. " + EXPIRED];"#));
    assert!(js.contains("    if (stored.expired === true) return \"expired\";\n"));
    assert!(js.contains(
        "    if (typeof stored.expires_at === \"number\" && now >= stored.expires_at) return \"expired\";\n"
    ));
    assert!(js.contains(r#"save(TOKEN_STORAGE, JSON.stringify({ expired: true }));"#));
    // Only Sign out removes the entry outright.
    assert_eq!(js.matches(r#"save(TOKEN_STORAGE, "");"#).count(), 1);
    let sign_out = section(&js, "  function signOut() {", "\n  }\n");
    assert!(sign_out.contains(r#"save(TOKEN_STORAGE, "");"#));
    assert!(js.contains(r#"$("signin-out").hidden = !t && !held.expired;"#));

    // Under node only, the pure decisions are exported for
    // scripts/workbench_unit_test.mjs and nothing is wired; a browser has no
    // module object and never takes that branch.
    assert_eq!(js.matches("module.exports").count(), 2);
    let guard = js
        .find("  if (typeof module !== \"undefined\" && module.exports && typeof document === \"undefined\") {\n    module.exports = {\n")
        .expect("the export block, guarded");
    let tail = &js[guard..];
    let returned = tail.find("    return;\n  }\n").unwrap();
    assert!(returned < tail.find("document.readyState").unwrap());
    assert!(tail.ends_with("  else wire();\n})();\n"));
    for helper in [
        "tokenState: tokenState",
        "tokenRecord: tokenRecord",
        "classifyCallback: classifyCallback",
        "authorizeParams: authorizeParams",
    ] {
        assert!(tail[..returned].contains(helper), "{helper}");
    }
    // No token, code or verifier near any URL, history entry or log.
    for line in js.lines() {
        let l = line.to_ascii_lowercase();
        if l.contains("token") || l.contains("verifier") || l.contains("\"code\"") {
            for sink in [
                "new url(",
                "location",
                "replacestate",
                "pushstate",
                "console.",
            ] {
                assert!(
                    !l.contains(sink) || l.trim_start().starts_with("//"),
                    "{line}"
                );
            }
        }
    }
    for banned in ["localstorage", "document.cookie", "console.", "pushstate"] {
        assert!(!lower.contains(banned), "app.js contains {banned}");
    }

    // Expiry, 401 and 403: the messages the page shows.
    assert!(js.contains(r#"var EXPIRED = "Your sign-in expired; sign in again.";"#));
    assert!(js.contains("if (res.status === 401 && res.oauth) {"));
    assert!(
        js.contains(r#"if (res.status === 403 && error && error.code === "INSUFFICIENT_SCOPE") {"#)
    );
    assert!(js.contains("did not grant \" + requiredScope() + \" to this sign-in"));
    assert!(js.contains("Your session at the identity provider is not ended"));
    // The IdP's own error and description, as text.
    assert!(js.contains(r#"idpError(params.get("error"), params.get("error_description"))"#));
    assert!(js.contains(r#"$("signin-message").textContent = message || "";"#));
    assert!(js.contains(r#"$("signin-start").addEventListener("click", signIn);"#));
    assert!(js.contains(r#"$("signin-out").addEventListener("click", signOut);"#));
}

/// The text of `js` from `start` to the first `end` after it.
fn section<'a>(js: &'a str, start: &str, end: &str) -> &'a str {
    let at = js.find(start).unwrap_or_else(|| panic!("no {start}"));
    let len = js[at..]
        .find(end)
        .unwrap_or_else(|| panic!("no end after {start}"));
    &js[at..at + len]
}

fn browser_oauth() -> mcp_http::BrowserOAuth {
    mcp_http::BrowserOAuth {
        issuer: "https://idp.example.test/realms/acme".into(),
        client_id: "boxscore-workbench".into(),
        authorization_endpoint: "https://idp.example.test/realms/acme/protocol/openid-connect/auth"
            .into(),
        token_endpoint: "https://IDP.example.test:8443/realms/acme/protocol/openid-connect/token"
            .into(),
        scope: "openid boxscore:calc".into(),
        audience: PUBLIC_URL.into(),
    }
}

#[tokio::test]
async fn auth_json_reports_the_mode_with_the_page_headers() {
    let oauth_only = Config {
        oauth_enabled: true,
        api_keys: false,
        ..keyed_config(None)
    };
    let both = Config {
        oauth_enabled: true,
        ..keyed_config(None)
    };
    for (config, mode) in [
        (loopback_config(3819), "none"),
        (keyed_config(None), "api_key"),
        (oauth_only.clone(), "oauth"),
        (both.clone(), "api_key_or_oauth"),
    ] {
        let reply = send(&mcp_http::router(config), get("/workbench/auth.json")).await;
        assert_page_headers(&reply, "application/json");
        assert_eq!(reply.json(), json!({ "mode": mode }));
    }

    // With browser sign-in: everything the page needs, identifiers only.
    for (config, mode) in [(oauth_only, "oauth"), (both, "api_key_or_oauth")] {
        let app = mcp_http::router(Config {
            browser_oauth: Some(browser_oauth()),
            ..config
        });
        let reply = send(&app, get("/workbench/auth.json")).await;
        assert_eq!(
            reply.json(),
            json!({
                "mode": mode,
                "issuer": "https://idp.example.test/realms/acme",
                "authorization_endpoint": "https://idp.example.test/realms/acme/protocol/openid-connect/auth",
                "token_endpoint": "https://IDP.example.test:8443/realms/acme/protocol/openid-connect/token",
                "client_id": "boxscore-workbench",
                "scope": "openid boxscore:calc",
                "audience": PUBLIC_URL,
                "redirect_uri": format!("{PUBLIC_URL}/"),
            })
        );
        assert!(!String::from_utf8_lossy(&reply.body).contains(SECRET));
    }
    // Public and GET only, like the page.
    let app = mcp_http::router(keyed_config(None));
    let posted = send(
        &app,
        Request::post("/workbench/auth.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(posted.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn connect_src_names_the_token_endpoint_url_only_with_browser_sign_in() {
    // The exact token endpoint (host lowercased, port kept), not its origin:
    // the rest of the identity provider's API stays out of the page's reach.
    let with = CSP.replace(
        "connect-src 'self'",
        "connect-src 'self' https://idp.example.test:8443/realms/acme/protocol/openid-connect/token",
    );
    assert_eq!(
        workbench::content_security_policy(Some(&browser_oauth())).unwrap(),
        with.as_str()
    );
    assert_eq!(workbench::content_security_policy(None).unwrap(), CSP);
    for (endpoint, source) in [
        (
            "https://idp.example.test:443/t",
            Some("https://idp.example.test/t"),
        ),
        (
            "http://127.0.0.1:8080/token",
            Some("http://127.0.0.1:8080/token"),
        ),
        ("http://[::1]:8080/token", Some("http://[::1]:8080/token")),
        (
            "https://idp-1.example.test/o/token",
            Some("https://idp-1.example.test/o/token"),
        ),
        // In the path, the characters that would end the directive, split
        // the list or open a keyword are percent-encoded.
        (
            "https://idp.example.test/a;b,c'd/token",
            Some("https://idp.example.test/a%3Bb%2Cc%27d/token"),
        ),
        // A WHATWG host may carry them: refused, never written into the header.
        ("https://idp;x.example.test/token", None),
        ("https://idp,x.example.test/token", None),
        ("https://idp'x.example.test/token", None),
        ("https://idp_x.example.test/token", None),
        ("https://idp.example.test/token?tenant=x", None),
        ("https://idp.example.test/token#x", None),
        ("not a url", None),
    ] {
        assert_eq!(
            workbench::token_endpoint_source(endpoint).as_deref(),
            source,
            "{endpoint}"
        );
    }
    // Those hosts do parse (WHATWG), so the LDH check is what refuses them.
    for host in ["idp;x", "idp,x", "idp'x"] {
        let url = reqwest::Url::parse(&format!("https://{host}.example.test/token")).unwrap();
        assert_eq!(
            url.host_str(),
            Some(format!("{host}.example.test").as_str())
        );
    }
    // Such a host refuses to start, naming the rule.
    let hostile = mcp_http::BrowserOAuth {
        token_endpoint: "https://idp;x.example.test/token".into(),
        ..browser_oauth()
    };
    let error = workbench::content_security_policy(Some(&hostile))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot be named in the workbench page's connect-src: its host must be an IP address or DNS labels of ASCII letters, digits, '-' and '.'"),
        "{error}"
    );
    let app = mcp_http::router(Config {
        oauth_enabled: true,
        browser_oauth: Some(browser_oauth()),
        ..keyed_config(None)
    });
    for path in ["/", "/app.css", "/app.js", "/workbench/auth.json"] {
        let reply = send(&app, get(path)).await;
        assert_eq!(reply.header("content-security-policy"), with, "{path}");
    }
    let plain = mcp_http::router(keyed_config(None));
    for path in ["/", "/workbench/auth.json"] {
        let reply = send(&plain, get(path)).await;
        assert_eq!(reply.header("content-security-policy"), CSP, "{path}");
    }
    // The page's own API calls are unchanged: the IdP origin is not allowed
    // to call this server.
    let reply = send(
        &app,
        calc(
            "describe",
            Some(SECRET),
            Some("https://idp.example.test:8443"),
            &json!({}),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[test]
fn browser_client_id_needs_oauth() {
    for options in [
        ServeOptions {
            oauth_browser_client_id: Some("boxscore-workbench".into()),
            ..ServeOptions::default()
        },
        ServeOptions {
            oauth_browser_client_id: Some("boxscore-workbench".into()),
            authorization_servers: vec!["https://idp.example.test".into()],
            ..ServeOptions::default()
        },
    ] {
        let error = mcp_http::prepare(&options).unwrap_err().to_string();
        assert!(
            error.starts_with("--oauth-browser-client-id needs OAuth configured: --authorization-server with --oauth-audience, --oauth-client-id and --oauth-client-secret-file"),
            "{error}"
        );
    }
    let stray = ServeOptions {
        oauth_token_endpoint: Some("https://idp.example.test/token".into()),
        ..ServeOptions::default()
    };
    let error = mcp_http::prepare(&stray).unwrap_err().to_string();
    assert!(
        error.contains("only with --oauth-browser-client-id"),
        "{error}"
    );
}

/// Each `<form ...>` start tag in the page.
fn form_tags(html: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<form") {
        let tail = &rest[at..];
        let end = tail.find('>').expect("a form tag is closed");
        found.push(&tail[..=end]);
        rest = &tail[end..];
    }
    found
}

#[test]
fn page_has_the_import_and_trace_landmarks_and_no_form_target() {
    let html = source("index.html");
    // The three landmarks the script wires: the import form, its profile
    // choice (filled from describe) and the trace lookup form.
    for landmark in [
        r#"<form id="import-form""#,
        r#"<select id="import-profile""#,
        r#"<form id="trace-form""#,
        r#"<fieldset id="issue-lock">"#,
    ] {
        assert!(html.contains(landmark), "index.html lacks {landmark}");
    }
    // Panels in order: sign-in, key, load a period, loaded data, review (its
    // body, then the issue block after the citation), trace.
    let order: Vec<usize> = [
        r#"id="signin-panel""#,
        r#"id="key-panel""#,
        r#"id="import-panel""#,
        r#"id="loaded""#,
        r#"id="review""#,
        r#"id="review-body""#,
        r#"id="issue""#,
        r#"id="trace-panel""#,
    ]
    .iter()
    .map(|id| html.find(id).unwrap_or_else(|| panic!("no {id}")))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    assert!(html.contains(">Load a period<") && html.contains(">Look up a trace<"));

    // CSP form-action 'none': every form is handled by script, so none has
    // an action or method (a file input's own form, if any, would be a form).
    let forms = form_tags(&html);
    // The issue block adds no form: its buttons are handled by script inside
    // a fieldset, so there is nothing to submit and no action to name.
    assert_eq!(forms.len(), 4, "{forms:?}");
    for tag in forms {
        let lower = tag.to_ascii_lowercase();
        for attribute in [" action", " method", " enctype", " target", " formaction"] {
            assert!(!lower.contains(attribute), "{tag} has{attribute}");
        }
    }
    assert!(!html.to_ascii_lowercase().contains("formaction"));
    // The correction fields and the confirmation are in the page.
    for id in [
        "import-correction",
        "import-supersedes",
        "import-reason",
        "import-confirm-yes",
        "import-confirm-no",
        "trace-input",
        "issue-reports",
        "issue-open",
        "issue-note",
        "issue-confirm",
        "issue-confirm-body",
        "issue-confirm-yes",
        "issue-confirm-no",
        "issue-result",
        "signin-start",
        "signin-out",
        "signin-message",
    ] {
        assert!(html.contains(&format!(r#"id="{id}""#)), "no {id}");
    }
}

/// The page labels each account's class from its category; its list of
/// revenue categories must be the engine's.
#[test]
fn page_revenue_categories_match_the_engine() {
    let js = source("app.js");
    let start = js
        .find("var REVENUE_CATEGORIES = [")
        .expect("REVENUE_CATEGORIES in app.js");
    let list = &js[start..];
    let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
    let categories: Vec<&str> = list
        .split(',')
        .map(|c| c.trim().trim_matches('"'))
        .collect();
    assert_eq!(categories.len(), 4, "{categories:?}");
    for category in &categories {
        assert_eq!(account_class(category), AccountClass::Revenue, "{category}");
    }
    assert!(js.contains(r#"if (c === "unmapped") return "unmapped";"#));
    // The page classifies the returned category as it is: the engine has
    // normalized it already, and JavaScript's trim() strips characters
    // Rust's keeps (U+FEFF), so a second normalization would disagree.
    let class_of = &js[js.find("function classOf(category) {").unwrap()..];
    let class_of = &class_of[..class_of.find("\n  }").unwrap()];
    assert!(class_of.contains("var c = text(category);"), "{class_of}");
    assert!(!class_of.contains("trim"), "{class_of}");
    assert!(
        !class_of.to_ascii_lowercase().contains("case"),
        "{class_of}"
    );
    assert_eq!(account_class("unmapped"), AccountClass::Unmapped);
    for expense in [
        "payroll",
        "utilities",
        "repairs and maintenance",
        "insurance",
    ] {
        assert_eq!(account_class(expense), AccountClass::Expense);
        assert!(!categories.contains(&expense));
    }
}

fn row(code: &str, name: &str, category: &str, amount: &str) -> Value {
    json!({"account_code": code, "account_name": name, "category": category, "amount": amount})
}

#[tokio::test]
async fn hostile_account_names_come_back_as_plain_json_strings() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(Config {
        database: Some(db),
        ..loopback_config(3819)
    });
    let origin = Some("http://127.0.0.1:3819");

    let dataset = json!({
        "property": "Synthetic <b>Elm</b>", "period": "2026-05",
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [row("4000", HOSTILE_NAME, "rental income", "1234567.89"),
                    row("5000", HOSTILE_SCRIPT, "payroll", "30.00")],
        "budgets": [row("4000", HOSTILE_NAME, "rental income", "1200000.00"),
                    row("5000", HOSTILE_SCRIPT, "payroll", "25.00")],
        "snapshot": null
    });
    let imported = send(
        &app,
        calc("import_period", None, origin, &json!({"dataset": dataset})),
    )
    .await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.json());
    let revision = imported.json()["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = send(&app, calc("list_revisions", None, origin, &json!({}))).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(
        listed.json()["result"]["revisions"][0]["property"],
        "Synthetic <b>Elm</b>"
    );

    let reviewed = send(
        &app,
        calc(
            "review_period",
            None,
            origin,
            &json!({"revision_id": revision}),
        ),
    )
    .await;
    assert_eq!(reviewed.status, StatusCode::OK);
    assert!(reviewed
        .header("content-type")
        .starts_with("application/json"));
    let envelope = reviewed.json();
    assert_eq!(envelope["status"], "review_required");
    let accounts = envelope["result"]["variance"]["by_account"]
        .as_array()
        .unwrap();
    assert_eq!(accounts[0]["account_name"], HOSTILE_NAME);
    assert_eq!(accounts[1]["account_name"], HOSTILE_SCRIPT);
    // Money stays a string; the page only regroups its digits.
    assert_eq!(accounts[0]["variance"], "34567.89");
    assert_eq!(
        envelope["result"]["variance"]["noi_bridge"]["noi_variance"],
        "34562.89"
    );
    // On the wire the names are JSON strings: the markup is quoted and the
    // quotes inside it escaped.
    let raw = String::from_utf8(reviewed.body.clone()).unwrap();
    assert!(raw.contains(&format!("\"account_name\":\"{HOSTILE_NAME}\"")));
    assert!(raw.contains(r#"<script>alert(\"x\")</script>"#));

    // A control character in an account name never reaches a stored revision
    // or the page: the import refuses, naming the row, the field and the
    // code point.
    let mut with_bell = dataset.clone();
    with_bell["actuals"][1]["account_name"] = json!("Payroll\u{7}\u{1b}[31m");
    let refused = send(
        &app,
        calc(
            "import_period",
            None,
            origin,
            &json!({"dataset": with_bell}),
        ),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.json()
    );
    let envelope = refused.json();
    assert_eq!(envelope["error"]["code"], "INVALID_INPUT");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("actuals[1].account_name") && message.contains("U+0007"),
        "{message}"
    );
}

/// UPL-102. Rust's trim keeps U+FEFF, so a category of "\u{FEFF}rental income"
/// is not revenue to the engine, while JavaScript's trim strips it and would
/// call it revenue. The import now refuses such a category outright, so the
/// two can no longer disagree about a stored revision; the engine's own class
/// function is unchanged and still calls it an expense.
#[tokio::test]
async fn a_byte_order_mark_in_a_category_is_refused_at_import() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(Config {
        database: Some(db),
        ..loopback_config(3819)
    });
    let marked = "\u{FEFF}rental income";
    assert_eq!(account_class(marked), AccountClass::Expense);
    let dataset = json!({
        "property": "Synthetic Birch", "period": "2026-05",
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [row("4000", "Rent", "rental income", "1000.00"),
                    row("4100", "Marked rent", marked, "300.00"),
                    row("5000", "Payroll", "payroll", "100.00")],
        "budgets": [row("4000", "Rent", "rental income", "900.00"),
                    row("4100", "Marked rent", marked, "200.00"),
                    row("5000", "Payroll", "payroll", "100.00")],
        "snapshot": null
    });
    let imported = send(
        &app,
        calc("import_period", None, None, &json!({"dataset": dataset})),
    )
    .await;
    assert_eq!(
        imported.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        imported.json()
    );
    let envelope = imported.json();
    assert_eq!(envelope["status"], "refused");
    assert_eq!(envelope["error"]["code"], "INVALID_INPUT");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("actuals[1].category") && message.contains("U+FEFF"),
        "{message}"
    );
    // Nothing was stored.
    let listed = send(&app, calc("list_revisions", None, None, &json!({}))).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json()["result"]["revisions"], json!([]));
}

#[cfg(unix)]
#[tokio::test]
async fn serve_announces_the_workbench_and_allows_its_own_origin() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut listening = String::new();
    stderr.read_line(&mut listening).unwrap();
    let mut announced = String::new();
    stderr.read_line(&mut announced).unwrap();
    let base = listening
        .split_whitespace()
        .find(|w| w.starts_with("http://127.0.0.1:"))
        .unwrap_or_else(|| panic!("no address in {listening:?}"))
        .to_string();
    assert_eq!(
        announced.trim_end(),
        format!("boxscore-exact serve: workbench at {base}/")
    );
    let port = base.rsplit(':').next().unwrap().to_string();

    let client = reqwest::Client::new();
    let page = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status().as_u16(), 200);
    assert_eq!(page.headers()["content-security-policy"], CSP);
    for (origin, expected) in [
        (base.clone(), 200),
        (format!("http://localhost:{port}"), 200),
        ("http://127.0.0.1:1".to_string(), 403),
    ] {
        let reply = client
            .post(format!("{base}/v1/calc/describe"))
            .header("origin", &origin)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status().as_u16(), expected, "{origin}");
    }

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("server did not stop on SIGTERM");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What the import screen sends: the docs/quickstart-style files as inline
/// text, one of them with markup in an account name. It comes back from
/// `review_period` and `get_provenance` as JSON strings, and the recorded
/// sources list each file's name, size and hash, which is what the page shows.
#[tokio::test]
async fn an_inline_import_with_markup_in_a_file_round_trips_as_text() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(Config {
        database: Some(db),
        ..loopback_config(3819)
    });
    let origin = Some("http://127.0.0.1:3819");

    let actuals = format!(
        "account_code,account_name,category,amount\n4000,{HOSTILE_NAME},rental income,1000.00\n5000,</td><script>alert(\"x\")</script>,payroll,100.00\n"
    );
    let budgets = format!(
        "account_code,account_name,category,amount\n4000,{HOSTILE_NAME},rental income,900.00\n5000,</td><script>alert(\"x\")</script>,payroll,90.00\n"
    );
    let payload = json!({
        "profile": "canonical.csv/1",
        "files": {"actuals": actuals, "budgets": budgets},
        "options": {"property": "Synthetic <b>Elm</b>", "period": "2026-05", "units": "10"}
    });
    let imported = send(&app, calc("import_period", None, origin, &payload)).await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.json());
    let envelope = imported.json();
    assert_eq!(envelope["status"], "calculated");
    let revision = envelope["result"]["revision_id"].as_str().unwrap();
    let trace = envelope["trace_id"].as_str().unwrap();
    assert!(envelope["citation"].as_str().unwrap().contains(trace));

    let reviewed = send(
        &app,
        calc(
            "review_period",
            None,
            origin,
            &json!({"revision_id": revision}),
        ),
    )
    .await;
    assert_eq!(reviewed.status, StatusCode::OK, "{}", reviewed.json());
    assert!(reviewed
        .header("content-type")
        .starts_with("application/json"));
    let review = reviewed.json();
    assert_eq!(review["result"]["property"], "Synthetic <b>Elm</b>");
    let accounts = review["result"]["variance"]["by_account"]
        .as_array()
        .unwrap();
    assert_eq!(accounts[0]["account_name"], HOSTILE_NAME);
    assert_eq!(
        accounts[1]["account_name"],
        "</td><script>alert(\"x\")</script>"
    );
    let raw = String::from_utf8(reviewed.body.clone()).unwrap();
    assert!(raw.contains(&format!("\"account_name\":\"{HOSTILE_NAME}\"")));

    // The trace lookup: the revision's sources carry the profile and one
    // entry per inline file, with its byte count and hash and no path.
    let looked = send(
        &app,
        calc("get_provenance", None, origin, &json!({"trace_id": trace})),
    )
    .await;
    assert_eq!(looked.status, StatusCode::OK, "{}", looked.json());
    let record = looked.json();
    assert_eq!(record["result"]["trace"]["function"], "import_period");
    assert_eq!(record["result"]["trace"]["revision_id"], revision);
    let sources = &record["result"]["revision"]["sources"];
    assert_eq!(sources["profile"], "canonical.csv/1");
    let files = sources["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["name"], "actuals");
    assert_eq!(files[0]["bytes"], actuals.len());
    assert_eq!(files[0]["sha256"].as_str().unwrap().len(), 64);
    assert!(files.iter().all(|f| f.get("path").is_none()));

    // An unknown trace id is a refusal the page shows as the engine says it.
    let unknown = send(
        &app,
        calc(
            "get_provenance",
            None,
            origin,
            &json!({"trace_id": "00000000-0000-0000-0000-000000000000"}),
        ),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unknown.json()["status"], "refused");
    assert_eq!(unknown.json()["error"]["code"], "NOT_FOUND");

    // A missing file is the engine's refusal too, not the page's.
    let missing = send(
        &app,
        calc(
            "import_period",
            None,
            origin,
            &json!({"profile": "canonical.csv/1", "files": {"actuals": actuals},
                    "options": {"property": "P", "period": "2026-05", "units": "10"}}),
        ),
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(missing.json()["error"]["code"], "MISSING_FILE");
}

/// What the review screen's Issue report button relies on: `issue_report` on
/// a fresh import returns a report id, `list_revisions` then lists it with its
/// `body_sha256` (the page's Issued reports block), issuing the same revision
/// again is allowed and makes a second, different report, an unknown revision
/// refuses `NOT_FOUND`, and the issue's trace id resolves to the report in the
/// trace lookup.
#[tokio::test]
async fn issuing_a_report_is_listed_and_a_second_issue_makes_another() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(Config {
        database: Some(db),
        ..loopback_config(3819)
    });
    let origin = Some("http://127.0.0.1:3819");

    let payload = json!({
        "profile": "canonical.csv/1",
        "files": {
            "actuals": "account_code,account_name,category,amount\n4000,Rent,rental income,1000.00\n5000,Payroll,payroll,100.00\n",
            "budgets": "account_code,account_name,category,amount\n4000,Rent,rental income,900.00\n5000,Payroll,payroll,90.00\n"
        },
        "options": {"property": "Synthetic Elm", "period": "2026-05", "units": "10"}
    });
    let imported = send(&app, calc("import_period", None, origin, &payload)).await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.json());
    let revision = imported.json()["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let input = json!({"revision_id": revision});

    let listed = |app: Router| async move {
        let reply = send(&app, calc("list_revisions", None, origin, &json!({}))).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.json());
        reply.json()["result"]["revisions"][0]["reports"].clone()
    };
    // Nothing issued yet: the block's empty state.
    assert_eq!(listed(app.clone()).await, json!([]));

    let first = send(&app, calc("issue_report", None, origin, &input)).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json());
    let first = first.json();
    let first_id = first["result"]["report_id"].as_str().unwrap().to_owned();
    assert!(first["result"]["issued_at"].is_string());
    assert_eq!(first["result"]["human_approval"], "not_granted");
    // No snapshot was loaded, so the review needs a person; the engine still
    // issues (review_required does not block it), and says why in the body.
    assert_eq!(first["status"], "review_required");
    assert_eq!(
        first["result"]["variance"]["review_reasons"],
        json!(["missing_occupancy_snapshot"])
    );
    assert!(first["citation"].as_str().unwrap().contains(&first_id));

    let reports = listed(app.clone()).await;
    let reports = reports.as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["id"], first_id.as_str());
    assert_eq!(reports[0]["created_at"], first["result"]["issued_at"]);
    let first_hash = reports[0]["body_sha256"].as_str().unwrap().to_owned();
    assert_eq!(first_hash.len(), 64);
    assert!(first_hash.chars().all(|c| c.is_ascii_hexdigit()));

    // The issue's trace id, as a citation carries it, shows the Report
    // section in the trace lookup.
    let trace = first["trace_id"].as_str().unwrap();
    let looked = send(
        &app,
        calc("get_provenance", None, origin, &json!({"trace_id": trace})),
    )
    .await;
    assert_eq!(looked.status, StatusCode::OK, "{}", looked.json());
    let record = looked.json();
    assert_eq!(record["result"]["trace"]["function"], "issue_report");
    assert_eq!(record["result"]["trace"]["report_id"], first_id.as_str());
    assert_eq!(record["result"]["report"]["id"], first_id.as_str());
    assert_eq!(
        record["result"]["report"]["body_sha256"],
        first_hash.as_str()
    );

    // The engine does not refuse a second issue for the same revision: it
    // makes another report, with a new id and hash, and keeps the first.
    let second = send(&app, calc("issue_report", None, origin, &input)).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.json());
    let second = second.json();
    assert_eq!(second["status"], "review_required");
    let second_id = second["result"]["report_id"].as_str().unwrap();
    assert_ne!(second_id, first_id);
    let reports = listed(app.clone()).await;
    let reports = reports.as_array().unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0]["id"], first_id.as_str());
    assert_eq!(reports[0]["body_sha256"], first_hash.as_str());
    assert_eq!(reports[1]["id"], second_id);
    assert_ne!(reports[1]["body_sha256"], first_hash.as_str());

    // An unknown revision is the engine's refusal, shown as it says it.
    let unknown = send(
        &app,
        calc(
            "issue_report",
            None,
            origin,
            &json!({"revision_id": "00000000-0000-0000-0000-000000000000"}),
        ),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unknown.json()["status"], "refused");
    assert_eq!(unknown.json()["error"]["code"], "NOT_FOUND");
    assert_eq!(unknown.json()["error"]["message"], "Unknown revision");
    assert_eq!(listed(app).await.as_array().unwrap().len(), 2);
}

/// What the review screen's superseded warning documents: the engine refuses
/// `issue_report` on a revision a correction has superseded, with `SUPERSEDED`
/// and a message naming the latest revision, and stores no report. `t12` with
/// `issued_only` finds no issued revision for the chain until the latest is
/// issued, and then uses it.
#[tokio::test]
async fn issuing_a_superseded_revision_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(["init", "--database", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(init.status.code(), Some(0));
    let app = mcp_http::router(Config {
        database: Some(db),
        ..loopback_config(3820)
    });
    let origin = Some("http://127.0.0.1:3820");

    let files = |rent: &str| {
        json!({
            "actuals": format!("account_code,account_name,category,amount\n4000,Rent,rental income,{rent}\n5000,Payroll,payroll,100.00\n"),
            "budgets": "account_code,account_name,category,amount\n4000,Rent,rental income,900.00\n5000,Payroll,payroll,90.00\n"
        })
    };
    let options = json!({"property": "Synthetic Oak", "period": "2026-05", "units": "10"});
    let first = send(
        &app,
        calc(
            "import_period",
            None,
            origin,
            &json!({"profile": "canonical.csv/1", "files": files("1000.00"), "options": options}),
        ),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.json());
    let old_id = first.json()["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let corrected = send(
        &app,
        calc(
            "import_period",
            None,
            origin,
            &json!({"profile": "canonical.csv/1", "files": files("1500.00"), "options": options,
                    "supersedes": old_id, "reason": "synthetic correction"}),
        ),
    )
    .await;
    assert_eq!(corrected.status, StatusCode::OK, "{}", corrected.json());
    let new_id = corrected.json()["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new_id, old_id);

    // The superseded revision is refused: the code, and a message naming
    // both revisions by id and revision number. Nothing is stored.
    let refused = send(
        &app,
        calc(
            "issue_report",
            None,
            origin,
            &json!({"revision_id": old_id}),
        ),
    )
    .await;
    let refusal = refused.json();
    assert_eq!(refusal["status"], "refused", "{refusal}");
    assert_eq!(refusal["error"]["code"], "SUPERSEDED");
    assert_eq!(
        refusal["error"]["message"],
        format!(
            "Revision {old_id} (revision 1) is superseded; the latest is {new_id} (revision 2)"
        )
    );
    let revisions = || {
        let app = app.clone();
        async move {
            let listed = send(&app, calc("list_revisions", None, origin, &json!({}))).await;
            assert_eq!(listed.status, StatusCode::OK, "{}", listed.json());
            listed.json()["result"]["revisions"].clone()
        }
    };
    let rows = revisions().await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let find = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(find(&new_id)["parent_id"], old_id.as_str());
    assert_eq!(find(&old_id)["reports"], json!([]));
    assert_eq!(find(&new_id)["reports"], json!([]));

    // t12 on the issued-only policy finds no issued revision for the chain;
    // the default policy uses the correction.
    let t12 = |policy: &str| {
        let app = app.clone();
        let input = json!({"property": "Synthetic Oak", "end_period": "2026-05", "revision_policy": policy});
        async move { send(&app, calc("t12", None, origin, &input)).await }
    };
    let none_issued = t12("issued_only").await;
    assert_eq!(
        none_issued.json()["error"]["code"],
        "NOT_FOUND",
        "{}",
        none_issued.json()
    );
    let latest = t12("latest").await;
    assert_eq!(latest.status, StatusCode::OK, "{}", latest.json());
    assert_eq!(
        latest.json()["result"]["revisions"][11]["revision_id"],
        new_id.as_str()
    );

    // The latest revision issues, repeat issuing makes another report, and
    // the issued-only policy then uses the correction.
    for _ in 0..2 {
        let issued = send(
            &app,
            calc(
                "issue_report",
                None,
                origin,
                &json!({"revision_id": new_id}),
            ),
        )
        .await;
        assert_eq!(issued.status, StatusCode::OK, "{}", issued.json());
        // These files carry no occupancy snapshot, so the revision is
        // `review_required`, and it still issues.
        assert_eq!(issued.json()["status"], "review_required");
    }
    let rows = revisions().await;
    let rows = rows.as_array().unwrap();
    let find = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(find(&old_id)["reports"], json!([]));
    assert_eq!(find(&new_id)["reports"].as_array().unwrap().len(), 2);
    let issued_only = t12("issued_only").await;
    assert_eq!(issued_only.status, StatusCode::OK, "{}", issued_only.json());
    assert_eq!(
        issued_only.json()["result"]["revisions"][11]["revision_id"],
        new_id.as_str()
    );
}
