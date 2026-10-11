//! `close_readiness`: is each property's period ready to close, judged only
//! from what the exact-cent store holds.
//!
//! The legacy readiness check answers a wider question (collections context,
//! weekly reports) from floating-point tables. This function answers the part
//! the exact store can vouch for: are actuals, budgets and the occupancy
//! snapshot there, do the accounts line up, does the latest revision carry any
//! blocking review reason, and has a report been issued for it. The review
//! rules themselves live in [`store::review`]; this module reads its output
//! and never re-derives them. Everything else is listed, per property, under
//! `not_tracked_in_exact_store` so a "ready" is never read as more than it is.
use super::{error, store, ExactError, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Close-readiness inputs the legacy check covers that the exact store does
/// not hold. Reported verbatim on every property.
const NOT_TRACKED: [&str; 3] = ["collections_context", "weekly_bddre", "weekly_rpcoe"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    period: String,
    #[serde(default)]
    property: Option<String>,
}

/// Run `close_readiness`. `database` is the host's resolution of the
/// database path; it is only consulted after the input has parsed and the
/// period has validated, so a malformed request is refused as such first.
pub async fn execute(input: &Value, database: Result<&Path>) -> Result<(&'static str, Value)> {
    let input: Input = serde_json::from_value(input.clone()).map_err(|e| {
        error(
            "INVALID_INPUT",
            format!("Input does not match the versioned schema: {e}"),
        )
    })?;
    let period = crate::db::normalize_period_label(&input.period)
        .map_err(|_| error("INVALID_INPUT", "Invalid YYYY-MM period"))?;
    if period != input.period {
        return Err(error("INVALID_INPUT", "Period must use YYYY-MM"));
    }
    if input
        .property
        .as_deref()
        .is_some_and(|p| p.trim().is_empty() || p.len() > 120)
    {
        return Err(error("INVALID_INPUT", "Invalid property filter"));
    }
    let pool = store::open(database?, true).await?;
    let outcome = evaluate(&pool, &period, input.property.as_deref()).await;
    pool.close().await;
    outcome
}

async fn evaluate(
    pool: &SqlitePool,
    period: &str,
    property: Option<&str>,
) -> Result<(&'static str, Value)> {
    // Revisions come back ordered by revision, so the last one per property
    // is the latest; the explicit comparison keeps that from being an accident.
    let mut latest: BTreeMap<String, Value> = BTreeMap::new();
    for revision in store::list_revisions(pool, property, Some(period)).await? {
        let name = text(&revision, "property")?.to_string();
        let number = integer(&revision, "revision")?;
        let newer = match latest.get(&name) {
            Some(held) => number > integer(held, "revision")?,
            None => true,
        };
        if newer {
            latest.insert(name, revision);
        }
    }

    let mut properties = Vec::with_capacity(latest.len());
    let (mut ready, mut not_ready) = (0_u64, 0_u64);
    for revision in latest.values() {
        let entry = property_entry(pool, period, revision).await?;
        if entry["status"] == "ready" {
            ready += 1;
        } else {
            not_ready += 1;
        }
        properties.push(entry);
    }

    let nothing_matched = properties.is_empty();
    let mut review_reasons = Vec::new();
    if nothing_matched {
        review_reasons.push("no_revisions_for_period");
    }
    if not_ready > 0 {
        review_reasons.push("properties_not_ready");
    }
    let status = if review_reasons.is_empty() {
        "calculated"
    } else {
        "review_required"
    };
    Ok((
        status,
        json!({
            "period": period,
            "properties": properties,
            "summary": {
                "ready": ready,
                "not_ready": not_ready,
                "no_revisions": u64::from(nothing_matched),
            },
            "review_reasons": review_reasons,
        }),
    ))
}

async fn property_entry(pool: &SqlitePool, period: &str, revision: &Value) -> Result<Value> {
    let id = text(revision, "id")?;
    // Verified first so a row changed after import refuses before anything
    // is reported about it; `review` verifies again on its own.
    let data = store::load_verified(pool, id).await?;
    let review = store::review(pool, id).await?;

    let variance = field(&review, "variance")?;
    let matched = array(variance, "by_account")?.len();
    let (mut actual_only, mut budget_only) = (BTreeSet::new(), BTreeSet::new());
    for excluded in array(&review, "excluded_accounts")? {
        let code = text(field(excluded, "line")?, "account_code")?;
        match text(excluded, "kind")? {
            "actual" => actual_only.insert(code),
            _ => budget_only.insert(code),
        };
    }
    let blocking = array(variance, "review_reasons")?.to_vec();

    let snapshot = match &data.snapshot {
        None => "missing",
        Some(s) if s.as_of_date.starts_with(&format!("{period}-")) => "present",
        Some(_) => "outside_period",
    };
    let reports: Vec<Value> = array(revision, "reports")?
        .iter()
        .map(|report| {
            Ok(json!({
                "id": text(report, "id")?,
                "created_at": text(report, "created_at")?,
            }))
        })
        .collect::<Result<_>>()?;

    let actuals_present = !data.actuals.is_empty();
    let budgets_present = !data.budgets.is_empty();
    let ready = actuals_present
        && budgets_present
        && snapshot == "present"
        && blocking.is_empty()
        && !reports.is_empty();

    Ok(json!({
        "property": data.property,
        "revision_id": id,
        "revision": integer(revision, "revision")?,
        "is_correction": !field(revision, "parent_id")?.is_null(),
        "correction_reason": field(revision, "reason")?,
        "actuals_present": actuals_present,
        "budgets_present": budgets_present,
        "snapshot": snapshot,
        "account_coverage": {
            "matched": matched,
            "actual_only": actual_only.len(),
            "budget_only": budget_only.len(),
        },
        "blocking_review_reasons": blocking,
        "issued_reports": reports,
        "status": if ready { "ready" } else { "not_ready" },
        "not_tracked_in_exact_store": NOT_TRACKED,
    }))
}

fn shape(key: &str) -> ExactError {
    error(
        "INTERNAL_ERROR",
        format!("Unexpected store output: {key} is missing or mistyped"),
    )
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).ok_or_else(|| shape(key))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?.as_str().ok_or_else(|| shape(key))
}
fn integer(value: &Value, key: &str) -> Result<i64> {
    field(value, key)?.as_i64().ok_or_else(|| shape(key))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    field(value, key)?.as_array().ok_or_else(|| shape(key))
}

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

