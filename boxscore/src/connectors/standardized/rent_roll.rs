//! Standardized rent roll adapter.
//!
//! Aggregates `Standardized/rent_roll.csv` into a property-level snapshot while
//! intentionally dropping resident names/codes before persistence.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SOURCE_FILE_NAME: &str = "rent_roll.csv";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RentRollSnapshotAggregate {
    pub rows_seen: usize,
    pub as_of_date: String,
    pub occupied_units: i64,
    pub vacant_units: i64,
    pub leased_units: i64,
    pub notice_units: i64,
    pub down_units: i64,
    pub market_rent_total: f64,
    pub in_place_rent_total: f64,
    pub missing_fields: Vec<String>,
}

pub fn read_rent_roll_snapshot(
    path: &Path,
    fallback_as_of_date: &str,
) -> Result<RentRollSnapshotAggregate> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open rent roll CSV: {}", path.display()))?;
    let headers = reader.headers()?.clone();
    let resident_idx = header_index(&headers, &["resident", "resident_code"]);
    let name_idx = header_index(&headers, &["name", "resident_name"]);
    let market_idx = header_index(&headers, &["market"]);
    let charge_rent_idx = header_index(&headers, &["charge_rent"]);
    let as_of_idx = header_index(&headers, &["snapshot_date", "as_of_date"]);
    let notice_idx = header_index(&headers, &["notice_units", "notice"]);
    let down_idx = header_index(&headers, &["down_units", "down"]);

    let mut aggregate = RentRollSnapshotAggregate {
        rows_seen: 0,
        as_of_date: fallback_as_of_date.to_string(),
        occupied_units: 0,
        vacant_units: 0,
        leased_units: 0,
        notice_units: 0,
        down_units: 0,
        market_rent_total: 0.0,
        in_place_rent_total: 0.0,
        missing_fields: Vec::new(),
    };

    push_missing(
        &mut aggregate.missing_fields,
        "snapshot_date",
        as_of_idx.is_none(),
    );
    push_missing(
        &mut aggregate.missing_fields,
        "resident_or_name",
        resident_idx.is_none() && name_idx.is_none(),
    );
    push_missing(
        &mut aggregate.missing_fields,
        "market",
        market_idx.is_none(),
    );
    push_missing(
        &mut aggregate.missing_fields,
        "charge_rent",
        charge_rent_idx.is_none(),
    );
    push_missing(
        &mut aggregate.missing_fields,
        "notice_units",
        notice_idx.is_none(),
    );
    push_missing(
        &mut aggregate.missing_fields,
        "down_units",
        down_idx.is_none(),
    );

    let mut sentinel_down_units = 0_i64;
    for record in reader.records() {
        let record = record?;
        aggregate.rows_seen += 1;
        if let Some(idx) = as_of_idx {
            if let Some(value) = cell(&record, idx).filter(|value| !value.is_empty()) {
                aggregate.as_of_date = value.to_string();
            }
        }

        let resident = resident_idx
            .and_then(|idx| cell(&record, idx))
            .unwrap_or_default();
        let name = name_idx
            .and_then(|idx| cell(&record, idx))
            .unwrap_or_default();
        let is_down = is_down_marker(resident) || is_down_marker(name);
        let is_vacant = resident.is_empty() && name.is_empty()
            || is_vacancy_marker(resident)
            || is_vacancy_marker(name)
            || is_down;

        if is_down {
            sentinel_down_units += 1;
        }
        if is_vacant {
            aggregate.vacant_units += 1;
        } else {
            aggregate.occupied_units += 1;
            aggregate.leased_units += 1;
        }

        if let Some(idx) = market_idx {
            aggregate.market_rent_total += parse_amount(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = charge_rent_idx {
            aggregate.in_place_rent_total += parse_amount(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = notice_idx {
            aggregate.notice_units +=
                parse_amount(cell(&record, idx).unwrap_or_default()).round() as i64;
        }
        if let Some(idx) = down_idx {
            aggregate.down_units +=
                parse_amount(cell(&record, idx).unwrap_or_default()).round() as i64;
        }
    }
    // Use exactly one source of truth for down units: the dedicated column
    // when present, otherwise the "down" sentinel in the resident/name field.
    // Counting both would double the figure when a file carries both signals.
    if down_idx.is_none() {
        aggregate.down_units = sentinel_down_units;
    }

    Ok(aggregate)
}

// ── Per-unit lease rows ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UnitLeaseRow {
    pub unit_label: String,
    pub resident_code: Option<String>,
    pub resident_name: Option<String>,
    pub market_rent: Option<f64>,
    pub charge_rent: Option<f64>,
    pub as_of_date: String,
    pub source_row: i64,
}

/// Parse per-unit lease rows (retains unit/resident identifiers, unlike the aggregate path).
pub fn parse_unit_leases(csv_path: &std::path::Path) -> anyhow::Result<Vec<UnitLeaseRow>> {
    let mut reader = crate::parse::csv_reader_from_path(csv_path)
        .with_context(|| format!("failed to open rent roll CSV: {}", csv_path.display()))?;
    let headers = reader.headers()?.clone();
    let unit_idx = header_index(&headers, &["unit", "unit_label"]);
    let resident_idx = header_index(&headers, &["resident", "resident_code"]);
    let name_idx = header_index(&headers, &["name", "resident_name"]);
    let market_idx = header_index(&headers, &["market"]);
    let charge_rent_idx = header_index(&headers, &["charge_rent"]);
    let as_of_idx = header_index(&headers, &["snapshot_date", "as_of_date"]);

    let mut rows = Vec::new();
    let mut fallback_as_of = String::new();
    for (source_row_usize, record) in (1_i64..).zip(reader.records()) {
        let record = record?;
        let source_row = source_row_usize;

        // Update fallback as_of_date from the first non-empty value seen.
        if let Some(idx) = as_of_idx {
            if let Some(value) = cell(&record, idx).filter(|v| !v.is_empty()) {
                fallback_as_of = value.to_string();
            }
        }

        let unit_label = unit_idx
            .and_then(|idx| cell(&record, idx))
            .unwrap_or_default()
            .to_string();
        if unit_label.is_empty() {
            continue;
        }

        let resident_code = resident_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let resident_name = name_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|v| !v.is_empty())
            .map(str::to_string);

        let market_rent = market_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|v| !v.is_empty())
            .map(parse_amount);
        let charge_rent = charge_rent_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|v| !v.is_empty())
            .map(parse_amount);

        let as_of_date = as_of_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback_as_of.clone());

        rows.push(UnitLeaseRow {
            unit_label,
            resident_code,
            resident_name,
            market_rent,
            charge_rent,
            as_of_date,
            source_row,
        });
    }
    Ok(rows)
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

fn is_vacancy_marker(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "vacant" | "vacancy" | "available" | "model" | "admin" | "down"
    )
}

fn is_down_marker(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("down")
}
