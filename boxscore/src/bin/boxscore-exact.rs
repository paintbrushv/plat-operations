use boxscore::exact::{
    self, catalog, error,
    ingest::{self, AdapterInputs},
    protocol, store, Result,
};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "boxscore-exact",
    version,
    about = "Exact-cent USD operating workflow; legacy floating-point commands are excluded"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Stateless, bounded JSON stdin/stdout. Never accesses a database or local input path.
    Protocol,
    /// Run one catalog function: input JSON on stdin, a provenance envelope on stdout.
    /// `--database` is required by stateful functions and is never read from the input.
    Call {
        #[arg(long)]
        function: String,
        /// Exact version to pin; refused if the catalog serves a different one.
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Show the recorded invocation behind a citation: the same envelope as
    /// `call --function get_provenance` with `{"trace_id": ID}`.
    Trace {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        id: String,
    },
    /// Serve the catalog as an MCP server: JSON-RPC 2.0, one message per line on
    /// stdin/stdout, until stdin closes. `--database` is fixed for the session
    /// and is never taken from a tool argument.
    Mcp {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Serve the catalog over HTTP: MCP Streamable HTTP at /mcp, one
    /// POST /v1/calc/{function} route per catalog function, /openapi.json,
    /// /health and OAuth protected-resource metadata. Loopback only unless
    /// --api-key-file or OAuth (--authorization-server with --oauth-audience,
    /// --oauth-client-id and --oauth-client-secret-file) is configured.
    /// Stops on SIGINT or SIGTERM.
    Serve {
        /// IP address and port to listen on.
        #[arg(long, default_value = exact::mcp_http::DEFAULT_BIND)]
        bind: String,
        #[arg(long)]
        database: Option<PathBuf>,
        /// File of `key_id:secret` lines (mode 0600); enables bearer auth.
        #[arg(long)]
        api_key_file: Option<PathBuf>,
        /// Origin allowed to send requests (repeatable); any other Origin gets 403.
        #[arg(long = "allow-origin")]
        allow_origins: Vec<String>,
        /// Public URL of this server (resource identifier); default http://127.0.0.1:<port>.
        #[arg(long)]
        public_url: Option<String>,
        /// OAuth issuer: advertised in metadata and used to validate access
        /// tokens; requires --oauth-audience, --oauth-client-id and
        /// --oauth-client-secret-file.
        #[arg(long = "authorization-server")]
        authorization_servers: Vec<String>,
        /// Requests per minute per key.
        #[arg(long, default_value_t = exact::mcp_http::DEFAULT_RATE_LIMIT)]
        rate_limit: u32,
        /// RFC 7662 introspection endpoint; default: read once at start-up
        /// from the issuer's RFC 8414 metadata (the path form
        /// <host>/.well-known/oauth-authorization-server<path> first, then
        /// <issuer>/.well-known/oauth-authorization-server on 404).
        #[arg(long)]
        introspection_endpoint: Option<String>,
        /// Client id this server presents to the introspection endpoint.
        #[arg(long)]
        oauth_client_id: Option<String>,
        /// File holding that client's secret (one line, mode 0600).
        #[arg(long)]
        oauth_client_secret_file: Option<PathBuf>,
        /// Value a token's `aud` must contain (or its `resource` equal).
        #[arg(long)]
        oauth_audience: Option<String>,
        /// Scope a token must carry.
        #[arg(long, value_name = "SCOPE")]
        oauth_scope: Option<String>,
        /// Public OAuth client registered at the issuer for the browser
        /// workbench (authorization code with PKCE, redirect URI
        /// <public-url>/): the page then offers sign-in through the identity
        /// provider. Needs the OAuth options above; the issuer's
        /// authorization and token endpoints are read from its metadata.
        #[arg(long, value_name = "ID")]
        oauth_browser_client_id: Option<String>,
    },
    /// Create a new exact-cent database; existing paths are refused.
    Init {
        #[arg(long)]
        database: PathBuf,
    },
    /// Import files through a registered source adapter (`adapters` lists them).
    /// Corrections require the current revision and a reason.
    ImportWith {
        #[arg(long)]
        database: PathBuf,
        /// Versioned adapter profile id, for example `canonical.csv/1`.
        #[arg(long)]
        profile: String,
        /// A named input file, `NAME=PATH`; repeat for each file.
        #[arg(long = "file", value_name = "NAME=PATH")]
        files: Vec<String>,
        /// A named adapter option, `KEY=VALUE`; repeat for each option.
        #[arg(long = "option", value_name = "KEY=VALUE")]
        options: Vec<String>,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List the registered source adapters and what each accepts and refuses.
    Adapters,
    /// Alias for `import-with --profile canonical.json/1 --file input=PATH`.
    Import {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Alias for `import-with --profile canonical.csv/1` with files `actuals`,
    /// `budgets`, optional `snapshot`, and options `property`, `period`, `units`.
    ImportCsv {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        actuals: PathBuf,
        #[arg(long)]
        budgets: PathBuf,
        #[arg(long)]
        property: String,
        #[arg(long)]
        period: String,
        #[arg(long)]
        units: i64,
        #[arg(long)]
        snapshot: Option<PathBuf>,
        #[arg(long)]
        supersedes: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    Review {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        revision: String,
    },
    /// Issue a new immutable report record; review flags remain visible.
    Issue {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        revision: String,
    },
    PlanMigration {
        #[arg(long)]
        source: PathBuf,
    },
    Migrate {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        review: PathBuf,
    },
}

/// Split a repeated `NAME=VALUE` flag into a map, refusing malformed or
/// repeated names.
fn pairs(flag: &str, values: Vec<String>) -> Result<std::collections::BTreeMap<String, String>> {
    let mut map = std::collections::BTreeMap::new();
    for value in values {
        let (name, rest) = value
            .split_once('=')
            .filter(|(name, _)| !name.is_empty())
            .ok_or_else(|| error("INVALID_INPUT", format!("--{flag} takes NAME=VALUE")))?;
        if map.insert(name.to_string(), rest.to_string()).is_some() {
            return Err(error(
                "INVALID_INPUT",
                format!("--{flag} {name:?} given more than once"),
            ));
        }
    }
    Ok(map)
}
/// The one import path: resolve the adapter, read, then store.
async fn import_through(
    database: &Path,
    profile: &str,
    inputs: &AdapterInputs,
    supersedes: Option<&str>,
    reason: Option<&str>,
) -> Result<Value> {
    let adapter = ingest::adapter(profile)?;
    let (data, sources) = ingest::read_with(adapter, inputs)?;
    let pool = store::open(database, false).await?;
    let outcome = store::import(&pool, &data, supersedes, reason, &sources).await;
    pool.close().await;
    Ok(json!({"contract_version":exact::CONTRACT,"status":"imported","revision_id":outcome?}))
}
async fn run(command: Command) -> Result<Value> {
    match command {
        Command::Protocol => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((protocol::MAX_REQUEST_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            protocol::calculate(&bytes).await
        }
        Command::Call {
            function,
            version,
            database,
        } => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((protocol::MAX_REQUEST_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > protocol::MAX_REQUEST_BYTES {
                return Err(error("INPUT_LIMIT", "Input exceeds 2 MiB"));
            }
            let empty = bytes.iter().all(u8::is_ascii_whitespace);
            let call = catalog::Call {
                function,
                version,
                input: if empty {
                    json!({})
                } else {
                    serde_json::from_slice(&bytes)?
                },
                database,
                caller: "cli".into(),
            };
            let envelope = if empty {
                catalog::invoke(&call).await
            } else {
                catalog::invoke_hashed(&call, exact::digest(&bytes)).await
            };
            Ok(envelope.to_value())
        }
        Command::Trace { database, id } => {
            let call = catalog::Call {
                function: "get_provenance".into(),
                version: None,
                input: json!({"trace_id": id}),
                database: Some(database),
                caller: "cli".into(),
            };
            Ok(catalog::invoke(&call).await.to_value())
        }
        Command::Mcp { .. } | Command::Serve { .. } => Err(error(
            "INTERNAL_ERROR",
            "servers are run by main, not as one-shot commands",
        )),
        Command::Init { database } => {
            let pool = store::create(&database).await?;
            pool.close().await;
            Ok(
                json!({"contract_version":exact::CONTRACT,"status":"created","schema_version":exact::SCHEMA}),
            )
        }
        Command::ImportWith {
            database,
            profile,
            files,
            options,
            supersedes,
            reason,
        } => {
            let inputs = AdapterInputs {
                files: pairs("file", files)?
                    .into_iter()
                    .map(|(name, path)| (name, PathBuf::from(path)))
                    .collect(),
                options: pairs("option", options)?,
                ..AdapterInputs::default()
            };
            import_through(
                &database,
                &profile,
                &inputs,
                supersedes.as_deref(),
                reason.as_deref(),
            )
            .await
        }
        Command::Adapters => Ok(json!({
            "contract_version": exact::CONTRACT,
            "adapters": ingest::describe_adapters(),
        })),
        Command::Import {
            database,
            input,
            supersedes,
            reason,
        } => {
            let inputs = AdapterInputs::default().with_file("input", input);
            import_through(
                &database,
                ingest::CANONICAL_JSON,
                &inputs,
                supersedes.as_deref(),
                reason.as_deref(),
            )
            .await
        }
        Command::ImportCsv {
            database,
            actuals,
            budgets,
            property,
            period,
            units,
            snapshot,
            supersedes,
            reason,
        } => {
            let mut inputs = AdapterInputs::default()
                .with_file("actuals", actuals)
                .with_file("budgets", budgets)
                .with_option("property", property)
                .with_option("period", period)
                .with_option("units", units.to_string());
            if let Some(snapshot) = snapshot {
                inputs = inputs.with_file("snapshot", snapshot);
            }
            import_through(
                &database,
                ingest::CANONICAL_CSV,
                &inputs,
                supersedes.as_deref(),
                reason.as_deref(),
            )
            .await
        }
        Command::Review { database, revision } => {
            let pool = store::open(&database, true).await?;
            let result = store::review(&pool, &revision).await?;
            pool.close().await;
            Ok(result)
        }
        Command::Issue { database, revision } => {
            let pool = store::open(&database, false).await?;
            let result = store::issue(&pool, &revision).await?;
            pool.close().await;
            Ok(result)
        }
        Command::PlanMigration { source } => exact::migration::plan(&source).await,
        Command::Migrate {
            source,
            destination,
            review,
        } => {
            let approved = serde_json::from_slice(&ingest::read_bounded(&review)?)?;
            exact::migration::migrate(&source, &destination, &approved).await
        }
    }
}
#[tokio::main]
async fn main() {
    // Intentionally no dotenv, tracing subscriber, or private legacy config.
    // The only HTTP server is `serve`, the exact catalog's own.
    let command = Cli::parse().command;
    if let Command::Serve {
        bind,
        database,
        api_key_file,
        allow_origins,
        public_url,
        authorization_servers,
        rate_limit,
        introspection_endpoint,
        oauth_client_id,
        oauth_client_secret_file,
        oauth_audience,
        oauth_scope,
        oauth_browser_client_id,
    } = command
    {
        let options = exact::mcp_http::ServeOptions {
            bind,
            database,
            api_key_file,
            allow_origins,
            public_url,
            authorization_servers,
            rate_limit,
            introspection_endpoint,
            oauth_client_id,
            oauth_client_secret_file,
            oauth_audience,
            oauth_scope,
            oauth_browser_client_id,
            oauth_authorization_endpoint: None,
            oauth_token_endpoint: None,
        };
        let code = match exact::mcp_http::serve(options, exact::mcp_http::shutdown_signal()).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("boxscore-exact serve: {e}");
                2
            }
        };
        std::process::exit(code);
    }
    if let Command::Mcp { database } = command {
        // stdout belongs to JSON-RPC alone; nothing is printed after the session.
        let code = match exact::mcp::run_stdio(database).await {
            Ok(()) => 0,
            Err(_) => 3,
        };
        std::process::exit(code);
    }
    let result = run(command).await;
    let (payload, code) = match result {
        Ok(v) => {
            // Catalog envelopes carry refusals inside the payload.
            let refused = v.get("status").and_then(Value::as_str) == Some("refused");
            (v, if refused { 2 } else { 0 })
        }
        Err(e) => (protocol::refusal(&e), 2),
    };
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &payload).is_err() || writeln!(stdout).is_err() {
        std::process::exit(3);
    }
    drop(stdout);
    std::process::exit(code);
}
