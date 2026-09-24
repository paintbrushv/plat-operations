//! Standardized aged receivables adapter.
//!
//! Aggregates `Standardized/aged_receivables.csv` into delinquency snapshots.
//! Resident codes and names are used only while reading the source row and are
//! not retained in the returned aggregate.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const SOURCE_FILE_NAME: &str = "aged_receivables.csv";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelinquencySnapshotAggregate {
    pub rows_seen: usize,
    pub as_of_date: String,
    pub delinquent_amount: f64,
    pub delinquent_units: i64,
    pub prepaid_amount: f64,
    pub used_bucket_fallback: bool,
    pub missing_fields: Vec<String>,
}

pub fn read_delinquency_snapshot(path: &Path) -> Result<DelinquencySnapshotAggregate> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open aged receivables CSV: {}", path.display()))?;
    let headers = reader.headers()?.clone();
    let snapshot_date_idx = header_index(&headers, &["snapshot_date", "as_of_date"]);
    let total_delinquent_idx = header_index(&headers, &["total_delinquent"]);
    let prepayments_idx = header_index(&headers, &["prepayments", "prepaid_amount"]);
    let bucket_indices = [
        header_index(&headers, &["days_0_30"]),
        header_index(&headers, &["days_31_60"]),
        header_index(&headers, &["days_61_90"]),
        header_index(&headers, &["days_over_90"]),
    ];
    let mut missing_fields = Vec::new();
    push_missing(
        &mut missing_fields,
        "snapshot_date",
        snapshot_date_idx.is_none(),
    );
    push_missing(
        &mut missing_fields,
        "prepayments",
        prepayments_idx.is_none(),
    );
    if total_delinquent_idx.is_none() && bucket_indices.iter().any(Option::is_none) {
        missing_fields.push("total_delinquent_or_aging_buckets".to_string());
    }

    let mut aggregate = DelinquencySnapshotAggregate {
        rows_seen: 0,
        as_of_date: String::new(),
        delinquent_amount: 0.0,
        delinquent_units: 0,
        prepaid_amount: 0.0,
        used_bucket_fallback: total_delinquent_idx.is_none(),
        missing_fields,
    };

    for record in reader.records() {
        let record = record?;
        aggregate.rows_seen += 1;

        if aggregate.as_of_date.is_empty() {
            if let Some(idx) = snapshot_date_idx {
                aggregate.as_of_date = cell(&record, idx).unwrap_or_default().to_string();
            }
        }

        let delinquent_amount = if let Some(idx) = total_delinquent_idx {
            parse_amount(cell(&record, idx).unwrap_or_default())
        } else {
            bucket_indices
                .iter()
                .flatten()
                .map(|idx| parse_amount(cell(&record, *idx).unwrap_or_default()))
                .sum()
        };
        if delinquent_amount > 0.0 {
            aggregate.delinquent_units += 1;
            aggregate.delinquent_amount += delinquent_amount;
        }
        if let Some(idx) = prepayments_idx {
            aggregate.prepaid_amount += parse_amount(cell(&record, idx).unwrap_or_default()).abs();
        }
    }

    if aggregate.as_of_date.trim().is_empty() {
        return Err(anyhow!("missing snapshot_date in aged receivables source"));
    }
    Ok(aggregate)
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

// ── Per-unit rows ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UnitReceivableRow {
    pub resident_code: String,
    pub resident_name: Option<String>,
    pub resident_status: Option<String>,
    pub total_delinquent: f64,
    pub current_owed: Option<f64>,
    pub as_of_date: String,
    pub source_row: i64,
}

/// Parse `aged_receivables.csv` into one [`UnitReceivableRow`] per data line.
///
/// Uses the same CSV reader and header-index logic as [`read_delinquency_snapshot`].
pub fn parse_unit_receivables(csv_path: &Path) -> anyhow::Result<Vec<UnitReceivableRow>> {
    let mut reader = crate::parse::csv_reader_from_path(csv_path).with_context(|| {
        format!(
            "failed to open aged receivables CSV: {}",
            csv_path.display()
        )
    })?;
    let headers = reader.headers()?.clone();

    let snapshot_date_idx = header_index(&headers, &["snapshot_date", "as_of_date"]);
    let total_delinquent_idx = header_index(&headers, &["total_delinquent"]);
    let current_owed_idx = header_index(&headers, &["current_owed"]);
    let resident_code_idx = header_index(&headers, &["resident_code", "unit_code", "unit"]);
    let resident_name_idx = header_index(&headers, &["resident_name", "name"]);
    let resident_status_idx = header_index(&headers, &["resident_status", "status"]);
    let bucket_indices = [
        header_index(&headers, &["days_0_30"]),
        header_index(&headers, &["days_31_60"]),
        header_index(&headers, &["days_61_90"]),
        header_index(&headers, &["days_over_90"]),
    ];

    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record?;
        let source_row = index as i64 + 2; // 1-based + header offset

        let resident_code = resident_code_idx
            .and_then(|idx| cell(&record, idx))
            .unwrap_or_default()
            .to_string();

        let as_of_date = snapshot_date_idx
            .and_then(|idx| cell(&record, idx))
            .unwrap_or_default()
            .to_string();

        let total_delinquent = if let Some(idx) = total_delinquent_idx {
            parse_amount(cell(&record, idx).unwrap_or_default())
        } else {
            bucket_indices
                .iter()
                .flatten()
                .map(|idx| parse_amount(cell(&record, *idx).unwrap_or_default()))
                .sum()
        };

        let current_owed =
            current_owed_idx.map(|idx| parse_amount(cell(&record, idx).unwrap_or_default()));

        let resident_name = resident_name_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let resident_status = resident_status_idx
            .and_then(|idx| cell(&record, idx))
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        rows.push(UnitReceivableRow {
            resident_code,
            resident_name,
            resident_status,
            total_delinquent,
            current_owed,
            as_of_date,
            source_row,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_unit_receivables_fixture() {
        let path = Path::new("tests/fixtures/standardized/willow_brook/aged_receivables.csv");
        let rows = parse_unit_receivables(path).expect("parse should succeed");

        // Fixture has 2 data rows (R-D and R-E).
        assert_eq!(rows.len(), 2, "expected 2 rows from fixture");

        // All rows must have a non-empty resident_code.
        for row in &rows {
            assert!(
                !row.resident_code.is_empty(),
                "resident_code must be non-empty, got empty at source_row {}",
                row.source_row
            );
        }

        // R-E (row 2 in CSV → source_row 3) has total_delinquent = 60.
        let re = rows
            .iter()
            .find(|r| r.resident_code == "R-E")
            .expect("R-E row must be present");
        assert_eq!(
            re.total_delinquent, 60.0,
            "R-E total_delinquent should be 60.0"
        );

        // R-D has total_delinquent = 0.
        let rd = rows
            .iter()
            .find(|r| r.resident_code == "R-D")
            .expect("R-D row must be present");
        assert_eq!(
            rd.total_delinquent, 0.0,
            "R-D total_delinquent should be 0.0"
        );
    }
}
