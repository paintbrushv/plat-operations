//! `close_readiness@1.0.0` over the exact-cent store: one answer per property
//! for a period, from the latest revision. Every test goes through
//! `catalog::invoke` against a temporary database. Synthetic data only.

use boxscore::exact::{
    catalog::{self, Call, Envelope},
    store::{self, Dataset},
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};

fn line(code: &str, category: &str, amount: &str) -> Value {
    json!({"account_code": code, "account_name": code, "category": category, "amount": amount})
}

fn full_snapshot(period: &str) -> Value {
    json!({
        "as_of_date": format!("{period}-28"),
        "occupied_units": 9, "vacant_units": 1, "down_units": 0,
        "market_rent_total": "100.10", "in_place_rent_total": "95.00",
        "delinquent_amount": null, "prepaid_amount": null, "concessions_amount": null
    })
}

/// Two matched accounts and a snapshot: nothing for `store::review` to flag.
fn full(property: &str, period: &str, rent: &str) -> Value {
    json!({
        "property": property, "period": period,
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [line("4000", "rental income", rent), line("5000", "payroll", "30.00")],
        "budgets": [line("4000", "rental income", "90.05"), line("5000", "payroll", "25.00")],
        "snapshot": full_snapshot(period)
    })
}

fn call(input: Value, database: Option<&Path>) -> Call {
    Call {
        function: "close_readiness".into(),
        version: None,
        input,
        database: database.map(Path::to_path_buf),
        caller: "test".into(),
    }
}

async fn run(input: Value, database: &Path) -> Envelope {
    catalog::invoke(&call(input, Some(database))).await
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    pool: SqlitePool,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.sqlite");
        let pool = store::create(&path).await.unwrap();
        Fixture {
            _dir: dir,
            path,
            pool,
        }
    }

    async fn import(&self, dataset: Value) -> String {
        let data: Dataset = serde_json::from_value(dataset).unwrap();
        store::import(&self.pool, &data, None, None, &json!({"synthetic": "hash"}))
            .await
            .unwrap()
    }

    async fn correct(&self, dataset: Value, parent: &str, reason: &str) -> String {
        let data: Dataset = serde_json::from_value(dataset).unwrap();
        store::import(
            &self.pool,
            &data,
            Some(parent),
            Some(reason),
            &json!({"synthetic": "hash"}),
        )
        .await
        .unwrap()
    }

    async fn issue(&self, revision: &str) -> String {
        store::issue(&self.pool, revision).await.unwrap()["report_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn result(envelope: &Envelope) -> &Value {
    envelope
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("refused: {:?}", envelope.error))
}

fn code(envelope: &Envelope) -> &str {
    assert!(
        envelope.refused(),
        "expected a refusal: {:?}",
        envelope.result
    );
    assert!(envelope.result.is_none() && envelope.output_sha256.is_none());
    envelope.error.as_ref().unwrap()["code"].as_str().unwrap()
}

fn only_property(envelope: &Envelope) -> &Value {
    let properties = result(envelope)["properties"].as_array().unwrap();
    assert_eq!(properties.len(), 1, "{properties:?}");
    &properties[0]
}

