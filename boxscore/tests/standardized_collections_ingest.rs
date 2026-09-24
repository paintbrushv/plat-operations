use std::path::Path;

use boxscore::{
    connectors::standardized::{
        collections_unified::{ingest_collections_file, read_collections_snapshot},
        source_registry::lane_by_key,
    },
    db,
};
use sqlx::Row;

#[test]
fn collections_parser_aggregates_without_retaining_resident_pii() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("collections_unified.csv");
    std::fs::write(
        &file,
        "property_id,unit,resident_code,resident_name,total_opportunity,delinquency_tier,total_delinquent,days_31_60,days_61_90,days_over_90,exception_count,missed_fee_total,underpricing_gap_monthly,pricing_opportunity,pre_delinquency_score,pre_delinquency_tier,payment_cohort,on_time_pct,days_0_30,intervention_priority,confidence,missed_fee_monthly_runrate,snapshot_date\np101,A101,R-A,Resident A,500,high,150,50,25,0,1,25,100,100,82,watch,late,0.75,75,1,0.90,25,2026-06-09\np101,A102,R-B,Resident B,100,low,0,0,0,0,0,0,0,0,20,stable,on_time,1.00,0,3,0.95,0,2026-06-09\n",
    )
    .unwrap();

    let snapshot = read_collections_snapshot(&file, "2026-06-09").unwrap();

    assert_eq!(snapshot.rows_seen, 2);
    assert_eq!(snapshot.as_of_date, "2026-06-09");
    assert_eq!(snapshot.total_delinquent, 150.0);
    assert_eq!(snapshot.delinquent_units, 1);
    assert_eq!(snapshot.high_risk_units, 1);
    assert_eq!(snapshot.total_opportunity, 600.0);
    assert_eq!(snapshot.pricing_opportunity, 100.0);
    assert_eq!(snapshot.missed_fee_total, 25.0);
    assert_eq!(snapshot.avg_on_time_pct, 0.875);
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("Resident A"));
}

#[tokio::test]
async fn collections_ingest_persists_aggregate_snapshot_and_evidence_without_pii() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/standardized/maplewood/collections_unified.csv");
    let lane = lane_by_key("maplewood").unwrap();

    let summary = ingest_collections_file(&pool, &lane, &fixture, "2026-06-09")
        .await
        .unwrap();

    assert_eq!(summary.rows_seen, 2);
    assert_eq!(summary.rows_inserted, 1);
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS count FROM collection_snapshots")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>("count"),
        1
    );
    let text = format!(
        "{}{}",
        serde_json::to_string(&db::list_gaps(&pool).await.unwrap()).unwrap(),
        sqlx::query("SELECT claim FROM evidence_items")
            .fetch_all(&pool)
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<String, _>("claim"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(!text.contains("Resident A"));
    assert!(!text.contains("R-A"));
}
