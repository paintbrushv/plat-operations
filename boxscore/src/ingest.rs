use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::path::Path;
use tracing::{info, warn};

use crate::db;

#[derive(Debug, Clone, Copy)]
pub enum IngestKind {
    Property,
    GlActuals,
    GlBudgets,
    RentRoll,
    Delinquency,
    Leasing,
    TurnCosts,
    UnitPnl,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestResult {
    pub kind: String,
    pub file: String,
    pub inserted: usize,
    pub skipped: usize,
    pub gaps_created: usize,
}

impl IngestKind {
    fn label(self) -> &'static str {
        match self {
            Self::Property => "property",
            Self::GlActuals => "gl_actuals",
            Self::GlBudgets => "gl_budgets",
            Self::RentRoll => "rent_roll",
            Self::Delinquency => "delinquency",
            Self::Leasing => "leasing",
            Self::TurnCosts => "turn_costs",
            Self::UnitPnl => "unit_pnl",
        }
    }
}

pub async fn ingest_file(pool: &SqlitePool, kind: IngestKind, file: &Path) -> Result<IngestResult> {
    let task_run_id = db::create_task_run(
        pool,
        "ingest",
        &format!("ingest {} from {}", kind.label(), file.display()),
    )
    .await?;

    let result = match kind {
        IngestKind::Property => ingest_properties(pool, &task_run_id, file).await,
        IngestKind::GlActuals => ingest_gl(pool, &task_run_id, file, "gl_actuals").await,
        IngestKind::GlBudgets => ingest_gl(pool, &task_run_id, file, "gl_budgets").await,
        IngestKind::RentRoll => ingest_rent_roll(pool, &task_run_id, file).await,
        IngestKind::Delinquency => ingest_delinquency(pool, &task_run_id, file).await,
        IngestKind::Leasing => ingest_leasing(pool, &task_run_id, file).await,
        IngestKind::TurnCosts => ingest_turn_costs(pool, file).await,
        IngestKind::UnitPnl => ingest_unit_pnl(pool, file).await,
    };

    match &result {
        Ok(summary) => {
            db::complete_task_run(
                pool,
                &task_run_id,
                "completed",
                Some(if summary.skipped == 0 { 0.95 } else { 0.70 }),
                Some(&format!(
                    "Inserted {}, skipped {}, gaps {}",
                    summary.inserted, summary.skipped, summary.gaps_created
                )),
            )
            .await?;
        }
        Err(err) => {
            db::complete_task_run(
                pool,
                &task_run_id,
                "failed",
                Some(0.0),
                Some(&err.to_string()),
            )
            .await?;
        }
    }
    result
}

#[derive(Debug, Deserialize)]
struct PropertyRow {
    name: String,
    market: String,
    unit_count: String,
    owner_entity: String,
    property_manager: String,
}

#[derive(Debug, Deserialize)]
struct GlRow {
    property: String,
    period: String,
    account_code: String,
    account_name: String,
    category: String,
    amount: String,
}

#[derive(Debug, Deserialize)]
struct RentRollRow {
    property: String,
    as_of_date: String,
    occupied_units: String,
    vacant_units: String,
    leased_units: String,
    notice_units: String,
    down_units: String,
    market_rent_total: String,
    in_place_rent_total: String,
}

#[derive(Debug, Deserialize)]
struct DelinquencyRow {
    property: String,
    as_of_date: String,
    delinquent_amount: String,
    delinquent_units: String,
    prepaid_amount: String,
}

#[derive(Debug, Deserialize)]
struct LeasingRow {
    property: String,
    as_of_date: String,
    leads: String,
    tours: String,
    applications: String,
    approvals: String,
    move_ins: String,
    move_outs: String,
    concessions_amount: String,
}

async fn ingest_properties(
    pool: &SqlitePool,
    task_run_id: &str,
    file: &Path,
) -> Result<IngestResult> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;
    let mut inserted = 0;
    let mut skipped = 0;
    let mut gaps = 0;
    for (index, row) in reader.deserialize::<PropertyRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row) if required(&[&row.name, &row.market, &row.unit_count]) => {
                let unit_count = match row.unit_count.parse::<i64>() {
                    Ok(value) => value,
                    Err(err) => {
                        skipped += 1;
                        gaps += create_ingest_gap(
                            pool,
                            task_run_id,
                            "missing_property_metadata",
                            file,
                            source_row,
                            &format!("Invalid unit_count for property {}: {err}", row.name),
                        )
                        .await?;
                        continue;
                    }
                };
                db::upsert_property(
                    pool,
                    row.name.trim(),
                    row.market.trim(),
                    unit_count,
                    row.owner_entity.trim(),
                    row.property_manager.trim(),
                )
                .await?;
                inserted += 1;
            }
            Ok(row) => {
                skipped += 1;
                gaps += create_ingest_gap(
                    pool,
                    task_run_id,
                    "missing_property_metadata",
                    file,
                    source_row,
                    &format!("Missing required property fields in row for {:?}", row.name),
                )
                .await?;
            }
            Err(err) => {
                skipped += 1;
                warn!(%err, source_row, "failed to parse property row");
            }
        }
    }
    info!(inserted, skipped, gaps, "property ingestion complete");
    Ok(result(IngestKind::Property, file, inserted, skipped, gaps))
}

