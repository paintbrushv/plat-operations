use std::path::PathBuf;

use boxscore::{
    connectors::standardized::{
        gl_transactions::{ingest_gl_transactions_for_lane, is_resident_payee},
        source_registry::PropertyLane,
    },
    db,
};

// ---------------------------------------------------------------------------
// A1: Schema / migration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gl_transactions_table_exists_after_init() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn init_database_is_idempotent() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    // Second call must not error or duplicate rows.
    db::init_database(&pool).await.unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// A2: Parquet ingest connector tests
// ---------------------------------------------------------------------------

/// Returns a PropertyLane (plus its owning tempdir) whose standardized_path
/// contains the small committed fixture parquet under the expected name, so
/// `lane.standardized_path.join("gl_transactions.parquet")` resolves to it.
fn fixture_lane() -> (tempfile::TempDir, PropertyLane) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gl_transactions_small.parquet");
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::copy(&fixture, temp_dir.path().join("gl_transactions.parquet")).unwrap();
    let dir = temp_dir.path().to_path_buf();
    let lane = PropertyLane {
        property_key: "maplewood".to_string(),
        display_name: "Maplewood Commons".to_string(),
        standardized_path: dir.clone(),
        raw_data_path: dir.clone(),
        root_path: dir,
        primary_property_ids: vec!["p101".to_string()],
        unit_count_hint: Some(494),
    };
    (temp_dir, lane)
}

#[tokio::test]
async fn ingest_fixture_inserts_correct_row_count() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let (_guard, lane) = fixture_lane();
    let summary = ingest_gl_transactions_for_lane(&pool, &lane, None)
        .await
        .unwrap();

    // Fixture has 5 rows; all 5 have valid periods and account codes.
    assert_eq!(
        summary.rows_inserted, 5,
        "all 5 fixture rows should be inserted"
    );
    assert_eq!(summary.rows_skipped, 0);

    let db_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_count, 5);
}

#[tokio::test]
async fn period_is_normalized_to_yyyy_mm() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let (_guard, lane) = fixture_lane();
    ingest_gl_transactions_for_lane(&pool, &lane, None)
        .await
        .unwrap();

    // The fixture has periods 2026-04, 2025-12, 2026-03.
    let periods: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT period FROM gl_transactions ORDER BY period")
            .fetch_all(&pool)
            .await
            .unwrap();

    assert!(
        periods.iter().all(|p| p.len() == 7 && p.contains('-')),
        "all periods must be YYYY-MM: {periods:?}"
    );
    assert!(periods.contains(&"2026-04".to_string()));
    assert!(periods.contains(&"2026-03".to_string()));
    assert!(periods.contains(&"2025-12".to_string()));
}

#[tokio::test]
async fn resident_flag_set_correctly() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let (_guard, lane) = fixture_lane();
    ingest_gl_transactions_for_lane(&pool, &lane, None)
        .await
        .unwrap();

    // "Hill (t0171778)" and "Smith (t0000001)" and "Jones (t0000002)" → is_resident = 1
    let resident_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions WHERE is_resident = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    // "MPG Security Solutions (mpgse)" and "Vendor Co" → is_resident = 0
    let vendor_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions WHERE is_resident = 0")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(resident_count, 3, "3 resident rows expected");
    assert_eq!(vendor_count, 2, "2 vendor rows expected");
}

#[tokio::test]
async fn reingest_does_not_duplicate() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let (_guard, lane) = fixture_lane();
    ingest_gl_transactions_for_lane(&pool, &lane, None)
        .await
        .unwrap();
    ingest_gl_transactions_for_lane(&pool, &lane, None)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 5, "re-ingest must replace, not duplicate");
}

#[tokio::test]
async fn since_filter_drops_earlier_periods() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let (_guard, lane) = fixture_lane();
    // since=2026-01 → only periods >= 2026-01 are inserted (2026-04 x3 + 2026-03 x1 = 4)
    let summary = ingest_gl_transactions_for_lane(&pool, &lane, Some("2026-01"))
        .await
        .unwrap();

    // 2025-12 row should be filtered out → 4 rows inserted
    assert_eq!(
        summary.rows_inserted, 4,
        "since filter should drop 2025-12 row"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 4);
}

// ---------------------------------------------------------------------------
// A3: Query helper tests
// ---------------------------------------------------------------------------

