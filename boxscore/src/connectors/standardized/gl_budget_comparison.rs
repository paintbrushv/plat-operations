use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    account_review::record_unmapped_account_for_review,
    connectors::standardized::{
        account_mapping::suggest_category, source_registry::PropertyLane, StandardizedIngestSummary,
    },
    db,
};

const SOURCE_SYSTEM: &str = "standardized-yardi";

#[derive(Debug, Clone, Deserialize)]
struct BudgetComparisonCsvRow {
    account_code: String,
    description: String,
    ptd_actual: String,
    ptd_budget: String,
    period: String,
    property_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetComparisonRow {
    pub account_code: String,
    pub description: String,
    pub ptd_actual: f64,
    /// `None` means the source carries no budget for this row (an empty
    /// cell), e.g. the 12-month statement adapter. An explicit `0`/`()` in
    /// the source is a real zero budget and parses as `Some(0.0)`.
    pub ptd_budget: Option<f64>,
    pub period: String,
    pub property_id: String,
    pub source_row: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkippedBudgetComparisonRow {
    pub source_row: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedBudgetComparison {
    pub rows: Vec<BudgetComparisonRow>,
    pub skipped: Vec<SkippedBudgetComparisonRow>,
}

pub fn read_budget_comparison_rows(path: &Path) -> Result<ParsedBudgetComparison> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open budget comparison CSV: {}", path.display()))?;
    let mut rows = Vec::new();
    let mut skipped = Vec::new();

    for (index, row) in reader.deserialize::<BudgetComparisonCsvRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row) => match parse_row(row, source_row) {
                Ok(parsed) => rows.push(parsed),
                Err(err) => skipped.push(SkippedBudgetComparisonRow {
                    source_row,
                    reason: err.to_string(),
                }),
            },
            Err(err) => skipped.push(SkippedBudgetComparisonRow {
                source_row,
                reason: err.to_string(),
            }),
        }
    }

    Ok(ParsedBudgetComparison { rows, skipped })
}

pub async fn ingest_budget_comparison_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
) -> Result<StandardizedIngestSummary> {
    ingest_budget_comparison_file(
        pool,
        lane,
        &lane.standardized_path.join("budget_comparison.csv"),
    )
    .await
}

