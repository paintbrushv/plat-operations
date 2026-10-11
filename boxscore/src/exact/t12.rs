//! Trailing twelve-period statement over stored exact-cent revisions.
//!
//! For each of the twelve months ending at `end_period` one stored revision
//! is chosen (`latest`, or `issued_only`: the highest revision that has an
//! issued report), verified against its input hash with
//! [`store::load_verified`], and its lines are folded into per-category
//! monthly totals. Money is only combined with `Money::checked_add` /
//! `checked_sub`. A month with no revision is `null` in every row, never
//! zero, and is listed in `missing_periods`; totals cover only the months
//! that are present.
//!
//! Unlike `review_period`, whose NOI bridge is limited to accounts present in
//! both actuals and budgets, this statement reports every stored line.
use super::{
    error,
    money::Money,
    store::{self, Dataset},
    variance::Line,
    Result,
};
use crate::ontology::{account_class, canonical_category, AccountClass, NOI_CATEGORIES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const PERIODS: usize = 12;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionPolicy {
    /// Highest revision number for the property and period.
    #[default]
    Latest,
    /// Highest revision that has at least one issued report.
    IssuedOnly,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    property: String,
    end_period: String,
    #[serde(default)]
    revision_policy: RevisionPolicy,
}

/// The revision picked for one period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub period: String,
    pub revision_id: String,
    pub revision: i64,
}

type Totals = BTreeMap<String, Money>;

#[derive(Debug, Serialize)]
struct Row {
    category: String,
    class: &'static str,
    values: Vec<Option<Money>>,
    total: Money,
}

#[derive(Debug, Serialize)]
struct Statement {
    rows: Vec<Row>,
    noi: Vec<Option<Money>>,
    noi_total: Money,
    unmapped: Vec<Option<Money>>,
}

/// The twelve `YYYY-MM` labels ending at `end_period`, earliest first.
fn window(end_period: &str) -> Result<Vec<String>> {
    let invalid = || error("INVALID_INPUT", "end_period must be YYYY-MM");
    let normalized = crate::db::normalize_period_label(end_period).map_err(|_| invalid())?;
    if normalized != end_period {
        return Err(invalid());
    }
    let (year, month) = crate::db::parse_period_label(end_period).map_err(|_| invalid())?;
    let end = year * 12 + (month - 1);
    Ok((0..PERIODS as i64)
        .rev()
        .map(|back| {
            let ordinal = end - back;
            format!(
                "{:04}-{:02}",
                ordinal.div_euclid(12),
                ordinal.rem_euclid(12) + 1
            )
        })
        .collect())
}

fn internal(what: &str) -> super::ExactError {
    error(
        "INTERNAL_ERROR",
        format!("Revision listing is missing {what}"),
    )
}

/// Choose one revision per period from `store::list_revisions` rows.
fn select(
    revisions: &[Value],
    periods: &[String],
    policy: RevisionPolicy,
) -> Result<Vec<Option<Selected>>> {
    let mut chosen: Vec<Option<Selected>> = vec![None; periods.len()];
    for row in revisions {
        let period = row
            .get("period")
            .and_then(Value::as_str)
            .ok_or_else(|| internal("period"))?;
        let Some(slot) = periods.iter().position(|p| p == period) else {
            continue;
        };
        let revision = row
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| internal("revision"))?;
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| internal("id"))?;
        if policy == RevisionPolicy::IssuedOnly {
            let issued = row
                .get("reports")
                .and_then(Value::as_array)
                .ok_or_else(|| internal("reports"))?;
            if issued.is_empty() {
                continue;
            }
        }
        if chosen[slot].as_ref().is_none_or(|c| revision > c.revision) {
            chosen[slot] = Some(Selected {
                period: period.to_string(),
                revision_id: id.to_string(),
                revision,
            });
        }
    }
    Ok(chosen)
}

/// Category key a stored line contributes to: the reviewed spelling when the
/// category is on the NOI list, otherwise the trimmed lower-case string.
fn key_of(category: &str) -> String {
    canonical_category(category)
        .map(str::to_string)
        .unwrap_or_else(|| category.trim().to_lowercase())
}

fn totals(lines: &[Line]) -> Result<Totals> {
    let mut sums = Totals::new();
    for line in lines {
        let slot = sums.entry(key_of(&line.category)).or_insert(Money::ZERO);
        *slot = slot.checked_add(line.amount)?;
    }
    Ok(sums)
}

fn class_name(class: &AccountClass) -> &'static str {
    match class {
        AccountClass::Revenue => "revenue",
        AccountClass::Expense => "expense",
        AccountClass::Unmapped => "unmapped",
    }
}

