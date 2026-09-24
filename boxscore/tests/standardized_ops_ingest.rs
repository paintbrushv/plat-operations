use std::path::Path;

use boxscore::{
    connectors::standardized::{
        aged_receivables::read_delinquency_snapshot, leasing_funnel::read_leasing_snapshot,
        operating_snapshots::ingest_operating_snapshots_files, rent_roll::read_rent_roll_snapshot,
        source_registry::lane_by_key,
    },
    db,
};
use sqlx::Row;

#[test]
fn rent_roll_aggregates_without_retaining_resident_names() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("rent_roll.csv");
    std::fs::write(
        &file,
        "unit,resident,name,market,charge_rent,property_id\nA101,Resident One,Resident One,1200,1100,p102\nA102,,,1300,0,p102\nA103,VACANT,,1250,0,p102\n",
    )
    .unwrap();

    let snapshot = read_rent_roll_snapshot(&file, "2026-06-09").unwrap();

    assert_eq!(snapshot.rows_seen, 3);
    assert_eq!(snapshot.occupied_units, 1);
    assert_eq!(snapshot.vacant_units, 2);
    assert_eq!(snapshot.leased_units, 1);
    assert_eq!(snapshot.market_rent_total, 3750.0);
    assert_eq!(snapshot.in_place_rent_total, 1100.0);
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("Resident One"));
}

#[test]
fn aged_receivables_prefers_total_delinquent_when_available() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("aged_receivables.csv");
    std::fs::write(
        &file,
        "property_id,resident_code,resident_name,current_owed,days_0_30,days_31_60,days_61_90,days_over_90,prepayments,total_owed,snapshot_date,total_delinquent\np101,R1,Resident One,999,10,20,30,40,-50,1049,2026-06-09,123\np101,R2,Resident Two,0,0,0,0,0,25,25,2026-06-09,0\n",
    )
    .unwrap();

    let snapshot = read_delinquency_snapshot(&file).unwrap();

    assert_eq!(snapshot.rows_seen, 2);
    assert_eq!(snapshot.as_of_date, "2026-06-09");
    assert_eq!(snapshot.delinquent_amount, 123.0);
    assert_eq!(snapshot.delinquent_units, 1);
    assert_eq!(snapshot.prepaid_amount, 75.0);
}

#[test]
fn aged_receivables_falls_back_to_aging_buckets() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("aged_receivables.csv");
    std::fs::write(
        &file,
        "property_id,resident_code,resident_name,current_owed,days_0_30,days_31_60,days_61_90,days_over_90,prepayments,total_owed,snapshot_date\njuniper_fund,R1,Resident One,0,10,20,30,40,0,100,2026-06-09\njuniper_fund,R2,Resident Two,0,0,0,0,0,0,0,2026-06-09\n",
    )
    .unwrap();

    let snapshot = read_delinquency_snapshot(&file).unwrap();

    assert_eq!(snapshot.delinquent_amount, 100.0);
    assert_eq!(snapshot.delinquent_units, 1);
}

#[test]
fn leasing_funnel_maps_shows_applications_and_approvals() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("leasing_funnel.csv");
    std::fs::write(
        &file,
        "week_start,week_end,shows,applications,approvals,property_id\n2026-06-01,2026-06-07,4,2,1,p102\n2026-06-08,2026-06-14,5,3,2,p102\n",
    )
    .unwrap();

    let snapshot = read_leasing_snapshot(&file).unwrap();

    assert_eq!(snapshot.rows_seen, 2);
    assert_eq!(snapshot.as_of_date, "2026-06-14");
    assert_eq!(snapshot.tours, 9);
    assert_eq!(snapshot.applications, 5);
    assert_eq!(snapshot.approvals, 3);
    assert_eq!(snapshot.leads, 0);
    assert_eq!(snapshot.move_ins, 0);
    assert_eq!(snapshot.move_outs, 0);
    assert_eq!(
        snapshot.missing_fields,
        vec![
            "leads".to_string(),
            "move_ins".to_string(),
            "move_outs".to_string()
        ]
    );
}

#[test]
fn leasing_funnel_prefers_week_end_when_event_date_is_blank() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("leasing_funnel.csv");
    std::fs::write(
        &file,
        "prospect_id,event_date,week_start,week_end,shows,applications,approvals,property_id\nP1,,2026-06-01,2026-06-07,4,2,1,p101\n",
    )
    .unwrap();

    let snapshot = read_leasing_snapshot(&file).unwrap();

    assert_eq!(snapshot.as_of_date, "2026-06-07");
    assert_eq!(snapshot.tours, 4);
}

#[tokio::test]
async fn standardized_ops_ingest_persists_snapshots_and_excludes_pii() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let rent_roll = temp_dir.path().join("rent_roll.csv");
    let delinquency = temp_dir.path().join("aged_receivables.csv");
    let leasing = temp_dir.path().join("leasing_funnel.csv");
    std::fs::write(
        &rent_roll,
        "unit,resident,name,market,charge_rent,property_id\nA101,Resident One,Resident One,1200,1100,p102\nA102,,,1300,0,p102\n",
    )
    .unwrap();
    std::fs::write(
        &delinquency,
        "property_id,resident_code,resident_name,current_owed,days_0_30,days_31_60,days_61_90,days_over_90,prepayments,total_owed,snapshot_date,total_delinquent\np102,R1,Resident One,0,10,20,30,40,-50,100,2026-06-09,100\n",
    )
    .unwrap();
    std::fs::write(
        &leasing,
        "week_start,week_end,shows,applications,approvals,property_id\n2026-06-01,2026-06-07,4,2,1,p102\n",
    )
    .unwrap();

    let lane = lane_by_key("willow-brook").unwrap();
    let summary = ingest_operating_snapshots_files(
        &pool,
        &lane,
        Path::new(&rent_roll),
        Path::new(&delinquency),
        Path::new(&leasing),
        "2026-06-09",
    )
    .await
    .unwrap();

    assert_eq!(summary.rows_inserted, 3);
    assert!(summary.gaps_created >= 3);
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS count FROM rent_roll_snapshots")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>("count"),
        1
    );
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS count FROM delinquency_snapshots")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>("count"),
        1
    );
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS count FROM leasing_snapshots")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>("count"),
        1
    );

    let persisted_text = format!(
        "{}{}{}{}",
        serde_json::to_string(
            &sqlx::query("SELECT * FROM rent_roll_snapshots")
                .fetch_all(&pool)
                .await
                .unwrap()
                .len()
        )
        .unwrap(),
        serde_json::to_string(
            &sqlx::query("SELECT * FROM delinquency_snapshots")
                .fetch_all(&pool)
                .await
                .unwrap()
                .len()
        )
        .unwrap(),
        serde_json::to_string(
            &sqlx::query("SELECT * FROM leasing_snapshots")
                .fetch_all(&pool)
                .await
                .unwrap()
                .len()
        )
        .unwrap(),
        serde_json::to_string(&db::list_gaps(&pool).await.unwrap()).unwrap()
    );
    assert!(!persisted_text.contains("Resident One"));
}
