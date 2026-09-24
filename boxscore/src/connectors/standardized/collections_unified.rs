//! Standardized collections context adapter.
//!
//! Aggregates `Standardized/collections_unified.csv` into property-level
//! collection snapshots. Resident codes/names are intentionally not retained.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::{
    connectors::standardized::{source_registry::PropertyLane, StandardizedIngestSummary},
    db, tools,
};

pub const SOURCE_FILE_NAME: &str = "collections_unified.csv";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectionsSnapshotAggregate {
    pub rows_seen: usize,
    pub as_of_date: String,
    pub total_delinquent: f64,
    pub delinquent_units: i64,
    pub high_risk_units: i64,
    pub total_opportunity: f64,
    pub pricing_opportunity: f64,
    pub missed_fee_total: f64,
    pub avg_on_time_pct: f64,
    pub missing_fields: Vec<String>,
}

pub async fn ingest_collections_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
) -> Result<StandardizedIngestSummary> {
    let fallback_date = infer_latest_raw_date(&lane.raw_data_path, "AgedReceivables")
        .unwrap_or_else(|| chrono::Utc::now().date_naive().to_string());
    ingest_collections_file(
        pool,
        lane,
        &lane.standardized_path.join(SOURCE_FILE_NAME),
        &fallback_date,
    )
    .await
}

