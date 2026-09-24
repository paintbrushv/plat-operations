use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::{
    connectors::standardized::{source_registry::built_in_lanes, validator},
    db,
    models::Property,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloseReadinessResult {
    pub period: String,
    pub generated_at: String,
    pub summary: CloseReadinessSummary,
    pub properties: Vec<PropertyCloseReadiness>,
    pub report_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloseReadinessSummary {
    pub property_count: usize,
    pub ready_count: usize,
    pub not_ready_count: usize,
    pub blocker_count: usize,
    pub warning_count: usize,
    pub owner_ready_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropertyCloseReadiness {
    pub property_id: String,
    pub property: String,
    pub unit_count: i64,
    pub status: CloseReadinessStatus,
    pub owner_ready: bool,
    pub blockers: usize,
    pub warning_count: usize,
    pub feeds: Vec<FeedReadiness>,
    pub operator_questions: Vec<String>,
    /// Native data-contract status for this property's Standardized/*.csv.
    /// A contract ERROR is a HARD pre-publish gate: owner_ready becomes false.
    pub contract_status: ContractStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractStatus {
    /// "PASS" | "WARN" | "FAIL" | "NOT_RUN" (no lane mapped for this property).
    pub status: String,
    pub error_count: usize,
    pub warning_count: usize,
    pub contract_set_version: String,
    /// One-line per ERROR finding, for the close-readiness report.
    pub error_messages: Vec<String>,
}

impl ContractStatus {
    fn not_run() -> Self {
        ContractStatus {
            status: "NOT_RUN".to_string(),
            error_count: 0,
            warning_count: 0,
            contract_set_version: validator::CONTRACT_SET_VERSION.to_string(),
            error_messages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedReadiness {
    pub name: String,
    pub status: FeedStatus,
    pub latest_period_or_date: Option<String>,
    pub row_count: i64,
    pub required_for_owner_report: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CloseReadinessStatus {
    Ready,
    NotReady,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FeedStatus {
    Current,
    Stale,
    Missing,
}

/// Pure assessment with no filesystem side effects — used by the Close Desk
/// TUI to refresh in place without rewriting reports on every keystroke.
pub async fn assess_portfolio(
    pool: &SqlitePool,
    period: &str,
) -> Result<(CloseReadinessSummary, Vec<PropertyCloseReadiness>)> {
    validate_period(period)?;
    let properties = db::list_properties(pool).await?;
    let mut readiness = Vec::new();
    for property in properties {
        readiness.push(assess_property(pool, period, &property).await?);
    }
    let summary = summarize(&readiness);
    Ok((summary, readiness))
}

pub async fn assess_close_readiness(
    pool: &SqlitePool,
    period: &str,
    report_dir: &Path,
) -> Result<CloseReadinessResult> {
    let (summary, readiness) = assess_portfolio(pool, period).await?;

    // Persist any data-contract ERRORs found by the hard gate into the learning
    // loop so they show up under `boxscore gaps list` alongside other findings.
    let has_contract_errors = readiness.iter().any(|p| p.contract_status.error_count > 0);
    if has_contract_errors {
        let task_run_id = db::create_task_run(
            pool,
            "close_readiness_contract_gate",
            &format!("close-readiness contract gate for {period}"),
        )
        .await?;
        for property in &readiness {
            for message in &property.contract_status.error_messages {
                let description = format!(
                    "[{}] close-readiness contract gate: {message}",
                    property.property
                );
                let gap_id = db::insert_gap(
                    pool,
                    &task_run_id,
                    "data_contract_violation",
                    "error",
                    &description,
                    "A data contract failed during close-readiness; this property is \
                     NOT owner-ready and its owner report must not be published.",
                    "Fix the upstream ETL/standardizer, then re-run `boxscore validate` \
                     and close-readiness to confirm the contract passes.",
                )
                .await?;
                db::insert_evidence(
                    pool,
                    db::NewEvidence {
                        task_run_id: &task_run_id,
                        source_type: "data_contract",
                        source_table: "standardized_csv",
                        source_id: Some(&gap_id),
                        source_file: None,
                        source_row: None,
                        claim: &description,
                    },
                )
                .await?;
            }
        }
        db::complete_task_run(
            pool,
            &task_run_id,
            "completed",
            None,
            Some("Close-readiness contract gate recorded data-contract blockers."),
        )
        .await?;
    }

    fs::create_dir_all(report_dir)?;
    let report_path = report_dir.join(format!("{period}-close-readiness.md"));
    let result = CloseReadinessResult {
        period: period.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        summary,
        properties: readiness,
        report_path,
    };
    fs::write(&result.report_path, render_close_readiness_report(&result))?;
    Ok(result)
}

async fn assess_property(
    pool: &SqlitePool,
    period: &str,
    property: &Property,
) -> Result<PropertyCloseReadiness> {
    let mut feeds = vec![
        gl_feed(pool, &property.id, "Actual GL", "gl_actuals", period).await?,
        gl_feed(pool, &property.id, "Budget GL", "gl_budgets", period).await?,
        snapshot_feed(
            pool,
            &property.id,
            "Rent roll",
            "rent_roll_snapshots",
            period,
        )
        .await?,
        snapshot_feed(
            pool,
            &property.id,
            "Delinquency",
            "delinquency_snapshots",
            period,
        )
        .await?,
        snapshot_feed(pool, &property.id, "Leasing", "leasing_snapshots", period).await?,
        snapshot_feed(
            pool,
            &property.id,
            "Collections",
            "collection_snapshots",
            period,
        )
        .await?,
    ];
    feeds.extend(weekly_context_feeds(property, period));

    let feed_blockers = feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report && feed.status == FeedStatus::Missing)
        .count();
    let feed_warnings = feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report && feed.status == FeedStatus::Stale)
        .count();
    let feeds_current = feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report)
        .all(|feed| feed.status == FeedStatus::Current);

    // HARD GATE: a property's owner_ready additionally REQUIRES that its native
    // data contracts PASS. A contract ERROR blocks publication regardless of how
    // current the feeds look — bad/missing standardized data must never reach an
    // owner-facing report.
    let contract_status = assess_contract_status(property);
    let contract_error = contract_status.error_count > 0;

    let blockers = feed_blockers + contract_status.error_count;
    let warning_count = feed_warnings + contract_status.warning_count;
    let owner_ready = feed_blockers == 0 && feed_warnings == 0 && feeds_current && !contract_error;
    let status = if owner_ready {
        CloseReadinessStatus::Ready
    } else {
        CloseReadinessStatus::NotReady
    };
    let mut operator_questions = operator_questions(period, &property.name, &feeds);
    if contract_error {
        operator_questions.push(format!(
            "Data-contract validation FAILED for {} ({} contract error(s), set {}). \
             The standardized data is untrusted — fix the upstream ETL and re-run \
             `boxscore validate --lane <key>` before publishing owner reports.",
            property.name, contract_status.error_count, contract_status.contract_set_version
        ));
    }

    Ok(PropertyCloseReadiness {
        property_id: property.id.clone(),
        property: property.name.clone(),
        unit_count: property.unit_count,
        status,
        owner_ready,
        blockers,
        warning_count,
        feeds,
        operator_questions,
        contract_status,
    })
}

/// Locate the built-in lane for a property and run the native data-contract
/// validator over its Standardized/*.csv. Returns NOT_RUN if no lane maps to
/// the property (e.g. demo properties without a standardized lane on disk).
fn assess_contract_status(property: &Property) -> ContractStatus {
    let Some(lane) = built_in_lanes()
        .into_iter()
        .find(|lane| lane.display_name == property.name)
    else {
        return ContractStatus::not_run();
    };
    assess_lane_contracts(&lane)
}

/// Run the native data-contract validator over a lane and project the result
/// into a `ContractStatus`. Public so close-readiness gate behaviour can be
/// exercised against a fixture lane in integration tests without depending on
/// live property data.
pub fn assess_lane_contracts(
    lane: &crate::connectors::standardized::source_registry::PropertyLane,
) -> ContractStatus {
    let result = validator::validate_lane(lane);
    let error_messages = result
        .findings
        .iter()
        .filter(|f| f.severity == validator::Severity::Error)
        .map(|f| {
            let loc = f
                .source_file
                .as_deref()
                .map(|file| match f.source_row {
                    Some(row) => format!(" [{file}:{row}]"),
                    None => format!(" [{file}]"),
                })
                .unwrap_or_default();
            format!("[{}]{} {}", f.contract, loc, f.message)
        })
        .collect();
    ContractStatus {
        status: result.status,
        error_count: result.error_count,
        warning_count: result.warning_count,
        contract_set_version: result.contract_set_version,
        error_messages,
    }
}

async fn gl_feed(
    pool: &SqlitePool,
    property_id: &str,
    name: &str,
    table: &str,
    period: &str,
) -> Result<FeedReadiness> {
    if !matches!(table, "gl_actuals" | "gl_budgets") {
        return Err(anyhow!("unsupported GL readiness table: {table}"));
    }
    let current_count = sqlx::query(&format!(
        "SELECT COUNT(*) AS count
         FROM {table} g
         JOIN periods p ON p.id = g.period_id
         WHERE g.property_id = ? AND p.label = ?"
    ))
    .bind(property_id)
    .bind(period)
    .fetch_one(pool)
    .await?
    .get::<i64, _>("count");

    let latest = sqlx::query(&format!(
        "SELECT p.label AS label, COUNT(*) AS count
         FROM {table} g
         JOIN periods p ON p.id = g.period_id
         WHERE g.property_id = ?
         GROUP BY p.label
         ORDER BY p.label DESC
         LIMIT 1"
    ))
    .bind(property_id)
    .fetch_optional(pool)
    .await?;

    let latest_period = latest.as_ref().map(|row| row.get::<String, _>("label"));
    let latest_count = latest
        .as_ref()
        .map(|row| row.get::<i64, _>("count"))
        .unwrap_or_default();

    let status = if current_count > 0 {
        FeedStatus::Current
    } else if latest_period.is_some() {
        FeedStatus::Stale
    } else {
        FeedStatus::Missing
    };
    let note = match status {
        FeedStatus::Current => format!("{current_count} rows available for {period}."),
        FeedStatus::Stale => format!(
            "No rows for {period}; latest available period is {}.",
            latest_period.as_deref().unwrap_or("unknown")
        ),
        FeedStatus::Missing => format!("No {name} rows are available for this property."),
    };

    Ok(FeedReadiness {
        name: name.to_string(),
        status,
        latest_period_or_date: latest_period,
        row_count: if current_count > 0 {
            current_count
        } else {
            latest_count
        },
        required_for_owner_report: true,
        note,
    })
}

async fn snapshot_feed(
    pool: &SqlitePool,
    property_id: &str,
    name: &str,
    table: &str,
    period: &str,
) -> Result<FeedReadiness> {
    if !matches!(
        table,
        "rent_roll_snapshots"
            | "delinquency_snapshots"
            | "leasing_snapshots"
            | "collection_snapshots"
    ) {
        return Err(anyhow!("unsupported snapshot readiness table: {table}"));
    }
    let current = sqlx::query(&format!(
        "SELECT as_of_date, COUNT(*) AS count
         FROM {table}
         WHERE property_id = ? AND as_of_date LIKE ?
         GROUP BY as_of_date
         ORDER BY as_of_date DESC
         LIMIT 1"
    ))
    .bind(property_id)
    .bind(format!("{period}%"))
    .fetch_optional(pool)
    .await?;
    let latest = sqlx::query(&format!(
        "SELECT as_of_date, COUNT(*) AS count
         FROM {table}
         WHERE property_id = ?
         GROUP BY as_of_date
         ORDER BY as_of_date DESC
         LIMIT 1"
    ))
    .bind(property_id)
    .fetch_optional(pool)
    .await?;

    let selected = current.as_ref().or(latest.as_ref());
    let latest_date = selected.map(|row| row.get::<String, _>("as_of_date"));
    let row_count = selected
        .map(|row| row.get::<i64, _>("count"))
        .unwrap_or_default();
    let status = if current.is_some() {
        FeedStatus::Current
    } else if latest.is_some() {
        FeedStatus::Stale
    } else {
        FeedStatus::Missing
    };
    let note = match status {
        FeedStatus::Current => format!(
            "{name} snapshot is period-matched at {}.",
            latest_date.as_deref().unwrap_or(period)
        ),
        FeedStatus::Stale => format!(
            "No {period} snapshot; latest available date is {}.",
            latest_date.as_deref().unwrap_or("unknown")
        ),
        FeedStatus::Missing => format!("No {name} snapshot is available for this property."),
    };

    Ok(FeedReadiness {
        name: name.to_string(),
        status,
        latest_period_or_date: latest_date,
        row_count,
        required_for_owner_report: true,
        note,
    })
}

fn weekly_context_feeds(property: &Property, period: &str) -> Vec<FeedReadiness> {
    let Some(lane) = built_in_lanes()
        .into_iter()
        .find(|lane| lane.display_name == property.name)
    else {
        return Vec::new();
    };
    let reports_dir = lane.root_path.join("Reports");
    vec![
        weekly_report_feed(&reports_dir, "RPCOE weekly", "RPCOE_Weekly_Report_", period),
        weekly_report_feed(&reports_dir, "BDDRE weekly", "BDDRE_Weekly_Report_", period),
    ]
}

fn weekly_report_feed(
    reports_dir: &Path,
    name: &str,
    file_prefix: &str,
    period: &str,
) -> FeedReadiness {
    let mut matching_period = Vec::new();
    let mut matching_any = Vec::new();
    if let Ok(entries) = fs::read_dir(reports_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with(file_prefix) && file_name.ends_with(".md") {
                matching_any.push(file_name.clone());
                if file_name.contains(period) {
                    matching_period.push(file_name);
                }
            }
        }
    }
    matching_period.sort();
    matching_any.sort();

    let status = if !matching_period.is_empty() {
        FeedStatus::Current
    } else if !matching_any.is_empty() {
        FeedStatus::Stale
    } else {
        FeedStatus::Missing
    };
    let latest = matching_period
        .last()
        .or_else(|| matching_any.last())
        .cloned();
    let note = match status {
        FeedStatus::Current => format!(
            "{} is available for {period}.",
            latest.as_deref().unwrap_or(name)
        ),
        FeedStatus::Stale => format!(
            "No {period} {name} report; latest file is {}.",
            latest.as_deref().unwrap_or("unknown")
        ),
        FeedStatus::Missing => format!("No {name} report files were found."),
    };

    FeedReadiness {
        name: name.to_string(),
        status,
        latest_period_or_date: latest,
        row_count: matching_period.len().max(matching_any.len()) as i64,
        required_for_owner_report: false,
        note,
    }
}

fn operator_questions(period: &str, property_name: &str, feeds: &[FeedReadiness]) -> Vec<String> {
    let month_label = period_label(period);
    feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report && feed.status != FeedStatus::Current)
        .map(|feed| match feed.status {
            FeedStatus::Missing => format!(
                "Can you provide {month_label} {} for {property_name}?",
                question_feed_name(&feed.name)
            ),
            FeedStatus::Stale => format!(
                "Is the latest {} ({}) still the best available source for {month_label}, or should we wait for a refreshed file?",
                question_feed_name(&feed.name),
                feed.latest_period_or_date.as_deref().unwrap_or("unknown")
            ),
            FeedStatus::Current => String::new(),
        })
        .collect()
}

fn question_feed_name(name: &str) -> String {
    // Lowercase the leading word only when it is a normal word; acronyms like
    // "RPCOE weekly" must not become "rPCOE weekly" in operator questions.
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => {
            format!("{}{}", first.to_lowercase(), &name[first.len_utf8()..])
        }
        _ => name.to_string(),
    }
}

fn summarize(properties: &[PropertyCloseReadiness]) -> CloseReadinessSummary {
    let property_count = properties.len();
    let ready_count = properties
        .iter()
        .filter(|property| property.owner_ready)
        .count();
    let blocker_count = properties.iter().map(|property| property.blockers).sum();
    let warning_count = properties
        .iter()
        .map(|property| property.warning_count)
        .sum();
    CloseReadinessSummary {
        property_count,
        ready_count,
        not_ready_count: property_count.saturating_sub(ready_count),
        blocker_count,
        warning_count,
        owner_ready_ratio: if property_count == 0 {
            0.0
        } else {
            ready_count as f64 / property_count as f64
        },
    }
}

pub fn render_close_readiness_report(result: &CloseReadinessResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Boxscore {} Close Readiness\n\n", result.period));
    out.push_str(&format!("Generated: {}\n\n", result.generated_at));
    out.push_str("## Portfolio Summary\n\n");
    out.push_str(&format!(
        "- Owner-ready properties: {}/{}\n",
        result.summary.ready_count, result.summary.property_count
    ));
    out.push_str(&format!("- Blockers: {}\n", result.summary.blocker_count));
    out.push_str(&format!("- Warnings: {}\n", result.summary.warning_count));
    out.push_str(&format!(
        "- Owner-ready ratio: {:.0}%\n\n",
        result.summary.owner_ready_ratio * 100.0
    ));

    out.push_str("## Asset Readiness\n\n");
    out.push_str("| Property | Status | Blockers | Warnings |\n");
    out.push_str("|---|---|---:|---:|\n");
    for property in &result.properties {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            property.property,
            if property.owner_ready {
                "owner-ready"
            } else {
                "not ready"
            },
            property.blockers,
            property.warning_count
        ));
    }

    for property in &result.properties {
        out.push_str(&format!("\n## {}\n\n", property.property));
        out.push_str(&format!(
            "**Data contracts ({}):** {} ({} error, {} warn)\n\n",
            property.contract_status.contract_set_version,
            property.contract_status.status,
            property.contract_status.error_count,
            property.contract_status.warning_count
        ));
        if !property.contract_status.error_messages.is_empty() {
            out.push_str("Contract blockers (publication gated):\n\n");
            for message in &property.contract_status.error_messages {
                out.push_str(&format!("- {message}\n"));
            }
            out.push('\n');
        }
        out.push_str("| Feed | Status | Latest | Rows/Files | Note |\n");
        out.push_str("|---|---|---|---:|---|\n");
        for feed in &property.feeds {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                feed.name,
                feed_status_label(&feed.status),
                feed.latest_period_or_date.as_deref().unwrap_or("none"),
                feed.row_count,
                feed.note
            ));
        }
        if property.operator_questions.is_empty() {
            out.push_str("\nNo close-readiness operator questions for this period.\n");
        } else {
            out.push_str("\n### Operator Questions\n\n");
            for question in &property.operator_questions {
                out.push_str(&format!("- {question}\n"));
            }
        }
    }

    out.push_str("\n## Interpretation Guardrail\n\n");
    out.push_str("Boxscore should not publish an owner-ready variance narrative when required feeds are stale or missing. Stale operating context can be discussed as context, but it should not be presented as period-matched evidence.\n");
    out
}

fn feed_status_label(status: &FeedStatus) -> &'static str {
    match status {
        FeedStatus::Current => "current",
        FeedStatus::Stale => "stale",
        FeedStatus::Missing => "missing",
    }
}

fn validate_period(period: &str) -> Result<()> {
    db::parse_period_label(period).map(|_| ())
}

fn period_label(period: &str) -> String {
    let Ok((year, month)) = db::parse_period_label(period) else {
        return period.to_string();
    };
    let month = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => return period.to_string(),
    };
    format!("{month} {year}")
}
