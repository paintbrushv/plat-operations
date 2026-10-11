//! Occupancy, economic occupancy and delinquency over the exact-cent
//! snapshot, plus the integer-only `Ratio` they report rates with. Synthetic
//! data only.

use boxscore::exact::{
    catalog::{self, Call},
    digest,
    ratio::Ratio,
    store::{self, Dataset, Snapshot},
    variance::Line,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn ratio(n: i128, d: i128) -> Ratio {
    Ratio::new(n, d).unwrap()
}

fn call(function: &str, input: Value, database: Option<&Path>) -> Call {
    Call {
        function: function.into(),
        version: None,
        input,
        database: database.map(Path::to_path_buf),
    }
}

fn snapshot_json() -> Value {
    json!({
        "as_of_date": "2026-05-31",
        "occupied_units": 153, "vacant_units": 15, "down_units": 4,
        "market_rent_total": "1000.00", "in_place_rent_total": "889.50",
        "delinquent_amount": "250.00", "prepaid_amount": "40.00",
        "concessions_amount": null
    })
}

fn inline(snapshot: Value) -> Value {
    json!({"snapshot": snapshot, "unit_count": 172})
}

fn with(field: &str, value: Value) -> Value {
    let mut snapshot = snapshot_json();
    snapshot[field] = value;
    snapshot
}

async fn run(function: &str, input: Value) -> catalog::Envelope {
    catalog::invoke(&call(function, input, None)).await
}

fn code(envelope: &catalog::Envelope) -> &str {
    assert!(
        envelope.refused(),
        "expected a refusal: {:?}",
        envelope.result
    );
    assert!(envelope.result.is_none() && envelope.output_sha256.is_none());
    envelope.error.as_ref().unwrap()["code"].as_str().unwrap()
}

// ---------------------------------------------------------------------------
// Ratio
// ---------------------------------------------------------------------------

#[test]
fn ratio_rounds_half_away_from_zero_at_scale_four() {
    for (n, d, expected) in [
        (153, 172, "0.8895"),
        (1, 3, "0.3333"),
        (2, 3, "0.6667"),
        (1, 8, "0.1250"),
        (5, 10_000, "0.0005"),
        // Exactly half of the last place rounds away from zero.
        (1, 20_000, "0.0001"),
        (-1, 20_000, "-0.0001"),
        (1, -20_000, "-0.0001"),
        (-1, -20_000, "0.0001"),
        (3, 20_000, "0.0002"),
        // Just under half rounds toward zero.
        (1, 20_001, "0.0000"),
        (-1, 20_001, "0.0000"),
        (-1, 3, "-0.3333"),
        (-2, 3, "-0.6667"),
        (1, -3, "-0.3333"),
        (0, 7, "0.0000"),
        (0, -7, "0.0000"),
        (172, 172, "1.0000"),
        (3, 1, "3.0000"),
        (123_456, 100, "1234.5600"),
    ] {
        assert_eq!(ratio(n, d).decimal(), expected, "{n}/{d}");
    }
}

#[test]
fn ratio_serializes_strings_with_a_numeric_scale_only() {
    assert_eq!(
        serde_json::to_value(ratio(153, 172)).unwrap(),
        json!({"numerator": "153", "denominator": "172", "decimal": "0.8895", "scale": 4})
    );
    assert_eq!(
        serde_json::to_string(&ratio(-1, 3)).unwrap(),
        r#"{"decimal":"-0.3333","denominator":"3","numerator":"-1","scale":4}"#
    );
    let value = serde_json::to_value(ratio(-1, 3)).unwrap();
    assert_eq!(value["numerator"], "-1");
    assert_eq!(value["decimal"], "-0.3333");
    let r = ratio(153, 172);
    assert_eq!((r.numerator(), r.denominator()), (153, 172));
}

#[test]
fn ratio_refuses_zero_denominator_and_out_of_range_values() {
    assert_eq!(Ratio::new(1, 0).unwrap_err().code, "ZERO_DENOMINATOR");
    assert_eq!(Ratio::new(0, 0).unwrap_err().code, "ZERO_DENOMINATOR");
    assert_eq!(Ratio::new(i128::MAX, 1).unwrap_err().code, "RATIO_OVERFLOW");
    assert_eq!(Ratio::new(i128::MIN, 1).unwrap_err().code, "RATIO_OVERFLOW");
    // The largest numerator that scales without overflow still works, and a
    // huge denominator does not overflow the rounding step.
    let big = i128::MAX / 10_000;
    assert_eq!(ratio(big, 1).decimal().len(), big.to_string().len() + 5);
    assert_eq!(ratio(1, i128::MAX).decimal(), "0.0000");
    assert_eq!(ratio(big, i128::MAX).decimal(), "0.0001");
    assert_eq!(ratio(big, big).decimal(), "1.0000");
    assert_eq!(ratio(1, i128::MIN).decimal(), "0.0000");
}

// ---------------------------------------------------------------------------
// Catalog: inline snapshots
// ---------------------------------------------------------------------------

#[tokio::test]
async fn occupancy_happy_path_has_a_deterministic_envelope() {
    let first = run("occupancy", inline(snapshot_json())).await;
    let second = run("occupancy", inline(snapshot_json())).await;
    assert_eq!(first.status, "review_required", "{:?}", first.error);
    assert_eq!(first.function, "occupancy");
    assert_eq!(first.version, Some("1.0.0"));
    assert_eq!(first.contract_version, "plat.ops/1");
    assert_eq!(first.producer["arithmetic"], "checked_i64_cents");
    let result = first.result.clone().unwrap();
    assert_eq!(
        result["occupancy"],
        json!({"numerator": "153", "denominator": "172", "decimal": "0.8895", "scale": 4})
    );
    assert_eq!(result["occupied_units"], 153);
    assert_eq!(result["vacant_units"], 15);
    assert_eq!(result["down_units"], 4);
    assert_eq!(result["unit_count"], 172);
    assert_eq!(result["as_of_date"], "2026-05-31");
    assert_eq!(result["review_reasons"], json!(["down_units_present"]));
    assert_eq!(first.result, second.result);
    assert_eq!(first.output_sha256, second.output_sha256);
    assert_eq!(first.input_sha256, second.input_sha256);
    assert_eq!(
        first.output_sha256.as_deref(),
        Some(digest(&catalog::canonical(&result)).as_str())
    );

    let mut no_down = snapshot_json();
    no_down["vacant_units"] = json!(19);
    no_down["down_units"] = json!(0);
    let clean = run("occupancy", inline(no_down)).await;
    assert_eq!(clean.status, "calculated");
    assert_eq!(clean.result.unwrap()["review_reasons"], json!([]));
}

#[tokio::test]
async fn economic_occupancy_happy_path_and_review_reason() {
    let envelope = run("economic_occupancy", inline(snapshot_json())).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    let result = envelope.result.clone().unwrap();
    assert_eq!(result["market_rent_total"], "1000.00");
    assert_eq!(result["in_place_rent_total"], "889.50");
    assert_eq!(
        result["economic_occupancy"],
        json!({"numerator": "88950", "denominator": "100000", "decimal": "0.8895", "scale": 4})
    );
    assert_eq!(result["review_reasons"], json!([]));
    let again = run("economic_occupancy", inline(snapshot_json())).await;
    assert_eq!(envelope.output_sha256, again.output_sha256);

    let over = run(
        "economic_occupancy",
        inline(with("in_place_rent_total", json!("1100.00"))),
    )
    .await;
    assert_eq!(over.status, "review_required");
    let result = over.result.unwrap();
    assert_eq!(result["economic_occupancy"]["decimal"], "1.1000");
    assert_eq!(result["review_reasons"], json!(["in_place_exceeds_market"]));
}

#[tokio::test]
async fn delinquency_happy_path_with_shares() {
    let envelope = run("delinquency", inline(snapshot_json())).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    let result = envelope.result.clone().unwrap();
    assert_eq!(result["delinquent_amount"], "250.00");
    assert_eq!(result["prepaid_amount"], "40.00");
    assert_eq!(result["net"], "210.00");
    assert_eq!(
        result["delinquent_share_of_in_place_rent"],
        json!({"numerator": "25000", "denominator": "88950", "decimal": "0.2811", "scale": 4})
    );
    assert_eq!(
        result["prepaid_share_of_in_place_rent"],
        json!({"numerator": "4000", "denominator": "88950", "decimal": "0.0450", "scale": 4})
    );
    assert_eq!(result["review_reasons"], json!([]));
    let again = run("delinquency", inline(snapshot_json())).await;
    assert_eq!(envelope.output_sha256, again.output_sha256);

    // Prepaid larger than delinquent gives a negative net, not a refusal.
    let credit = run(
        "delinquency",
        inline(with("prepaid_amount", json!("300.00"))),
    )
    .await;
    assert_eq!(credit.result.unwrap()["net"], "-50.00");
}

#[tokio::test]
async fn delinquency_review_reasons_for_missing_prepaid_and_in_place_rent() {
    let no_prepaid = run("delinquency", inline(with("prepaid_amount", Value::Null))).await;
    assert_eq!(no_prepaid.status, "review_required");
    let result = no_prepaid.result.unwrap();
    assert_eq!(result["prepaid_amount"], Value::Null);
    assert_eq!(result["net"], Value::Null);
    assert_eq!(result["prepaid_share_of_in_place_rent"], Value::Null);
    assert_eq!(
        result["delinquent_share_of_in_place_rent"]["decimal"],
        "0.2811"
    );
    assert_eq!(result["review_reasons"], json!(["missing_prepaid"]));

    for missing in [Value::Null, json!("0.00")] {
        let no_rent = run("delinquency", inline(with("in_place_rent_total", missing))).await;
        assert_eq!(no_rent.status, "review_required");
        let result = no_rent.result.unwrap();
        assert_eq!(result["net"], "210.00");
        assert_eq!(result["delinquent_share_of_in_place_rent"], Value::Null);
        assert_eq!(result["prepaid_share_of_in_place_rent"], Value::Null);
        assert_eq!(result["review_reasons"], json!(["missing_in_place_rent"]));
    }

    let mut neither = snapshot_json();
    neither["prepaid_amount"] = Value::Null;
    neither["in_place_rent_total"] = Value::Null;
    let both = run("delinquency", inline(neither)).await;
    assert_eq!(
        both.result.unwrap()["review_reasons"],
        json!(["missing_prepaid", "missing_in_place_rent"])
    );
}

#[tokio::test]
async fn metric_refusals_name_their_codes() {
    let cases = [
        (
            "economic_occupancy",
            with("market_rent_total", Value::Null),
            "MISSING_MARKET_RENT",
        ),
        (
            "economic_occupancy",
            with("in_place_rent_total", Value::Null),
            "MISSING_IN_PLACE_RENT",
        ),
        (
            "economic_occupancy",
            with("market_rent_total", json!("0.00")),
            "ZERO_DENOMINATOR",
        ),
        (
            "delinquency",
            with("delinquent_amount", Value::Null),
            "MISSING_DELINQUENCY",
        ),
        // Occupied, vacant and down units must reconcile to the unit count.
        (
            "occupancy",
            with("vacant_units", json!(16)),
            "SNAPSHOT_MISMATCH",
        ),
        (
            "occupancy",
            with("down_units", json!(-1)),
            "SNAPSHOT_MISMATCH",
        ),
        (
            "occupancy",
            with("as_of_date", json!("2026-5-31")),
            "INVALID_INPUT",
        ),
        (
            "occupancy",
            with("as_of_date", json!("2026-02-30")),
            "INVALID_INPUT",
        ),
        (
            "delinquency",
            {
                // Delinquent minus a negative prepaid overflows the cent range.
                let mut s = with("delinquent_amount", json!("92233720368547758.07"));
                s["prepaid_amount"] = json!("-1.00");
                s
            },
            "MONEY_OVERFLOW",
        ),
    ];
    for (function, snapshot, expected) in cases {
        let envelope = run(function, inline(snapshot.clone())).await;
        assert_eq!(code(&envelope), expected, "{function} {snapshot}");
        assert_eq!(envelope.status, "refused");
    }
}

#[tokio::test]
async fn input_forms_are_exclusive_and_strict() {
    for function in ["occupancy", "economic_occupancy", "delinquency"] {
        for input in [
            json!({}),
            json!({"snapshot": snapshot_json()}),
            json!({"unit_count": 172}),
            json!({"revision_id": "r1", "unit_count": 172}),
            json!({"revision_id": "r1", "snapshot": snapshot_json(), "unit_count": 172}),
            json!({"snapshot": snapshot_json(), "unit_count": 172, "database": "/tmp/x"}),
            json!({"snapshot": snapshot_json(), "unit_count": 0}),
            json!({"snapshot": snapshot_json(), "unit_count": 10_000_001}),
            json!({"snapshot": snapshot_json(), "unit_count": "172"}),
        ] {
            let envelope = run(function, input.clone()).await;
            assert_eq!(code(&envelope), "INVALID_INPUT", "{function} {input}");
        }
    }
    let mut float_money = snapshot_json();
    float_money["market_rent_total"] = json!(1000.0);
    let envelope = run("economic_occupancy", inline(float_money)).await;
    assert_eq!(code(&envelope), "INVALID_INPUT");

    let mut unknown_field = snapshot_json();
    unknown_field["extra"] = json!(1);
    let envelope = run("occupancy", inline(unknown_field)).await;
    assert_eq!(code(&envelope), "INVALID_INPUT");

    let mut pinned = call("occupancy", inline(snapshot_json()), None);
    pinned.version = Some("2.0.0".into());
    assert_eq!(
        catalog::invoke(&pinned).await.error.unwrap()["code"],
        "VERSION_MISMATCH"
    );
}

#[tokio::test]
async fn inline_snapshots_never_touch_a_database_even_when_one_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.sqlite");
    let envelope =
        catalog::invoke(&call("occupancy", inline(snapshot_json()), Some(&missing))).await;
    assert_eq!(envelope.status, "review_required", "{:?}", envelope.error);
    assert!(!missing.exists());
}

