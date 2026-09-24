use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{db, gaps::GapProposal};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProposal {
    pub title: String,
    pub description: String,
    pub trigger_reason: String,
    pub expected_value: String,
    pub implementation_hint: String,
    pub priority: i64,
}

pub struct EvolutionEngine;

impl EvolutionEngine {
    pub fn propose(gaps: &[GapProposal]) -> Vec<CapabilityProposal> {
        let mut proposals = Vec::new();
        for gap in gaps {
            let proposal = match gap.gap_type.as_str() {
                "missing_budget_data" => Some(cap(
                    "Add Yardi GL budget export parser",
                    "Import standard monthly budget exports and normalize account/category mappings.",
                    &gap.description,
                    "Reduces manual setup friction and improves variance confidence.",
                    "Create a parser adapter with source-file lineage and account mapping review.",
                    2,
                )),
                "missing_rent_roll" => Some(cap(
                    "Add rent-roll unit-level parser",
                    "Parse rent roll exports into unit-level occupancy and rent economics.",
                    &gap.description,
                    "Connects revenue variance to physical and economic occupancy drivers.",
                    "Start with CSV/XLSX schema detection and redact resident-level PII.",
                    1,
                )),
                "missing_occupancy_budget" => Some(cap(
                    "Add occupancy budget import",
                    "Store budgeted physical/economic occupancy by property and period.",
                    &gap.description,
                    "Turns inferred occupancy pressure into auditable budget variance evidence.",
                    "Add occupancy_budget_snapshots table and ingest command.",
                    1,
                )),
                "missing_leasing_data" => Some(cap(
                    "Add weekly leasing velocity dashboard",
                    "Track leads, tours, applications, approvals, move-ins, move-outs, and concessions.",
                    &gap.description,
                    "Gives operators faster warning when occupancy risk is emerging.",
                    "Normalize leasing source exports and build period rollups.",
                    3,
                )),
                "missing_collections_context" => Some(cap(
                    "Add collections and bad debt bridge",
                    "Import collections context and connect delinquency, prepaid balances, interventions, and bad debt timing to NOI variance.",
                    &gap.description,
                    "Improves confidence in revenue shortfall explanations and collections accountability.",
                    "Normalize collections_unified, RPCOE, and BDDRE weekly reports into period rollups.",
                    2,
                )),
                "missing_account_mapping" => Some(cap(
                    "Add account mapping UI",
                    "Let operators map GL accounts to Boxscore ontology categories.",
                    &gap.description,
                    "Prevents recurring category ambiguity and improves institutional reporting consistency.",
                    "Back the UI with a durable account_mappings table and review status.",
                    4,
                )),
                "unexplained_variance_over_threshold" => Some(cap(
                    "Add anomaly detection for controllable expenses",
                    "Flag unusual R&M, payroll, marketing, and administrative expense movements.",
                    &gap.description,
                    "Shortens the path from GL variance to accountable operating follow-up.",
                    "Use rolling historical baselines before introducing statistical models.",
                    5,
                )),
                _ => None,
            };
            if let Some(proposal) = proposal {
                if !proposals
                    .iter()
                    .any(|existing: &CapabilityProposal| existing.title == proposal.title)
                {
                    proposals.push(proposal);
                }
            }
        }
        proposals.sort_by_key(|proposal| proposal.priority);
        proposals
    }
}

pub async fn persist_capabilities(
    pool: &SqlitePool,
    proposals: &[CapabilityProposal],
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for proposal in proposals {
        let id = db::new_id();
        sqlx::query(
            "INSERT INTO capability_backlog (id, title, description, trigger_reason, expected_value, implementation_hint, priority, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'proposed', ?)
             ON CONFLICT(title) DO UPDATE SET trigger_reason = excluded.trigger_reason, priority = min(capability_backlog.priority, excluded.priority)",
        )
        .bind(&id)
        .bind(&proposal.title)
        .bind(&proposal.description)
        .bind(&proposal.trigger_reason)
        .bind(&proposal.expected_value)
        .bind(&proposal.implementation_hint)
        .bind(proposal.priority)
        .bind(db::now_iso())
        .execute(pool)
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

fn cap(
    title: &str,
    description: &str,
    trigger_reason: &str,
    expected_value: &str,
    implementation_hint: &str,
    priority: i64,
) -> CapabilityProposal {
    CapabilityProposal {
        title: title.to_string(),
        description: description.to_string(),
        trigger_reason: trigger_reason.to_string(),
        expected_value: expected_value.to_string(),
        implementation_hint: implementation_hint.to_string(),
        priority,
    }
}
