use boxscore::exact::{
    money::Money,
    store::{self, Dataset, Snapshot},
    variance::Line,
};
use sqlx::Row;

fn dataset() -> Dataset {
    Dataset {
        property: "Synthetic Oak".into(),
        period: "2026-05".into(),
        currency: "USD".into(),
        expense_convention: "positive_costs".into(),
        unit_count: 10,
        actuals: vec![Line {
            account_code: "4000".into(),
            account_name: "Rent".into(),
            category: "rental income".into(),
            amount: "100.10".parse().unwrap(),
        }],
        budgets: vec![Line {
            account_code: "4000".into(),
            account_name: "Rent".into(),
            category: "rental income".into(),
            amount: "90.05".parse().unwrap(),
        }],
        snapshot: Some(Snapshot {
            as_of_date: "2026-05-31".into(),
            occupied_units: 9,
            vacant_units: 1,
            down_units: 0,
            market_rent_total: Some("100.10".parse().unwrap()),
            in_place_rent_total: None,
            delinquent_amount: None,
            prepaid_amount: None,
            concessions_amount: None,
        }),
    }
}

#[tokio::test]
async fn import_persists_cents_and_issues_immutable_correction_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("cents.sqlite");
    let pool = store::create(&path).await.unwrap();
    let data = dataset();
    let first = store::import(
        &pool,
        &data,
        None,
        None,
        &serde_json::json!({"synthetic":"hash"}),
    )
    .await
    .unwrap();
    let cell = sqlx::query(
        "SELECT amount_cents, typeof(amount_cents) AS kind FROM exact_gl WHERE kind='actual'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cell.get::<i64, _>("amount_cents"), 10010);
    assert_eq!(cell.get::<String, _>("kind"), "integer");
    let report = store::issue(&pool, &first).await.unwrap();
    let original = serde_json::to_string(&report).unwrap();
    let repeated = store::issue(&pool, &first).await.unwrap();
    assert_ne!(report["report_id"], repeated["report_id"]);
    let mut correction = data.clone();
    correction.actuals[0].amount = "100.12".parse::<Money>().unwrap();
    let second = store::import(
        &pool,
        &correction,
        Some(&first),
        Some("Synthetic two-cent correction"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let corrected = store::issue(&pool, &second).await.unwrap();
    assert_eq!(corrected["changes"]["actual_noi"], "0.02");
    assert_eq!(corrected["supersedes_revision"], first);
    let stored: String = sqlx::query_scalar("SELECT body FROM exact_reports WHERE id=?")
        .bind(report["report_id"].as_str().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, original);
    assert!(sqlx::query("UPDATE exact_gl SET amount_cents=0")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("DELETE FROM exact_reports")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("INSERT INTO gl_actuals (amount) VALUES (1.0)")
        .execute(&pool)
        .await
        .is_err());
    assert!(store::import(
        &pool,
        &data,
        Some(&first),
        Some("stale correction"),
        &serde_json::json!({})
    )
    .await
    .is_err());
    assert!(store::create(&path).await.is_err());
    pool.close().await;
    assert!(
        boxscore::db::connect(&format!("sqlite://{}", path.display()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalid_snapshot_and_excess_precision_never_create_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.snapshot.as_mut().unwrap().occupied_units = 100;
    assert!(
        store::import(&pool, &data, None, None, &serde_json::json!({}))
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM exact_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let raw = serde_json::to_string(&dataset())
        .unwrap()
        .replace("100.10", "100.101");
    assert!(serde_json::from_str::<Dataset>(&raw).is_err());
}

#[tokio::test]
async fn missing_budget_is_excluded_and_overflow_import_is_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.budgets.clear();
    let revision = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    let report = store::review(&pool, &revision).await.unwrap();
    assert_eq!(report["variance"]["noi_bridge"]["actual_revenue"], "0.00");
    assert_eq!(report["excluded_accounts"][0]["line"]["amount"], "100.10");
    assert_eq!(report["status"], "review_required");
    data.actuals[0].amount = "92233720368547758.07".parse().unwrap();
    data.actuals.push(Line {
        amount: "0.01".parse().unwrap(),
        ..data.actuals[0].clone()
    });
    assert!(store::import(
        &pool,
        &data,
        Some(&revision),
        Some("overflow"),
        &serde_json::json!({})
    )
    .await
    .is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM exact_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn coverage_uses_account_codes_and_preserves_complete_totals() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.budgets[0].account_name = "Different budget label".into();
    data.budgets[0].category = " Rental Income ".into();
    let id = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    let report = store::review(&pool, &id).await.unwrap();
    assert_eq!(report["variance"]["noi_bridge"]["noi_variance"], "10.05");
    assert!(report["excluded_accounts"].as_array().unwrap().is_empty());
    assert_eq!(
        report["variance"]["by_account"].as_array().unwrap().len(),
        1
    );
    data.budgets[0].category = "repairs".into();
    assert_eq!(
        store::import(
            &pool,
            &data,
            Some(&id),
            Some("conflicting map"),
            &serde_json::json!({})
        )
        .await
        .unwrap_err()
        .code,
        "ACCOUNT_MAPPING_CONFLICT"
    );
}
