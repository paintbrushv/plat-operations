//! F3 — Renewals / Lease-Expiration screen tests (DB helpers + RPCOE reader).

use std::path::PathBuf;

use boxscore::db;
use boxscore::db::UnitLeaseRow;
use boxscore::tui::app::{apply_window_filter, DeskApp};
use boxscore::tui::rpcoe;
use boxscore::tui::rpcoe::RentRec;

fn rec(unit: &str, days: Option<i64>) -> RentRec {
    RentRec {
        lane: "L".into(),
        unit: unit.into(),
        resident_code: "t".into(),
        resident_name: "R".into(),
        current_rent: 1000.0,
        market_rent: 1000.0,
        rent_vs_market_pct: 100.0,
        days_to_expiration: days,
        lease_expiration: String::new(),
        recommended_new_rent: 1000.0,
        recommended_increase_pct: 0.0,
        confidence: "High".into(),
        concession: String::new(),
        top_driver: String::new(),
    }
}

fn lease(unit: &str, market: f64, charge: f64) -> UnitLeaseRow {
    UnitLeaseRow {
        unit_label: unit.to_string(),
        resident_code: Some("t1".to_string()),
        resident_name: Some("Resident".to_string()),
        market_rent: Some(market),
        charge_rent: Some(charge),
        as_of_date: "2026-06-30".to_string(),
    }
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
async fn list_leases_returns_biggest_upside_first() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    // Spread (market - charge): A=+200, B=+50, C=-100 (over market).
    db::insert_unit_lease(
        &pool,
        "p1",
        "2026-06-30",
        "U-101",
        Some("tA"),
        Some("Big Upside"),
        Some(1700.0),
        Some(1500.0),
        "rr.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_lease(
        &pool,
        "p1",
        "2026-06-30",
        "U-102",
        Some("tB"),
        Some("Small Upside"),
        Some(1550.0),
        Some(1500.0),
        "rr.csv",
        3,
    )
    .await
    .unwrap();
    db::insert_unit_lease(
        &pool,
        "p1",
        "2026-06-30",
        "U-103",
        Some("tC"),
        Some("Over Market"),
        Some(1400.0),
        Some(1500.0),
        "rr.csv",
        4,
    )
    .await
    .unwrap();

    let rows = db::list_leases_for_period(&pool, "p1", "2026-06")
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    // Biggest upside first.
    assert_eq!(rows[0].unit_label, "U-101");
    assert_eq!(rows[1].unit_label, "U-102");
    assert_eq!(rows[2].unit_label, "U-103");
}

#[tokio::test]
async fn list_leases_uses_latest_snapshot_per_property() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    seed_property(&pool, "p1", "Vantage").await;

    db::insert_unit_lease(
        &pool,
        "p1",
        "2026-06-15",
        "U-STALE",
        Some("tStale"),
        Some("Stale"),
        Some(1600.0),
        Some(1500.0),
        "rr.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_lease(
        &pool,
        "p1",
        "2026-06-30",
        "U-FRESH",
        Some("tFresh"),
        Some("Fresh"),
        Some(1600.0),
        Some(1500.0),
        "rr.csv",
        3,
    )
    .await
    .unwrap();

    let rows = db::list_leases_for_period(&pool, "p1", "2026-06")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].unit_label, "U-FRESH");
}

#[test]
fn lease_opportunity_sums_underpriced_only() {
    // U-1 underpriced (<0.95×market): market 2000, charge 1700 → +300, counts.
    // U-2 near market (charge 1960 > 0.95×2000=1900) → excluded.
    // U-3 underpriced: market 1500, charge 1000 → +500, counts.
    // U-4 over market: market 1400, charge 1500 → excluded.
    let rows = vec![
        lease("U-1", 2000.0, 1700.0),
        lease("U-2", 2000.0, 1960.0),
        lease("U-3", 1500.0, 1000.0),
        lease("U-4", 1400.0, 1500.0),
    ];
    let (monthly, annual, underpriced) = db::lease_opportunity(&rows);
    assert_eq!(underpriced, 2);
    assert_eq!(monthly, 800.0); // 300 + 500
    assert_eq!(annual, 9600.0); // ×12
}

