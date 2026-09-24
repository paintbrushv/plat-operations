use boxscore::connectors::standardized::rent_roll::parse_unit_leases;
use boxscore::db;
use boxscore::ingest;
use boxscore::models::Call;
use boxscore::variance::{self, VarianceRequest};

async fn seed_property(pool: &sqlx::SqlitePool, name: &str) -> String {
    db::upsert_property(pool, name, "Austin", 100, "Example Sponsor", "Example PM")
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_creates_calls_table() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM calls")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, 0);
}

#[tokio::test]
async fn insert_and_list_calls_roundtrip() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Oak Ridge").await;

    let id = db::insert_call(
        &pool,
        &pid,
        "2026-05",
        "noi_diagnosis",
        "2026-06",
        Some(0.4),
        r#"{"account_code":"5120"}"#,
        None,
    )
    .await
    .unwrap();

    let calls: Vec<Call> = db::list_calls(&pool).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, id);
    assert_eq!(calls[0].status, "open");
    assert_eq!(calls[0].mature_by, "2026-06");

    // Not due yet at an earlier period.
    let due_early = db::calls_due_for_scoring(&pool, "2026-05").await.unwrap();
    assert_eq!(due_early.len(), 0);
    // Due once the period reaches mature_by.
    let due = db::calls_due_for_scoring(&pool, "2026-06").await.unwrap();
    assert_eq!(due.len(), 1);

    db::mark_call_scored(&pool, &id, 1.0, r#"{"hit":true}"#, "normalized ✓")
        .await
        .unwrap();
    let scored: Vec<Call> = db::fetch_scored_calls(&pool, &pid, "noi_diagnosis", 10)
        .await
        .unwrap();
    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].status, "scored");
    assert_eq!(scored[0].score, Some(1.0));
}

#[tokio::test]
async fn record_call_resolves_property_and_persists() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Cedar Park").await;

    let id = db::insert_call(
        &pool,
        &pid,
        "2026-05",
        "delinquency_risk",
        "2026-06",
        Some(0.7),
        r#"{"resident_code":"t101","probability":0.7}"#,
        None,
    )
    .await
    .unwrap();

    let calls = db::list_calls(&pool).await.unwrap();
    assert!(calls
        .iter()
        .any(|c| c.id == id && c.call_type == "delinquency_risk"));
}

/// Convenience wrapper — inserts a single gl_actuals row for a given account code.
async fn insert_gl_actual(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    period_id: &str,
    account_code: &str,
    _account_name: &str,
    _category: &str,
    amount: f64,
) {
    insert_gl_row(
        pool,
        "gl_actuals",
        property_id,
        period_id,
        account_code,
        amount,
    )
    .await;
}

/// Convenience wrapper — inserts a single gl_budgets row for a given account code.
async fn insert_gl_budget(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    period_id: &str,
    account_code: &str,
    _account_name: &str,
    _category: &str,
    amount: f64,
) {
    insert_gl_row(
        pool,
        "gl_budgets",
        property_id,
        period_id,
        account_code,
        amount,
    )
    .await;
}

/// Seed one GL actual and one GL budget row into the named table.
/// Uses the "Repairs & Maintenance" category so ontology maps it to Expense.
async fn insert_gl_row(
    pool: &sqlx::SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
    account_code: &str,
    amount: f64,
) {
    sqlx::query(&format!(
        "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, ?, 'Repairs & Maintenance', 'Repairs & Maintenance', ?, 'test.csv', 1, ?)"
    ))
    .bind(db::new_id())
    .bind(property_id)
    .bind(period_id)
    .bind(account_code)
    .bind(amount)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn variance_emits_noi_diagnosis_calls_for_negative_drivers() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    // Seed property and period.
    let property_id = seed_property(&pool, "Maple Run").await;
    let period_id = db::upsert_period(&pool, "2026-05").await.unwrap();

    // Insert an expense account where actual (2000) > budget (500):
    // this creates a negative NOI driver (expense overrun).
    // Expenses are stored with natural positive sign per Yardi convention.
    insert_gl_row(
        &pool,
        "gl_actuals",
        &property_id,
        &period_id,
        "5200",
        2000.0,
    )
    .await;
    insert_gl_row(&pool, "gl_budgets", &property_id, &period_id, "5200", 500.0).await;

    // Also insert a revenue line so NOI is computable.
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '4000', 'Rental Income', 'rental income', 5000.0, 'test.csv', 2, ?)"
    )
    .bind(db::new_id())
    .bind(&property_id)
    .bind(&period_id)
    .bind(db::now_iso())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_budgets (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '4000', 'Rental Income', 'rental income', 5000.0, 'test.csv', 2, ?)"
    )
    .bind(db::new_id())
    .bind(&property_id)
    .bind(&period_id)
    .bind(db::now_iso())
    .execute(&pool)
    .await
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();
    variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Maple Run".to_string(),
            period: "2026-05".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    // Expect at least one noi_diagnosis call was emitted for the expense overrun.
    let calls = db::list_calls(&pool).await.unwrap();
    let noi_calls: Vec<&Call> = calls
        .iter()
        .filter(|c| c.call_type == "noi_diagnosis")
        .collect();
    assert!(
        !noi_calls.is_empty(),
        "expected at least one noi_diagnosis call to be emitted"
    );
    // The call should mature in the next period (2026-06) and be open.
    assert!(
        noi_calls
            .iter()
            .any(|c| c.mature_by == "2026-06" && c.status == "open"),
        "expected a noi_diagnosis call with mature_by=2026-06 and status=open"
    );
}

