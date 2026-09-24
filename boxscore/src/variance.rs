use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::{
    db,
    evolution::{self, CapabilityProposal, EvolutionEngine},
    gaps::{GapContext, GapEngine, GapProposal},
    memory,
    models::{CollectionSnapshot, DelinquencySnapshot, GlLine, LeasingSnapshot, RentRollSnapshot},
    ontology::{self, AccountClass},
    questions::{self, QuestionEngine, QuestionProposal},
    reports, tools,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarianceRequest {
    pub property: String,
    pub period: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVariance {
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub actual: f64,
    pub budget: f64,
    pub variance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiBridge {
    pub actual_revenue: f64,
    pub budget_revenue: f64,
    pub revenue_variance: f64,
    pub actual_expenses: f64,
    pub budget_expenses: f64,
    pub expense_variance: f64,
    pub actual_noi: f64,
    pub budget_noi: f64,
    pub noi_variance: f64,
    /// Unmapped account totals, excluded from the bridge above and disclosed
    /// separately so nothing silently disappears from owner review.
    #[serde(default)]
    pub unmapped_actual: f64,
    #[serde(default)]
    pub unmapped_budget: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OperatingMetrics {
    pub physical_occupancy: Option<f64>,
    pub economic_occupancy: Option<f64>,
    pub occupied_units: Option<i64>,
    pub vacant_units: Option<i64>,
    pub down_units: Option<i64>,
    pub delinquent_amount: Option<f64>,
    pub delinquent_units: Option<i64>,
    pub leasing_leads: Option<i64>,
    pub tours: Option<i64>,
    pub applications: Option<i64>,
    pub move_ins: Option<i64>,
    pub move_outs: Option<i64>,
    pub concessions_amount: Option<f64>,
    pub collections_total_delinquent: Option<f64>,
    pub collections_delinquent_units: Option<i64>,
    pub collections_high_risk_units: Option<i64>,
    pub collections_total_opportunity: Option<f64>,
    pub collections_pricing_opportunity: Option<f64>,
    pub collections_missed_fee_total: Option<f64>,
    pub collections_avg_on_time_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceCoverage {
    pub actual_rows: usize,
    pub budget_rows: usize,
    pub has_actuals: bool,
    pub has_budgets: bool,
    pub has_rent_roll: bool,
    pub has_delinquency: bool,
    pub has_leasing: bool,
    pub account_mappings_known: bool,
    pub collections_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarianceAnalysisResult {
    pub task_run_id: String,
    pub property: String,
    pub period: String,
    pub executive_summary: String,
    pub noi_bridge: NoiBridge,
    pub account_variances: Vec<AccountVariance>,
    pub top_positive_drivers: Vec<AccountVariance>,
    pub top_negative_drivers: Vec<AccountVariance>,
    pub operating_metrics: OperatingMetrics,
    pub source_coverage: SourceCoverage,
    pub evidence: Vec<String>,
    pub gaps: Vec<GapProposal>,
    pub operator_questions: Vec<QuestionProposal>,
    pub suggested_capabilities: Vec<CapabilityProposal>,
    pub confidence_score: f64,
    pub report_path: Option<String>,
}

pub async fn analyze_variance(
    pool: &SqlitePool,
    request: VarianceRequest,
    report_dir: &std::path::Path,
) -> Result<VarianceAnalysisResult> {
    info!(property = %request.property, period = %request.period, "starting variance analysis");
    let property = db::require_property_by_name(pool, &request.property).await?;
    let period = db::period_by_label(pool, &request.period)
        .await?
        .ok_or_else(|| anyhow!("period not found: {}", request.period))?;

    let task_run_id = db::create_task_run(
        pool,
        "ops_variance_analysis",
        &format!(
            "Analyze NOI variance for {} {}",
            request.property, request.period
        ),
    )
    .await?;

    let result = async {
        let actuals = load_gl(pool, "gl_actuals", &property.id, &period.id).await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadActualsTool",
            json!({"property": &request.property, "period": &request.period}),
            Some(json!({"rows": actuals.len()})),
            None,
        )
        .await?;

        let budgets = load_gl(pool, "gl_budgets", &property.id, &period.id).await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadBudgetsTool",
            json!({"property_id": &property.id, "period_id": &period.id}),
            Some(json!({"rows": budgets.len()})),
            None,
        )
        .await?;

        let rent_roll = latest_rent_roll(pool, &property.id, &request.period).await?;
        let delinquency = latest_delinquency(pool, &property.id, &request.period).await?;
        let leasing = latest_leasing(pool, &property.id, &request.period).await?;
        let collections = latest_collections(pool, &property.id, &request.period).await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadRentRollTool",
            json!({"property_id": &property.id, "period": &request.period}),
            Some(json!({"found": rent_roll.is_some()})),
            None,
        )
        .await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadDelinquencyTool",
            json!({"property_id": &property.id, "period": &request.period}),
            Some(json!({"found": delinquency.is_some()})),
            None,
        )
        .await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadLeasingTool",
            json!({"property_id": &property.id, "period": &request.period}),
            Some(json!({"found": leasing.is_some()})),
            None,
        )
        .await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "LoadCollectionsTool",
            json!({"property_id": &property.id, "period": &request.period}),
            Some(json!({"found": collections.is_some()})),
            None,
        )
        .await?;

        let account_variances = compute_account_variances(&actuals, &budgets);
        let noi_bridge = compute_noi_bridge(&account_variances);
        let top_positive_drivers = top_drivers(&account_variances, true);
        let top_negative_drivers = top_drivers(&account_variances, false);
        tools::log_tool_run(
            pool,
            &task_run_id,
            "ComputeVarianceTool",
            json!({"actual_rows": actuals.len(), "budget_rows": budgets.len()}),
            Some(json!({"noi_variance": noi_bridge.noi_variance})),
            None,
        )
        .await?;

        let operating_metrics = operating_metrics(
            rent_roll.as_ref(),
            delinquency.as_ref(),
            leasing.as_ref(),
            collections.as_ref(),
        );
        // A mapping is "missing" only when an account in this period has not
        // been reviewed at all. Accounts an operator reviewed and approved as
        // `Unmapped` (balance-sheet lines, capitalized repairs, mortgage
        // interest, distributions) are intentional NOI exclusions, not gaps,
        // so they must not penalize confidence.
        let unreviewed_account_count =
            db::count_unreviewed_gl_accounts(pool, &property.id, &request.period).await?;
        let account_mapping_missing = unreviewed_account_count > 0;
        let source_coverage = SourceCoverage {
            actual_rows: actuals.len(),
            budget_rows: budgets.len(),
            has_actuals: !actuals.is_empty(),
            has_budgets: !budgets.is_empty(),
            has_rent_roll: rent_roll.is_some(),
            has_delinquency: delinquency.is_some(),
            has_leasing: leasing.is_some(),
            account_mappings_known: !account_mapping_missing,
            collections_context: collections.is_some(),
        };
        let prior_period_count = prior_period_count(pool, &property.id, &request.period).await?;
        let base_confidence = confidence_score(
            &source_coverage,
            prior_period_count,
            account_mapping_missing,
        );
        let scored_calls =
            db::fetch_scored_calls(pool, &property.id, "noi_diagnosis", 20).await?;
        let calibration_points: Vec<crate::calibration::ScoredPoint> = scored_calls
            .iter()
            .filter_map(|call| {
                call.score.map(|s| crate::calibration::ScoredPoint {
                    score: s,
                    age_months: crate::recall::months_between(&request.period, &call.origin_period),
                })
            })
            .collect();
        let confidence_score = calibrate_confidence(base_confidence, &calibration_points);
        let largest_unexplained_variance = top_negative_drivers
            .first()
            .map(noi_impact)
            .unwrap_or_default();
        let mut gaps = GapEngine::detect(&GapContext {
            actual_count: actuals.len(),
            budget_count: budgets.len(),
            has_rent_roll: rent_roll.is_some(),
            has_delinquency: delinquency.is_some(),
            has_leasing: leasing.is_some(),
            has_collections: collections.is_some(),
            prior_period_count,
            account_mapping_missing,
            largest_unexplained_variance,
            confidence_score,
        });
        // Guard the sign convention: total OpEx should never be negative.
        // A negative total means the source flipped expense signs, which
        // would silently corrupt NOI without this warning.
        if noi_bridge.actual_expenses < 0.0 || noi_bridge.budget_expenses < 0.0 {
            gaps.push(GapProposal {
                gap_type: "expense_sign_anomaly".to_string(),
                severity: "high".to_string(),
                description: "Total expenses for this period are negative, which suggests the GL source stores expenses with a flipped sign convention (expected: positive costs).".to_string(),
                why_it_matters: "NOI is computed as revenue minus expenses; a flipped expense sign convention overstates or understates NOI with no other warning.".to_string(),
                proposed_resolution: "Inspect the GL source sign convention and correct the ingest adapter before publishing owner-ready variance.".to_string(),
            });
        }
        for gap in &gaps {
            db::insert_gap(
                pool,
                &task_run_id,
                &gap.gap_type,
                &gap.severity,
                &gap.description,
                &gap.why_it_matters,
                &gap.proposed_resolution,
            )
            .await?;
        }
        tools::log_tool_run(
            pool,
            &task_run_id,
            "DetectGapsTool",
            json!({"confidence_score": confidence_score}),
            Some(json!({"gaps": gaps.len()})),
            None,
        )
        .await?;

        let operator_questions =
            QuestionEngine::from_gaps(&gaps, &request.property, &request.period);
        questions::persist_questions(pool, &task_run_id, &operator_questions).await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "GenerateQuestionsTool",
            json!({"gaps": gaps.len()}),
            Some(json!({"questions": operator_questions.len()})),
            None,
        )
        .await?;

        let suggested_capabilities = EvolutionEngine::propose(&gaps);
        evolution::persist_capabilities(pool, &suggested_capabilities).await?;
        tools::log_tool_run(
            pool,
            &task_run_id,
            "UpdateCapabilityBacklogTool",
            json!({"gaps": gaps.len()}),
            Some(json!({"capabilities": suggested_capabilities.len()})),
            None,
        )
        .await?;

        let mut evidence = Vec::new();
        persist_financial_evidence(
            pool,
            &task_run_id,
            &top_positive_drivers,
            &actuals,
            &budgets,
            &mut evidence,
        )
        .await?;
        persist_financial_evidence(
            pool,
            &task_run_id,
            &top_negative_drivers,
            &actuals,
            &budgets,
            &mut evidence,
        )
        .await?;
        persist_operating_evidence(
            pool,
            &task_run_id,
            rent_roll.as_ref(),
            delinquency.as_ref(),
            leasing.as_ref(),
            collections.as_ref(),
            &mut evidence,
        )
        .await?;

        let executive_summary = executive_summary(
            &request.property,
            &request.period,
            &noi_bridge,
            &top_negative_drivers,
            &operating_metrics,
            confidence_score,
        );

        let mut analysis = VarianceAnalysisResult {
            task_run_id: task_run_id.clone(),
            property: request.property.clone(),
            period: request.period.clone(),
            executive_summary,
            noi_bridge,
            account_variances,
            top_positive_drivers,
            top_negative_drivers,
            operating_metrics,
            source_coverage,
            evidence,
            gaps,
            operator_questions,
            suggested_capabilities,
            confidence_score,
            report_path: None,
        };

        let report_path = reports::write_variance_report(report_dir, &analysis)?;
        analysis.report_path = Some(report_path.to_string_lossy().to_string());
        tools::log_tool_run(
            pool,
            &task_run_id,
            "WriteMarkdownReportTool",
            json!({"report_dir": report_dir.to_string_lossy()}),
            Some(json!({"report_path": analysis.report_path})),
            None,
        )
        .await?;

        memory::record_analysis_memories(pool, &analysis).await?;
        crate::calls::emit_noi_diagnosis_calls(pool, &property.id, &analysis).await?;
        Ok::<_, anyhow::Error>(analysis)
    }
    .await;

    match &result {
        Ok(analysis) => {
            db::complete_task_run(
                pool,
                &task_run_id,
                "completed",
                Some(analysis.confidence_score),
                Some(&analysis.executive_summary),
            )
            .await?;
            info!(task_run_id, "variance analysis complete");
        }
        Err(err) => {
            db::complete_task_run(
                pool,
                &task_run_id,
                "failed",
                Some(0.0),
                Some(&err.to_string()),
            )
            .await?;
        }
    }
    result
}