fn period() -> Value {
    json!({"type": "string", "pattern": "^[0-9]{4}-(0[1-9]|1[0-2])$", "description": "YYYY-MM"})
}

pub fn input_schema() -> Value {
    json!({
        "$schema": DRAFT, "type": "object", "additionalProperties": false,
        "required": ["period"],
        "properties": {
            "period": period(),
            "property": {"type": "string", "minLength": 1, "maxLength": 120,
                         "description": "Only this property; omit for every property with a revision for the period"}
        }
    })
}

pub fn output_schema() -> Value {
    let count = json!({"type": "integer", "minimum": 0});
    json!({
        "$schema": DRAFT, "type": "object",
        "required": ["period", "properties", "summary", "review_reasons"],
        "properties": {
            "period": period(),
            "properties": {
                "type": "array",
                "description": "One entry per property with a revision for the period, using its latest revision",
                "items": {
                    "type": "object",
                    "required": ["property", "revision_id", "revision", "is_correction",
                                 "correction_reason", "actuals_present", "budgets_present",
                                 "snapshot", "account_coverage", "blocking_review_reasons",
                                 "issued_reports", "status", "not_tracked_in_exact_store"],
                    "properties": {
                        "property": {"type": "string"},
                        "revision_id": {"type": "string"},
                        "revision": {"type": "integer", "minimum": 1},
                        "is_correction": {"type": "boolean"},
                        "correction_reason": {"type": ["string", "null"]},
                        "actuals_present": {"type": "boolean"},
                        "budgets_present": {"type": "boolean"},
                        "snapshot": {"enum": ["present", "missing", "outside_period"]},
                        "account_coverage": {
                            "type": "object",
                            "required": ["matched", "actual_only", "budget_only"],
                            "properties": {
                                "matched": count, "actual_only": count, "budget_only": count
                            },
                            "description": "Distinct account codes: in both actuals and budgets, only actuals, only budgets"
                        },
                        "blocking_review_reasons": {"type": "array", "items": {"type": "string"},
                            "description": "variance.review_reasons from review_period for this revision"},
                        "issued_reports": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["id", "created_at"],
                                "properties": {
                                    "id": {"type": "string"}, "created_at": {"type": "string"}
                                }
                            }
                        },
                        "status": {"enum": ["ready", "not_ready"]},
                        "not_tracked_in_exact_store": {
                            "type": "array", "items": {"type": "string"},
                            "description": "Readiness inputs the legacy check covers that this store does not hold"
                        }
                    }
                }
            },
            "summary": {
                "type": "object",
                "required": ["ready", "not_ready", "no_revisions"],
                "properties": {
                    "ready": count, "not_ready": count,
                    "no_revisions": {"type": "integer", "minimum": 0, "maximum": 1,
                                     "description": "1 when no property had a revision for the period, else 0"}
                }
            },
            "review_reasons": {"type": "array", "items": {"enum": ["no_revisions_for_period", "properties_not_ready"]}}
        }
    })
}