#[tokio::test]
async fn variance_emit_is_idempotent() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let property_id = seed_property(&pool, "Maple Run Idempotent").await;
    let period_id = db::upsert_period(&pool, "2026-05").await.unwrap();

    // Expense overrun => negative NOI driver => noi_diagnosis calls will be emitted.
    insert_gl_row(
        &pool,
        "gl_actuals",
        &property_id,
        &period_id,
        "5200",
        2000.0,
    )
    .await;
    insert_gl_row(&pool, "gl_budgets", &property_id, &period_id, "5200", 500.0).await;
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '4000', 'Rental Income', 'rental income', 5000.0, 'test.csv', 2, ?)"
    )
    .bind(db::new_id())
    .bind(&property_id)
    .bind(&period_id)
    .bind(db::now_iso())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_budgets (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '4000', 'Rental Income', 'rental income', 5000.0, 'test.csv', 2, ?)"
    )
    .bind(db::new_id())
    .bind(&property_id)
    .bind(&period_id)
    .bind(db::now_iso())
    .execute(&pool)
    .await
    .unwrap();

    let report_dir = tempfile::tempdir().unwrap();

    // First run: should emit calls.
    variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Maple Run Idempotent".to_string(),
            period: "2026-05".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    let count_after_first = db::list_calls(&pool)
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.call_type == "noi_diagnosis")
        .count();
    assert!(
        count_after_first > 0,
        "first run should emit at least one noi_diagnosis call"
    );

    // Second run on the same property+period: should be a no-op (idempotent).
    variance::analyze_variance(
        &pool,
        VarianceRequest {
            property: "Maple Run Idempotent".to_string(),
            period: "2026-05".to_string(),
        },
        report_dir.path(),
    )
    .await
    .unwrap();

    let count_after_second = db::list_calls(&pool)
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.call_type == "noi_diagnosis")
        .count();
    assert_eq!(
        count_after_second, count_after_first,
        "second variance run should NOT emit additional noi_diagnosis calls (idempotent)"
    );
}

#[tokio::test]
async fn score_sweep_scores_noi_call_and_writes_track_record() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Birch Hollow").await;
    // Open noi_diagnosis call maturing 2026-06, baseline 18000, expecting normalize.
    db::insert_call(
        &pool, &pid, "2026-05", "noi_diagnosis", "2026-06", Some(0.4),
        r#"{"account_code":"5120","category":"Repairs & Maintenance","baseline_actual":18000.0,"expected_direction":"normalize"}"#,
        None,
    ).await.unwrap();
    // Next-period actual is lower -> normalized -> hit.
    let period_id = db::upsert_period(&pool, "2026-06").await.unwrap();
    insert_gl_actual(
        &pool,
        &pid,
        &period_id,
        "5120",
        "R&M",
        "Repairs & Maintenance",
        9000.0,
    )
    .await;

    let n = boxscore::calls::score_due_calls(&pool, "2026-06")
        .await
        .unwrap();
    assert_eq!(n, 1);

    let calls = db::list_calls(&pool).await.unwrap();
    assert_eq!(calls[0].status, "scored");
    assert_eq!(calls[0].score, Some(1.0));

    let mems = db::list_memories(&pool).await.unwrap();
    assert!(mems
        .iter()
        .any(|m| m.memory_type == "track_record" && m.scope == "noi_diagnosis"));
}

#[tokio::test]
async fn unit_receivables_roundtrip_and_lookup() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Stonegate").await;
    db::insert_unit_receivable(
        &pool,
        &pid,
        "2026-06-30",
        "t101",
        Some("Doe"),
        Some("Current"),
        850.0,
        Some(850.0),
        Some(0),
        "ar.csv",
        2,
    )
    .await
    .unwrap();

    assert!(db::receivables_snapshot_exists(&pool, &pid, "2026-06")
        .await
        .unwrap());
    assert!(!db::receivables_snapshot_exists(&pool, &pid, "2026-07")
        .await
        .unwrap());
    assert_eq!(
        db::unit_delinquent_total(&pool, &pid, "t101", "2026-06")
            .await
            .unwrap(),
        850.0
    );
    assert_eq!(
        db::unit_delinquent_total(&pool, &pid, "t999", "2026-06")
            .await
            .unwrap(),
        0.0
    );
}

