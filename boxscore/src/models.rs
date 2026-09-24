use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Property {
    pub id: String,
    pub name: String,
    pub market: String,
    pub unit_count: i64,
    pub owner_entity: String,
    pub property_manager: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Period {
    pub id: String,
    pub year: i64,
    pub month: i64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct GlLine {
    pub id: String,
    pub property_id: String,
    pub period_id: String,
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub amount: f64,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RentRollSnapshot {
    pub id: String,
    pub property_id: String,
    pub as_of_date: String,
    pub occupied_units: i64,
    pub vacant_units: i64,
    pub leased_units: i64,
    pub notice_units: i64,
    pub down_units: i64,
    pub market_rent_total: f64,
    pub in_place_rent_total: f64,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DelinquencySnapshot {
    pub id: String,
    pub property_id: String,
    pub as_of_date: String,
    pub delinquent_amount: f64,
    pub delinquent_units: i64,
    pub prepaid_amount: f64,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LeasingSnapshot {
    pub id: String,
    pub property_id: String,
    pub as_of_date: String,
    pub leads: i64,
    pub tours: i64,
    pub applications: i64,
    pub approvals: i64,
    pub move_ins: i64,
    pub move_outs: i64,
    pub concessions_amount: f64,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CollectionSnapshot {
    pub id: String,
    pub property_id: String,
    pub as_of_date: String,
    pub total_delinquent: f64,
    pub delinquent_units: i64,
    pub high_risk_units: i64,
    pub total_opportunity: f64,
    pub pricing_opportunity: f64,
    pub missed_fee_total: f64,
    pub avg_on_time_pct: f64,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TaskRun {
    pub id: String,
    pub task_type: String,
    pub user_prompt: String,
    pub status: String,
    pub confidence_score: Option<f64>,
    pub summary: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Gap {
    pub id: String,
    pub task_run_id: String,
    pub gap_type: String,
    pub severity: String,
    pub description: String,
    pub why_it_matters: String,
    pub proposed_resolution: String,
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OperatorQuestion {
    pub id: String,
    pub task_run_id: String,
    pub question: String,
    pub reason: String,
    pub priority: i64,
    pub status: String,
    pub answer: Option<String>,
    pub created_at: String,
    pub answered_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Capability {
    pub id: String,
    pub title: String,
    pub description: String,
    pub trigger_reason: String,
    pub expected_value: String,
    pub implementation_hint: String,
    pub priority: i64,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Memory {
    pub id: String,
    pub memory_type: String,
    pub scope: String,
    pub key: String,
    pub value: String,
    pub confidence_score: f64,
    pub source_task_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EvidenceItem {
    pub id: String,
    pub task_run_id: String,
    pub source_type: String,
    pub source_table: String,
    pub source_id: Option<String>,
    pub source_file: Option<String>,
    pub source_row: Option<i64>,
    pub claim: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountMapping {
    pub id: String,
    pub source_system: String,
    pub property_scope: String,
    pub account_code: String,
    pub account_name: String,
    pub noi_category: String,
    pub confidence_score: f64,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UnmappedAccount {
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub line_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct GlTransaction {
    pub id: String,
    pub property_id: String,
    pub entity_code: String,
    pub account_code: String,
    pub txn_date: Option<String>,
    pub period: String,
    pub payee: String,
    pub is_resident: i64,
    pub control: Option<String>,
    pub reference: Option<String>,
    pub amount: f64,
    pub remarks: Option<String>,
    pub source_file: String,
    pub source_row: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Call {
    pub id: String,
    pub property_id: String,
    pub origin_period: String,
    pub call_type: String,
    pub status: String,
    pub made_at: String,
    pub mature_by: String,
    pub confidence: Option<f64>,
    pub payload_json: String,
    pub outcome_json: Option<String>,
    pub score: Option<f64>,
    pub outcome_summary: Option<String>,
    pub scored_at: Option<String>,
    pub source_task_run_id: Option<String>,
    /// A scored call whose outcome was confounded by an intervention (see migration 010).
    /// Confounded calls are excluded from the track record / self-improvement feed.
    pub confounded: bool,
    pub decision_kind: Option<String>,
    pub entities_json: Option<String>,
    pub outcome_mode: Option<String>,
    pub value_class: Option<String>,
    pub acted_on: bool,
    pub accepted_recall: Option<bool>,
    pub source_surface: Option<String>,
    pub context_json: Option<String>,
    pub operator_outcome: Option<String>,
    pub resolved_by: Option<String>,
    pub self_graded: bool,
    pub regime_tag: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MonthlyActual {
    pub id: String,
    pub property_id: String,
    pub period: String,
    pub account_code: String,
    pub account_name: Option<String>,
    pub amount: f64,
    pub source_file: String,
    pub created_at: String,
}
