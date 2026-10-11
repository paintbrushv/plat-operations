//! `t12@1.0.0` through the catalog, against a temporary exact-cent database.
//! All data is synthetic.

use boxscore::exact::{
    catalog::{self, Call, Envelope},
    store,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};

const PROPERTY: &str = "Synthetic Oak";
/// The window ending 2026-05, earliest first.
const PERIODS: [&str; 12] = [
    "2025-06", "2025-07", "2025-08", "2025-09", "2025-10", "2025-11", "2025-12", "2026-01",
    "2026-02", "2026-03", "2026-04", "2026-05",
];

fn dollars(cents: i64) -> String {
    format!(
        "{}{}.{:02}",
        if cents < 0 { "-" } else { "" },
        cents.abs() / 100,
        cents.abs() % 100
    )
}

fn line(code: &str, category: &str, cents: i64) -> Value {
    json!({"account_code": code, "account_name": code, "category": category, "amount": dollars(cents)})
}

/// Month `i` of the window. Actual NOI is `67_000 + 996 * i` cents and budget
/// NOI is a constant `64_500`, so every expected number is checkable by hand.
fn month(property: &str, period: &str, i: i64, with_budget: bool) -> store::Dataset {
    let budgets = if with_budget {
        json!([
            line("4000", "Rental Income", 99_000),
            line("4100", "Other Income", 5_000),
            line("5000", "Payroll", 31_000),
            line("5100", "Utilities", 8_500)
        ])
    } else {
        json!([])
    };
    dataset(
        property,
        period,
        json!([
            line("4000", "Rental Income", 100_000 + 1_005 * i),
            line("4100", "Other Income", 5_000 + i),
            line("5000", "Payroll", 30_000 + 7 * i),
            line("5100", "Utilities", 8_000 + 3 * i)
        ]),
        budgets,
    )
}

fn dataset(property: &str, period: &str, actuals: Value, budgets: Value) -> store::Dataset {
    serde_json::from_value(json!({
        "property": property, "period": period, "currency": "USD",
        "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": actuals, "budgets": budgets, "snapshot": null
    }))
    .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    pool: SqlitePool,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("exact.sqlite");
        let pool = store::create(&db).await.unwrap();
        Self {
            _dir: dir,
            db,
            pool,
        }
    }

    async fn import(&self, data: &store::Dataset) -> String {
        store::import(&self.pool, data, None, None, &json!({}))
            .await
            .unwrap()
    }

    async fn correct(&self, data: &store::Dataset, parent: &str) -> String {
        store::import(
            &self.pool,
            data,
            Some(parent),
            Some("synthetic correction"),
            &json!({}),
        )
        .await
        .unwrap()
    }

    /// Import the twelve standard months, skipping the indexes in `skip`.
    async fn seed(&self, skip: &[usize]) -> Vec<Option<String>> {
        let mut ids = Vec::new();
        for (i, period) in PERIODS.iter().enumerate() {
            ids.push(if skip.contains(&i) {
                None
            } else {
                Some(self.import(&month(PROPERTY, period, i as i64, true)).await)
            });
        }
        ids
    }

    async fn t12(&self, input: Value) -> Envelope {
        run(Some(&self.db), input).await
    }
}

async fn run(database: Option<&Path>, input: Value) -> Envelope {
    catalog::invoke(&Call {
        function: "t12".into(),
        version: None,
        input,
        database: database.map(Path::to_path_buf),
    })
    .await
}

fn ask() -> Value {
    json!({"property": PROPERTY, "end_period": "2026-05"})
}

fn result(envelope: &Envelope) -> Value {
    assert_ne!(envelope.status, "refused", "{:?}", envelope.error);
    envelope.result.clone().unwrap()
}

fn code(envelope: &Envelope) -> &str {
    assert!(
        envelope.refused(),
        "expected a refusal, got {}",
        envelope.status
    );
    envelope.error.as_ref().unwrap()["code"].as_str().unwrap()
}

fn row<'a>(statement: &'a Value, category: &str) -> &'a Value {
    statement["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["category"] == category)
        .unwrap_or_else(|| panic!("no row {category}"))
}

