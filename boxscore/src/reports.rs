use anyhow::Result;
use chrono::Utc;
use sqlx::SqlitePool;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    db,
    variance::{noi_impact, AccountVariance, VarianceAnalysisResult},
};

pub async fn issue_variance_report(
    pool: &SqlitePool,
    property_id: &str,
    period_id: &str,
    report_dir: &Path,
    result: &VarianceAnalysisResult,
) -> Result<PathBuf> {
    fs::create_dir_all(report_dir)?;
    let slug: String = result
        .property
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let path = report_dir.join(format!(
        "{slug}-{}-{}-variance.md",
        result.period, result.task_run_id
    ));
    let markdown = render_variance_report(result);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path)?;
    if let Err(err) = file
        .write_all(markdown.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(err.into());
    }
    drop(file);
    let path = fs::canonicalize(path)?;
    let saved = sqlx::query(
        "INSERT INTO variance_report_artifacts \
         (task_run_id, property_id, period_id, report_path, report_markdown, actual_noi, budget_noi, issued_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&result.task_run_id)
    .bind(property_id)
    .bind(period_id)
    .bind(path.to_string_lossy().as_ref())
    .bind(&markdown)
    .bind(result.noi_bridge.actual_noi)
    .bind(result.noi_bridge.budget_noi)
    .bind(db::now_iso())
    .execute(pool)
    .await;
    if let Err(err) = saved {
        let _ = fs::remove_file(&path);
        return Err(err.into());
    }
    Ok(path)
}

