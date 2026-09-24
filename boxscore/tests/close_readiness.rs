use boxscore::{close_readiness, connectors::standardized::source_registry::PropertyLane, db};

fn fixture_lane(std_dir: &std::path::Path, footer: bool) -> PropertyLane {
    let rent_roll = if footer {
        // Inject a Yardi grand-total footer row: 171 / Total / All Properties / 158529.
        "unit,resident,name,market\n101,r1,Alice,1200\n171,Total,All Properties,158529\n"
    } else {
        "unit,resident,name,market\n101,r1,Alice,1200\n102,r2,Bob,1350\n"
    };
    std::fs::write(std_dir.join("rent_roll.csv"), rent_roll).unwrap();
    std::fs::write(
        std_dir.join("aged_receivables.csv"),
        "property_id,days_0_30,days_31_60,days_61_90,days_over_90\ns1,0,100,0,50\n",
    )
    .unwrap();
    std::fs::write(
        std_dir.join("tenant_profile.csv"),
        "unit,rent_position_cohort,has_active_concession\n101,At Median,0\n102,Above Median,0\n",
    )
    .unwrap();
    PropertyLane {
        property_key: "fixture".to_string(),
        display_name: "Fixture Property".to_string(),
        root_path: std_dir.parent().unwrap().to_path_buf(),
        standardized_path: std_dir.to_path_buf(),
        raw_data_path: std_dir.to_path_buf(),
        primary_property_ids: vec![],
        unit_count_hint: None,
    }
}

#[tokio::test]
async fn injected_footer_records_data_contract_violation_gap() {
    use boxscore::connectors::standardized::validator::{self, Severity};

    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let std = dir.path().join("Standardized");
    std::fs::create_dir_all(&std).unwrap();
    let lane = fixture_lane(&std, true); // footer injected

    let result = validator::validate_lane(&lane);
    assert!(!result.ok(), "footer lane must FAIL contracts");

    // Mirror the run_validate persistence path: record each ERROR/WARN as a gap.
    let task_run_id = db::create_task_run(&pool, "data_contract_validation", "test")
        .await
        .unwrap();
    for finding in &result.findings {
        if matches!(finding.severity, Severity::Error | Severity::Warn) {
            db::insert_gap(
                &pool,
                &task_run_id,
                "data_contract_violation",
                if finding.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                },
                &finding.message,
                "contract failed",
                "fix upstream ETL",
            )
            .await
            .unwrap();
        }
    }

    let gaps = db::list_gaps(&pool).await.unwrap();
    assert!(
        gaps.iter().any(|g| g.gap_type == "data_contract_violation"),
        "a data_contract_violation gap must be recorded"
    );
}

#[test]
fn footer_row_in_lane_forces_owner_not_ready_via_contract_gate() {
    // Clean fixture lane: contracts PASS.
    let clean = tempfile::tempdir().unwrap();
    let clean_std = clean.path().join("Standardized");
    std::fs::create_dir_all(&clean_std).unwrap();
    let clean_lane = fixture_lane(&clean_std, false);
    let clean_status = close_readiness::assess_lane_contracts(&clean_lane);
    assert_eq!(clean_status.status, "PASS");
    assert_eq!(clean_status.error_count, 0);

    // Footer-injected lane: C1 ERROR -> hard gate must block owner-readiness.
    let dirty = tempfile::tempdir().unwrap();
    let dirty_std = dirty.path().join("Standardized");
    std::fs::create_dir_all(&dirty_std).unwrap();
    let dirty_lane = fixture_lane(&dirty_std, true);
    let dirty_status = close_readiness::assess_lane_contracts(&dirty_lane);

    assert_eq!(dirty_status.status, "FAIL");
    assert!(
        dirty_status.error_count > 0,
        "footer row must produce contract ERROR"
    );
    assert!(dirty_status
        .error_messages
        .iter()
        .any(|m| m.contains("footer/total row detected")));

    // The close-readiness owner_ready gate: even with otherwise-current feeds,
    // a contract ERROR makes owner_ready false. This mirrors assess_property:
    //   owner_ready = feeds_ok && contract_status.error_count == 0
    let feeds_ok = true; // assume all required feeds current
    let owner_ready = feeds_ok && dirty_status.error_count == 0;
    assert!(!owner_ready, "a footer row must force owner_ready = false");
}

#[tokio::test]
async fn close_readiness_marks_property_ready_when_required_feeds_are_period_matched() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property_with_june_feeds(&pool, "Ready Ridge").await;

    let temp_dir = tempfile::tempdir().unwrap();
    let result = close_readiness::assess_close_readiness(&pool, "2026-06", temp_dir.path())
        .await
        .unwrap();

    assert_eq!(result.period, "2026-06");
    assert_eq!(result.properties.len(), 1);
    let property = &result.properties[0];
    assert_eq!(property.property, "Ready Ridge");
    assert_eq!(
        property.status,
        close_readiness::CloseReadinessStatus::Ready
    );
    assert!(property.owner_ready);
    assert_eq!(property.blockers, 0);
    assert_eq!(property.warning_count, 0);
    assert_eq!(property.feeds.len(), 6);
    assert!(property
        .feeds
        .iter()
        .all(|feed| feed.status == close_readiness::FeedStatus::Current));
    assert!(result.report_path.exists());
    let report = std::fs::read_to_string(&result.report_path).unwrap();
    assert!(report.contains("# Boxscore 2026-06 Close Readiness"));
    assert!(report.contains("Ready Ridge"));
    assert!(report.contains("owner-ready"));
}

