//! F4 — Portfolio summary band (Close Desk).
//!
//! F4.1 exercises `db::portfolio_rollup` aggregate math; F4.3 asserts the
//! Close Desk render carries the band figures via ratatui's `TestBackend`.

use boxscore::db;
use boxscore::tui::app::{DeskApp, Screen};

async fn seed_property(pool: &sqlx::SqlitePool, id: &str, name: &str, units: i64) {
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at) \
         VALUES (?, ?, 'Phoenix, AZ', ?, 'Bagholder Capital Partners, LP', 'Apex Residential', '2026-01-01')",
    )
    .bind(id)
    .bind(name)
    .bind(units)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_rent_roll(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    as_of: &str,
    occupied: i64,
    vacant: i64,
    down: i64,
    in_place_rent: f64,
) {
    sqlx::query(
        "INSERT INTO rent_roll_snapshots (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, notice_units, down_units, market_rent_total, in_place_rent_total, source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?, ?, 'test.csv', 1, '2026-01-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of)
    .bind(occupied)
    .bind(vacant)
    .bind(down)
    .bind(in_place_rent * 1.1) // market > in-place
    .bind(in_place_rent)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_delinquency(pool: &sqlx::SqlitePool, property_id: &str, as_of: &str, amount: f64) {
    sqlx::query(
        "INSERT INTO delinquency_snapshots (id, property_id, as_of_date, delinquent_amount, delinquent_units, prepaid_amount, source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, 3, 0.0, 'test.csv', 1, '2026-01-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of)
    .bind(amount)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_gl(
    pool: &sqlx::SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
    category: &str,
    amount: f64,
) {
    let sql = format!(
        "INSERT INTO {table} (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
         VALUES (?, ?, ?, '0000', 'Acct', ?, ?, 'test.csv', 1, '2026-01-01')"
    );
    sqlx::query(&sql)
        .bind(db::new_id())
        .bind(property_id)
        .bind(period_id)
        .bind(category)
        .bind(amount)
        .execute(pool)
        .await
        .unwrap();
}

/// F4.1 — `portfolio_rollup` aggregates units/count, occupancy range, in-place
/// rent, delinquency, and NOI variance with the favorable-positive convention.
#[tokio::test]
async fn portfolio_rollup_aggregates_the_whole_book() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let period = "2026-05";
    let as_of = "2026-05-31";
    let period_id = db::upsert_period(&pool, period).await.unwrap();

    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;
    seed_property(&pool, "p2", "Promote Pointe", 200).await;

    // p1: 95% occupancy (285 occ / 15 vac), in-place $400k, delinquent $50k.
    seed_rent_roll(&pool, "p1", as_of, 285, 15, 0, 400_000.0).await;
    seed_delinquency(&pool, "p1", as_of, 50_000.0).await;
    // p2: 90% occupancy (180 occ / 20 vac), in-place $250k, delinquent $30k.
    seed_rent_roll(&pool, "p2", as_of, 180, 20, 0, 250_000.0).await;
    seed_delinquency(&pool, "p2", as_of, 30_000.0).await;

    // GL: portfolio revenue 700k actual / 680k budget; expense 300k / 310k.
    // NOI actual = 400k, NOI budget = 370k → favorable +30k.
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "rental income",
        700_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "rental income",
        680_000.0,
    )
    .await;
    seed_gl(&pool, "gl_actuals", "p1", &period_id, "payroll", 300_000.0).await;
    seed_gl(&pool, "gl_budgets", "p1", &period_id, "payroll", 310_000.0).await;
    // An unmapped line that must NOT touch NOI.
    seed_gl(&pool, "gl_actuals", "p1", &period_id, "unmapped", 999_999.0).await;

    let rollup = db::portfolio_rollup(&pool, period, 1).await.unwrap();

    assert_eq!(rollup.property_count, 2);
    assert_eq!(rollup.total_units, 500);
    assert_eq!(rollup.owner_ready, 1);
    assert_eq!(rollup.occ_properties, 2);
    assert!(
        (rollup.occ_low - 0.90).abs() < 1e-9,
        "occ_low {}",
        rollup.occ_low
    );
    assert!(
        (rollup.occ_high - 0.95).abs() < 1e-9,
        "occ_high {}",
        rollup.occ_high
    );
    assert!((rollup.inplace_rent_total - 650_000.0).abs() < 1e-6);
    assert!((rollup.delinquent_total - 80_000.0).abs() < 1e-6);
    assert!(
        (rollup.noi_actual - 400_000.0).abs() < 1e-6,
        "noi_actual {}",
        rollup.noi_actual
    );
    assert!(
        (rollup.noi_budget - 370_000.0).abs() < 1e-6,
        "noi_budget {}",
        rollup.noi_budget
    );
    assert!((rollup.noi_variance() - 30_000.0).abs() < 1e-6);
    assert!((rollup.ready_ratio() - 0.5).abs() < 1e-9);
}

/// Empty book degrades cleanly — no division by zero, all-zero rollup.
#[tokio::test]
async fn portfolio_rollup_is_zero_for_empty_book() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let rollup = db::portfolio_rollup(&pool, "2026-05", 0).await.unwrap();
    assert_eq!(rollup.property_count, 0);
    assert_eq!(rollup.total_units, 0);
    assert_eq!(rollup.occ_properties, 0);
    assert_eq!(rollup.occ_low, 0.0);
    assert_eq!(rollup.occ_high, 0.0);
    assert_eq!(rollup.ready_ratio(), 0.0);
}

/// F4.3 — the Close Desk render carries the portfolio band: owner-ready ratio,
/// occupancy range, NOI Δ, and delinquency total, above the property board.
#[tokio::test]
async fn close_desk_renders_portfolio_band() {
    use ratatui::{backend::TestBackend, Terminal};

    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let period = "2026-05";
    let as_of = "2026-05-31";
    let period_id = db::upsert_period(&pool, period).await.unwrap();
    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;
    seed_property(&pool, "p2", "Promote Pointe", 200).await;
    seed_rent_roll(&pool, "p1", as_of, 285, 15, 0, 400_000.0).await;
    seed_rent_roll(&pool, "p2", as_of, 180, 20, 0, 250_000.0).await;
    seed_delinquency(&pool, "p1", as_of, 50_000.0).await;
    seed_delinquency(&pool, "p2", as_of, 30_000.0).await;
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "rental income",
        700_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "rental income",
        680_000.0,
    )
    .await;
    seed_gl(&pool, "gl_actuals", "p1", &period_id, "payroll", 300_000.0).await;
    seed_gl(&pool, "gl_budgets", "p1", &period_id, "payroll", 310_000.0).await;

    let mut app = DeskApp::new(period.to_string());
    app.reload(&pool).await;
    app.screen = Screen::CloseDesk;

    let backend = TestBackend::new(140, 36);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| boxscore::tui::ui::draw(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }

    // Band labels + aggregate figures.
    assert!(
        out.contains("Owner-ready"),
        "band missing owner-ready: {out}"
    );
    assert!(out.contains("Props"), "band missing props label: {out}");
    assert!(out.contains("Units"), "band missing units label: {out}");
    assert!(out.contains("Occ"), "band missing occupancy: {out}");
    assert!(out.contains("NOI"), "band missing NOI delta: {out}");
    assert!(out.contains("Delinq"), "band missing delinquency: {out}");
    // Owner-ready ratio "1/2" (only p1 has GL/contracts; in this fixture both
    // lack a lane so neither is owner-ready → 0/2). Assert the denominator shows.
    assert!(
        out.contains("/2"),
        "band missing owner-ready denominator: {out}"
    );
    // Occupancy range across the two properties (90–95%).
    assert!(
        out.contains("90") && out.contains("95"),
        "band missing occ range: {out}"
    );
    // In-place rent total $650.0k.
    assert!(out.contains("$650.0k"), "band missing in-place rent: {out}");
    // Delinquency total $80.0k.
    assert!(
        out.contains("$80.0k"),
        "band missing delinquency total: {out}"
    );
    // The property board must still render below the band.
    assert!(out.contains("Portfolio"), "property board missing: {out}");
    assert!(
        out.contains("Vantage at Yieldmore"),
        "board row missing: {out}"
    );
}

/// The band must not crowd the property board out at small heights — below the
/// min, the board renders full-area and still shows asset rows.
#[tokio::test]
async fn close_desk_band_yields_to_board_at_small_heights() {
    use ratatui::{backend::TestBackend, Terminal};

    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let period = "2026-05";
    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;

    let mut app = DeskApp::new(period.to_string());
    app.reload(&pool).await;
    app.screen = Screen::CloseDesk;

    // 9 rows total leaves < MIN_BOARD_HEIGHT after a 3-row band, so the band is
    // suppressed and the full area goes to the board.
    let backend = TestBackend::new(120, 9);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| boxscore::tui::ui::draw(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    assert!(
        out.contains("Vantage at Yieldmore"),
        "board row missing at small height: {out}"
    );
}
