//! Connector conformance: the catalog, the MCP server and the HTTP server
//! must return the same envelope for the same input.
//!
//! Every case in `tests/fixtures/catalog/` (format: that directory's README)
//! runs four ways: `catalog::invoke`, `mcp::Server::handle` with a
//! `tools/call`, `POST /v1/calc/{function}` and `POST /mcp` through the axum
//! router (loopback configuration, no authentication). Result, status, error,
//! hashes, function and version must agree across all of them, match the
//! golden `expected.json`, and satisfy the function's published output
//! schema. Synthetic data only.
//!
//! `BOXSCORE_UPDATE_GOLDENS=1` writes missing goldens and regenerates goldens
//! whose pinned `version` is no longer the served one. It never rewrites a
//! golden whose version is unchanged: a changed number under an unchanged
//! version is a failure, not something to regenerate.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use boxscore::exact::{
    catalog::{self, Call, CATALOG},
    digest,
    mcp::Server,
    mcp_http::{self, ServeOptions},
    store,
};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use tower::ServiceExt;

const UPDATE_VAR: &str = "BOXSCORE_UPDATE_GOLDENS";
const CHAIN_DIR: &str = "chain";
const CHAIN_FILE: &str = "steps.json";
const INPUT_SUFFIX: &str = ".input.json";
const EXPECTED_SUFFIX: &str = ".expected.json";
const EXPECTED_KEYS: [&str; 5] = ["status", "version", "result", "error", "output_sha256"];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog")
}

fn updating() -> bool {
    std::env::var_os(UPDATE_VAR).is_some_and(|v| !v.is_empty() && v != "0")
}

// ---------------------------------------------------------------------------
// Corpus discovery
// ---------------------------------------------------------------------------

struct Case {
    id: String,
    function: String,
    input_path: PathBuf,
    expected_path: PathBuf,
}

struct Chain {
    id: String,
    path: PathBuf,
}

#[derive(Default)]
struct Corpus {
    cases: Vec<Case>,
    chains: Vec<Chain>,
    /// Layout problems: stray files, unknown directories, unpaired files.
    problems: Vec<String>,
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn discover() -> Corpus {
    let mut corpus = Corpus::default();
    let root = corpus_root();
    let entries = match read_dir_sorted(&root) {
        Ok(entries) => entries,
        Err(e) => {
            corpus.problems.push(e);
            return corpus;
        }
    };
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() {
            if name != "README.md" {
                corpus
                    .problems
                    .push(format!("{name}: unexpected file at the corpus root"));
            }
            continue;
        }
        if name == CHAIN_DIR {
            discover_chains(&entry.path(), &mut corpus);
        } else if CATALOG.iter().any(|c| c.name == name) {
            discover_function(&name, &entry.path(), &mut corpus);
        } else {
            corpus.problems.push(format!(
                "{name}/: not a catalog function and not {CHAIN_DIR}/ (typo in the directory name?)"
            ));
        }
    }
    corpus
}

fn discover_function(function: &str, dir: &Path, corpus: &mut Corpus) {
    let entries = match read_dir_sorted(dir) {
        Ok(entries) => entries,
        Err(e) => {
            corpus.problems.push(e);
            return;
        }
    };
    let mut inputs = BTreeSet::new();
    let mut expecteds = BTreeSet::new();
    for entry in entries {
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = file.strip_suffix(INPUT_SUFFIX) {
            inputs.insert(stem.to_string());
        } else if let Some(stem) = file.strip_suffix(EXPECTED_SUFFIX) {
            expecteds.insert(stem.to_string());
        } else {
            corpus.problems.push(format!(
                "{function}/{file}: expected <case>{INPUT_SUFFIX} or <case>{EXPECTED_SUFFIX}"
            ));
        }
    }
    for stem in expecteds.difference(&inputs) {
        corpus.problems.push(format!(
            "{function}/{stem}{EXPECTED_SUFFIX}: no matching {stem}{INPUT_SUFFIX}"
        ));
    }
    for stem in inputs {
        corpus.cases.push(Case {
            id: format!("{function}/{stem}"),
            function: function.to_string(),
            input_path: dir.join(format!("{stem}{INPUT_SUFFIX}")),
            expected_path: dir.join(format!("{stem}{EXPECTED_SUFFIX}")),
        });
    }
}

