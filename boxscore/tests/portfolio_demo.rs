use std::path::PathBuf;

use boxscore::{
    connectors::standardized::source_registry::{lane_by_key, PropertyLane},
    db, portfolio_demo,
};

#[tokio::test]
async fn portfolio_demo_generates_index_and_property_reports_from_sanitized_fixtures() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let report_dir = tempfile::tempdir().unwrap();

    let result =
        portfolio_demo::run_portfolio_demo_for_lanes(&pool, report_dir.path(), fixture_lanes())
            .await
            .unwrap();

    assert_eq!(result.properties.len(), 3);
    assert_eq!(result.gl_ingests.len(), 3);
    assert_eq!(result.ops_ingests.len(), 3);
    assert_eq!(result.collections_ingests.len(), 3);
    assert!(result.unmapped_accounts >= 1);
    assert!(result.open_questions >= 1);
    assert!(result.index_path.exists());

    let index = std::fs::read_to_string(&result.index_path).unwrap();
    assert!(index.contains("# Boxscore Portfolio Demo Index"));
    assert!(index.contains("Maplewood Commons"));
    assert!(index.contains("juniper_fund"));
    assert!(index.contains("Willow Brook"));
    assert!(index.contains("## Period Alignment Note"));
    assert!(index.contains("## Account Mapping Loop"));
    assert!(index.contains("## Asset Manager Review Queue"));

    for property in &result.properties {
        assert!(property.report_path.exists());
        let report = std::fs::read_to_string(&property.report_path).unwrap();
        assert!(report.contains("## Source Coverage"));
        assert!(report.contains("## NOI Driver Confidence"));
    }
}

