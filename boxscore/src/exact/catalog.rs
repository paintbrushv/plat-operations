//! Named, versioned calculations over the exact-cent core.
//!
//! The catalog is the API boundary every connector builds against. Each
//! calculation has a name, a semantic version, JSON Schemas for its input and
//! output, and returns an [`Envelope`] that states exactly which function and
//! version produced the result and the SHA-256 of what went in and what came
//! out. Nothing here reaches past `exact::`; legacy floating-point features
//! are never exposed through the catalog.
//!
//! Every invocation, refusals included, gets a `trace_id` and a one-line
//! `citation`, and is recorded by [`super::trace`] in the host's database (or
//! a JSONL file when the host configures one). `get_provenance` reads a
//! recorded invocation back.
//!
//! Versioning policy (per function, independent of the `plat.ops/1`
//! contract): a **patch** bump changes nothing a consumer can observe in
//! `result`; a **minor** bump adds fields to `result` without changing any
//! existing value; a **major** bump may change a number for the same input.
//! A consumer that pins `version` is refused rather than silently served a
//! different one.
use super::{
    digest, error, readiness, snapshot_metrics, store, t12, trace, variance, ExactError, Result,
    CONTRACT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const PRODUCER_NAME: &str = "boxscore-exact";
pub const ARITHMETIC: &str = "checked_i64_cents";

/// One entry in the catalog. Schemas are produced by functions so the table
/// itself can be a `const`.
#[derive(Debug, Clone, Copy)]
pub struct Calculation {
    pub name: &'static str,
    pub version: &'static str,
    pub summary: &'static str,
    /// Needs a database. The host supplies the path; the input never does.
    pub stateful: bool,
    input_schema: fn() -> Value,
    output_schema: fn() -> Value,
}

impl Calculation {
    pub fn input_schema(&self) -> Value {
        (self.input_schema)()
    }
    pub fn output_schema(&self) -> Value {
        (self.output_schema)()
    }
    pub fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "version": self.version,
            "summary": self.summary,
            "stateful": self.stateful,
            "input_schema": self.input_schema(),
            "output_schema": self.output_schema(),
        })
    }
}

pub const CATALOG: &[Calculation] = &[
    Calculation {
        name: "describe",
        version: "1.0.0",
        summary: "List every calculation in this catalog with its version and JSON Schemas.",
        stateful: false,
        input_schema: schemas::empty_input,
        output_schema: schemas::describe_output,
    },
    Calculation {
        name: "noi_variance",
        version: "1.0.0",
        summary: "Actual-versus-budget NOI bridge from inline GL rows, in checked USD cents.",
        stateful: false,
        input_schema: schemas::noi_variance_input,
        output_schema: schemas::variance_output,
    },
    Calculation {
        name: "import_period",
        version: "1.0.0",
        summary: "Store a canonical period dataset as a new immutable revision; corrections require the current revision id and a reason.",
        stateful: true,
        input_schema: schemas::import_period_input,
        output_schema: schemas::import_period_output,
    },
    Calculation {
        name: "review_period",
        version: "1.0.0",
        summary: "Review a stored revision: matched-account NOI bridge, review reasons, excluded accounts, and changes versus the superseded revision.",
        stateful: true,
        input_schema: schemas::revision_input,
        output_schema: schemas::review_output,
    },
    Calculation {
        name: "issue_report",
        version: "1.0.0",
        summary: "Issue an immutable report record for a revision. Review flags stay visible; human approval is never granted here.",
        stateful: true,
        input_schema: schemas::revision_input,
        output_schema: schemas::issue_output,
    },
    Calculation {
        name: "list_revisions",
        version: "1.0.0",
        summary: "List stored revisions and their issued reports, optionally filtered by property and period.",
        stateful: true,
        input_schema: schemas::list_revisions_input,
        output_schema: schemas::list_revisions_output,
    },
    Calculation {
        name: "occupancy",
        version: "1.0.0",
        summary: "Physical occupancy (occupied over total units) from a stored revision's snapshot or an inline snapshot, as an exact four-place ratio.",
        stateful: false,
        input_schema: schemas::snapshot_metric_input,
        output_schema: schemas::occupancy_output,
    },
    Calculation {
        name: "economic_occupancy",
        version: "1.0.0",
        summary: "Economic occupancy (in-place over market rent) from a stored revision's snapshot or an inline snapshot, as an exact four-place ratio of cents.",
        stateful: false,
        input_schema: schemas::snapshot_metric_input,
        output_schema: schemas::economic_occupancy_output,
    },
    Calculation {
        name: "delinquency",
        version: "1.0.0",
        summary: "Delinquent and prepaid balances, their net, and each as a share of in-place rent, from a stored revision's snapshot or an inline snapshot.",
        stateful: false,
        input_schema: schemas::snapshot_metric_input,
        output_schema: schemas::delinquency_output,
    },
    Calculation {
        name: "close_readiness",
        version: "1.0.0",
        summary: "Per-property close readiness for a period from the exact store: latest revision, account coverage, blocking review reasons, issued reports, and what the store does not track.",
        stateful: true,
        input_schema: readiness::input_schema,
        output_schema: readiness::output_schema,
    },
    Calculation {
        name: "t12",
        version: "1.0.0",
        summary: "Trailing twelve-period statement for a property from stored revisions: category rows, NOI, budgets and variance per month; a month with no revision is null and flagged, never zero.",
        stateful: true,
        input_schema: t12::input_schema,
        output_schema: t12::output_schema,
    },
    Calculation {
        name: "get_provenance",
        version: "1.0.0",
        summary: "Read back a recorded invocation by trace_id: function, version, caller, hashes, status, and the revision and report it produced or read.",
        stateful: true,
        input_schema: schemas::get_provenance_input,
        output_schema: schemas::get_provenance_output,
    },
];

