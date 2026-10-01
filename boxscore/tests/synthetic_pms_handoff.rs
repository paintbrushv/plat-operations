use std::{
    fs,
    path::{Path, PathBuf},
};

use boxscore::{
    db,
    ingest::{self, IngestKind},
    synthetic_pms::{self, HandoffBoundary},
    variance::{self, VarianceRequest},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic_pms")
        .join(name)
}

async fn setup() -> sqlx::SqlitePool {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn sealed_september_close_keeps_issued_noi_after_late_outgoing_correction() {
    let pool = setup().await;
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    let ccar = HandoffBoundary::from_path(&fixture("ccar_boundary.json")).unwrap();
    let tc_before =
        synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
            .await
            .unwrap();
    let tc_after =
        synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
            .await
            .unwrap();
    let ccar_before =
        synthetic_pms::import_file(&pool, &ccar, "before", &fixture("ccar_outgoing_yardi.csv"))
            .await
            .unwrap();
    let ccar_after =
        synthetic_pms::import_file(&pool, &ccar, "after", &fixture("ccar_incoming_yardi.csv"))
            .await
            .unwrap();
    assert_eq!(
        (tc_before.revisions_accepted, tc_after.revisions_accepted),
        (3, 3)
    );
    assert_eq!(
        (
            ccar_before.revisions_accepted,
            ccar_after.revisions_accepted
        ),
        (2, 2)
    );
    assert_eq!(tc_before.profile_version, synthetic_pms::YARDI_PROFILE);
    assert_eq!(tc_after.profile_version, synthetic_pms::RESMAN_PROFILE);
    assert_eq!(ccar_after.profile_version, synthetic_pms::YARDI_PROFILE);
    assert_ne!(ccar_before.source_namespace, ccar_after.source_namespace);

    ingest::ingest_file(
        &pool,
        IngestKind::GlBudgets,
        &fixture("september_budgets.csv"),
    )
    .await
    .unwrap();
    let reports = tempfile::tempdir().unwrap();
    let issued_variance = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: tc.property_name.clone(),
            period: tc.period.clone(),
        },
        reports.path(),
    )
    .await
    .unwrap();
    assert_eq!(issued_variance.noi_bridge.actual_revenue, 80_000.0);
    assert_eq!(issued_variance.noi_bridge.actual_expenses, 25_000.0);
    assert_eq!(issued_variance.noi_bridge.actual_noi, 55_000.0);
    assert_eq!(issued_variance.noi_bridge.budget_noi, 57_000.0);
    let issued_path = issued_variance.report_path.as_ref().unwrap();
    let issued_markdown = fs::read_to_string(issued_path).unwrap();
    let tc_id = db::require_property_by_name(&pool, &tc.property_name)
        .await
        .unwrap()
        .id;
    let tc_period_id = db::period_by_label(&pool, &tc.period)
        .await
        .unwrap()
        .unwrap()
        .id;
    assert!(boxscore::reports::issue_variance_report(
        &pool,
        &tc_id,
        &tc_period_id,
        reports.path(),
        &issued_variance,
    )
    .await
    .is_err());
    assert_eq!(fs::read_to_string(issued_path).unwrap(), issued_markdown);
    let ccar_variance = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: ccar.property_name.clone(),
            period: ccar.period.clone(),
        },
        reports.path(),
    )
    .await
    .unwrap();
    let tc_close = synthetic_pms::seal_synthetic_close(&pool, &tc, &issued_variance.task_run_id)
        .await
        .unwrap();
    let ccar_close = synthetic_pms::seal_synthetic_close(&pool, &ccar, &ccar_variance.task_run_id)
        .await
        .unwrap();
    assert_eq!(
        (
            tc_close.issued_actual_noi_cents,
            tc_close.issued_budget_noi_cents
        ),
        (5_500_000, 5_700_000)
    );
    assert_eq!(
        (
            ccar_close.issued_actual_noi_cents,
            ccar_close.issued_budget_noi_cents
        ),
        (4_400_000, 4_600_000)
    );
    assert_eq!(tc_close.accepted_revision_count_at_issue, 6);
    assert_eq!(
        tc_close.issued_report_task_run_id.as_deref(),
        Some(issued_variance.task_run_id.as_str())
    );
    assert!(tc_close.synthetic_only);
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, &issued_variance.task_run_id)
            .await
            .is_err()
    );

    let repeat =
        synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
            .await
            .unwrap();
    assert_eq!(
        (
            repeat.revisions_accepted,
            repeat.exact_retries,
            repeat.net_noi_change_cents
        ),
        (0, 3, 0)
    );
    let correction =
        synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_correction.csv"))
            .await
            .unwrap();
    assert_eq!(
        (
            correction.revisions_accepted,
            correction.net_noi_change_cents
        ),
        (1, -50_000)
    );
    let restated = synthetic_pms::synthetic_close(&pool, &tc.property_name, &tc.period)
        .await
        .unwrap();
    assert_eq!(restated.id, tc_close.id);
    assert_eq!(restated.issued_actual_noi_cents, 5_500_000);
    assert_eq!(restated.current_actual_noi_cents, 5_450_000);
    assert_eq!(restated.current_budget_noi_cents, 5_700_000);
    assert_eq!(restated.accepted_revision_count_at_issue, 6);
    let restated_variance = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: tc.property_name.clone(),
            period: tc.period.clone(),
        },
        reports.path(),
    )
    .await
    .unwrap();
    assert_eq!(restated_variance.noi_bridge.actual_expenses, 25_500.0);
    assert_eq!(restated_variance.noi_bridge.actual_noi, 54_500.0);
    assert_eq!(restated_variance.noi_bridge.budget_noi, 57_000.0);
    assert_ne!(issued_variance.report_path, restated_variance.report_path);
    assert_eq!(fs::read_to_string(issued_path).unwrap(), issued_markdown);
    let stored_report: (String,) = sqlx::query_as(
        "SELECT report_markdown FROM variance_report_artifacts WHERE task_run_id = ?",
    )
    .bind(&issued_variance.task_run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_report.0, issued_markdown);
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, &restated_variance.task_run_id)
            .await
            .is_err()
    );
    let other = synthetic_pms::synthetic_close(&pool, &ccar.property_name, &ccar.period)
        .await
        .unwrap();
    assert_eq!(
        other.issued_actual_noi_cents,
        other.current_actual_noi_cents
    );

    let equal_receipts: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT source_namespace) FROM pms_source_revisions \
         WHERE property_id = ? AND source_record_id = 'TX-003' AND amount_cents = 500000",
    )
    .bind(&tc_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(equal_receipts, (2, 2));
    let correction_rows: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pms_source_revisions WHERE property_id = ? AND source_record_id = 'TX-002' AND revision = 2",
    ).bind(&tc_id).fetch_one(&pool).await.unwrap();
    assert_eq!(correction_rows.0, 1);
    let repeat_correction =
        synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_correction.csv"))
            .await
            .unwrap();
    assert_eq!(
        (
            repeat_correction.revisions_accepted,
            repeat_correction.exact_retries
        ),
        (0, 1)
    );
    let old_replay =
        synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
            .await
            .unwrap();
    assert_eq!(
        (old_replay.revisions_accepted, old_replay.exact_retries),
        (0, 3)
    );
    assert_eq!(
        synthetic_pms::synthetic_close(&pool, &tc.property_name, &tc.period)
            .await
            .unwrap()
            .current_actual_noi_cents,
        5_450_000
    );
}