#[tokio::test]
async fn score_sweep_scores_delinquency_call() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Granite Park").await;
    db::insert_call(
        &pool,
        &pid,
        "2026-05",
        "delinquency_risk",
        "2026-06",
        Some(0.7),
        r#"{"resident_code":"t101","probability":0.7,"window_days":45}"#,
        None,
    )
    .await
    .unwrap();
    // Resident did become delinquent next period.
    db::insert_unit_receivable(
        &pool,
        &pid,
        "2026-06-30",
        "t101",
        None,
        None,
        900.0,
        Some(900.0),
        Some(35),
        "ar.csv",
        2,
    )
    .await
    .unwrap();

    let n = boxscore::calls::score_due_calls(&pool, "2026-06")
        .await
        .unwrap();
    assert_eq!(n, 1);
    let calls = db::list_calls(&pool).await.unwrap();
    let c = calls
        .iter()
        .find(|c| c.call_type == "delinquency_risk")
        .unwrap();
    assert_eq!(c.status, "scored");
    // Brier: 1 - (0.7 - 1.0)^2 = 0.91
    assert!((c.score.unwrap() - 0.91).abs() < 1e-6);
}

#[tokio::test]
async fn system_prompt_includes_track_record_after_scoring() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Willow Bend").await;
    db::upsert_memory(
        &pool,
        "track_record",
        "noi_diagnosis",
        &pid,
        "noi_diagnosis: 67% (3 scored, calibration +0.13)",
        0.67,
        None,
    )
    .await
    .unwrap();

    let prompt = boxscore::ask::build_system_prompt(&pool).await.unwrap();
    assert!(prompt.contains("Harness track record"));
    assert!(prompt.contains("noi_diagnosis: 67%"));
}

#[tokio::test]
async fn unit_leases_roundtrip_and_lookup() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Lakeview").await;
    db::insert_unit_lease(
        &pool,
        &pid,
        "2026-06-30",
        "R-214",
        Some("t101"),
        Some("Doe"),
        Some(1500.0),
        Some(1460.0),
        "rr.csv",
        5,
    )
    .await
    .unwrap();

    assert!(db::leases_snapshot_exists(&pool, &pid, "2026-06")
        .await
        .unwrap());
    assert_eq!(
        db::unit_lease_rent(&pool, &pid, "R-214", "t101", "2026-06")
            .await
            .unwrap(),
        Some(1460.0)
    );
    assert_eq!(
        db::unit_lease_rent(&pool, &pid, "R-214", "t999", "2026-06")
            .await
            .unwrap(),
        None
    );
}

#[test]
fn parse_unit_leases_fixture() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/standardized/maplewood/rent_roll.csv");
    let rows = parse_unit_leases(&fixture).unwrap();
    // Fixture has 4 data rows (A101, A102, A103, A104)
    assert!(rows.len() >= 4);
    assert!(rows.iter().all(|r| !r.unit_label.is_empty()));
    // Spot-check the first row
    let first = &rows[0];
    assert_eq!(first.unit_label, "A101");
    assert_eq!(first.charge_rent, Some(1125.0));
}

#[tokio::test]
async fn backfill_emits_and_scores_historical_calls() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Heritage Oaks").await;

    // Period 2026-04: expense overage -> negative driver -> noi_diagnosis call
    // emitted with mature_by 2026-05.
    let p4 = db::upsert_period(&pool, "2026-04").await.unwrap();
    insert_gl_actual(
        &pool,
        &pid,
        &p4,
        "5200",
        "R&M",
        "Repairs & Maintenance",
        2000.0,
    )
    .await;
    insert_gl_budget(
        &pool,
        &pid,
        &p4,
        "5200",
        "R&M",
        "Repairs & Maintenance",
        500.0,
    )
    .await;

    // Period 2026-05: expense normalized -> the 2026-04 call should score a HIT
    // when scored against 2026-05 actuals.
    let p5 = db::upsert_period(&pool, "2026-05").await.unwrap();
    insert_gl_actual(
        &pool,
        &pid,
        &p5,
        "5200",
        "R&M",
        "Repairs & Maintenance",
        600.0,
    )
    .await;
    insert_gl_budget(
        &pool,
        &pid,
        &p5,
        "5200",
        "R&M",
        "Repairs & Maintenance",
        500.0,
    )
    .await;

    // backfill through 2026-05 with lookback=1 covers both 2026-04 and 2026-05.
    let (analyzed, scored) =
        boxscore::calls::backfill_noi_diagnosis(&pool, "Heritage Oaks", "2026-05", 1)
            .await
            .unwrap();

    assert!(
        analyzed >= 1,
        "expected at least one period analyzed, got {analyzed}"
    );

    let calls = db::list_calls(&pool).await.unwrap();
    let noi: Vec<_> = calls
        .iter()
        .filter(|c| c.call_type == "noi_diagnosis")
        .collect();
    assert!(!noi.is_empty(), "expected backfilled noi_diagnosis calls");

    // At least one call should have been scored (the 2026-04 call matures at 2026-05
    // and 2026-05 actuals exist, so the scorer should fire).
    assert!(
        scored >= 1,
        "expected at least one matured call scored, got {scored}"
    );
    assert!(
        noi.iter().any(|c| c.status == "scored"),
        "expected at least one noi_diagnosis call with status=scored"
    );
}

