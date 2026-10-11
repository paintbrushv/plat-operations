//! The calculation catalog is the API boundary every connector builds on.
//! These tests pin the envelope, the determinism of its hashes, the refusal
//! codes, and the agreement between each function's JSON Schema and the
//! inputs the implementation actually accepts.

use boxscore::exact::{
    catalog::{self, Call, CATALOG},
    digest, store,
};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn line(code: &str, category: &str, amount: &str) -> Value {
    json!({"account_code": code, "account_name": code, "category": category, "amount": amount})
}

fn variance_input(actual: &str, budget: &str) -> Value {
    json!({
        "currency": "USD", "expense_convention": "positive_costs",
        "actuals": [line("4000", "rental income", actual)],
        "budgets": [line("4000", "rental income", budget)],
    })
}

fn dataset(rent: &str) -> Value {
    json!({
        "property": "Synthetic Oak", "period": "2026-05",
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [line("4000", "rental income", rent), line("5000", "payroll", "30.00")],
        "budgets": [line("4000", "rental income", "90.05"), line("5000", "payroll", "25.00")],
        "snapshot": {
            "as_of_date": "2026-05-31", "occupied_units": 9, "vacant_units": 1, "down_units": 0,
            "market_rent_total": "100.10", "in_place_rent_total": null,
            "delinquent_amount": null, "prepaid_amount": null, "concessions_amount": null
        }
    })
}

fn call(function: &str, input: Value, database: Option<&Path>) -> Call {
    Call {
        function: function.into(),
        version: None,
        input,
        database: database.map(Path::to_path_buf),
        caller: "test".into(),
    }
}

async fn fresh_database(dir: &Path) -> PathBuf {
    let path = dir.join("exact.sqlite");
    store::create(&path).await.unwrap().close().await;
    path
}

#[tokio::test]
async fn describe_lists_every_function_with_schemas_and_a_hashed_envelope() {
    let envelope = catalog::invoke(&call("describe", json!({}), None)).await;
    assert_eq!(envelope.status, "calculated");
    assert_eq!(envelope.version, Some("1.0.0"));
    assert_eq!(envelope.contract_version, "plat.ops/1");
    assert_eq!(envelope.producer["arithmetic"], "checked_i64_cents");
    assert_eq!(envelope.producer["name"], "boxscore-exact");
    assert_eq!(envelope.input_sha256, digest(b"{}"));
    let result = envelope.result.clone().unwrap();
    assert_eq!(
        envelope.output_sha256.as_deref(),
        Some(digest(&catalog::canonical(&result)).as_str())
    );
    let names: Vec<&str> = result["calculations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "describe",
            "noi_variance",
            "import_period",
            "review_period",
            "issue_report",
            "list_revisions",
            "occupancy",
            "economic_occupancy",
            "delinquency",
            "close_readiness",
            "t12",
            "get_provenance"
        ]
    );
    for entry in result["calculations"].as_array().unwrap() {
        assert_eq!(entry["input_schema"]["type"], "object", "{}", entry["name"]);
        assert_eq!(
            entry["output_schema"]["type"], "object",
            "{}",
            entry["name"]
        );
        assert!(entry["version"].as_str().unwrap().starts_with("1."));
    }
    assert_eq!(
        result["calculations"].as_array().unwrap().len(),
        CATALOG.len()
    );
    assert_eq!(CATALOG.len(), 12);
}

#[tokio::test]
async fn noi_variance_envelope_is_deterministic_and_carries_review_status() {
    let first = catalog::invoke(&call(
        "noi_variance",
        variance_input("100.10", "90.05"),
        None,
    ))
    .await;
    let second = catalog::invoke(&call(
        "noi_variance",
        variance_input("100.10", "90.05"),
        None,
    ))
    .await;
    assert_eq!(first.status, "calculated");
    assert_eq!(first.function, "noi_variance");
    assert_eq!(first.result, second.result);
    assert_eq!(first.output_sha256, second.output_sha256);
    assert_eq!(first.input_sha256, second.input_sha256);
    assert_eq!(
        first.result.as_ref().unwrap()["noi_bridge"]["noi_variance"],
        "10.05"
    );
    // The hash is over the result only, so wall-clock time never changes it.
    assert_eq!(
        first.output_sha256.as_deref().unwrap(),
        digest(&catalog::canonical(first.result.as_ref().unwrap())).as_str()
    );
    assert!(!first.computed_at.is_empty());

    let mut negative = variance_input("100.00", "90.00");
    negative["actuals"]
        .as_array_mut()
        .unwrap()
        .push(line("5000", "payroll", "-5.00"));
    let flagged = catalog::invoke(&call("noi_variance", negative, None)).await;
    assert_eq!(flagged.status, "review_required");
    assert_eq!(
        flagged.result.unwrap()["review_reasons"],
        json!(["negative_net_expenses"])
    );
}

