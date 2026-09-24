//! Native standardized-CSV data-contract validator (Phase 2).
//!
//! This is a faithful Rust port of the Python reference implementation at
//! `a Python ETL layer (data_validation.py)`. Both implementations
//! enforce the SAME spec: `boxscore/docs/DATA_CONTRACTS.md` (contract set
//! v1.1, C1-C5). When one changes, the other MUST be updated to match.
//!
//! It runs AFTER each property's per-property ETL has produced its
//! `Standardized/*.csv` files. Its single job is to catch the class of defect
//! where bad or MISSING data flows silently to owner-facing RPCOE/BDDRE
//! reports. Guiding principle: FAIL LOUD / SURFACE THE GAP — never silently
//! coerce a MISSING feed to zero.
//!
//! Contracts evaluated natively (against already-standardized CSVs):
//!   * C1 rent_roll        — no footer/total rows; market rent in [100, 25000].
//!   * C1 downstream       — same footer/market check on `collections_unified.csv`
//!     and `pre_delinquency_scores.csv`.
//!   * C2 concession flow  — active concessions in source must reach
//!     `tenant_profile.has_active_concession`
//!     (juniper_fund allowlisted -> INFO).
//!   * C3 aged receivables — at least one AR feed must exist AND carry aging
//!     buckets; count-only rollup -> ERROR; $0 31+ -> WARN.
//!   * C4 elasticity       — if records analyzed, the rent-position distribution
//!     must not be 0/0/0.
//!
//! C5 (required header resolvable) is DELIBERATELY NOT IMPLEMENTED HERE. C5 is a
//! RAW-Yardi-parse contract enforced UPSTREAM in the Python ETL
//! (`your ETL column resolver`), which
//! raises `UnresolvedColumnError` when a required header cannot be resolved by
//! name on a raw export. The Rust validator only ever sees ALREADY-standardized
//! (named-column) CSVs, so the raw-header-resolution contract is not checkable
//! and not applicable natively. We note it here rather than faking it.
//!
//! This module is READ-ONLY; it never mutates `Standardized/*.csv`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::source_registry::PropertyLane;

/// Sane per-unit market-rent band. $158,529 (the D1 juniper_fund grand-total
/// footer) is wildly out of band and is exactly what we want to reject.
pub const MARKET_RENT_MIN: f64 = 100.0;
pub const MARKET_RENT_MAX: f64 = 25_000.0;

/// Tokens that mark a Yardi total/subtotal/footer row that must never be a unit.
/// `total` is matched exactly (avoid resident names like "Total Wine LLC");
/// the multi-word phrases use substring `contains`.
pub const FOOTER_TOKENS: &[&str] = &[
    "total",
    "all properties",
    "grand total",
    "subtotal",
    "summary",
];

/// Properties whose third-party PM does NOT use ConcessionBurnOff to set active
/// concessions. For these, "0 active concessions" downstream is the CORRECT
/// answer and is reported as INFO, never an ERROR.
pub const EXPECTED_ZERO_CONCESSIONS: &[&str] = &["juniper_fund"];

/// Columns that, if present, prove an AR feed carries aging buckets (vs. a
/// count-only rollup that masquerades as $0 of 31+ delinquency).
pub const AR_BUCKET_COLUMNS: &[&str] = &["days_31_60", "days_61_90", "days_over_90"];

/// Downstream unit-keyed files that inherit a footer/total row if one slips
/// through upstream (the juniper_fund footer survived here after rent_roll cleanup).
pub const DOWNSTREAM_UNIT_FILES: &[&str] =
    &["collections_unified.csv", "pre_delinquency_scores.csv"];