// ---------------------------------------------------------------------------
// Catalog: stored revisions
// ---------------------------------------------------------------------------

fn line(code: &str, category: &str, amount: &str) -> Line {
    Line {
        account_code: code.into(),
        account_name: code.into(),
        category: category.into(),
        amount: amount.parse().unwrap(),
    }
}

fn dataset(period: &str, snapshot: Option<Snapshot>) -> Dataset {
    Dataset {
        property: "Synthetic Oak".into(),
        period: period.into(),
        currency: "USD".into(),
        expense_convention: "positive_costs".into(),
        unit_count: 172,
        actuals: vec![line("4000", "rental income", "100.10")],
        budgets: vec![line("4000", "rental income", "90.05")],
        snapshot,
    }
}

fn stored_snapshot() -> Snapshot {
    serde_json::from_value(snapshot_json()).unwrap()
}

async fn database_with_revisions(dir: &Path) -> (PathBuf, String, String) {
    let path = dir.join("exact.sqlite");
    let pool = store::create(&path).await.unwrap();
    let sources = json!({"synthetic": "hash"});
    let with_snapshot = store::import(
        &pool,
        &dataset("2026-05", Some(stored_snapshot())),
        None,
        None,
        &sources,
    )
    .await
    .unwrap();
    let without = store::import(&pool, &dataset("2026-06", None), None, None, &sources)
        .await
        .unwrap();
    pool.close().await;
    (path, with_snapshot, without)
}

