use std::path::Path;

use boxscore::{
    connectors::standardized::{
        account_mapping::{suggest_category, SuggestedMapping},
        gl_budget_comparison::{parse_period_label, read_budget_comparison_rows},
    },
    db,
};

#[test]
fn parses_month_name_period_labels() {
    assert_eq!(parse_period_label("Mar 2026").unwrap(), "2026-03");
    assert_eq!(parse_period_label("December 2025").unwrap(), "2025-12");
    assert!(parse_period_label("2026 March").is_err());
}

#[test]
fn two_digit_years_are_normalized_not_stored_as_year_26() {
    assert_eq!(parse_period_label("Mar 26").unwrap(), "2026-03");
}

#[test]
fn accounting_zero_budgets_stay_zero_and_empty_budgets_mean_missing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n4000,Rental Income,\"$1,000.00\",(),Mar 2026,p101\n5200,Repairs,(250.00),-,Mar 2026,p101\n6100,Salaries,500.00,,Apr 2026,p101\n",
    )
    .unwrap();

    let parsed = read_budget_comparison_rows(&file).unwrap();

    assert_eq!(parsed.rows.len(), 3);
    assert_eq!(parsed.skipped.len(), 0);
    // "()" and "-" are accounting zeros: real budget rows of 0.
    assert_eq!(parsed.rows[0].ptd_actual, 1000.0);
    assert_eq!(parsed.rows[0].ptd_budget, Some(0.0));
    assert_eq!(parsed.rows[1].ptd_actual, -250.0);
    assert_eq!(parsed.rows[1].ptd_budget, Some(0.0));
    // An empty cell means no budget exists (12-month statement adapter rows).
    assert_eq!(parsed.rows[2].ptd_budget, None);
}

#[tokio::test]
async fn missing_budget_rows_do_not_create_gl_budget_entries() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n4000,Rental Income,1000,,Apr 2026,p101\n",
    )
    .unwrap();
    let lane =
        boxscore::connectors::standardized::source_registry::lane_by_key("maplewood").unwrap();

    ingest_file_for_lane(&pool, &lane, &file).await;

    let actuals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_actuals")
        .fetch_one(&pool)
        .await
        .unwrap();
    let budgets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_budgets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(actuals, 1);
    assert_eq!(
        budgets, 0,
        "a missing budget must not fake a zero gl_budgets row"
    );
}

async fn ingest_file_for_lane(
    pool: &sqlx::SqlitePool,
    lane: &boxscore::connectors::standardized::source_registry::PropertyLane,
    file: &std::path::Path,
) {
    boxscore::connectors::standardized::gl_budget_comparison::ingest_budget_comparison_file(
        pool, lane, file,
    )
    .await
    .unwrap();
}

#[test]
fn account_mapping_prefers_clear_ontology_patterns() {
    assert_eq!(
        suggest_category("4000", "Apartment Rent").unwrap(),
        SuggestedMapping {
            category: "Rental Income".to_string(),
            confidence_score: 0.80,
            reason: "Matched rent income pattern".to_string(),
        }
    );
    assert_eq!(
        suggest_category("5200", "Repairs & Maintenance")
            .unwrap()
            .category,
        "Repairs & Maintenance"
    );
    assert!(suggest_category("9999", "Mystery Clearing").is_none());
}

#[test]
fn suggestions_match_on_word_boundaries_and_never_turn_expenses_into_income() {
    let category = |code: &str, name: &str| suggest_category(code, name).map(|m| m.category);
    // "fee" used to win before "management fee", classifying the expense as
    // revenue and overstating NOI by twice the fee.
    for name in [
        "Management Fees",
        "Property Management Fee",
        "Mgmt Fee - Base",
    ] {
        assert_eq!(
            category("6300", name).as_deref(),
            Some("Management Fees"),
            "{name}"
        );
    }
    // "rent" must not match inside "current".
    for name in ["Current Year Distributions", "Current Owner Draw"] {
        assert_eq!(category("3900", name), None, "{name}");
    }
    assert_eq!(
        category("4000", "Apartment Rent").as_deref(),
        Some("Rental Income")
    );
    assert_eq!(
        category("4010", "Rental Income - Residential").as_deref(),
        Some("Rental Income")
    );
    assert_eq!(
        category("4200", "Pet Rent").as_deref(),
        Some("Other Income")
    );
    assert_eq!(
        category("4210", "Late Fees").as_deref(),
        Some("Other Income")
    );
    assert_eq!(category("5300", "Utilities").as_deref(), Some("Utilities"));
    assert_eq!(
        category("5050", "Salaries - Leasing").as_deref(),
        Some("Payroll")
    );
    // A chart-of-accounts prefix is not evidence: no name match means review.
    assert_eq!(category("6010", "Something Else"), None);
    assert_eq!(category("6020", "Property Taxes").as_deref(), Some("Taxes"));
}

