use std::path::{Path, PathBuf};

use boxscore::{
    connectors::standardized::{
        collections_unified::ingest_collections_file,
        gl_budget_comparison::ingest_budget_comparison_file,
        operating_snapshots::ingest_operating_snapshots_files,
        source_registry::{lane_by_key, PropertyLane},
    },
    db,
    variance::{self, VarianceRequest},
};
use sqlx::{Column, Row, SqlitePool};

#[tokio::test]
async fn standardized_fixtures_ingest_and_run_variance_analysis() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let lanes = fixture_lanes();

    let mut gl_summaries = Vec::new();
    for (lane, fixture_root) in &lanes {
        gl_summaries.push(ingest_fixture_lane(&pool, lane, fixture_root).await);
    }
    let maplewood_gl_summary = &gl_summaries[0];
    assert_eq!(maplewood_gl_summary.rows_skipped, 1);

    let report_dir = tempfile::tempdir().unwrap();
    let result = variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Maplewood Commons".to_string(),
            period: "2026-06".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    assert!(result.report_path.is_some());
    assert!(result.confidence_score >= 0.65);
    assert!(result.source_coverage.has_actuals);
    assert!(result.source_coverage.has_budgets);
    assert!(result.source_coverage.has_rent_roll);
    assert!(result.source_coverage.has_delinquency);
    assert!(result.source_coverage.has_leasing);
    assert!(result.source_coverage.collections_context);
    assert!(result
        .operating_metrics
        .collections_total_delinquent
        .is_some());
    assert!(result
        .evidence
        .iter()
        .any(|item| item.contains("evidence:")));
    assert!(db::list_gaps(&pool).await.unwrap().len() >= 3);
    assert!(!db::list_questions(&pool).await.unwrap().is_empty());
    assert!(std::fs::read_to_string(result.report_path.unwrap())
        .unwrap()
        .contains("## Collections And Bad Debt Bridge"));
}

#[tokio::test]
async fn standardized_fixtures_do_not_persist_resident_names_in_snapshots_or_evidence() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    for (lane, fixture_root) in fixture_lanes() {
        ingest_fixture_lane(&pool, &lane, &fixture_root).await;
    }
    let report_dir = tempfile::tempdir().unwrap();
    variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Maplewood Commons".to_string(),
            period: "2026-06".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    let persisted_text = format!(
        "{}\n{}\n{}\n{}",
        query_rows_as_text(&pool, "rent_roll_snapshots").await,
        query_rows_as_text(&pool, "delinquency_snapshots").await,
        query_rows_as_text(&pool, "leasing_snapshots").await,
        query_rows_as_text(&pool, "evidence_items").await
    );

    for pii_token in [
        "Resident A",
        "Resident B",
        "Resident C",
        "Resident D",
        "Resident E",
        "R-A",
        "R-B",
        "R-C",
        "R-D",
        "R-E",
    ] {
        assert!(
            !persisted_text.contains(pii_token),
            "persisted text leaked {pii_token}"
        );
    }
}

fn fixture_lanes() -> Vec<(PropertyLane, PathBuf)> {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("standardized");
    vec![
        (
            lane_by_key("maplewood").unwrap(),
            fixture_root.join("maplewood"),
        ),
        (
            lane_by_key("juniper_fund").unwrap(),
            fixture_root.join("juniper_fund"),
        ),
        (
            lane_by_key("willow-brook").unwrap(),
            fixture_root.join("willow_brook"),
        ),
    ]
}

async fn ingest_fixture_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
    fixture_root: &Path,
) -> boxscore::connectors::standardized::StandardizedIngestSummary {
    let gl_summary =
        ingest_budget_comparison_file(pool, lane, &fixture_root.join("budget_comparison.csv"))
            .await
            .unwrap();
    ingest_operating_snapshots_files(
        pool,
        lane,
        &fixture_root.join("rent_roll.csv"),
        &fixture_root.join("aged_receivables.csv"),
        &fixture_root.join("leasing_funnel.csv"),
        "2026-06-09",
    )
    .await
    .unwrap();
    ingest_collections_file(
        pool,
        lane,
        &fixture_root.join("collections_unified.csv"),
        "2026-06-09",
    )
    .await
    .unwrap();
    gl_summary
}

async fn query_rows_as_text(pool: &SqlitePool, table: &str) -> String {
    let rows = sqlx::query(&format!("SELECT * FROM {table}"))
        .fetch_all(pool)
        .await
        .unwrap();
    rows.into_iter()
        .map(|row| {
            row.columns()
                .iter()
                .map(|column| {
                    let name = column.name();
                    row.try_get::<String, _>(name)
                        .or_else(|_| row.try_get::<i64, _>(name).map(|value| value.to_string()))
                        .or_else(|_| row.try_get::<f64, _>(name).map(|value| value.to_string()))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