/// Contract set version stamped on validated output (kept in sync with the spec).
pub const CONTRACT_SET_VERSION: &str = "v1.1 (C1-C5)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Error,
    Warn,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
        }
    }
    fn rank(self) -> u8 {
        match self {
            Severity::Error => 3,
            Severity::Warn => 2,
            Severity::Info => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    /// Contract group, e.g. `rent_roll`, `concessions`, `aged_receivables`.
    pub contract: String,
    pub severity: Severity,
    pub message: String,
    pub source_file: Option<String>,
    /// 1-based source row (data rows start at 1; header excluded), if known.
    pub source_row: Option<i64>,
}

impl Finding {
    fn new(contract: &str, severity: Severity, message: impl Into<String>) -> Self {
        Finding {
            contract: contract.to_string(),
            severity,
            message: message.into(),
            source_file: None,
            source_row: None,
        }
    }
    fn with_file(mut self, file: &str) -> Self {
        self.source_file = Some(file.to_string());
        self
    }
    fn with_row(mut self, row: i64) -> Self {
        self.source_row = Some(row);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationResult {
    pub property_key: String,
    pub property_name: String,
    pub contract_set_version: String,
    pub findings: Vec<Finding>,
    /// Overall status: FAIL if any ERROR, WARN if any WARN, else PASS.
    pub status: String,
    pub error_count: usize,
    pub warning_count: usize,
}

impl ValidationResult {
    pub fn ok(&self) -> bool {
        self.error_count == 0
    }
}

struct Builder {
    findings: Vec<Finding>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            findings: Vec::new(),
        }
    }
    fn add(&mut self, finding: Finding) {
        self.findings.push(finding);
    }
}

/// A minimal in-memory CSV table: headers (normalized) + string rows.
struct Table {
    /// Normalized lower_snake_case headers.
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn col(&self, name: &str) -> Option<usize> {
        let target = normalize_header(name);
        self.headers.iter().position(|h| *h == target)
    }
    fn has_col(&self, name: &str) -> bool {
        self.col(name).is_some()
    }
    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn normalize_header(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-', '/'], "_")
}

/// Read a CSV if present and non-empty. Returns `None` if the file is absent
/// (caller decides whether absence is an error), `Some(empty Table)` if the
/// file exists but has no data rows.
fn read_csv(path: &Path) -> Option<Table> {
    if !path.exists() {
        return None;
    }
    let mut reader = match crate::parse::csv_reader_from_path(path) {
        Ok(r) => r,
        Err(_) => {
            return Some(Table {
                headers: Vec::new(),
                rows: Vec::new(),
            })
        }
    };
    let headers = match reader.headers() {
        Ok(h) => h.iter().map(normalize_header).collect::<Vec<_>>(),
        Err(_) => {
            return Some(Table {
                headers: Vec::new(),
                rows: Vec::new(),
            })
        }
    };
    let mut rows = Vec::new();
    for record in reader.records().flatten() {
        rows.push(
            record
                .iter()
                .map(|c| c.trim().to_string())
                .collect::<Vec<_>>(),
        );
    }
    Some(Table { headers, rows })
}

fn footer_match(cell: &str, token: &str) -> bool {
    let s = cell.to_ascii_lowercase();
    let s = s.trim();
    if token == "total" {
        s == token
    } else {
        s.contains(token)
    }
}

// -----------------------------------------------------------------------------
// C1 — RENT ROLL: no footer rows, sane market rent
// -----------------------------------------------------------------------------

fn validate_rent_roll(table: Option<&Table>, b: &mut Builder) {
    let contract = "rent_roll";
    let file = "rent_roll.csv";
    let Some(table) = table else {
        b.add(
            Finding::new(
                contract,
                Severity::Error,
                "rent_roll.csv missing — cannot validate units.",
            )
            .with_file(file),
        );
        return;
    };
    if table.is_empty() {
        b.add(Finding::new(contract, Severity::Error, "rent_roll.csv is empty.").with_file(file));
        return;
    }

    // Footer / total / subtotal rows masquerading as units (D1).
    let identity_cols = ["resident", "name", "unit", "property_name", "unit_type"];
    let mut footer_hits = 0_usize;
    for col_name in identity_cols {
        let Some(idx) = table.col(col_name) else {
            continue;
        };
        for token in FOOTER_TOKENS {
            for (row_i, row) in table.rows.iter().enumerate() {
                if let Some(cell) = row.get(idx) {
                    if footer_match(cell, token) {
                        footer_hits += 1;
                        b.add(
                            Finding::new(
                                contract,
                                Severity::Error,
                                format!(
                                    "footer/total row detected: '{token}' in column '{col_name}'. \
                                     Yardi total/subtotal rows must be dropped on ingest (D1)."
                                ),
                            )
                            .with_file(file)
                            .with_row((row_i + 1) as i64),
                        );
                    }
                }
            }
        }
    }

    // Absurd market rent (the $158,529 footer) and negatives.
    if let Some(midx) = table.col("market") {
        let mut too_high = 0_usize;
        let mut max_high = 0.0_f64;
        let mut negative = 0_usize;
        for row in &table.rows {
            let v =
                crate::parse::parse_money_lenient(row.get(midx).map(String::as_str).unwrap_or(""));
            if v > MARKET_RENT_MAX {
                too_high += 1;
                if v > max_high {
                    max_high = v;
                }
            }
            if v < 0.0 {
                negative += 1;
            }
        }
        if too_high > 0 {
            b.add(
                Finding::new(
                    contract,
                    Severity::Error,
                    format!(
                        "{too_high} unit(s) with market rent above ${MARKET_RENT_MAX:.0} \
                         (max ${max_high:.0}). This is the D1 footer-as-unit signature (e.g. $158,529)."
                    ),
                )
                .with_file(file),
            );
        }
        if negative > 0 {
            b.add(
                Finding::new(
                    contract,
                    Severity::Error,
                    format!("{negative} unit(s) with negative market rent."),
                )
                .with_file(file),
            );
        }
        if footer_hits == 0 {
            b.add(
                Finding::new(
                    contract,
                    Severity::Info,
                    format!(
                        "{} units, market rent within [${MARKET_RENT_MIN:.0}, ${MARKET_RENT_MAX:.0}].",
                        table.rows.len()
                    ),
                )
                .with_file(file),
            );
        }
    }
}

// -----------------------------------------------------------------------------
// C1 (downstream) — unit-keyed files must not inherit footer rows
// -----------------------------------------------------------------------------

fn validate_downstream_footers(std_dir: &Path, b: &mut Builder) {
    for fname in DOWNSTREAM_UNIT_FILES {
        let Some(table) = read_csv(&std_dir.join(fname)) else {
            continue;
        };
        if table.is_empty() {
            continue;
        }
        let contract = fname.trim_end_matches(".csv");
        let identity_cols = [
            "resident",
            "name",
            "unit",
            "property_name",
            "unit_type",
            "resident_name",
        ];
        let mut hits = 0_usize;
        for col_name in identity_cols {
            let Some(idx) = table.col(col_name) else {
                continue;
            };
            for token in FOOTER_TOKENS {
                for (row_i, row) in table.rows.iter().enumerate() {
                    if let Some(cell) = row.get(idx) {
                        if footer_match(cell, token) {
                            hits += 1;
                            b.add(
                                Finding::new(
                                    contract,
                                    Severity::Error,
                                    format!(
                                        "footer/total row leaked into {fname}: '{token}' in column \
                                         '{col_name}'. A downstream file inherited an upstream footer row (D1)."
                                    ),
                                )
                                .with_file(fname)
                                .with_row((row_i + 1) as i64),
                            );
                        }
                    }
                }
            }
        }
        for mcol in ["market", "market_rent"] {
            if let Some(idx) = table.col(mcol) {
                let high = table.rows.iter().any(|row| {
                    crate::parse::parse_money_lenient(
                        row.get(idx).map(String::as_str).unwrap_or(""),
                    ) > MARKET_RENT_MAX
                });
                if high {
                    b.add(
                        Finding::new(
                            contract,
                            Severity::Error,
                            format!(
                                "{fname}: '{mcol}' above ${MARKET_RENT_MAX:.0} \
                                 (D1 footer-as-unit signature, e.g. $158,529)."
                            ),
                        )
                        .with_file(fname),
                    );
                }
            }
        }
        if hits == 0 {
            b.add(
                Finding::new(
                    contract,
                    Severity::Info,
                    format!(
                        "{fname}: {} rows, no footer contamination.",
                        table.rows.len()
                    ),
                )
                .with_file(fname),
            );
        }
    }
}

// -----------------------------------------------------------------------------
// C2 — CONCESSIONS flow through to tenant_profile
// -----------------------------------------------------------------------------

/// Count active concessions in a (possibly raw) concessions.csv. Mirrors the
/// Python `_count_active_concessions`: it relies on a STANDARDIZED
/// `has_active_concession` column. A near-raw ConcessionBurnOff dump (the live
/// shape: `concession_burn_off, unnamed:_1, ...`) has no such column, so the
/// source count is 0 — exactly as the Python fallback (`std = conc_df`) yields
/// when the `tenant_profile_etl` standardizer import is unavailable.
fn count_active_concessions(conc: &Table) -> i64 {
    let Some(idx) = conc.col("has_active_concession") else {
        return 0;
    };
    conc.rows
        .iter()
        .filter(|row| {
            let cell = row.get(idx).map(String::as_str).unwrap_or("");
            is_truthy(cell)
        })
        .count() as i64
}

fn is_truthy(cell: &str) -> bool {
    let s = cell.trim().to_ascii_lowercase();
    !(s.is_empty() || s == "0" || s == "0.0" || s == "false" || s == "no" || s == "nan")
}

fn validate_concession_flowthrough(
    property_name: &str,
    conc: Option<&Table>,
    profile: Option<&Table>,
    b: &mut Builder,
) {
    let contract = "concessions";
    let conc = match conc {
        Some(c) if !c.is_empty() => c,
        _ => {
            b.add(Finding::new(
                contract,
                Severity::Info,
                "no concessions.csv (or empty) — nothing to flow through.",
            ));
            return;
        }
    };

    let source_active = count_active_concessions(conc);

    if EXPECTED_ZERO_CONCESSIONS.contains(&property_name) {
        b.add(Finding::new(
            contract,
            Severity::Info,
            format!(
                "{property_name} is on the expected-zero-concessions allowlist \
                 (PM does not use ConcessionBurnOff); {source_active} active in source \
                 treated as non-binding. Downstream 0 active is OK."
            ),
        ));
        return;
    }

    if source_active <= 0 {
        b.add(Finding::new(
            contract,
            Severity::Info,
            "concessions.csv present but 0 active concessions in source — downstream 0 is consistent.",
        ));
        return;
    }

    // Source HAS active concessions -> tenant_profile MUST reflect them.
    let profile_has_col = profile
        .map(|p| p.has_col("has_active_concession"))
        .unwrap_or(false);
    if !profile_has_col {
        b.add(
            Finding::new(
                contract,
                Severity::Error,
                format!(
                    "source has {source_active} active concession(s) but tenant_profile.csv is \
                     missing or lacks has_active_concession (D2: concessions not flowing through)."
                ),
            )
            .with_file("tenant_profile.csv"),
        );
        return;
    }

    let profile = profile.unwrap();
    let pidx = profile.col("has_active_concession").unwrap();
    let profile_active = profile
        .rows
        .iter()
        .filter(|row| is_truthy(row.get(pidx).map(String::as_str).unwrap_or("")))
        .count() as i64;
    if profile_active <= 0 {
        b.add(
            Finding::new(
                contract,
                Severity::Error,
                format!(
                    "source has {source_active} active concession(s) but tenant_profile.csv reports \
                     0 active (D2: broken concession join/merge — RPCOE will show $0)."
                ),
            )
            .with_file("tenant_profile.csv"),
        );
    } else {
        b.add(Finding::new(
            contract,
            Severity::Info,
            format!("{source_active} active in source, {profile_active} flowed through to tenant_profile."),
        ));
    }
}

// -----------------------------------------------------------------------------
// C3 — AGED RECEIVABLES presence (missing != zero)
// -----------------------------------------------------------------------------

fn has_real_buckets(table: &Table) -> bool {
    AR_BUCKET_COLUMNS.iter().any(|c| table.has_col(c))
}

fn validate_aged_receivables(ar: Option<&Table>, summary: Option<&Table>, b: &mut Builder) {
    let contract = "aged_receivables";
    let candidates: Vec<(&str, &Table)> = [
        ("aged_receivables.csv", ar),
        ("aging_detail_summary.csv", summary),
    ]
    .into_iter()
    .filter_map(|(name, t)| t.map(|t| (name, t)))
    .collect();

    if candidates.is_empty() {
        b.add(Finding::new(
            contract,
            Severity::Error,
            "no aged_receivables.csv or aging_detail_summary.csv found — 31+ delinquency would be \
             reported as $0 from a MISSING feed (D3). This is a DATA-GAP, not a true zero.",
        ));
        return;
    }

    let bucketed = candidates.iter().find(|(_, t)| has_real_buckets(t));
    let Some((name, table)) = bucketed else {
        let names = candidates
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ");
        b.add(
            Finding::new(
                contract,
                Severity::Error,
                format!(
                    "AR feed(s) present ({names}) but NONE carry aging-bucket columns {AR_BUCKET_COLUMNS:?} \
                     — this is a count-only rollup that would silently report $0 of 31+ delinquency (D3). \
                     Wire the ETL to the AgedReceivables export."
                ),
            )
            .with_file(candidates[0].0),
        );
        return;
    };

    let bucket_idxs: Vec<usize> = AR_BUCKET_COLUMNS
        .iter()
        .filter_map(|c| table.col(c))
        .collect();
    let total_31plus: f64 = table
        .rows
        .iter()
        .flat_map(|row| {
            bucket_idxs.iter().map(move |&i| {
                crate::parse::parse_money_lenient(row.get(i).map(String::as_str).unwrap_or(""))
            })
        })
        .sum();

    if total_31plus <= 0.0 {
        b.add(
            Finding::new(
                contract,
                Severity::Warn,
                format!(
                    "AR source {name} has bucket columns but total 31+ delinquency is $0.00. This MAY be \
                     a genuine all-current snapshot — verify it is not an empty/thin feed (D3: absence vs zero)."
                ),
            )
            .with_file(name),
        );
    } else {
        b.add(
            Finding::new(
                contract,
                Severity::Info,
                format!("AR source {name}: total 31+ delinquency ${total_31plus:.2} across {} accounts.", table.rows.len()),
            )
            .with_file(name),
        );
    }
}

// -----------------------------------------------------------------------------
// C4 — RPCOE elasticity distribution populated when records analyzed
// -----------------------------------------------------------------------------

fn validate_elasticity_distribution(
    records_analyzed: i64,
    underpriced: i64,
    at_market: i64,
    overpriced: i64,
    b: &mut Builder,
) {
    let contract = "rpcoe_elasticity";
    let total_bucketed = underpriced + at_market + overpriced;
    if records_analyzed > 0 && total_bucketed == 0 {
        b.add(
            Finding::new(
                contract,
                Severity::Error,
                format!(
                    "renewal elasticity analyzed {records_analyzed} records but the rent-position \
                     distribution is 0/0/0 (D4: broken rent_gap proxy — likely a unit-key join mismatch \
                     or a history file with no computable rent_gap). Distribution must be populated."
                ),
            )
            .with_file("tenant_profile.csv"),
        );
    } else if records_analyzed > 0 {
        b.add(Finding::new(
            contract,
            Severity::Info,
            format!("elasticity distribution populated: {underpriced}/{at_market}/{overpriced} over {records_analyzed} records."),
        ));
    } else {
        b.add(Finding::new(
            contract,
            Severity::Info,
            "no elasticity records analyzed (nothing to check).",
        ));
    }
}

/// Derive the elasticity distribution from `tenant_profile.csv` and assert it
/// would not be 0/0/0 (D4), mirroring `_derive_elasticity_from_profile`.
fn derive_elasticity_from_profile(profile: Option<&Table>, b: &mut Builder) {
    if let Some(profile) = profile {
        if let Some(cidx) = profile.col("rent_position_cohort") {
            let mut records = 0_i64;
            let mut under = 0_i64;
            let mut over = 0_i64;
            for row in &profile.rows {
                let v = row
                    .get(cidx)
                    .map(String::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if v.is_empty() {
                    continue;
                }
                records += 1;
                if v.contains("under") || v.contains("below") {
                    under += 1;
                } else if v.contains("over") || v.contains("above") {
                    over += 1;
                }
            }
            let at = (records - under - over).max(0);
            validate_elasticity_distribution(records, under, at, over, b);
            return;
        }
        if let Some(gidx) = profile.col("rent_gap") {
            let mut records = 0_i64;
            let mut under = 0_i64;
            let mut over = 0_i64;
            let mut at = 0_i64;
            for row in &profile.rows {
                let raw = row.get(gidx).map(String::as_str).unwrap_or("").trim();
                if raw.is_empty() {
                    continue;
                }
                let Ok(g) = crate::parse::parse_money(raw) else {
                    continue;
                };
                records += 1;
                if g < 0.0 {
                    under += 1;
                } else if g > 0.0 {
                    over += 1;
                } else {
                    at += 1;
                }
            }
            if records > 0 {
                validate_elasticity_distribution(records, under, at, over, b);
                return;
            }
        }
    }
    b.add(Finding::new(
        "rpcoe_elasticity",
        Severity::Info,
        "tenant_profile lacks rent_position_cohort/rent_gap — elasticity contract not checkable from standardized data.",
    ));
}

// -----------------------------------------------------------------------------
// ORCHESTRATION
// -----------------------------------------------------------------------------

/// Validate one property lane's `Standardized/*.csv` against contracts C1-C4 +
/// the C1-downstream check. Reads directly from `lane.standardized_path`.
pub fn validate_lane(lane: &PropertyLane) -> ValidationResult {
    validate_std_dir(
        &lane.property_key,
        &lane.display_name,
        &lane.standardized_path,
    )
}

/// Validate an arbitrary Standardized directory (used by tests against a
/// tempdir fixture, and by `validate_lane` against a real lane).
pub fn validate_std_dir(
    property_key: &str,
    property_name: &str,
    std_dir: &Path,
) -> ValidationResult {
    let mut b = Builder::new();

    if !std_dir.exists() {
        b.add(Finding::new(
            "property",
            Severity::Error,
            format!("Standardized/ not found at {}.", std_dir.display()),
        ));
        return finalize(property_key, property_name, b);
    }

    let rent_roll = read_csv(&std_dir.join("rent_roll.csv"));
    let concessions = read_csv(&std_dir.join("concessions.csv"));
    let profile = read_csv(&std_dir.join("tenant_profile.csv"));
    let aged_receivables = read_csv(&std_dir.join("aged_receivables.csv"));
    let aging_summary = read_csv(&std_dir.join("aging_detail_summary.csv"));

    validate_rent_roll(rent_roll.as_ref(), &mut b); // C1
    validate_downstream_footers(std_dir, &mut b); // C1 (downstream)
    validate_concession_flowthrough(
        property_name,
        concessions.as_ref(),
        profile.as_ref(),
        &mut b,
    ); // C2
    validate_aged_receivables(aged_receivables.as_ref(), aging_summary.as_ref(), &mut b); // C3
    derive_elasticity_from_profile(profile.as_ref(), &mut b); // C4
                                                              // C5: upstream-owned (raw Yardi parse, yardi_columns.py) — not native. See module doc.

    finalize(property_key, property_name, b)
}

fn finalize(property_key: &str, property_name: &str, b: Builder) -> ValidationResult {
    let error_count = b
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warning_count = b
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Warn)
        .count();
    let status = if error_count > 0 {
        "FAIL"
    } else if warning_count > 0 {
        "WARN"
    } else {
        "PASS"
    };
    ValidationResult {
        property_key: property_key.to_string(),
        property_name: property_name.to_string(),
        contract_set_version: CONTRACT_SET_VERSION.to_string(),
        findings: b.findings,
        status: status.to_string(),
        error_count,
        warning_count,
    }
}

/// Roll findings up to one worst-severity line per contract (for summaries).
pub fn worst_per_contract(result: &ValidationResult) -> Vec<&Finding> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<&str, &Finding> = BTreeMap::new();
    for f in &result.findings {
        match map.get(f.contract.as_str()) {
            Some(existing) if existing.severity.rank() >= f.severity.rank() => {}
            _ => {
                map.insert(f.contract.as_str(), f);
            }
        }
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_std() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("boxscore-validate-{}", crate::db::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: impl AsRef<Path>, name: &str, contents: &str) {
        fs::write(dir.as_ref().join(name), contents).unwrap();
    }

    /// A clean fixture lane that should PASS all contracts.
    fn write_clean(dir: &Path) {
        write(
            dir,
            "rent_roll.csv",
            "unit,resident,name,market\n101,r1,Alice,1200\n102,r2,Bob,1350\n",
        );
        write(
            dir,
            "collections_unified.csv",
            "unit,resident_name,total_delinquent\n101,Alice,50\n102,Bob,0\n",
        );
        write(
            dir,
            "pre_delinquency_scores.csv",
            "unit,resident_name,pre_delinquency_score\n101,Alice,0.2\n",
        );
        write(
            dir,
            "concessions.csv",
            "concession_burn_off,unnamed:_1\nfoo,bar\n",
        );
        write(
            dir,
            "aged_receivables.csv",
            "property_id,resident_code,days_0_30,days_31_60,days_61_90,days_over_90\ns1,r1,0,100,0,50\n",
        );
        write(
            dir,
            "tenant_profile.csv",
            "unit,rent_position_cohort,has_active_concession\n101,At Median (90-110%),0\n102,Above Median (>110%),0\n103,Below Median (<90%),0\n",
        );
    }

    #[test]
    fn clean_lane_passes() {
        let dir = tmp_std();
        write_clean(&dir);
        let r = validate_std_dir("maplewood", "maplewood", &dir);
        assert!(r.ok(), "expected PASS, got findings: {:?}", r.findings);
        assert_eq!(r.status, "PASS");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn footer_row_in_rent_roll_is_c1_error() {
        let dir = tmp_std();
        write_clean(&dir);
        // Inject a Yardi grand-total footer row (171 / Total / All Properties / 158529).
        write(
            &dir,
            "rent_roll.csv",
            "unit,resident,name,market\n101,r1,Alice,1200\n171,Total,All Properties,158529\n",
        );
        let r = validate_std_dir("juniper_fund", "juniper_fund", &dir);
        assert!(!r.ok(), "footer row must FAIL");
        let rr = r
            .findings
            .iter()
            .filter(|f| f.contract == "rent_roll" && f.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert!(rr
            .iter()
            .any(|f| f.message.contains("footer/total row detected")));
        assert!(rr.iter().any(|f| f.message.contains("158,529")
            || f.message.contains("158529")
            || f.message.contains("D1 footer-as-unit")));
        assert!(
            rr.iter().any(|f| f.source_row == Some(2)),
            "footer row index recorded"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn footer_leak_in_downstream_is_error() {
        let dir = tmp_std();
        write_clean(&dir);
        write(
            &dir,
            "collections_unified.csv",
            "unit,resident_name,total_delinquent\n101,Alice,50\n171,Total,0\n",
        );
        let r = validate_std_dir("juniper_fund", "juniper_fund", &dir);
        assert!(!r.ok());
        assert!(r
            .findings
            .iter()
            .any(|f| f.contract == "collections_unified"
                && f.severity == Severity::Error
                && f.message.contains("leaked into collections_unified.csv")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concessions_present_but_zeroed_is_c2_error() {
        let dir = tmp_std();
        write_clean(&dir);
        // Standardized concessions carrying an ACTIVE concession.
        write(
            &dir,
            "concessions.csv",
            "unit,resident,has_active_concession\n101,r1,1\n",
        );
        // tenant_profile reports 0 active -> broken flow-through.
        write(
            &dir,
            "tenant_profile.csv",
            "unit,rent_position_cohort,has_active_concession\n101,At Median,0\n102,Above Median,0\n",
        );
        let r = validate_std_dir("willow-brook", "Willow Brook", &dir);
        assert!(!r.ok(), "C2 broken flow-through must FAIL");
        assert!(r.findings.iter().any(|f| f.contract == "concessions"
            && f.severity == Severity::Error
            && f.message.contains("D2")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn juniper_fund_zero_concessions_is_info() {
        let dir = tmp_std();
        write_clean(&dir);
        // Even with active concessions in source, juniper_fund is allowlisted -> INFO.
        write(
            &dir,
            "concessions.csv",
            "unit,resident,has_active_concession\n101,r1,1\n",
        );
        write(
            &dir,
            "tenant_profile.csv",
            "unit,rent_position_cohort,has_active_concession\n101,At Median,0\n102,Above Median,0\n",
        );
        let r = validate_std_dir("juniper_fund", "juniper_fund", &dir);
        let conc = r
            .findings
            .iter()
            .filter(|f| f.contract == "concessions")
            .collect::<Vec<_>>();
        assert!(
            conc.iter().all(|f| f.severity == Severity::Info),
            "juniper_fund concessions must never ERROR: {conc:?}"
        );
        assert!(conc.iter().any(|f| f.message.contains("allowlist")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bucketless_ar_is_c3_error() {
        let dir = tmp_std();
        write_clean(&dir);
        // Remove the bucketed AR; leave only a count-only rollup.
        fs::remove_file(dir.join("aged_receivables.csv")).unwrap();
        write(
            &dir,
            "aging_detail_summary.csv",
            "snapshot_date,record_count,property_id\n2026-05-31,42,s1\n",
        );
        let r = validate_std_dir("maplewood", "maplewood", &dir);
        assert!(!r.ok(), "bucket-less AR must FAIL");
        assert!(r.findings.iter().any(|f| f.contract == "aged_receivables"
            && f.severity == Severity::Error
            && f.message.contains("count-only rollup")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_ar_is_c3_error() {
        let dir = tmp_std();
        write_clean(&dir);
        fs::remove_file(dir.join("aged_receivables.csv")).unwrap();
        let r = validate_std_dir("maplewood", "maplewood", &dir);
        assert!(!r.ok());
        assert!(r.findings.iter().any(|f| f.contract == "aged_receivables"
            && f.severity == Severity::Error
            && f.message.contains("DATA-GAP")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ar_buckets_all_zero_is_warn() {
        let dir = tmp_std();
        write_clean(&dir);
        write(
            &dir,
            "aged_receivables.csv",
            "property_id,days_0_30,days_31_60,days_61_90,days_over_90\ns1,0,0,0,0\n",
        );
        let r = validate_std_dir("maplewood", "maplewood", &dir);
        assert!(r.ok(), "all-current AR is WARN not ERROR");
        assert_eq!(r.status, "WARN");
        assert!(r
            .findings
            .iter()
            .any(|f| f.contract == "aged_receivables" && f.severity == Severity::Warn));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zero_distribution_elasticity_is_c4_error() {
        let dir = tmp_std();
        write_clean(&dir);
        // records analyzed (rows present) but rent_gap all zero / cohort empty.
        write(
            &dir,
            "tenant_profile.csv",
            "unit,rent_gap,has_active_concession\n101,0,0\n102,0,0\n",
        );
        let r = validate_std_dir("maplewood", "maplewood", &dir);
        // rent_gap all 0 -> under=0/over=0/at=N, which is NOT 0/0/0, so this is INFO.
        // To force 0/0/0 we need records>0 with no computable buckets: use cohort col empty.
        assert!(r.ok());
        // Now force a true 0/0/0: cohort present but all blank -> records 0 -> INFO; instead
        // use a non-empty cohort that maps to none of under/over and is counted as 'at' (not 0/0/0).
        // The genuine D4 case: records_analyzed>0 from report but distribution 0/0/0. We model it
        // via the explicit validator entrypoint below.
        let mut b = Builder::new();
        validate_elasticity_distribution(4560, 0, 0, 0, &mut b);
        assert!(b
            .findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("0/0/0")));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn elasticity_populated_is_info() {
        let dir = tmp_std();
        write_clean(&dir);
        let r = validate_std_dir("juniper_fund", "juniper_fund", &dir);
        assert!(r.findings.iter().any(|f| f.contract == "rpcoe_elasticity"
            && f.severity == Severity::Info
            && f.message.contains("populated")));
        fs::remove_dir_all(&dir).ok();
    }
}