async fn ingest_gl(
    pool: &SqlitePool,
    task_run_id: &str,
    file: &Path,
    table: &str,
) -> Result<IngestResult> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;
    let mut inserted = 0;
    let mut skipped = 0;
    let mut gaps = 0;
    delete_prior_file_rows(pool, table, file).await?;
    for (index, row) in reader.deserialize::<GlRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row)
                if required(&[
                    &row.property,
                    &row.period,
                    &row.account_code,
                    &row.account_name,
                    &row.category,
                    &row.amount,
                ]) =>
            {
                let Some(property) = db::find_property_by_name(pool, row.property.trim()).await?
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_property_metadata",
                        file,
                        source_row,
                        &format!("No property metadata found for {}", row.property),
                    )
                    .await?;
                    continue;
                };
                let period_id = db::upsert_period(pool, row.period.trim()).await?;
                let amount = match crate::parse::parse_money(&row.amount) {
                    Ok(value) => value,
                    Err(err) => {
                        skipped += 1;
                        gaps += create_ingest_gap(
                            pool,
                            task_run_id,
                            "missing_actuals",
                            file,
                            source_row,
                            &format!("Invalid GL amount for {}: {err}", row.account_code),
                        )
                        .await?;
                        continue;
                    }
                };
                sqlx::query(&format!(
                    "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                ))
                .bind(db::new_id())
                .bind(property.id)
                .bind(period_id)
                .bind(row.account_code.trim())
                .bind(row.account_name.trim())
                .bind(row.category.trim())
                .bind(amount)
                .bind(file.to_string_lossy().to_string())
                .bind(source_row)
                .bind(db::now_iso())
                .execute(pool)
                .await?;
                inserted += 1;
            }
            Ok(row) => {
                skipped += 1;
                gaps += create_ingest_gap(
                    pool,
                    task_run_id,
                    if table == "gl_actuals" {
                        "missing_actuals"
                    } else {
                        "missing_budget_data"
                    },
                    file,
                    source_row,
                    &format!("Missing required GL fields for row {:?}", row.account_code),
                )
                .await?;
            }
            Err(err) => {
                skipped += 1;
                warn!(%err, source_row, table, "failed to parse GL row");
            }
        }
    }
    Ok(result(
        if table == "gl_actuals" {
            IngestKind::GlActuals
        } else {
            IngestKind::GlBudgets
        },
        file,
        inserted,
        skipped,
        gaps,
    ))
}

async fn ingest_rent_roll(
    pool: &SqlitePool,
    task_run_id: &str,
    file: &Path,
) -> Result<IngestResult> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;
    let mut inserted = 0;
    let mut skipped = 0;
    let mut gaps = 0;
    delete_prior_file_rows(pool, "rent_roll_snapshots", file).await?;
    for (index, row) in reader.deserialize::<RentRollRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row) if required(&[&row.property, &row.as_of_date]) => {
                let Some(property) = db::find_property_by_name(pool, row.property.trim()).await?
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_property_metadata",
                        file,
                        source_row,
                        "Rent roll row references an unknown property",
                    )
                    .await?;
                    continue;
                };
                let parsed = (|| -> Result<_> {
                    Ok((
                        parse_i64(&row.occupied_units, "occupied_units")?,
                        parse_i64(&row.vacant_units, "vacant_units")?,
                        parse_i64(&row.leased_units, "leased_units")?,
                        parse_i64(&row.notice_units, "notice_units")?,
                        parse_i64(&row.down_units, "down_units")?,
                        parse_f64(&row.market_rent_total, "market_rent_total")?,
                        parse_f64(&row.in_place_rent_total, "in_place_rent_total")?,
                    ))
                })();
                let Ok((
                    occupied_units,
                    vacant_units,
                    leased_units,
                    notice_units,
                    down_units,
                    market_rent_total,
                    in_place_rent_total,
                )) = parsed
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_rent_roll",
                        file,
                        source_row,
                        "Rent roll row has invalid numeric fields",
                    )
                    .await?;
                    continue;
                };
                sqlx::query(
                    "INSERT INTO rent_roll_snapshots (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, notice_units, down_units, market_rent_total, in_place_rent_total, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(db::new_id())
                .bind(property.id)
                .bind(row.as_of_date.trim())
                .bind(occupied_units)
                .bind(vacant_units)
                .bind(leased_units)
                .bind(notice_units)
                .bind(down_units)
                .bind(market_rent_total)
                .bind(in_place_rent_total)
                .bind(file.to_string_lossy().to_string())
                .bind(source_row)
                .bind(db::now_iso())
                .execute(pool)
                .await?;
                inserted += 1;
            }
            _ => {
                skipped += 1;
                gaps += create_ingest_gap(
                    pool,
                    task_run_id,
                    "missing_rent_roll",
                    file,
                    source_row,
                    "Rent roll row is missing property or as_of_date",
                )
                .await?;
            }
        }
    }
    Ok(result(IngestKind::RentRoll, file, inserted, skipped, gaps))
}

