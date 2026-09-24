//! F5 — Visual NOI bridge.
//!
//! The single worst bug this feature can ship is a flipped sign: a waterfall
//! that reconciles to the right ending NOI numerically but tells an LP the
//! opposite story. These tests pin the favorable/unfavorable convention on
//! BOTH a revenue category and an expense category, exactly mirroring
//! `variance.rs::noi_impact` / `ontology::account_class`.

use boxscore::db::{self, CategoryVariance};

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

fn find<'a>(rows: &'a [CategoryVariance], category: &str) -> &'a CategoryVariance {
    rows.iter()
        .find(|r| r.category == category)
        .unwrap_or_else(|| panic!("category {category} missing from {rows:?}"))
}

/// F5.1 — the sign convention. Seed a revenue category UNDER budget (must read
/// UNFAVORABLE = negative) and an expense category OVER budget (must also read
/// UNFAVORABLE = negative). This is the assertion the whole feature hinges on.
#[tokio::test]
async fn category_variance_signs_favorable_correctly_for_revenue_and_expense() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let period = "2026-05";
    let period_id = db::upsert_period(&pool, period).await.unwrap();
    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;

    // Rental Income UNDER budget: actual 90k vs budget 100k → unfavorable.
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Rental Income",
        90_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "Rental Income",
        100_000.0,
    )
    .await;
    // R&M OVER budget: actual 30k vs budget 20k → unfavorable (cost overrun).
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Repairs & Maintenance",
        30_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "Repairs & Maintenance",
        20_000.0,
    )
    .await;

    let rows = db::category_variance_for_period(&pool, "p1", period)
        .await
        .unwrap();

    // Revenue under budget is UNFAVORABLE → variance must be NEGATIVE.
    let rev = find(&rows, "Rental Income");
    assert!(
        rev.variance < 0.0,
        "revenue under budget must be unfavorable (negative), got {}",
        rev.variance
    );
    assert!(
        (rev.variance - (-10_000.0)).abs() < 1e-6,
        "rev variance {}",
        rev.variance
    );

    // Expense over budget is UNFAVORABLE → variance must be NEGATIVE.
    let rm = find(&rows, "Repairs & Maintenance");
    assert!(
        rm.variance < 0.0,
        "expense over budget must be unfavorable (negative), got {}",
        rm.variance
    );
    assert!(
        (rm.variance - (-10_000.0)).abs() < 1e-6,
        "rm variance {}",
        rm.variance
    );
}

/// The favorable side: revenue OVER budget and expense UNDER budget must both
/// read FAVORABLE = positive.
#[tokio::test]
async fn category_variance_signs_favorable_for_revenue_over_and_expense_under() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let period = "2026-05";
    let period_id = db::upsert_period(&pool, period).await.unwrap();
    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;

    // Rental Income OVER budget → favorable.
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Rental Income",
        110_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "Rental Income",
        100_000.0,
    )
    .await;
    // R&M UNDER budget → favorable (cost savings).
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Repairs & Maintenance",
        15_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "Repairs & Maintenance",
        20_000.0,
    )
    .await;

    let rows = db::category_variance_for_period(&pool, "p1", period)
        .await
        .unwrap();

    let rev = find(&rows, "Rental Income");
    assert!(
        rev.variance > 0.0,
        "revenue over budget must be favorable, got {}",
        rev.variance
    );
    assert!((rev.variance - 10_000.0).abs() < 1e-6);

    let rm = find(&rows, "Repairs & Maintenance");
    assert!(
        rm.variance > 0.0,
        "expense under budget must be favorable, got {}",
        rm.variance
    );
    assert!((rm.variance - 5_000.0).abs() < 1e-6);
}

/// Unmapped categories never reach the bridge — a balance-sheet-sized unmapped
/// row must be dropped, not distort the waterfall.
#[tokio::test]
async fn category_variance_excludes_unmapped_and_orders_by_magnitude() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let period = "2026-05";
    let period_id = db::upsert_period(&pool, period).await.unwrap();
    seed_property(&pool, "p1", "Vantage at Yieldmore", 300).await;

    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Rental Income",
        95_000.0,
    )
    .await;
    seed_gl(
        &pool,
        "gl_budgets",
        "p1",
        &period_id,
        "Rental Income",
        100_000.0,
    )
    .await; // |5k|
    seed_gl(&pool, "gl_actuals", "p1", &period_id, "Payroll", 60_000.0).await;
    seed_gl(&pool, "gl_budgets", "p1", &period_id, "Payroll", 20_000.0).await; // |40k|
    seed_gl(
        &pool,
        "gl_actuals",
        "p1",
        &period_id,
        "Unmapped",
        9_000_000.0,
    )
    .await;

    let rows = db::category_variance_for_period(&pool, "p1", period)
        .await
        .unwrap();

    assert!(
        rows.iter().all(|r| r.category != "Unmapped"),
        "unmapped leaked into bridge"
    );
    assert_eq!(rows.len(), 2);
    // Biggest swing first: Payroll (|40k|) before Rental Income (|5k|).
    assert_eq!(rows[0].category, "Payroll");
    assert_eq!(rows[1].category, "Rental Income");
}