#[tokio::test]
async fn pinned_layout_dates_accounts_and_revisions_fail_closed_without_partial_import() {
    let pool = setup().await;
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.csv");
    let original = fs::read_to_string(fixture("tc_outgoing_yardi.csv")).unwrap();

    fs::write(&path, original.replace("posting_date", "unknown_date")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .unwrap_err()
        .to_string()
        .contains("headers"));
    fs::write(&path, original.replace("SYN-TC", "SYN-OTHER")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .is_err());
    fs::write(&path, original.replace("5200,Repairs", "9999,Repairs")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .is_err());
    fs::write(&path, original.replace("2026-09-22", "2026-09-23")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .is_err());
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pms_source_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
        .await
        .unwrap();
    let mut changed_boundary = tc.clone();
    changed_boundary
        .before
        .account_categories
        .insert("5200".into(), "Administrative".into());
    assert!(synthetic_pms::import_file(
        &pool,
        &changed_boundary,
        "before",
        &fixture("tc_outgoing_yardi.csv"),
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("pinned"));
    let correction = fs::read_to_string(fixture("tc_outgoing_correction.csv")).unwrap();
    fs::write(&path, correction.replace(",2,5200,", ",3,5200,")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .is_err());
    fs::write(&path, original.replace("18000.00", "18001.00")).unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .unwrap_err()
        .to_string()
        .contains("different content"));
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pms_source_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 3);

    // A good correction followed by an invalid next revision rolls the file back.
    fs::write(
        &path,
        format!(
            "{correction}{}",
            correction
                .lines()
                .nth(1)
                .unwrap()
                .replace(",2,5200,", ",4,5200,")
                + "\n"
        ),
    )
    .unwrap();
    assert!(synthetic_pms::import_file(&pool, &tc, "before", &path)
        .await
        .is_err());
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pms_source_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 3);
}