#[test]
fn reads_budget_comparison_rows_and_skips_bad_numeric_values() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n4000,Rental Income,1000,900,Mar 2026,p101\n9999,Mystery Clearing,not-a-number,10,Mar 2026,p101\n",
    )
    .unwrap();

    let parsed = read_budget_comparison_rows(&file).unwrap();

    assert_eq!(parsed.rows.len(), 1);
    assert_eq!(parsed.rows[0].period, "2026-03");
    assert_eq!(parsed.rows[0].ptd_actual, 1000.0);
    assert_eq!(parsed.rows[0].source_row, 2);
    assert_eq!(parsed.skipped.len(), 1);
    assert_eq!(parsed.skipped[0].source_row, 3);
}

#[tokio::test]
async fn approved_account_mapping_overrides_pattern_suggestion() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    db::upsert_account_mapping(
        &pool,
        db::NewAccountMapping {
            source_system: "standardized-yardi",
            property_scope: "p101",
            account_code: "4000",
            account_name: "Apartment Rent",
            noi_category: "Other Income",
            confidence_score: 0.99,
            status: "approved",
        },
    )
    .await
    .unwrap();

    let mapping = db::find_account_mapping(&pool, "standardized-yardi", "p101", "4000")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(mapping.noi_category, "Other Income");
    assert_eq!(mapping.status, "approved");
}

#[tokio::test]
async fn standardized_gl_ingest_creates_unmapped_gap_for_unknown_accounts() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n9999,Mystery Clearing,100,75,Mar 2026,p101\n",
    )
    .unwrap();

    let lane =
        boxscore::connectors::standardized::source_registry::lane_by_key("maplewood").unwrap();
    let summary =
        boxscore::connectors::standardized::gl_budget_comparison::ingest_budget_comparison_file(
            &pool,
            &lane,
            Path::new(&file),
        )
        .await
        .unwrap();

    assert_eq!(summary.rows_seen, 1);
    assert_eq!(summary.rows_inserted, 2);
    assert_eq!(summary.gaps_created, 1);
    assert_eq!(
        db::list_gaps(&pool).await.unwrap()[0].gap_type,
        "missing_account_mapping"
    );
    assert!(db::list_questions(&pool).await.unwrap()[0]
        .question
        .contains("9999"));
}

#[test]
fn subtotal_rows_are_skipped_to_prevent_double_counting() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n5012-0010,Market Rent,1000,900,Mar 2026,p102\n5012-0150,TOTAL GROSS POTENTIAL RENT,1000,900,Mar 2026,p102\n5012-9999,NET RENTAL INCOME,1000,900,Mar 2026,p102\n71990,Total Repairs & Maintenance,500,450,Mar 2026,p102\n",
    )
    .unwrap();

    let parsed = read_budget_comparison_rows(&file).unwrap();

    assert_eq!(parsed.rows.len(), 1, "only the component account survives");
    assert_eq!(parsed.rows[0].description, "Market Rent");
    assert_eq!(parsed.skipped.len(), 3);
    assert!(parsed
        .skipped
        .iter()
        .all(|row| row.reason.contains("subtotal")));
}

#[tokio::test]
async fn reingesting_the_same_gl_file_does_not_duplicate_rows() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n4000,Rental Income,1000,900,Mar 2026,p101\n",
    )
    .unwrap();
    let lane =
        boxscore::connectors::standardized::source_registry::lane_by_key("maplewood").unwrap();

    for _ in 0..2 {
        boxscore::connectors::standardized::gl_budget_comparison::ingest_budget_comparison_file(
            &pool,
            &lane,
            Path::new(&file),
        )
        .await
        .unwrap();
    }

    let actual_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_actuals")
        .fetch_one(&pool)
        .await
        .unwrap();
    let budget_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_budgets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(actual_count, 1, "re-ingest must replace, not duplicate");
    assert_eq!(budget_count, 1, "re-ingest must replace, not duplicate");
}