pub async fn ingest_budget_comparison_file(
    pool: &SqlitePool,
    lane: &PropertyLane,
    path: &Path,
) -> Result<StandardizedIngestSummary> {
    let parsed = read_budget_comparison_rows(path)?;
    let task_run_id = db::create_task_run(
        pool,
        "standardized_gl_ingest",
        &format!("Ingest standardized GL bridge for {}", lane.display_name),
    )
    .await?;
    let property_id = db::upsert_property(
        pool,
        &lane.display_name,
        "Unknown",
        i64::from(lane.unit_count_hint.unwrap_or_default()),
        "Example Sponsor",
        "Unknown",
    )
    .await?;
    let source_file = path.to_string_lossy().to_string();
    let mut rows_inserted = 0;
    let mut gaps_created = 0;

    // Re-ingest idempotency: replace any rows previously loaded from this
    // exact source file instead of duplicating them (which would double
    // every actual/budget amount in variance analysis).
    for table in ["gl_actuals", "gl_budgets"] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE property_id = ? AND source_file = ?"
        ))
        .bind(&property_id)
        .bind(&source_file)
        .execute(pool)
        .await?;
    }

    for skipped in &parsed.skipped {
        // Subtotal rows are skipped by design, not a data-quality problem.
        if skipped.reason == SUBTOTAL_SKIP_REASON {
            continue;
        }
        db::insert_gap(
            pool,
            &task_run_id,
            "invalid_budget_comparison_row",
            "medium",
            &format!(
                "Skipped standardized budget comparison row {}: {}",
                skipped.source_row, skipped.reason
            ),
            "Invalid GL bridge rows reduce variance completeness.",
            "Fix the standardized CSV row or upstream ETL and re-run standardized GL ingestion.",
        )
        .await?;
        gaps_created += 1;
    }

    for row in &parsed.rows {
        let period_id = db::upsert_period(pool, &row.period).await?;
        let category =
            resolve_category(pool, &row.property_id, &row.account_code, &row.description).await?;
        if category == "Unmapped" {
            db::insert_gap(
                pool,
                &task_run_id,
                "missing_account_mapping",
                "medium",
                &format!(
                    "Account {} ({}) is not mapped to the Boxscore ontology.",
                    row.account_code, row.description
                ),
                "Unmapped accounts can misclassify NOI drivers and weaken owner reporting.",
                &format!(
                    "Run `boxscore accounts map --account-code {} --category <category> --scope {}`.",
                    row.account_code, row.property_id
                ),
            )
            .await?;
            db::insert_question(
                pool,
                &task_run_id,
                &format!(
                    "How should account {} ({}) be mapped in Boxscore for scope {}?",
                    row.account_code, row.description, row.property_id
                ),
                "The standardized GL bridge found an unmapped account, and category assignment affects NOI driver classification.",
                3,
            )
            .await?;
            gaps_created += 1;
        }

        insert_gl_line(
            pool,
            NewGlLine {
                table: "gl_actuals",
                property_id: &property_id,
                period_id: &period_id,
                row,
                category: &category,
                amount: row.ptd_actual,
                source_file: &source_file,
            },
        )
        .await?;
        rows_inserted += 1;
        // A missing budget cell means no budget exists for this row; not
        // inserting anything keeps Budget GL honestly stale/missing in
        // close readiness instead of faking a zero budget.
        if let Some(ptd_budget) = row.ptd_budget {
            insert_gl_line(
                pool,
                NewGlLine {
                    table: "gl_budgets",
                    property_id: &property_id,
                    period_id: &period_id,
                    row,
                    category: &category,
                    amount: ptd_budget,
                    source_file: &source_file,
                },
            )
            .await?;
            rows_inserted += 1;
        }
    }

    db::complete_task_run(
        pool,
        &task_run_id,
        "completed",
        Some(if gaps_created == 0 { 0.90 } else { 0.65 }),
        Some(&format!(
            "Inserted {rows_inserted} GL rows from {} with {} skipped rows and {gaps_created} gaps.",
            path.display(),
            parsed.skipped.len()
        )),
    )
    .await?;

    Ok(StandardizedIngestSummary {
        lane: lane.property_key.clone(),
        source_file,
        rows_seen: parsed.rows.len() + parsed.skipped.len(),
        rows_inserted,
        rows_skipped: parsed.skipped.len(),
        gaps_created,
    })
}

pub fn parse_period_label(label: &str) -> Result<String> {
    let mut parts = label.split_whitespace();
    let month_name = parts
        .next()
        .ok_or_else(|| anyhow!("period is missing month name"))?;
    let year = parts
        .next()
        .ok_or_else(|| anyhow!("period is missing year"))?;
    if parts.next().is_some() {
        return Err(anyhow!("period must be like `Mar 2026`"));
    }
    let month = match month_name.to_ascii_lowercase().as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return Err(anyhow!("unknown month name: {month_name}")),
    };
    let mut year = year.parse::<i64>()?;
    // Yardi sometimes emits two-digit years ("Mar 26"); without this guard
    // they would be stored as year 26 and never match a requested period.
    if (0..=99).contains(&year) {
        year += 2000;
    }
    if !(1990..=2100).contains(&year) {
        return Err(anyhow!("implausible period year: {year}"));
    }
    Ok(format!("{year:04}-{month:02}"))
}

async fn resolve_category(
    pool: &SqlitePool,
    property_scope: &str,
    account_code: &str,
    account_name: &str,
) -> Result<String> {
    if let Some(mapping) =
        db::find_account_mapping(pool, SOURCE_SYSTEM, property_scope, account_code).await?
    {
        if mapping.status == "approved" {
            return Ok(mapping.noi_category);
        }
    }
    if let Some(suggestion) = suggest_category(account_code, account_name) {
        db::upsert_account_mapping(
            pool,
            db::NewAccountMapping {
                source_system: SOURCE_SYSTEM,
                property_scope,
                account_code,
                account_name,
                noi_category: &suggestion.category,
                confidence_score: suggestion.confidence_score,
                status: "suggested",
            },
        )
        .await?;
        return Ok(suggestion.category);
    }
    record_unmapped_account_for_review(pool, property_scope, account_code, account_name).await?;
    Ok("Unmapped".to_string())
}

