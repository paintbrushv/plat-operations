//! Model Context Protocol server over stdio for the calculation catalog.
//!
//! JSON-RPC 2.0, one JSON message per line on stdin, one per line on stdout.
//! Hand-rolled on `serde_json` and tokio I/O so the exact-cent binary gains no
//! dependency. The adapter is deliberately thin:
//!
//! - every tool is generated from [`catalog::CATALOG`]; tool arguments are the
//!   catalog input and nothing else;
//! - the database path comes from the server's own command line, never from a
//!   tool argument, and no tool accepts a path, SQL or network target;
//! - every tool result is the catalog [`catalog::Envelope`] unchanged, so the
//!   function, version, producer and hashes travel with every number, and
//!   engine refusals reach the model as typed refusals (`isError: true`);
//! - the server never writes artifacts of its own and never prints anything
//!   but JSON-RPC to stdout. Diagnostics go to stderr only when
//!   `BOXSCORE_MCP_DEBUG` is set, and they name methods, never content.
use super::{catalog, protocol::MAX_REQUEST_BYTES};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// The protocol revision this server speaks by default.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions accepted from a client's `initialize`; anything else is answered
/// with [`PROTOCOL_VERSION`] so the client can decide whether to continue.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26"];
pub const SERVER_NAME: &str = "boxscore-exact";
pub const PROMPT_NAME: &str = "boxscore_grounding";
/// The analyst entry-point prompt, served beside the grounding prompt.
pub const START_HERE_PROMPT_NAME: &str = "start_here";

/// The three questions an analyst asks first. This is the single source: the
/// `start_here` prompt quotes them, and a test fails if the fenced block under
/// the `start-here-questions` marker in `START_HERE.md` says anything else.
pub const START_HERE_QUESTIONS: [&str; 3] = [
    "What is loaded?",
    "Why did NOI miss budget for Sample Gardens in 2026-05?",
    "What does review_required mean here?",
];

/// The grounding rule, verbatim, on every tool description, in the
/// `initialize` instructions and in the grounding prompt.
pub const GROUNDING_RULE: &str = "Numbers come from the engine. If the tool refused, say so and ask; never compute, round or restate a figure the engine did not return. Cite function@version, the trace_id and the output_sha256 with every number.";

const SCOPE: &str = "The boxscore-exact catalog runs named, versioned calculations over checked integer USD cents. It can list itself (describe), compute an actual-versus-budget NOI bridge from inline GL rows (noi_variance), compute physical occupancy (occupancy), economic occupancy (economic_occupancy) and delinquency (delinquency) as exact ratios from an inline snapshot or a stored revision, and, when the server was started with a database, store a canonical period dataset as an immutable revision (import_period), review a stored revision with its review reasons, excluded accounts and changes versus the superseded revision (review_period), issue an immutable report record (issue_report), list revisions with their issued reports (list_revisions), report per-property close readiness for a period (close_readiness), build a trailing twelve-period statement for a property (t12) and read back a recorded invocation by trace_id (get_provenance). It cannot read files, URLs or SQL, cannot infer vendor export layouts, signs, account mappings or currency, does not forecast or value, computes no rate or ratio beyond those functions, and never grants human approval: every review and report says human_approval not_granted, and review flags stay visible until a person resolves them.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Configuration shared by every request. The database is fixed at start-up.
#[derive(Debug, Clone, Default)]
pub struct Server {
    database: Option<PathBuf>,
    /// Recorded as the trace caller of every tool call: `"mcp"` over stdio,
    /// `"http:<subject>"` over HTTP.
    caller: String,
}

impl Server {
    pub fn new(database: Option<PathBuf>) -> Self {
        Self {
            database,
            caller: "mcp".into(),
        }
    }

    /// The same server, recording `caller` as the trace caller.
    pub fn with_caller(&self, caller: impl Into<String>) -> Self {
        Self {
            database: self.database.clone(),
            caller: caller.into(),
        }
    }

