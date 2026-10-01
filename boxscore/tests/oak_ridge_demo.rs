//! Repeatable proof of `scripts/demo.sh` for Oak Ridge, period 2026-05.
//!
//! The demo ingests the synthetic sample actuals, budget, and rent roll, then
//! prints an NOI variance analysis. This test runs that same ingest and
//! analysis. The variance assertion is actual NOI minus budget NOI for that
//! period. Occupancy is the occupied, vacant, and down counts the analysis
//! already prints.

use std::path::PathBuf;

use boxscore::{
    db,
    ingest::{self, IngestKind},
    variance::{self, VarianceRequest},
};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data/sample")
        .join(name)
}

#[tokio::test]
async fn oak_ridge_may_2026_variance_is_actual_noi_minus_budget_noi() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    for (kind, file) in [
        (IngestKind::Property, "properties.csv"),
        (IngestKind::GlActuals, "gl_actuals.csv"),
        (IngestKind::GlBudgets, "gl_budgets.csv"),
        (IngestKind::RentRoll, "rent_roll_snapshots.csv"),
        (IngestKind::Delinquency, "delinquency_snapshots.csv"),
        (IngestKind::Leasing, "leasing_snapshots.csv"),
    ] {
        ingest::ingest_file(&pool, kind, &sample(file))
            .await
            .unwrap();
    }

    let report_dir = tempfile::tempdir().unwrap();
    let result = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Oak Ridge".to_string(),
            period: "2026-05".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    // Figures `analyze variance` prints for this demo period. Variance is
    // actual NOI minus budget NOI; the formula itself is unchanged.
    let actual_noi = 46_750.0_f64;
    let budget_noi = 88_600.0_f64;
    let bridge = &result.noi_bridge;
    assert_eq!(bridge.actual_revenue, 192_950.0);
    assert_eq!(bridge.actual_expenses, 146_200.0);
    assert_eq!(bridge.budget_revenue, 220_900.0);
    assert_eq!(bridge.budget_expenses, 132_300.0);
    assert_eq!(bridge.actual_noi, actual_noi);
    assert_eq!(bridge.budget_noi, budget_noi);
    assert_eq!(bridge.noi_variance, actual_noi - budget_noi);
    assert!(!result
        .gaps
        .iter()
        .any(|gap| gap.gap_type == "expense_sign_anomaly"));

    let metrics = &result.operating_metrics;
    assert_eq!(metrics.occupied_units, Some(153));
    assert_eq!(metrics.vacant_units, Some(15));
    assert_eq!(metrics.down_units, Some(4));

    let report_path = result.report_path.expect("demo writes a variance report");
    let report = std::fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("| NOI | $46750 | $88600 | $-41850 |"));
    assert!(report
        .contains("Rent roll showed 153 occupied, 15 vacant, and 4 down units as of 2026-05-31."));
}

#[tokio::test]
async fn legacy_negative_expense_totals_refuse_to_issue_a_variance_report() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let property_id =
        db::upsert_property(&pool, "Synthetic Legacy Sign", "Demo", 0, "Demo", "Demo")
            .await
            .unwrap();
    let period_id = db::upsert_period(&pool, "2026-05").await.unwrap();
    for (category, amount) in [("Rental Income", 100.0), ("Payroll", -30.0)] {
        sqlx::query(
            "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'legacy-demo', 1, ?)",
        )
        .bind(db::new_id())
        .bind(&property_id)
        .bind(&period_id)
        .bind(category)
        .bind(category)
        .bind(category)
        .bind(amount)
        .bind(db::now_iso())
        .execute(&pool)
        .await
        .unwrap();
    }
    let reports = tempfile::tempdir().unwrap();
    let err = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Synthetic Legacy Sign".to_string(),
            period: "2026-05".to_string(),
        },
        reports.path(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("negative expense total"));
    assert_eq!(std::fs::read_dir(reports.path()).unwrap().count(), 0);
    let gap_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM gaps WHERE gap_type = 'expense_sign_anomaly'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(gap_count.0, 1);
}