#[tokio::test]
async fn score_sweep_scores_renewal_call() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Harbor Point").await;
    db::insert_call(
        &pool,
        &pid,
        "2026-05",
        "renewal_rec",
        "2026-06",
        Some(0.6),
        r#"{"unit_label":"R-214","resident_code":"t101","recommended_rent":1485.0}"#,
        None,
    )
    .await
    .unwrap();
    // Resident renewed at 1460 (close to the 1485 rec).
    db::insert_unit_lease(
        &pool,
        &pid,
        "2026-06-30",
        "R-214",
        Some("t101"),
        Some("Doe"),
        Some(1500.0),
        Some(1460.0),
        "rr.csv",
        5,
    )
    .await
    .unwrap();

    let n = boxscore::calls::score_due_calls(&pool, "2026-06")
        .await
        .unwrap();
    assert_eq!(n, 1);
    let calls = db::list_calls(&pool).await.unwrap();
    let c = calls.iter().find(|c| c.call_type == "renewal_rec").unwrap();
    assert_eq!(c.status, "scored");
    // closeness = 1 - |1460-1485|/1485 ≈ 0.9832
    assert!((c.score.unwrap() - 0.9832).abs() < 0.001);
}

#[tokio::test]
async fn import_delinquency_calls_from_csv_is_idempotent() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "Aspen Grove").await;
    let dir = std::env::temp_dir().join("boxscore_import_test_delinquency");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bddre.csv");
    std::fs::write(
        &path,
        "resident_code,predictive_risk_score\nt101,80\nt102,0\n",
    )
    .unwrap();

    let n = boxscore::calls::import_calls_from_csv(
        &pool,
        "delinquency_risk",
        "Aspen Grove",
        "2026-05",
        &path,
    )
    .await
    .unwrap();
    assert_eq!(n, 1); // t102 (score 0) is skipped

    let calls = db::list_calls(&pool).await.unwrap();
    let dq: Vec<_> = calls
        .iter()
        .filter(|c| c.call_type == "delinquency_risk")
        .collect();
    assert_eq!(dq.len(), 1);
    assert_eq!(dq[0].mature_by, "2026-06");
    assert!(dq[0].payload_json.contains("t101"));
    assert!((dq[0].confidence.unwrap() - 0.8).abs() < 1e-9);

    // Re-run: idempotent, no duplicates.
    let n2 = boxscore::calls::import_calls_from_csv(
        &pool,
        "delinquency_risk",
        "Aspen Grove",
        "2026-05",
        &path,
    )
    .await
    .unwrap();
    assert_eq!(n2, 0);
    assert_eq!(
        db::list_calls(&pool)
            .await
            .unwrap()
            .iter()
            .filter(|c| c.call_type == "delinquency_risk")
            .count(),
        1
    );
}

#[tokio::test]
async fn import_renewal_calls_from_csv_maps_confidence() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "Brook Hollow").await;
    let dir = std::env::temp_dir().join("boxscore_import_test_renewal");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rpcoe.csv");
    std::fs::write(
        &path,
        "unit,resident_code,recommended_new_rent,confidence\nR-214,t101,1485,High\n",
    )
    .unwrap();

    let n = boxscore::calls::import_calls_from_csv(
        &pool,
        "renewal_rec",
        "Brook Hollow",
        "2026-05",
        &path,
    )
    .await
    .unwrap();
    assert_eq!(n, 1);

    let calls = db::list_calls(&pool).await.unwrap();
    let r = calls.iter().find(|c| c.call_type == "renewal_rec").unwrap();
    assert!(r.payload_json.contains("R-214"));
    assert!(r.payload_json.contains("1485"));
    assert!((r.confidence.unwrap() - 0.9).abs() < 1e-9); // High -> 0.9
}