pub fn render_variance_report(result: &VarianceAnalysisResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {} {} NOI Variance Report\n\n",
        result.property, result.period
    ));
    out.push_str(&format!("Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("## Executive Summary\n\n");
    out.push_str(&format!("[Inference] {}\n\n", result.executive_summary));
    out.push_str("## Source Coverage\n\n");
    out.push_str("| Source | Coverage |\n|---|---|\n");
    out.push_str(&format!(
        "| Actual GL | {} rows |\n",
        result.source_coverage.actual_rows
    ));
    out.push_str(&format!(
        "| Budget GL | {} rows |\n",
        result.source_coverage.budget_rows
    ));
    out.push_str(&format!(
        "| Rent roll | {} |\n",
        yes_no(result.source_coverage.has_rent_roll)
    ));
    out.push_str(&format!(
        "| Delinquency | {} |\n",
        yes_no(result.source_coverage.has_delinquency)
    ));
    out.push_str(&format!(
        "| Leasing | {} |\n",
        yes_no(result.source_coverage.has_leasing)
    ));
    out.push_str(&format!(
        "| Account mappings | {} |\n",
        if result.source_coverage.account_mappings_known {
            "known categories"
        } else {
            "unmapped accounts present"
        }
    ));
    out.push_str(&format!(
        "| Collections context | {} |\n\n",
        yes_no(result.source_coverage.collections_context)
    ));
    out.push_str("## NOI Driver Confidence\n\n");
    out.push_str(&format!(
        "[Inference] Confidence is {} at {:.0}%. {}\n\n",
        confidence_band(result.confidence_score),
        result.confidence_score * 100.0,
        confidence_explanation(result)
    ));
    out.push_str("## NOI Bridge\n\n");
    out.push_str("| Metric | Actual | Budget | Variance |\n|---|---:|---:|---:|\n");
    out.push_str(&format!(
        "| Revenue | ${:.0} | ${:.0} | ${:.0} |\n",
        result.noi_bridge.actual_revenue,
        result.noi_bridge.budget_revenue,
        result.noi_bridge.revenue_variance
    ));
    out.push_str(&format!(
        "| Expenses | ${:.0} | ${:.0} | ${:.0} |\n",
        result.noi_bridge.actual_expenses,
        result.noi_bridge.budget_expenses,
        result.noi_bridge.expense_variance
    ));
    out.push_str(&format!(
        "| NOI | ${:.0} | ${:.0} | ${:.0} |\n",
        result.noi_bridge.actual_noi, result.noi_bridge.budget_noi, result.noi_bridge.noi_variance
    ));
    if result.noi_bridge.unmapped_actual != 0.0 || result.noi_bridge.unmapped_budget != 0.0 {
        out.push_str(&format!(
            "| Unmapped (excluded from NOI) | ${:.0} | ${:.0} | ${:.0} |\n",
            result.noi_bridge.unmapped_actual,
            result.noi_bridge.unmapped_budget,
            result.noi_bridge.unmapped_actual - result.noi_bridge.unmapped_budget
        ));
        out.push('\n');
        out.push_str("[Inference] Unmapped accounts are excluded from revenue, expense, and NOI totals until an operator reviews their mapping. Run `boxscore accounts export-review` to resolve them.\n");
    }
    out.push('\n');
    out.push_str("## Revenue Bridge\n\n");
    out.push_str(&category_bridge_table(
        &result.account_variances,
        &["Rental Income", "Concessions", "Bad Debt", "Other Income"],
    ));
    out.push_str("\n## Collections And Bad Debt Bridge\n\n");
    out.push_str("[Inference] Collections context helps explain whether income pressure is tied to delinquency, bad debt timing, or collection intervention opportunity. It does not prove causality without operator review.\n\n");
    out.push_str("| Signal | Value |\n|---|---:|\n");
    let (_, _, bad_debt_variance) = category_totals(&result.account_variances, &["Bad Debt"]);
    out.push_str(&format!(
        "| Bad debt variance | ${bad_debt_variance:.0} |\n"
    ));
    if let Some(amount) = result.operating_metrics.collections_total_delinquent {
        out.push_str(&format!(
            "| Collections delinquent amount | ${amount:.0} |\n"
        ));
    }
    if let Some(units) = result.operating_metrics.collections_delinquent_units {
        out.push_str(&format!("| Delinquent units/residents | {units} |\n"));
    }
    if let Some(units) = result.operating_metrics.collections_high_risk_units {
        out.push_str(&format!("| High-risk collections units | {units} |\n"));
    }
    if let Some(amount) = result.operating_metrics.collections_total_opportunity {
        out.push_str(&format!(
            "| Total collections opportunity | ${amount:.0} |\n"
        ));
    }
    if let Some(amount) = result.operating_metrics.collections_pricing_opportunity {
        out.push_str(&format!("| Pricing opportunity | ${amount:.0} |\n"));
    }
    if let Some(amount) = result.operating_metrics.collections_missed_fee_total {
        out.push_str(&format!("| Missed fee opportunity | ${amount:.0} |\n"));
    }
    if let Some(pct) = result.operating_metrics.collections_avg_on_time_pct {
        out.push_str(&format!(
            "| Average on-time payment pct | {:.1}% |\n",
            pct * 100.0
        ));
    }
    if !result.source_coverage.collections_context {
        out.push_str("| Collections context | missing |\n");
    }
    out.push('\n');
    out.push_str("\n## Expense Bridge\n\n");
    out.push_str("| Group | Actual | Budget | Variance |\n|---|---:|---:|---:|\n");
    out.push_str(&expense_group_row(
        "Controllable Expenses",
        &result.account_variances,
        &[
            "Payroll",
            "Repairs & Maintenance",
            "Utilities",
            "Marketing",
            "Administrative",
            "Management Fees",
        ],
    ));
    out.push_str(&expense_group_row(
        "Non-Controllable Expenses",
        &result.account_variances,
        &["Taxes", "Insurance"],
    ));
    out.push('\n');
    out.push_str("## Top Drivers\n\n");
    out.push_str("### Unfavorable\n\n");
    out.push_str(&drivers_table(&result.top_negative_drivers));
    out.push_str("\n### Favorable\n\n");
    out.push_str(&drivers_table(&result.top_positive_drivers));
    out.push_str("\n## Operating Evidence\n\n");
    out.push_str("[Evidence] Operating metrics below are sourced from persisted operating snapshots when available.\n\n");
    if let Some(rate) = result.operating_metrics.physical_occupancy {
        out.push_str(&format!("- Physical occupancy: {:.1}%\n", rate * 100.0));
    }
    if let Some(rate) = result.operating_metrics.economic_occupancy {
        out.push_str(&format!("- Economic occupancy: {:.1}%\n", rate * 100.0));
    }
    if let Some(amount) = result.operating_metrics.delinquent_amount {
        out.push_str(&format!("- Delinquency: ${amount:.0}\n"));
    }
    if let Some(amount) = result.operating_metrics.concessions_amount {
        out.push_str(&format!("- Leasing concessions: ${amount:.0}\n"));
    }
    if let Some(tours) = result.operating_metrics.tours {
        out.push_str(&format!("- Tours: {tours}\n"));
    }
    if let Some(applications) = result.operating_metrics.applications {
        out.push_str(&format!("- Applications: {applications}\n"));
    }
    if let Some(move_ins) = result.operating_metrics.move_ins {
        out.push_str(&format!("- Move-ins: {move_ins}\n"));
    }
    if let Some(move_outs) = result.operating_metrics.move_outs {
        out.push_str(&format!("- Move-outs: {move_outs}\n"));
    }
    out.push_str("\n## Evidence\n\n");
    for item in &result.evidence {
        out.push_str(&format!("- [Evidence] {item}\n"));
    }
    out.push_str("\n## Gaps\n\n");
    for gap in &result.gaps {
        out.push_str(&format!(
            "- [{}] {}: {} Resolution: {}\n",
            gap.severity, gap.gap_type, gap.description, gap.proposed_resolution
        ));
    }
    out.push_str("\n## Unresolved Operator Questions\n\n");
    let mut questions = result.operator_questions.clone();
    questions.sort_by_key(|question| question.priority);
    for question in &questions {
        out.push_str(&format!(
            "- P{}: {} ({})\n",
            question.priority, question.question, question.reason
        ));
    }
    out.push_str("\n## Data Quality And Next Imports\n\n");
    if result.gaps.is_empty() {
        out.push_str("- No open data-quality gaps were generated by this analysis.\n");
    } else {
        for gap in &result.gaps {
            out.push_str(&format!(
                "- [{}] {} - {}\n",
                gap.severity, gap.gap_type, gap.proposed_resolution
            ));
        }
    }
    out.push_str("\n## Confidence Score\n\n");
    out.push_str(&format!("{:.0}%\n\n", result.confidence_score * 100.0));
    out.push_str("## Next Capabilities\n\n");
    for capability in &result.suggested_capabilities {
        out.push_str(&format!(
            "- P{}: {} - {}\n",
            capability.priority, capability.title, capability.expected_value
        ));
    }
    out
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "available"
    } else {
        "missing"
    }
}

