use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    connectors::standardized::{
        aged_receivables::{
            parse_unit_receivables, read_delinquency_snapshot,
            SOURCE_FILE_NAME as AGED_RECEIVABLES_FILE,
        },
        leasing_funnel::{read_leasing_snapshot, SOURCE_FILE_NAME as LEASING_FUNNEL_FILE},
        rent_roll::{
            parse_unit_leases, read_rent_roll_snapshot, SOURCE_FILE_NAME as RENT_ROLL_FILE,
        },
        source_registry::PropertyLane,
        StandardizedIngestSummary,
    },
    db, tools,
};

pub async fn ingest_operating_snapshots_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
) -> Result<StandardizedIngestSummary> {
    let fallback_rent_roll_date = infer_latest_raw_date(&lane.raw_data_path, "RentRoll")
        .unwrap_or_else(|| chrono::Utc::now().date_naive().to_string());
    ingest_operating_snapshots_files(
        pool,
        lane,
        &lane.standardized_path.join(RENT_ROLL_FILE),
        &lane.standardized_path.join(AGED_RECEIVABLES_FILE),
        &lane.standardized_path.join(LEASING_FUNNEL_FILE),
        &fallback_rent_roll_date,
    )
    .await
}

pub async fn ingest_operating_snapshots_files(
    pool: &SqlitePool,
    lane: &PropertyLane,
    rent_roll_path: &Path,
    aged_receivables_path: &Path,
    leasing_funnel_path: &Path,
    fallback_rent_roll_date: &str,
) -> Result<StandardizedIngestSummary> {
    let task_run_id = db::create_task_run(
        pool,
        "standardized_ops_ingest",
        &format!(
            "Ingest standardized operating snapshots for {}",
            lane.display_name
        ),
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

    let rent_roll = read_rent_roll_snapshot(rent_roll_path, fallback_rent_roll_date)
        .with_context(|| format!("failed to parse {}", rent_roll_path.display()))?;
    tools::log_tool_run(
        pool,
        &task_run_id,
        "LoadRentRollTool",
        json!({"source_file": rent_roll_path.to_string_lossy()}),
        Some(json!({"rows": rent_roll.rows_seen, "as_of_date": rent_roll.as_of_date})),
        None,
    )
    .await?;

    let rent_roll_source_file = rent_roll_path.to_string_lossy().to_string();
    let aged_receivables_source_file = aged_receivables_path.to_string_lossy().to_string();
    let leasing_funnel_source_file = leasing_funnel_path.to_string_lossy().to_string();

    // Per-unit leases: parse the same rent_roll CSV and insert one row per unit.
    // Errors are non-fatal — the aggregate snapshot above is the authoritative signal.
    let rent_roll_period = &rent_roll.as_of_date[..7.min(rent_roll.as_of_date.len())];
    let already_have_leases = db::leases_snapshot_exists(pool, &property_id, rent_roll_period)
        .await
        .unwrap_or(false);
    if !already_have_leases {
        match parse_unit_leases(rent_roll_path) {
            Ok(unit_lease_rows) => {
                for row in &unit_lease_rows {
                    if let Err(e) = db::insert_unit_lease(
                        pool,
                        &property_id,
                        &row.as_of_date,
                        &row.unit_label,
                        row.resident_code.as_deref(),
                        row.resident_name.as_deref(),
                        row.market_rent,
                        row.charge_rent,
                        &rent_roll_source_file,
                        row.source_row,
                    )
                    .await
                    {
                        tracing::warn!(
                            error = %e,
                            unit_label = %row.unit_label,
                            source_row = row.source_row,
                            "unit_leases: skip row {}: {}",
                            row.source_row,
                            e
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    path = %rent_roll_path.display(),
                    "parse_unit_leases failed — skipping per-unit insert; aggregate snapshot still recorded"
                );
            }
        }
    }

    let delinquency = read_delinquency_snapshot(aged_receivables_path)
        .with_context(|| format!("failed to parse {}", aged_receivables_path.display()))?;
    tools::log_tool_run(
        pool,
        &task_run_id,
        "LoadDelinquencyTool",
        json!({"source_file": aged_receivables_path.to_string_lossy()}),
        Some(json!({"rows": delinquency.rows_seen, "as_of_date": delinquency.as_of_date})),
        None,
    )
    .await?;

    let leasing = read_leasing_snapshot(leasing_funnel_path)
        .with_context(|| format!("failed to parse {}", leasing_funnel_path.display()))?;
    tools::log_tool_run(
        pool,
        &task_run_id,
        "LoadLeasingTool",
        json!({"source_file": leasing_funnel_path.to_string_lossy()}),
        Some(json!({"rows": leasing.rows_seen, "as_of_date": leasing.as_of_date})),
        None,
    )
    .await?;

    let rent_roll_id =
        insert_rent_roll_snapshot(pool, &property_id, &rent_roll, &rent_roll_source_file).await?;
    let rent_roll_claim = format!(
        "Rent roll aggregate has {} occupied units, {} vacant units, and ${:.0} market rent.",
        rent_roll.occupied_units, rent_roll.vacant_units, rent_roll.market_rent_total
    );
    db::insert_evidence(
        pool,
        db::NewEvidence {
            task_run_id: &task_run_id,
            source_type: "standardized_csv",
            source_table: "rent_roll_snapshots",
            source_id: Some(&rent_roll_id),
            source_file: Some(&rent_roll_source_file),
            source_row: Some(1),
            claim: &rent_roll_claim,
        },
    )
    .await?;

    let delinquency_id = insert_delinquency_snapshot(
        pool,
        &property_id,
        &delinquency,
        &aged_receivables_source_file,
    )
    .await?;
    let delinquency_claim = format!(
        "Delinquency aggregate has ${:.0} delinquent across {} units/residents.",
        delinquency.delinquent_amount, delinquency.delinquent_units
    );
    db::insert_evidence(
        pool,
        db::NewEvidence {
            task_run_id: &task_run_id,
            source_type: "standardized_csv",
            source_table: "delinquency_snapshots",
            source_id: Some(&delinquency_id),
            source_file: Some(&aged_receivables_source_file),
            source_row: Some(1),
            claim: &delinquency_claim,
        },
    )
    .await?;

    // Per-unit receivables: parse the same CSV and insert one row per resident.
    // Errors are non-fatal — the aggregate snapshot above is the authoritative signal.
    let delinquency_period = &delinquency.as_of_date[..7.min(delinquency.as_of_date.len())];
    let already_have_receivables =
        db::receivables_snapshot_exists(pool, &property_id, delinquency_period)
            .await
            .unwrap_or(false);
    if !already_have_receivables {
        match parse_unit_receivables(aged_receivables_path) {
            Ok(unit_rows) => {
                for row in &unit_rows {
                    if row.resident_code.is_empty() {
                        continue;
                    }
                    if let Err(e) = db::insert_unit_receivable(
                        pool,
                        &property_id,
                        &row.as_of_date,
                        &row.resident_code,
                        row.resident_name.as_deref(),
                        row.resident_status.as_deref(),
                        row.total_delinquent,
                        row.current_owed,
                        None, // days_late: aged_receivables.csv has no per-resident age column
                        &aged_receivables_source_file,
                        row.source_row,
                    )
                    .await
                    {
                        tracing::warn!(
                            error = %e,
                            resident_code = %row.resident_code,
                            path = %aged_receivables_path.display(),
                            "insert_unit_receivable failed — skipping this row; aggregate snapshot still recorded"
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    path = %aged_receivables_path.display(),
                    "parse_unit_receivables failed — skipping per-unit insert; aggregate snapshot still recorded"
                );
            }
        }
    }

    let leasing_id =
        insert_leasing_snapshot(pool, &property_id, &leasing, &leasing_funnel_source_file).await?;
    let leasing_claim = format!(
        "Leasing funnel aggregate has {} tours, {} applications, and {} approvals.",
        leasing.tours, leasing.applications, leasing.approvals
    );
    db::insert_evidence(
        pool,
        db::NewEvidence {
            task_run_id: &task_run_id,
            source_type: "standardized_csv",
            source_table: "leasing_snapshots",
            source_id: Some(&leasing_id),
            source_file: Some(&leasing_funnel_source_file),
            source_row: Some(1),
            claim: &leasing_claim,
        },
    )
    .await?;

    let mut gaps_created = 0;
    gaps_created += persist_missing_field_gaps(
        pool,
        &task_run_id,
        &lane.display_name,
        RENT_ROLL_FILE,
        &rent_roll.missing_fields,
    )
    .await?;
    gaps_created += persist_missing_field_gaps(
        pool,
        &task_run_id,
        &lane.display_name,
        AGED_RECEIVABLES_FILE,
        &delinquency.missing_fields,
    )
    .await?;
    gaps_created += persist_missing_field_gaps(
        pool,
        &task_run_id,
        &lane.display_name,
        LEASING_FUNNEL_FILE,
        &leasing.missing_fields,
    )
    .await?;

    let rows_seen = rent_roll.rows_seen + delinquency.rows_seen + leasing.rows_seen;
    db::complete_task_run(
        pool,
        &task_run_id,
        "completed",
        Some(if gaps_created == 0 { 0.90 } else { 0.72 }),
        Some(&format!(
            "Inserted operating snapshots for {} from {rows_seen} source rows with {gaps_created} gaps.",
            lane.display_name
        )),
    )
    .await?;

    Ok(StandardizedIngestSummary {
        lane: lane.property_key.clone(),
        source_file: "operating_snapshots".to_string(),
        rows_seen,
        rows_inserted: 3,
        rows_skipped: 0,
        gaps_created,
    })
}

async fn insert_rent_roll_snapshot(
    pool: &SqlitePool,
    property_id: &str,
    snapshot: &crate::connectors::standardized::rent_roll::RentRollSnapshotAggregate,
    source_file: &str,
) -> Result<String> {
    delete_prior_snapshot(
        pool,
        "rent_roll_snapshots",
        property_id,
        source_file,
        &snapshot.as_of_date,
    )
    .await?;
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO rent_roll_snapshots (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, notice_units, down_units, market_rent_total, in_place_rent_total, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(&snapshot.as_of_date)
    .bind(snapshot.occupied_units)
    .bind(snapshot.vacant_units)
    .bind(snapshot.leased_units)
    .bind(snapshot.notice_units)
    .bind(snapshot.down_units)
    .bind(snapshot.market_rent_total)
    .bind(snapshot.in_place_rent_total)
    .bind(source_file)
    .bind(1_i64)
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_delinquency_snapshot(
    pool: &SqlitePool,
    property_id: &str,
    snapshot: &crate::connectors::standardized::aged_receivables::DelinquencySnapshotAggregate,
    source_file: &str,
) -> Result<String> {
    delete_prior_snapshot(
        pool,
        "delinquency_snapshots",
        property_id,
        source_file,
        &snapshot.as_of_date,
    )
    .await?;
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(&snapshot.as_of_date)
    .bind(snapshot.delinquent_amount)
    .bind(snapshot.delinquent_units)
    .bind(snapshot.prepaid_amount)
    .bind(source_file)
    .bind(1_i64)
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_leasing_snapshot(
    pool: &SqlitePool,
    property_id: &str,
    snapshot: &crate::connectors::standardized::leasing_funnel::LeasingSnapshotAggregate,
    source_file: &str,
) -> Result<String> {
    delete_prior_snapshot(
        pool,
        "leasing_snapshots",
        property_id,
        source_file,
        &snapshot.as_of_date,
    )
    .await?;
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO leasing_snapshots (id, property_id, as_of_date, leads, tours, applications, approvals, move_ins, move_outs, concessions_amount, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(&snapshot.as_of_date)
    .bind(snapshot.leads)
    .bind(snapshot.tours)
    .bind(snapshot.applications)
    .bind(snapshot.approvals)
    .bind(snapshot.move_ins)
    .bind(snapshot.move_outs)
    .bind(snapshot.concessions_amount)
    .bind(source_file)
    .bind(1_i64)
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

/// Re-ingest idempotency: a snapshot from the same source file with the same
/// as-of date replaces the prior row instead of duplicating it.
pub(crate) async fn delete_prior_snapshot(
    pool: &SqlitePool,
    table: &str,
    property_id: &str,
    source_file: &str,
    as_of_date: &str,
) -> Result<()> {
    if !matches!(
        table,
        "rent_roll_snapshots"
            | "delinquency_snapshots"
            | "leasing_snapshots"
            | "collection_snapshots"
    ) {
        return Err(anyhow::anyhow!("unsupported snapshot table: {table}"));
    }
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE property_id = ? AND source_file = ? AND as_of_date = ?"
    ))
    .bind(property_id)
    .bind(source_file)
    .bind(as_of_date)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_missing_field_gaps(
    pool: &SqlitePool,
    task_run_id: &str,
    property_name: &str,
    source_file: &str,
    missing_fields: &[String],
) -> Result<usize> {
    let mut count = 0;
    for field in missing_fields {
        let (gap_type, severity, description, why_it_matters, proposed_resolution, question) =
            missing_field_gap(property_name, source_file, field);
        db::insert_gap(
            pool,
            task_run_id,
            &gap_type,
            severity,
            &description,
            why_it_matters,
            proposed_resolution,
        )
        .await?;
        db::insert_question(
            pool,
            task_run_id,
            &question,
            &format!("{source_file} did not include `{field}`, so Boxscore used a conservative placeholder or fallback."),
            4,
        )
        .await?;
        count += 1;
    }
    Ok(count)
}

fn missing_field_gap(
    property_name: &str,
    source_file: &str,
    field: &str,
) -> (
    String,
    &'static str,
    String,
    &'static str,
    &'static str,
    String,
) {
    match field {
        "snapshot_date" => (
            "missing_snapshot_date".to_string(),
            "medium",
            format!("{source_file} does not carry an authoritative snapshot date for {property_name}."),
            "Operating metrics are time-sensitive; the wrong as-of date can misstate occupancy or collections context.",
            "Add an as_of_date/snapshot_date column to the standardized export or source profile.",
            format!("What is the authoritative as-of date for {property_name} {source_file}?"),
        ),
        "leads" | "move_ins" | "move_outs" => (
            "missing_leasing_field".to_string(),
            "medium",
            format!("{source_file} is missing `{field}` for {property_name}."),
            "Leasing movement explains occupancy and revenue changes, but missing funnel fields limit confidence.",
            "Extend the standardized leasing ETL to include this field or document that zero is semantically correct.",
            format!("Does {property_name} have reliable `{field}` data for the leasing period?"),
        ),
        _ => (
            "missing_operating_field".to_string(),
            "low",
            format!("{source_file} is missing `{field}` for {property_name}."),
            "Missing operating fields reduce the auditability of NOI variance explanations.",
            "Add the field to the standardized CSV when available, or document why it is not applicable.",
            format!("Should `{field}` be added to the standardized {source_file} export for {property_name}?"),
        ),
    }
}

fn infer_latest_raw_date(raw_data_path: &Path, filename_contains: &str) -> Option<String> {
    let entries = std::fs::read_dir(raw_data_path).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.contains(filename_contains))
                .unwrap_or(false)
        })
        .filter_map(|path| extract_latest_date_from_path(&path))
        .max()
        .map(|date| date.to_string())
}

fn extract_latest_date_from_path(path: &Path) -> Option<NaiveDate> {
    let filename = path.file_name()?.to_str()?;
    let numbers = filename
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    let full_dates = numbers.windows(3).filter_map(|window| {
        let month = window[0].parse::<u32>().ok()?;
        let day = window[1].parse::<u32>().ok()?;
        let year = window[2].parse::<i32>().ok()?;
        if window[2].len() == 4 {
            NaiveDate::from_ymd_opt(year, month, day)
        } else {
            None
        }
    });
    full_dates.max()
}
