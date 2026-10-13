//! Every catalog invocation gets a trace id and a citation, and is recorded
//! (database, JSONL, or nowhere, as the host configured) so the citation can
//! be resolved later with `get_provenance` / `boxscore-exact trace`.
//! Synthetic data only.
//!
//! Tests that depend on `BOXSCORE_TRACE_DIR` or `BOXSCORE_TRACE_INPUTS` run
//! the real binary with an explicit environment instead of mutating this
//! process's environment, so they cannot race the in-process tests.

use boxscore::exact::{
    catalog::{self, Call},
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

fn variance_input() -> Value {
    json!({
        "currency": "USD", "expense_convention": "positive_costs",
        "actuals": [line("4000", "rental income", "100.10")],
        "budgets": [line("4000", "rental income", "90.05")],
    })
}

fn dataset() -> Value {
    json!({
        "property": "Synthetic Trace", "period": "2026-05",
        "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
        "actuals": [line("4000", "rental income", "100.10"), line("5000", "payroll", "30.00")],
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

async fn provenance(db: &Path, trace_id: &str) -> catalog::Envelope {
    catalog::invoke(&call(
        "get_provenance",
        json!({"trace_id": trace_id}),
        Some(db),
    ))
    .await
}

fn short(hash: &str) -> &str {
    &hash[..12]
}

fn is_uuid_v4(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.iter().map(|g| g.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
        && groups[2].starts_with('4')
}

/// Run the real binary with neither trace variable inherited, plus `envs`.
fn run(args: &[&str], stdin: &[u8], envs: &[(&str, &Path)]) -> (i32, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"));
    command
        .args(args)
        .env_remove("BOXSCORE_TRACE_DIR")
        .env_remove("BOXSCORE_TRACE_INPUTS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
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
fn stateless_call_without_a_trace_store_still_has_a_trace_id_and_citation() {
    let bytes = serde_json::to_vec(&variance_input()).unwrap();
    let (code, envelope) = run(&["call", "--function", "noi_variance"], &bytes, &[]);
    assert_eq!(code, 0, "{envelope}");
    let trace_id = envelope["trace_id"].as_str().unwrap();
    assert!(is_uuid_v4(trace_id), "{trace_id}");
    assert_eq!(envelope["trace_store"], "none");
    assert!(envelope.get("trace_warning").is_none());
    let input = envelope["input_sha256"].as_str().unwrap();
    let output = envelope["output_sha256"].as_str().unwrap();
    assert_eq!(input, digest(&bytes));
    assert_eq!(
        envelope["citation"],
        format!(
            "boxscore noi_variance@1.0.0 · trace {trace_id} · input sha256:{}… · output sha256:{}…",
            short(input),
            short(output)
        )
    );

    // A refusal cites its code instead of an output hash.
    let (code, refused) = run(&["call", "--function", "cap_rate"], b"{}", &[]);
    assert_eq!(code, 2);
    assert_eq!(refused["trace_store"], "none");
    assert_eq!(
        refused["citation"],
        format!(
            "boxscore cap_rate@unknown · trace {} · input sha256:{}… · refused UNKNOWN_FUNCTION",
            refused["trace_id"].as_str().unwrap(),
            short(&digest(b"{}"))
        )
    );
}

#[tokio::test]
async fn trace_fields_follow_computed_at_and_ids_are_unique() {
    let first = catalog::invoke(&call("noi_variance", variance_input(), None)).await;
    let second = catalog::invoke(&call("noi_variance", variance_input(), None)).await;
    assert_ne!(first.trace_id, second.trace_id);
    // The trace id changes per call; the hashes do not.
    assert_eq!(first.output_sha256, second.output_sha256);
    let text = serde_json::to_string(&first).unwrap();
    let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap();
    assert!(at("computed_at") < at("trace_id"));
    assert!(at("trace_id") < at("trace_store"));
    assert!(at("trace_store") < at("citation"));
    assert!(at("citation") < at("status"));
    assert!(at("output_sha256") < at("computed_at"));
}

#[test]
fn trace_dir_appends_one_jsonl_line_per_call_without_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let env = [("BOXSCORE_TRACE_DIR", dir.path())];
    let (code, envelope) = run(&["call", "--function", "describe"], b"", &env);
    assert_eq!(code, 0);
    assert_eq!(envelope["trace_store"], "jsonl");
    let log = dir.path().join("invocations.jsonl");
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1);
    let record: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(record["trace_id"], envelope["trace_id"]);
    assert_eq!(record["function"], "describe");
    assert_eq!(record["version"], "1.2.0");
    assert_eq!(record["caller"], "cli");
    assert_eq!(record["status"], "calculated");
    assert_eq!(record["input_sha256"], envelope["input_sha256"]);
    assert_eq!(record["output_sha256"], envelope["output_sha256"]);
    assert_eq!(record["input_json"], Value::Null);
    assert_eq!(record["error_code"], Value::Null);
    for key in ["producer_version", "started_at", "finished_at"] {
        assert!(record[key].is_string(), "{key}");
    }

    // The protocol host records itself as the caller, refusals included.
    let (code, refused) = run(
        &["protocol"],
        br#"{"contract_version":"plat.ops/1","operation":"review_period","revision_id":"x"}"#,
        &env,
    );
    assert_eq!(code, 2);
    assert_eq!(refused["trace_store"], "jsonl");
    let text = std::fs::read_to_string(&log).unwrap();
    let second: Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert_eq!(second["caller"], "protocol");
    assert_eq!(second["status"], "refused");
    assert_eq!(second["error_code"], "DATABASE_REQUIRED");

    // A trace directory that does not exist is reported, not fatal.
    let missing = dir.path().join("missing");
    let (code, envelope) = run(
        &["call", "--function", "describe"],
        b"",
        &[("BOXSCORE_TRACE_DIR", &missing)],
    );
    assert_eq!(code, 0);
    assert_eq!(envelope["status"], "calculated");
    assert_eq!(envelope["trace_store"], "none");
    assert!(envelope["trace_warning"]
        .as_str()
        .unwrap()
        .contains("BOXSCORE_TRACE_DIR"));
}

#[tokio::test]
async fn stateful_call_is_recorded_and_get_provenance_returns_the_revision_sources() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let sources = json!({"actuals.csv": "sha256:synthetic-a", "budgets.csv": "sha256:synthetic-b"});
    let imported = catalog::invoke(&call(
        "import_period",
        json!({"dataset": dataset(), "sources": sources}),
        Some(&db),
    ))
    .await;
    assert_eq!(imported.status, "calculated", "{:?}", imported.error);
    assert_eq!(imported.trace_store, "database");
    assert!(imported.trace_warning.is_none());
    let revision = imported.result.as_ref().unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(imported.citation.starts_with(&format!(
        "boxscore import_period@1.1.0 · trace {}",
        imported.trace_id
    )));
    assert!(imported
        .citation
        .ends_with(&format!(" · revision {revision}")));

    let found = provenance(&db, &imported.trace_id).await;
    assert_eq!(found.status, "calculated", "{:?}", found.error);
    assert_eq!(found.version, Some("1.0.0"));
    assert_eq!(found.trace_store, "database");
    let result = found.result.unwrap();
    let record = &result["trace"];
    assert_eq!(record["trace_id"], imported.trace_id.as_str());
    assert_eq!(record["function"], "import_period");
    assert_eq!(record["version"], "1.1.0");
    assert_eq!(record["caller"], "test");
    assert_eq!(record["status"], "calculated");
    assert_eq!(record["input_sha256"], imported.input_sha256.as_str());
    assert_eq!(
        record["output_sha256"].as_str(),
        imported.output_sha256.as_deref()
    );
    assert_eq!(record["started_at"], imported.computed_at.as_str());
    assert_eq!(record["revision_id"], revision.as_str());
    assert_eq!(record["report_id"], Value::Null);
    assert_eq!(result["revision"]["id"], revision.as_str());
    assert_eq!(result["revision"]["property"], "Synthetic Trace");
    assert_eq!(result["revision"]["period"], "2026-05");
    assert_eq!(result["revision"]["revision"], 1);
    assert_eq!(result["revision"]["parent_id"], Value::Null);
    assert_eq!(result["revision"]["reason"], Value::Null);
    // The caller's sources are recorded only under `supplied`, beside the
    // hash the server computed of the canonical dataset.
    let recorded = result["revision"]["sources"].clone();
    assert_eq!(recorded["supplied"], sources);
    assert_eq!(recorded.as_object().unwrap().len(), 2, "{recorded}");
    assert_eq!(
        recorded["canonical_json_sha256"].as_str().unwrap().len(),
        64,
        "{recorded}"
    );
    assert_eq!(result["report"], Value::Null);
    // The result agrees with the published output schema.
    let schema = catalog::find("get_provenance", None)
        .unwrap()
        .output_schema();
    for (key, properties) in [
        ("trace", &schema["properties"]["trace"]),
        ("revision", &schema["properties"]["revision"]["anyOf"][0]),
    ] {
        for required in properties["required"].as_array().unwrap() {
            assert!(
                result[key].get(required.as_str().unwrap()).is_some(),
                "{key}.{required}"
            );
        }
        for field in result[key].as_object().unwrap().keys() {
            assert!(
                properties["properties"].get(field).is_some(),
                "{key}.{field}"
            );
        }
    }
    // The provenance lookup does not itself claim the revision.
    assert!(!found.citation.contains(" · revision "));

    // A metric read from the stored revision cites the revision it read.
    let occupancy = catalog::invoke(&call(
        "occupancy",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(occupancy.status, "calculated", "{:?}", occupancy.error);
    assert!(occupancy
        .citation
        .ends_with(&format!(" · revision {revision}")));
    let found = provenance(&db, &occupancy.trace_id).await;
    assert_eq!(found.result.unwrap()["revision"]["sources"], recorded);
}

#[tokio::test]
async fn refused_stateful_call_is_recorded_with_its_error_code() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let refused = catalog::invoke(&call(
        "review_period",
        json!({"revision_id": "no-such-revision"}),
        Some(&db),
    ))
    .await;
    assert!(refused.refused());
    assert_eq!(refused.error.as_ref().unwrap()["code"], "NOT_FOUND");
    assert_eq!(refused.trace_store, "database");
    assert!(refused.citation.ends_with(" · refused NOT_FOUND"));

    let found = provenance(&db, &refused.trace_id).await;
    let result = found.result.unwrap();
    assert_eq!(result["trace"]["function"], "review_period");
    assert_eq!(result["trace"]["status"], "refused");
    assert_eq!(result["trace"]["error_code"], "NOT_FOUND");
    assert_eq!(result["trace"]["output_sha256"], Value::Null);
    assert_eq!(result["trace"]["revision_id"], Value::Null);
    assert_eq!(result["revision"], Value::Null);
}

#[tokio::test]
async fn issue_report_records_the_report_and_provenance_returns_its_hash() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let imported = catalog::invoke(&call(
        "import_period",
        json!({"dataset": dataset()}),
        Some(&db),
    ))
    .await;
    let revision = imported.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let issued = catalog::invoke(&call(
        "issue_report",
        json!({"revision_id": revision}),
        Some(&db),
    ))
    .await;
    assert_eq!(issued.status, "calculated", "{:?}", issued.error);
    let report = issued.result.as_ref().unwrap()["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(issued
        .citation
        .ends_with(&format!(" · revision {revision} · report {report}")));

    let found = provenance(&db, &issued.trace_id).await;
    let result = found.result.unwrap();
    assert_eq!(result["trace"]["report_id"], report.as_str());
    assert_eq!(result["trace"]["revision_id"], revision.as_str());
    let pool = store::open(&db, true).await.unwrap();
    let (body_sha256, created_at): (String, String) =
        sqlx::query_as("SELECT body_sha256, created_at FROM exact_reports WHERE id=?")
            .bind(&report)
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    assert_eq!(result["report"]["id"], report.as_str());
    assert_eq!(result["report"]["body_sha256"], body_sha256.as_str());
    assert_eq!(result["report"]["created_at"], created_at.as_str());
    assert!(result["revision"]["sources"]["canonical_json_sha256"].is_string());
}

#[tokio::test]
async fn unknown_trace_id_refuses_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let missing = provenance(&db, "00000000-0000-4000-8000-000000000000").await;
    assert!(missing.refused());
    assert_eq!(missing.error.as_ref().unwrap()["code"], "NOT_FOUND");
    // Even the failed lookup is on the record.
    assert_eq!(missing.trace_store, "database");

    let stateless = catalog::invoke(&call(
        "get_provenance",
        json!({"trace_id": missing.trace_id}),
        None,
    ))
    .await;
    assert_eq!(
        stateless.error.as_ref().unwrap()["code"],
        "DATABASE_REQUIRED"
    );
    let extra = provenance_with(&db, json!({"trace_id": "x", "database": "/tmp/x"})).await;
    assert_eq!(extra.error.as_ref().unwrap()["code"], "INVALID_INPUT");
}

async fn provenance_with(db: &Path, input: Value) -> catalog::Envelope {
    catalog::invoke(&call("get_provenance", input, Some(db))).await
}

#[test]
fn input_json_is_kept_only_when_the_host_opts_in_and_trace_matches_call() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("exact.sqlite");
    let db_arg = db.to_str().unwrap();
    let (code, _) = run(&["init", "--database", db_arg], b"", &[]);
    assert_eq!(code, 0);
    let input = variance_input();
    let bytes = serde_json::to_vec(&input).unwrap();

    let (code, plain) = run(
        &["call", "--database", db_arg, "--function", "noi_variance"],
        &bytes,
        &[],
    );
    assert_eq!(code, 0, "{plain}");
    assert_eq!(plain["trace_store"], "database");
    let plain_id = plain["trace_id"].as_str().unwrap();
    let (code, traced) = run(&["trace", "--database", db_arg, "--id", plain_id], b"", &[]);
    assert_eq!(code, 0, "{traced}");
    assert_eq!(traced["function"], "get_provenance");
    assert_eq!(traced["result"]["trace"]["caller"], "cli");
    assert_eq!(traced["result"]["trace"]["input_json"], Value::Null);
    assert_eq!(traced["result"]["trace"]["input_sha256"], digest(&bytes));

    // `trace` is `call --function get_provenance` with the id as input.
    let (code, called) = run(
        &["call", "--database", db_arg, "--function", "get_provenance"],
        &serde_json::to_vec(&json!({"trace_id": plain_id})).unwrap(),
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(called["result"], traced["result"]);
    assert_eq!(called["output_sha256"], traced["output_sha256"]);
    for key in [
        "contract_version",
        "function",
        "version",
        "producer",
        "status",
    ] {
        assert_eq!(called[key], traced[key], "{key}");
    }

    let one = Path::new("1");
    let (code, kept) = run(
        &["call", "--database", db_arg, "--function", "noi_variance"],
        &bytes,
        &[("BOXSCORE_TRACE_INPUTS", one)],
    );
    assert_eq!(code, 0);
    let (_, traced) = run(
        &[
            "trace",
            "--database",
            db_arg,
            "--id",
            kept["trace_id"].as_str().unwrap(),
        ],
        b"",
        &[],
    );
    let stored = traced["result"]["trace"]["input_json"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(stored).unwrap(), input);

    let (code, unknown) = run(&["trace", "--database", db_arg, "--id", "nope"], b"", &[]);
    assert_eq!(code, 2);
    assert_eq!(unknown["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn invocations_are_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let described = catalog::invoke(&call("describe", json!({}), Some(&db))).await;
    assert_eq!(described.trace_store, "database");
    let pool = store::open(&db, false).await.unwrap();
    assert!(
        sqlx::query("UPDATE exact_invocations SET status='calculated', caller='forged'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("DELETE FROM exact_invocations")
        .execute(&pool)
        .await
        .is_err());
    let (count, caller): (i64, String) =
        sqlx::query_as("SELECT count(*), max(caller) FROM exact_invocations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((count, caller.as_str()), (1, "test"));
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 2);
    pool.close().await;
}

#[tokio::test]
async fn schema_one_database_refuses_and_tracing_failure_never_fails_a_calculation() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let pool = store::open(&db, false).await.unwrap();
    sqlx::query("PRAGMA user_version=1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let refused = store::open(&db, true).await.unwrap_err();
    assert_eq!(refused.code, "SCHEMA_MISMATCH");
    assert!(
        refused.message.contains("created by exact-cent schema 1"),
        "{}",
        refused.message
    );
    assert!(refused.message.contains("Re-import"));
    assert!(refused.message.contains("reviewed copy migration"));

    // A stateless calculation still answers; the trace failure is explained.
    let described = catalog::invoke(&call("describe", json!({}), Some(&db))).await;
    assert_eq!(described.status, "calculated");
    assert_eq!(described.trace_store, "none");
    let warning = described.trace_warning.unwrap();
    assert!(warning.contains("SCHEMA_MISMATCH") && warning.contains("schema 1"));
    assert!(described.citation.contains(&described.trace_id));

    // A stateful one refuses for the schema, and is not recorded either.
    let review = catalog::invoke(&call(
        "review_period",
        json!({"revision_id": "x"}),
        Some(&db),
    ))
    .await;
    assert_eq!(review.error.as_ref().unwrap()["code"], "SCHEMA_MISMATCH");
    assert_eq!(review.trace_store, "none");
    assert!(review.trace_warning.is_some());

    // A database path that does not exist is reported the same way.
    let gone = catalog::invoke(&call(
        "noi_variance",
        variance_input(),
        Some(&dir.path().join("missing.sqlite")),
    ))
    .await;
    assert_eq!(gone.status, "calculated");
    assert_eq!(gone.trace_store, "none");
    assert!(gone.trace_warning.is_some());
    assert!(!dir.path().join("missing.sqlite").exists());
}

#[tokio::test]
async fn blank_caller_is_recorded_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_database(dir.path()).await;
    let mut anonymous = call("describe", json!({}), Some(&db));
    anonymous.caller = "  ".into();
    let described = catalog::invoke(&anonymous).await;
    let found = provenance(&db, &described.trace_id).await;
    assert_eq!(found.result.unwrap()["trace"]["caller"], "unknown");
    assert_eq!(Call::default().caller, "unknown");
}

#[tokio::test]
async fn describe_lists_twelve_functions_ending_with_get_provenance() {
    let envelope = catalog::invoke(&call("describe", json!({}), None)).await;
    let calculations = envelope.result.unwrap()["calculations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(calculations.len(), 12);
    assert_eq!(catalog::CATALOG.len(), 12);
    let last = &calculations[11];
    assert_eq!(last["name"], "get_provenance");
    assert_eq!(last["version"], "1.0.0");
    assert_eq!(last["stateful"], true);
    assert_eq!(last["input_schema"]["required"], json!(["trace_id"]));
}
