//! Connectors for standardized multifamily operating CSVs.
//!
//! These adapters transform `data/*/Standardized/` files into
//! Boxscore tables. The older `ingest.rs` module remains the sample/generic
//! CSV path used by the demo data.

pub mod account_mapping;
pub mod aged_receivables;
pub mod collections_unified;
pub mod gl_budget_comparison;
pub mod gl_transactions;
pub mod leasing_funnel;
pub mod operating_snapshots;
pub mod profiler;
pub mod rent_roll;
pub mod source_registry;
pub mod turn_costs;
pub mod unit_pnl;
pub mod validator;

use serde::{Deserialize, Serialize};

/// Source-system label the legacy standardized-CSV lanes stamp on account
/// mappings and review rows. One home for the string so the label cannot
/// drift between the GL budget connector and the account review export.
/// It is a lineage tag, not an ingestion profile: the exact-cent adapters in
/// `exact::ingest` use versioned profile ids instead.
pub const STANDARDIZED_SOURCE_SYSTEM: &str = "standardized-yardi";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StandardizedIngestSummary {
    pub lane: String,
    pub source_file: String,
    pub rows_seen: usize,
    pub rows_inserted: usize,
    pub rows_skipped: usize,
    pub gaps_created: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceLineage {
    pub source_file: String,
    pub source_row: i64,
    pub source_system: String,
    pub raw_reference: Option<String>,
}