struct NewGlLine<'a> {
    table: &'a str,
    property_id: &'a str,
    period_id: &'a str,
    row: &'a BudgetComparisonRow,
    category: &'a str,
    amount: f64,
    source_file: &'a str,
}

async fn insert_gl_line(pool: &SqlitePool, line: NewGlLine<'_>) -> Result<()> {
    sqlx::query(&format!(
        "INSERT INTO {} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        line.table
    ))
    .bind(db::new_id())
    .bind(line.property_id)
    .bind(line.period_id)
    .bind(line.row.account_code.trim())
    .bind(line.row.description.trim())
    .bind(line.category)
    .bind(line.amount)
    .bind(line.source_file)
    .bind(line.row.source_row)
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(())
}

fn parse_row(row: BudgetComparisonCsvRow, source_row: i64) -> Result<BudgetComparisonRow> {
    if row.account_code.trim().is_empty() || row.description.trim().is_empty() {
        return Err(anyhow!("missing account code or description"));
    }
    if is_subtotal_row(&row.account_code, &row.description) {
        return Err(anyhow!(SUBTOTAL_SKIP_REASON));
    }
    // An empty budget cell means "no budget exists for this row" (the
    // 12-month statement adapter emits these); "-" and "()" are accounting
    // zeros and stay real budget rows.
    let ptd_budget = if row.ptd_budget.trim().is_empty() {
        None
    } else {
        Some(parse_money(&row.ptd_budget)?)
    };
    Ok(BudgetComparisonRow {
        account_code: row.account_code.trim().to_string(),
        description: row.description.trim().to_string(),
        ptd_actual: parse_money(&row.ptd_actual)?,
        ptd_budget,
        period: parse_period_label(row.period.trim())?,
        property_id: row.property_id.trim().to_string(),
        source_row,
    })
}

fn parse_money(value: &str) -> Result<f64> {
    crate::parse::parse_money(value)
}

pub const SUBTOTAL_SKIP_REASON: &str =
    "subtotal row excluded to prevent double counting against its component accounts";

/// Yardi budget-comparison exports interleave subtotal rows ("TOTAL PAYROLL
/// EXPENSE", "NET RENTAL INCOME", "Total Utilities") with the component
/// accounts they sum. Ingesting both would double or triple count every
/// revenue and expense total.
///
/// Detection works off BOTH the description and the account code, because some
/// Yardi rollup rows do not start with TOTAL/NET in their name:
/// - "EFFECTIVE GROSS INCOME" (5990-9999) and "OTHER INCOME" (5051-9999) are
///   rollups whose names alone would otherwise be auto-mapped into revenue,
///   double counting EGI and the Other Income subtotal.
/// - Yardi maplewood/TC rollup codes end in `-9999`, `-9000`, `-1999`, `-9199`,
///   or `-9399`. (We deliberately do NOT key off `-0150`: real operating
///   accounts use it, e.g. 5050-0150 Administrative Fees, 6450-0150 Floor
///   Clean/Repairs; the one `-0150` rollup, 5012-0150 TOTAL GROSS POTENTIAL
///   RENT, is caught by the "total " name token below.)
/// - The trial-balance style exports carry an `is_total_row` flag; here the
///   equivalent signal is the name token / code suffix.
fn is_subtotal_row(account_code: &str, description: &str) -> bool {
    let code = account_code.trim();
    if code.ends_with("-9999")
        || code.ends_with("-9000")
        || code.ends_with("-1999")
        || code.ends_with("-9199")
        || code.ends_with("-9399")
    {
        return true;
    }

    let lowered = description.trim().to_ascii_lowercase();
    lowered == "total"
        || lowered.starts_with("total ")
        || lowered.starts_with("net ")
        || lowered.starts_with("subtotal")
        || lowered.contains("effective gross income")
        || lowered.contains("net operating income")
        || lowered.contains("net rental income")
}