fn confidence_band(score: f64) -> &'static str {
    if score >= 0.80 {
        "high"
    } else if score >= 0.65 {
        "medium"
    } else {
        "low"
    }
}

fn confidence_explanation(result: &VarianceAnalysisResult) -> String {
    let mut reasons = Vec::new();
    if result.source_coverage.has_actuals && result.source_coverage.has_budgets {
        reasons.push("actuals and budgets are both present");
    }
    if result.source_coverage.has_rent_roll {
        reasons.push("rent roll evidence is available");
    }
    if result.source_coverage.has_delinquency {
        reasons.push("delinquency evidence is available");
    }
    if result.source_coverage.has_leasing {
        reasons.push("leasing evidence is available");
    }
    if !result.source_coverage.account_mappings_known {
        reasons.push("unmapped accounts limit category confidence");
    }
    if !result.source_coverage.collections_context {
        reasons.push("collections context is not yet imported");
    }
    if reasons.is_empty() {
        return "No source coverage details were available.".to_string();
    }
    format!("Key factors: {}.", reasons.join("; "))
}

fn category_bridge_table(lines: &[AccountVariance], categories: &[&str]) -> String {
    let mut out = String::from("| Category | Actual | Budget | Variance |\n|---|---:|---:|---:|\n");
    for category in categories {
        let (actual, budget, variance) = category_totals(lines, &[category]);
        out.push_str(&format!(
            "| {category} | ${actual:.0} | ${budget:.0} | ${variance:.0} |\n"
        ));
    }
    out
}

fn expense_group_row(label: &str, lines: &[AccountVariance], categories: &[&str]) -> String {
    let (actual, budget, variance) = category_totals(lines, categories);
    format!("| {label} | ${actual:.0} | ${budget:.0} | ${variance:.0} |\n")
}

fn category_totals(lines: &[AccountVariance], categories: &[&str]) -> (f64, f64, f64) {
    let mut actual = 0.0;
    let mut budget = 0.0;
    for line in lines {
        if categories
            .iter()
            .any(|category| line.category.eq_ignore_ascii_case(category))
        {
            actual += line.actual;
            budget += line.budget;
        }
    }
    (actual, budget, actual - budget)
}

