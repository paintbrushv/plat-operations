use boxscore::intake::{self, IntakeSourceType, IntakeStatus};

#[test]
fn intake_scan_classifies_close_files_without_importing() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(
        inbox
            .path()
            .join("willow_brook_budget_vs_actual_2026-06.csv"),
        "property,period,account_code,account_name,actual,budget\nWillow Brook,2026-06,4000,Rental Income,100,90\n",
    )
    .unwrap();
    std::fs::write(
        inbox.path().join("RPCOE_Weekly_Report_2026-06-09.md"),
        "# RPCOE\n",
    )
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();

    assert_eq!(result.summary.file_count, 2);
    assert_eq!(result.summary.financial_candidates, 1);
    assert_eq!(result.summary.weekly_context_files, 1);
    let financial = result
        .files
        .iter()
        .find(|file| file.file_name.contains("budget_vs_actual"))
        .unwrap();
    assert_eq!(
        financial.source_type,
        IntakeSourceType::FinancialActualBudgetCandidate
    );
    assert_eq!(financial.status, IntakeStatus::ReadyForReview);
    assert_eq!(financial.likely_property.as_deref(), Some("Willow Brook"));
    assert_eq!(financial.likely_period.as_deref(), Some("2026-06"));
    assert_eq!(financial.row_count, 1);
    assert!(financial.import_recommendation.contains("dry-run"));
    assert!(result.report_path.exists());
}

#[test]
fn intake_scan_flags_period_mismatches_and_unsupported_files() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(
        inbox
            .path()
            .join("juniper_fund_income_statement_2026-05.csv"),
        "property,period,account,actual,budget\njuniper_fund,2026-05,4000,100,90\n",
    )
    .unwrap();
    std::fs::write(inbox.path().join("random_notes.txt"), "hello").unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();

    assert_eq!(result.summary.file_count, 2);
    assert_eq!(result.summary.period_mismatches, 1);
    assert_eq!(result.summary.unsupported_files, 1);
    let stale = result
        .files
        .iter()
        .find(|file| file.file_name.contains("income_statement"))
        .unwrap();
    assert_eq!(stale.status, IntakeStatus::NeedsOperatorReview);
    assert!(stale
        .warnings
        .iter()
        .any(|warning| warning.contains("does not match target period")));
}

#[test]
fn intake_report_contains_close_checklist_and_guardrails() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(
        inbox.path().join("maplewood_rent_roll_2026-06.csv"),
        "property,as_of_date,unit,status\nMaplewood Commons,2026-06-05,101,Occupied\n",
    )
    .unwrap();
    std::fs::write(
        inbox.path().join("BDDRE_Weekly_Report_2026-06-09.md"),
        "# BDDRE\n",
    )
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();
    let report = std::fs::read_to_string(&result.report_path).unwrap();

    assert!(report.contains("# Boxscore 2026-06 Close Intake Workbench"));
    assert!(report.contains("## Dry-Run Import Plan"));
    assert!(report.contains("## Unsupported Inventory"));
    assert!(report.contains("## Close Package Checklist"));
    assert!(report.contains("No analytical tables were modified"));
}

#[test]
fn intake_scan_infers_periods_from_yardi_mm_dd_yyyy_filenames() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(
        inbox.path().join("willow_brook_rent_roll_06_09_2026.csv"),
        "unit,market_rent\n101,1200\n",
    )
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();

    let rent_roll = result
        .files
        .iter()
        .find(|file| file.file_name.contains("rent_roll"))
        .unwrap();
    assert_eq!(rent_roll.likely_period.as_deref(), Some("2026-06"));
}

#[test]
fn intake_scan_skips_tooling_dirs_and_survives_binary_csvs() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(inbox.path().join(".git")).unwrap();
    std::fs::write(inbox.path().join(".git/blob.bin"), [0u8, 159, 146, 150]).unwrap();
    std::fs::create_dir_all(inbox.path().join("target")).unwrap();
    std::fs::write(inbox.path().join("target/junk.csv"), [0u8, 159, 146, 150]).unwrap();
    // A binary file masquerading as a CSV must warn, not abort the scan.
    std::fs::write(inbox.path().join("corrupt.csv"), [0u8, 159, 146, 150]).unwrap();
    std::fs::write(
        inbox.path().join("juniper_fund_rent_roll_2026-06.csv"),
        "unit,market_rent\n101,950\n",
    )
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();

    assert_eq!(result.summary.file_count, 2, "tooling dirs must be skipped");
    let corrupt = result
        .files
        .iter()
        .find(|file| file.file_name == "corrupt.csv")
        .unwrap();
    assert!(corrupt
        .warnings
        .iter()
        .any(|warning| warning.contains("Could not profile CSV contents")));
}

#[test]
fn intake_scan_handles_unicode_without_panicking() {
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(
        inbox.path().join("owner_notes.csv"),
        "note,period\nRevenue bridge — operator review,2026-06\n",
    )
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    let result = intake::scan_inbox(inbox.path(), "2026-06", report_dir.path()).unwrap();

    assert_eq!(result.summary.file_count, 1);
    assert_eq!(result.files[0].likely_period.as_deref(), Some("2026-06"));
}
