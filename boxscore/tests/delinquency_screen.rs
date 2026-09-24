//! F2 — Delinquency / Collections screen tests (DB helpers + aging buckets).

use std::path::PathBuf;

use boxscore::db;
use boxscore::tui::bddre;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bddre")
        .join(name)
}

#[test]
fn bddre_intervention_queue_parses_worst_first_and_cleans_actions() {
    let rows =
        bddre::parse_intervention_queue("maplewood", &fixture("bddre_intervention_queue.csv"));
    assert_eq!(rows.len(), 2);
    // Sorted by the caller; parse keeps file order, so sort here mirrors discover().
    let mut rows = rows;
    rows.sort_by(|a, b| b.risk_score.partial_cmp(&a.risk_score).unwrap());
    // Worst-first: 82.4 High before 48.7 Medium.
    assert_eq!(rows[0].resident_name, "Resident One");
    assert_eq!(rows[0].risk_tier, "High");
    assert_eq!(rows[0].total_delinquent, 2480.00);
    // Action string is clean — no brackets or quotes.
    assert_eq!(rows[0].top_action, "Send cure-or-quit notice");
    assert!(!rows[0].top_action.contains('['));
    assert!(!rows[0].top_action.contains('\''));
    // The second row's first list element is a PLACEHOLDER — must be skipped.
    assert_eq!(rows[1].top_action, "Structured payment plan option");
}

#[test]
fn bddre_watchlist_parses_with_defaults() {
    let rows = bddre::parse_pre_delinquency_watchlist(
        "maplewood",
        &fixture("bddre_pre_delinquency_watchlist.csv"),
    );
    assert_eq!(rows.len(), 2);
    let high = rows.iter().find(|r| r.risk_tier == "High").unwrap();
    assert_eq!(high.resident_name, "Resident Four");
    assert_eq!(high.risk_score, 71.2);
    // Watchlist has no delinquent balance / action.
    assert_eq!(high.total_delinquent, 0.0);
    assert_eq!(high.top_action, "");
}

#[test]
fn bddre_missing_file_is_empty_not_a_panic() {
    let rows = bddre::parse_intervention_queue("Ghost", &PathBuf::from("/tmp/nope-xyz-bddre.csv"));
    assert!(rows.is_empty());
}

async fn seed_property(pool: &sqlx::SqlitePool, id: &str, name: &str) {
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at) \
         VALUES (?, ?, 'Phoenix, AZ', 300, 'Owner LP', 'PM Co', '2026-01-01')",
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn list_receivables_returns_worst_first() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    // Three residents: small, large, prepaid (credit).
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-31",
        "tA",
        Some("Small Owe"),
        Some("current"),
        500.0,
        Some(500.0),
        Some(10),
        "ar.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-31",
        "tB",
        Some("Big Owe"),
        Some("current"),
        3200.0,
        Some(3200.0),
        Some(95),
        "ar.csv",
        3,
    )
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-31",
        "tC",
        Some("Prepaid Resident"),
        Some("prepaid"),
        0.0,
        Some(-450.0),
        None,
        "ar.csv",
        4,
    )
    .await
    .unwrap();

    let rows = db::list_receivables_for_period(&pool, "p1", "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    // Worst-first: highest total_delinquent at the top.
    assert_eq!(rows[0].resident_code, "tB");
    assert_eq!(rows[0].total_delinquent, 3200.0);
    assert_eq!(rows[1].resident_code, "tA");
    // Prepaid (0.0 delinquent) sorts last.
    assert_eq!(rows[2].resident_code, "tC");
    assert_eq!(rows[2].current_owed, Some(-450.0));
}

#[tokio::test]
async fn list_receivables_uses_latest_snapshot_per_property() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    // Two snapshots in the same period; only the newest should be returned.
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-15",
        "tStale",
        Some("Stale"),
        Some("current"),
        999.0,
        Some(999.0),
        Some(20),
        "ar.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-31",
        "tFresh",
        Some("Fresh"),
        Some("current"),
        100.0,
        Some(100.0),
        Some(5),
        "ar.csv",
        3,
    )
    .await
    .unwrap();

    let rows = db::list_receivables_for_period(&pool, "p1", "2026-05")
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only the latest as_of_date should be returned"
    );
    assert_eq!(rows[0].resident_code, "tFresh");
}