#[tokio::test]
async fn refusals_keep_the_envelope_and_name_the_cause() {
    let unknown = catalog::invoke(&call("cap_rate", json!({}), None)).await;
    assert!(unknown.refused());
    assert_eq!(unknown.version, None);
    assert_eq!(unknown.error.as_ref().unwrap()["code"], "UNKNOWN_FUNCTION");
    assert!(unknown.result.is_none() && unknown.output_sha256.is_none());

    let mut pinned = call("describe", json!({}), None);
    pinned.version = Some("2.0.0".into());
    let mismatch = catalog::invoke(&pinned).await;
    assert_eq!(mismatch.error.as_ref().unwrap()["code"], "VERSION_MISMATCH");
    assert_eq!(mismatch.version, Some("1.0.0"));

    let stateless =
        catalog::invoke(&call("review_period", json!({"revision_id": "x"}), None)).await;
    assert_eq!(
        stateless.error.as_ref().unwrap()["code"],
        "DATABASE_REQUIRED"
    );

    let extra = catalog::invoke(&call("describe", json!({"verbose": true}), None)).await;
    assert_eq!(extra.error.as_ref().unwrap()["code"], "INVALID_INPUT");

    let mut bad_money = variance_input("100.10", "90.05");
    bad_money["actuals"][0]["amount"] = json!(100.1);
    let refused = catalog::invoke(&call("noi_variance", bad_money, None)).await;
    assert_eq!(refused.error.as_ref().unwrap()["code"], "INVALID_INPUT");
    assert_eq!(refused.status, "refused");
}