pub fn compute_account_variances(actuals: &[GlLine], budgets: &[GlLine]) -> Vec<AccountVariance> {
    let mut keys = std::collections::BTreeMap::<(String, String, String), (f64, f64)>::new();
    for line in actuals {
        let key = (
            line.account_code.clone(),
            line.account_name.clone(),
            line.category.clone(),
        );
        keys.entry(key).or_default().0 += line.amount;
    }
    for line in budgets {
        let key = (
            line.account_code.clone(),
            line.account_name.clone(),
            line.category.clone(),
        );
        keys.entry(key).or_default().1 += line.amount;
    }
    keys.into_iter()
        .map(
            |((account_code, account_name, category), (actual, budget))| AccountVariance {
                account_code,
                account_name,
                category,
                actual,
                budget,
                variance: actual - budget,
            },
        )
        .collect()
}

pub fn compute_noi_bridge(lines: &[AccountVariance]) -> NoiBridge {
    let mut actual_revenue = 0.0;
    let mut budget_revenue = 0.0;
    let mut actual_expenses = 0.0;
    let mut budget_expenses = 0.0;
    let mut unmapped_actual = 0.0;
    let mut unmapped_budget = 0.0;
    for line in lines {
        match ontology::account_class(&line.category) {
            AccountClass::Revenue => {
                actual_revenue += line.actual;
                budget_revenue += line.budget;
            }
            AccountClass::Expense => {
                actual_expenses += line.actual;
                budget_expenses += line.budget;
            }
            AccountClass::Unmapped => {
                unmapped_actual += line.actual;
                unmapped_budget += line.budget;
            }
        }
    }
    // Convention: expenses are stored with their natural positive sign
    // (matching the standardized Yardi budget_comparison exports), so
    // NOI = revenue - expenses. Contra-revenue (concessions, bad debt)
    // stays negative inside the revenue total.
    let actual_noi = actual_revenue - actual_expenses;
    let budget_noi = budget_revenue - budget_expenses;
    NoiBridge {
        actual_revenue,
        budget_revenue,
        revenue_variance: actual_revenue - budget_revenue,
        actual_expenses,
        budget_expenses,
        expense_variance: actual_expenses - budget_expenses,
        actual_noi,
        budget_noi,
        noi_variance: actual_noi - budget_noi,
        unmapped_actual,
        unmapped_budget,
    }
}