/// Revenue first, then expenses, each in `NOI_CATEGORIES` order, then any
/// non-canonical category alphabetically.
fn order_key(key: &str) -> (u8, usize, String) {
    match NOI_CATEGORIES.iter().position(|known| *known == key) {
        Some(index) => {
            let group = if account_class(key) == AccountClass::Revenue {
                0
            } else {
                1
            };
            (group, index, String::new())
        }
        None => (2, 0, key.to_string()),
    }
}

fn sum_present(values: &[Option<Money>]) -> Result<Money> {
    values
        .iter()
        .flatten()
        .try_fold(Money::ZERO, |sum, value| sum.checked_add(*value))
}

/// `None` periods are missing: they stay `None` in every output vector.
fn statement(periods: &[Option<Totals>]) -> Result<Statement> {
    let keys: BTreeSet<&String> = periods.iter().flatten().flat_map(BTreeMap::keys).collect();
    let mut ordered: Vec<&String> = keys.into_iter().collect();
    ordered.sort_by_key(|key| order_key(key));

    let mut rows = Vec::new();
    for key in ordered {
        let values: Vec<Option<Money>> = periods
            .iter()
            .map(|p| {
                p.as_ref()
                    .map(|t| t.get(key).copied().unwrap_or(Money::ZERO))
            })
            .collect();
        // Rows with no activity in any present period are omitted.
        if values.iter().flatten().all(|v| *v == Money::ZERO) {
            continue;
        }
        rows.push(Row {
            category: key.clone(),
            class: class_name(&account_class(key)),
            total: sum_present(&values)?,
            values,
        });
    }

    let mut noi = Vec::with_capacity(periods.len());
    let mut unmapped = Vec::with_capacity(periods.len());
    for period in periods {
        let Some(t) = period else {
            noi.push(None);
            unmapped.push(None);
            continue;
        };
        let (mut revenue, mut expense, mut other) = (Money::ZERO, Money::ZERO, Money::ZERO);
        for (key, amount) in t {
            let target = match account_class(key) {
                AccountClass::Revenue => &mut revenue,
                AccountClass::Expense => &mut expense,
                AccountClass::Unmapped => &mut other,
            };
            *target = target.checked_add(*amount)?;
        }
        noi.push(Some(revenue.checked_sub(expense)?));
        unmapped.push(Some(other));
    }
    Ok(Statement {
        noi_total: sum_present(&noi)?,
        rows,
        noi,
        unmapped,
    })
}

/// Actual minus budget NOI where both periods are present.
fn variance(actuals: &Statement, budgets: &Statement) -> Result<Value> {
    let noi = actuals
        .noi
        .iter()
        .zip(&budgets.noi)
        .map(|pair| match pair {
            (Some(actual), Some(budget)) => actual.checked_sub(*budget).map(Some),
            _ => Ok(None),
        })
        .collect::<Result<Vec<Option<Money>>>>()?;
    Ok(json!({"noi_total": sum_present(&noi)?, "noi": noi}))
}

type Loaded = Vec<Option<(Selected, Dataset)>>;

async fn load(
    pool: &sqlx::SqlitePool,
    property: &str,
    periods: &[String],
    policy: RevisionPolicy,
) -> Result<Loaded> {
    let revisions = store::list_revisions(pool, Some(property), None).await?;
    let chosen = select(&revisions, periods, policy)?;
    let mut loaded = Vec::with_capacity(chosen.len());
    for pick in chosen {
        loaded.push(match pick {
            Some(selected) => {
                let dataset = store::load_verified(pool, &selected.revision_id).await?;
                Some((selected, dataset))
            }
            None => None,
        });
    }
    Ok(loaded)
}

fn compute(property: &str, periods: &[String], loaded: &Loaded) -> Result<(&'static str, Value)> {
    if loaded.iter().all(Option::is_none) {
        return Err(error(
            "NOT_FOUND",
            "No stored revision for this property in the twelve-period window",
        ));
    }
    let actual_totals = loaded
        .iter()
        .map(|slot| slot.as_ref().map(|(_, d)| totals(&d.actuals)).transpose())
        .collect::<Result<Vec<_>>>()?;
    let actuals = statement(&actual_totals)?;

    // Budgets are reported only when every period that has a revision also
    // has budget rows; a partial budget would make the variance misleading.
    let budgets_complete = loaded
        .iter()
        .flatten()
        .all(|(_, dataset)| !dataset.budgets.is_empty());
    let (budgets, variance) = if budgets_complete {
        let budget_totals = loaded
            .iter()
            .map(|slot| slot.as_ref().map(|(_, d)| totals(&d.budgets)).transpose())
            .collect::<Result<Vec<_>>>()?;
        let budgets = statement(&budget_totals)?;
        let variance = variance(&actuals, &budgets)?;
        (serde_json::to_value(budgets)?, variance)
    } else {
        (Value::Null, Value::Null)
    };

    let missing: Vec<&String> = periods
        .iter()
        .zip(loaded)
        .filter(|(_, slot)| slot.is_none())
        .map(|(period, _)| period)
        .collect();
    let review_reasons: Vec<&str> = if missing.is_empty() {
        Vec::new()
    } else {
        vec!["missing_periods"]
    };
    let revisions: Vec<Value> = periods
        .iter()
        .zip(loaded)
        .map(|(period, slot)| match slot {
            Some((selected, _)) => json!({
                "period": period,
                "revision_id": selected.revision_id,
                "revision": selected.revision,
            }),
            None => json!({"period": period, "revision_id": null, "revision": null}),
        })
        .collect();
    let status = if review_reasons.is_empty() {
        "calculated"
    } else {
        "review_required"
    };
    Ok((
        status,
        json!({
            "property": property,
            "periods": periods,
            "revisions": revisions,
            "actuals": actuals,
            "budgets": budgets,
            "variance": variance,
            "review_reasons": review_reasons,
            "missing_periods": missing,
        }),
    ))
}