#[tokio::test]
async fn monthly_actuals_roundtrip_and_t12_mean() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Granite Ridge").await;
    // 6 prior months of account 6100 at 1000 each, then the target month at 4000.
    for (per, amt) in [
        ("2025-01", 1000.0),
        ("2025-02", 1000.0),
        ("2025-03", 1000.0),
        ("2025-04", 1000.0),
        ("2025-05", 1000.0),
        ("2025-06", 1000.0),
    ] {
        db::insert_monthly_actual(&pool, &pid, per, "6100", Some("R&M"), amt, "m.csv")
            .await
            .unwrap();
    }
    db::insert_monthly_actual(&pool, &pid, "2025-07", "6100", Some("R&M"), 4000.0, "m.csv")
        .await
        .unwrap();

    assert_eq!(
        db::monthly_actual(&pool, &pid, "2025-07", "6100")
            .await
            .unwrap(),
        Some(4000.0)
    );
    let (mean, n) = db::t12_mean(&pool, &pid, "2025-07", "6100")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 6);
    assert!((mean - 1000.0).abs() < 1e-9);
    assert!(db::monthly_actuals_exist(&pool, &pid, "2025-07")
        .await
        .unwrap());
    assert!(!db::monthly_actuals_exist(&pool, &pid, "2025-08")
        .await
        .unwrap());
    let accts = db::pl_accounts_for_period(&pool, &pid, "2025-07")
        .await
        .unwrap();
    assert!(accts.iter().any(|(c, _)| c == "6100"));
}

#[tokio::test]
async fn ingest_monthly_actuals_csv_inserts_and_queries() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Sunset Ridge").await;

    // Write a tiny gl_monthly_actuals.csv with 2 rows for account 6100.
    let dir = std::env::temp_dir().join("boxscore_monthly_actuals_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gl_monthly_actuals.csv");
    std::fs::write(
        &path,
        "property_id,period,account_code,account_name,amount\n\
         Sunset Ridge,2025-10,6100,Repairs & Maintenance,1200.00\n\
         Sunset Ridge,2025-11,6100,Repairs & Maintenance,950.50\n",
    )
    .unwrap();

    let n = ingest::ingest_monthly_actuals_csv(&pool, &pid, &path)
        .await
        .unwrap();

    // Both rows should have been ingested.
    assert_eq!(n, 2, "expected 2 rows ingested");

    // db::monthly_actual should return the correct value for 2025-10.
    let oct = db::monthly_actual(&pool, &pid, "2025-10", "6100")
        .await
        .unwrap();
    assert_eq!(oct, Some(1200.0));

    // And for 2025-11.
    let nov = db::monthly_actual(&pool, &pid, "2025-11", "6100")
        .await
        .unwrap();
    assert!((nov.unwrap() - 950.5).abs() < 1e-9);

    // monthly_actuals_exist should confirm the period is present.
    assert!(db::monthly_actuals_exist(&pool, &pid, "2025-10")
        .await
        .unwrap());
    assert!(!db::monthly_actuals_exist(&pool, &pid, "2025-12")
        .await
        .unwrap());

    // pl_accounts_for_period should list account 6100 for 2025-10.
    let accts = db::pl_accounts_for_period(&pool, &pid, "2025-10")
        .await
        .unwrap();
    assert!(
        accts
            .iter()
            .any(|(c, name)| c == "6100" && name == "Repairs & Maintenance"),
        "expected account 6100 with name in pl_accounts_for_period"
    );
}

#[tokio::test]
async fn emit_t12_reversion_flags_deviating_account() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Summit View").await;
    // Account 6100: 6 prior months at 1000, target month 2025-07 spikes to 5000 (dev +4000 > thresholds).
    for per in [
        "2025-01", "2025-02", "2025-03", "2025-04", "2025-05", "2025-06",
    ] {
        db::insert_monthly_actual(&pool, &pid, per, "6100", Some("R&M"), 1000.0, "m.csv")
            .await
            .unwrap();
    }
    db::insert_monthly_actual(&pool, &pid, "2025-07", "6100", Some("R&M"), 5000.0, "m.csv")
        .await
        .unwrap();
    // Account 7000: flat 2000 every month incl target -> no deviation -> not flagged.
    for per in [
        "2025-01", "2025-02", "2025-03", "2025-04", "2025-05", "2025-06", "2025-07",
    ] {
        db::insert_monthly_actual(&pool, &pid, per, "7000", Some("Flat"), 2000.0, "m.csv")
            .await
            .unwrap();
    }
    // Both accounts need an NOI category — the emitter only flags categorized P&L accounts
    // (skips balance-sheet/Unmapped). insert_gl_actual stamps a category.
    let p = db::upsert_period(&pool, "2025-07").await.unwrap();
    insert_gl_actual(&pool, &pid, &p, "6100", "", "", 1.0).await;
    insert_gl_actual(&pool, &pid, &p, "7000", "", "", 1.0).await;

    let n = boxscore::calls::emit_t12_reversion_calls(&pool, &pid, "2025-07")
        .await
        .unwrap();
    assert_eq!(n, 1);
    let calls = db::list_calls(&pool).await.unwrap();
    let c = calls
        .iter()
        .find(|c| c.call_type == "t12_reversion")
        .unwrap();
    assert_eq!(c.mature_by, "2025-08");
    assert!(c.payload_json.contains("6100"));
    assert!(c.payload_json.contains("\"baseline_mean\":1000"));

    // Idempotent re-run.
    let n2 = boxscore::calls::emit_t12_reversion_calls(&pool, &pid, "2025-07")
        .await
        .unwrap();
    assert_eq!(n2, 0);
}