async fn ingest_delinquency(
    pool: &SqlitePool,
    task_run_id: &str,
    file: &Path,
) -> Result<IngestResult> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;
    let mut inserted = 0;
    let mut skipped = 0;
    let mut gaps = 0;
    delete_prior_file_rows(pool, "delinquency_snapshots", file).await?;
    for (index, row) in reader.deserialize::<DelinquencyRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row) if required(&[&row.property, &row.as_of_date]) => {
                let Some(property) = db::find_property_by_name(pool, row.property.trim()).await?
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_property_metadata",
                        file,
                        source_row,
                        "Delinquency row references an unknown property",
                    )
                    .await?;
                    continue;
                };
                let parsed = (|| -> Result<_> {
                    Ok((
                        parse_f64(&row.delinquent_amount, "delinquent_amount")?,
                        parse_i64(&row.delinquent_units, "delinquent_units")?,
                        parse_f64(&row.prepaid_amount, "prepaid_amount")?,
                    ))
                })();
                let Ok((delinquent_amount, delinquent_units, prepaid_amount)) = parsed else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_delinquency_snapshot",
                        file,
                        source_row,
                        "Delinquency row has invalid numeric fields",
                    )
                    .await?;
                    continue;
                };
                sqlx::query(
                    "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(db::new_id())
                .bind(property.id)
                .bind(row.as_of_date.trim())
                .bind(delinquent_amount)
                .bind(delinquent_units)
                .bind(prepaid_amount)
                .bind(file.to_string_lossy().to_string())
                .bind(source_row)
                .bind(db::now_iso())
                .execute(pool)
                .await?;
                inserted += 1;
            }
            _ => {
                skipped += 1;
                gaps += create_ingest_gap(
                    pool,
                    task_run_id,
                    "missing_delinquency_snapshot",
                    file,
                    source_row,
                    "Delinquency row is missing property or as_of_date",
                )
                .await?;
            }
        }
    }
    Ok(result(
        IngestKind::Delinquency,
        file,
        inserted,
        skipped,
        gaps,
    ))
}

async fn ingest_leasing(pool: &SqlitePool, task_run_id: &str, file: &Path) -> Result<IngestResult> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;
    let mut inserted = 0;
    let mut skipped = 0;
    let mut gaps = 0;
    delete_prior_file_rows(pool, "leasing_snapshots", file).await?;
    for (index, row) in reader.deserialize::<LeasingRow>().enumerate() {
        let source_row = index as i64 + 2;
        match row {
            Ok(row) if required(&[&row.property, &row.as_of_date]) => {
                let Some(property) = db::find_property_by_name(pool, row.property.trim()).await?
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_property_metadata",
                        file,
                        source_row,
                        "Leasing row references an unknown property",
                    )
                    .await?;
                    continue;
                };
                let parsed = (|| -> Result<_> {
                    Ok((
                        parse_i64(&row.leads, "leads")?,
                        parse_i64(&row.tours, "tours")?,
                        parse_i64(&row.applications, "applications")?,
                        parse_i64(&row.approvals, "approvals")?,
                        parse_i64(&row.move_ins, "move_ins")?,
                        parse_i64(&row.move_outs, "move_outs")?,
                        parse_f64(&row.concessions_amount, "concessions_amount")?,
                    ))
                })();
                let Ok((
                    leads,
                    tours,
                    applications,
                    approvals,
                    move_ins,
                    move_outs,
                    concessions_amount,
                )) = parsed
                else {
                    skipped += 1;
                    gaps += create_ingest_gap(
                        pool,
                        task_run_id,
                        "missing_leasing_data",
                        file,
                        source_row,
                        "Leasing row has invalid numeric fields",
                    )
                    .await?;
                    continue;
                };
                sqlx::query(
                    "INSERT INTO leasing_snapshots (id, property_id, as_of_date, leads, tours, applications, approvals, move_ins, move_outs, concessions_amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(db::new_id())
                .bind(property.id)
                .bind(row.as_of_date.trim())
                .bind(leads)
                .bind(tours)
                .bind(applications)
                .bind(approvals)
                .bind(move_ins)
                .bind(move_outs)
                .bind(concessions_amount)
                .bind(file.to_string_lossy().to_string())
                .bind(source_row)
                .bind(db::now_iso())
                .execute(pool)
                .await?;
                inserted += 1;
            }
            _ => {
                skipped += 1;
                gaps += create_ingest_gap(
                    pool,
                    task_run_id,
                    "missing_leasing_data",
                    file,
                    source_row,
                    "Leasing row is missing property or as_of_date",
                )
                .await?;
            }
        }
    }
    Ok(result(IngestKind::Leasing, file, inserted, skipped, gaps))
}