pub async fn run(database: &Path, input: Input) -> Result<(&'static str, Value)> {
    let periods = window(&input.end_period)?;
    let pool = store::open(database, true).await?;
    let outcome = load(&pool, &input.property, &periods, input.revision_policy).await;
    pool.close().await;
    compute(&input.property, &periods, &outcome?)
}

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

fn period() -> Value {
    json!({"type": "string", "pattern": "^[0-9]{4}-(0[1-9]|1[0-2])$", "description": "YYYY-MM"})
}

fn money() -> Value {
    super::catalog::schemas::money()
}

fn optional_money() -> Value {
    json!({"anyOf": [money(), {"type": "null"}]})
}

fn twelve(items: Value) -> Value {
    json!({"type": "array", "items": items, "minItems": PERIODS, "maxItems": PERIODS})
}

fn statement_schema() -> Value {
    json!({
        "type": "object",
        "required": ["rows", "noi", "noi_total", "unmapped"],
        "properties": {
            "rows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["category", "class", "values", "total"],
                    "properties": {
                        "category": {"type": "string"},
                        "class": {"enum": ["revenue", "expense", "unmapped"]},
                        "values": twelve(optional_money()),
                        "total": money()
                    }
                }
            },
            "noi": twelve(optional_money()),
            "noi_total": money(),
            "unmapped": twelve(optional_money())
        }
    })
}

pub fn input_schema() -> Value {
    json!({
        "$schema": DRAFT, "type": "object", "additionalProperties": false,
        "required": ["property", "end_period"],
        "properties": {
            "property": {"type": "string", "minLength": 1},
            "end_period": period(),
            "revision_policy": {
                "enum": ["latest", "issued_only"], "default": "latest",
                "description": "latest: highest revision per period; issued_only: highest revision with an issued report"
            }
        }
    })
}

pub fn output_schema() -> Value {
    json!({
        "$schema": DRAFT, "type": "object",
        "required": ["property", "periods", "revisions", "actuals", "budgets", "variance",
                     "review_reasons", "missing_periods"],
        "properties": {
            "property": {"type": "string"},
            "periods": twelve(period()),
            "revisions": twelve(json!({
                "type": "object",
                "required": ["period", "revision_id", "revision"],
                "properties": {
                    "period": period(),
                    "revision_id": {"type": ["string", "null"]},
                    "revision": {"type": ["integer", "null"], "minimum": 1}
                }
            })),
            "actuals": statement_schema(),
            "budgets": {"anyOf": [statement_schema(), {"type": "null"}],
                        "description": "Present only when every period with a revision has budget rows"},
            "variance": {
                "anyOf": [{
                    "type": "object",
                    "required": ["noi", "noi_total"],
                    "properties": {"noi": twelve(optional_money()), "noi_total": money()},
                    "description": "Actual minus budget NOI where both are present"
                }, {"type": "null"}]
            },
            "review_reasons": {"type": "array", "items": {"enum": ["missing_periods"]}},
            "missing_periods": {"type": "array", "items": period()}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_spans_the_year_boundary_earliest_first() {
        let w = window("2026-05").unwrap_or_default();
        assert_eq!(w.len(), 12);
        assert_eq!(w[0], "2025-06");
        assert_eq!(w[11], "2026-05");
        let w = window("2026-01").unwrap_or_default();
        assert_eq!((w[0].as_str(), w[2].as_str()), ("2025-02", "2025-04"));
    }

    #[test]
    fn window_refuses_non_canonical_labels() {
        for bad in ["2026-5", "2026-13", " 2026-05", "2026/05", "", "x"] {
            assert_eq!(window(bad).err().map(|e| e.code), Some("INVALID_INPUT"));
        }
    }
}