#[test]
fn lease_opportunity_handles_missing_rents() {
    let mut row = lease("U-X", 0.0, 0.0);
    row.market_rent = None;
    let rows = vec![row, lease("U-Y", 0.0, 0.0)];
    let (monthly, annual, underpriced) = db::lease_opportunity(&rows);
    assert_eq!(monthly, 0.0);
    assert_eq!(annual, 0.0);
    assert_eq!(underpriced, 0);
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("rpcoe")
        .join(name)
}

#[test]
fn rpcoe_parses_recommendations_and_filters_placeholder_drivers() {
    let recs = rpcoe::parse_recommendations("maplewood", &fixture("rpcoe_recommendations.csv"));
    assert_eq!(recs.len(), 2);
    // Row C00107: float day count parsed to whole days.
    let r = recs.iter().find(|r| r.unit == "C00107").unwrap();
    assert_eq!(r.days_to_expiration, Some(69));
    assert_eq!(r.lease_expiration, "2026-08-17");
    assert_eq!(r.recommended_new_rent, 1079.0);
    assert_eq!(r.recommended_increase_pct, 6.5);
    assert_eq!(r.confidence, "Medium");
    // driver_1 is the human driver; driver_2 is a PLACEHOLDER and is skipped.
    assert_eq!(r.top_driver, "Frequent late payments - higher risk");
    assert!(!r.top_driver.contains("PLACEHOLDER"));
}

#[test]
fn rpcoe_sorts_urgent_first_and_buckets_windows() {
    let recs = rpcoe::parse_recommendations("maplewood", &fixture("rpcoe_recommendations.csv"));
    // expiration_windows buckets by days_to_expiration: 48 → 31-60, 69 → 61-90.
    let windows = rpcoe::expiration_windows(&recs);
    assert_eq!(windows, [0, 1, 1, 0]);
}

#[test]
fn rpcoe_missing_file_is_empty_not_a_panic() {
    let recs = rpcoe::parse_recommendations("Ghost", &PathBuf::from("/tmp/nope-xyz-rpcoe.csv"));
    assert!(recs.is_empty());
}

#[test]
fn window_filter_shrinks_list_to_chosen_window() {
    let recs = vec![
        rec("U-15", Some(15)),   // ≤30
        rec("U-45", Some(45)),   // 31-60
        rec("U-80", Some(80)),   // 61-90
        rec("U-120", Some(120)), // 90+
    ];
    // No filter → all four.
    assert_eq!(apply_window_filter(recs.clone(), None).len(), 4);
    // ≤30 window → only U-15.
    let w0 = apply_window_filter(recs.clone(), Some(0));
    assert_eq!(w0.len(), 1);
    assert_eq!(w0[0].unit, "U-15");
    // 31-60 window → only U-45.
    let w1 = apply_window_filter(recs.clone(), Some(1));
    assert_eq!(w1.len(), 1);
    assert_eq!(w1[0].unit, "U-45");
    // 90+ window → only U-120.
    assert_eq!(apply_window_filter(recs, Some(3)).len(), 1);
}

#[test]
fn renew_cycle_window_filter_advances_and_wraps() {
    let mut app = DeskApp::new("2026-06".to_string());
    assert_eq!(app.renew_window_filter, None);
    app.renew_cycle_window_filter();
    assert_eq!(app.renew_window_filter, Some(0));
    app.renew_cycle_window_filter();
    assert_eq!(app.renew_window_filter, Some(1));
    app.renew_cycle_window_filter();
    app.renew_cycle_window_filter();
    assert_eq!(app.renew_window_filter, Some(3));
    // Wraps back to None (all windows).
    app.renew_cycle_window_filter();
    assert_eq!(app.renew_window_filter, None);
}
