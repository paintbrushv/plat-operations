use boxscore::{
    db,
    exact::{
        migration::{self, Review},
        store,
    },
};
use serde_json::Value;

#[tokio::test]
async fn reviewed_copy_preserves_original_reports_and_records_rounding_and_signs() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("legacy.sqlite");
    let pool = db::connect(&format!("sqlite://{}", source.display()))
        .await
        .unwrap();
    db::init_database(&pool).await.unwrap();
    let property = db::upsert_property(
        &pool,
        "Synthetic migration",
        "Synthetic",
        10,
        "Synthetic",
        "Synthetic",
    )
    .await
    .unwrap();
    let period = db::upsert_period(&pool, "2026-05").await.unwrap();
    for (table, code, category, amount) in [
        ("gl_actuals", "4000", "Rental Income", 100.005),
        ("gl_budgets", "4000", "Rental Income", 90.00),
        ("gl_actuals", "6000", "Repairs", -1.005),
        ("gl_budgets", "6000", "Repairs", 5.00),
    ] {
        sqlx::query(&format!("INSERT INTO {table} VALUES (?,?,?,?,?,?,?,?,?,?)"))
            .bind(format!("{table}-{code}"))
            .bind(&property)
            .bind(&period)
            .bind(code)
            .bind(code)
            .bind(category)
            .bind(amount)
            .bind("synthetic.csv")
            .bind(1i64)
            .bind("2026-05-31")
            .execute(&pool)
            .await
            .unwrap();
    }
    let task = db::create_task_run(&pool, "synthetic", "synthetic")
        .await
        .unwrap();
    let body = "Historical issued body: keep 100.005 exactly.\n";
    sqlx::query("INSERT INTO variance_report_artifacts VALUES (?,?,?,?,?,?,?,?)")
        .bind(&task)
        .bind(&property)
        .bind(&period)
        .bind("synthetic-original.md")
        .bind(body)
        .bind(100.005)
        .bind(90.0)
        .bind("2026-05-31")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let before = std::fs::read(&source).unwrap();
    let plan = migration::plan(&source).await.unwrap();
    assert!(plan["differences"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["kind"] == "rounding"));
    assert!(plan["differences"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["kind"] == "negative_expense"));
    let mut review = Review {
        contract_version: "plat.ops-migration-review/1".into(),
        source_sha256: plan["source_sha256"].as_str().unwrap().into(),
        reviewer: "synthetic:test".into(),
        reviewed_at: "2026-10-03".into(),
        rounding: "reject".into(),
        acknowledge_negative_expenses: false,
        acknowledge_snapshot_selection: false,
        expense_convention: "positive_costs".into(),
    };
    let destination = temp.path().join("reviewed-copy");
    assert!(migration::migrate(&source, &destination, &review)
        .await
        .is_err());
    assert!(!destination.exists());
    review.rounding = "half_away_from_zero".into();
    assert!(migration::migrate(&source, &destination, &review)
        .await
        .is_err());
    assert!(!destination.exists());
    review.acknowledge_negative_expenses = true;
    let result = migration::migrate(&source, &destination, &review)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(
        std::fs::read(destination.join("legacy.sqlite")).unwrap(),
        before
    );
    let cents = store::open(&destination.join("exact.sqlite"), true)
        .await
        .unwrap();
    let report = store::review(&cents, result["revisions"][0].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(report["variance"]["noi_bridge"]["actual_revenue"], "100.01");
    assert_eq!(report["variance"]["noi_bridge"]["actual_expenses"], "-1.01");
    assert_eq!(report["status"], "review_required");
    let archived: String = sqlx::query_scalar("SELECT original_body FROM archived_reports")
        .fetch_one(&cents)
        .await
        .unwrap();
    assert_eq!(archived, body);
    let differences: String = sqlx::query_scalar("SELECT differences_json FROM exact_migrations")
        .fetch_one(&cents)
        .await
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&differences)
            .unwrap()
            .as_array()
            .unwrap()
            .len()
            >= 3
    );
    assert!(migration::migrate(&source, &destination, &review)
        .await
        .is_err());
    review.source_sha256 = "0".repeat(64);
    assert!(
        migration::migrate(&source, &temp.path().join("stale"), &review)
            .await
            .is_err()
    );
    assert!(!temp.path().join("stale").exists());
}