/// Seed a property plus a handful of gl_transactions rows (and one gl_actuals
/// row so account_activity can resolve an account name).
async fn seed_ledger(pool: &sqlx::SqlitePool) -> String {
    let property_id =
        db::upsert_property(pool, "Test Property", "DFW", 100, "Example Sponsor", "PMC")
            .await
            .unwrap();
    let period_id = db::upsert_period(pool, "2026-04").await.unwrap();

    // gl_actuals row gives account 4000 its display name.
    sqlx::query(
        "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
         VALUES (?, ?, ?, '4000', 'Rental Income', 'Rental Income', 2300.0, 'seed.csv', 1, ?)",
    )
    .bind(db::new_id())
    .bind(&property_id)
    .bind(&period_id)
    .bind(db::now_iso())
    .execute(pool)
    .await
    .unwrap();

    // (account_code, txn_date, period, payee, is_resident, amount, remarks)
    type SeedRow<'a> = (
        &'a str,
        Option<&'a str>,
        &'a str,
        &'a str,
        i64,
        f64,
        Option<&'a str>,
    );
    let rows: Vec<SeedRow> = vec![
        (
            "4000",
            Some("2026-04-03"),
            "2026-04",
            "Hill (t0171778)",
            1,
            -1200.0,
            Some("April rent"),
        ),
        (
            "4000",
            Some("2026-04-01"),
            "2026-04",
            "Smith (t0000001)",
            1,
            -1100.0,
            None,
        ),
        (
            "5200",
            Some("2026-04-15"),
            "2026-04",
            "100% Maintenance Co",
            0,
            350.0,
            Some("HVAC repair"),
        ),
        (
            "5200",
            Some("2026-03-20"),
            "2026-03",
            "MPG Security Solutions (mpgse)",
            0,
            275.0,
            Some("patrol"),
        ),
    ];
    for (account_code, txn_date, period, payee, is_resident, amount, remarks) in rows {
        sqlx::query(
            "INSERT INTO gl_transactions (id, property_id, entity_code, account_code, txn_date, period, payee, is_resident, control, reference, amount, remarks, source_file, source_row, created_at) \
             VALUES (?, ?, 'p101', ?, ?, ?, ?, ?, NULL, NULL, ?, ?, 'seed.parquet', 1, ?)",
        )
        .bind(db::new_id())
        .bind(&property_id)
        .bind(account_code)
        .bind(txn_date)
        .bind(period)
        .bind(payee)
        .bind(is_resident)
        .bind(amount)
        .bind(remarks)
        .bind(db::now_iso())
        .execute(pool)
        .await
        .unwrap();
    }
    property_id
}

#[tokio::test]
async fn account_activity_groups_and_resolves_names() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let property_id = seed_ledger(&pool).await;

    let activity = db::account_activity(&pool, &property_id, "2026-04")
        .await
        .unwrap();

    assert_eq!(activity.len(), 2, "two accounts active in 2026-04");
    let rental = activity
        .iter()
        .find(|a| a.account_code == "4000")
        .expect("account 4000 present");
    assert_eq!(rental.account_name, "Rental Income");
    assert_eq!(rental.txn_count, 2);
    assert!((rental.total - (-2300.0)).abs() < 1e-9);

    let repairs = activity
        .iter()
        .find(|a| a.account_code == "5200")
        .expect("account 5200 present");
    assert_eq!(repairs.account_name, "", "no gl_actuals name for 5200");
    assert_eq!(repairs.txn_count, 1);
    assert!((repairs.total - 350.0).abs() < 1e-9);
}

#[tokio::test]
async fn list_account_transactions_orders_by_date() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let property_id = seed_ledger(&pool).await;

    let txns = db::list_account_transactions(&pool, &property_id, "4000", "2026-04")
        .await
        .unwrap();

    assert_eq!(txns.len(), 2);
    assert_eq!(txns[0].txn_date.as_deref(), Some("2026-04-01"));
    assert_eq!(txns[1].txn_date.as_deref(), Some("2026-04-03"));
    assert_eq!(txns[1].payee, "Hill (t0171778)");
}

#[tokio::test]
async fn search_transactions_matches_payee_remarks_and_account() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let property_id = seed_ledger(&pool).await;

    // Match on payee.
    let by_payee = db::search_transactions(&pool, Some(&property_id), "MPG", None, false, 50)
        .await
        .unwrap();
    assert_eq!(by_payee.len(), 1);
    assert_eq!(by_payee[0].account_code, "5200");

    // Match on remarks.
    let by_remarks = db::search_transactions(&pool, Some(&property_id), "HVAC", None, false, 50)
        .await
        .unwrap();
    assert_eq!(by_remarks.len(), 1);
    assert_eq!(by_remarks[0].payee, "100% Maintenance Co");

    // Match on account_code, no property filter.
    let by_account = db::search_transactions(&pool, None, "4000", None, false, 50)
        .await
        .unwrap();
    assert_eq!(by_account.len(), 2);

    // Limit respected.
    let limited = db::search_transactions(&pool, None, "4000", None, false, 1)
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
}

#[tokio::test]
async fn search_transactions_escapes_like_wildcards() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let property_id = seed_ledger(&pool).await;

    // "100%" must match only the literal payee "100% Maintenance Co",
    // not act as a wildcard matching everything starting with 100.
    let hits = db::search_transactions(&pool, Some(&property_id), "100%", None, false, 50)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].payee, "100% Maintenance Co");

    // A bare "%" needle must not match every row.
    let wild = db::search_transactions(&pool, Some(&property_id), "zzz%zzz", None, false, 50)
        .await
        .unwrap();
    assert!(wild.is_empty());

    // Underscore is also escaped: "t_0" must not wildcard-match "t00".
    let underscore = db::search_transactions(&pool, Some(&property_id), "t_0", None, false, 50)
        .await
        .unwrap();
    assert!(underscore.is_empty());
}

// ---------------------------------------------------------------------------
// Unit tests for is_resident_payee helper
// ---------------------------------------------------------------------------

#[test]
fn resident_payee_detection() {
    assert!(is_resident_payee("Hill (t0171778)"));
    assert!(is_resident_payee("Smith (t0000001)"));
    assert!(is_resident_payee("Jones (t1)"));
    // Not resident:
    assert!(!is_resident_payee("MPG Security Solutions (mpgse)"));
    assert!(!is_resident_payee("Vendor Co"));
    assert!(!is_resident_payee("(t)")); // no digits after 't'
    assert!(!is_resident_payee("Foo (tabcd)")); // letters after 't', not digits
    assert!(!is_resident_payee("")); // empty
    assert!(!is_resident_payee("()")); // no 't'
}