pub async fn ingest_collections_file(
    pool: &SqlitePool,
    lane: &PropertyLane,
    path: &Path,
    fallback_as_of_date: &str,
) -> Result<StandardizedIngestSummary> {
    let snapshot = read_collections_snapshot(path, fallback_as_of_date)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let task_run_id = db::create_task_run(
        pool,
        "standardized_collections_ingest",
        &format!(
            "Ingest standardized collections context for {}",
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
    let source_file = path.to_string_lossy().to_string();
    let snapshot_id =
        insert_collection_snapshot(pool, &property_id, &snapshot, &source_file).await?;
    let claim = format!(
        "Collections context showed ${:.0} delinquent, {} delinquent units/residents, {} high-risk units, and ${:.0} total opportunity as of {}.",
        snapshot.total_delinquent,
        snapshot.delinquent_units,
        snapshot.high_risk_units,
        snapshot.total_opportunity,
        snapshot.as_of_date
    );
    db::insert_evidence(
        pool,
        db::NewEvidence {
            task_run_id: &task_run_id,
            source_type: "standardized_csv",
            source_table: "collection_snapshots",
            source_id: Some(&snapshot_id),
            source_file: Some(&source_file),
            source_row: Some(1),
            claim: &claim,
        },
    )
    .await?;
    tools::log_tool_run(
        pool,
        &task_run_id,
        "LoadCollectionsTool",
        json!({"source_file": source_file}),
        Some(json!({"rows": snapshot.rows_seen, "as_of_date": snapshot.as_of_date})),
        None,
    )
    .await?;

    let mut gaps_created = 0;
    for field in &snapshot.missing_fields {
        let (gap_type, severity, description, why_it_matters, resolution) =
            missing_field_gap(&lane.display_name, field);
        db::insert_gap(
            pool,
            &task_run_id,
            gap_type,
            severity,
            &description,
            why_it_matters,
            resolution,
        )
        .await?;
        gaps_created += 1;
    }

    db::complete_task_run(
        pool,
        &task_run_id,
        "completed",
        Some(if gaps_created == 0 { 0.90 } else { 0.75 }),
        Some(&format!(
            "Inserted collections snapshot for {} from {} source rows with {gaps_created} gaps.",
            lane.display_name, snapshot.rows_seen
        )),
    )
    .await?;

    Ok(StandardizedIngestSummary {
        lane: lane.property_key.clone(),
        source_file,
        rows_seen: snapshot.rows_seen,
        rows_inserted: 1,
        rows_skipped: 0,
        gaps_created,
    })
}

pub fn read_collections_snapshot(
    path: &Path,
    fallback_as_of_date: &str,
) -> Result<CollectionsSnapshotAggregate> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open collections CSV: {}", path.display()))?;
    let headers = reader.headers()?.clone();
    let snapshot_date_idx = header_index(&headers, &["snapshot_date", "as_of_date"]);
    let total_delinquent_idx = header_index(&headers, &["total_delinquent"]);
    let total_opportunity_idx = header_index(&headers, &["total_opportunity"]);
    let pricing_opportunity_idx = header_index(&headers, &["pricing_opportunity"]);
    let missed_fee_idx = header_index(&headers, &["missed_fee_total"]);
    let on_time_idx = header_index(&headers, &["on_time_pct"]);
    let delinquency_tier_idx = header_index(&headers, &["delinquency_tier"]);
    let pre_delinquency_tier_idx = header_index(&headers, &["pre_delinquency_tier"]);
    let intervention_priority_idx = header_index(&headers, &["intervention_priority"]);

    let mut missing_fields = Vec::new();
    push_missing(
        &mut missing_fields,
        "snapshot_date",
        snapshot_date_idx.is_none(),
    );
    push_missing(
        &mut missing_fields,
        "total_delinquent",
        total_delinquent_idx.is_none(),
    );
    push_missing(
        &mut missing_fields,
        "total_opportunity",
        total_opportunity_idx.is_none(),
    );
    push_missing(
        &mut missing_fields,
        "pricing_opportunity",
        pricing_opportunity_idx.is_none(),
    );
    push_missing(
        &mut missing_fields,
        "missed_fee_total",
        missed_fee_idx.is_none(),
    );
    push_missing(&mut missing_fields, "on_time_pct", on_time_idx.is_none());

    let mut snapshot = CollectionsSnapshotAggregate {
        rows_seen: 0,
        as_of_date: fallback_as_of_date.to_string(),
        total_delinquent: 0.0,
        delinquent_units: 0,
        high_risk_units: 0,
        total_opportunity: 0.0,
        pricing_opportunity: 0.0,
        missed_fee_total: 0.0,
        avg_on_time_pct: 0.0,
        missing_fields,
    };
    let mut on_time_count = 0_i64;
    let mut on_time_total = 0.0;

    for record in reader.records() {
        let record = record?;
        snapshot.rows_seen += 1;
        if let Some(idx) = snapshot_date_idx {
            if let Some(value) = cell(&record, idx).filter(|value| !value.is_empty()) {
                snapshot.as_of_date = value.to_string();
            }
        }
        let delinquent = total_delinquent_idx
            .map(|idx| parse_amount(cell(&record, idx).unwrap_or_default()))
            .unwrap_or_default();
        snapshot.total_delinquent += delinquent;
        if delinquent > 0.0 {
            snapshot.delinquent_units += 1;
        }
        snapshot.total_opportunity += total_opportunity_idx
            .map(|idx| parse_amount(cell(&record, idx).unwrap_or_default()))
            .unwrap_or_default();
        snapshot.pricing_opportunity += pricing_opportunity_idx
            .map(|idx| parse_amount(cell(&record, idx).unwrap_or_default()))
            .unwrap_or_default();
        snapshot.missed_fee_total += missed_fee_idx
            .map(|idx| parse_amount(cell(&record, idx).unwrap_or_default()))
            .unwrap_or_default();
        if is_high_risk(
            &record,
            delinquency_tier_idx,
            pre_delinquency_tier_idx,
            intervention_priority_idx,
        ) {
            snapshot.high_risk_units += 1;
        }
        if let Some(idx) = on_time_idx {
            let on_time = parse_amount(cell(&record, idx).unwrap_or_default());
            if on_time > 0.0 {
                on_time_total += on_time;
                on_time_count += 1;
            }
        }
    }
    if on_time_count > 0 {
        snapshot.avg_on_time_pct = on_time_total / on_time_count as f64;
    }
    Ok(snapshot)
}

async fn insert_collection_snapshot(
    pool: &SqlitePool,
    property_id: &str,
    snapshot: &CollectionsSnapshotAggregate,
    source_file: &str,
) -> Result<String> {
    crate::connectors::standardized::operating_snapshots::delete_prior_snapshot(
        pool,
        "collection_snapshots",
        property_id,
        source_file,
        &snapshot.as_of_date,
    )
    .await?;
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO collection_snapshots (id, property_id, as_of_date, total_delinquent, delinquent_units, high_risk_units, total_opportunity, pricing_opportunity, missed_fee_total, avg_on_time_pct, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(&snapshot.as_of_date)
    .bind(snapshot.total_delinquent)
    .bind(snapshot.delinquent_units)
    .bind(snapshot.high_risk_units)
    .bind(snapshot.total_opportunity)
    .bind(snapshot.pricing_opportunity)
    .bind(snapshot.missed_fee_total)
    .bind(snapshot.avg_on_time_pct)
    .bind(source_file)
    .bind(1_i64)
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

fn is_high_risk(
    record: &csv::StringRecord,
    delinquency_tier_idx: Option<usize>,
    pre_delinquency_tier_idx: Option<usize>,
    intervention_priority_idx: Option<usize>,
) -> bool {
    let delinquency_tier = delinquency_tier_idx
        .and_then(|idx| cell(record, idx))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let pre_tier = pre_delinquency_tier_idx
        .and_then(|idx| cell(record, idx))
        .unwrap_or_default()
        .to_ascii_lowercase();
    // An empty/missing priority cell must NOT default to 0 (which would
    // falsely flag the unit as highest priority); treat it as no signal.
    let priority = intervention_priority_idx
        .and_then(|idx| cell(record, idx))
        .filter(|value| !value.is_empty())
        .and_then(|value| crate::parse::parse_money(value).ok())
        .unwrap_or(99.0);
    delinquency_tier.contains("high")
        || pre_tier.contains("watch")
        || pre_tier.contains("high")
        || priority <= 1.0
}

fn missing_field_gap(
    property_name: &str,
    field: &str,
) -> (
    &'static str,
    &'static str,
    String,
    &'static str,
    &'static str,
) {
    match field {
        "snapshot_date" => (
            "missing_collections_snapshot_date",
            "medium",
            format!("Collections context for {property_name} does not carry an authoritative snapshot date."),
            "Collections and bad debt timing are period-sensitive; a fallback date limits confidence.",
            "Add snapshot_date/as_of_date to collections_unified.csv or its source profile.",
        ),
        _ => (
            "missing_collections_field",
            "low",
            format!("Collections context for {property_name} is missing `{field}`."),
            "Missing collections fields limit the collections and bad debt bridge.",
            "Extend the standardized collections ETL with the missing field when available.",
        ),
    }
}

fn infer_latest_raw_date(raw_data_path: &Path, filename_contains: &str) -> Option<String> {
    std::fs::read_dir(raw_data_path)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if name.contains(filename_contains) {
                extract_latest_date_from_name(name)
            } else {
                None
            }
        })
        .max()
        .map(|date| date.to_string())
}

fn extract_latest_date_from_name(filename: &str) -> Option<chrono::NaiveDate> {
    let numbers = filename
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    numbers
        .windows(3)
        .filter_map(|window| {
            let month = window[0].parse::<u32>().ok()?;
            let day = window[1].parse::<u32>().ok()?;
            let year = window[2].parse::<i32>().ok()?;
            if window[2].len() == 4 {
                chrono::NaiveDate::from_ymd_opt(year, month, day)
            } else {
                None
            }
        })
        .max()
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

fn parse_amount(value: &str) -> f64 {
    crate::parse::parse_money_lenient(value)
}

fn push_missing(missing_fields: &mut Vec<String>, field: &str, missing: bool) {
    if missing {
        missing_fields.push(field.to_string());
    }
}