#[tokio::test]
async fn ready_when_complete_and_a_report_has_been_issued() {
    let fx = Fixture::new().await;
    let revision = fx.import(full("Synthetic Oak", "2026-05", "100.10")).await;
    let report = fx.issue(&revision).await;

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    assert_eq!(envelope.function, "close_readiness");
    assert_eq!(envelope.version, Some("1.0.0"));
    assert!(envelope.output_sha256.is_some());
    let body = result(&envelope);
    assert_eq!(body["period"], "2026-05");
    assert_eq!(body["review_reasons"], json!([]));
    assert_eq!(
        body["summary"],
        json!({"ready": 1, "not_ready": 0, "no_revisions": 0})
    );

    let entry = only_property(&envelope);
    assert_eq!(entry["property"], "Synthetic Oak");
    assert_eq!(entry["revision_id"], revision.as_str());
    assert_eq!(entry["revision"], 1);
    assert_eq!(entry["is_correction"], false);
    assert_eq!(entry["correction_reason"], Value::Null);
    assert_eq!(entry["actuals_present"], true);
    assert_eq!(entry["budgets_present"], true);
    assert_eq!(entry["snapshot"], "present");
    assert_eq!(
        entry["account_coverage"],
        json!({"matched": 2, "actual_only": 0, "budget_only": 0})
    );
    assert_eq!(entry["blocking_review_reasons"], json!([]));
    let reports = entry["issued_reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["id"], report.as_str());
    assert!(!reports[0]["created_at"].as_str().unwrap().is_empty());
    assert_eq!(entry["status"], "ready");
    assert_eq!(
        entry["not_tracked_in_exact_store"],
        json!(["collections_context", "weekly_bddre", "weekly_rpcoe"])
    );

    // Same input, same stored data: same output hash.
    let again = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(again.output_sha256, envelope.output_sha256);
}

#[tokio::test]
async fn empty_budgets_block_readiness() {
    let fx = Fixture::new().await;
    let mut dataset = full("Synthetic Oak", "2026-05", "100.10");
    dataset["budgets"] = json!([]);
    let revision = fx.import(dataset).await;
    fx.issue(&revision).await;

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(envelope.status, "review_required");
    let entry = only_property(&envelope);
    assert_eq!(entry["status"], "not_ready");
    assert_eq!(entry["actuals_present"], true);
    assert_eq!(entry["budgets_present"], false);
    let reasons: Vec<&str> = entry["blocking_review_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"missing_budgets"), "{reasons:?}");
    assert_eq!(
        entry["account_coverage"],
        json!({"matched": 0, "actual_only": 2, "budget_only": 0})
    );
    assert_eq!(
        result(&envelope)["summary"],
        json!({"ready": 0, "not_ready": 1, "no_revisions": 0})
    );
    assert_eq!(
        result(&envelope)["review_reasons"],
        json!(["properties_not_ready"])
    );
}

#[tokio::test]
async fn no_issued_report_means_not_ready() {
    let fx = Fixture::new().await;
    let revision = fx.import(full("Synthetic Oak", "2026-05", "100.10")).await;

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(envelope.status, "review_required");
    let entry = only_property(&envelope);
    assert_eq!(entry["revision_id"], revision.as_str());
    assert_eq!(entry["issued_reports"], json!([]));
    assert_eq!(entry["blocking_review_reasons"], json!([]));
    assert_eq!(entry["snapshot"], "present");
    assert_eq!(entry["status"], "not_ready");

    // Issuing the report is the only change needed.
    fx.issue(&revision).await;
    let after = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(after.status, "calculated");
    assert_eq!(only_property(&after)["status"], "ready");
}

#[tokio::test]
async fn incomplete_account_coverage_blocks_and_counts_distinct_accounts() {
    let fx = Fixture::new().await;
    let mut dataset = full("Synthetic Oak", "2026-05", "100.10");
    dataset["actuals"] = json!([
        line("4000", "rental income", "100.10"),
        line("5000", "payroll", "30.00"),
        line("6000", "utilities", "4.00"),
        // A second line on the same actual-only account counts once.
        line("6000", "utilities", "1.00"),
    ]);
    dataset["budgets"] = json!([
        line("4000", "rental income", "90.05"),
        line("5000", "payroll", "25.00"),
        line("7000", "repairs", "9.00"),
    ]);
    let revision = fx.import(dataset).await;
    fx.issue(&revision).await;

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(envelope.status, "review_required");
    let entry = only_property(&envelope);
    assert_eq!(
        entry["account_coverage"],
        json!({"matched": 2, "actual_only": 1, "budget_only": 1})
    );
    assert_eq!(
        entry["blocking_review_reasons"],
        json!(["incomplete_account_coverage"])
    );
    assert_eq!(entry["actuals_present"], true);
    assert_eq!(entry["budgets_present"], true);
    assert_eq!(entry["snapshot"], "present");
    assert_eq!(entry["issued_reports"].as_array().unwrap().len(), 1);
    assert_eq!(entry["status"], "not_ready");
}

#[tokio::test]
async fn missing_snapshot_blocks_readiness() {
    let fx = Fixture::new().await;
    let mut dataset = full("Synthetic Oak", "2026-05", "100.10");
    dataset["snapshot"] = Value::Null;
    let revision = fx.import(dataset).await;
    fx.issue(&revision).await;

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    let entry = only_property(&envelope);
    assert_eq!(entry["snapshot"], "missing");
    assert_eq!(
        entry["blocking_review_reasons"],
        json!(["missing_occupancy_snapshot"])
    );
    assert_eq!(entry["status"], "not_ready");
}

#[tokio::test]
async fn correction_uses_the_latest_revision_and_needs_its_own_report() {
    let fx = Fixture::new().await;
    let first = fx.import(full("Synthetic Oak", "2026-05", "100.10")).await;
    fx.issue(&first).await;
    let second = fx
        .correct(
            full("Synthetic Oak", "2026-05", "110.10"),
            &first,
            "May accrual",
        )
        .await;

    // The first revision's report does not cover the correction.
    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(envelope.status, "review_required");
    let entry = only_property(&envelope);
    assert_eq!(entry["revision_id"], second.as_str());
    assert_eq!(entry["revision"], 2);
    assert_eq!(entry["is_correction"], true);
    assert_eq!(entry["correction_reason"], "May accrual");
    assert_eq!(entry["issued_reports"], json!([]));
    assert_eq!(entry["status"], "not_ready");

    let report = fx.issue(&second).await;
    let after = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(after.status, "calculated");
    let entry = only_property(&after);
    assert_eq!(entry["revision"], 2);
    assert_eq!(entry["issued_reports"][0]["id"], report.as_str());
    assert_eq!(entry["status"], "ready");
}

#[tokio::test]
async fn property_filter_and_one_entry_per_property() {
    let fx = Fixture::new().await;
    let oak = fx.import(full("Synthetic Oak", "2026-05", "100.10")).await;
    fx.issue(&oak).await;
    fx.import(full("Synthetic Pine", "2026-05", "100.10")).await;
    // Another period must not leak into May.
    let june = fx.import(full("Synthetic Oak", "2026-06", "100.10")).await;
    fx.issue(&june).await;

    let all = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(all.status, "review_required");
    let properties = result(&all)["properties"].as_array().unwrap();
    let names: Vec<&str> = properties
        .iter()
        .map(|p| p["property"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Synthetic Oak", "Synthetic Pine"]);
    assert_eq!(properties[0]["status"], "ready");
    assert_eq!(properties[1]["status"], "not_ready");
    assert_eq!(
        result(&all)["summary"],
        json!({"ready": 1, "not_ready": 1, "no_revisions": 0})
    );

    let oak_only = run(
        json!({"period": "2026-05", "property": "Synthetic Oak"}),
        &fx.path,
    )
    .await;
    assert_eq!(oak_only.status, "calculated");
    assert_eq!(only_property(&oak_only)["property"], "Synthetic Oak");
    assert_eq!(
        result(&oak_only)["summary"],
        json!({"ready": 1, "not_ready": 0, "no_revisions": 0})
    );

    let pine_only = run(
        json!({"period": "2026-05", "property": "Synthetic Pine"}),
        &fx.path,
    )
    .await;
    assert_eq!(pine_only.status, "review_required");
    assert_eq!(only_property(&pine_only)["property"], "Synthetic Pine");

    // A filter that matches nothing for the period behaves like no data.
    let none = run(
        json!({"period": "2026-05", "property": "Synthetic Elm"}),
        &fx.path,
    )
    .await;
    assert_eq!(result(&none)["properties"], json!([]));
    assert_eq!(
        result(&none)["review_reasons"],
        json!(["no_revisions_for_period"])
    );
}

#[tokio::test]
async fn nothing_imported_for_the_period_is_an_empty_review_required_result() {
    let fx = Fixture::new().await;
    let empty = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(empty.status, "review_required");
    let body = result(&empty);
    assert_eq!(body["period"], "2026-05");
    assert_eq!(body["properties"], json!([]));
    assert_eq!(
        body["summary"],
        json!({"ready": 0, "not_ready": 0, "no_revisions": 1})
    );
    assert_eq!(body["review_reasons"], json!(["no_revisions_for_period"]));

    // Data for a different period does not count.
    fx.import(full("Synthetic Oak", "2026-04", "100.10")).await;
    let other = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(other.status, "review_required");
    assert_eq!(result(&other)["properties"], json!([]));
}

#[tokio::test]
async fn invalid_input_is_refused() {
    let fx = Fixture::new().await;
    for input in [
        json!({"period": "2026-5"}),
        json!({"period": "2026-13"}),
        json!({"period": "2026-05-01"}),
        json!({"period": " 2026-05"}),
        json!({}),
        json!({"period": "2026-05", "property": "  "}),
        json!({"period": "2026-05", "extra": true}),
    ] {
        let envelope = run(input.clone(), &fx.path).await;
        assert_eq!(code(&envelope), "INVALID_INPUT", "{input}");
    }
}

#[tokio::test]
async fn requires_a_database_after_the_input_is_checked() {
    let envelope = catalog::invoke(&call(json!({"period": "2026-05"}), None)).await;
    assert_eq!(code(&envelope), "DATABASE_REQUIRED");
    // A malformed request is still reported as malformed.
    let envelope = catalog::invoke(&call(json!({"period": "2026-5"}), None)).await;
    assert_eq!(code(&envelope), "INVALID_INPUT");
}

#[tokio::test]
async fn tampered_rows_refuse_with_integrity_error() {
    let fx = Fixture::new().await;
    let revision = fx.import(full("Synthetic Oak", "2026-05", "100.10")).await;
    fx.issue(&revision).await;
    assert_eq!(
        run(json!({"period": "2026-05"}), &fx.path).await.status,
        "calculated"
    );

    sqlx::query("DROP TRIGGER exact_gl_no_UPDATE")
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE exact_gl SET amount_cents = amount_cents + 1 WHERE revision_id = ? AND kind = 'actual' AND ordinal = 0")
        .bind(&revision)
        .execute(&fx.pool)
        .await
        .unwrap();

    let envelope = run(json!({"period": "2026-05"}), &fx.path).await;
    assert_eq!(code(&envelope), "INTEGRITY_ERROR");
}

#[tokio::test]
async fn describe_lists_twelve_functions_with_close_readiness_stateful() {
    let envelope = catalog::invoke(&Call {
        function: "describe".into(),
        version: None,
        input: json!({}),
        database: None,
        caller: "test".into(),
    })
    .await;
    let calculations = result(&envelope)["calculations"].as_array().unwrap();
    assert_eq!(calculations.len(), 12);
    let entry = &calculations[9];
    assert_eq!(entry["name"], "close_readiness");
    assert_eq!(entry["version"], "1.0.0");
    assert_eq!(entry["stateful"], true);
    assert_eq!(entry["input_schema"]["additionalProperties"], false);
    assert_eq!(entry["input_schema"]["required"], json!(["period"]));
    assert_eq!(entry["output_schema"]["type"], "object");
}
