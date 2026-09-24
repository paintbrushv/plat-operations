//! Standardized leasing funnel adapter.
//!
//! Aggregates `Standardized/leasing_funnel.csv` into leasing snapshots. The
//! current standardized Yardi funnel uses `shows`; Boxscore maps that to tours
//! until a cleaner tours field is available.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const SOURCE_FILE_NAME: &str = "leasing_funnel.csv";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeasingSnapshotAggregate {
    pub rows_seen: usize,
    pub as_of_date: String,
    pub leads: i64,
    pub tours: i64,
    pub applications: i64,
    pub approvals: i64,
    pub move_ins: i64,
    pub move_outs: i64,
    pub concessions_amount: f64,
    pub missing_fields: Vec<String>,
}

pub fn read_leasing_snapshot(path: &Path) -> Result<LeasingSnapshotAggregate> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open leasing funnel CSV: {}", path.display()))?;
    let headers = reader.headers()?.clone();
    let week_end_idx = header_index(&headers, &["week_end", "event_date", "snapshot_date"]);
    let leads_idx = header_index(&headers, &["leads"]);
    let tours_idx = header_index(&headers, &["tours", "shows"]);
    let applications_idx = header_index(&headers, &["applications"]);
    let approvals_idx = header_index(&headers, &["approvals"]);
    let move_ins_idx = header_index(&headers, &["move_ins", "moveins"]);
    let move_outs_idx = header_index(&headers, &["move_outs", "moveouts"]);
    let concessions_idx = header_index(&headers, &["concessions_amount", "concessions"]);

    let mut missing_fields = Vec::new();
    push_missing(&mut missing_fields, "week_end", week_end_idx.is_none());
    push_missing(&mut missing_fields, "leads", leads_idx.is_none());
    push_missing(&mut missing_fields, "tours_or_shows", tours_idx.is_none());
    push_missing(
        &mut missing_fields,
        "applications",
        applications_idx.is_none(),
    );
    push_missing(&mut missing_fields, "approvals", approvals_idx.is_none());
    push_missing(&mut missing_fields, "move_ins", move_ins_idx.is_none());
    push_missing(&mut missing_fields, "move_outs", move_outs_idx.is_none());

    let mut aggregate = LeasingSnapshotAggregate {
        rows_seen: 0,
        as_of_date: String::new(),
        leads: 0,
        tours: 0,
        applications: 0,
        approvals: 0,
        move_ins: 0,
        move_outs: 0,
        concessions_amount: 0.0,
        missing_fields,
    };

    for record in reader.records() {
        let record = record?;
        aggregate.rows_seen += 1;
        if let Some(idx) = week_end_idx {
            if let Some(date) = cell(&record, idx).filter(|value| !value.is_empty()) {
                if date > aggregate.as_of_date.as_str() {
                    aggregate.as_of_date = date.to_string();
                }
            }
        }
        if let Some(idx) = leads_idx {
            aggregate.leads += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = tours_idx {
            aggregate.tours += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = applications_idx {
            aggregate.applications += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = approvals_idx {
            aggregate.approvals += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = move_ins_idx {
            aggregate.move_ins += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = move_outs_idx {
            aggregate.move_outs += parse_i64(cell(&record, idx).unwrap_or_default());
        }
        if let Some(idx) = concessions_idx {
            aggregate.concessions_amount += parse_amount(cell(&record, idx).unwrap_or_default());
        }
    }

    if aggregate.as_of_date.trim().is_empty() {
        return Err(anyhow!(
            "missing week_end/event_date in leasing funnel source"
        ));
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

fn parse_i64(value: &str) -> i64 {
    parse_amount(value).round() as i64
}

fn parse_amount(value: &str) -> f64 {
    crate::parse::parse_money_lenient(value)
}

fn push_missing(missing_fields: &mut Vec<String>, field: &str, missing: bool) {
    if missing {
        missing_fields.push(field.to_string());
    }
}
