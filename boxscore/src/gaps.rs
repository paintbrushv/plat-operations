use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapProposal {
    pub gap_type: String,
    pub severity: String,
    pub description: String,
    pub why_it_matters: String,
    pub proposed_resolution: String,
}

#[derive(Debug, Clone)]
pub struct GapContext {
    pub actual_count: usize,
    pub budget_count: usize,
    pub has_rent_roll: bool,
    pub has_delinquency: bool,
    pub has_leasing: bool,
    pub has_collections: bool,
    pub prior_period_count: usize,
    pub account_mapping_missing: bool,
    pub largest_unexplained_variance: f64,
    pub confidence_score: f64,
}

pub struct GapEngine;

impl GapEngine {
    pub fn detect(context: &GapContext) -> Vec<GapProposal> {
        let mut gaps = Vec::new();

        if context.actual_count == 0 {
            gaps.push(gap(
                "missing_actuals",
                "critical",
                "No actual GL lines were available for the selected property and period.",
                "NOI variance cannot be calculated without actuals.",
                "Ingest the actual GL export for this property and month.",
            ));
        }
        if context.budget_count == 0 {
            gaps.push(gap(
                "missing_budget_data",
                "critical",
                "No budget GL lines were available for the selected property and period.",
                "Budget variance requires a budget baseline.",
                "Ingest the monthly budget export for this property and period.",
            ));
        }
        if !context.has_rent_roll {
            gaps.push(gap(
                "missing_rent_roll",
                "high",
                "No rent roll snapshot was available for the analysis month.",
                "Occupancy and rent roll economics often explain rental income misses.",
                "Ingest a month-end or clearly dated rent roll snapshot.",
            ));
        } else {
            gaps.push(gap(
                "missing_occupancy_budget",
                "medium",
                "Actual occupancy is available, but budgeted occupancy is not.",
                "Without budgeted occupancy, Boxscore can infer an occupancy pressure but not prove the exact budget miss driver.",
                "Add an occupancy budget import or property business-plan assumptions table.",
            ));
        }
        if !context.has_delinquency {
            gaps.push(gap(
                "missing_delinquency_snapshot",
                "medium",
                "No delinquency snapshot was available for the analysis month.",
                "Collections movement, bad debt, and prepaid balances can materially alter NOI.",
                "Ingest an aged receivables or delinquency snapshot.",
            ));
        }
        if !context.has_leasing {
            gaps.push(gap(
                "missing_leasing_data",
                "medium",
                "No leasing snapshot was available for the analysis month.",
                "Leads, tours, applications, move-ins, move-outs, and concessions explain demand and occupancy movement.",
                "Ingest weekly or monthly leasing funnel data.",
            ));
        }
        if !context.has_collections {
            gaps.push(gap(
                "missing_collections_context",
                "medium",
                "No collections context was available for the analysis month.",
                "Collections timing, delinquency risk, prepaid balances, and bad debt recognition can materially change the NOI story.",
                "Ingest collections_unified.csv or a period-matched collections rollup.",
            ));
        }
        if context.prior_period_count < 2 {
            gaps.push(gap(
                "insufficient_history",
                "medium",
                "Less than two prior periods of actuals were available for the property.",
                "Trend context is needed to separate one-time noise from recurring operational issues.",
                "Ingest at least three months of actuals and operating snapshots.",
            ));
        }
        if context.account_mapping_missing {
            gaps.push(gap(
                "missing_account_mapping",
                "medium",
                "At least one GL category is not part of the known Boxscore ontology.",
                "Unmapped accounts can be misclassified as revenue or expense.",
                "Add an account mapping table and operator review workflow.",
            ));
        }
        if context.largest_unexplained_variance.abs() > 5_000.0 {
            gaps.push(gap(
                "unexplained_variance_over_threshold",
                "high",
                "A material account variance remains only partially explained by available operating snapshots.",
                "Large unexplained variances create reporting risk and can hide control issues.",
                "Ask the operator for event context, invoices, unit blocks, concessions policy, or timing notes.",
            ));
        }
        if context.confidence_score < 0.65 {
            gaps.push(gap(
                "low_confidence_analysis",
                "high",
                "The analysis confidence score is below the product threshold.",
                "Low-confidence narratives should not be sent to owners or investors as final conclusions.",
                "Resolve high-severity data gaps and re-run the analysis.",
            ));
        }

        gaps
    }
}

fn gap(
    gap_type: &str,
    severity: &str,
    description: &str,
    why_it_matters: &str,
    proposed_resolution: &str,
) -> GapProposal {
    GapProposal {
        gap_type: gap_type.to_string(),
        severity: severity.to_string(),
        description: description.to_string(),
        why_it_matters: why_it_matters.to_string(),
        proposed_resolution: proposed_resolution.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_budget_and_rent_roll() {
        let gaps = GapEngine::detect(&GapContext {
            actual_count: 10,
            budget_count: 0,
            has_rent_roll: false,
            has_delinquency: true,
            has_leasing: true,
            has_collections: true,
            prior_period_count: 3,
            account_mapping_missing: false,
            largest_unexplained_variance: 100.0,
            confidence_score: 0.70,
        });
        assert!(gaps.iter().any(|gap| gap.gap_type == "missing_budget_data"));
        assert!(gaps.iter().any(|gap| gap.gap_type == "missing_rent_roll"));
    }
}