#[tokio::test]
async fn aging_buckets_populate_from_days_late() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    // One resident in each of the five buckets + one prepaid.
    let cases: &[(&str, f64, Option<i64>)] = &[
        ("tCur", 200.0, Some(0)),    // current
        ("t30", 800.0, Some(20)),    // 0-30
        ("t60", 1100.0, Some(45)),   // 31-60
        ("t90", 1400.0, Some(75)),   // 61-90
        ("t90p", 2600.0, Some(120)), // 90+
        ("tNull", 700.0, None),      // null → treated as aged (90+)
    ];
    for (i, (code, amt, days)) in cases.iter().enumerate() {
        db::insert_unit_receivable(
            &pool,
            "p1",
            "2026-05-31",
            code,
            Some("R"),
            Some("current"),
            *amt,
            Some(*amt),
            *days,
            "ar.csv",
            (i + 2) as i64,
        )
        .await
        .unwrap();
    }
    // A prepaid credit.
    db::insert_unit_receivable(
        &pool,
        "p1",
        "2026-05-31",
        "tPre",
        Some("Pre"),
        Some("prepaid"),
        0.0,
        Some(-300.0),
        None,
        "ar.csv",
        99,
    )
    .await
    .unwrap();

    let aging = db::delinquency_aging_for_period(&pool, "p1", "2026-05")
        .await
        .unwrap();

    assert_eq!(aging.current_owed_total, 200.0);
    assert_eq!(aging.cnt_current, 1);
    assert_eq!(aging.b0_30, 800.0);
    assert_eq!(aging.cnt_0_30, 1);
    assert_eq!(aging.b31_60, 1100.0);
    assert_eq!(aging.cnt_31_60, 1);
    assert_eq!(aging.b61_90, 1400.0);
    assert_eq!(aging.cnt_61_90, 1);
    // 90+ bucket = explicit 90+ row (2600) + NULL-days row (700).
    assert_eq!(aging.b90_plus, 3300.0);
    assert_eq!(aging.cnt_90_plus, 2);
    // Prepaid feeds its own cells, not the delinquency buckets.
    assert_eq!(aging.prepaid_total, 300.0);
    assert_eq!(aging.prepaid_cnt, 1);
    // delinquent_cnt() excludes cnt_current (days_late == 0): 1+1+1+2 = 5.
    assert_eq!(aging.delinquent_cnt(), 5);
    // delinquent_total() excludes the current-but-owed bucket (not yet truly late).
    assert_eq!(aging.delinquent_total(), 800.0 + 1100.0 + 1400.0 + 3300.0);
}

#[tokio::test]
async fn delinquency_trend_is_chronological() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    for (as_of, amt, row) in [
        ("2026-02-28", 50000.0, 1),
        ("2026-03-31", 42000.0, 2),
        ("2026-04-30", 38000.0, 3),
        ("2026-05-31", 31000.0, 4),
    ] {
        sqlx::query(
            "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at) \
             VALUES (?, 'p1', ?, ?, 10, 0.0, 'd.csv', ?, '2026-01-01')",
        )
        .bind(format!("ds{row}"))
        .bind(as_of)
        .bind(amt)
        .bind(row)
        .execute(&pool)
        .await
        .unwrap();
    }

    let trend = db::delinquency_trend(&pool, "p1", 3).await.unwrap();
    // last_n = 3 → newest three, returned oldest→newest.
    assert_eq!(trend.len(), 3);
    assert_eq!(trend[0].0, "2026-03-31");
    assert_eq!(trend[0].1, 42000.0);
    assert_eq!(trend[2].0, "2026-05-31");
    assert_eq!(trend[2].1, 31000.0);
}
