//! RPCOE lane-CSV reader for the Renewals / Lease-Expiration screen.
//!
//! The RPCOE (Renewal Pricing & Concession Optimization Engine) writes a
//! single-file (latest) `rpcoe_recommendations.csv` per lane under `Reports/`.
//! It carries — per occupied unit — the current vs market rent, the recommended
//! new rent / increase %, a confidence label, up to three human-readable
//! drivers, the lease-expiration date and days-to-expiration, and a concession
//! recommendation. The database has **no** expiry dates or recommendations, so
//! these are RPCOE-only.
//!
//! We read defensively (mirrors `bddre.rs`): a missing file skips that lane, and
//! the `driver_1..3` columns are filtered so an engine `[PLACEHOLDER: ...]`
//! string never surfaces as a driver.

use crate::connectors::standardized::source_registry::load_lanes;

/// One renewal-pricing recommendation row, normalized for the Renewals screen.
#[derive(Debug, Clone)]
pub struct RentRec {
    /// Lane display name (e.g. "Maplewood Commons").
    pub lane: String,
    pub unit: String,
    pub resident_code: String,
    pub resident_name: String,
    pub current_rent: f64,
    pub market_rent: f64,
    /// Current rent as a percent of market (e.g. 101.81 = 101.81%).
    pub rent_vs_market_pct: f64,
    /// Days until the lease expires; `None` when the source is blank.
    pub days_to_expiration: Option<i64>,
    pub lease_expiration: String,
    pub recommended_new_rent: f64,
    pub recommended_increase_pct: f64,
    /// Confidence label: "High" / "Medium" / "Low".
    pub confidence: String,
    /// Concession recommendation (one human-readable line).
    pub concession: String,
    /// First usable driver from `driver_1..3` (placeholders filtered out).
    pub top_driver: String,
}

/// Pick the first non-empty, non-placeholder driver. Engine placeholders look
/// like `[PLACEHOLDER: Comparable renewals cleared at X% - requires ...]` —
/// filter any value that mentions `PLACEHOLDER`. Returns "" when none usable.
pub fn first_driver(drivers: &[&str]) -> String {
    for d in drivers {
        let cleaned = d.trim();
        if cleaned.is_empty() {
            continue;
        }
        if cleaned.contains("PLACEHOLDER") {
            continue;
        }
        return cleaned.to_string();
    }
    String::new()
}

/// Sort recommendations most-urgent-first: soonest expiry at the top. Rows with
/// no `days_to_expiration` sort last; ties break on unit label.
fn sort_urgent_first(mut recs: Vec<RentRec>) -> Vec<RentRec> {
    recs.sort_by(|a, b| {
        let ak = a.days_to_expiration.unwrap_or(i64::MAX);
        let bk = b.days_to_expiration.unwrap_or(i64::MAX);
        ak.cmp(&bk).then_with(|| a.unit.cmp(&b.unit))
    });
    recs
}

/// Bucket recommendations into expiration windows by `days_to_expiration`:
/// `[<=30, 31-60, 61-90, 90+]`. Rows with no days value are not counted.
pub fn expiration_windows(recs: &[RentRec]) -> [usize; 4] {
    let mut w = [0usize; 4];
    for r in recs {
        if let Some(d) = r.days_to_expiration {
            if d <= 30 {
                w[0] += 1;
            } else if d <= 60 {
                w[1] += 1;
            } else if d <= 90 {
                w[2] += 1;
            } else {
                w[3] += 1;
            }
        }
    }
    w
}

/// Parse the RPCOE recommendations CSV at `path` for one lane. Missing or
/// unreadable file → empty vec (the caller simply shows fewer rows).
pub fn parse_recommendations(lane: &str, path: &std::path::Path) -> Vec<RentRec> {
    let mut reader = match csv::Reader::from_path(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut recs = Vec::new();
    for record in reader.deserialize::<RecommendationRecord>().flatten() {
        let top_driver = first_driver(&[
            record.driver_1.trim(),
            record.driver_2.trim(),
            record.driver_3.trim(),
        ]);
        recs.push(RentRec {
            lane: lane.to_string(),
            unit: record.unit,
            resident_code: record.resident_code,
            resident_name: record.resident_name,
            current_rent: record.current_rent,
            market_rent: record.market_rent,
            rent_vs_market_pct: record.rent_vs_market_pct,
            // The engine writes day counts as floats ("69.0"); round to whole days.
            days_to_expiration: record.days_to_expiration.map(|d| d.round() as i64),
            lease_expiration: record.lease_expiration,
            recommended_new_rent: record.recommended_new_rent,
            recommended_increase_pct: record.recommended_increase_pct,
            confidence: record.confidence,
            concession: record.concession_recommendation,
            top_driver,
        });
    }
    recs
}

/// Discover RPCOE recommendations across all built-in lanes, most-urgent-first
/// (soonest expiry at the top).
pub fn discover_rpcoe_recommendations() -> Vec<RentRec> {
    let mut all = Vec::new();
    for lane in load_lanes() {
        let path = lane
            .root_path
            .join("Reports")
            .join("rpcoe_recommendations.csv");
        all.extend(parse_recommendations(&lane.display_name, &path));
    }
    sort_urgent_first(all)
}

// ── CSV record shape (serde via the csv crate) ───────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct RecommendationRecord {
    unit: String,
    resident_code: String,
    resident_name: String,
    #[serde(default)]
    current_rent: f64,
    #[serde(default)]
    market_rent: f64,
    #[serde(default)]
    rent_vs_market_pct: f64,
    // The engine writes this as a float ("69.0"); blank cells → None.
    #[serde(default)]
    days_to_expiration: Option<f64>,
    #[serde(default)]
    lease_expiration: String,
    #[serde(default)]
    recommended_increase_pct: f64,
    #[serde(default)]
    recommended_new_rent: f64,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    driver_1: String,
    #[serde(default)]
    driver_2: String,
    #[serde(default)]
    driver_3: String,
    #[serde(default)]
    concession_recommendation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_driver_skips_placeholders() {
        assert_eq!(
            first_driver(&["[PLACEHOLDER: needs data]", "Clean payment history", ""]),
            "Clean payment history"
        );
        assert_eq!(
            first_driver(&["Frequent late payments", "x"]),
            "Frequent late payments"
        );
    }

    #[test]
    fn first_driver_empty_when_all_placeholder_or_blank() {
        assert_eq!(first_driver(&["", "  ", "[PLACEHOLDER: x]"]), "");
        assert_eq!(first_driver(&[]), "");
    }

    #[test]
    fn expiration_windows_buckets_by_days() {
        let r = |d: Option<i64>| RentRec {
            lane: "L".into(),
            unit: "U".into(),
            resident_code: "t".into(),
            resident_name: "R".into(),
            current_rent: 0.0,
            market_rent: 0.0,
            rent_vs_market_pct: 0.0,
            days_to_expiration: d,
            lease_expiration: String::new(),
            recommended_new_rent: 0.0,
            recommended_increase_pct: 0.0,
            confidence: String::new(),
            concession: String::new(),
            top_driver: String::new(),
        };
        let recs = vec![
            r(Some(10)),  // <=30
            r(Some(30)),  // <=30
            r(Some(45)),  // 31-60
            r(Some(90)),  // 61-90
            r(Some(120)), // 90+
            r(None),      // uncounted
        ];
        assert_eq!(expiration_windows(&recs), [2, 1, 1, 1]);
    }
}