    /// Handle one raw line (without its newline). `None` means the line was a
    /// notification, a client response or blank, and nothing is written back.
    pub async fn handle_line(&self, line: &[u8]) -> Option<Value> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        if line.len() > MAX_REQUEST_BYTES {
            return Some(oversized());
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(message) => self.handle(message).await,
            Err(_) => Some(failure(Value::Null, PARSE_ERROR, "Parse error")),
        }
    }

    /// Handle one parsed JSON-RPC message.
    pub async fn handle(&self, message: Value) -> Option<Value> {
        let Value::Object(mut message) = message else {
            // Batches were removed in 2025-06-18; a bare scalar is not a message.
            return Some(failure(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request: expected one JSON-RPC object per line",
            ));
        };
        let id = message.remove("id");
        let method = message.remove("method");
        let params = message.remove("params");
        let valid_version = message.get("jsonrpc").and_then(Value::as_str) == Some("2.0");

        let Some(method) = method else {
            // A response to a request we never send, or garbage. Responses
            // carry an id with result/error; answer only malformed requests.
            let is_response = message.contains_key("result") || message.contains_key("error");
            return match id {
                Some(_) if is_response => None,
                Some(id) => Some(failure(
                    valid_id(&id).unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "Invalid Request: missing method",
                )),
                None => None,
            };
        };

        let Some(id) = id else {
            // Notification: never answered, whatever it names.
            debug(&format!("notification {}", method.as_str().unwrap_or("?")));
            return None;
        };
        let Some(id) = valid_id(&id) else {
            return Some(failure(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request: id must be a string or a number",
            ));
        };
        if !valid_version {
            return Some(failure(
                id,
                INVALID_REQUEST,
                "Invalid Request: jsonrpc must be \"2.0\"",
            ));
        }
        let Value::String(method) = method else {
            return Some(failure(
                id,
                INVALID_REQUEST,
                "Invalid Request: method must be a string",
            ));
        };
        let params = match params {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(params)) => params,
            Some(_) => {
                return Some(failure(
                    id,
                    INVALID_PARAMS,
                    "Invalid params: params must be an object",
                ))
            }
        };
        debug(&format!("request {method}"));
        Some(match self.dispatch(&method, params).await {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => failure(id, code, &message),
        })
    }

    async fn dispatch(
        &self,
        method: &str,
        params: Map<String, Value>,
    ) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => self.call_tool(params).await,
            "prompts/list" => Ok(json!({"prompts": prompts()})),
            "prompts/get" => get_prompt(&params),
            _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
        }
    }

    async fn call_tool(&self, mut params: Map<String, Value>) -> Result<Value, (i64, String)> {
        let name = match params.remove("name") {
            Some(Value::String(name)) => name,
            _ => return Err(invalid_params("tools/call needs a string name")),
        };
        // Only catalog functions are tools; anything else is a protocol error,
        // not an engine refusal, because the tool was never offered.
        if !catalog::CATALOG.iter().any(|c| c.name == name) {
            return Err(invalid_params(&format!("Unknown tool: {name}")));
        }
        let input = match params.remove("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::Object(arguments)) => Value::Object(arguments),
            Some(_) => return Err(invalid_params("tools/call arguments must be an object")),
        };
        let call = catalog::Call {
            function: name,
            version: None,
            input,
            // The host's configured database, never anything from arguments.
            database: self.database.clone(),
            caller: self.caller.clone(),
        };
        let envelope = catalog::invoke(&call).await;
        let refused = envelope.refused();
        let structured = envelope.to_value();
        let text = serde_json::to_string(&structured)
            .unwrap_or_else(|_| String::from(r#"{"status":"refused"}"#));
        Ok(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": structured,
            "isError": refused,
        }))
    }
}

fn initialize(params: &Map<String, Value>) -> Result<Value, (i64, String)> {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("initialize needs a string protocolVersion"))?;
    let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };
    Ok(json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}, "prompts": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
        "instructions": instructions(),
    }))
}

/// The `initialize` instructions: the grounding rule first, then scope.
pub fn instructions() -> String {
    format!("{GROUNDING_RULE}\n\n{SCOPE}")
}