/// Demo smoke test: lock the "every screen has data" guarantee in CI. Seeds the
/// unit-level tables (unit_receivables, unit_leases) + a track-record memory and
/// a call — mirroring what `demo/seed.sh` writes — then builds a DeskApp, runs a
/// full reload, and asserts the seeded tables are queryable (non-empty). F2.8 /
/// F3.8 extend this to assert the screen-specific `app.*` fields once they exist.
#[tokio::test]
async fn demo_smoke_unit_level_tables_are_seeded() {
    use boxscore::tui::app::DeskApp;

    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    // A property to anchor the synthetic rows (mirrors the demo Bagholder set).
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
         VALUES ('demo1', 'Vantage at Yieldmore', 'Phoenix, AZ', 312, 'Bagholder Capital Partners, LP', 'Apex Residential', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let period = "2026-05";
    let as_of = "2026-05-31";

    // ≥2 unit_receivables (one delinquent, one prepaid).
    db::insert_unit_receivable(
        &pool,
        "demo1",
        as_of,
        "t111111",
        Some("Avery Brooks"),
        Some("current"),
        2540.58,
        Some(2540.58),
        Some(95),
        "demo/unit_receivables.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "demo1",
        as_of,
        "t222222",
        Some("Bianca Chen"),
        Some("prepaid"),
        0.0,
        Some(-480.0),
        None,
        "demo/unit_receivables.csv",
        3,
    )
    .await
    .unwrap();

    // ≥2 unit_leases (one underpriced, one near market).
    db::insert_unit_lease(
        &pool,
        "demo1",
        as_of,
        "01-100",
        Some("t333333"),
        Some("Carlos Delgado"),
        Some(1600.0),
        Some(1364.0),
        "demo/unit_leases.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_lease(
        &pool,
        "demo1",
        as_of,
        "01-101",
        Some("t444444"),
        Some("Diana Emerson"),
        Some(1500.0),
        Some(1490.0),
        "demo/unit_leases.csv",
        3,
    )
    .await
    .unwrap();

    // ≥1 track-record memory + ≥1 call.
    db::upsert_memory(
        &pool,
        "track_record",
        "global",
        "overall",
        "Overall scored-call hit rate: 27/36 (75%).",
        0.80,
        None,
    )
    .await
    .unwrap();
    db::insert_call(
        &pool,
        "demo1",
        period,
        "noi_diagnosis",
        "2026-05-28T23:59:59Z",
        Some(0.6),
        "{\"category\":\"Insurance\"}",
        None,
    )
    .await
    .unwrap();

    // F4: snapshots + GL so the Close Desk portfolio band has real aggregates.
    let period_id = db::upsert_period(&pool, period).await.unwrap();
    sqlx::query(
        "INSERT INTO rent_roll_snapshots (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, notice_units, down_units, market_rent_total, in_place_rent_total, source_file, source_row, created_at) \
         VALUES (?, 'demo1', ?, 295, 17, 0, 0, 0, 480000.0, 452000.0, 'demo/rent_roll.csv', 1, '2026-01-01')",
    )
    .bind(db::new_id())
    .bind(as_of)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at) \
         VALUES (?, 'demo1', ?, 41250.0, 9, 0.0, 'demo/delinquency.csv', 1, '2026-01-01')",
    )
    .bind(db::new_id())
    .bind(as_of)
    .execute(&pool)
    .await
    .unwrap();
    for (table, rev, exp) in [
        ("gl_actuals", 470000.0, 205000.0),
        ("gl_budgets", 462000.0, 198000.0),
    ] {
        let sql = format!(
            "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
             VALUES (?, 'demo1', ?, '4000', 'Rental Income', 'rental income', ?, 'demo/gl.csv', 1, '2026-01-01'), \
                    (?, 'demo1', ?, '5200', 'Payroll', 'payroll', ?, 'demo/gl.csv', 2, '2026-01-01')"
        );
        sqlx::query(&sql)
            .bind(db::new_id())
            .bind(&period_id)
            .bind(rev)
            .bind(db::new_id())
            .bind(&period_id)
            .bind(exp)
            .execute(&pool)
            .await
            .unwrap();
    }

    // A full reload must not panic with the seeded data present.
    let mut app = DeskApp::new(period.to_string());
    app.reload(&pool).await;
    assert!(
        app.load_error.is_none(),
        "reload error: {:?}",
        app.load_error
    );

    // F4.4: the portfolio band rolls up the demo book — counts, occupancy,
    // in-place rent, NOI variance, and delinquency are all populated.
    assert_eq!(app.portfolio.property_count, 1, "rollup property_count");
    assert_eq!(app.portfolio.total_units, 312, "rollup total_units");
    assert!(
        app.portfolio.occ_properties >= 1,
        "rollup occupancy present"
    );
    assert!(
        app.portfolio.occ_high > 0.0 && app.portfolio.occ_high <= 1.0,
        "rollup occupancy in range, got {}",
        app.portfolio.occ_high
    );
    assert!(
        app.portfolio.inplace_rent_total > 0.0,
        "rollup in-place rent should be > 0"
    );
    assert!(
        app.portfolio.delinquent_total > 0.0,
        "rollup delinquency should be > 0"
    );
    // Actual NOI = 470k-205k = 265k; budget = 462k-198k = 264k → favorable +1k.
    assert!(
        (app.portfolio.noi_actual - 265_000.0).abs() < 1.0,
        "rollup NOI actual, got {}",
        app.portfolio.noi_actual
    );
    assert!(
        app.portfolio.noi_variance() > 0.0,
        "rollup NOI variance should be favorable, got {}",
        app.portfolio.noi_variance()
    );

    // The seeded unit-level tables are queryable (non-empty).
    assert!(
        db::receivables_snapshot_exists(&pool, "demo1", period)
            .await
            .unwrap(),
        "unit_receivables should be populated for the demo period"
    );
    assert!(
        db::leases_snapshot_exists(&pool, "demo1", period)
            .await
            .unwrap(),
        "unit_leases should be populated for the demo period"
    );
    assert!(
        !db::list_calls(&pool).await.unwrap().is_empty(),
        "calls should be populated"
    );
    assert!(
        !db::recent_track_record_memories(&pool, 10)
            .await
            .unwrap()
            .is_empty(),
        "track_record memories should be populated"
    );

    // F2.8: the Delinquency screen's fields are populated after the full reload.
    // The seeded delinquent resident has days_late = 95 (90+ aged bucket); the
    // prepaid resident feeds the prepaid cells.
    assert!(
        !app.delin_residents.is_empty(),
        "delinquency residents should be populated after reload"
    );
    assert!(
        app.delin_aging.b90_plus > 0.0,
        "delinquency aging should have a non-zero aged (90+) bucket, got {:?}",
        app.delin_aging
    );
    assert!(
        app.delin_aging.prepaid_cnt > 0,
        "delinquency aging should record the seeded prepaid resident"
    );

    // F3.6: the Renewals screen's fields are populated after the full reload.
    // The seeded leases include one underpriced unit (1600 market / 1364 charge
    // = 0.85× → underpriced), so the renewal opportunity must be positive.
    assert!(
        !app.renew_leases.is_empty(),
        "renewal leases should be populated after reload"
    );
    assert!(
        app.renew_opportunity.0 > 0.0,
        "renewal monthly opportunity should be > 0, got {:?}",
        app.renew_opportunity
    );
    assert!(
        app.renew_opportunity.2 >= 1,
        "renewal underpriced count should be >= 1, got {:?}",
        app.renew_opportunity
    );

    // F5.6: the NOI Bridge is built after the full reload, with drivers in BOTH
    // directions. The demo GL has Rental Income 470k vs 462k budget (+8k
    // favorable) and Payroll 205k actual vs 198k budget (over budget → -7k
    // unfavorable), so the bridge must carry a favorable and an unfavorable step.
    assert!(
        !app.bridge_steps.is_empty(),
        "bridge_steps should be populated after reload"
    );
    // Anchors book-end the waterfall.
    assert!(
        app.bridge_steps.first().map(|s| s.is_anchor) == Some(true),
        "first bridge step must be the Budget NOI anchor"
    );
    assert!(
        app.bridge_steps.last().map(|s| s.is_anchor) == Some(true),
        "last bridge step must be the Actual NOI anchor"
    );
    // At least one favorable (+) and one unfavorable (−) driver — both directions.
    let drivers: Vec<f64> = app
        .bridge_steps
        .iter()
        .filter(|s| !s.is_anchor)
        .map(|s| s.delta)
        .collect();
    assert!(
        drivers.iter().any(|&d| d > 0.0),
        "bridge should have a favorable driver, got {drivers:?}"
    );
    assert!(
        drivers.iter().any(|&d| d < 0.0),
        "bridge should have an unfavorable driver, got {drivers:?}"
    );
    // Budget NOI = 462k-198k = 264k; Actual = 470k-205k = 265k → reconciles +1k.
    assert!(
        (app.bridge_budget_noi - 264_000.0).abs() < 1.0,
        "bridge budget NOI, got {}",
        app.bridge_budget_noi
    );
    assert!(
        (app.bridge_actual_noi - 265_000.0).abs() < 1.0,
        "bridge actual NOI, got {}",
        app.bridge_actual_noi
    );
    // The running total reconciles: Budget NOI + Σ driver deltas = Actual NOI.
    let driver_sum: f64 = drivers.iter().sum();
    assert!(
        (app.bridge_budget_noi + driver_sum - app.bridge_actual_noi).abs() < 1.0,
        "bridge must reconcile: {} + {} != {}",
        app.bridge_budget_noi,
        driver_sum,
        app.bridge_actual_noi
    );
}

fn fixture_lanes() -> Vec<PropertyLane> {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("standardized");
    vec![
        lane_with_fixture_path(
            lane_by_key("maplewood").unwrap(),
            fixture_root.join("maplewood"),
        ),
        lane_with_fixture_path(
            lane_by_key("juniper_fund").unwrap(),
            fixture_root.join("juniper_fund"),
        ),
        lane_with_fixture_path(
            lane_by_key("willow-brook").unwrap(),
            fixture_root.join("willow_brook"),
        ),
    ]
}

fn lane_with_fixture_path(mut lane: PropertyLane, path: PathBuf) -> PropertyLane {
    lane.standardized_path = path.clone();
    lane.raw_data_path = path;
    lane
}
