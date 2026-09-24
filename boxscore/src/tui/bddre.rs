//! BDDRE lane-CSV reader for the Delinquency screen's risk watchlist.
//!
//! The BDDRE engine writes two single-file (latest) CSVs per lane under
//! `Reports/`: `bddre_intervention_queue.csv` (residents who need action now,
//! with a recommended intervention) and `bddre_pre_delinquency_watchlist.csv`
//! (residents trending toward delinquency). Both carry a predictive risk score
//! and tier. We read them defensively — a missing file skips that lane, and the
//! Python-list-literal `recommended_interventions` field is parsed without ever
//! panicking, filtering out `[PLACEHOLDER: ...]` values.

use crate::connectors::standardized::source_registry::load_lanes;

/// One risk-watchlist row, normalized across the two BDDRE CSVs.
#[derive(Debug, Clone)]
pub struct RiskRow {
    /// Lane display name (e.g. "Maplewood Commons").
    pub lane: String,
    pub unit: String,
    pub resident_code: String,
    pub resident_name: String,
    /// Predictive risk score (0-100; higher = worse).
    pub risk_score: f64,
    /// Predictive risk tier: "High" / "Medium" / "Low".
    pub risk_tier: String,
    /// Total delinquent balance, when the source carries it (queue only).
    pub total_delinquent: f64,
    /// First recommended intervention (queue only; "" for the watchlist).
    pub top_action: String,
}

/// Parse a Python-list-literal string like
/// `"['Structured payment plan option', 'Manager-level engagement']"` into its
/// first usable element, filtering out `[PLACEHOLDER: ...]` entries. Returns an
/// empty string when nothing usable is present. Never panics.
pub fn first_intervention(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip the surrounding brackets if present.
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    for piece in inner.split(',') {
        let cleaned = piece.trim().trim_matches(['\'', '"']).trim();
        if cleaned.is_empty() {
            continue;
        }
        // Drop engine placeholders like "[PLACEHOLDER: needs review]".
        if cleaned.starts_with("[PLACEHOLDER") || cleaned.contains("PLACEHOLDER") {
            continue;
        }
        return cleaned.to_string();
    }
    String::new()
}

/// Sort rows worst-first (highest predictive risk score at the top) and return.
fn sort_worst_first(mut rows: Vec<RiskRow>) -> Vec<RiskRow> {
    rows.sort_by(|a, b| {
        b.risk_score
            .partial_cmp(&a.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.unit.cmp(&b.unit))
    });
    rows
}

/// Parse the intervention-queue CSV at `path` for one lane. Missing/unreadable
/// file → empty vec (the caller simply shows fewer rows).
pub fn parse_intervention_queue(lane: &str, path: &std::path::Path) -> Vec<RiskRow> {
    let mut reader = match csv::Reader::from_path(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut rows = Vec::new();
    for record in reader.deserialize::<InterventionQueueRecord>().flatten() {
        rows.push(RiskRow {
            lane: lane.to_string(),
            unit: record.unit,
            resident_code: record.resident_code,
            resident_name: record.resident_name,
            risk_score: record.predictive_risk_score,
            risk_tier: record.predictive_risk_tier,
            total_delinquent: record.total_delinquent,
            top_action: first_intervention(&record.recommended_interventions),
        });
    }
    rows
}

/// Parse the pre-delinquency watchlist CSV at `path` for one lane.
pub fn parse_pre_delinquency_watchlist(lane: &str, path: &std::path::Path) -> Vec<RiskRow> {
    let mut reader = match csv::Reader::from_path(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut rows = Vec::new();
    for record in reader.deserialize::<WatchlistRecord>().flatten() {
        rows.push(RiskRow {
            lane: lane.to_string(),
            unit: record.unit,
            resident_code: record.resident_code,
            resident_name: record.resident_name,
            risk_score: record.predictive_risk_score,
            risk_tier: record.predictive_risk_tier,
            total_delinquent: 0.0,
            top_action: String::new(),
        });
    }
    rows
}

/// Discover the intervention queue across all built-in lanes, worst-first.
pub fn discover_intervention_queue() -> Vec<RiskRow> {
    let mut all = Vec::new();
    for lane in load_lanes() {
        let path = lane
            .root_path
            .join("Reports")
            .join("bddre_intervention_queue.csv");
        all.extend(parse_intervention_queue(&lane.display_name, &path));
    }
    sort_worst_first(all)
}

/// Discover the pre-delinquency watchlist across all built-in lanes, worst-first.
pub fn discover_pre_delinquency_watchlist() -> Vec<RiskRow> {
    let mut all = Vec::new();
    for lane in load_lanes() {
        let path = lane
            .root_path
            .join("Reports")
            .join("bddre_pre_delinquency_watchlist.csv");
        all.extend(parse_pre_delinquency_watchlist(&lane.display_name, &path));
    }
    sort_worst_first(all)
}

// ── CSV record shapes (serde via the csv crate) ──────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct InterventionQueueRecord {
    unit: String,
    resident_code: String,
    resident_name: String,
    #[serde(default)]
    total_delinquent: f64,
    #[serde(default)]
    predictive_risk_score: f64,
    #[serde(default)]
    predictive_risk_tier: String,
    #[serde(default)]
    recommended_interventions: String,
}

#[derive(Debug, serde::Deserialize)]
struct WatchlistRecord {
    unit: String,
    resident_code: String,
    resident_name: String,
    #[serde(default)]
    predictive_risk_score: f64,
    #[serde(default)]
    predictive_risk_tier: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_intervention_parses_python_list() {
        let raw = "['Structured payment plan option', 'Manager-level engagement', 'Heightened monitoring cadence']";
        assert_eq!(first_intervention(raw), "Structured payment plan option");
    }

    #[test]
    fn first_intervention_filters_placeholders() {
        let raw = "['[PLACEHOLDER: pending review]', 'Send cure-or-quit notice']";
        assert_eq!(first_intervention(raw), "Send cure-or-quit notice");
    }

    #[test]
    fn first_intervention_handles_empty_and_garbage() {
        assert_eq!(first_intervention(""), "");
        assert_eq!(first_intervention("[]"), "");
        assert_eq!(first_intervention("[PLACEHOLDER: x]"), "");
        // No brackets at all — still safe, returns the lone value.
        assert_eq!(first_intervention("'Call resident'"), "Call resident");
    }
}
