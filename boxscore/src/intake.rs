use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntakeScanResult {
    pub period: String,
    pub inbox: PathBuf,
    pub generated_at: String,
    pub summary: IntakeSummary,
    pub files: Vec<IntakeFileProfile>,
    pub report_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntakeSummary {
    pub file_count: usize,
    pub financial_candidates: usize,
    pub operating_files: usize,
    pub weekly_context_files: usize,
    pub unsupported_files: usize,
    pub period_mismatches: usize,
    pub ready_for_review: usize,
    pub needs_operator_review: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntakeFileProfile {
    pub path: PathBuf,
    pub file_name: String,
    pub extension: String,
    pub source_type: IntakeSourceType,
    pub status: IntakeStatus,
    pub likely_property: Option<String>,
    pub likely_period: Option<String>,
    pub row_count: usize,
    pub headers: Vec<String>,
    pub privacy_level: String,
    pub import_recommendation: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntakeSourceType {
    FinancialActualBudgetCandidate,
    RentRoll,
    Delinquency,
    Leasing,
    Collections,
    RpcOeWeekly,
    BddreWeekly,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntakeStatus {
    ReadyForReview,
    NeedsOperatorReview,
    Unsupported,
}

pub fn scan_inbox(inbox: &Path, period: &str, report_dir: &Path) -> Result<IntakeScanResult> {
    db::parse_period_label(period)?;
    let mut files = Vec::new();
    collect_files(inbox, &mut files)
        .with_context(|| format!("failed to scan inbox: {}", inbox.display()))?;

    let mut profiles = files
        .into_iter()
        .map(|path| profile_file(&path, period))
        .collect::<Result<Vec<_>>>()?;
    profiles.sort_by(|left, right| left.path.cmp(&right.path));

    fs::create_dir_all(report_dir)?;
    let summary = summarize(&profiles);
    let report_path = report_dir.join(format!("{period}-intake-workbench.md"));
    let result = IntakeScanResult {
        period: period.to_string(),
        inbox: inbox.to_path_buf(),
        generated_at: Utc::now().to_rfc3339(),
        summary,
        files: profiles,
        report_path,
    };
    fs::write(&result.report_path, render_intake_report(&result))?;
    Ok(result)
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // Skip hidden entries and tooling directories: they are never close
        // materials, and binary blobs inside them would only add noise.
        if name.starts_with('.')
            || matches!(name, "target" | "node_modules" | "__pycache__" | "venv")
        {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn profile_file(path: &Path, target_period: &str) -> Result<IntakeFileProfile> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut warnings = Vec::new();
    // One unreadable file must not abort the whole scan; profile what we can
    // and surface the problem as a warning for operator review.
    let csv_profile = if extension == "csv" {
        match read_csv_profile(path) {
            Ok(profile) => profile,
            Err(err) => {
                warnings.push(format!("Could not profile CSV contents: {err}."));
                CsvIntakeProfile::default()
            }
        }
    } else {
        CsvIntakeProfile::default()
    };
    let source_type = classify_source_type(&file_name, &csv_profile.headers);
    let likely_period = infer_period(&file_name, &csv_profile.sample_values);
    let likely_property = infer_property(&file_name, &csv_profile.sample_values);
    if let Some(period) = &likely_period {
        if period != target_period {
            warnings.push(format!(
                "Detected period {period} does not match target period {target_period}."
            ));
        }
    } else if matches!(
        source_type,
        IntakeSourceType::FinancialActualBudgetCandidate
            | IntakeSourceType::RentRoll
            | IntakeSourceType::Delinquency
            | IntakeSourceType::Leasing
            | IntakeSourceType::Collections
    ) {
        warnings.push(format!(
            "Could not infer period for target {target_period}."
        ));
    }
    if extension != "csv"
        && !matches!(
            source_type,
            IntakeSourceType::RpcOeWeekly | IntakeSourceType::BddreWeekly
        )
    {
        warnings.push(format!(
            "File extension `{extension}` is not supported by the dry-run profiler."
        ));
    }

    let status = if source_type == IntakeSourceType::Unsupported {
        IntakeStatus::Unsupported
    } else if warnings.is_empty() {
        IntakeStatus::ReadyForReview
    } else {
        IntakeStatus::NeedsOperatorReview
    };

    Ok(IntakeFileProfile {
        path: path.to_path_buf(),
        file_name,
        extension,
        source_type: source_type.clone(),
        status,
        likely_property,
        likely_period,
        row_count: csv_profile.row_count,
        headers: csv_profile.headers,
        privacy_level: privacy_level(&source_type).to_string(),
        import_recommendation: import_recommendation(&source_type),
        warnings,
    })
}

#[derive(Default)]
struct CsvIntakeProfile {
    headers: Vec<String>,
    row_count: usize,
    sample_values: Vec<String>,
}

fn read_csv_profile(path: &Path) -> Result<CsvIntakeProfile> {
    let mut reader = crate::parse::csv_reader_from_path(path)
        .with_context(|| format!("failed to open intake CSV: {}", path.display()))?;
    let headers = reader
        .headers()
        .map(|headers| headers.iter().map(str::to_string).collect::<Vec<_>>())?;
    let mut row_count = 0;
    let mut sample_values = headers.clone();
    for record in reader.records() {
        // Tolerate individual malformed records; this is a dry-run profiler,
        // not an importer, so a partial profile beats an aborted scan.
        let Ok(record) = record else {
            continue;
        };
        row_count += 1;
        if row_count <= 3 {
            sample_values.extend(record.iter().map(str::to_string));
        }
    }
    Ok(CsvIntakeProfile {
        headers,
        row_count,
        sample_values,
    })
}

fn classify_source_type(file_name: &str, headers: &[String]) -> IntakeSourceType {
    let name = file_name.to_ascii_lowercase();
    let header_text = headers
        .iter()
        .map(|header| header.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("|");

    if name.contains("rpcoe_weekly_report") {
        IntakeSourceType::RpcOeWeekly
    } else if name.contains("bddre_weekly_report") {
        IntakeSourceType::BddreWeekly
    } else if name.contains("budget_vs_actual")
        || name.contains("budget_variance")
        || name.contains("budget_comparison")
        || name.contains("income_statement")
        || name.contains("trial_balance")
        || (header_text.contains("actual") && header_text.contains("budget"))
    {
        IntakeSourceType::FinancialActualBudgetCandidate
    } else if name.contains("rent_roll") || header_text.contains("market_rent") {
        IntakeSourceType::RentRoll
    } else if name.contains("aged_receivable")
        || name.contains("delinquency")
        || header_text.contains("delinquent")
    {
        IntakeSourceType::Delinquency
    } else if name.contains("leasing")
        || name.contains("traffic")
        || header_text.contains("applications")
        || header_text.contains("approvals")
    {
        IntakeSourceType::Leasing
    } else if name.contains("collections") || header_text.contains("total_opportunity") {
        IntakeSourceType::Collections
    } else {
        IntakeSourceType::Unsupported
    }
}

fn infer_period(file_name: &str, sample_values: &[String]) -> Option<String> {
    find_period(file_name).or_else(|| {
        sample_values
            .iter()
            .find_map(|value| find_period(value.as_str()))
    })
}

fn find_period(value: &str) -> Option<String> {
    // ISO style: 2026-06 anywhere in the string.
    for window in value.as_bytes().windows(7) {
        if window[0..4].iter().all(u8::is_ascii_digit)
            && window[4] == b'-'
            && window[5..7].iter().all(u8::is_ascii_digit)
        {
            let candidate = String::from_utf8_lossy(window).to_string();
            if db::parse_period_label(&candidate).is_ok() {
                return Some(candidate);
            }
        }
    }
    // Yardi export style: MM_DD_YYYY / MM-DD-YYYY (e.g. LeaseExpiration06_09_2026.xlsx).
    let numbers = value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for window in numbers.windows(3) {
        if window[2].len() != 4 {
            continue;
        }
        let (Ok(month), Ok(day), Ok(year)) = (
            window[0].parse::<u32>(),
            window[1].parse::<u32>(),
            window[2].parse::<i32>(),
        ) else {
            continue;
        };
        if (1990..=2100).contains(&year)
            && chrono::NaiveDate::from_ymd_opt(year, month, day).is_some()
        {
            return Some(format!("{year:04}-{month:02}"));
        }
    }
    None
}

fn infer_property(file_name: &str, sample_values: &[String]) -> Option<String> {
    let haystack = format!(
        "{} {}",
        file_name.to_ascii_lowercase().replace(['_', '-'], " "),
        sample_values
            .iter()
            .map(|value| value.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if haystack.contains("willow brook") {
        Some("Willow Brook".to_string())
    } else if haystack.contains("juniper_fund") {
        Some("juniper_fund".to_string())
    } else if haystack.contains("maplewood") || haystack.contains("maplewood") {
        Some("Maplewood Commons".to_string())
    } else {
        None
    }
}

fn privacy_level(source_type: &IntakeSourceType) -> &'static str {
    match source_type {
        IntakeSourceType::FinancialActualBudgetCandidate => "medium",
        IntakeSourceType::RentRoll
        | IntakeSourceType::Delinquency
        | IntakeSourceType::Leasing
        | IntakeSourceType::Collections => "high",
        IntakeSourceType::RpcOeWeekly | IntakeSourceType::BddreWeekly => "medium",
        IntakeSourceType::Unsupported => "unknown",
    }
}

fn import_recommendation(source_type: &IntakeSourceType) -> String {
    match source_type {
        IntakeSourceType::FinancialActualBudgetCandidate => {
            "dry-run only: review headers, subtotals, period, and account mappings before enabling a June GL adapter.".to_string()
        }
        IntakeSourceType::RentRoll => {
            "review as operating snapshot candidate; use standardized ops ingestion only after source is normalized.".to_string()
        }
        IntakeSourceType::Delinquency => {
            "review as delinquency snapshot candidate; verify as-of date and aggregate-only import.".to_string()
        }
        IntakeSourceType::Leasing => {
            "review as leasing snapshot candidate; verify weekly date range and missing funnel fields.".to_string()
        }
        IntakeSourceType::Collections => {
            "review as collections context candidate; do not persist resident names or tenant-level PII.".to_string()
        }
        IntakeSourceType::RpcOeWeekly | IntakeSourceType::BddreWeekly => {
            "use as supporting weekly context; do not treat as substitute for period-matched GL.".to_string()
        }
        IntakeSourceType::Unsupported => {
            "unsupported by the intake workbench; operator should classify or ignore.".to_string()
        }
    }
}

fn summarize(files: &[IntakeFileProfile]) -> IntakeSummary {
    IntakeSummary {
        file_count: files.len(),
        financial_candidates: files
            .iter()
            .filter(|file| file.source_type == IntakeSourceType::FinancialActualBudgetCandidate)
            .count(),
        operating_files: files
            .iter()
            .filter(|file| {
                matches!(
                    file.source_type,
                    IntakeSourceType::RentRoll
                        | IntakeSourceType::Delinquency
                        | IntakeSourceType::Leasing
                        | IntakeSourceType::Collections
                )
            })
            .count(),
        weekly_context_files: files
            .iter()
            .filter(|file| {
                matches!(
                    file.source_type,
                    IntakeSourceType::RpcOeWeekly | IntakeSourceType::BddreWeekly
                )
            })
            .count(),
        unsupported_files: files
            .iter()
            .filter(|file| file.source_type == IntakeSourceType::Unsupported)
            .count(),
        period_mismatches: files
            .iter()
            .filter(|file| {
                file.warnings
                    .iter()
                    .any(|warning| warning.contains("does not match target period"))
            })
            .count(),
        ready_for_review: files
            .iter()
            .filter(|file| file.status == IntakeStatus::ReadyForReview)
            .count(),
        needs_operator_review: files
            .iter()
            .filter(|file| file.status == IntakeStatus::NeedsOperatorReview)
            .count(),
    }
}

pub fn render_intake_report(result: &IntakeScanResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Boxscore {} Close Intake Workbench\n\n",
        result.period
    ));
    out.push_str(&format!("Generated: {}\n\n", result.generated_at));
    out.push_str("No analytical tables were modified. This is a dry-run intake profile for operator review.\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(&format!("- Files scanned: {}\n", result.summary.file_count));
    out.push_str(&format!(
        "- Financial candidates: {}\n",
        result.summary.financial_candidates
    ));
    out.push_str(&format!(
        "- Operating files: {}\n",
        result.summary.operating_files
    ));
    out.push_str(&format!(
        "- Weekly context files: {}\n",
        result.summary.weekly_context_files
    ));
    out.push_str(&format!(
        "- Unsupported files: {}\n",
        result.summary.unsupported_files
    ));
    out.push_str(&format!(
        "- Period mismatches: {}\n\n",
        result.summary.period_mismatches
    ));

    out.push_str("## Dry-Run Import Plan\n\n");
    out.push_str("| File | Type | Status | Property | Period | Rows | Recommendation |\n");
    out.push_str("|---|---|---|---|---|---:|---|\n");
    let actionable = result
        .files
        .iter()
        .filter(|file| file.source_type != IntakeSourceType::Unsupported)
        .collect::<Vec<_>>();
    if actionable.is_empty() {
        out.push_str("| none | Unsupported | Unsupported | unknown | unknown | 0 | No supported close-material candidates were detected. |\n");
    }
    for file in actionable {
        out.push_str(&format!(
            "| {} | {:?} | {:?} | {} | {} | {} | {} |\n",
            file.file_name,
            file.source_type,
            file.status,
            file.likely_property.as_deref().unwrap_or("unknown"),
            file.likely_period.as_deref().unwrap_or("unknown"),
            file.row_count,
            file.import_recommendation
        ));
    }

    out.push_str("\n## Unsupported Inventory\n\n");
    let unsupported = result
        .files
        .iter()
        .filter(|file| file.source_type == IntakeSourceType::Unsupported)
        .collect::<Vec<_>>();
    out.push_str(&format!(
        "{} unsupported files were detected. The JSON output retains the full list; the markdown report shows the first 50 to keep operator review focused.\n\n",
        unsupported.len()
    ));
    if !unsupported.is_empty() {
        out.push_str("| File | Extension | Warning |\n");
        out.push_str("|---|---|---|\n");
        for file in unsupported.iter().take(50) {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                file.file_name,
                file.extension,
                file.warnings.first().map(String::as_str).unwrap_or("none")
            ));
        }
    }
    if unsupported.len() > 50 {
        out.push_str(&format!(
            "\n{} additional unsupported files omitted from markdown.\n",
            unsupported.len() - 50
        ));
    }

    out.push_str("\n## Close Package Checklist\n\n");
    out.push_str("- [ ] Confirm June actual GL source.\n");
    out.push_str("- [ ] Confirm June budget GL source.\n");
    out.push_str("- [ ] Confirm rent roll as-of date.\n");
    out.push_str("- [ ] Confirm delinquency/aged receivables as-of date.\n");
    out.push_str("- [ ] Confirm leasing funnel week/date range.\n");
    out.push_str("- [ ] Confirm collections context and PII handling.\n");
    out.push_str("- [ ] Confirm BDDRE/RPCOE weekly context is supporting evidence only.\n");
    out.push_str("- [ ] Review account mappings before owner-ready reporting.\n");

    out.push_str("\n## Warnings\n\n");
    let mut wrote_warning = false;
    for file in &result.files {
        for warning in &file.warnings {
            wrote_warning = true;
            out.push_str(&format!("- {}: {}\n", file.file_name, warning));
        }
    }
    if !wrote_warning {
        out.push_str("- No intake warnings were detected.\n");
    }

    out.push_str("\n## Guardrail\n\n");
    out.push_str("The intake workbench proposes what to review next. It does not import data, delete files, transmit private data, or mark a property owner-ready.\n");
    out
}