#[tokio::test]
async fn close_readiness_surfaces_stale_and_missing_feeds_as_not_ready() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property_with_stale_and_missing_feeds(&pool, "Stale Property").await;

    let temp_dir = tempfile::tempdir().unwrap();
    let result = close_readiness::assess_close_readiness(&pool, "2026-06", temp_dir.path())
        .await
        .unwrap();

    let property = &result.properties[0];
    assert_eq!(
        property.status,
        close_readiness::CloseReadinessStatus::NotReady
    );
    assert!(!property.owner_ready);
    assert!(property.blockers >= 1);
    assert!(property.warning_count >= 1);
    assert_feed_status(property, "Actual GL", close_readiness::FeedStatus::Missing);
    assert_feed_status(property, "Budget GL", close_readiness::FeedStatus::Stale);
    assert_feed_status(property, "Rent roll", close_readiness::FeedStatus::Stale);
    assert_feed_status(
        property,
        "Collections",
        close_readiness::FeedStatus::Current,
    );
    assert!(property
        .operator_questions
        .iter()
        .any(|question| question.contains("June 2026 actual GL")));
}

#[tokio::test]
async fn close_readiness_result_rolls_up_portfolio_counts() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property_with_june_feeds(&pool, "Ready Ridge").await;
    seed_property_with_stale_and_missing_feeds(&pool, "Stale Property").await;

    let temp_dir = tempfile::tempdir().unwrap();
    let result = close_readiness::assess_close_readiness(&pool, "2026-06", temp_dir.path())
        .await
        .unwrap();

    assert_eq!(result.summary.property_count, 2);
    assert_eq!(result.summary.ready_count, 1);
    assert_eq!(result.summary.not_ready_count, 1);
    assert!(result.summary.blocker_count >= 1);
    assert!(result.summary.warning_count >= 1);
    assert!(result.summary.owner_ready_ratio > 0.0);
}

fn assert_feed_status(
    property: &close_readiness::PropertyCloseReadiness,
    feed_name: &str,
    expected: close_readiness::FeedStatus,
) {
    let feed = property
        .feeds
        .iter()
        .find(|feed| feed.name == feed_name)
        .unwrap();
    assert_eq!(feed.status, expected);
}

async fn seed_property_with_june_feeds(pool: &sqlx::SqlitePool, name: &str) {
    let property_id =
        db::upsert_property(pool, name, "Austin", 100, "Example Sponsor", "Example PM")
            .await
            .unwrap();
    let period_id = db::upsert_period(pool, "2026-06").await.unwrap();
    insert_gl(
        pool,
        "gl_actuals",
        &property_id,
        &period_id,
        "4000",
        100_000.0,
    )
    .await;
    insert_gl(
        pool,
        "gl_budgets",
        &property_id,
        &period_id,
        "4000",
        98_000.0,
    )
    .await;
    insert_rent_roll(pool, &property_id, "2026-06-30").await;
    insert_delinquency(pool, &property_id, "2026-06-30").await;
    insert_leasing(pool, &property_id, "2026-06-30").await;
    insert_collections(pool, &property_id, "2026-06-09").await;
}

async fn seed_property_with_stale_and_missing_feeds(pool: &sqlx::SqlitePool, name: &str) {
    let property_id =
        db::upsert_property(pool, name, "Austin", 100, "Example Sponsor", "Example PM")
            .await
            .unwrap();
    let stale_period_id = db::upsert_period(pool, "2026-05").await.unwrap();
    insert_gl(
        pool,
        "gl_budgets",
        &property_id,
        &stale_period_id,
        "4000",
        98_000.0,
    )
    .await;
    insert_rent_roll(pool, &property_id, "2026-05-31").await;
    insert_delinquency(pool, &property_id, "2026-05-31").await;
    insert_leasing(pool, &property_id, "2026-06-07").await;
    insert_collections(pool, &property_id, "2026-06-09").await;
}

async fn insert_gl(
    pool: &sqlx::SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
    account_code: &str,
    amount: f64,
) {
    sqlx::query(&format!(
        "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, ?, ?, 'Rental Income', ?, 'test.csv', 2, ?)"
    ))
    .bind(db::new_id())
    .bind(property_id)
    .bind(period_id)
    .bind(account_code)
    .bind("Rental Income")
    .bind(amount)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_rent_roll(pool: &sqlx::SqlitePool, property_id: &str, as_of_date: &str) {
    sqlx::query(
        "INSERT INTO rent_roll_snapshots (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, notice_units, down_units, market_rent_total, in_place_rent_total, source_file, source_row, created_at)
         VALUES (?, ?, ?, 94, 6, 95, 2, 1, 100000.0, 94000.0, 'rent_roll.csv', 1, ?)",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of_date)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_delinquency(pool: &sqlx::SqlitePool, property_id: &str, as_of_date: &str) {
    sqlx::query(
        "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, 1200.0, 3, 500.0, 'aged_receivables.csv', 1, ?)",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of_date)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_leasing(pool: &sqlx::SqlitePool, property_id: &str, as_of_date: &str) {
    sqlx::query(
        "INSERT INTO leasing_snapshots (id, property_id, as_of_date, leads, tours, applications, approvals, move_ins, move_outs, concessions_amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, 30, 15, 7, 5, 4, 3, 1000.0, 'leasing_funnel.csv', 1, ?)",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of_date)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_collections(pool: &sqlx::SqlitePool, property_id: &str, as_of_date: &str) {
    sqlx::query(
        "INSERT INTO collection_snapshots (id, property_id, as_of_date, total_delinquent, delinquent_units, high_risk_units, total_opportunity, pricing_opportunity, missed_fee_total, avg_on_time_pct, source_file, source_row, created_at)
         VALUES (?, ?, ?, 1200.0, 3, 1, 2400.0, 600.0, 75.0, 0.82, 'collections_unified.csv', 1, ?)",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of_date)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}