#[tokio::test]
async fn revision_path_reads_the_stored_snapshot_and_unit_count() {
    let dir = tempfile::tempdir().unwrap();
    let (db, revision, _) = database_with_revisions(dir.path()).await;

    let occupancy = catalog::invoke(&call(
        "occupancy",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(occupancy.status, "review_required", "{:?}", occupancy.error);
    let stored = occupancy.result.clone().unwrap();
    let direct = run("occupancy", inline(snapshot_json()))
        .await
        .result
        .unwrap();
    assert_eq!(stored, direct, "stored and inline paths must agree");
    assert_eq!(stored["occupancy"]["decimal"], "0.8895");
    assert_eq!(stored["unit_count"], 172);

    let economic = catalog::invoke(&call(
        "economic_occupancy",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(economic.status, "calculated", "{:?}", economic.error);
    assert_eq!(
        economic.result.unwrap()["economic_occupancy"]["decimal"],
        "0.8895"
    );

    let delinquency = catalog::invoke(&call(
        "delinquency",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(delinquency.status, "calculated", "{:?}", delinquency.error);
    assert_eq!(delinquency.result.unwrap()["net"], "210.00");
}

#[tokio::test]
async fn revision_path_refusals() {
    for function in ["occupancy", "economic_occupancy", "delinquency"] {
        let no_database = run(function, json!({"revision_id": "r1"})).await;
        assert_eq!(code(&no_database), "DATABASE_REQUIRED", "{function}");
        assert_eq!(no_database.version, Some("1.0.0"));
    }

    let dir = tempfile::tempdir().unwrap();
    let (db, _, without_snapshot) = database_with_revisions(dir.path()).await;
    let no_snapshot = catalog::invoke(&call(
        "occupancy",
        json!({"revision_id": without_snapshot}),
        Some(&db),
    ))
    .await;
    assert_eq!(code(&no_snapshot), "MISSING_OCCUPANCY_SNAPSHOT");

    let unknown = catalog::invoke(&call(
        "delinquency",
        json!({"revision_id": "nope"}),
        Some(&db),
    ))
    .await;
    assert_eq!(code(&unknown), "NOT_FOUND");

    let not_a_database = dir.path().join("plain.sqlite");
    std::fs::write(&not_a_database, b"").unwrap();
    let wrong = catalog::invoke(&call(
        "occupancy",
        json!({"revision_id": "r1"}),
        Some(&not_a_database),
    ))
    .await;
    assert!(wrong.refused());
}

// ---------------------------------------------------------------------------
// Catalog surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn describe_lists_the_snapshot_metrics_stateless() {
    let envelope = run("describe", json!({})).await;
    let result = envelope.result.unwrap();
    let calculations = result["calculations"].as_array().unwrap();
    let names: Vec<&str> = calculations
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(calculations.len(), 11);
    assert_eq!(
        names[6..9],
        ["occupancy", "economic_occupancy", "delinquency"]
    );
    for entry in &calculations[6..9] {
        assert_eq!(entry["version"], "1.0.0");
        assert_eq!(entry["stateful"], false);
        assert_eq!(entry["input_schema"]["additionalProperties"], false);
        assert_eq!(entry["input_schema"]["oneOf"].as_array().unwrap().len(), 2);
        assert_eq!(entry["output_schema"]["type"], "object");
    }
}

#[tokio::test]
async fn tampered_stored_snapshot_refuses_with_integrity_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exact.sqlite");
    let pool = store::create(&path).await.unwrap();
    let revision = store::import(
        &pool,
        &dataset("2026-05", Some(stored_snapshot())),
        None,
        None,
        &json!({"synthetic": "hash"}),
    )
    .await
    .unwrap();

    // Before tampering, the revision is served.
    let before = catalog::invoke(&call(
        "occupancy",
        json!({"revision_id": revision}),
        Some(&path),
    ))
    .await;
    assert_eq!(before.status, "review_required", "{:?}", before.error);

    // Stored rows are immutable by trigger; a hostile or corrupting writer
    // has to remove the trigger first.
    sqlx::query("DROP TRIGGER exact_snapshots_no_UPDATE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE exact_snapshots SET occupied_units = occupied_units - 1 WHERE revision_id = ?",
    )
    .bind(&revision)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    for function in [
        "occupancy",
        "economic_occupancy",
        "delinquency",
        "review_period",
    ] {
        let envelope = catalog::invoke(&call(
            function,
            json!({"revision_id": revision}),
            Some(&path),
        ))
        .await;
        assert_eq!(code(&envelope), "INTEGRITY_ERROR", "{function}");
        assert_eq!(
            envelope.error.unwrap()["message"],
            "Stored period no longer matches its input hash"
        );
    }
}
