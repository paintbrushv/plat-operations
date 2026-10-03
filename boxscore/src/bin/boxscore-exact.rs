use boxscore::exact::{self, error, protocol, store, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(version, about = "Exact-cent USD operating workflow; legacy floating-point commands are excluded")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Stateless, bounded JSON stdin/stdout. Never accesses a database or local input path.
    Protocol,
    /// Create a new exact-cent database; existing paths are refused.
    Init {
        #[arg(long)]
        database: PathBuf,
    },
    /// Import canonical JSON; corrections require the current revision and a reason.
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
    /// Import canonical CSV GL rows with decimal-string money.
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

fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((protocol::MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > protocol::MAX_REQUEST_BYTES {
        return Err(error("INPUT_LIMIT", "Input file exceeds 2 MiB"));
    }
    Ok(bytes)
}
fn csv_lines(bytes: &[u8]) -> Result<Vec<exact::variance::Line>> {
    csv::Reader::from_reader(bytes)
        .deserialize()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            error(
                "INVALID_INPUT",
                "CSV requires account_code, account_name, category, and decimal amount",
            )
        })
}
async fn run(command: Command) -> Result<Value> {
    match command {
        Command::Protocol => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((protocol::MAX_REQUEST_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            protocol::calculate(&bytes)
        }
        Command::Init { database } => {
            let pool = store::create(&database).await?;
            pool.close().await;
            Ok(
                json!({"contract_version":exact::CONTRACT,"status":"created","schema_version":exact::SCHEMA}),
            )
        }
        Command::Import {
            database,
            input,
            supersedes,
            reason,
        } => {
            let bytes = read(&input)?;
            let data: store::Dataset = serde_json::from_slice(&bytes)?;
            let pool = store::open(&database, false).await?;
            let id = store::import(
                &pool,
                &data,
                supersedes.as_deref(),
                reason.as_deref(),
                &json!({"canonical_json_sha256":exact::digest(&bytes)}),
            )
            .await?;
            pool.close().await;
            Ok(json!({"contract_version":exact::CONTRACT,"status":"imported","revision_id":id}))
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
            let a = read(&actuals)?;
            let b = read(&budgets)?;
            let snap = snapshot.map(|p| read(&p)).transpose()?;
            let data = store::Dataset {
                property,
                period,
                currency: "USD".into(),
                expense_convention: "positive_costs".into(),
                unit_count: units,
                actuals: csv_lines(&a)?,
                budgets: csv_lines(&b)?,
                snapshot: snap
                    .as_ref()
                    .map(|v| serde_json::from_slice(v))
                    .transpose()?,
            };
            let pool = store::open(&database, false).await?;
            let id = store::import(&pool,&data,supersedes.as_deref(),reason.as_deref(),
                &json!({"actuals_sha256":exact::digest(&a),"budgets_sha256":exact::digest(&b),"snapshot_sha256":snap.as_ref().map(|v| exact::digest(v))})).await?;
            pool.close().await;
            Ok(json!({"contract_version":exact::CONTRACT,"status":"imported","revision_id":id}))
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
            let approved = serde_json::from_slice(&read(&review)?)?;
            exact::migration::migrate(&source, &destination, &approved).await
        }
    }
}
#[tokio::main]
async fn main() {
    // Intentionally no dotenv, tracing subscriber, HTTP server, or private legacy config.
    let result = run(Cli::parse().command).await;
    let (payload, code) = match result {
        Ok(v) => (v, 0),
        Err(e) => (protocol::refusal(&e), 2),
    };
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &payload).is_err() || writeln!(stdout).is_err() {
        std::process::exit(3);
    }
    drop(stdout);
    std::process::exit(code);
}
