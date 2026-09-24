//! Standardized per-unit P&L adapter.
//!
//! Reads `Standardized/unit_pnl_annual.csv` and inserts one [`unit_pnl`] row per
//! unit-period (the source grain is unit × year). Only unit economics are
//! retained — unit number, period (year), income, the two expense components, and
//! NOI. NO resident code / name / balance is read or stored: this is a
//! unit-level financial feed, never a resident-level one.
//!
//! Column names vary per property and are resolved by name with a small candidate
//! list. maplewood/juniper_fund carry the rich schema
//! (`unit, year, total_income, direct_expense, allocated_expense, total_expense,
//! noi, noi_margin`); Willow Brook carries a leaner one
//! (`unit, year, income, expense, noi, margin`) where only income / NOI map and
//! the direct/allocated expense split is absent (those columns degrade to NULL
//! rather than erroring). A numeric cell that is present but non-numeric (e.g.
//! "N/A") is stored as NULL, never coerced to $0 — a coerced zero would be a
//! fabricated figure that misstates the unit's NOI.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::{
    connectors::standardized::{source_registry::PropertyLane, StandardizedIngestSummary},
    db,
};

pub const SOURCE_FILE_NAME: &str = "unit_pnl_annual.csv";

/// One parsed unit-P&L row. `unit`/`period` are optional because a row may omit
/// them (a blank/footer line); the numeric figures are `None` (→ SQL NULL) when
/// the cell is empty OR present-but-unparseable — they are NEVER coerced to 0.0,
/// so a NULL NOI is distinguishable from a true $0 NOI.
#[derive(Debug, Clone)]
pub struct UnitPnlRow {
    pub unit: Option<String>,
    pub period: Option<String>,
    pub total_income: Option<f64>,
    pub direct_expense: Option<f64>,
    pub allocated_expense: Option<f64>,
    pub noi: Option<f64>,
    pub source_row: i64,
}

/// Parse `unit_pnl_annual.csv` into one [`UnitPnlRow`] per data line.
///
/// A row is only meaningful when it carries a `unit`; fully-blank rows (e.g. a
/// trailing footer) are dropped and surface as a skip in the connector's gap
/// count.
pub fn parse_unit_pnl(csv_path: &Path) -> Result<Vec<UnitPnlRow>> {
    let mut reader = crate::parse::csv_reader_from_path(csv_path)
        .with_context(|| format!("failed to open unit P&L CSV: {}", csv_path.display()))?;
    let headers = reader.headers()?.clone();

    let unit_idx = header_index(&headers, &["unit", "unit_label"]);
    let period_idx = header_index(&headers, &["year", "period"]);
    let total_income_idx = header_index(&headers, &["total_income", "income"]);
    let direct_expense_idx = header_index(&headers, &["direct_expense"]);
    let allocated_expense_idx = header_index(&headers, &["allocated_expense"]);
    let noi_idx = header_index(&headers, &["noi"]);

    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record?;
        let source_row = index as i64 + 2; // 1-based + header offset

        let unit = unit_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let period = period_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Numeric cells are stored as NULL (None) when the cell is empty OR
        // non-empty-but-unparseable (e.g. "N/A"). We use the STRICT parser
        // (`parse_money` → `.ok()`) deliberately: the lenient parser would coerce
        // an unparseable figure to $0, fabricating a value that misstates NOI.
        // Accounting blanks (`-`, `()`) still parse to a legitimate 0.0; only
        // genuinely non-numeric text becomes NULL.
        let total_income = parse_money_cell(&record, total_income_idx);
        let direct_expense = parse_money_cell(&record, direct_expense_idx);
        let allocated_expense = parse_money_cell(&record, allocated_expense_idx);
        let noi = parse_money_cell(&record, noi_idx);

        rows.push(UnitPnlRow {
            unit,
            period,
            total_income,
            direct_expense,
            allocated_expense,
            noi,
            source_row,
        });
    }
    Ok(rows)
}