#[tokio::test]
async fn stateful_functions_round_trip_import_review_issue_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;

    let imported = catalog::invoke(&call(
        "import_period",
        json!({"dataset": dataset("100.10"), "sources": {"actuals.csv": "sha256:abc"}}),
        Some(&db),
    ))
    .await;
    assert_eq!(imported.status, "calculated", "{:?}", imported.error);
    let revision = imported.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    let review = catalog::invoke(&call(
        "review_period",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(review.status, "calculated", "{:?}", review.error);
    let review_result = review.result.unwrap();
    assert_eq!(
        review_result["variance"]["noi_bridge"]["noi_variance"],
        "5.05"
    );
    assert_eq!(review_result["sources"]["actuals.csv"], "sha256:abc");
    assert_eq!(review_result["human_approval"], "not_granted");

    let issued = catalog::invoke(&call(
        "issue_report",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(issued.status, "calculated");
    let report_id = issued.result.unwrap()["report_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A correction needs the current revision and a reason; the listing shows
    // the chain and the issued report on the first revision.
    let rejected = catalog::invoke(&call(
        "import_period",
        json!({"dataset": dataset("110.10")}),
        Some(&db),
    ))
    .await;
    assert_eq!(rejected.error.as_ref().unwrap()["code"], "STALE_REVISION");
    let corrected = catalog::invoke(&call(
        "import_period",
        json!({"dataset": dataset("110.10"), "supersedes": revision, "reason": "June accrual"}),
        Some(&db),
    ))
    .await;
    assert_eq!(corrected.status, "calculated", "{:?}", corrected.error);

    let listed = catalog::invoke(&call(
        "list_revisions",
        json!({"property": "Synthetic Oak"}),
        Some(&db),
    ))
    .await;
    assert_eq!(listed.status, "calculated", "{:?}", listed.error);
    let revisions = listed.result.unwrap()["revisions"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0]["revision"], 1);
    assert_eq!(revisions[0]["reports"][0]["id"], report_id);
    assert_eq!(revisions[1]["revision"], 2);
    assert_eq!(revisions[1]["parent_id"], revisions[0]["id"]);
    assert_eq!(revisions[1]["reason"], "June accrual");
    assert_eq!(revisions[1]["reports"], json!([]));

    let filtered = catalog::invoke(&call(
        "list_revisions",
        json!({"period": "2026-06"}),
        Some(&db),
    ))
    .await;
    assert_eq!(filtered.result.unwrap()["revisions"], json!([]));

    let missing = catalog::invoke(&call(
        "review_period",
        json!({"revision_id": "nope"}),
        Some(&db),
    ))
    .await;
    assert_eq!(missing.error.as_ref().unwrap()["code"], "NOT_FOUND");
}

/// Every golden input must be describable by its function's input schema:
/// no key outside `properties`, every `required` key present. This is the
/// guard that keeps hand-written schemas and serde types from drifting.
#[test]
fn golden_inputs_agree_with_their_input_schemas() {
    let goldens: Vec<(&str, Value)> = vec![
        ("describe", json!({})),
        ("noi_variance", variance_input("1.00", "2.00")),
        (
            "import_period",
            json!({"dataset": dataset("1.00"), "supersedes": "r1", "reason": "x", "sources": {}}),
        ),
        ("review_period", json!({"revision_id": "r1"})),
        ("issue_report", json!({"revision_id": "r1"})),
        (
            "list_revisions",
            json!({"property": "p", "period": "2026-05"}),
        ),
        ("close_readiness", json!({"period": "2026-05"})),
        (
            "close_readiness",
            json!({"period": "2026-05", "property": "p"}),
        ),
        (
            "t12",
            json!({"property": "p", "end_period": "2026-05", "revision_policy": "issued_only"}),
        ),
        ("t12", json!({"property": "p", "end_period": "2026-05"})),
        ("get_provenance", json!({"trace_id": "t1"})),
    ];
    // The snapshot metrics take either a stored revision or an inline
    // snapshot; each form is a golden input against the same schema.
    let snapshot = dataset("1.00")["snapshot"].clone();
    let mut goldens = goldens;
    for name in ["occupancy", "economic_occupancy", "delinquency"] {
        goldens.push((name, json!({"revision_id": "r1"})));
        goldens.push((name, json!({"snapshot": snapshot, "unit_count": 10})));
    }
    for (name, input) in goldens {
        let calc = catalog::find(name, None).unwrap();
        let schema = calc.input_schema();
        assert_eq!(schema["additionalProperties"], false, "{name}");
        let properties = schema["properties"].as_object().unwrap();
        for key in input.as_object().unwrap().keys() {
            assert!(properties.contains_key(key), "{name}: {key} not in schema");
        }
        for required in schema["required"].as_array().into_iter().flatten() {
            let key = required.as_str().unwrap();
            assert!(input.get(key).is_some(), "{name}: required {key} missing");
        }
        // Each oneOf branch must be satisfiable by exactly the golden inputs
        // that use its keys, and the nested snapshot keys must be known.
        if let Some(branches) = schema["oneOf"].as_array() {
            let matching = branches
                .iter()
                .filter(|branch| {
                    let present = |k: &str| input.get(k).is_some();
                    let required_ok = branch["required"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .all(|k| present(k.as_str().unwrap()));
                    let forbidden_ok = branch["properties"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(_, v)| **v == json!(false))
                        .all(|(k, _)| !present(k));
                    required_ok && forbidden_ok
                })
                .count();
            assert_eq!(matching, 1, "{name}: {input} must match exactly one form");
            if let Some(inline) = input.get("snapshot") {
                let snapshot_properties = properties["snapshot"]["properties"].as_object().unwrap();
                for key in inline.as_object().unwrap().keys() {
                    assert!(snapshot_properties.contains_key(key), "snapshot.{key}");
                }
            }
        }
        // Nested dataset keys are checked the same way.
        if name == "import_period" {
            let dataset_schema = &properties["dataset"];
            let dataset_properties = dataset_schema["properties"].as_object().unwrap();
            for key in input["dataset"].as_object().unwrap().keys() {
                assert!(dataset_properties.contains_key(key), "dataset.{key}");
            }
        }
    }
    // The money pattern matches what Money::from_str accepts and rejects.
    let pattern = regex_lite(catalog::schemas::MONEY_PATTERN);
    for ok in ["0.00", "-5.01", "100.1", "12345678901234567.99"] {
        assert!(pattern(ok), "{ok}");
    }
    for bad in ["1,000.00", "+1.00", "1.", ".5", "1.000", "nan", " 1.00"] {
        assert!(!pattern(bad), "{bad}");
    }
}

/// Minimal matcher for the one pattern we publish, so the test needs no
/// regex dependency: optional '-', 1-17 digits, optional '.' + 1-2 digits.
fn regex_lite(pattern: &str) -> impl Fn(&str) -> bool {
    assert_eq!(pattern, "^-?[0-9]{1,17}(\\.[0-9]{1,2})?$");
    |s: &str| {
        let s = s.strip_prefix('-').unwrap_or(s);
        let (whole, fraction) = match s.split_once('.') {
            Some((w, f)) => (w, Some(f)),
            None => (s, None),
        };
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        digits(whole) && whole.len() <= 17 && fraction.is_none_or(|f| digits(f) && f.len() <= 2)
    }
}

// ---------------------------------------------------------------------------
// Through the real binary: the protocol routes catalog operations, keeps the
// original `variance` shape, and `call` honours exit codes and --database.
// ---------------------------------------------------------------------------

fn run_binary(args: &[&str], stdin: &[u8]) -> (i32, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().unwrap(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn protocol_routes_catalog_operations_and_keeps_variance_shape() {
    let mut request = variance_input("0.30", "0.10");
    request["contract_version"] = json!("plat.ops/1");
    request["operation"] = json!("noi_variance");
    let bytes = serde_json::to_vec(&request).unwrap();
    let (code, envelope) = run_binary(&["protocol"], &bytes);
    assert_eq!(code, 0);
    assert_eq!(envelope["function"], "noi_variance");
    assert_eq!(envelope["version"], "1.0.0");
    assert_eq!(envelope["input_sha256"], digest(&bytes));
    assert_eq!(envelope["result"]["noi_bridge"]["noi_variance"], "0.20");
    assert!(envelope["output_sha256"].is_string());

    request["operation"] = json!("variance");
    let (code, legacy) = run_binary(&["protocol"], &serde_json::to_vec(&request).unwrap());
    assert_eq!(code, 0);
    assert!(
        legacy.get("function").is_none(),
        "legacy shape must not change"
    );
    assert_eq!(legacy["result"]["noi_bridge"]["noi_variance"], "0.20");

    let (code, describe) = run_binary(
        &["protocol"],
        br#"{"contract_version":"plat.ops/1","operation":"describe"}"#,
    );
    assert_eq!(code, 0);
    assert_eq!(describe["result"]["calculations"][0]["name"], "describe");

    let (code, stateful) = run_binary(
        &["protocol"],
        br#"{"contract_version":"plat.ops/1","operation":"review_period","revision_id":"x"}"#,
    );
    assert_eq!(code, 2);
    assert_eq!(stateful["status"], "refused");
    assert_eq!(stateful["error"]["code"], "DATABASE_REQUIRED");

    let (code, pinned) = run_binary(
        &["protocol"],
        br#"{"contract_version":"plat.ops/1","operation":"describe","version":"9.9.9"}"#,
    );
    assert_eq!(
        (code, pinned["error"]["code"].as_str()),
        (2, Some("VERSION_MISMATCH"))
    );
}

#[test]
fn call_subcommand_runs_stateful_functions_against_the_given_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let db_arg = db.to_str().unwrap();
    let (code, created) = run_binary(&["init", "--database", db_arg], b"");
    assert_eq!((code, created["status"].as_str()), (0, Some("created")));

    let (code, imported) = run_binary(
        &["call", "--database", db_arg, "--function", "import_period"],
        &serde_json::to_vec(&json!({"dataset": dataset("100.10")})).unwrap(),
    );
    assert_eq!(code, 0, "{imported}");
    let revision = imported["result"]["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (code, reviewed) = run_binary(
        &[
            "call",
            "--database",
            db_arg,
            "--function",
            "review_period",
            "--version",
            "1.0.0",
        ],
        &serde_json::to_vec(&json!({"revision_id": revision})).unwrap(),
    );
    assert_eq!(code, 0, "{reviewed}");
    assert_eq!(reviewed["function"], "review_period");
    assert_eq!(
        reviewed["result"]["variance"]["noi_bridge"]["noi_variance"],
        "5.05"
    );

    // Empty stdin means an empty input object.
    let (code, described) = run_binary(&["call", "--function", "describe"], b"");
    assert_eq!(code, 0);
    assert_eq!(described["input_sha256"], digest(b"{}"));

    let (code, refused) = run_binary(&["call", "--function", "nope"], b"{}");
    assert_eq!((code, refused["status"].as_str()), (2, Some("refused")));
}