fn drivers_table(drivers: &[AccountVariance]) -> String {
    if drivers.is_empty() {
        return "No drivers in this direction.\n".to_string();
    }
    let mut out = String::from(
        "| Account | Category | Actual | Budget | Variance | NOI Impact |\n|---|---|---:|---:|---:|---:|\n",
    );
    for driver in drivers {
        out.push_str(&format!(
            "| {} | {} | ${:.0} | ${:.0} | ${:.0} | ${:.0} |\n",
            driver.account_name,
            driver.category,
            driver.actual,
            driver.budget,
            driver.variance,
            noi_impact(driver)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evolution::CapabilityProposal,
        gaps::GapProposal,
        questions::QuestionProposal,
        variance::{
            AccountVariance, NoiBridge, OperatingMetrics, SourceCoverage, VarianceAnalysisResult,
        },
    };

    #[test]
    fn variance_report_contains_owner_ready_sections() {
        let result = VarianceAnalysisResult {
            task_run_id: "task-1".to_string(),
            property: "Fixture Property".to_string(),
            period: "2026-06".to_string(),
            executive_summary: "Fixture Property missed budgeted NOI by $100. [Inference]"
                .to_string(),
            noi_bridge: NoiBridge {
                actual_revenue: 1000.0,
                budget_revenue: 1100.0,
                revenue_variance: -100.0,
                actual_expenses: 400.0,
                budget_expenses: 350.0,
                expense_variance: 50.0,
                actual_noi: 600.0,
                budget_noi: 750.0,
                noi_variance: -150.0,
                unmapped_actual: 25.0,
                unmapped_budget: 0.0,
            },
            account_variances: vec![
                account("4000", "Apartment Rent", "Rental Income", 1000.0, 1100.0),
                account("4990", "Concessions", "Concessions", -50.0, -25.0),
                account("5200", "R&M", "Repairs & Maintenance", 400.0, 350.0),
            ],
            top_positive_drivers: Vec::new(),
            top_negative_drivers: vec![account(
                "4000",
                "Apartment Rent",
                "Rental Income",
                1000.0,
                1100.0,
            )],
            operating_metrics: OperatingMetrics {
                physical_occupancy: Some(0.92),
                economic_occupancy: Some(0.88),
                occupied_units: Some(92),
                vacant_units: Some(8),
                down_units: Some(0),
                delinquent_amount: Some(1200.0),
                delinquent_units: Some(3),
                leasing_leads: Some(0),
                tours: Some(12),
                applications: Some(5),
                move_ins: Some(2),
                move_outs: Some(1),
                concessions_amount: Some(50.0),
                collections_total_delinquent: Some(1200.0),
                collections_delinquent_units: Some(3),
                collections_high_risk_units: Some(2),
                collections_total_opportunity: Some(2500.0),
                collections_pricing_opportunity: Some(600.0),
                collections_missed_fee_total: Some(100.0),
                collections_avg_on_time_pct: Some(0.82),
            },
            source_coverage: SourceCoverage {
                actual_rows: 3,
                budget_rows: 3,
                has_actuals: true,
                has_budgets: true,
                has_rent_roll: true,
                has_delinquency: true,
                has_leasing: true,
                account_mappings_known: true,
                collections_context: true,
            },
            evidence: vec!["[Evidence] Apartment Rent actual was $1000.".to_string()],
            gaps: vec![GapProposal {
                gap_type: "missing_occupancy_budget".to_string(),
                severity: "medium".to_string(),
                description: "No occupancy budget.".to_string(),
                why_it_matters: "the operatorers.".to_string(),
                proposed_resolution: "Import occupancy budget.".to_string(),
            }],
            operator_questions: vec![QuestionProposal {
                question: "What was budgeted occupancy?".to_string(),
                reason: "Actual occupancy exists but no budget exists.".to_string(),
                priority: 2,
            }],
            suggested_capabilities: vec![CapabilityProposal {
                title: "Add occupancy budget import".to_string(),
                description: "Store occupancy budgets.".to_string(),
                trigger_reason: "Missing occupancy budget.".to_string(),
                expected_value: "Improves confidence.".to_string(),
                implementation_hint: "Add occupancy budget snapshots.".to_string(),
                priority: 1,
            }],
            confidence_score: 0.75,
            report_path: None,
        };

        let markdown = render_variance_report(&result);

        for section in [
            "## Source Coverage",
            "## NOI Driver Confidence",
            "## Revenue Bridge",
            "## Collections And Bad Debt Bridge",
            "## Expense Bridge",
            "## Operating Evidence",
            "## Unresolved Operator Questions",
            "## Data Quality And Next Imports",
        ] {
            assert!(markdown.contains(section), "missing section {section}");
        }
        assert!(markdown.contains("[Evidence]"));
        assert!(markdown.contains("[Inference]"));
    }

    fn account(
        account_code: &str,
        account_name: &str,
        category: &str,
        actual: f64,
        budget: f64,
    ) -> AccountVariance {
        AccountVariance {
            account_code: account_code.to_string(),
            account_name: account_name.to_string(),
            category: category.to_string(),
            actual,
            budget,
            variance: actual - budget,
        }
    }
}