/// One MCP tool per catalog function.
pub fn tools() -> Vec<Value> {
    catalog::CATALOG
        .iter()
        .map(|calc| {
            json!({
                "name": calc.name,
                "title": format!("{}@{}", calc.name, calc.version),
                "description": format!(
                    "{} ({}@{}{})\n\n{GROUNDING_RULE}",
                    calc.summary,
                    calc.name,
                    calc.version,
                    if calc.stateful {
                        "; needs the database configured on the server"
                    } else {
                        ""
                    },
                ),
                "inputSchema": calc.input_schema(),
            })
        })
        .collect()
}

/// Every prompt the server offers, in the order `prompts/list` returns them.
pub fn prompts() -> Vec<Value> {
    vec![
        json!({
            "name": PROMPT_NAME,
            "title": "Boxscore grounding rule",
            "description": "How to use boxscore-exact numbers: the grounding rule and what the catalog can and cannot do.",
            "arguments": [],
        }),
        json!({
            "name": START_HERE_PROMPT_NAME,
            "title": "Start here",
            "description": "First session for an analyst: show what is loaded, then offer three questions to start with. Every number cited.",
            "arguments": [],
        }),
    ]
}

/// The grounding prompt text: the rule verbatim plus the catalog's scope.
pub fn prompt_text() -> String {
    format!("{GROUNDING_RULE}\n\n{SCOPE}")
}

/// The `start_here` prompt text: what to call, how to summarize it, the three
/// questions from [`START_HERE_QUESTIONS`] verbatim, and the grounding rule.
pub fn start_here_text() -> String {
    let questions = START_HERE_QUESTIONS
        .iter()
        .map(|q| format!("- {q}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "I am an analyst using boxscore-exact for the first time. Get me started.\n\n\
         1. Call describe. Then call list_revisions with no arguments (no property or period filter), so you see everything stored.\n\
         2. Summarize what is loaded, in a few short lines: each property, its periods, the latest revision number for each property and period, and whether a report has been issued for that revision (its reports list is empty when none has). If nothing is stored, say so. If list_revisions refused (for example DATABASE_REQUIRED, when the server has no database), say so and name the error code; do not guess what is loaded.\n\
         3. Offer me these three questions to start with, word for word:\n\
         {questions}\n\
         If Sample Gardens 2026-05 is not loaded, say so and offer the second question for a property and period that is.\n\
         4. Use only the tools for numbers. review_required is not an error: the numbers are returned, and a person should look at the review reasons before relying on them.\n\n\
         {GROUNDING_RULE}"
    )
}

fn get_prompt(params: &Map<String, Value>) -> Result<Value, (i64, String)> {
    match params.get("name").and_then(Value::as_str) {
        Some(PROMPT_NAME) => Ok(json!({
            "description": "Grounding rule for boxscore-exact results",
            "messages": [{"role": "user", "content": {"type": "text", "text": prompt_text()}}],
        })),
        Some(START_HERE_PROMPT_NAME) => Ok(json!({
            "description": "Start here: what is loaded, and three questions to start with",
            "messages": [{"role": "user", "content": {"type": "text", "text": start_here_text()}}],
        })),
        Some(other) => Err(invalid_params(&format!("Unknown prompt: {other}"))),
        None => Err(invalid_params("prompts/get needs a string name")),
    }
}

fn invalid_params(message: &str) -> (i64, String) {
    (INVALID_PARAMS, format!("Invalid params: {message}"))
}

/// JSON-RPC ids are strings or numbers. MCP forbids null; so do we.
fn valid_id(id: &Value) -> Option<Value> {
    match id {
        Value::String(_) | Value::Number(_) => Some(id.clone()),
        _ => None,
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn oversized() -> Value {
    failure(
        Value::Null,
        INVALID_REQUEST,
        "Invalid Request: message exceeds 2 MiB",
    )
}

fn debug(message: &str) {
    if std::env::var_os("BOXSCORE_MCP_DEBUG").is_some() {
        eprintln!("boxscore-exact mcp: {message}");
    }
}

enum Frame {
    Line,
    Oversized,
    Eof,
}

/// Read one newline-terminated frame into `buf`, holding at most
/// `MAX_REQUEST_BYTES` of it. A longer line is drained chunk by chunk and
/// discarded, so an oversized message never occupies more than the cap plus
/// the reader's buffer.
async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Frame> {
    buf.clear();
    let mut seen = false;
    let mut oversized = false;
    loop {
        let (consumed, done) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(match (seen, oversized) {
                    (false, _) => Frame::Eof,
                    (true, true) => Frame::Oversized,
                    (true, false) => Frame::Line,
                });
            }
            seen = true;
            let (chunk, consumed, done) = match available.iter().position(|b| *b == b'\n') {
                Some(end) => (&available[..end], end + 1, true),
                None => (available, available.len(), false),
            };
            if !oversized {
                if buf.len() + chunk.len() > MAX_REQUEST_BYTES {
                    oversized = true;
                    buf.clear();
                } else {
                    buf.extend_from_slice(chunk);
                }
            }
            (consumed, done)
        };
        reader.consume(consumed);
        if done {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(if oversized {
                Frame::Oversized
            } else {
                Frame::Line
            });
        }
    }
}

/// Serve until the reader reaches end of input. Requests are answered in
/// order, one line each; a bad request is answered and the loop continues.
pub async fn serve<R, W>(server: &Server, reader: R, mut writer: W) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut buf = Vec::new();
    loop {
        let response = match read_frame(&mut reader, &mut buf).await? {
            Frame::Eof => break,
            Frame::Oversized => Some(oversized()),
            Frame::Line => server.handle_line(&buf).await,
        };
        if let Some(response) = response {
            let mut bytes = serde_json::to_vec(&response).unwrap_or_else(|_| {
                br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#
                    .to_vec()
            });
            bytes.push(b'\n');
            writer.write_all(&bytes).await?;
            writer.flush().await?;
        }
    }
    writer.flush().await
}