/// Signed NOI impact of an account variance: positive is favorable.
/// Revenue beating budget helps NOI; an expense over budget hurts it.
pub fn noi_impact(line: &AccountVariance) -> f64 {
    match ontology::account_class(&line.category) {
        AccountClass::Revenue => line.variance,
        AccountClass::Expense => -line.variance,
        // Unmapped accounts are excluded from the bridge, so they carry no
        // NOI impact until an operator maps them.
        AccountClass::Unmapped => 0.0,
    }
}

fn top_drivers(lines: &[AccountVariance], positive: bool) -> Vec<AccountVariance> {
    let mut drivers = lines.to_vec();
    if positive {
        drivers.sort_by(|left, right| noi_impact(right).total_cmp(&noi_impact(left)));
        drivers
            .into_iter()
            .filter(|line| noi_impact(line) > 0.0)
            .take(5)
            .collect()
    } else {
        drivers.sort_by(|left, right| noi_impact(left).total_cmp(&noi_impact(right)));
        drivers
            .into_iter()
            .filter(|line| noi_impact(line) < 0.0)
            .take(5)
            .collect()
    }
}

fn operating_metrics(
    rent_roll: Option<&RentRollSnapshot>,
    delinquency: Option<&DelinquencySnapshot>,
    leasing: Option<&LeasingSnapshot>,
    collections: Option<&CollectionSnapshot>,
) -> OperatingMetrics {
    OperatingMetrics {
        physical_occupancy: rent_roll.and_then(|row| {
            ontology::occupancy_rate(row.occupied_units, row.vacant_units, row.down_units)
        }),
        economic_occupancy: rent_roll.and_then(|row| {
            ontology::economic_occupancy(row.in_place_rent_total, row.market_rent_total)
        }),
        occupied_units: rent_roll.map(|row| row.occupied_units),
        vacant_units: rent_roll.map(|row| row.vacant_units),
        down_units: rent_roll.map(|row| row.down_units),
        delinquent_amount: delinquency.map(|row| row.delinquent_amount),
        delinquent_units: delinquency.map(|row| row.delinquent_units),
        leasing_leads: leasing.map(|row| row.leads),
        tours: leasing.map(|row| row.tours),
        applications: leasing.map(|row| row.applications),
        move_ins: leasing.map(|row| row.move_ins),
        move_outs: leasing.map(|row| row.move_outs),
        concessions_amount: leasing.map(|row| row.concessions_amount),
        collections_total_delinquent: collections.map(|row| row.total_delinquent),
        collections_delinquent_units: collections.map(|row| row.delinquent_units),
        collections_high_risk_units: collections.map(|row| row.high_risk_units),
        collections_total_opportunity: collections.map(|row| row.total_opportunity),
        collections_pricing_opportunity: collections.map(|row| row.pricing_opportunity),
        collections_missed_fee_total: collections.map(|row| row.missed_fee_total),
        collections_avg_on_time_pct: collections.map(|row| row.avg_on_time_pct),
    }
}

