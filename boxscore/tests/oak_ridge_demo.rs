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
    let actual_noi = 339_150.0_f64;
    let budget_noi = 353_200.0_f64;
    let bridge = &result.noi_bridge;
    assert_eq!(bridge.actual_noi, actual_noi);
    assert_eq!(bridge.budget_noi, budget_noi);
    assert_eq!(bridge.noi_variance, actual_noi - budget_noi);

    let metrics = &result.operating_metrics;
    assert_eq!(metrics.occupied_units, Some(153));
    assert_eq!(metrics.vacant_units, Some(15));
    assert_eq!(metrics.down_units, Some(4));

    let report_path = result.report_path.expect("demo writes a variance report");
    let report = std::fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("| NOI | $339150 | $353200 | $-14050 |"));
    assert!(report
        .contains("Rent roll showed 153 occupied, 15 vacant, and 4 down units as of 2026-05-31."));
}
