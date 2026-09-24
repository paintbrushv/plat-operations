use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{db, gaps::GapProposal};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionProposal {
    pub question: String,
    pub reason: String,
    pub priority: i64,
}

pub struct QuestionEngine;

impl QuestionEngine {
    pub fn from_gaps(gaps: &[GapProposal], property: &str, period: &str) -> Vec<QuestionProposal> {
        let mut questions = Vec::new();
        for gap in gaps {
            let proposal = match gap.gap_type.as_str() {
                "missing_budget_data" => Some(q(
                    format!("Do you have the approved budget export for {property} in {period}?"),
                    "Budget data is required to quantify the NOI miss or beat.",
                    1,
                )),
                "missing_rent_roll" => Some(q(
                    format!("Can you provide the rent roll snapshot for {property} at month-end {period}?"),
                    "Rent roll data connects rental income movement to occupancy and rent levels.",
                    2,
                )),
                "missing_occupancy_budget" => Some(q(
                    format!("What was the budgeted occupancy or leased percentage for {property} in {period}?"),
                    "Actual occupancy is visible, but the budgeted occupancy assumption is missing.",
                    3,
                )),
                "missing_delinquency_snapshot" => Some(q(
                    format!("Was there a delinquency or collections event at {property} during {period}?"),
                    "Bad debt and collections timing can explain income and NOI movement.",
                    4,
                )),
                "missing_leasing_data" => Some(q(
                    format!("Were concessions intentionally increased at {property} in {period} to accelerate lease-up?"),
                    "Leasing velocity and concession policy often explain occupancy and rental income misses.",
                    5,
                )),
                "missing_collections_context" => Some(q(
                    format!("Do you have the collections, RPCOE, or BDDRE weekly context for {property} in {period}?"),
                    "Collections timing and delinquency intervention context can explain bad debt, prepaid balances, and rental income movement.",
                    4,
                )),
                "unexplained_variance_over_threshold" => Some(q(
                    format!("Was there a known move-out, unit block, repair event, insurance item, or timing issue at {property} in {period}?"),
                    "A material variance remains unexplained by currently available data.",
                    1,
                )),
                "missing_account_mapping" => Some(q(
                    "Should any unmapped accounts be assigned to Repairs & Maintenance, CapEx, utilities, or another NOI category?".to_string(),
                    "Account mapping controls whether variance drivers are classified correctly.",
                    6,
                )),
                _ => None,
            };
            if let Some(proposal) = proposal {
                questions.push(proposal);
            }
        }
        questions.sort_by_key(|question| question.priority);
        questions
    }
}

pub async fn persist_questions(
    pool: &SqlitePool,
    task_run_id: &str,
    questions: &[QuestionProposal],
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for question in questions {
        ids.push(
            db::insert_question(
                pool,
                task_run_id,
                &question.question,
                &question.reason,
                question.priority,
            )
            .await?,
        );
    }
    Ok(ids)
}

pub async fn answer_question(pool: &SqlitePool, id: &str, answer: &str) -> Result<()> {
    let answered_at = db::now_iso();
    let result = sqlx::query(
        "UPDATE operator_questions SET status = 'answered', answer = ?, answered_at = ? WHERE id = ?",
    )
    .bind(answer)
    .bind(&answered_at)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("question not found: {id}");
    }

    let question = sqlx::query_as::<_, crate::models::OperatorQuestion>(
        "SELECT * FROM operator_questions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    if answer.trim().len() >= 12 {
        db::upsert_memory(
            pool,
            "data_quality_note",
            "operator_question",
            id,
            &format!("Question: {} Answer: {}", question.question, answer.trim()),
            0.65,
            Some(&question.task_run_id),
        )
        .await?;
    }
    Ok(())
}

fn q(question: String, reason: &str, priority: i64) -> QuestionProposal {
    QuestionProposal {
        question,
        reason: reason.to_string(),
        priority,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gaps::GapProposal;

    #[test]
    fn turns_missing_occupancy_budget_into_question() {
        let questions = QuestionEngine::from_gaps(
            &[GapProposal {
                gap_type: "missing_occupancy_budget".to_string(),
                severity: "medium".to_string(),
                description: "missing".to_string(),
                why_it_matters: "matters".to_string(),
                proposed_resolution: "fix".to_string(),
            }],
            "Oak Ridge",
            "2026-05",
        );
        assert!(questions[0].question.contains("budgeted occupancy"));
    }
}