/// Serve on the process's stdin and stdout until stdin closes.
pub async fn run_stdio(database: Option<PathBuf>) -> std::io::Result<()> {
    let server = Server::new(database);
    let reader = tokio::io::BufReader::new(tokio::io::stdin());
    serve(&server, reader, tokio::io::stdout()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: Value, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[tokio::test]
    async fn dispatcher_answers_requests_and_ignores_notifications() {
        let server = Server::new(None);
        let init = server
            .handle(request(
                json!(1),
                "initialize",
                json!({"protocolVersion": "2025-03-26", "capabilities": {},
                       "clientInfo": {"name": "t", "version": "0"}}),
            ))
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(
            init["result"]["capabilities"],
            json!({"tools": {}, "prompts": {}})
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "boxscore-exact");
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with(GROUNDING_RULE));

        let future = server
            .handle(request(
                json!(2),
                "initialize",
                json!({"protocolVersion": "2099-01-01"}),
            ))
            .await
            .unwrap();
        assert_eq!(future["result"]["protocolVersion"], PROTOCOL_VERSION);

        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(server.handle(note).await.is_none());
        let unknown_note = json!({"jsonrpc": "2.0", "method": "no/such"});
        assert!(server.handle(unknown_note).await.is_none());

        let tools = server
            .handle(request(json!("t"), "tools/list", json!({})))
            .await
            .unwrap();
        assert_eq!(tools["id"], "t");
        assert_eq!(
            tools["result"]["tools"].as_array().unwrap().len(),
            catalog::CATALOG.len()
        );

        let missing = server
            .handle(request(json!(3), "resources/list", json!({})))
            .await
            .unwrap();
        assert_eq!(missing["error"]["code"], METHOD_NOT_FOUND);

        let no_name = server
            .handle(request(json!(4), "tools/call", json!({})))
            .await
            .unwrap();
        assert_eq!(no_name["error"]["code"], INVALID_PARAMS);

        let not_a_tool = server
            .handle(request(json!(5), "tools/call", json!({"name": "cap_rate"})))
            .await
            .unwrap();
        assert_eq!(not_a_tool["error"]["code"], INVALID_PARAMS);

        let null_id = server
            .handle(json!({"jsonrpc": "2.0", "id": null, "method": "ping"}))
            .await
            .unwrap();
        assert_eq!(null_id["error"]["code"], INVALID_REQUEST);
        assert_eq!(null_id["id"], Value::Null);

        let batch = server.handle(json!([{"jsonrpc": "2.0"}])).await.unwrap();
        assert_eq!(batch["error"]["code"], INVALID_REQUEST);

        let parse = server.handle_line(b"{not json").await.unwrap();
        assert_eq!(parse["error"]["code"], PARSE_ERROR);
        assert!(server.handle_line(b"   ").await.is_none());

        let response = json!({"jsonrpc": "2.0", "id": 9, "result": {}});
        assert!(server.handle(response).await.is_none());
    }

    #[tokio::test]
    async fn tool_calls_return_the_envelope_and_never_take_a_database_from_arguments() {
        let server = Server::new(None);
        let described = server
            .handle(request(
                json!(1),
                "tools/call",
                json!({"name": "describe", "arguments": {}}),
            ))
            .await
            .unwrap();
        let result = &described["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["function"], "describe");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);

        let smuggled = server
            .handle(request(
                json!(2),
                "tools/call",
                json!({"name": "list_revisions", "arguments": {"database": "/tmp/x.sqlite"}}),
            ))
            .await
            .unwrap();
        assert_eq!(smuggled["result"]["isError"], true);
        assert_eq!(
            smuggled["result"]["structuredContent"]["error"]["code"],
            "INVALID_INPUT"
        );

        let no_db = server
            .handle(request(
                json!(3),
                "tools/call",
                json!({"name": "list_revisions", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(
            no_db["result"]["structuredContent"]["error"]["code"],
            "DATABASE_REQUIRED"
        );
    }

    #[tokio::test]
    async fn serve_refuses_oversized_lines_and_keeps_going() {
        let mut input = Vec::new();
        input.extend(std::iter::repeat_n(b'x', 3 * 1024 * 1024));
        input.push(b'\n');
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\r\n");
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}");
        let mut output = Vec::new();
        serve(&Server::new(None), input.as_slice(), &mut output)
            .await
            .unwrap();
        let lines: Vec<Value> = output
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(lines[1], json!({"jsonrpc": "2.0", "id": 7, "result": {}}));
        assert_eq!(lines[2]["id"], 8);
    }

    #[tokio::test]
    async fn prompts_list_both_and_get_start_here() {
        let server = Server::new(None);
        let listed = server
            .handle(request(json!(1), "prompts/list", json!({})))
            .await
            .unwrap();
        let prompts = listed["result"]["prompts"].as_array().unwrap();
        let names: Vec<&str> = prompts
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, [PROMPT_NAME, START_HERE_PROMPT_NAME]);
        for prompt in prompts {
            assert!(!prompt["title"].as_str().unwrap().is_empty());
            assert!(!prompt["description"].as_str().unwrap().is_empty());
            assert_eq!(prompt["arguments"], json!([]));
        }

        let got = server
            .handle(request(
                json!(2),
                "prompts/get",
                json!({"name": START_HERE_PROMPT_NAME}),
            ))
            .await
            .unwrap();
        let message = &got["result"]["messages"][0];
        assert_eq!(message["role"], "user");
        let text = message["content"]["text"].as_str().unwrap();
        assert_eq!(text, start_here_text());
        for question in START_HERE_QUESTIONS {
            assert!(text.contains(question), "{question}");
        }
        assert!(text.contains("Call describe. Then call list_revisions with no arguments"));
        assert!(text.ends_with(GROUNDING_RULE));

        let unknown = server
            .handle(request(json!(3), "prompts/get", json!({"name": "nope"})))
            .await
            .unwrap();
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        let unnamed = server
            .handle(request(json!(4), "prompts/get", json!({})))
            .await
            .unwrap();
        assert_eq!(unnamed["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn every_tool_carries_its_schema_and_the_grounding_rule() {
        for (tool, calc) in tools().iter().zip(catalog::CATALOG) {
            assert_eq!(tool["name"], calc.name);
            assert_eq!(tool["inputSchema"], calc.input_schema());
            let description = tool["description"].as_str().unwrap();
            assert!(description.starts_with(calc.summary));
            assert!(description.ends_with(GROUNDING_RULE));
        }
        assert!(prompt_text().starts_with(GROUNDING_RULE));
    }
}