#[tokio::test]
async fn score_sweep_scores_t12_reversion_partial() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Cobalt Court").await;
    // Open t12_reversion call: baseline_mean 1000, actual_at_origin 5000 (dev +4000), maturing 2025-08.
    db::insert_call(&pool, &pid, "2025-07", "t12_reversion", "2025-08", Some(0.5),
        r#"{"account_code":"6100","account_name":"R&M","baseline_mean":1000.0,"actual_at_origin":5000.0}"#, None).await.unwrap();
    // Next month reverts to 2000 (dev +1000) -> closed 3000 of 4000 -> 75%.
    db::insert_monthly_actual(&pool, &pid, "2025-08", "6100", Some("R&M"), 2000.0, "m.csv")
        .await
        .unwrap();

    let n = boxscore::calls::score_due_calls(&pool, "2025-08")
        .await
        .unwrap();
    assert_eq!(n, 1);
    let calls = db::list_calls(&pool).await.unwrap();
    let c = calls
        .iter()
        .find(|c| c.call_type == "t12_reversion")
        .unwrap();
    assert_eq!(c.status, "scored");
    assert!((c.score.unwrap() - 0.75).abs() < 1e-6); // (|4000|-|1000|)/|4000| = 0.75
}

#[tokio::test]
async fn backfill_t12_emits_and_scores() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Ironwood").await;
    // 13 months of account 6100: flat 1000 for 12 months, then a spike at 2026-01, revert at 2026-02.
    let mut per = "2025-01".to_string();
    for _ in 0..12 {
        db::insert_monthly_actual(&pool, &pid, &per, "6100", Some("R&M"), 1000.0, "m.csv")
            .await
            .unwrap();
        per = boxscore::calls::next_period(&per).unwrap();
    }
    // per is now 2026-01: spike
    db::insert_monthly_actual(&pool, &pid, "2026-01", "6100", Some("R&M"), 6000.0, "m.csv")
        .await
        .unwrap();
    // 2026-02 revert toward mean
    db::insert_monthly_actual(&pool, &pid, "2026-02", "6100", Some("R&M"), 1500.0, "m.csv")
        .await
        .unwrap();
    // Account needs an NOI category for the emitter to flag it.
    let gp = db::upsert_period(&pool, "2026-01").await.unwrap();
    insert_gl_actual(&pool, &pid, &gp, "6100", "", "", 1.0).await;

    // from 2026-01 (has full 12-month trailing window) through 2026-02.
    let (periods, scored) =
        boxscore::calls::backfill_t12_reversion(&pool, "Ironwood", "2026-02", Some("2026-01"))
            .await
            .unwrap();
    assert!(periods >= 1);
    let calls = db::list_calls(&pool).await.unwrap();
    let t12: Vec<_> = calls
        .iter()
        .filter(|c| c.call_type == "t12_reversion")
        .collect();
    assert!(!t12.is_empty(), "expected t12_reversion calls");
    // The 2026-01 call matures 2026-02 (actuals exist) -> scored.
    assert!(scored >= 1, "expected >=1 scored, got {scored}");
    assert!(t12.iter().any(|c| c.status == "scored"));
}

#[tokio::test]
async fn ingest_monthly_actuals_csv_skips_bad_rows() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Pebble Creek").await;

    let dir = std::env::temp_dir().join("boxscore_monthly_actuals_skip_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gl_monthly_actuals_bad.csv");
    std::fs::write(
        &path,
        // Row 1: valid
        // Row 2: blank period — skip
        // Row 3: blank account_code — skip
        // Row 4: unparseable amount — skip
        "property_id,period,account_code,account_name,amount\n\
         Pebble Creek,2025-10,6100,R&M,500.00\n\
         Pebble Creek,,6100,R&M,200.00\n\
         Pebble Creek,2025-10,,R&M,200.00\n\
         Pebble Creek,2025-10,6200,R&M,not_a_number\n",
    )
    .unwrap();

    let n = ingest::ingest_monthly_actuals_csv(&pool, &pid, &path)
        .await
        .unwrap();

    // Only the first row is valid.
    assert_eq!(n, 1, "expected only 1 valid row ingested");
    assert_eq!(
        db::monthly_actual(&pool, &pid, "2025-10", "6100")
            .await
            .unwrap(),
        Some(500.0)
    );
}

