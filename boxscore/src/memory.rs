use anyhow::Result;
use sqlx::SqlitePool;

use crate::{db, variance::VarianceAnalysisResult};

pub async fn record_analysis_memories(
    pool: &SqlitePool,
    result: &VarianceAnalysisResult,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    if let Some(occupancy) = result.operating_metrics.physical_occupancy {
        ids.push(
            db::upsert_memory(
                pool,
                "property_fact",
                &result.property,
                "latest_physical_occupancy",
                &format!(
                    "{} physical occupancy was {:.1}% for {}.",
                    result.property,
                    occupancy * 100.0,
                    result.period
                ),
                result.confidence_score.min(0.80),
                Some(&result.task_run_id),
            )
            .await?,
        );
    }
    if result
        .top_negative_drivers
        .iter()
        .any(|driver| driver.category == "Repairs & Maintenance" && driver.variance.abs() > 5_000.0)
    {
        ids.push(
            db::upsert_memory(
                pool,
                "recurring_issue",
                &result.property,
                "r_and_m_overage_watch",
                "Repairs & Maintenance produced a material unfavorable variance and should be reviewed for make-ready, insurance, or one-time repair context.",
                0.55,
                Some(&result.task_run_id),
            )
            .await?,
        );
    }
    Ok(ids)
}
