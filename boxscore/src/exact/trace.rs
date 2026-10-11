//! Append-only record of every catalog invocation, and the citation line that
//! points back to it.
//!
//! [`super::catalog::invoke_hashed`] builds one [`Invocation`] per call after
//! the calculation has finished (its own effects are already committed) and
//! hands it to [`record`]. With a host-supplied database the record goes to
//! `exact_invocations`; without one, and with `BOXSCORE_TRACE_DIR` naming a
//! directory, it is appended to `<dir>/invocations.jsonl`. Tracing never fails
//! a calculation: when the record cannot be written the envelope says
//! `trace_store: "none"` and carries a `trace_warning`.
//!
//! Raw input is stored only when `BOXSCORE_TRACE_INPUTS=1`, so whoever runs
//! the host decides whether operating numbers are retained beyond their hash.
use super::{error, store, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// Directory for the JSONL trace when the call has no database.
pub const TRACE_DIR_ENV: &str = "BOXSCORE_TRACE_DIR";
/// Set to `1` to keep the canonical input JSON with each record.
pub const TRACE_INPUTS_ENV: &str = "BOXSCORE_TRACE_INPUTS";
pub const JSONL_FILE: &str = "invocations.jsonl";
/// Hex characters of each hash shown in a citation.
const CITED_HASH_CHARS: usize = 12;

/// One row of `exact_invocations`, and one line of `invocations.jsonl`.
#[derive(Debug, Clone, Serialize)]
pub struct Invocation {
    pub trace_id: String,
    pub function: String,
    pub version: String,
    pub producer_version: String,
    pub caller: String,
    pub input_sha256: String,
    pub output_sha256: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub revision_id: Option<String>,
    pub report_id: Option<String>,
    pub input_json: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

/// Whether the host asked for raw inputs to be retained.
pub fn keep_inputs() -> bool {
    std::env::var(TRACE_INPUTS_ENV).is_ok_and(|v| v == "1")
}

fn short(hash: &str) -> &str {
    hash.get(..CITED_HASH_CHARS).unwrap_or(hash)
}

impl Invocation {
    /// One line a person can paste into a memo, for example
    /// `boxscore noi_variance@1.0.0 · trace 7f3a1c2e-… · input sha256:1b7638a0c2d4… · output sha256:ce5487f1e2a3…`
    /// (the trace id is printed in full; it is shortened here only).
    /// The trace id is given in full because it is the lookup key for
    /// `get_provenance` / `boxscore-exact trace`.
    pub fn citation(&self) -> String {
        let mut line = format!(
            "boxscore {}@{} · trace {} · input sha256:{}…",
            self.function,
            self.version,
            self.trace_id,
            short(&self.input_sha256)
        );
        match (&self.output_sha256, &self.error_code) {
            (Some(output), _) => line.push_str(&format!(" · output sha256:{}…", short(output))),
            (None, Some(code)) => line.push_str(&format!(" · refused {code}")),
            (None, None) => line.push_str(" · refused"),
        }
        if let Some(revision) = &self.revision_id {
            line.push_str(&format!(" · revision {revision}"));
        }
        if let Some(report) = &self.report_id {
            line.push_str(&format!(" · report {report}"));
        }
        line
    }
}

/// Where a record went, and why it did not go anywhere when it should have.
pub struct Recorded {
    pub store: &'static str,
    pub warning: Option<String>,
}

/// Write the record to the host's database when one was given, otherwise to
/// the JSONL trace directory when one is configured. Never returns an error.
pub async fn record(database: Option<&Path>, invocation: &Invocation) -> Recorded {
    if let Some(path) = database {
        return match record_database(path, invocation).await {
            Ok(()) => Recorded {
                store: "database",
                warning: None,
            },
            Err(e) => Recorded {
                store: "none",
                warning: Some(format!(
                    "Invocation was not recorded in the database ({}: {}); the result is unaffected",
                    e.code, e.message
                )),
            },
        };
    }
    let Some(dir) = std::env::var_os(TRACE_DIR_ENV).filter(|d| !d.is_empty()) else {
        return Recorded {
            store: "none",
            warning: None,
        };
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Recorded {
            store: "none",
            warning: Some(format!(
                "{TRACE_DIR_ENV} does not name an existing directory; invocation was not recorded"
            )),
        };
    }
    match record_jsonl(&dir, invocation) {
        Ok(()) => Recorded {
            store: "jsonl",
            warning: None,
        },
        Err(e) => Recorded {
            store: "none",
            warning: Some(format!(
                "Invocation was not appended to {JSONL_FILE} ({}: {}); the result is unaffected",
                e.code, e.message
            )),
        },
    }
}

async fn record_database(path: &Path, invocation: &Invocation) -> Result<()> {
    let pool = store::open(path, false).await?;
    let outcome = sqlx::query(
        "INSERT INTO exact_invocations (trace_id,function,version,producer_version,caller,\
         input_sha256,output_sha256,status,error_code,revision_id,report_id,input_json,\
         started_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&invocation.trace_id)
    .bind(&invocation.function)
    .bind(&invocation.version)
    .bind(&invocation.producer_version)
    .bind(&invocation.caller)
    .bind(&invocation.input_sha256)
    .bind(&invocation.output_sha256)
    .bind(&invocation.status)
    .bind(&invocation.error_code)
    .bind(&invocation.revision_id)
    .bind(&invocation.report_id)
    .bind(&invocation.input_json)
    .bind(&invocation.started_at)
    .bind(&invocation.finished_at)
    .execute(&pool)
    .await;
    pool.close().await;
    outcome?;
    Ok(())
}

fn record_jsonl(dir: &Path, invocation: &Invocation) -> Result<()> {
    let mut line = serde_json::to_vec(invocation)?;
    line.push(b'\n');
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // One write per record so concurrent appenders do not interleave lines.
    options.open(dir.join(JSONL_FILE))?.write_all(&line)?;
    Ok(())
}

/// The stored record for `trace_id`, with the revision and report it names.
pub async fn provenance(pool: &SqlitePool, trace_id: &str) -> Result<Value> {
    let row = sqlx::query("SELECT * FROM exact_invocations WHERE trace_id=?")
        .bind(trace_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| error("NOT_FOUND", "Unknown trace"))?;
    let revision_id: Option<String> = row.try_get("revision_id")?;
    let report_id: Option<String> = row.try_get("report_id")?;
    let trace = json!({
        "trace_id": row.try_get::<String, _>("trace_id")?,
        "function": row.try_get::<String, _>("function")?,
        "version": row.try_get::<String, _>("version")?,
        "producer_version": row.try_get::<String, _>("producer_version")?,
        "caller": row.try_get::<String, _>("caller")?,
        "input_sha256": row.try_get::<String, _>("input_sha256")?,
        "output_sha256": row.try_get::<Option<String>, _>("output_sha256")?,
        "status": row.try_get::<String, _>("status")?,
        "error_code": row.try_get::<Option<String>, _>("error_code")?,
        "revision_id": revision_id,
        "report_id": report_id,
        "input_json": row.try_get::<Option<String>, _>("input_json")?,
        "started_at": row.try_get::<String, _>("started_at")?,
        "finished_at": row.try_get::<String, _>("finished_at")?,
    });
    let revision = match &revision_id {
        None => None,
        Some(id) => sqlx::query(
            "SELECT id,property,period,revision,parent_id,reason,input_sha256,sources \
             FROM exact_revisions WHERE id=?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(|r| -> Result<Value> {
            Ok(json!({
                "id": r.try_get::<String, _>("id")?,
                "property": r.try_get::<String, _>("property")?,
                "period": r.try_get::<String, _>("period")?,
                "revision": r.try_get::<i64, _>("revision")?,
                "parent_id": r.try_get::<Option<String>, _>("parent_id")?,
                "reason": r.try_get::<Option<String>, _>("reason")?,
                "input_sha256": r.try_get::<String, _>("input_sha256")?,
                "sources": serde_json::from_str::<Value>(&r.try_get::<String, _>("sources")?)?,
            }))
        })
        .transpose()?,
    };
    let report = match &report_id {
        None => None,
        Some(id) => sqlx::query("SELECT id,created_at,body_sha256 FROM exact_reports WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .map(|r| -> Result<Value> {
                Ok(json!({
                    "id": r.try_get::<String, _>("id")?,
                    "created_at": r.try_get::<String, _>("created_at")?,
                    "body_sha256": r.try_get::<String, _>("body_sha256")?,
                }))
            })
            .transpose()?,
    };
    Ok(json!({"trace": trace, "revision": revision, "report": report}))
}