/// Resolve a calculation by name and, when given, exact version.
pub fn find(name: &str, version: Option<&str>) -> Result<&'static Calculation> {
    let calc = CATALOG.iter().find(|c| c.name == name).ok_or_else(|| {
        error(
            "UNKNOWN_FUNCTION",
            format!("No calculation named {name:?}; call describe for the catalog"),
        )
    })?;
    if let Some(wanted) = version {
        if wanted != calc.version {
            return Err(error(
                "VERSION_MISMATCH",
                format!(
                    "{name} is available at version {} only, not {wanted}",
                    calc.version
                ),
            ));
        }
    }
    Ok(calc)
}

pub fn producer() -> Value {
    json!({
        "name": PRODUCER_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "arithmetic": ARITHMETIC,
    })
}

/// A request to run one calculation. `database` and `caller` are set by the
/// host process (CLI flag, connector configuration), never parsed from
/// `input`.
#[derive(Debug, Clone)]
pub struct Call {
    pub function: String,
    pub version: Option<String>,
    pub input: Value,
    pub database: Option<PathBuf>,
    /// Which host made the call, recorded with the trace: `"cli"`,
    /// `"protocol"`, `"mcp"`. Blank is recorded as `"unknown"`.
    pub caller: String,
}

impl Default for Call {
    fn default() -> Self {
        Self {
            function: String::new(),
            version: None,
            input: json!({}),
            database: None,
            caller: UNKNOWN_CALLER.into(),
        }
    }
}

pub const UNKNOWN_CALLER: &str = "unknown";

/// Every catalog response, success or refusal.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub contract_version: &'static str,
    pub function: String,
    /// `None` only when the function itself is unknown.
    pub version: Option<&'static str>,
    pub producer: Value,
    /// SHA-256 of the input bytes as received by the transport, or of the
    /// canonical JSON serialization for in-process calls.
    pub input_sha256: String,
    /// SHA-256 of the canonical JSON serialization of `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    pub computed_at: String,
    /// UUID v4 naming this invocation's trace record.
    pub trace_id: String,
    /// Where the trace record went: `"database"`, `"jsonl"` or `"none"`.
    pub trace_store: &'static str,
    /// One pasteable line naming function@version, trace id and hashes.
    pub citation: String,
    /// Why the trace record could not be written; the result is unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_warning: Option<String>,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

impl Envelope {
    pub fn refused(&self) -> bool {
        self.status == "refused"
    }
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"status": "refused"}))
    }
}