/// File-based entry point for turn costs (`boxscore ingest turn-costs --file ...`).
///
/// Unlike the GL/rent-roll demo paths, `turn_costs_summary.csv` carries no
/// `property` column for most lanes, so attribution is resolved by matching the
/// file's parent `Standardized/` directory to a configured property lane, then
/// delegating to the lane connector (the same code the standardized lane command
/// runs). Errors if the file does not live under a known lane.
async fn ingest_turn_costs(pool: &SqlitePool, file: &Path) -> Result<IngestResult> {
    use crate::connectors::standardized::source_registry::load_lanes;
    use crate::connectors::standardized::turn_costs::ingest_turn_costs_for_lane;

    let parent = file.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "turn-costs file has no parent directory: {}",
            file.display()
        )
    })?;
    let lane = load_lanes()
        .into_iter()
        .find(|lane| lane.standardized_path == parent)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no property lane owns {}; use `boxscore ingest-standardized turn-costs --lane <key>`",
                file.display()
            )
        })?;
    let summary = ingest_turn_costs_for_lane(pool, &lane).await?;
    Ok(IngestResult {
        kind: IngestKind::TurnCosts.label().to_string(),
        file: file.to_string_lossy().to_string(),
        inserted: summary.rows_inserted,
        skipped: summary.rows_skipped,
        gaps_created: summary.gaps_created,
    })
}

/// File-based entry point for unit P&L (`boxscore ingest unit-pnl --file ...`).
///
/// Like turn costs, `unit_pnl_annual.csv` carries no `property` column, so
/// attribution is resolved by matching the file's parent `Standardized/`
/// directory to a configured property lane, then delegating to the lane
/// connector (the same code the standardized lane command runs). Errors if the
/// file does not live under a known lane.
async fn ingest_unit_pnl(pool: &SqlitePool, file: &Path) -> Result<IngestResult> {
    use crate::connectors::standardized::source_registry::load_lanes;
    use crate::connectors::standardized::unit_pnl::ingest_unit_pnl_for_lane;

    let parent = file.parent().ok_or_else(|| {
        anyhow::anyhow!("unit-pnl file has no parent directory: {}", file.display())
    })?;
    let lane = load_lanes()
        .into_iter()
        .find(|lane| lane.standardized_path == parent)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no property lane owns {}; use `boxscore ingest-standardized unit-pnl --lane <key>`",
                file.display()
            )
        })?;
    let summary = ingest_unit_pnl_for_lane(pool, &lane).await?;
    Ok(IngestResult {
        kind: IngestKind::UnitPnl.label().to_string(),
        file: file.to_string_lossy().to_string(),
        inserted: summary.rows_inserted,
        skipped: summary.rows_skipped,
        gaps_created: summary.gaps_created,
    })
}

/// Re-ingest idempotency: rows from a given source file are replaced, not
/// duplicated, when the same file is ingested again.
async fn delete_prior_file_rows(pool: &SqlitePool, table: &str, file: &Path) -> Result<()> {
    if !matches!(
        table,
        "gl_actuals"
            | "gl_budgets"
            | "monthly_actuals"
            | "rent_roll_snapshots"
            | "delinquency_snapshots"
            | "leasing_snapshots"
    ) {
        return Err(anyhow::anyhow!("unsupported ingest table: {table}"));
    }
    sqlx::query(&format!("DELETE FROM {table} WHERE source_file = ?"))
        .bind(file.to_string_lossy().to_string())
        .execute(pool)
        .await?;
    Ok(())
}