fn categories(statement: &Value) -> Vec<&str> {
    statement["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["category"].as_str().unwrap())
        .collect()
}

fn values(v: &Value) -> Vec<Option<&str>> {
    v.as_array().unwrap().iter().map(Value::as_str).collect()
}

fn without(v: &Value, index: usize) -> Vec<Value> {
    let mut all = v.as_array().unwrap().clone();
    all.remove(index);
    all
}

#[tokio::test]
async fn twelve_present_periods_have_no_nulls_and_noi_matches_review_period() {
    let fx = Fixture::new().await;
    let ids = fx.seed(&[]).await;
    let envelope = fx.t12(ask()).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    assert_eq!(envelope.function, "t12");
    assert_eq!(envelope.version, Some("1.0.0"));
    let out = result(&envelope);

    assert_eq!(out["property"], PROPERTY);
    assert_eq!(out["periods"], json!(PERIODS));
    assert_eq!(out["review_reasons"], json!([]));
    assert_eq!(out["missing_periods"], json!([]));
    for (i, revision) in out["revisions"].as_array().unwrap().iter().enumerate() {
        assert_eq!(revision["period"], PERIODS[i]);
        assert_eq!(revision["revision_id"], json!(ids[i]));
        assert_eq!(revision["revision"], 1);
    }

    let actuals = &out["actuals"];
    assert_eq!(
        categories(actuals),
        ["Rental Income", "Other Income", "Payroll", "Utilities"]
    );
    assert_eq!(row(actuals, "Rental Income")["class"], "revenue");
    assert_eq!(row(actuals, "Other Income")["class"], "revenue");
    assert_eq!(row(actuals, "Payroll")["class"], "expense");
    assert_eq!(row(actuals, "Utilities")["class"], "expense");
    for r in actuals["rows"].as_array().unwrap() {
        assert!(
            r["values"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.is_string()),
            "{r}"
        );
        assert_eq!(r["values"].as_array().unwrap().len(), 12);
    }
    assert!(actuals["noi"]
        .as_array()
        .unwrap()
        .iter()
        .all(Value::is_string));
    assert_eq!(actuals["unmapped"], json!(vec!["0.00"; 12]));

    // Hand-computed: rent 1000.00 + 10.05 i; NOI 670.00 + 9.96 i.
    let rent = values(&row(actuals, "Rental Income")["values"]);
    assert_eq!(rent[0], Some("1000.00"));
    assert_eq!(rent[11], Some("1110.55"));
    assert_eq!(
        row(actuals, "Rental Income")["total"],
        dollars((0..12).map(|i| 100_000 + 1_005 * i).sum())
    );
    let noi_cents = |i: i64| 67_000 + 996 * i;
    for i in 0..12 {
        assert_eq!(
            actuals["noi"][i as usize],
            dollars(noi_cents(i)),
            "{}",
            PERIODS[i as usize]
        );
    }
    assert_eq!(actuals["noi_total"], dollars((0..12).map(noi_cents).sum()));

    // Budgets and variance are present and actual-minus-budget.
    let budgets = &out["budgets"];
    assert_eq!(budgets["noi"], json!(vec!["645.00"; 12]));
    assert_eq!(budgets["noi_total"], dollars(64_500 * 12));
    for i in 0..12 {
        assert_eq!(
            out["variance"]["noi"][i as usize],
            dollars(noi_cents(i) - 64_500)
        );
    }
    assert_eq!(
        out["variance"]["noi_total"],
        dollars((0..12).map(|i| noi_cents(i) - 64_500).sum())
    );

    // Cross-check every month against review_period for the same revision.
    for (i, id) in ids.iter().enumerate() {
        let review = catalog::invoke(&Call {
            function: "review_period".into(),
            version: None,
            input: json!({"revision_id": id}),
            database: Some(fx.db.clone()),
        })
        .await;
        let bridge = result(&review)["variance"]["noi_bridge"].clone();
        assert_eq!(
            bridge["actual_noi"], actuals["noi"][i],
            "actual NOI {}",
            PERIODS[i]
        );
        assert_eq!(
            bridge["budget_noi"], budgets["noi"][i],
            "budget NOI {}",
            PERIODS[i]
        );
        assert_eq!(
            bridge["noi_variance"], out["variance"]["noi"][i],
            "variance {}",
            PERIODS[i]
        );
    }
}

#[tokio::test]
async fn a_gap_month_is_null_not_zero_and_requires_review() {
    let fx = Fixture::new().await;
    let ids = fx.seed(&[3]).await;
    let envelope = fx.t12(ask()).await;
    assert_eq!(envelope.status, "review_required");
    let out = result(&envelope);

    assert_eq!(out["missing_periods"], json!(["2025-09"]));
    assert_eq!(out["review_reasons"], json!(["missing_periods"]));
    assert_eq!(
        out["revisions"][3],
        json!({"period": "2025-09", "revision_id": null, "revision": null})
    );
    assert_eq!(out["revisions"][4]["revision_id"], json!(ids[4]));

    let actuals = &out["actuals"];
    for r in actuals["rows"].as_array().unwrap() {
        assert!(r["values"][3].is_null(), "{r}");
        assert_eq!(
            r["values"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.is_null())
                .count(),
            1
        );
    }
    assert!(actuals["noi"][3].is_null());
    assert!(actuals["unmapped"][3].is_null());
    // Totals sum only the eleven present months.
    let present = |f: &dyn Fn(i64) -> i64| -> i64 { (0..12).filter(|i| *i != 3).map(f).sum() };
    assert_eq!(
        actuals["noi_total"],
        dollars(present(&|i| 67_000 + 996 * i))
    );
    assert_eq!(
        row(actuals, "Rental Income")["total"],
        dollars(present(&|i| 100_000 + 1_005 * i))
    );
    // Budgets stay complete for the present months; the gap is null there too.
    assert!(out["budgets"]["noi"][3].is_null());
    assert!(out["variance"]["noi"][3].is_null());
    assert_eq!(
        out["variance"]["noi_total"],
        dollars(present(&|i| 2_500 + 996 * i))
    );
}

#[tokio::test]
async fn a_correction_changes_only_its_own_column_and_revision() {
    let fx = Fixture::new().await;
    let ids = fx.seed(&[]).await;
    let before = result(&fx.t12(ask()).await);

    let mut corrected = month(PROPERTY, PERIODS[8], 8, true);
    corrected.actuals[0].amount = "1500.00".parse().unwrap();
    let new_id = fx.correct(&corrected, ids[8].as_ref().unwrap()).await;
    let after = result(&fx.t12(ask()).await);

    for (i, revision) in after["revisions"].as_array().unwrap().iter().enumerate() {
        if i == 8 {
            assert_eq!(revision["revision_id"], json!(new_id));
            assert_eq!(revision["revision"], 2);
        } else {
            assert_eq!(revision, &before["revisions"][i]);
        }
    }
    let changed = |a: &Value, b: &Value| a != b;
    for r in after["actuals"]["rows"].as_array().unwrap() {
        let old = row(&before["actuals"], r["category"].as_str().unwrap());
        assert_eq!(
            without(&r["values"], 8),
            without(&old["values"], 8),
            "{}",
            r["category"]
        );
        assert_eq!(
            changed(&r["values"][8], &old["values"][8]),
            r["category"] == "Rental Income"
        );
    }
    assert_eq!(
        without(&after["actuals"]["noi"], 8),
        without(&before["actuals"]["noi"], 8)
    );
    let moved = 150_000 - (100_000 + 1_005 * 8);
    assert_eq!(
        after["actuals"]["noi"][8],
        dollars(67_000 + 996 * 8 + moved)
    );
    assert_eq!(
        without(&after["variance"]["noi"], 8),
        without(&before["variance"]["noi"], 8)
    );
}

#[tokio::test]
async fn issued_only_prefers_the_issued_revision_over_a_later_unissued_one() {
    let fx = Fixture::new().await;
    let ids = fx.seed(&[]).await;
    // Issue every month's revision 1 except 2025-11.
    for (i, id) in ids.iter().enumerate() {
        if i != 5 {
            store::issue(&fx.pool, id.as_ref().unwrap()).await.unwrap();
        }
    }
    let mut corrected = month(PROPERTY, PERIODS[11], 11, true);
    corrected.actuals[0].amount = "2000.00".parse().unwrap();
    let rev2 = fx.correct(&corrected, ids[11].as_ref().unwrap()).await;

    let latest = result(&fx.t12(ask()).await);
    assert_eq!(latest["revisions"][11]["revision_id"], json!(rev2));
    assert_eq!(latest["missing_periods"], json!([]));

    let mut input = ask();
    input["revision_policy"] = json!("issued_only");
    let envelope = fx.t12(input).await;
    assert_eq!(envelope.status, "review_required");
    let issued = result(&envelope);
    assert_eq!(issued["revisions"][11]["revision_id"], json!(ids[11]));
    assert_eq!(issued["revisions"][11]["revision"], 1);
    assert_eq!(issued["actuals"]["noi"][11], dollars(67_000 + 996 * 11));
    assert_ne!(issued["actuals"]["noi"][11], latest["actuals"]["noi"][11]);
    // A month whose only revision is unissued is missing, not silently used.
    assert_eq!(issued["missing_periods"], json!(["2025-11"]));
    assert!(issued["actuals"]["noi"][5].is_null());

    // Issuing the correction makes it the issued revision.
    store::issue(&fx.pool, &rev2).await.unwrap();
    let mut input = ask();
    input["revision_policy"] = json!("issued_only");
    let reissued = result(&fx.t12(input).await);
    assert_eq!(reissued["revisions"][11]["revision_id"], json!(rev2));

    // An explicit "latest" matches the default.
    let mut input = ask();
    input["revision_policy"] = json!("latest");
    assert_eq!(result(&fx.t12(input).await), latest);
}

#[tokio::test]
async fn budgets_appear_only_when_every_present_period_has_budget_rows() {
    let fx = Fixture::new().await;
    // 2025-10 has actuals but no budget rows; 2025-09 is absent entirely.
    for (i, period) in PERIODS.iter().enumerate() {
        match i {
            3 => {}
            4 => {
                fx.import(&month(PROPERTY, period, i as i64, false)).await;
            }
            _ => {
                fx.import(&month(PROPERTY, period, i as i64, true)).await;
            }
        }
    }
    let out = result(&fx.t12(ask()).await);
    assert!(out["budgets"].is_null());
    assert!(out["variance"].is_null());
    assert!(out["actuals"]["noi"][4].is_string());
    assert_eq!(out["missing_periods"], json!(["2025-09"]));
}

#[tokio::test]
async fn a_missing_month_alone_does_not_remove_the_budgets_block() {
    let fx = Fixture::new().await;
    fx.seed(&[0]).await;
    let out = result(&fx.t12(ask()).await);
    assert!(out["budgets"]["noi"][0].is_null());
    assert_eq!(out["budgets"]["noi"][1], "645.00");
    assert_eq!(out["budgets"]["noi_total"], dollars(64_500 * 11));
    assert!(out["variance"]["noi"][0].is_null());
}

#[tokio::test]
async fn differently_cased_categories_share_one_row() {
    let fx = Fixture::new().await;
    // 2026-04: lower case and padded upper case on separate accounts.
    fx.import(&dataset(
        PROPERTY,
        "2026-04",
        json!([
            line("4000", "rental income", 100_000),
            line("4001", " RENTAL INCOME ", 2_500),
            line("5000", "payroll", 30_000)
        ]),
        json!([]),
    ))
    .await;
    fx.import(&dataset(
        PROPERTY,
        "2026-05",
        json!([
            line("4000", "Rental Income", 101_000),
            line("5000", "Payroll", 30_000)
        ]),
        json!([]),
    ))
    .await;
    let out = result(&fx.t12(ask()).await);
    let actuals = &out["actuals"];
    assert_eq!(categories(actuals), ["Rental Income", "Payroll"]);
    let rent = row(actuals, "Rental Income");
    assert_eq!(rent["values"][10], "1025.00");
    assert_eq!(rent["values"][11], "1010.00");
    assert_eq!(rent["total"], "2035.00");
    assert_eq!(actuals["noi"][10], "725.00");
    assert!(out["budgets"].is_null());
}

#[tokio::test]
async fn rows_are_ordered_and_unmapped_is_kept_out_of_noi() {
    let fx = Fixture::new().await;
    fx.import(&dataset(
        PROPERTY,
        "2026-05",
        json!([
            line("9100", "zebra", 700),
            line("9000", "Landscaping", 600),
            line("5300", "Taxes", 5_000),
            line("5000", "Payroll", 30_000),
            line("8000", "unmapped", 12_345),
            line("4100", "Other Income", 1_000),
            line("4000", "Rental Income", 100_000),
            line("4500", "Concessions", -2_000),
            line("5900", "Insurance", 0)
        ]),
        json!([]),
    ))
    .await;
    let out = result(&fx.t12(ask()).await);
    let actuals = &out["actuals"];
    // Revenue, then expenses in ontology order, then the rest alphabetically;
    // the all-zero Insurance row is omitted.
    assert_eq!(
        categories(actuals),
        [
            "Rental Income",
            "Concessions",
            "Other Income",
            "Payroll",
            "Taxes",
            "landscaping",
            "Unmapped",
            "zebra"
        ]
    );
    assert_eq!(row(actuals, "landscaping")["class"], "expense");
    assert_eq!(row(actuals, "Unmapped")["class"], "unmapped");
    assert_eq!(row(actuals, "Concessions")["values"][11], "-20.00");
    // NOI = revenue - expenses, with unmapped excluded and reported apart.
    let revenue = 100_000 - 2_000 + 1_000;
    let expenses = 30_000 + 5_000 + 600 + 700;
    assert_eq!(actuals["noi"][11], dollars(revenue - expenses));
    assert_eq!(actuals["unmapped"][11], "123.45");
    // A nonzero unmapped total is a review reason alongside the missing months.
    assert_eq!(
        out["review_reasons"],
        json!(["missing_periods", "unmapped_accounts"])
    );
    assert!(actuals["noi"][0].is_null());
}

#[tokio::test]
async fn nonzero_unmapped_alone_requires_review() {
    let fx = Fixture::new().await;
    for (i, period) in PERIODS.iter().enumerate() {
        let mut data = month(PROPERTY, period, i as i64, true);
        match i {
            4 => data
                .actuals
                .push(serde_json::from_value(line("8000", "Unmapped", 250)).unwrap()),
            // A zero unmapped total is not a reason.
            7 => data
                .actuals
                .push(serde_json::from_value(line("8000", "unmapped", 0)).unwrap()),
            _ => {}
        }
        fx.import(&data).await;
    }
    let envelope = fx.t12(ask()).await;
    assert_eq!(envelope.status, "review_required");
    let out = result(&envelope);
    assert_eq!(out["review_reasons"], json!(["unmapped_accounts"]));
    assert_eq!(out["missing_periods"], json!([]));
    assert_eq!(out["actuals"]["unmapped"][4], "2.50");
    assert_eq!(out["actuals"]["noi"][4], dollars(67_000 + 996 * 4));
    assert_eq!(row(&out["actuals"], "Unmapped")["total"], "2.50");
}

#[tokio::test]
async fn refusals_name_their_cause() {
    let fx = Fixture::new().await;
    fx.seed(&[]).await;

    let mut unknown = ask();
    unknown["property"] = json!("Nowhere Plaza");
    assert_eq!(code(&fx.t12(unknown).await), "NOT_FOUND");

    // Revisions that exist only outside the window do not count.
    let mut later = ask();
    later["end_period"] = json!("2028-05");
    assert_eq!(code(&fx.t12(later).await), "NOT_FOUND");

    let mut short = ask();
    short["end_period"] = json!("2026-5");
    assert_eq!(code(&fx.t12(short).await), "INVALID_INPUT");
    for bad in ["2026-13", "2026-05-01", "May 2026", " 2026-05"] {
        let mut input = ask();
        input["end_period"] = json!(bad);
        assert_eq!(code(&fx.t12(input).await), "INVALID_INPUT", "{bad}");
    }

    let mut policy = ask();
    policy["revision_policy"] = json!("whatever");
    assert_eq!(code(&fx.t12(policy).await), "INVALID_INPUT");
    let mut extra = ask();
    extra["database"] = json!("/tmp/other.sqlite");
    assert_eq!(code(&fx.t12(extra).await), "INVALID_INPUT");
    assert_eq!(
        code(&fx.t12(json!({"property": PROPERTY})).await),
        "INVALID_INPUT"
    );

    // With the database missing, the refusal is DATABASE_REQUIRED.
    assert_eq!(code(&run(None, ask()).await), "DATABASE_REQUIRED");
}

#[tokio::test]
async fn catalog_lists_eleven_functions_with_t12_last_and_schemas_agree() {
    let envelope = catalog::invoke(&Call {
        function: "describe".into(),
        version: None,
        input: json!({}),
        database: None,
    })
    .await;
    let described = result(&envelope);
    let entries = described["calculations"].as_array().unwrap();
    assert_eq!(entries.len(), 11);
    let last = &entries[10];
    assert_eq!(last["name"], "t12");
    assert_eq!(last["version"], "1.0.0");
    assert_eq!(last["stateful"], true);

    let schema = &last["input_schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["property", "end_period"]));
    let properties = schema["properties"].as_object().unwrap();
    for key in ["property", "end_period", "revision_policy"] {
        assert!(properties.contains_key(key), "{key}");
    }

    // Every top-level output key is described, and nothing else is emitted.
    let fx = Fixture::new().await;
    fx.seed(&[]).await;
    let out = result(&fx.t12(ask()).await);
    let described_keys: Vec<&String> = last["output_schema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    let mut emitted: Vec<&String> = out.as_object().unwrap().keys().collect();
    emitted.sort();
    let mut described_sorted = described_keys.clone();
    described_sorted.sort();
    assert_eq!(emitted, described_sorted);
}