/// Canonical bytes: serde_json without `preserve_order` serializes object
/// keys in sorted order, and the exact types use `BTreeMap`, so the same
/// value always produces the same bytes.
pub fn canonical(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Run a calculation, hashing the canonical serialization of `input`.
pub async fn invoke(call: &Call) -> Envelope {
    let input_sha256 = digest(&canonical(&call.input));
    invoke_hashed(call, input_sha256).await
}

/// Run a calculation with an `input_sha256` computed by the transport over
/// the bytes it actually received.
pub async fn invoke_hashed(call: &Call, input_sha256: String) -> Envelope {
    let computed_at = crate::db::now_iso();
    // Report the served version even when a pinned version is refused, so the
    // caller can see what it would get.
    let served = CATALOG
        .iter()
        .find(|c| c.name == call.function)
        .map(|c| c.version);
    let (version, outcome) = match find(&call.function, call.version.as_deref()) {
        Err(e) => (served, Err(e)),
        Ok(calc) => (Some(calc.version), execute(calc, call).await),
    };
    let finished_at = crate::db::now_iso();
    let (status, result, failure) = match outcome {
        Ok((status, result)) => (status, Some(result), None),
        Err(e) => ("refused", None, Some(e)),
    };
    let output_sha256 = result.as_ref().map(|r| digest(&canonical(r)));
    let result_id = |key: &str| {
        result
            .as_ref()
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    // A successful calculation that read a stored revision (the snapshot
    // metrics) cites it even though its result does not repeat the id.
    let revision_id = result_id("revision_id").or_else(|| {
        result.as_ref().and_then(|_| {
            call.input
                .get("revision_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    });
    let caller = call.caller.trim();
    let invocation = trace::Invocation {
        trace_id: crate::db::new_id(),
        function: call.function.clone(),
        version: version.unwrap_or("unknown").to_string(),
        producer_version: env!("CARGO_PKG_VERSION").to_string(),
        caller: if caller.is_empty() {
            UNKNOWN_CALLER.to_string()
        } else {
            caller.to_string()
        },
        input_sha256,
        output_sha256,
        status: status.to_string(),
        error_code: failure.as_ref().map(|e| e.code.to_string()),
        revision_id,
        report_id: result_id("report_id"),
        input_json: trace::keep_inputs()
            .then(|| String::from_utf8_lossy(&canonical(&call.input)).into_owned()),
        started_at: computed_at.clone(),
        finished_at,
    };
    let citation = invocation.citation();
    let recorded = trace::record(call.database.as_deref(), &invocation).await;
    let trace::Invocation {
        trace_id,
        input_sha256,
        output_sha256,
        ..
    } = invocation;
    Envelope {
        contract_version: CONTRACT,
        function: call.function.clone(),
        version,
        producer: producer(),
        input_sha256,
        output_sha256,
        computed_at,
        trace_id,
        trace_store: recorded.store,
        citation,
        trace_warning: recorded.warning,
        status,
        result,
        error: failure.map(|e| json!({"code": e.code, "message": e.message})),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoiVarianceInput {
    currency: String,
    expense_convention: String,
    actuals: Vec<variance::Line>,
    budgets: Vec<variance::Line>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPeriodInput {
    dataset: store::Dataset,
    #[serde(default)]
    supersedes: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    /// Free-form provenance for the files behind this dataset (names and
    /// hashes). Recorded verbatim on the revision.
    #[serde(default)]
    sources: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionInput {
    revision_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceInput {
    trace_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRevisionsInput {
    #[serde(default)]
    property: Option<String>,
    #[serde(default)]
    period: Option<String>,
}

/// Exactly one of `revision_id` (read the stored revision's snapshot; needs
/// a host-supplied database) or `snapshot` with `unit_count` (inline, no
/// database).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMetricInput {
    #[serde(default)]
    revision_id: Option<String>,
    #[serde(default)]
    snapshot: Option<store::Snapshot>,
    #[serde(default)]
    unit_count: Option<i64>,
}

fn parse<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T> {
    serde_json::from_value(input.clone()).map_err(|e| {
        error(
            "INVALID_INPUT",
            format!("Input does not match the versioned schema: {e}"),
        )
    })
}

fn status_of(value: &Value) -> Result<&'static str> {
    match value.get("status").and_then(Value::as_str) {
        Some("calculated") => Ok("calculated"),
        Some("review_required") => Ok("review_required"),
        other => Err(error(
            "INTERNAL_ERROR",
            format!("Unexpected status {other:?} from the store"),
        )),
    }
}

async fn execute(calc: &Calculation, call: &Call) -> Result<(&'static str, Value)> {
    match calc.name {
        "describe" => {
            let _: EmptyInput = parse(&call.input)?;
            Ok((
                "calculated",
                json!({
                    "contract_version": CONTRACT,
                    "producer": producer(),
                    "calculations": CATALOG.iter().map(Calculation::describe).collect::<Vec<_>>(),
                }),
            ))
        }
        "noi_variance" => {
            let input: NoiVarianceInput = parse(&call.input)?;
            if input.currency != "USD" || input.expense_convention != "positive_costs" {
                return Err(error(
                    "INVALID_INPUT",
                    "Require currency USD and expense_convention positive_costs",
                ));
            }
            let result = variance::compute(&input.actuals, &input.budgets)?;
            let status = if result.review_reasons.is_empty() {
                "calculated"
            } else {
                "review_required"
            };
            Ok((status, serde_json::to_value(result)?))
        }
        "import_period" => {
            let input: ImportPeriodInput = parse(&call.input)?;
            let database = database(calc, call)?;
            let sources = match input.sources {
                Some(sources) => sources,
                None => {
                    json!({"canonical_json_sha256": digest(&serde_json::to_vec(&input.dataset)?)})
                }
            };
            let pool = store::open(database, false).await?;
            let outcome = store::import(
                &pool,
                &input.dataset,
                input.supersedes.as_deref(),
                input.reason.as_deref(),
                &sources,
            )
            .await;
            pool.close().await;
            Ok(("calculated", json!({"revision_id": outcome?})))
        }
        "review_period" => {
            let input: RevisionInput = parse(&call.input)?;
            let pool = store::open(database(calc, call)?, true).await?;
            let outcome = store::review(&pool, &input.revision_id).await;
            pool.close().await;
            let result = outcome?;
            Ok((status_of(&result)?, result))
        }
        "issue_report" => {
            let input: RevisionInput = parse(&call.input)?;
            let pool = store::open(database(calc, call)?, false).await?;
            let outcome = store::issue(&pool, &input.revision_id).await;
            pool.close().await;
            let result = outcome?;
            Ok((status_of(&result)?, result))
        }
        "list_revisions" => {
            let input: ListRevisionsInput = parse(&call.input)?;
            let pool = store::open(database(calc, call)?, true).await?;
            let outcome =
                store::list_revisions(&pool, input.property.as_deref(), input.period.as_deref())
                    .await;
            pool.close().await;
            Ok(("calculated", json!({"revisions": outcome?})))
        }
        "get_provenance" => {
            let input: ProvenanceInput = parse(&call.input)?;
            let pool = store::open(database(calc, call)?, true).await?;
            let outcome = trace::provenance(&pool, &input.trace_id).await;
            pool.close().await;
            Ok(("calculated", outcome?))
        }
        "occupancy" | "economic_occupancy" | "delinquency" => {
            let input: SnapshotMetricInput = parse(&call.input)?;
            let (snapshot, unit_count) = metric_snapshot(calc, call, input).await?;
            let (result, review_reasons) = match calc.name {
                "occupancy" => snapshot_metrics::occupancy(&snapshot, unit_count)?,
                "economic_occupancy" => snapshot_metrics::economic_occupancy(&snapshot)?,
                _ => snapshot_metrics::delinquency(&snapshot)?,
            };
            let status = if review_reasons.is_empty() {
                "calculated"
            } else {
                "review_required"
            };
            let mut result = result;
            result["review_reasons"] = json!(review_reasons);
            Ok((status, result))
        }
        "close_readiness" => readiness::execute(&call.input, database(calc, call)).await,
        "t12" => {
            let input: t12::Input = parse(&call.input)?;
            t12::run(database(calc, call)?, input).await
        }
        other => Err(error(
            "INTERNAL_ERROR",
            format!("Catalog entry {other} has no implementation"),
        )),
    }
}

/// Resolve the snapshot and unit count a snapshot metric runs over, from
/// either input form, and validate the snapshot the same way an import does.
async fn metric_snapshot(
    calc: &Calculation,
    call: &Call,
    input: SnapshotMetricInput,
) -> Result<(store::Snapshot, i64)> {
    let (snapshot, unit_count) = match input {
        SnapshotMetricInput {
            revision_id: Some(revision_id),
            snapshot: None,
            unit_count: None,
        } => {
            let path = call
                .database
                .as_deref()
                .ok_or_else(|| database_required(calc))?;
            let pool = store::open(path, true).await?;
            let outcome = store::load_verified(&pool, &revision_id).await;
            pool.close().await;
            let dataset = outcome?;
            let snapshot = dataset.snapshot.ok_or_else(|| {
                error(
                    "MISSING_OCCUPANCY_SNAPSHOT",
                    "The revision has no occupancy snapshot",
                )
            })?;
            (snapshot, dataset.unit_count)
        }
        SnapshotMetricInput {
            revision_id: None,
            snapshot: Some(snapshot),
            unit_count: Some(unit_count),
        } => (snapshot, unit_count),
        _ => {
            return Err(error(
                "INVALID_INPUT",
                "Give exactly one of revision_id, or snapshot together with unit_count",
            ))
        }
    };
    if !(1..=10_000_000).contains(&unit_count) {
        return Err(error("INVALID_INPUT", "Require a credible unit count"));
    }
    snapshot.validate(unit_count)?;
    Ok((snapshot, unit_count))
}

fn database<'a>(calc: &Calculation, call: &'a Call) -> Result<&'a std::path::Path> {
    debug_assert!(calc.stateful);
    call.database
        .as_deref()
        .ok_or_else(|| database_required(calc))
}

fn database_required(calc: &Calculation) -> ExactError {
    ExactError {
        code: "DATABASE_REQUIRED",
        message: format!(
            "{} needs an exact-cent database; the host supplies it (`boxscore-exact call --database`), never the input",
            calc.name
        ),
    }
}

/// Hand-written JSON Schema (draft 2020-12) for every catalog function.
/// Kept next to the serde types they describe; `tests/exact_catalog.rs`
/// checks each golden input against its schema's properties and required
/// keys so the two cannot drift silently.
pub mod schemas {
    use serde_json::{json, Value};

    const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
    /// Mirrors `Money::from_str`: optional sign, up to 17 whole digits, at
    /// most two decimal places.
    pub const MONEY_PATTERN: &str = "^-?[0-9]{1,17}(\\.[0-9]{1,2})?$";

    pub fn money() -> Value {
        json!({"type": "string", "pattern": MONEY_PATTERN,
               "description": "USD amount as a decimal string with at most two places; never a JSON number"})
    }
    fn optional_money() -> Value {
        json!({"anyOf": [money(), {"type": "null"}]})
    }
    fn period() -> Value {
        json!({"type": "string", "pattern": "^[0-9]{4}-(0[1-9]|1[0-2])$", "description": "YYYY-MM"})
    }
    fn line() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["account_code", "account_name", "category", "amount"],
            "properties": {
                "account_code": {"type": "string", "minLength": 1, "maxLength": 64,
                                 "description": "Account identity; no surrounding whitespace"},
                "account_name": {"type": "string", "maxLength": 256},
                "category": {"type": "string", "minLength": 1, "maxLength": 128,
                             "description": "rental income, concessions, bad debt and other income are revenue; unmapped is disclosed separately; anything else is an expense"},
                "amount": money()
            }
        })
    }
    fn lines() -> Value {
        json!({"type": "array", "items": line(), "maxItems": 100000})
    }
    fn snapshot() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["as_of_date", "occupied_units", "vacant_units", "down_units",
                         "market_rent_total", "in_place_rent_total", "delinquent_amount",
                         "prepaid_amount", "concessions_amount"],
            "properties": {
                "as_of_date": {"type": "string", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"},
                "occupied_units": {"type": "integer", "minimum": 0},
                "vacant_units": {"type": "integer", "minimum": 0},
                "down_units": {"type": "integer", "minimum": 0},
                "market_rent_total": optional_money(),
                "in_place_rent_total": optional_money(),
                "delinquent_amount": optional_money(),
                "prepaid_amount": optional_money(),
                "concessions_amount": optional_money()
            }
        })
    }
    fn dataset() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["property", "period", "currency", "expense_convention", "unit_count",
                         "actuals", "budgets", "snapshot"],
            "properties": {
                "property": {"type": "string", "minLength": 1, "maxLength": 120},
                "period": period(),
                "currency": {"const": "USD"},
                "expense_convention": {"const": "positive_costs"},
                "unit_count": {"type": "integer", "minimum": 1, "maximum": 10000000},
                "actuals": lines(),
                "budgets": lines(),
                "snapshot": {"anyOf": [snapshot(), {"type": "null"}]}
            }
        })
    }
    fn account_variance() -> Value {
        json!({
            "type": "object",
            "required": ["account_code", "account_name", "category", "actual", "budget", "variance"],
            "properties": {
                "account_code": {"type": "string"},
                "account_name": {"type": "string"},
                "category": {"type": "string"},
                "actual": money(), "budget": money(), "variance": money()
            }
        })
    }
    fn variance() -> Value {
        json!({
            "type": "object",
            "required": ["by_account", "noi_bridge", "review_reasons"],
            "properties": {
                "by_account": {"type": "array", "items": account_variance()},
                "noi_bridge": {
                    "type": "object",
                    "required": ["actual_revenue", "budget_revenue", "revenue_variance",
                                 "actual_expenses", "budget_expenses", "expense_variance",
                                 "actual_noi", "budget_noi", "noi_variance",
                                 "unmapped_actual", "unmapped_budget"],
                    "additionalProperties": money()
                },
                "review_reasons": {"type": "array", "items": {"type": "string"}}
            }
        })
    }
    fn calculation_entry() -> Value {
        json!({
            "type": "object",
            "required": ["name", "version", "summary", "stateful", "input_schema", "output_schema"],
            "properties": {
                "name": {"type": "string"},
                "version": {"type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+$"},
                "summary": {"type": "string"},
                "stateful": {"type": "boolean"},
                "input_schema": {"type": "object"},
                "output_schema": {"type": "object"}
            }
        })
    }

    pub fn empty_input() -> Value {
        json!({"$schema": DRAFT, "type": "object", "additionalProperties": false, "properties": {}})
    }
    pub fn describe_output() -> Value {
        json!({
            "$schema": DRAFT, "type": "object",
            "required": ["contract_version", "producer", "calculations"],
            "properties": {
                "contract_version": {"const": super::CONTRACT},
                "producer": {"type": "object"},
                "calculations": {"type": "array", "items": calculation_entry()}
            }
        })
    }
    pub fn noi_variance_input() -> Value {
        json!({
            "$schema": DRAFT, "type": "object", "additionalProperties": false,
            "required": ["currency", "expense_convention", "actuals", "budgets"],
            "properties": {
                "currency": {"const": "USD"},
                "expense_convention": {"const": "positive_costs"},
                "actuals": lines(),
                "budgets": lines()
            }
        })
    }
    pub fn variance_output() -> Value {
        let mut schema = variance();
        schema["$schema"] = json!(DRAFT);
        schema
    }
    pub fn import_period_input() -> Value {
        json!({
            "$schema": DRAFT, "type": "object", "additionalProperties": false,
            "required": ["dataset"],
            "properties": {
                "dataset": dataset(),
                "supersedes": {"type": "string", "description": "Current revision id when this import is a correction"},
                "reason": {"type": "string", "maxLength": 2000, "description": "Required with supersedes"},
                "sources": {"type": "object", "description": "File names and hashes behind this dataset; recorded verbatim"}
            }
        })
    }
    pub fn import_period_output() -> Value {
        json!({"$schema": DRAFT, "type": "object", "required": ["revision_id"],
               "properties": {"revision_id": {"type": "string"}}})
    }
    pub fn revision_input() -> Value {
        json!({"$schema": DRAFT, "type": "object", "additionalProperties": false,
               "required": ["revision_id"], "properties": {"revision_id": {"type": "string"}}})
    }
    pub fn review_output() -> Value {
        json!({
            "$schema": DRAFT, "type": "object",
            "required": ["contract_version", "revision_id", "property", "period", "currency",
                         "expense_convention", "input_sha256", "sources", "supersedes_revision",
                         "correction_reason", "changes", "status", "variance", "bridge_scope",
                         "excluded_accounts", "snapshot", "unit_count", "human_approval"],
            "properties": {
                "contract_version": {"const": super::CONTRACT},
                "revision_id": {"type": "string"},
                "property": {"type": "string"},
                "period": period(),
                "currency": {"const": "USD"},
                "expense_convention": {"const": "positive_costs"},
                "input_sha256": {"type": "string"},
                "sources": {"type": "object"},
                "supersedes_revision": {"type": ["string", "null"]},
                "correction_reason": {"type": ["string", "null"]},
                "changes": {"type": "object", "additionalProperties": money(),
                            "description": "Change in each bridge value versus the superseded revision; empty for a first revision"},
                "status": {"enum": ["calculated", "review_required"]},
                "variance": variance(),
                "bridge_scope": {"const": "matched_accounts_only"},
                "excluded_accounts": {"type": "array"},
                "snapshot": {"anyOf": [snapshot(), {"type": "null"}]},
                "unit_count": {"type": "integer"},
                "human_approval": {"const": "not_granted"}
            }
        })
    }
    pub fn issue_output() -> Value {
        let mut schema = review_output();
        if let Some(required) = schema["required"].as_array_mut() {
            required.extend([json!("report_id"), json!("issued_at")]);
        }
        schema["properties"]["report_id"] = json!({"type": "string"});
        schema["properties"]["issued_at"] = json!({"type": "string"});
        schema
    }
    fn ratio() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["numerator", "denominator", "decimal", "scale"],
            "properties": {
                "numerator": {"type": "string", "pattern": "^-?[0-9]+$"},
                "denominator": {"type": "string", "pattern": "^-?[0-9]+$"},
                "decimal": {"type": "string", "pattern": "^-?[0-9]+\\.[0-9]{4}$",
                            "description": "numerator / denominator rounded half away from zero to four places, in integer arithmetic"},
                "scale": {"const": 4}
            }
        })
    }
    fn optional_ratio() -> Value {
        json!({"anyOf": [ratio(), {"type": "null"}]})
    }
    fn snapshot_metric_output(required: &[&str], properties: Vec<(&str, Value)>) -> Value {
        let mut required: Vec<Value> = required.iter().map(|k| json!(k)).collect();
        required.extend([json!("as_of_date"), json!("review_reasons")]);
        let mut schema = json!({
            "$schema": DRAFT, "type": "object", "required": required,
            "properties": {
                "as_of_date": {"type": "string", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"},
                "review_reasons": {"type": "array", "items": {"type": "string"}}
            }
        });
        for (key, value) in properties {
            schema["properties"][key] = value;
        }
        schema
    }
    /// Either a stored revision (needs the host's database) or an inline
    /// snapshot with its unit count; never both.
    pub fn snapshot_metric_input() -> Value {
        json!({
            "$schema": DRAFT, "type": "object", "additionalProperties": false,
            "properties": {
                "revision_id": {"type": "string", "description": "Use the snapshot stored with this revision; needs the host-supplied database"},
                "snapshot": snapshot(),
                "unit_count": {"type": "integer", "minimum": 1, "maximum": 10000000,
                               "description": "Total units; occupied, vacant and down units must sum to it"}
            },
            "oneOf": [
                {"required": ["revision_id"],
                 "properties": {"snapshot": false, "unit_count": false}},
                {"required": ["snapshot", "unit_count"],
                 "properties": {"revision_id": false}}
            ]
        })
    }
    pub fn occupancy_output() -> Value {
        let count = json!({"type": "integer", "minimum": 0});
        snapshot_metric_output(
            &[
                "unit_count",
                "occupied_units",
                "vacant_units",
                "down_units",
                "occupancy",
            ],
            vec![
                ("unit_count", json!({"type": "integer", "minimum": 1})),
                ("occupied_units", count.clone()),
                ("vacant_units", count.clone()),
                ("down_units", count),
                ("occupancy", ratio()),
            ],
        )
    }
    pub fn economic_occupancy_output() -> Value {
        snapshot_metric_output(
            &[
                "market_rent_total",
                "in_place_rent_total",
                "economic_occupancy",
            ],
            vec![
                ("market_rent_total", money()),
                ("in_place_rent_total", money()),
                ("economic_occupancy", ratio()),
            ],
        )
    }
    pub fn delinquency_output() -> Value {
        snapshot_metric_output(
            &[
                "delinquent_amount",
                "prepaid_amount",
                "net",
                "in_place_rent_total",
                "delinquent_share_of_in_place_rent",
                "prepaid_share_of_in_place_rent",
            ],
            vec![
                ("delinquent_amount", money()),
                ("prepaid_amount", optional_money()),
                ("net", optional_money()),
                ("in_place_rent_total", optional_money()),
                ("delinquent_share_of_in_place_rent", optional_ratio()),
                ("prepaid_share_of_in_place_rent", optional_ratio()),
            ],
        )
    }
    pub fn list_revisions_input() -> Value {
        json!({
            "$schema": DRAFT, "type": "object", "additionalProperties": false,
            "properties": {
                "property": {"type": "string"},
                "period": period()
            }
        })
    }
    pub fn list_revisions_output() -> Value {
        json!({
            "$schema": DRAFT, "type": "object", "required": ["revisions"],
            "properties": {
                "revisions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "property", "period", "revision", "parent_id", "reason",
                                     "unit_count", "input_sha256", "sources", "created_at", "reports"],
                        "properties": {
                            "id": {"type": "string"},
                            "property": {"type": "string"},
                            "period": period(),
                            "revision": {"type": "integer", "minimum": 1},
                            "parent_id": {"type": ["string", "null"]},
                            "reason": {"type": ["string", "null"]},
                            "unit_count": {"type": "integer"},
                            "input_sha256": {"type": "string"},
                            "sources": {"type": "object"},
                            "created_at": {"type": "string"},
                            "reports": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "required": ["id", "body_sha256", "created_at"],
                                    "properties": {
                                        "id": {"type": "string"},
                                        "body_sha256": {"type": "string"},
                                        "created_at": {"type": "string"}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }
    pub fn get_provenance_input() -> Value {
        json!({"$schema": DRAFT, "type": "object", "additionalProperties": false,
               "required": ["trace_id"],
               "properties": {"trace_id": {"type": "string", "description": "trace_id from an envelope or its citation"}}})
    }
    pub fn get_provenance_output() -> Value {
        let optional = |kind: &str| json!({"type": [kind, "null"]});
        json!({
            "$schema": DRAFT, "type": "object", "required": ["trace", "revision", "report"],
            "properties": {
                "trace": {
                    "type": "object",
                    "required": ["trace_id", "function", "version", "producer_version", "caller",
                                 "input_sha256", "output_sha256", "status", "error_code",
                                 "revision_id", "report_id", "input_json", "started_at", "finished_at"],
                    "properties": {
                        "trace_id": {"type": "string"},
                        "function": {"type": "string"},
                        "version": {"type": "string"},
                        "producer_version": {"type": "string"},
                        "caller": {"type": "string"},
                        "input_sha256": {"type": "string"},
                        "output_sha256": optional("string"),
                        "status": {"enum": ["calculated", "review_required", "refused"]},
                        "error_code": optional("string"),
                        "revision_id": optional("string"),
                        "report_id": optional("string"),
                        "input_json": {"type": ["string", "null"],
                                       "description": "Canonical input JSON; null unless the host set BOXSCORE_TRACE_INPUTS=1"},
                        "started_at": {"type": "string"},
                        "finished_at": {"type": "string"}
                    }
                },
                "revision": {
                    "anyOf": [{
                        "type": "object",
                        "required": ["id", "property", "period", "revision", "parent_id", "reason",
                                     "input_sha256", "sources"],
                        "properties": {
                            "id": {"type": "string"},
                            "property": {"type": "string"},
                            "period": period(),
                            "revision": {"type": "integer", "minimum": 1},
                            "parent_id": optional("string"),
                            "reason": optional("string"),
                            "input_sha256": {"type": "string"},
                            "sources": {"type": "object"}
                        }
                    }, {"type": "null"}]
                },
                "report": {
                    "anyOf": [{
                        "type": "object",
                        "required": ["id", "created_at", "body_sha256"],
                        "properties": {
                            "id": {"type": "string"},
                            "created_at": {"type": "string"},
                            "body_sha256": {"type": "string"}
                        }
                    }, {"type": "null"}]
                }
            }
        })
    }
}
