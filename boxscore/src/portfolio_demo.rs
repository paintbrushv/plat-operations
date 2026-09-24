use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::{
    connectors::standardized::{
        collections_unified::ingest_collections_for_lane,
        gl_budget_comparison::ingest_budget_comparison_for_lane,
        operating_snapshots::ingest_operating_snapshots_for_lane,
        source_registry::{load_lanes, PropertyLane},
        StandardizedIngestSummary,
    },
    db,
    variance::{self, VarianceRequest},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioDemoResult {
    pub index_path: PathBuf,
    pub gl_ingests: Vec<StandardizedIngestSummary>,
    pub ops_ingests: Vec<StandardizedIngestSummary>,
    pub collections_ingests: Vec<StandardizedIngestSummary>,
    pub properties: Vec<PortfolioPropertyDemo>,
    pub open_gaps: usize,
    pub open_questions: usize,
    pub capability_backlog_items: usize,
    pub unmapped_accounts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioPropertyDemo {
    pub lane: String,
    pub property: String,
    pub period: String,
    pub report_path: PathBuf,
    pub confidence_score: f64,
    pub noi_variance: f64,
}

pub async fn run_portfolio_demo(
    pool: &SqlitePool,
    report_dir: &Path,
) -> Result<PortfolioDemoResult> {
    run_portfolio_demo_for_lanes(pool, report_dir, load_lanes()).await
}

pub async fn run_portfolio_demo_for_lanes(
    pool: &SqlitePool,
    report_dir: &Path,
    lanes: Vec<PropertyLane>,
) -> Result<PortfolioDemoResult> {
    fs::create_dir_all(report_dir)?;

    let mut gl_ingests = Vec::new();
    let mut ops_ingests = Vec::new();
    let mut collections_ingests = Vec::new();
    for lane in &lanes {
        gl_ingests.push(ingest_budget_comparison_for_lane(pool, lane).await?);
        ops_ingests.push(ingest_operating_snapshots_for_lane(pool, lane).await?);
        collections_ingests.push(ingest_collections_for_lane(pool, lane).await?);
    }

    let mut properties = Vec::new();
    for lane in &lanes {
        let property = db::require_property_by_name(pool, &lane.display_name).await?;
        let period = latest_complete_period(pool, &property.id)
            .await?
            .ok_or_else(|| anyhow!("no complete actual/budget period for {}", lane.display_name))?;
        let analysis = variance::analyze_variance(
            pool,
            VarianceRequest {
                property: lane.display_name.clone(),
                period: period.clone(),
            },
            report_dir,
        )
        .await?;
        properties.push(PortfolioPropertyDemo {
            lane: lane.property_key.clone(),
            property: lane.display_name.clone(),
            period,
            report_path: analysis
                .report_path
                .as_ref()
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("analysis did not return a report path"))?,
            confidence_score: analysis.confidence_score,
            noi_variance: analysis.noi_bridge.noi_variance,
        });
    }

    let open_gaps = db::list_gaps(pool).await?.len();
    let open_questions = db::list_questions(pool).await?.len();
    let capability_backlog_items = db::list_capabilities(pool).await?.len();
    let unmapped_accounts = db::list_unmapped_accounts(pool).await?.len();
    let result = PortfolioDemoResult {
        index_path: report_dir.join("portfolio-demo-index.md"),
        gl_ingests,
        ops_ingests,
        collections_ingests,
        properties,
        open_gaps,
        open_questions,
        capability_backlog_items,
        unmapped_accounts,
    };
    fs::write(&result.index_path, render_portfolio_index(&result))?;
    Ok(result)
}

fn render_portfolio_index(result: &PortfolioDemoResult) -> String {
    let mut out = String::new();
    out.push_str("# Boxscore Portfolio Demo Index\n\n");
    out.push_str("This local-first demo ingests standardized GL and operating snapshots, runs variance analysis for each property lane, and records the feedback loop an asset manager should inspect next.\n\n");
    out.push_str("## Portfolio Variance Reports\n\n");
    out.push_str("| Property | Lane | Period | NOI Variance | Confidence | Report |\n");
    out.push_str("|---|---|---|---:|---:|---|\n");
    for property in &result.properties {
        out.push_str(&format!(
            "| {} | {} | {} | ${:.0} | {:.0}% | {} |\n",
            property.property,
            property.lane,
            property.period,
            property.noi_variance,
            property.confidence_score * 100.0,
            property.report_path.display()
        ));
    }

    out.push_str("\n## Period Alignment Note\n\n");
    out.push_str("Boxscore selects the latest period with both actuals and budgets for each property. Operating snapshots are only used when their `as_of_date` matches that analysis month. If the property reports show missing rent roll, delinquency, or leasing evidence, the demo is preserving a real source-period mismatch instead of attaching stale or future operating data.\n");

    out.push_str("\n## Source Ingestion Summary\n\n");
    out.push_str("| Lane | GL Rows Seen | GL Rows Inserted | GL Skipped | Ops Rows Seen | Ops Snapshots Inserted | Collections Rows Seen | Gaps Created |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for ((gl, ops), collections) in result
        .gl_ingests
        .iter()
        .zip(&result.ops_ingests)
        .zip(&result.collections_ingests)
    {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            gl.lane,
            gl.rows_seen,
            gl.rows_inserted,
            gl.rows_skipped,
            ops.rows_seen,
            ops.rows_inserted,
            collections.rows_seen,
            gl.gaps_created + ops.gaps_created + collections.gaps_created
        ));
    }

    out.push_str("\n## Account Mapping Loop\n\n");
    out.push_str(&format!(
        "- Unmapped account groups requiring review: {}\n",
        result.unmapped_accounts
    ));
    out.push_str("- Recommended next operator action: run `boxscore accounts unmapped`, approve recurring mappings, then re-run the portfolio demo to compare confidence and driver classification.\n");

    out.push_str("\n## Asset Manager Review Queue\n\n");
    out.push_str(&format!(
        "- Open gaps/questions: {}/{}\n",
        result.open_gaps, result.open_questions
    ));
    out.push_str(&format!(
        "- Capability backlog items proposed: {}\n",
        result.capability_backlog_items
    ));
    out.push_str("- Review each property report for source coverage, unresolved operator questions, and whether evidence supports the NOI story before using it in owner/investor communication.\n");
    out
}

async fn latest_complete_period(pool: &SqlitePool, property_id: &str) -> Result<Option<String>> {
    let label = sqlx::query(
        "SELECT p.label
         FROM periods p
         WHERE EXISTS (
           SELECT 1 FROM gl_actuals a WHERE a.property_id = ? AND a.period_id = p.id
         )
         AND EXISTS (
           SELECT 1 FROM gl_budgets b WHERE b.property_id = ? AND b.period_id = p.id
         )
         ORDER BY p.label DESC
         LIMIT 1",
    )
    .bind(property_id)
    .bind(property_id)
    .fetch_optional(pool)
    .await?
    .map(|row| row.get::<String, _>("label"));
    Ok(label)
}