fn discover_chains(dir: &Path, corpus: &mut Corpus) {
    let entries = match read_dir_sorted(dir) {
        Ok(entries) => entries,
        Err(e) => {
            corpus.problems.push(e);
            return;
        }
    };
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let steps = entry.path().join(CHAIN_FILE);
        if entry.path().is_dir() && steps.is_file() {
            corpus.chains.push(Chain {
                id: format!("{CHAIN_DIR}/{name}"),
                path: steps,
            });
        } else {
            corpus.problems.push(format!(
                "{CHAIN_DIR}/{name}: a chain is a directory holding {CHAIN_FILE}"
            ));
        }
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Functions with at least one single case or at least one chain step.
fn covered_functions(corpus: &Corpus) -> BTreeSet<String> {
    let mut covered: BTreeSet<String> = corpus.cases.iter().map(|c| c.function.clone()).collect();
    for chain in &corpus.chains {
        if let Ok(doc) = read_json(&chain.path) {
            for step in doc["steps"].as_array().into_iter().flatten() {
                if let Some(function) = step["function"].as_str() {
                    covered.insert(function.to_string());
                }
            }
        }
    }
    covered
}

// ---------------------------------------------------------------------------
// Output-schema validation (minimal, structural)
// ---------------------------------------------------------------------------

fn type_matches(name: &str, value: &Value) -> Result<bool, String> {
    Ok(match name {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        other => return Err(format!("validator does not know the type {other:?}")),
    })
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Check `value` against the subset of JSON Schema the catalog publishes:
/// `type` (name or list), `enum`, `const`, `required`, `properties`,
/// `additionalProperties` (schema or `false`), `items`, `minimum`, `anyOf`
/// and `oneOf`. Anything else (`pattern`, `description`, ...) is ignored.
/// The error names the first path that does not conform.
fn validate(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(wanted) = schema.get("type") {
        let names: Vec<&str> = match wanted {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => return Err(format!("{path}: schema has a malformed type")),
        };
        let mut matched = false;
        for name in &names {
            matched |= type_matches(name, value).map_err(|e| format!("{path}: {e}"))?;
        }
        if !matched {
            return Err(format!(
                "{path}: expected type {}, got {}",
                names.join(" or "),
                kind(value)
            ));
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            return Err(format!("{path}: expected const {constant}, got {value}"));
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum") {
        if !allowed.contains(value) {
            return Err(format!("{path}: {value} is not one of {allowed:?}"));
        }
    }
    if let (Some(minimum), Some(number)) = (
        schema.get("minimum").and_then(Value::as_f64),
        value.as_f64(),
    ) {
        if number < minimum {
            return Err(format!("{path}: {number} is below the minimum {minimum}"));
        }
    }
    if let Value::Object(object) = value {
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{path}: missing required key {key:?}"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            match properties.and_then(|p| p.get(key)) {
                Some(child_schema) => validate(child_schema, child, &child_path)?,
                None => match additional {
                    Some(Value::Bool(false)) => {
                        return Err(format!("{child_path}: key is not allowed by the schema"))
                    }
                    Some(extra @ Value::Object(_)) => validate(extra, child, &child_path)?,
                    _ => {}
                },
            }
        }
    }
    if let (Value::Array(items), Some(item_schema @ Value::Object(_))) =
        (value, schema.get("items"))
    {
        for (index, item) in items.iter().enumerate() {
            validate(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    if let Some(Value::Array(branches)) = schema.get("anyOf") {
        let failures: Vec<String> = branches
            .iter()
            .filter_map(|b| validate(b, value, path).err())
            .collect();
        if failures.len() == branches.len() {
            return Err(format!(
                "{path}: matches none of anyOf ({})",
                failures.join("; ")
            ));
        }
    }
    if let Some(Value::Array(branches)) = schema.get("oneOf") {
        let passing = branches
            .iter()
            .filter(|b| validate(b, value, path).is_ok())
            .count();
        if passing != 1 {
            return Err(format!("{path}: matches {passing} oneOf branches, not 1"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Placeholders: ids and timestamps compare by shape, everything else exactly
// ---------------------------------------------------------------------------

fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_timestamp(text: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

/// Whether `expected` is a placeholder, and if so whether `actual` fits it.
/// `None` means `expected` is an ordinary string.
fn placeholder_fits(
    expected: &str,
    actual: &Value,
    captures: &BTreeMap<String, String>,
) -> Option<Result<(), String>> {
    let inner = expected.strip_prefix('<')?.strip_suffix('>')?;
    let text = actual.as_str();
    let shape = |ok: bool, what: &str| -> Result<(), String> {
        if ok {
            Ok(())
        } else {
            Err(format!("expected {what}, got {actual}"))
        }
    };
    Some(match inner {
        "id" => shape(text.is_some_and(is_id), "an id"),
        "string" => shape(text.is_some_and(|t| !t.is_empty()), "a non-empty string"),
        "timestamp" => shape(text.is_some_and(is_timestamp), "an RFC 3339 timestamp"),
        "sha256" => shape(text.is_some_and(is_sha256), "a SHA-256 hex digest"),
        _ => {
            let name = inner.strip_prefix("id=")?;
            match captures.get(name) {
                None => Err(format!("placeholder {expected} names an id never captured")),
                Some(id) if text == Some(id.as_str()) => Ok(()),
                Some(id) => Err(format!("expected the captured {name} ({id}), got {actual}")),
            }
        }
    })
}

/// Compare `actual` with `expected`: same shape, same keys, same values,
/// except where `expected` holds a placeholder, which matches by shape.
/// Returns `actual` with each placeholder position replaced by the
/// placeholder text, so results from different databases can be compared.
fn normalize(
    expected: &Value,
    actual: &Value,
    path: &str,
    captures: &BTreeMap<String, String>,
) -> Result<Value, String> {
    match (expected, actual) {
        (Value::String(text), _) => match placeholder_fits(text, actual, captures) {
            Some(Ok(())) => Ok(expected.clone()),
            Some(Err(e)) => Err(format!("{path}: {e}")),
            None if expected == actual => Ok(actual.clone()),
            None => Err(format!("{path}: expected {expected}, got {actual}")),
        },
        (Value::Object(want), Value::Object(got)) => {
            for key in want.keys().chain(got.keys()) {
                if !want.contains_key(key) || !got.contains_key(key) {
                    let side = if want.contains_key(key) {
                        "actual"
                    } else {
                        "expected"
                    };
                    return Err(format!("{path}.{key}: missing from {side}"));
                }
            }
            let mut out = Map::new();
            for (key, want_child) in want {
                out.insert(
                    key.clone(),
                    normalize(want_child, &got[key], &format!("{path}.{key}"), captures)?,
                );
            }
            Ok(Value::Object(out))
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                return Err(format!(
                    "{path}: expected {} items, got {}",
                    want.len(),
                    got.len()
                ));
            }
            want.iter()
                .zip(got)
                .enumerate()
                .map(|(i, (w, g))| normalize(w, g, &format!("{path}[{i}]"), captures))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        _ if expected == actual => Ok(actual.clone()),
        _ => Err(format!("{path}: expected {expected}, got {actual}")),
    }
}

/// Golden generation for chains: replace generated values by placeholders.
fn placeholderize(value: &Value, key: &str, captures: &BTreeMap<String, String>) -> Value {
    const ID_KEYS: [&str; 6] = [
        "revision_id",
        "report_id",
        "id",
        "parent_id",
        "supersedes_revision",
        "trace_id",
    ];
    const TIME_KEYS: [&str; 4] = ["created_at", "issued_at", "started_at", "finished_at"];
    const HASH_KEYS: [&str; 3] = ["input_sha256", "body_sha256", "output_sha256"];
    match value {
        Value::String(text) if ID_KEYS.contains(&key) => {
            match captures.iter().find(|(_, id)| *id == text) {
                Some((name, _)) => json!(format!("<id={name}>")),
                None => json!("<id>"),
            }
        }
        Value::String(_) if TIME_KEYS.contains(&key) => json!("<timestamp>"),
        // The crate version moves with every release, not with a function version.
        Value::String(_) if key == "producer_version" => json!("<string>"),
        Value::String(_) if HASH_KEYS.contains(&key) => json!("<sha256>"),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(k, v)| (k.clone(), placeholderize(v, k, captures)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| placeholderize(v, key, captures))
                .collect(),
        ),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// The four ways to run a call
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Via {
    Direct,
    McpServer,
    HttpCalc,
    HttpMcp,
}

const ALL_VIA: [Via; 4] = [Via::Direct, Via::McpServer, Via::HttpCalc, Via::HttpMcp];

impl Via {
    fn name(self) -> &'static str {
        match self {
            Via::Direct => "catalog::invoke",
            Via::McpServer => "mcp::Server::handle tools/call",
            Via::HttpCalc => "POST /v1/calc/{function}",
            Via::HttpMcp => "POST /mcp tools/call",
        }
    }
}

/// The loopback configuration `boxscore-exact serve` builds with no
/// `--api-key-file`: no authentication, the database fixed by the host.
fn loopback_router(database: Option<&Path>) -> Result<Router, String> {
    let options = ServeOptions {
        database: database.map(Path::to_path_buf),
        ..ServeOptions::default()
    };
    let (addr, config) = mcp_http::prepare(&options).map_err(|e| format!("{e:?}"))?;
    if !addr.ip().is_loopback() || config.validator.is_some() {
        return Err("expected a loopback configuration without authentication".into());
    }
    Ok(mcp_http::router(config))
}

async fn post(app: Router, request: Request<Body>) -> Result<(StatusCode, Vec<u8>), String> {
    let response = app.oneshot(request).await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| e.to_string())?;
    Ok((status, body.to_vec()))
}

fn tool_call_message(function: &str, input: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
           "params": {"name": function, "arguments": input}})
}

/// Pull the envelope out of an MCP `tools/call` reply, checking the MCP
/// framing on the way: no JSON-RPC error, `isError` matches the refusal, and
/// the text content is the same envelope as `structuredContent`.
fn envelope_from_tool_reply(reply: &Value) -> Result<Value, String> {
    if let Some(error) = reply.get("error") {
        return Err(format!("JSON-RPC error instead of a tool result: {error}"));
    }
    let result = reply
        .get("result")
        .ok_or_else(|| format!("reply has no result: {reply}"))?;
    let envelope = result
        .get("structuredContent")
        .cloned()
        .ok_or("tool result has no structuredContent")?;
    let text = result["content"][0]["text"]
        .as_str()
        .ok_or("tool result has no text content")?;
    let from_text: Value =
        serde_json::from_str(text).map_err(|e| format!("text content is not JSON: {e}"))?;
    if from_text != envelope {
        return Err("text content and structuredContent are different envelopes".into());
    }
    let refused = envelope["status"] == "refused";
    if result["isError"] != json!(refused) {
        return Err(format!(
            "isError is {} for status {}",
            result["isError"], envelope["status"]
        ));
    }
    Ok(envelope)
}

async fn run_via(
    via: Via,
    function: &str,
    input: &Value,
    database: Option<&Path>,
) -> Result<Value, String> {
    match via {
        Via::Direct => {
            let call = Call {
                function: function.into(),
                version: None,
                input: input.clone(),
                database: database.map(Path::to_path_buf),
                caller: "test".into(),
            };
            Ok(catalog::invoke(&call).await.to_value())
        }
        Via::McpServer => {
            let server = Server::new(database.map(Path::to_path_buf));
            let reply = server
                .handle(tool_call_message(function, input))
                .await
                .ok_or("the server did not answer a request")?;
            envelope_from_tool_reply(&reply)
        }
        Via::HttpCalc => {
            let request = Request::post(format!("/v1/calc/{function}"))
                .header("content-type", "application/json")
                .body(Body::from(catalog::canonical(input)))
                .map_err(|e| e.to_string())?;
            let (status, body) = post(loopback_router(database)?, request).await?;
            let envelope: Value = serde_json::from_slice(&body)
                .map_err(|e| format!("HTTP {status}: body is not JSON: {e}"))?;
            let expected_status = if envelope["status"] == "refused" {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::OK
            };
            if status != expected_status {
                return Err(format!(
                    "HTTP {status} for status {}, expected {expected_status}",
                    envelope["status"]
                ));
            }
            Ok(envelope)
        }
        Via::HttpMcp => {
            let body = serde_json::to_vec(&tool_call_message(function, input))
                .map_err(|e| e.to_string())?;
            let request = Request::post("/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(Body::from(body))
                .map_err(|e| e.to_string())?;
            let (status, body) = post(loopback_router(database)?, request).await?;
            if status != StatusCode::OK {
                return Err(format!(
                    "HTTP {status}: {}",
                    String::from_utf8_lossy(&body[..body.len().min(300)])
                ));
            }
            let reply: Value =
                serde_json::from_slice(&body).map_err(|e| format!("body is not JSON: {e}"))?;
            envelope_from_tool_reply(&reply)
        }
    }
}

// ---------------------------------------------------------------------------
// One step: run through every path, compare, check against the golden
// ---------------------------------------------------------------------------

struct StepRun<'a> {
    label: &'a str,
    function: &'a str,
    input: &'a Value,
    /// The pinned expectation, if the corpus has one yet.
    expected: Option<&'a Value>,
    /// Database state before the step; every path gets its own copy.
    base: Option<&'a Path>,
    work: &'a Path,
    captures: &'a BTreeMap<String, String>,
    /// Chains replace generated ids, timestamps and hashes with placeholders.
    placeholders: bool,
    update: bool,
}

#[derive(Default)]
struct StepOutcome {
    failures: Vec<String>,
    /// The direct path's envelope.
    envelope: Option<Value>,
    /// The direct path's database after the step.
    database: Option<PathBuf>,
    /// A golden to write: the pinned one was missing or stale.
    rewritten: Option<Value>,
}

fn stale_help(function: &str) -> String {
    format!(
        "bumping {function}'s version means regenerating its goldens: run `{UPDATE_VAR}=1 cargo test --test exact_conformance` and review the diff"
    )
}

fn check_envelope(
    via: Via,
    envelope: &Value,
    run: &StepRun<'_>,
    schema: &Value,
) -> Result<(), String> {
    let calc = catalog::find(run.function, None).map_err(|e| e.to_string())?;
    let field = |name: &str| envelope.get(name).and_then(Value::as_str);
    let mut problems = Vec::new();
    if field("contract_version") != Some("plat.ops/1") {
        problems.push("contract_version is not plat.ops/1".to_string());
    }
    if field("function") != Some(run.function) {
        problems.push(format!("function is {:?}", envelope.get("function")));
    }
    if field("version") != Some(calc.version) {
        problems.push(format!(
            "version is {:?}, catalog serves {}",
            envelope.get("version"),
            calc.version
        ));
    }
    if field("input_sha256") != Some(digest(&catalog::canonical(run.input)).as_str()) {
        problems.push("input_sha256 is not the SHA-256 of the canonical input".into());
    }
    if field("computed_at").is_none_or(str::is_empty) {
        problems.push("computed_at is missing".into());
    }
    // Per-invocation fields: every call gets its own trace id and citation, so
    // these are checked for shape on each path and never compared across paths
    // (nor are computed_at, trace_store or trace_warning).
    match field("trace_id") {
        Some(id) if is_id(id) => {
            let cite = format!("{}@{}", run.function, calc.version);
            if !field("citation").is_some_and(|c| c.contains(&cite) && c.contains(id)) {
                problems.push(format!("citation does not name {cite} and the trace id"));
            }
        }
        other => problems.push(format!("trace_id is not an id: {other:?}")),
    }
    if !matches!(field("trace_store"), Some("database" | "jsonl" | "none")) {
        problems.push(format!("trace_store is {:?}", envelope.get("trace_store")));
    }
    match envelope.get("status").and_then(Value::as_str) {
        Some("refused") => {
            if envelope.get("result").is_some() || envelope.get("output_sha256").is_some() {
                problems.push("a refusal carries a result or output_sha256".into());
            }
            if envelope["error"]["code"].as_str().is_none_or(str::is_empty) {
                problems.push("a refusal has no error.code".into());
            }
        }
        Some("calculated" | "review_required") => {
            match envelope.get("result") {
                None => problems.push("no result".into()),
                Some(result) => {
                    if field("output_sha256") != Some(digest(&catalog::canonical(result)).as_str())
                    {
                        problems.push(
                            "output_sha256 is not the SHA-256 of the canonical result".into(),
                        );
                    }
                    if let Err(e) = validate(schema, result, "$.result") {
                        problems.push(format!("result does not satisfy the output schema: {e}"));
                    }
                }
            }
            if envelope.get("error").is_some() {
                problems.push("a success carries an error".into());
            }
        }
        other => problems.push(format!("unexpected status {other:?}")),
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{}: {}", via.name(), problems.join("; ")))
    }
}

fn copy_database(from: &Path, to: &Path) -> Result<(), String> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copy {} to {}: {e}", from.display(), to.display()))
}

async fn run_step(run: &StepRun<'_>) -> StepOutcome {
    let mut out = StepOutcome::default();
    let Ok(calc) = catalog::find(run.function, None) else {
        out.failures.push(format!(
            "{}: {} is not in the catalog",
            run.label, run.function
        ));
        return out;
    };
    let schema = calc.output_schema();

    // Run every path against its own copy of the same starting database.
    let mut envelopes: Vec<(Via, Value)> = Vec::new();
    for (index, via) in ALL_VIA.into_iter().enumerate() {
        let database = match run.base {
            None => None,
            Some(base) => {
                let copy = run.work.join(format!("path-{index}.sqlite"));
                // A previous step's copy is replaced.
                let _ = fs::remove_file(&copy);
                if let Err(e) = copy_database(base, &copy) {
                    out.failures.push(format!("{}: {e}", run.label));
                    return out;
                }
                Some(copy)
            }
        };
        match run_via(via, run.function, run.input, database.as_deref()).await {
            Ok(envelope) => {
                if let Err(e) = check_envelope(via, &envelope, run, &schema) {
                    out.failures.push(format!("{}: {e}", run.label));
                }
                if matches!(via, Via::Direct) {
                    out.database = database;
                }
                envelopes.push((via, envelope));
            }
            Err(e) => {
                out.failures
                    .push(format!("{}: {}: {e}", run.label, via.name()));
                return out;
            }
        }
    }
    let direct = envelopes[0].1.clone();
    out.envelope = Some(direct.clone());

    // Pin down what the golden says, if there is one.
    let served = direct["version"].as_str().unwrap_or("").to_string();
    let pinned = run.expected.and_then(|e| e["version"].as_str());
    let stale = run.expected.is_some() && pinned != Some(served.as_str());
    // Does the direct result fit the golden? Reported once, below. Without a
    // fitting golden, the one this run would write stands in as the template,
    // so generated ids still compare by shape across paths.
    let generated = generated_golden(&direct, run);
    let usable = run.expected.filter(|_| !stale);
    let golden_mismatch = match (usable.and_then(|e| e.get("result")), direct.get("result")) {
        (Some(want), Some(got)) => normalize(want, got, "$.result", run.captures).err(),
        _ => None,
    };
    let template = match (usable, &golden_mismatch) {
        (Some(expected), None) => expected.get("result"),
        _ => generated.get("result"),
    };

    // Identical envelope fields across all paths, ids and timestamps masked.
    let masked = |envelope: &Value| -> Result<Option<Value>, String> {
        match (envelope.get("result"), template) {
            (Some(result), Some(template)) => {
                normalize(template, result, "$.result", run.captures).map(Some)
            }
            (result, _) => Ok(result.cloned()),
        }
    };
    let direct_masked = masked(&direct).unwrap_or_else(|_| direct.get("result").cloned());
    for (via, envelope) in &envelopes[1..] {
        let tag = format!(
            "{}: {} differs from {}",
            run.label,
            via.name(),
            Via::Direct.name()
        );
        for key in [
            "contract_version",
            "function",
            "version",
            "producer",
            "status",
            "error",
            "input_sha256",
        ] {
            if envelope.get(key) != direct.get(key) {
                out.failures.push(format!(
                    "{tag} in {key}: {:?} vs {:?}",
                    envelope.get(key),
                    direct.get(key)
                ));
            }
        }
        match masked(envelope) {
            Ok(other) if other == direct_masked => {}
            Ok(other) => out
                .failures
                .push(format!("{tag} in result: {other:?} vs {direct_masked:?}")),
            Err(e) => out
                .failures
                .push(format!("{tag}: result does not fit the same shape at {e}")),
        }
        // Equal results must hash equally (results with fresh ids may differ).
        if envelope.get("result") == direct.get("result")
            && envelope.get("output_sha256") != direct.get("output_sha256")
        {
            out.failures
                .push(format!("{tag} in output_sha256 for an identical result"));
        }
    }

    let Some(expected) = run.expected else {
        if run.update {
            if out.failures.is_empty() {
                out.rewritten = Some(generated);
            }
        } else {
            out.failures.push(format!(
                "{}: no golden. Create it with `{UPDATE_VAR}=1 cargo test --test exact_conformance` and review it",
                run.label
            ));
        }
        return out;
    };
    if stale {
        if run.update {
            if out.failures.is_empty() {
                out.rewritten = Some(generated);
            }
        } else {
            out.failures.push(format!(
                "{}: STALE GOLDEN. It pins {} {:?} but the catalog serves {served}; {}",
                run.label,
                run.function,
                pinned.unwrap_or("(none)"),
                stale_help(run.function)
            ));
        }
        return out;
    }
    let before = out.failures.len();
    if let Some(e) = golden_mismatch {
        out.failures.push(format!(
            "{}: result differs from the golden at {e}",
            run.label
        ));
    }
    if direct["status"] != expected["status"] {
        out.failures.push(format!(
            "{}: status is {}, golden says {}",
            run.label, direct["status"], expected["status"]
        ));
    }
    if direct["error"]["code"] != expected["error"]["code"] {
        out.failures.push(format!(
            "{}: error.code is {}, golden says {}",
            run.label, direct["error"]["code"], expected["error"]["code"]
        ));
    }
    if let Some(want) = expected.get("output_sha256").and_then(Value::as_str) {
        let got = direct.get("output_sha256").and_then(Value::as_str);
        let fits = match placeholder_fits(want, &json!(got), run.captures) {
            Some(fit) => fit.is_ok(),
            None => got == Some(want),
        };
        if !fits {
            out.failures.push(format!(
                "{}: output_sha256 is {got:?}, golden says {want}",
                run.label
            ));
        }
    }
    if expected.get("result").is_some() != direct.get("result").is_some() {
        out.failures.push(format!(
            "{}: golden and engine disagree on whether there is a result",
            run.label
        ));
    }
    if out.failures.len() > before {
        out.failures.push(format!(
            "{}: {} {served} is still the version this golden pins, so a changed value is a regression unless the version is bumped in catalog.rs; {}",
            run.label,
            run.function,
            stale_help(run.function)
        ));
    }
    out
}

/// The golden for what the direct path just returned.
fn generated_golden(envelope: &Value, run: &StepRun<'_>) -> Value {
    let mut golden = Map::new();
    golden.insert("status".into(), envelope["status"].clone());
    golden.insert("version".into(), envelope["version"].clone());
    match (envelope.get("result"), envelope.get("error")) {
        (Some(result), _) => {
            let result = if run.placeholders {
                placeholderize(result, "", run.captures)
            } else {
                result.clone()
            };
            golden.insert("result".into(), result);
            let hash = if run.placeholders {
                json!("<sha256>")
            } else {
                envelope["output_sha256"].clone()
            };
            golden.insert("output_sha256".into(), hash);
        }
        (None, Some(error)) => {
            golden.insert("error".into(), json!({"code": error["code"]}));
        }
        (None, None) => {}
    }
    Value::Object(golden)
}

/// A golden's own shape: known keys, and `result` xor `error`.
fn golden_problems(label: &str, expected: &Value) -> Vec<String> {
    let Some(object) = expected.as_object() else {
        return vec![format!("{label}: expected must be a JSON object")];
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|k| !EXPECTED_KEYS.contains(&k.as_str()))
        .map(|k| format!("{label}: unknown key {k:?} in expected (allowed: {EXPECTED_KEYS:?})"))
        .collect();
    let status = object.get("status").and_then(Value::as_str);
    if !matches!(status, Some("calculated" | "review_required" | "refused")) {
        problems.push(format!(
            "{label}: expected.status must be calculated, review_required or refused"
        ));
    }
    if object.get("version").and_then(Value::as_str).is_none() {
        problems.push(format!(
            "{label}: expected.version is required; it pins the function version"
        ));
    }
    match (
        status,
        object.contains_key("result"),
        object.get("error").and_then(|e| e["code"].as_str()),
    ) {
        (Some("refused"), false, Some(_)) => {}
        (Some("calculated" | "review_required"), true, None) => {}
        _ => problems.push(format!(
            "{label}: a refusal needs error.code and no result; any other status needs a result"
        )),
    }
    problems
}

// ---------------------------------------------------------------------------
// Cases and chains
// ---------------------------------------------------------------------------

async fn fresh_database(path: &Path) -> Result<(), String> {
    store::create(path)
        .await
        .map_err(|e| format!("create {}: {e}", path.display()))?
        .close()
        .await;
    Ok(())
}

async fn run_case(case: &Case, update: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let input = match read_json(&case.input_path) {
        Ok(input) if input.is_object() => input,
        Ok(_) => return vec![format!("{}: input must be a JSON object", case.id)],
        Err(e) => return vec![e],
    };
    let expected = if case.expected_path.is_file() {
        match read_json(&case.expected_path) {
            Ok(expected) => {
                failures.extend(golden_problems(&case.id, &expected));
                Some(expected)
            }
            Err(e) => return vec![e],
        }
    } else {
        None
    };
    let work = match tempfile::tempdir() {
        Ok(work) => work,
        Err(e) => return vec![format!("{}: {e}", case.id)],
    };
    let Ok(calc) = catalog::find(&case.function, None) else {
        return vec![format!(
            "{}: {} is not in the catalog",
            case.id, case.function
        )];
    };
    // Stateless functions get no database at all. Stateful ones get a
    // freshly created, empty one.
    let base = work.path().join("base.sqlite");
    let database = if calc.stateful {
        if let Err(e) = fresh_database(&base).await {
            return vec![format!("{}: {e}", case.id)];
        }
        Some(base.as_path())
    } else {
        None
    };
    let captures = BTreeMap::new();
    let outcome = run_step(&StepRun {
        label: &case.id,
        function: &case.function,
        input: &input,
        expected: expected.as_ref(),
        base: database,
        work: work.path(),
        captures: &captures,
        placeholders: false,
        update,
    })
    .await;
    failures.extend(outcome.failures);
    if let Some(golden) = outcome.rewritten {
        match write_json(&case.expected_path, &golden) {
            Ok(()) => eprintln!("{UPDATE_VAR}: wrote {}", case.expected_path.display()),
            Err(e) => failures.push(e),
        }
    }
    failures
}

/// Replace every string exactly equal to `${name}` with the captured id.
fn substitute(value: &Value, captures: &BTreeMap<String, String>) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => match text.strip_prefix("${").and_then(|t| t.strip_suffix('}')) {
            Some(name) => json!(captures
                .get(name)
                .ok_or_else(|| format!("${{{name}}} is used before any step captured it"))?),
            None => value.clone(),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| substitute(v, captures))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(k, v)| substitute(v, captures).map(|v| (k.clone(), v)))
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// Run an ordered list of steps against one evolving database. The direct
/// path's database carries the state forward; the other paths run each step
/// against copies of the same starting file, so every path sees the same ids.
async fn run_chain(chain: &Chain, update: bool) -> Vec<String> {
    let mut doc = match read_json(&chain.path) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let Some(steps) = doc["steps"].as_array().cloned().filter(|s| !s.is_empty()) else {
        return vec![format!("{}: steps must be a non-empty array", chain.id)];
    };
    let work = match tempfile::tempdir() {
        Ok(work) => work,
        Err(e) => return vec![format!("{}: {e}", chain.id)],
    };
    let base = work.path().join("base.sqlite");
    if let Err(e) = fresh_database(&base).await {
        return vec![format!("{}: {e}", chain.id)];
    }
    let mut failures = Vec::new();
    let mut captures: BTreeMap<String, String> = BTreeMap::new();
    let mut rewritten = false;
    for (index, step) in steps.iter().enumerate() {
        let name = step["name"].as_str().unwrap_or("(unnamed)");
        let label = format!("{} step {} ({name})", chain.id, index + 1);
        let Some(function) = step["function"].as_str() else {
            failures.push(format!("{label}: function is required"));
            break;
        };
        let input = match substitute(&step["input"], &captures) {
            Ok(input) if input.is_object() => input,
            Ok(_) => {
                failures.push(format!("{label}: input must be a JSON object"));
                break;
            }
            Err(e) => {
                failures.push(format!("{label}: {e}"));
                break;
            }
        };
        let expected = step.get("expected");
        if let Some(expected) = expected {
            failures.extend(golden_problems(&label, expected));
        }
        let outcome = run_step(&StepRun {
            label: &label,
            function,
            input: &input,
            expected,
            base: Some(&base),
            work: work.path(),
            captures: &captures,
            placeholders: true,
            update,
        })
        .await;
        failures.extend(outcome.failures);
        let (Some(envelope), Some(database)) = (outcome.envelope, outcome.database) else {
            break;
        };
        if let Some(golden) = outcome.rewritten {
            doc["steps"][index]["expected"] = golden;
            rewritten = true;
        }
        if let Err(e) = copy_database_over(&database, &base) {
            failures.push(format!("{label}: {e}"));
            break;
        }
        for (capture, pointer) in step["capture"].as_object().into_iter().flatten() {
            let id = pointer
                .as_str()
                .and_then(|p| envelope.pointer(p))
                .and_then(Value::as_str);
            match id {
                Some(id) => {
                    captures.insert(capture.clone(), id.to_string());
                }
                None => failures.push(format!(
                    "{label}: capture {capture} found nothing at {pointer}"
                )),
            }
        }
    }
    if rewritten {
        match write_json(&chain.path, &doc) {
            Ok(()) => eprintln!("{UPDATE_VAR}: wrote {}", chain.path.display()),
            Err(e) => failures.push(e),
        }
    }
    failures
}

fn copy_database_over(from: &Path, to: &Path) -> Result<(), String> {
    fs::remove_file(to).map_err(|e| format!("{}: {e}", to.display()))?;
    copy_database(from, to)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_case_agrees_across_catalog_mcp_and_http() {
    let corpus = discover();
    let update = updating();
    assert!(
        !corpus.cases.is_empty() && !corpus.chains.is_empty(),
        "the corpus at {} is empty",
        corpus_root().display()
    );
    let mut failures = Vec::new();
    for case in &corpus.cases {
        failures.extend(run_case(case, update).await);
    }
    for chain in &corpus.chains {
        failures.extend(run_chain(chain, update).await);
    }
    assert!(
        failures.is_empty(),
        "{} conformance failure(s) over {} case(s) and {} chain(s):\n  - {}\n",
        failures.len(),
        corpus.cases.len(),
        corpus.chains.len(),
        failures.join("\n  - ")
    );
}

#[test]
fn every_catalog_function_has_a_case() {
    let corpus = discover();
    let covered = covered_functions(&corpus);
    let missing: Vec<&str> = CATALOG
        .iter()
        .map(|c| c.name)
        .filter(|name| !covered.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "catalog function(s) with no conformance case: {}. Add tests/fixtures/catalog/<function>/<case>.input.json (see that directory's README), then create the golden with {UPDATE_VAR}=1",
        missing.join(", ")
    );
}

#[test]
fn corpus_layout_is_well_formed() {
    let corpus = discover();
    assert!(
        corpus.problems.is_empty(),
        "corpus layout problems:\n  - {}",
        corpus.problems.join("\n  - ")
    );
    // Every chain step names a catalog function and every capture is used
    // after it is defined.
    for chain in &corpus.chains {
        let doc = read_json(&chain.path).unwrap_or_else(|e| panic!("{e}"));
        let mut defined: BTreeSet<String> = BTreeSet::new();
        for (index, step) in doc["steps"].as_array().into_iter().flatten().enumerate() {
            let label = format!("{} step {}", chain.id, index + 1);
            let function = step["function"].as_str().unwrap_or("");
            assert!(
                CATALOG.iter().any(|c| c.name == function),
                "{label}: {function:?} is not a catalog function"
            );
            let known: BTreeMap<String, String> =
                defined.iter().map(|n| (n.clone(), String::new())).collect();
            substitute(&step["input"], &known).unwrap_or_else(|e| panic!("{label}: {e}"));
            for name in step["capture"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, _)| k)
            {
                defined.insert(name.clone());
            }
        }
    }
}

#[test]
fn schema_validator_reports_the_path_that_does_not_conform() {
    let schema = json!({
        "type": "object",
        "required": ["id", "tags"],
        "properties": {
            "id": {"type": "integer", "minimum": 1},
            "name": {"type": ["string", "null"]},
            "state": {"enum": ["open", "closed"]},
            "kind": {"const": "box"},
            "ok": {"type": "boolean"},
            "tags": {"type": "array", "items": {"type": "object", "required": ["k"],
                      "properties": {"k": {"type": "string"}}}},
            "either": {"anyOf": [{"type": "string"}, {"type": "null"}]},
            "one": {"oneOf": [{"type": "string"}, {"type": "string", "enum": ["a"]}]},
            "money": {"type": "object", "additionalProperties": {"type": "string"}}
        }
    });
    let good = json!({"id": 3, "name": null, "state": "open", "kind": "box", "ok": true,
                      "tags": [{"k": "x"}], "either": null, "money": {"a": "1.00"}});
    assert_eq!(validate(&schema, &good, "$"), Ok(()));

    let cases = [
        (json!({"tags": []}), "$: missing required key \"id\""),
        (
            json!({"id": 1.5, "tags": []}),
            "$.id: expected type integer, got number",
        ),
        (
            json!({"id": 0, "tags": []}),
            "$.id: 0 is below the minimum 1",
        ),
        (
            json!({"id": 1, "tags": [], "name": 4}),
            "$.name: expected type string or null, got number",
        ),
        (json!({"id": 1, "tags": [], "state": "ajar"}), "$.state:"),
        (
            json!({"id": 1, "tags": [], "kind": "bag"}),
            "$.kind: expected const",
        ),
        (
            json!({"id": 1, "tags": [{"k": "x"}, {"k": 2}]}),
            "$.tags[1].k: expected type string, got number",
        ),
        (
            json!({"id": 1, "tags": [], "either": 7}),
            "$.either: matches none of anyOf",
        ),
        (
            json!({"id": 1, "tags": [], "one": "a"}),
            "$.one: matches 2 oneOf branches",
        ),
        (
            json!({"id": 1, "tags": [], "money": {"a": 5}}),
            "$.money.a: expected type string, got number",
        ),
        (
            json!({"id": 1, "tags": "none"}),
            "$.tags: expected type array, got string",
        ),
    ];
    for (value, want) in cases {
        let error = validate(&schema, &value, "$").unwrap_err();
        assert!(
            error.contains(want),
            "{value}: {error} does not contain {want}"
        );
    }
}

#[test]
fn placeholders_match_by_shape_and_everything_else_exactly() {
    let captures = BTreeMap::from([("rev1".to_string(), "abc-123".to_string())]);
    let expected = json!({
        "revision_id": "<id=rev1>", "other": "<id>", "at": "<timestamp>",
        "hash": "<sha256>", "money": "10.05", "list": [{"parent_id": null}]
    });
    let actual = json!({
        "revision_id": "abc-123", "other": "zzz", "at": "2026-10-11T15:00:00+00:00",
        "hash": "a".repeat(64), "money": "10.05", "list": [{"parent_id": null}]
    });
    assert_eq!(
        normalize(&expected, &actual, "$", &captures),
        Ok(expected.clone())
    );
    for (key, bad) in [
        ("revision_id", json!("other-id")),
        ("other", json!("")),
        ("at", json!("yesterday")),
        ("hash", json!("abc")),
        ("money", json!("10.06")),
        ("list", json!([])),
    ] {
        let mut broken = actual.clone();
        broken[key] = bad;
        assert!(
            normalize(&expected, &broken, "$", &captures)
                .unwrap_err()
                .starts_with(&format!("$.{key}")),
            "{key}"
        );
    }
}

#[test]
fn generated_goldens_use_the_same_placeholders_they_are_checked_with() {
    let captures = BTreeMap::from([("rev1".to_string(), "abc-123".to_string())]);
    let result = json!({
        "revision_id": "abc-123", "report_id": "r-9", "issued_at": "2026-10-11T15:00:00+00:00",
        "input_sha256": "f".repeat(64), "variance": {"actual": "1.00"},
        "revisions": [{"id": "abc-123", "parent_id": null}],
        "trace": {"trace_id": "abc-123", "started_at": "2026-10-11T15:00:00+00:00",
                  "output_sha256": "e".repeat(64), "producer_version": "0.2.0", "caller": "test"}
    });
    let golden = placeholderize(&result, "", &captures);
    assert_eq!(golden["revision_id"], "<id=rev1>");
    assert_eq!(golden["report_id"], "<id>");
    assert_eq!(golden["issued_at"], "<timestamp>");
    assert_eq!(golden["input_sha256"], "<sha256>");
    assert_eq!(golden["variance"]["actual"], "1.00");
    assert_eq!(golden["revisions"][0]["id"], "<id=rev1>");
    assert_eq!(golden["revisions"][0]["parent_id"], Value::Null);
    assert_eq!(golden["trace"]["trace_id"], "<id=rev1>");
    assert_eq!(golden["trace"]["started_at"], "<timestamp>");
    assert_eq!(golden["trace"]["output_sha256"], "<sha256>");
    assert_eq!(golden["trace"]["producer_version"], "<string>");
    assert_eq!(golden["trace"]["caller"], "test");
    assert!(normalize(&golden, &result, "$", &captures).is_ok());
}
