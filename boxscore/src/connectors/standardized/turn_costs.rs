//! Standardized turn-cost (turnover) adapter.
//!
//! Reads `Standardized/turn_costs_summary.csv` and inserts one [`turn_costs`]
//! row per make-ready / turn event. Resident codes in the source (prior_tenant /
//! new_tenant) are intentionally dropped — only unit, turn_date, the turn-cost
//! figures, and vacancy days are retained.
//!
//! Column names vary slightly per property (juniper_fund carries a leading
//! `property_id` column; maplewood/Willow Brook do not), so headers are resolved by
//! name with a small candidate list and missing optional columns degrade
//! gracefully to NULL rather than erroring. A cost/vacancy cell that is present
//! but non-numeric (e.g. "N/A", "pending") is stored as NULL, never coerced to
//! $0 — a coerced zero would be a fabricated figure that understates averages.
//!
//! juniper_fund is a FUND, not a single asset: its turn feed blends four
//! sub-properties (Central Station, Places at Red Rock, Silver Oaks, Steel City
//! Flats) into the one "juniper_fund" fund-level property the DB models. That blend
//! is intentional and consistent with the fund-level data model — but it is made
//! EXPLICIT at ingest: when a lane's source rows carry more than one distinct
//! source `property_id`, a low-severity data-quality gap is recorded so the
//! blend is surfaced rather than silent. (When the source has no `property_id`
//! column, as with maplewood/Willow Brook, the check is skipped.)

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::{
    connectors::standardized::{source_registry::PropertyLane, StandardizedIngestSummary},
    db,
};

pub const SOURCE_FILE_NAME: &str = "turn_costs_summary.csv";

/// One parsed turn row. `unit`/`turn_date` are optional because a row may omit
/// them; the numeric figures are `None` (→ SQL NULL) when the cell is empty OR
/// present-but-unparseable — they are NEVER coerced to 0.0, so a NULL cost is
/// distinguishable from a true $0 cost and is excluded from cost averages.
#[derive(Debug, Clone)]
pub struct TurnCostRow {
    pub unit: Option<String>,
    pub turn_date: Option<String>,
    pub turn_cost_total: Option<f64>,
    pub vacancy_days: Option<i64>,
    pub total_turn_impact: Option<f64>,
    /// Source `property_id` for this row when the CSV carries that column (used
    /// only to detect a fund-level blend at ingest; attribution still resolves to
    /// the lane's property). `None` when the source has no `property_id` column.
    pub source_property_id: Option<String>,
    pub source_row: i64,
}

/// Parse `turn_costs_summary.csv` into one [`TurnCostRow`] per data line.
///
/// A row is only meaningful when it carries at least a `unit` or `turn_date`;
/// fully-blank rows (e.g. a trailing footer) are dropped and surface as a skip
/// in the connector's gap count.
pub fn parse_turn_costs(csv_path: &Path) -> Result<Vec<TurnCostRow>> {
    let mut reader = crate::parse::csv_reader_from_path(csv_path)
        .with_context(|| format!("failed to open turn costs CSV: {}", csv_path.display()))?;
    let headers = reader.headers()?.clone();

    let unit_idx = header_index(&headers, &["unit", "unit_label"]);
    let turn_date_idx = header_index(&headers, &["turn_date"]);
    let turn_cost_total_idx = header_index(&headers, &["turn_cost_total"]);
    let vacancy_days_idx = header_index(&headers, &["vacancy_days"]);
    let total_turn_impact_idx = header_index(&headers, &["total_turn_impact"]);
    // Optional: only present on fund lanes (e.g. juniper_fund). Read for the
    // blend-detection check only — attribution still resolves to the lane.
    let source_property_id_idx = header_index(&headers, &["property_id"]);

    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record?;
        let source_row = index as i64 + 2; // 1-based + header offset

        let unit = unit_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let turn_date = turn_date_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Cost/numeric cells are stored as NULL (None) when the cell is empty OR
        // non-empty-but-unparseable (e.g. "N/A", "pending"). We use the STRICT
        // parser (`parse_money` → `.ok()`) deliberately: the lenient parser would
        // coerce an unparseable cost to $0, fabricating a figure that then
        // understates the average. Accounting blanks (`-`, `()`) still parse to a
        // legitimate 0.0; only genuinely non-numeric text becomes NULL.
        let turn_cost_total = turn_cost_total_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .and_then(|s| crate::parse::parse_money(s).ok());
        // vacancy_days arrives as an integer or float-with-trailing-zero (e.g.
        // "26.0" on juniper_fund); round to the nearest whole day. NULL when empty
        // or unparseable (so it does not dilute the vacancy-day average as a 0).
        let vacancy_days = vacancy_days_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .and_then(|s| crate::parse::parse_money(s).ok())
            .map(|v| v.round() as i64);
        let total_turn_impact = total_turn_impact_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .and_then(|s| crate::parse::parse_money(s).ok());

        let source_property_id = source_property_id_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        rows.push(TurnCostRow {
            unit,
            turn_date,
            source_property_id,
            turn_cost_total,
            vacancy_days,
            total_turn_impact,
            source_row,
        });
    }
    Ok(rows)
}