fn confidence_score(
    source_coverage: &SourceCoverage,
    prior_period_count: usize,
    account_mapping_missing: bool,
) -> f64 {
    let mut score: f64 = 0.20;
    if source_coverage.has_actuals {
        score += 0.20;
    }
    if source_coverage.has_budgets {
        score += 0.20;
    }
    if source_coverage.has_rent_roll {
        score += 0.15;
    }
    if source_coverage.has_delinquency {
        score += 0.10;
    }
    if source_coverage.has_leasing {
        score += 0.10;
    }
    if source_coverage.collections_context {
        score += 0.05;
    }
    if prior_period_count >= 2 {
        score += 0.05;
    }
    if account_mapping_missing {
        score -= 0.10;
    }
    score.clamp(0.0, 0.95)
}

/// Adjust a base confidence by the harness's track record on this lane+call-type.
/// Uses a Beta-Binomial posterior over time-decayed scored points — no MIN_CALLS cliff.
/// Small-n shrinkage toward the 0.5 prior naturally guards against over-reacting to noise.
/// A strong fresh perfect record nudges up by up to ~0.10; a poor one nudges down similarly.
fn calibrate_confidence(base: f64, points: &[crate::calibration::ScoredPoint]) -> f64 {
    let stat = crate::calibration::calibrated_stat(
        points,
        crate::calibration::PRIOR_MEAN,
        crate::calibration::PRIOR_STRENGTH,
        crate::calibration::HALF_LIFE_MONTHS,
        crate::calibration::ABSTAIN_N_EFF,
    );
    (base + (stat.posterior_mean - 0.5) * 0.2).clamp(0.0, 0.95)
}

