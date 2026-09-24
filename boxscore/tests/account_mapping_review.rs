use std::path::Path;

use boxscore::{
    account_review::{
        export_account_mapping_review, import_account_mapping_review, AccountReviewImportError,
    },
    connectors::standardized::{
        gl_budget_comparison::ingest_budget_comparison_file, source_registry::lane_by_key,
    },
    db,
};

#[tokio::test]
async fn export_review_csv_contains_unmapped_accounts_with_suggestions() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    ingest_unknown_and_suggested_accounts(&pool).await;
    let temp_dir = tempfile::tempdir().unwrap();
    let review_file = temp_dir.path().join("account_mapping_review.csv");

    let summary = export_account_mapping_review(&pool, &review_file)
        .await
        .unwrap();

    assert_eq!(summary.rows_exported, 2);
    let csv = std::fs::read_to_string(review_file).unwrap();
    assert!(csv.contains("source_system,property_scope,account_code"));
    assert!(csv.contains("9999"));
    assert!(csv.contains("Mystery Clearing"));
    assert!(csv.contains("5200"));
    assert!(csv.contains("Repairs & Maintenance"));
    assert!(csv.contains("reviewed_category"));
}

#[tokio::test]
async fn import_review_csv_approves_mappings_and_records_memories() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let review_file = temp_dir.path().join("account_mapping_review.csv");
    std::fs::write(
        &review_file,
        "source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes\nstandardized-yardi,p101,9999,Mystery Clearing,Unmapped,,0.0,approved,Other Income,operator reviewed\nstandardized-yardi,p101,8888,Ignored Account,Unmapped,,0.0,pending,,not ready\n",
    )
    .unwrap();

    let summary = import_account_mapping_review(&pool, &review_file)
        .await
        .unwrap();

    assert_eq!(summary.rows_seen, 2);
    assert_eq!(summary.rows_imported, 1);
    let mapping = db::find_account_mapping(&pool, "standardized-yardi", "p101", "9999")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.noi_category, "Other Income");
    assert_eq!(mapping.status, "approved");
    assert_eq!(db::list_memories(&pool).await.unwrap().len(), 1);
    assert!(
        db::find_account_mapping(&pool, "standardized-yardi", "p101", "8888")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn import_review_rejects_approved_rows_missing_reviewed_category() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let review_file = temp_dir.path().join("account_mapping_review.csv");
    std::fs::write(
        &review_file,
        "source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes\nstandardized-yardi,p101,9999,Mystery Clearing,Unmapped,,0.0,approved,,missing category\n",
    )
    .unwrap();

    let err = import_account_mapping_review(&pool, &review_file)
        .await
        .unwrap_err();

    assert!(matches!(
        err.downcast_ref::<AccountReviewImportError>(),
        Some(AccountReviewImportError::ApprovedMissingReviewedCategory { source_row: 2 })
    ));
}

#[tokio::test]
async fn approved_review_mapping_reduces_unmapped_rows_after_reingest() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    ingest_unknown_and_suggested_accounts(&pool).await;
    assert_eq!(db::list_unmapped_accounts(&pool).await.unwrap().len(), 1);
    let temp_dir = tempfile::tempdir().unwrap();
    let review_file = temp_dir.path().join("account_mapping_review.csv");
    std::fs::write(
        &review_file,
        "source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes\nstandardized-yardi,p101,9999,Mystery Clearing,Unmapped,,0.0,approved,Other Income,operator reviewed\n",
    )
    .unwrap();
    import_account_mapping_review(&pool, &review_file)
        .await
        .unwrap();
    ingest_unknown_and_suggested_accounts(&pool).await;

    let unmapped = db::list_unmapped_accounts(&pool).await.unwrap();
    assert!(unmapped
        .iter()
        .all(|account| account.account_code != "9999"));
}

#[tokio::test]
async fn approving_a_mapping_for_one_property_does_not_reclassify_other_properties() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    // Same unmapped account code at two different properties.
    ingest_unmapped_account_for_lane(&pool, "maplewood", "p101").await;
    ingest_unmapped_account_for_lane(&pool, "juniper_fund", "juniper_fund").await;

    let temp_dir = tempfile::tempdir().unwrap();
    let review_file = temp_dir.path().join("account_mapping_review.csv");
    std::fs::write(
        &review_file,
        "source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes\nstandardized-yardi,p101,9999,Mystery Clearing,Unmapped,,0.0,approved,Other Income,maplewood only\n",
    )
    .unwrap();
    import_account_mapping_review(&pool, &review_file)
        .await
        .unwrap();

    let categories: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.name, g.category FROM gl_actuals g JOIN properties p ON p.id = g.property_id WHERE g.account_code = '9999'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let maplewood = categories
        .iter()
        .find(|(name, _)| name == "Maplewood Commons")
        .unwrap();
    let juniper_fund = categories
        .iter()
        .find(|(name, _)| name == "Juniper Fund")
        .unwrap();
    assert_eq!(maplewood.1, "Other Income");
    assert_eq!(
        juniper_fund.1, "Unmapped",
        "an approval scoped to p101 must not touch juniper_fund GL rows"
    );
}

async fn ingest_unmapped_account_for_lane(pool: &sqlx::SqlitePool, lane_key: &str, scope: &str) {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        format!(
            "account_code,description,ptd_actual,ptd_budget,period,property_id\n9999,Mystery Clearing,100,75,Jun 2026,{scope}\n"
        ),
    )
    .unwrap();
    let lane = lane_by_key(lane_key).unwrap();
    ingest_budget_comparison_file(pool, &lane, Path::new(&file))
        .await
        .unwrap();
}

async fn ingest_unknown_and_suggested_accounts(pool: &sqlx::SqlitePool) {
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n9999,Mystery Clearing,100,75,Jun 2026,p101\n5200,Repairs & Maintenance,-50,-40,Jun 2026,p101\n",
    )
    .unwrap();
    let lane = lane_by_key("maplewood").unwrap();
    ingest_budget_comparison_file(pool, &lane, Path::new(&file))
        .await
        .unwrap();
}