#[tokio::test]
async fn reversion_report_aggregates_by_noi_category() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Reversion Test Prop").await;

    // Seed a period and gl_actuals rows with distinct categories.
    let period_id = db::upsert_period(&pool, "2026-05").await.unwrap();
    // account 6100 -> "Repairs & Maintenance"
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '6100', 'R&M', 'Repairs & Maintenance', 5000.0, 'test.csv', 1, ?)"
    )
    .bind(db::new_id()).bind(&pid).bind(&period_id).bind(db::now_iso())
    .execute(&pool).await.unwrap();
    // account 6550 -> "Taxes"
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, '6550', 'Real Estate Taxes', 'Taxes', 1000.0, 'test.csv', 2, ?)"
    )
    .bind(db::new_id()).bind(&pid).bind(&period_id).bind(db::now_iso())
    .execute(&pool).await.unwrap();

    // Insert two scored t12_reversion calls: one for 6100 (score 0.9), one for 6550 (score 0.1).
    let id_rm = db::insert_call(
        &pool, &pid, "2026-04", "t12_reversion", "2026-05",
        Some(0.5), r#"{"account_code":"6100","account_name":"R&M","baseline_mean":1000.0,"actual_at_origin":5000.0}"#, None,
    ).await.unwrap();
    db::mark_call_scored(
        &pool,
        &id_rm,
        0.9,
        r#"{"reverted_fraction":0.9}"#,
        "R&M reverted",
    )
    .await
    .unwrap();

    let id_tax = db::insert_call(
        &pool, &pid, "2026-04", "t12_reversion", "2026-05",
        Some(0.5), r#"{"account_code":"6550","account_name":"Taxes","baseline_mean":1000.0,"actual_at_origin":1200.0}"#, None,
    ).await.unwrap();
    db::mark_call_scored(
        &pool,
        &id_tax,
        0.1,
        r#"{"reverted_fraction":0.1}"#,
        "Taxes persisted",
    )
    .await
    .unwrap();

    let stats =
        boxscore::calls::reversion_report(&pool, "t12_reversion", Some("Reversion Test Prop"))
            .await
            .unwrap();

    // Expect exactly two categories: "Repairs & Maintenance" and "Taxes".
    assert_eq!(
        stats.len(),
        2,
        "expected 2 category rows, got {:?}",
        stats.iter().map(|s| &s.category).collect::<Vec<_>>()
    );

    let rm = stats
        .iter()
        .find(|s| s.category == "Repairs & Maintenance")
        .expect("expected Repairs & Maintenance category");
    assert_eq!(rm.n, 1);
    assert!(
        (rm.mean_score - 0.9).abs() < 1e-9,
        "R&M mean_score expected 0.9, got {}",
        rm.mean_score
    );
    assert!(
        (rm.pct_strong - 1.0).abs() < 1e-9,
        "R&M pct_strong expected 1.0, got {}",
        rm.pct_strong
    );

    let tax = stats
        .iter()
        .find(|s| s.category == "Taxes")
        .expect("expected Taxes category");
    assert_eq!(tax.n, 1);
    assert!(
        (tax.mean_score - 0.1).abs() < 1e-9,
        "Taxes mean_score expected 0.1, got {}",
        tax.mean_score
    );
    assert!(
        (tax.pct_strong - 0.0).abs() < 1e-9,
        "Taxes pct_strong expected 0.0, got {}",
        tax.pct_strong
    );
}