/// Ingest a lane's `unit_pnl_annual.csv` into the `unit_pnl` table.
///
/// Property attribution mirrors the turn-costs connector: the lane's display name
/// resolves (and upserts) the property — the source CSV is not consulted for
/// attribution. Re-ingesting the same file is idempotent (prior rows from this
/// source_file are deleted first). Rows with no `unit` are skipped and counted as
/// a gap.
pub async fn ingest_unit_pnl_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
) -> Result<StandardizedIngestSummary> {
    let path = lane.standardized_path.join(SOURCE_FILE_NAME);
    let task_run_id = db::create_task_run(
        pool,
        "standardized_unit_pnl_ingest",
        &format!("Ingest standardized unit P&L for {}", lane.display_name),
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
    let parsed =
        parse_unit_pnl(&path).with_context(|| format!("failed to parse {}", path.display()))?;
    let rows_seen = parsed.len();

    // Idempotency: replace, never duplicate, rows from this same source file.
    sqlx::query("DELETE FROM unit_pnl WHERE source_file = ?")
        .bind(&source_file)
        .execute(pool)
        .await?;

    let mut inserted = 0_usize;
    let mut skipped = 0_usize;
    let mut gaps_created = 0_usize;

    for row in &parsed {
        // A unit-P&L row with no unit carries no usable signal (it cannot be
        // attributed to a unit), so it is skipped rather than inserted.
        let Some(unit) = row.unit.as_deref() else {
            skipped += 1;
            gaps_created += db::insert_gap(
                pool,
                &task_run_id,
                "missing_unit_pnl_row",
                "low",
                &format!(
                    "{SOURCE_FILE_NAME} row {} for {} has no unit; skipped.",
                    row.source_row, lane.display_name
                ),
                "A unit-P&L row with no unit cannot be attributed, so it cannot inform per-unit \
                 NOI analysis.",
                "Confirm the upstream unit-P&L export drops blank/footer rows, or add the missing \
                 unit, then re-run ingestion.",
            )
            .await
            .map(|_| 1)
            .unwrap_or(0);
            continue;
        };
        sqlx::query(
            "INSERT INTO unit_pnl (id, property_id, unit, period, total_income, direct_expense, allocated_expense, noi, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(db::new_id())
        .bind(&property_id)
        .bind(unit)
        .bind(row.period.as_deref())
        .bind(row.total_income)
        .bind(row.direct_expense)
        .bind(row.allocated_expense)
        .bind(row.noi)
        .bind(&source_file)
        .bind(row.source_row)
        .bind(db::now_iso())
        .execute(pool)
        .await?;
        inserted += 1;
    }

    db::complete_task_run(
        pool,
        &task_run_id,
        "completed",
        Some(if skipped == 0 { 0.90 } else { 0.72 }),
        Some(&format!(
            "Inserted {inserted} unit-P&L rows for {} from {rows_seen} source rows ({skipped} skipped, {gaps_created} gaps).",
            lane.display_name
        )),
    )
    .await?;

    Ok(StandardizedIngestSummary {
        lane: lane.property_key.clone(),
        source_file: SOURCE_FILE_NAME.to_string(),
        rows_seen,
        rows_inserted: inserted,
        rows_skipped: skipped,
        gaps_created,
    })
}

fn parse_money_cell(record: &csv::StringRecord, idx: Option<usize>) -> Option<f64> {
    idx.and_then(|idx| cell(record, idx))
        .filter(|s| !s.is_empty())
        .and_then(|s| crate::parse::parse_money(s).ok())
}

fn header_index(headers: &csv::StringRecord, candidates: &[&str]) -> Option<usize> {
    candidates.iter().find_map(|candidate| {
        let normalized_candidate = normalize_header(candidate);
        headers
            .iter()
            .position(|header| normalize_header(header) == normalized_candidate)
    })
}

fn normalize_header(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-', '/'], "_")
}

fn cell(record: &csv::StringRecord, idx: usize) -> Option<&str> {
    record.get(idx).map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_rich_schema_and_drops_blank() {
        // maplewood/juniper_fund-shaped file (rich schema).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unit_pnl_annual.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "unit,year,total_income,transaction_count,direct_expense,allocated_expense,total_expense,noi,noi_margin"
        )
        .unwrap();
        writeln!(
            f,
            "C00101,2025,20321.91,175.0,0.0,14739.81,14739.81,5582.10,0.27"
        )
        .unwrap();
        writeln!(f, ",,,,,,,,").unwrap(); // blank/footer row → no unit
        f.flush().unwrap();

        let rows = parse_unit_pnl(&path).unwrap();
        assert_eq!(
            rows.len(),
            2,
            "parser returns every line; connector skips blanks"
        );

        let first = &rows[0];
        assert_eq!(first.unit.as_deref(), Some("C00101"));
        assert_eq!(first.period.as_deref(), Some("2025"));
        assert_eq!(first.total_income, Some(20321.91));
        assert_eq!(first.direct_expense, Some(0.0));
        assert_eq!(first.allocated_expense, Some(14739.81));
        assert_eq!(first.noi, Some(5582.10));

        let blank = &rows[1];
        assert!(blank.unit.is_none());
    }

    #[test]
    fn parses_lean_terrace_schema() {
        // Willow Brook-shaped file: only income / expense / noi; no direct/allocated split.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unit_pnl_annual.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "unit,year,income,expense,noi,margin").unwrap();
        writeln!(f, "1010,2024,12000.0,8000.0,4000.0,0.33").unwrap();
        f.flush().unwrap();

        let rows = parse_unit_pnl(&path).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.unit.as_deref(), Some("1010"));
        assert_eq!(r.total_income, Some(12000.0));
        assert_eq!(r.noi, Some(4000.0));
        // No direct/allocated split in the lean schema → NULL, not 0.
        assert_eq!(r.direct_expense, None);
        assert_eq!(r.allocated_expense, None);
    }

    #[test]
    fn unparseable_numeric_is_null_not_zero() {
        // A present-but-non-numeric NOI cell ("N/A") must parse to NULL (None),
        // never be coerced to 0.0 — a coerced $0 NOI is a fabricated figure.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unit_pnl_annual.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "unit,year,total_income,direct_expense,allocated_expense,noi"
        )
        .unwrap();
        writeln!(f, "C00101,2025,N/A,0.0,100.0,N/A").unwrap();
        f.flush().unwrap();

        let rows = parse_unit_pnl(&path).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.total_income, None, "unparseable income must be NULL");
        assert_eq!(r.noi, None, "unparseable NOI must be NULL, not 0.0");
        // The row is still a real unit (unit + period present).
        assert_eq!(r.unit.as_deref(), Some("C00101"));
        assert_eq!(r.period.as_deref(), Some("2025"));
    }
}