fn executive_summary(
    property: &str,
    period: &str,
    bridge: &NoiBridge,
    negative_drivers: &[AccountVariance],
    metrics: &OperatingMetrics,
    confidence_score: f64,
) -> String {
    let result_word = if bridge.noi_variance >= 0.0 {
        "beat"
    } else {
        "missed"
    };
    let mut summary = format!(
        "{property} {result_word} budgeted NOI by ${}.",
        dollars(bridge.noi_variance.abs())
    );
    if !negative_drivers.is_empty() {
        let drivers = negative_drivers
            .iter()
            .take(3)
            .map(|driver| driver.account_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        summary.push_str(&format!(" The largest unfavorable drivers were {drivers}."));
    }
    if let Some(occupancy) = metrics.physical_occupancy {
        summary.push_str(&format!(
            " Physical occupancy was {:.1}% for {period}, but no budgeted occupancy file is assumed in v0.1 unless imported later.",
            occupancy * 100.0
        ));
    }
    if let Some(delinquent_amount) = metrics.delinquent_amount {
        summary.push_str(&format!(
            " Delinquency stood at ${}, which should be compared against collections and bad debt timing.",
            dollars(delinquent_amount)
        ));
    }
    summary.push_str(&format!(
        " Confidence score: {:.0}%.",
        confidence_score * 100.0
    ));
    summary
}

fn dollars(value: f64) -> String {
    format!("{value:.0}")
}

async fn load_gl(
    pool: &SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
) -> Result<Vec<GlLine>> {
    let sql = format!(
        "SELECT * FROM {table} WHERE property_id = ? AND period_id = ? ORDER BY account_code"
    );
    sqlx::query_as::<_, GlLine>(&sql)
        .bind(property_id)
        .bind(period_id)
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

async fn latest_rent_roll(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Option<RentRollSnapshot>> {
    sqlx::query_as::<_, RentRollSnapshot>(
        "SELECT * FROM rent_roll_snapshots WHERE property_id = ? AND as_of_date LIKE ? ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(format!("{period}%"))
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn latest_delinquency(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Option<DelinquencySnapshot>> {
    sqlx::query_as::<_, DelinquencySnapshot>(
        "SELECT * FROM delinquency_snapshots WHERE property_id = ? AND as_of_date LIKE ? ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(format!("{period}%"))
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn latest_leasing(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Option<LeasingSnapshot>> {
    sqlx::query_as::<_, LeasingSnapshot>(
        "SELECT * FROM leasing_snapshots WHERE property_id = ? AND as_of_date LIKE ? ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(format!("{period}%"))
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn latest_collections(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Option<CollectionSnapshot>> {
    sqlx::query_as::<_, CollectionSnapshot>(
        "SELECT * FROM collection_snapshots WHERE property_id = ? AND as_of_date LIKE ? ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(format!("{period}%"))
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn prior_period_count(pool: &SqlitePool, property_id: &str, period: &str) -> Result<usize> {
    let count = sqlx::query(
        "SELECT COUNT(DISTINCT p.label) AS count FROM gl_actuals a JOIN periods p ON p.id = a.period_id WHERE a.property_id = ? AND p.label < ?",
    )
    .bind(property_id)
    .bind(period)
    .fetch_one(pool)
    .await?
    .get::<i64, _>("count");
    Ok(count as usize)
}

async fn persist_financial_evidence(
    pool: &SqlitePool,
    task_run_id: &str,
    drivers: &[AccountVariance],
    actuals: &[GlLine],
    budgets: &[GlLine],
    evidence: &mut Vec<String>,
) -> Result<()> {
    for driver in drivers {
        if let Some(actual) = actuals
            .iter()
            .find(|line| line.account_code == driver.account_code)
        {
            let claim = format!(
                "{} actual was ${:.0}; variance vs budget was ${:.0}.",
                driver.account_name, driver.actual, driver.variance
            );
            let evidence_id = db::insert_evidence(
                pool,
                db::NewEvidence {
                    task_run_id,
                    source_type: "financial",
                    source_table: "gl_actuals",
                    source_id: Some(&actual.id),
                    source_file: Some(&actual.source_file),
                    source_row: Some(actual.source_row),
                    claim: &claim,
                },
            )
            .await?;
            evidence.push(format!("[evidence:{evidence_id}] {claim}"));
        }
        if let Some(budget) = budgets
            .iter()
            .find(|line| line.account_code == driver.account_code)
        {
            let claim = format!("{} budget was ${:.0}.", driver.account_name, driver.budget);
            db::insert_evidence(
                pool,
                db::NewEvidence {
                    task_run_id,
                    source_type: "financial",
                    source_table: "gl_budgets",
                    source_id: Some(&budget.id),
                    source_file: Some(&budget.source_file),
                    source_row: Some(budget.source_row),
                    claim: &claim,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn persist_operating_evidence(
    pool: &SqlitePool,
    task_run_id: &str,
    rent_roll: Option<&RentRollSnapshot>,
    delinquency: Option<&DelinquencySnapshot>,
    leasing: Option<&LeasingSnapshot>,
    collections: Option<&CollectionSnapshot>,
    evidence: &mut Vec<String>,
) -> Result<()> {
    if let Some(row) = rent_roll {
        let claim = format!(
            "Rent roll showed {} occupied, {} vacant, and {} down units as of {}.",
            row.occupied_units, row.vacant_units, row.down_units, row.as_of_date
        );
        let evidence_id = db::insert_evidence(
            pool,
            db::NewEvidence {
                task_run_id,
                source_type: "operating",
                source_table: "rent_roll_snapshots",
                source_id: Some(&row.id),
                source_file: Some(&row.source_file),
                source_row: Some(row.source_row),
                claim: &claim,
            },
        )
        .await?;
        evidence.push(format!("[evidence:{evidence_id}] {claim}"));
    }
    if let Some(row) = delinquency {
        let claim = format!(
            "Delinquency snapshot showed ${:.0} across {} units as of {}.",
            row.delinquent_amount, row.delinquent_units, row.as_of_date
        );
        let evidence_id = db::insert_evidence(
            pool,
            db::NewEvidence {
                task_run_id,
                source_type: "operating",
                source_table: "delinquency_snapshots",
                source_id: Some(&row.id),
                source_file: Some(&row.source_file),
                source_row: Some(row.source_row),
                claim: &claim,
            },
        )
        .await?;
        evidence.push(format!("[evidence:{evidence_id}] {claim}"));
    }
    if let Some(row) = leasing {
        let claim = format!(
            "Leasing snapshot showed {} leads, {} tours, {} move-ins, {} move-outs, and ${:.0} concessions as of {}.",
            row.leads, row.tours, row.move_ins, row.move_outs, row.concessions_amount, row.as_of_date
        );
        let evidence_id = db::insert_evidence(
            pool,
            db::NewEvidence {
                task_run_id,
                source_type: "operating",
                source_table: "leasing_snapshots",
                source_id: Some(&row.id),
                source_file: Some(&row.source_file),
                source_row: Some(row.source_row),
                claim: &claim,
            },
        )
        .await?;
        evidence.push(format!("[evidence:{evidence_id}] {claim}"));
    }
    if let Some(row) = collections {
        let claim = format!(
            "Collections context showed ${:.0} delinquent, {} delinquent units/residents, {} high-risk units, and ${:.0} total opportunity as of {}.",
            row.total_delinquent,
            row.delinquent_units,
            row.high_risk_units,
            row.total_opportunity,
            row.as_of_date
        );
        let evidence_id = db::insert_evidence(
            pool,
            db::NewEvidence {
                task_run_id,
                source_type: "operating",
                source_table: "collection_snapshots",
                source_id: Some(&row.id),
                source_file: Some(&row.source_file),
                source_row: Some(row.source_row),
                claim: &claim,
            },
        )
        .await?;
        evidence.push(format!("[evidence:{evidence_id}] {claim}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(code: &str, category: &str, amount: f64) -> GlLine {
        GlLine {
            id: code.to_string(),
            property_id: "p".to_string(),
            period_id: "m".to_string(),
            account_code: code.to_string(),
            account_name: code.to_string(),
            category: category.to_string(),
            amount,
            source_file: "sample.csv".to_string(),
            source_row: 1,
            created_at: "now".to_string(),
        }
    }

    #[test]
    fn computes_noi_variance_from_revenue_and_expense_lines() {
        // Expenses use the natural positive sign, matching the standardized
        // Yardi budget_comparison exports (e.g. Manager Salaries 5253.0).
        let actuals = vec![
            line("4000", "Rental Income", 100.0),
            line("5200", "Repairs & Maintenance", 30.0),
        ];
        let budgets = vec![
            line("4000", "Rental Income", 110.0),
            line("5200", "Repairs & Maintenance", 20.0),
        ];
        let variances = compute_account_variances(&actuals, &budgets);
        let bridge = compute_noi_bridge(&variances);
        assert_eq!(bridge.actual_noi, 70.0);
        assert_eq!(bridge.budget_noi, 90.0);
        assert_eq!(bridge.noi_variance, -20.0);
        // Expense overrun of $10 shows as +10 raw variance but -10 NOI impact.
        assert_eq!(bridge.expense_variance, 10.0);
    }

    #[test]
    fn expense_overruns_rank_as_unfavorable_drivers() {
        let actuals = vec![
            line("4000", "Rental Income", 100.0),
            line("5200", "Repairs & Maintenance", 80.0),
        ];
        let budgets = vec![
            line("4000", "Rental Income", 90.0),
            line("5200", "Repairs & Maintenance", 20.0),
        ];
        let variances = compute_account_variances(&actuals, &budgets);
        let unfavorable = top_drivers(&variances, false);
        let favorable = top_drivers(&variances, true);
        // The $60 expense overrun must be unfavorable, not favorable.
        assert_eq!(unfavorable.len(), 1);
        assert_eq!(unfavorable[0].account_code, "5200");
        assert_eq!(favorable.len(), 1);
        assert_eq!(favorable[0].account_code, "4000");
    }

    #[test]
    fn unmapped_accounts_are_excluded_from_noi_but_disclosed() {
        let actuals = vec![
            line("4000", "Rental Income", 100.0),
            line("3000", "Unmapped", 500_000.0),
        ];
        let budgets = vec![line("4000", "Rental Income", 90.0)];
        let variances = compute_account_variances(&actuals, &budgets);
        let bridge = compute_noi_bridge(&variances);
        // A balance-sheet-sized unmapped row must not distort NOI...
        assert_eq!(bridge.actual_noi, 100.0);
        assert_eq!(bridge.actual_expenses, 0.0);
        // ...but it must be disclosed, not silently dropped.
        assert_eq!(bridge.unmapped_actual, 500_000.0);
        // And it must not appear as a favorable/unfavorable driver.
        assert!(top_drivers(&variances, false)
            .iter()
            .all(|driver| driver.account_code != "3000"));
        assert!(top_drivers(&variances, true)
            .iter()
            .all(|driver| driver.account_code != "3000"));
    }

    #[test]
    fn calibrate_confidence_uses_posterior_no_cliff() {
        use crate::calibration::ScoredPoint;
        // No history: posterior = prior mean 0.5 → adjustment 0 → returns base.
        let none: Vec<ScoredPoint> = vec![];
        assert!((calibrate_confidence(0.6, &none) - 0.6).abs() < 1e-6);
        // Strong fresh record nudges up but stays clamped to 0.95.
        let good: Vec<ScoredPoint> = (0..20)
            .map(|_| ScoredPoint {
                score: 1.0,
                age_months: 0.0,
            })
            .collect();
        assert!(calibrate_confidence(0.6, &good) > 0.6);
        assert!(calibrate_confidence(0.6, &good) <= 0.95);
    }

    #[test]
    fn calibrate_confidence_nudges_up_on_perfect_record() {
        use crate::calibration::ScoredPoint;
        // 20 fresh perfect scores — posterior pulls well above 0.5, nudges up.
        // posterior_mean = (20 + 8*0.5)/(20+8) = 24/28 ≈ 0.857
        // adjustment = (0.857 - 0.5) * 0.2 ≈ 0.0714
        // 0.85 + 0.0714 = 0.921 < 0.95, not clamped
        let good: Vec<ScoredPoint> = (0..20)
            .map(|_| ScoredPoint {
                score: 1.0,
                age_months: 0.0,
            })
            .collect();
        let result = calibrate_confidence(0.85, &good);
        assert!(result > 0.85, "expected nudge up but got {result}");
        assert!(result <= 0.95, "expected clamped ≤ 0.95 but got {result}");
        let result2 = calibrate_confidence(0.70, &good);
        assert!(result2 > 0.70, "expected nudge up but got {result2}");
        assert!(result2 <= 0.95);
    }

    #[test]
    fn calibrate_confidence_nudges_down_on_zero_record() {
        use crate::calibration::ScoredPoint;
        // 20 fresh zero scores — posterior pulls below 0.5, nudges down.
        // posterior_mean = (0 + 8*0.5)/(20+8) = 4/28 ≈ 0.143
        // adjustment = (0.143 - 0.5) * 0.2 ≈ -0.0714; 0.70 - 0.0714 ≈ 0.629
        let bad: Vec<ScoredPoint> = (0..20)
            .map(|_| ScoredPoint {
                score: 0.0,
                age_months: 0.0,
            })
            .collect();
        let result = calibrate_confidence(0.70, &bad);
        assert!(result < 0.70, "expected nudge down but got {result}");
        assert!(result >= 0.0);
    }

    #[test]
    fn calibrate_confidence_no_adjustment_on_empty_history() {
        use crate::calibration::ScoredPoint;
        // Empty points → posterior_mean = PRIOR_MEAN = 0.5 → (0.5-0.5)*0.2 = 0.0 → base unchanged.
        let none: Vec<ScoredPoint> = vec![];
        let result = calibrate_confidence(0.70, &none);
        assert!(
            (result - 0.70).abs() < 1e-6,
            "expected 0.70 but got {result}"
        );
    }

    #[test]
    fn contra_revenue_concessions_growth_is_unfavorable() {
        // Concessions are contra-revenue stored negative; giving away more
        // concessions than budget (more negative) must rank unfavorable.
        let actuals = vec![line("4990", "Concessions", -50.0)];
        let budgets = vec![line("4990", "Concessions", -20.0)];
        let variances = compute_account_variances(&actuals, &budgets);
        let unfavorable = top_drivers(&variances, false);
        assert_eq!(unfavorable.len(), 1);
        assert_eq!(unfavorable[0].account_code, "4990");
    }
}
