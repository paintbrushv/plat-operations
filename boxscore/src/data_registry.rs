use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceDefinition {
    pub name: &'static str,
    pub table_name: &'static str,
    pub description: &'static str,
    pub private_data_risk: &'static str,
}

pub fn registry() -> Vec<DataSourceDefinition> {
    vec![
        source(
            "properties",
            "properties",
            "Property metadata and management context.",
            "low",
        ),
        source("periods", "periods", "Monthly reporting periods.", "low"),
        source(
            "gl_actuals",
            "gl_actuals",
            "Actual GL lines with source lineage.",
            "medium",
        ),
        source(
            "gl_budgets",
            "gl_budgets",
            "Budget GL lines with source lineage.",
            "medium",
        ),
        source(
            "rent_roll_snapshots",
            "rent_roll_snapshots",
            "Aggregated occupancy and rent roll economics.",
            "medium",
        ),
        source(
            "delinquency_snapshots",
            "delinquency_snapshots",
            "Aggregated delinquency and prepaid balances.",
            "medium",
        ),
        source(
            "leasing_snapshots",
            "leasing_snapshots",
            "Aggregated leasing funnel and concessions.",
            "medium",
        ),
        source(
            "task_runs",
            "task_runs",
            "Durable run log for harness tasks.",
            "low",
        ),
        source(
            "tool_runs",
            "tool_runs",
            "Durable run log for tool calls.",
            "low",
        ),
        source(
            "evidence_items",
            "evidence_items",
            "Claim-to-source evidence ledger.",
            "medium",
        ),
        source(
            "gaps",
            "gaps",
            "Structured missing-data and uncertainty records.",
            "low",
        ),
        source(
            "operator_questions",
            "operator_questions",
            "Human feedback loop questions and answers.",
            "medium",
        ),
        source(
            "capability_backlog",
            "capability_backlog",
            "Proposal-first product evolution backlog.",
            "low",
        ),
        source(
            "memories",
            "memories",
            "Specific reusable domain memories with confidence.",
            "medium",
        ),
    ]
}

fn source(
    name: &'static str,
    table_name: &'static str,
    description: &'static str,
    private_data_risk: &'static str,
) -> DataSourceDefinition {
    DataSourceDefinition {
        name,
        table_name,
        description,
        private_data_risk,
    }
}