#[tokio::test]
async fn synthetic_close_refuses_legacy_negative_expense_budget_sign() {
    let pool = setup().await;
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
        .await
        .unwrap();
    synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
        .await
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let budget = temp.path().join("wrong-sign-budgets.csv");
    let contents = fs::read_to_string(fixture("september_budgets.csv")).unwrap();
    fs::write(&budget, contents.replace(",23000.00", ",-23000.00")).unwrap();
    ingest::ingest_file(&pool, IngestKind::GlBudgets, &budget)
        .await
        .unwrap();
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, "no-valid-report")
            .await
            .unwrap_err()
            .to_string()
            .contains("negative expense")
    );
}

#[tokio::test]
async fn synthetic_close_refuses_unattributed_gl_actuals() {
    let pool = setup().await;
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
        .await
        .unwrap();
    synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
        .await
        .unwrap();
    ingest::ingest_file(
        &pool,
        IngestKind::GlBudgets,
        &fixture("september_budgets.csv"),
    )
    .await
    .unwrap();
    let property_id = db::require_property_by_name(&pool, &tc.property_name)
        .await
        .unwrap()
        .id;
    let period_id = db::period_by_label(&pool, &tc.period)
        .await
        .unwrap()
        .unwrap()
        .id;
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
         VALUES (?, ?, ?, '9999', 'Unattributed', 'Other Income', 1.0, 'outside', 1, ?)",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(period_id)
    .bind(db::now_iso())
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, "missing-report")
            .await
            .unwrap_err()
            .to_string()
            .contains("outside accepted PMS revisions")
    );
}

#[tokio::test]
async fn seal_requires_current_and_untampered_issued_report() {
    let pool = setup().await;
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
        .await
        .unwrap();
    synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
        .await
        .unwrap();
    ingest::ingest_file(
        &pool,
        IngestKind::GlBudgets,
        &fixture("september_budgets.csv"),
    )
    .await
    .unwrap();
    assert!(synthetic_pms::seal_synthetic_close(&pool, &tc, "missing")
        .await
        .unwrap_err()
        .to_string()
        .contains("completed variance report"));
    let reports = tempfile::tempdir().unwrap();
    let analysis = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: tc.property_name.clone(),
            period: tc.period.clone(),
        },
        reports.path(),
    )
    .await
    .unwrap();
    let report_path = analysis.report_path.as_ref().unwrap();
    let issued_markdown = fs::read_to_string(report_path).unwrap();
    fs::write(report_path, "tampered").unwrap();
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, &analysis.task_run_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("differs from stored report")
    );
    fs::write(report_path, issued_markdown).unwrap();
    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_correction.csv"))
        .await
        .unwrap();
    assert!(
        synthetic_pms::seal_synthetic_close(&pool, &tc, &analysis.task_run_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not match current synthetic close totals")
    );
}

#[tokio::test]
async fn file_backed_demo_shape_seals_without_sqlite_lock() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("demo-copy.sqlite");
    let pool = db::connect(&format!("sqlite://{}?mode=rwc", database.display()))
        .await
        .unwrap();
    db::init_database(&pool).await.unwrap();
    let tc = HandoffBoundary::from_path(&fixture("tc_boundary.json")).unwrap();
    synthetic_pms::import_file(&pool, &tc, "before", &fixture("tc_outgoing_yardi.csv"))
        .await
        .unwrap();
    synthetic_pms::import_file(&pool, &tc, "after", &fixture("tc_incoming_resman.csv"))
        .await
        .unwrap();
    ingest::ingest_file(
        &pool,
        IngestKind::GlBudgets,
        &fixture("september_budgets.csv"),
    )
    .await
    .unwrap();
    let analysis = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: tc.property_name.clone(),
            period: tc.period.clone(),
        },
        temp.path(),
    )
    .await
    .unwrap();
    let close = synthetic_pms::seal_synthetic_close(&pool, &tc, &analysis.task_run_id)
        .await
        .unwrap();
    assert_eq!(close.issued_actual_noi_cents, 5_500_000);
    pool.close().await;
    let reopened = db::connect(&format!("sqlite://{}?mode=ro", database.display()))
        .await
        .unwrap();
    let reread = synthetic_pms::synthetic_close(&reopened, &tc.property_name, &tc.period)
        .await
        .unwrap();
    assert_eq!(reread.id, close.id);
    reopened.close().await;
}