fn required(fields: &[&str]) -> bool {
    fields.iter().all(|field| !field.trim().is_empty())
}

fn parse_i64(value: &str, field: &str) -> Result<i64> {
    crate::parse::parse_count(value).with_context(|| format!("invalid {field}"))
}

fn parse_f64(value: &str, field: &str) -> Result<f64> {
    crate::parse::parse_money(value).with_context(|| format!("invalid {field}"))
}

async fn create_ingest_gap(
    pool: &SqlitePool,
    task_run_id: &str,
    gap_type: &str,
    file: &Path,
    source_row: i64,
    description: &str,
) -> Result<usize> {
    db::insert_gap(
        pool,
        task_run_id,
        gap_type,
        "medium",
        description,
        "Incomplete source data lowers confidence and can hide a true operating driver.",
        &format!(
            "Correct {} row {} and re-run ingestion.",
            file.display(),
            source_row
        ),
    )
    .await?;
    Ok(1)
}

fn result(
    kind: IngestKind,
    file: &Path,
    inserted: usize,
    skipped: usize,
    gaps_created: usize,
) -> IngestResult {
    IngestResult {
        kind: kind.label().to_string(),
        file: file.to_string_lossy().to_string(),
        inserted,
        skipped,
        gaps_created,
    }
}

// ---------------------------------------------------------------------------
// monthly_actuals — T12 reversion flywheel (T2)
// ---------------------------------------------------------------------------

/// Ingest a `gl_monthly_actuals.csv` (P&L monthly rollup from the GL parquet).
///
/// Expected columns (case-insensitive): `property_id`, `period`, `account_code`,
/// `account_name`, `amount`.  Rows with a blank `period` or `account_code`, or
/// an unparseable `amount`, are silently skipped.  The `property_id` column in
/// the CSV is ignored — the caller passes `property_id` explicitly so the DB
/// foreign-key is always resolved via `require_property_by_name`.
///
/// Returns the count of rows inserted.
pub async fn ingest_monthly_actuals_csv(
    pool: &SqlitePool,
    property_id: &str,
    file: &Path,
) -> Result<usize> {
    let mut reader = crate::parse::csv_reader_from_path(file)?;

    // Resolve header positions case-insensitively.
    let headers: Vec<String> = reader
        .headers()
        .with_context(|| format!("cannot read headers from {}", file.display()))?
        .iter()
        .map(|h| h.to_lowercase())
        .collect();

    let col = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| anyhow::anyhow!("column '{}' not found in {}", name, file.display()))
    };

    let idx_period = col("period")?;
    let idx_account_code = col("account_code")?;
    let idx_account_name = col("account_name").ok();
    let idx_amount = col("amount")?;

    let source = file.to_string_lossy();

    // Idempotency: delete any rows previously ingested from this same file
    // so that re-ingesting the same monthly-close CSV replaces rather than
    // doubles every account total. Mirrors the pattern used by `ingest_gl`.
    delete_prior_file_rows(pool, "monthly_actuals", file).await?;

    let mut count = 0usize;

    for record in reader.records() {
        let record = match record {
            Ok(r) => r,
            Err(err) => {
                warn!(%err, "skipping malformed CSV record in monthly_actuals");
                continue;
            }
        };

        let period = record.get(idx_period).unwrap_or("").trim().to_string();
        let account_code = record
            .get(idx_account_code)
            .unwrap_or("")
            .trim()
            .to_string();

        if period.is_empty() || account_code.is_empty() {
            continue;
        }

        let amount_str = record.get(idx_amount).unwrap_or("").trim();
        let amount = match crate::parse::parse_money(amount_str) {
            Ok(v) => v,
            Err(_) => {
                warn!(
                    period,
                    account_code, amount_str, "skipping monthly_actual row with unparseable amount"
                );
                continue;
            }
        };

        let account_name = idx_account_name
            .and_then(|i| record.get(i))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        db::insert_monthly_actual(
            pool,
            property_id,
            &period,
            &account_code,
            account_name,
            amount,
            &source,
        )
        .await?;

        count += 1;
    }

    info!(
        count,
        property_id,
        ?file,
        "monthly_actuals ingestion complete"
    );
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn ingests_sample_properties() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        let result = ingest_file(
            &pool,
            IngestKind::Property,
            Path::new("data/sample/properties.csv"),
        )
        .await
        .unwrap();

        assert_eq!(result.inserted, 3);
        assert_eq!(result.skipped, 0);
    }
}