/// Ingest a lane's `turn_costs_summary.csv` into the `turn_costs` table.
///
/// Property attribution mirrors the operating-snapshots connector: the lane's
/// display name resolves (and upserts) the property — the source CSV's own
/// `property_id` column is NOT used for attribution. Re-ingesting the same file
/// is idempotent (prior rows from this source_file are deleted first). Rows with
/// no `unit` AND no `turn_date` are skipped and counted as a gap.
///
/// Fund-blend disclosure: when the source rows carry more than one distinct
/// `property_id` (only juniper_fund does — a fund of four sub-properties), a
/// low-severity data-quality gap is recorded so the blend into one fund-level
/// property is explicit, not silent.
pub async fn ingest_turn_costs_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
) -> Result<StandardizedIngestSummary> {
    let path = lane.standardized_path.join(SOURCE_FILE_NAME);
    let task_run_id = db::create_task_run(
        pool,
        "standardized_turn_costs_ingest",
        &format!("Ingest standardized turn costs for {}", lane.display_name),
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
        parse_turn_costs(&path).with_context(|| format!("failed to parse {}", path.display()))?;
    let rows_seen = parsed.len();

    // Idempotency: replace, never duplicate, rows from this same source file.
    sqlx::query("DELETE FROM turn_costs WHERE source_file = ?")
        .bind(&source_file)
        .execute(pool)
        .await?;

    let mut inserted = 0_usize;
    let mut skipped = 0_usize;
    let mut gaps_created = 0_usize;

    // Fund-blend disclosure: if the source carries a property_id column and the
    // rows span more than one distinct id, surface the blend as a low-severity
    // data-quality gap. (juniper_fund is a fund of 4 sub-properties modeled as one
    // fund-level property — the blend is correct, but it must be visible.)
    let distinct_source_props: std::collections::BTreeSet<&str> = parsed
        .iter()
        .filter_map(|r| r.source_property_id.as_deref())
        .collect();
    if distinct_source_props.len() > 1 {
        gaps_created += db::insert_gap(
            pool,
            &task_run_id,
            "turn_feed_fund_blend",
            "low",
            &format!(
                "{SOURCE_FILE_NAME} for {} blends {} sub-properties ({}) into the single fund-level property.",
                lane.display_name,
                distinct_source_props.len(),
                distinct_source_props
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            "The turn feed spans multiple source properties aggregated into one fund-level \
             property; a cross-property turn-cost comparison against this lane is fund-vs-asset, \
             not asset-vs-asset, and must be read as such.",
            "This blend is intentional for fund-level lanes (e.g. juniper_fund). Confirm the \
             fund-level rollup is the intended grain; no action needed if so.",
        )
        .await
        .map(|_| 1)
        .unwrap_or(0);
    }

    for row in &parsed {
        // A turn row with neither unit nor turn_date carries no usable signal.
        if row.unit.is_none() && row.turn_date.is_none() {
            skipped += 1;
            gaps_created += db::insert_gap(
                pool,
                &task_run_id,
                "missing_turn_cost_row",
                "low",
                &format!(
                    "{SOURCE_FILE_NAME} row {} for {} has neither unit nor turn_date; skipped.",
                    row.source_row, lane.display_name
                ),
                "A turn row with no unit or date cannot be attributed to a make-ready event, \
                 so it cannot inform turn-cost or vacancy analysis.",
                "Confirm the upstream turn export drops blank/footer rows, or add the missing \
                 unit/turn_date, then re-run ingestion.",
            )
            .await
            .map(|_| 1)
            .unwrap_or(0);
            continue;
        }
        sqlx::query(
            "INSERT INTO turn_costs (id, property_id, unit, turn_date, turn_cost_total, vacancy_days, total_turn_impact, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(db::new_id())
        .bind(&property_id)
        .bind(row.unit.as_deref())
        .bind(row.turn_date.as_deref())
        .bind(row.turn_cost_total)
        .bind(row.vacancy_days)
        .bind(row.total_turn_impact)
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
            "Inserted {inserted} turn rows for {} from {rows_seen} source rows ({skipped} skipped, {gaps_created} gaps).",
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
    fn parses_turn_rows_and_drops_blank() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn_costs_summary.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "unit,turn_date,turn_cost_total,prior_tenant,new_tenant,vacancy_days,total_turn_impact"
        )
        .unwrap();
        writeln!(f, "C00114,2025-12-15,100.0,t1,t2,10,434.7").unwrap();
        writeln!(f, "C00317,2025-12-23,85.0,t3,t4,7,288.9").unwrap();
        writeln!(f, ",,,,,,").unwrap(); // blank/footer row → no unit, no date
        f.flush().unwrap();

        let rows = parse_turn_costs(&path).unwrap();
        assert_eq!(
            rows.len(),
            3,
            "parser returns every line; connector skips blanks"
        );

        let first = &rows[0];
        assert_eq!(first.unit.as_deref(), Some("C00114"));
        assert_eq!(first.turn_date.as_deref(), Some("2025-12-15"));
        assert_eq!(first.turn_cost_total, Some(100.0));
        assert_eq!(first.vacancy_days, Some(10));
        assert_eq!(first.total_turn_impact, Some(434.7));

        let blank = &rows[2];
        assert!(blank.unit.is_none() && blank.turn_date.is_none());
    }

    #[test]
    fn rounds_float_vacancy_days() {
        // juniper_fund carries vacancy_days as "26.0" — must round to integer days.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn_costs_summary.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "unit,turn_date,turn_cost_total,vacancy_days,total_turn_impact"
        )
        .unwrap();
        writeln!(f, "1612-A,2022-08-26,695.0,26.0,1496.67").unwrap();
        f.flush().unwrap();

        let rows = parse_turn_costs(&path).unwrap();
        assert_eq!(rows[0].vacancy_days, Some(26));
    }

    #[test]
    fn unparseable_cost_is_null_not_zero() {
        // A present-but-non-numeric cost cell ("N/A") must parse to NULL (None),
        // never be coerced to 0.0 — a coerced $0 is a fabricated figure.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn_costs_summary.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "unit,turn_date,turn_cost_total,vacancy_days,total_turn_impact"
        )
        .unwrap();
        writeln!(f, "U1,2026-01-10,N/A,pending,400.0").unwrap();
        f.flush().unwrap();

        let rows = parse_turn_costs(&path).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            r.turn_cost_total, None,
            "unparseable cost must be NULL, not 0.0"
        );
        assert_eq!(
            r.vacancy_days, None,
            "unparseable vacancy_days must be NULL, not 0"
        );
        // The row is still a real turn event (unit + date present).
        assert_eq!(r.unit.as_deref(), Some("U1"));
        assert_eq!(r.turn_date.as_deref(), Some("2026-01-10"));
    }

    #[test]
    fn reads_source_property_id_when_present() {
        // juniper_fund-shaped file: a property_id column drives blend detection.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn_costs_summary.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "property_id,unit,turn_date,turn_cost_total,vacancy_days,total_turn_impact"
        )
        .unwrap();
        writeln!(f, "200101,1612-A,2022-08-26,695.0,26.0,1496.67").unwrap();
        writeln!(f, "200102,2200-B,2022-09-01,300.0,12,800.0").unwrap();
        f.flush().unwrap();

        let rows = parse_turn_costs(&path).unwrap();
        assert_eq!(rows[0].source_property_id.as_deref(), Some("200101"));
        assert_eq!(rows[1].source_property_id.as_deref(), Some("200102"));
    }
}