/// Re-ingesting the same gl_monthly_actuals.csv must NOT double account totals.
/// This is the idempotency regression test for the delete-first fix (FIX I-1).
#[tokio::test]
async fn ingest_monthly_actuals_csv_reingest_is_idempotent() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Cedar Glen").await;

    let dir = std::env::temp_dir().join("boxscore_monthly_actuals_idempotent_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gl_monthly_actuals.csv");
    std::fs::write(
        &path,
        "property_id,period,account_code,account_name,amount\n\
         Cedar Glen,2025-10,6100,Repairs & Maintenance,1200.00\n\
         Cedar Glen,2025-10,7000,Management Fees,800.00\n",
    )
    .unwrap();

    // First ingest.
    let n1 = ingest::ingest_monthly_actuals_csv(&pool, &pid, &path)
        .await
        .unwrap();
    assert_eq!(n1, 2, "first ingest: expected 2 rows");

    let oct_6100_first = db::monthly_actual(&pool, &pid, "2025-10", "6100")
        .await
        .unwrap();
    assert_eq!(
        oct_6100_first,
        Some(1200.0),
        "first ingest: 6100 should be 1200"
    );

    // Second ingest of the SAME file — must replace, not append.
    let n2 = ingest::ingest_monthly_actuals_csv(&pool, &pid, &path)
        .await
        .unwrap();
    assert_eq!(n2, 2, "second ingest: expected 2 rows");

    let oct_6100_second = db::monthly_actual(&pool, &pid, "2025-10", "6100")
        .await
        .unwrap();
    assert_eq!(
        oct_6100_second,
        Some(1200.0),
        "re-ingest must not double: expected 1200, not 2400"
    );

    let oct_7000_second = db::monthly_actual(&pool, &pid, "2025-10", "7000")
        .await
        .unwrap();
    assert_eq!(
        oct_7000_second,
        Some(800.0),
        "re-ingest must not double: expected 800, not 1600"
    );

    // Row count in monthly_actuals for this property+period must stay at 2 (not 4).
    let row_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monthly_actuals WHERE property_id = ? AND period = ?",
    )
    .bind(&pid)
    .bind("2025-10")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row_count, 2,
        "expected 2 rows in DB after re-ingest, not {row_count}"
    );
}

async fn insert_gl_cat(
    pool: &sqlx::SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
    account_code: &str,
    category: &str,
    amount: f64,
) {
    sqlx::query(&format!(
        "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'test.csv', 1, 't')"
    ))
    .bind(db::new_id())
    .bind(property_id)
    .bind(period_id)
    .bind(account_code)
    .bind(category)
    .bind(category)
    .bind(amount)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn category_normalize_rate_reflects_history() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Rate Ridge").await;
    for _ in 0..10 {
        let id = db::insert_call(
            &pool,
            &pid,
            "2025-05",
            "noi_diagnosis",
            "2025-06",
            Some(0.4),
            r#"{"account_code":"6550","category":"Taxes","expected_direction":"normalize"}"#,
            None,
        )
        .await
        .unwrap();
        db::mark_call_scored(&pool, &id, 0.0, "{}", "miss")
            .await
            .unwrap();
    }
    let (rate, n) = db::category_normalize_rate(&pool, &pid, "Taxes")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 10);
    assert!((rate - 0.0).abs() < 1e-9);
    assert!(
        db::category_normalize_rate(&pool, &pid, "Repairs & Maintenance")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn noi_emitter_flips_to_persist_for_poor_normalize_category() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = seed_property(&pool, "Persist Park").await;
    // History: 12 scored "normalize" Taxes calls, all wrong (rate 0 < 0.5, n >= 10).
    for _ in 0..12 {
        let id = db::insert_call(
            &pool,
            &pid,
            "2025-05",
            "noi_diagnosis",
            "2025-06",
            Some(0.4),
            r#"{"account_code":"6550","category":"Taxes","expected_direction":"normalize"}"#,
            None,
        )
        .await
        .unwrap();
        db::mark_call_scored(&pool, &id, 0.0, "{}", "miss")
            .await
            .unwrap();
    }
    // A Taxes account that's a negative driver in 2026-05 (actual > budget).
    let period_id = db::upsert_period(&pool, "2026-05").await.unwrap();
    insert_gl_cat(
        &pool,
        "gl_actuals",
        &pid,
        &period_id,
        "6550",
        "Taxes",
        5000.0,
    )
    .await;
    insert_gl_cat(
        &pool,
        "gl_budgets",
        &pid,
        &period_id,
        "6550",
        "Taxes",
        1000.0,
    )
    .await;

    let dir = std::env::temp_dir().join("noi_persist_test");
    std::fs::create_dir_all(&dir).unwrap();
    boxscore::variance::analyze_variance(
        &pool,
        boxscore::variance::VarianceRequest {
            property: "Persist Park".into(),
            period: "2026-05".into(),
        },
        &dir,
    )
    .await
    .unwrap();

    let calls = db::list_calls(&pool).await.unwrap();
    let new = calls
        .iter()
        .find(|c| {
            c.call_type == "noi_diagnosis"
                && c.origin_period == "2026-05"
                && c.payload_json.contains("6550")
        })
        .expect("expected a 2026-05 noi_diagnosis call for the Taxes account");
    assert!(
        new.payload_json
            .contains("\"expected_direction\":\"persist\""),
        "expected persist for poor-normalize Taxes, got: {}",
        new.payload_json
    );
}
