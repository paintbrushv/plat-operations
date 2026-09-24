//! Phase B6 tests for the ask engine.
//!
//! All tests use MockProvider — no network calls are made.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use boxscore::{
    ask::{redact_residents, run_ask, tool_specs, AskStatus},
    db,
    model_provider::{
        ChatMessage, CompletionResponse, ContentBlock, ToolSpec, ToolUseProvider, Usage,
    },
};
use serde_json::json;

// ── MockProvider ──────────────────────────────────────────────────────────────

/// A scripted provider that plays through a queue of CompletionResponse values.
struct MockProvider {
    model_name: String,
    responses: std::sync::Mutex<std::collections::VecDeque<CompletionResponse>>,
    call_count: Arc<AtomicUsize>,
}

impl MockProvider {
    fn new(model_name: &str, responses: Vec<CompletionResponse>) -> Self {
        Self {
            model_name: model_name.to_string(),
            responses: std::sync::Mutex::new(responses.into()),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ToolUseProvider for MockProvider {
    fn model(&self) -> &str {
        &self.model_name
    }

    async fn complete(
        &self,
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> anyhow::Result<CompletionResponse> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.responses.lock().unwrap();
        queue
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("MockProvider: no more scripted responses"))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_usage(input: i64, output: i64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
    }
}

fn text_response(text: &str) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some("end_turn".to_string()),
        model: "claude-opus-4-8".to_string(),
        usage: make_usage(10, 20),
    }
}

fn tool_use_response(
    tool_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::ToolUse {
            id: tool_id.to_string(),
            name: tool_name.to_string(),
            input,
        }],
        stop_reason: Some("tool_use".to_string()),
        model: "claude-opus-4-8".to_string(),
        usage: make_usage(50, 30),
    }
}

fn refusal_response() -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text {
            text: "I cannot answer that.".to_string(),
        }],
        stop_reason: Some("refusal".to_string()),
        model: "claude-opus-4-8".to_string(),
        usage: make_usage(5, 5),
    }
}

/// Seed the in-memory database with a property and a few transactions.
async fn seed_db(pool: &sqlx::SqlitePool) -> String {
    db::init_database(pool).await.unwrap();
    let prop_id = db::upsert_property(
        pool,
        "Test Apartments",
        "Dallas",
        100,
        "Example Fund I",
        "PM Co",
    )
    .await
    .unwrap();

    // Vendor transactions
    for (payee, amount, is_resident) in [
        ("7 Kings Landscaping", -1500.0, 0i64),
        ("7 Kings Landscaping", -2000.0, 0),
        ("Apex Roofing", -5000.0, 0),
        ("John Smith", -900.0, 1), // resident — must be excluded from vendor_spend
    ] {
        sqlx::query(
            "INSERT INTO gl_transactions \
             (id, property_id, entity_code, account_code, txn_date, period, \
              payee, is_resident, amount, source_file, source_row, created_at) \
             VALUES (?, ?, 'E1', '5100', '2026-01-15', '2026-01', ?, ?, ?, 'seed.csv', 1, '2026-01-01')",
        )
        .bind(db::new_id())
        .bind(&prop_id)
        .bind(payee)
        .bind(is_resident)
        .bind(amount)
        .execute(pool)
        .await
        .unwrap();
    }

    prop_id
}

// ── search_transactions: period + amount ordering ────────────────────────────

/// Seed one property with rows across two periods, including a clear positive
/// max and a large NEGATIVE row, to exercise ABS() ordering and period filter.
async fn seed_txn_search(pool: &sqlx::SqlitePool) -> String {
    db::init_database(pool).await.unwrap();
    let prop_id = db::upsert_property(
        pool,
        "juniper_fund",
        "Dallas",
        100,
        "Example Fund I",
        "PM Co",
    )
    .await
    .unwrap();

    // (period, txn_date, payee, amount)
    let rows: [(&str, &str, &str, f64); 5] = [
        ("2026-05", "2026-05-02", "Apex Roofing", 4200.0), // positive max-abs in 2026-05
        ("2026-05", "2026-05-10", "7 Kings Landscaping", -9000.0), // largest by ABS overall
        ("2026-05", "2026-05-15", "City Utilities", 300.0),
        ("2026-04", "2026-04-04", "Apex Roofing", 12000.0), // bigger, but wrong period
        ("2026-04", "2026-04-20", "Mega Vendor", -50.0),
    ];
    for (period, txn_date, payee, amount) in rows {
        sqlx::query(
            "INSERT INTO gl_transactions \
             (id, property_id, entity_code, account_code, txn_date, period, \
              payee, is_resident, amount, source_file, source_row, created_at) \
             VALUES (?, ?, 'E1', '5100', ?, ?, ?, 0, ?, 'seed.csv', 1, '2026-01-01')",
        )
        .bind(db::new_id())
        .bind(&prop_id)
        .bind(txn_date)
        .bind(period)
        .bind(payee)
        .bind(amount)
        .execute(pool)
        .await
        .unwrap();
    }
    prop_id
}

#[tokio::test]
async fn search_transactions_orders_by_abs_amount() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_txn_search(&pool).await;

    // Largest by ABS amount within 2026-05 is the -9000 row (sign-agnostic),
    // not the +4200 positive max.
    let rows = db::search_transactions(&pool, Some(&prop_id), "", Some("2026-05"), true, 50)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3, "all three 2026-05 rows returned");
    assert_eq!(rows[0].payee, "7 Kings Landscaping");
    assert!(
        (rows[0].amount - (-9000.0)).abs() < 1e-9,
        "max-abs row is first"
    );
    // Second is the +4200 row, then the +300 row — full ABS DESC order.
    assert!((rows[1].amount - 4200.0).abs() < 1e-9);
    assert!((rows[2].amount - 300.0).abs() < 1e-9);
}

#[tokio::test]
async fn search_transactions_filters_by_period() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_txn_search(&pool).await;

    // Only 2026-04 rows come back when scoped to that period.
    let rows = db::search_transactions(&pool, Some(&prop_id), "", Some("2026-04"), false, 50)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "exactly the two 2026-04 rows");
    for r in &rows {
        assert_eq!(r.period, "2026-04");
    }

    // Non-padded "2026-5" normalizes to "2026-05" and matches the same rows.
    let normalized = db::search_transactions(&pool, Some(&prop_id), "", Some("2026-5"), false, 50)
        .await
        .unwrap();
    assert_eq!(
        normalized.len(),
        3,
        "2026-5 normalizes to 2026-05 (three rows)"
    );
}

#[tokio::test]
async fn search_transactions_empty_query_matches_all_for_period() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_txn_search(&pool).await;

    // Empty needle must NOT search for "" — it drops the text clause entirely
    // and returns every row for the property/period (not zero).
    let rows = db::search_transactions(&pool, Some(&prop_id), "", Some("2026-05"), false, 50)
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "empty query returns all 2026-05 rows, not zero"
    );
}

// ── payee_transactions ────────────────────────────────────────────────────────

#[tokio::test]
async fn payee_transactions_returns_newest_first_excludes_residents() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;

    // Confirm 7 Kings rows come back newest-first.
    let txns = db::payee_transactions(&pool, "7 Kings Landscaping", None, 10)
        .await
        .unwrap();

    assert_eq!(txns.len(), 2, "both 7 Kings rows should be returned");
    // All must be non-residents.
    for txn in &txns {
        assert_eq!(txn.is_resident, 0, "residents must be excluded");
    }

    // Resident payee must yield nothing.
    let resident_txns = db::payee_transactions(&pool, "John Smith", None, 10)
        .await
        .unwrap();
    assert!(resident_txns.is_empty(), "resident payee must be excluded");

    // Property filter: property_id matches only prop_id rows.
    let filtered = db::payee_transactions(&pool, "7 Kings Landscaping", Some(&prop_id), 10)
        .await
        .unwrap();
    assert_eq!(
        filtered.len(),
        2,
        "property filter should return matching rows"
    );

    // Property filter with wrong id yields nothing.
    let none_rows = db::payee_transactions(&pool, "7 Kings Landscaping", Some("no-such-id"), 10)
        .await
        .unwrap();
    assert!(none_rows.is_empty(), "wrong property_id must return empty");
}

// ── vendor_spend per-property breakdown ──────────────────────────────────────

#[tokio::test]
async fn vendor_spend_breaks_out_spend_per_property() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_a = seed_db(&pool).await;
    // A second property with the same vendor.
    let prop_b = db::upsert_property(
        &pool,
        "Second Apartments",
        "Austin",
        50,
        "Example Fund I",
        "PM Co",
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_transactions \
         (id, property_id, entity_code, account_code, txn_date, period, \
          payee, is_resident, amount, source_file, source_row, created_at) \
         VALUES (?, ?, 'E2', '5100', '2026-02-10', '2026-02', '7 Kings Landscaping', 0, -750.0, 'seed.csv', 1, '2026-01-01')",
    )
    .bind(db::new_id())
    .bind(&prop_b)
    .execute(&pool)
    .await
    .unwrap();
    let _ = prop_a;

    let vendors = db::vendor_spend(&pool, None, None, Some("7 Kings"), 50)
        .await
        .unwrap();

    assert_eq!(vendors.len(), 1);
    let vendor = &vendors[0];
    assert_eq!(vendor.txn_count, 3);
    assert!((vendor.total - (-4250.0)).abs() < 0.01);
    assert_eq!(vendor.first_period, "2026-01");
    assert_eq!(vendor.last_period, "2026-02");
    // Two property shares that sum to the vendor total, biggest first.
    assert_eq!(vendor.by_property.len(), 2);
    let share_sum: f64 = vendor.by_property.iter().map(|s| s.total).sum();
    assert!((share_sum - vendor.total).abs() < 0.01);
    assert_eq!(vendor.by_property[0].property, "Test Apartments");
    assert!((vendor.by_property[0].total - (-3500.0)).abs() < 0.01);
    assert_eq!(vendor.by_property[1].property, "Second Apartments");
}

// ── B6 test 1: default mode executes vendor_spend tool, logs one tool_run, makes exactly ONE provider call ──

#[tokio::test]
async fn default_mode_executes_vendor_spend_tool() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "vendor_spend",
            json!({"payee_contains": "Kings"}),
        )],
    );

    let result = run_ask(
        &pool,
        &provider,
        "what did we pay 7 Kings Landscaping?",
        false,
    )
    .await
    .unwrap();

    // Exactly one API call in default mode
    assert_eq!(
        provider.calls(),
        1,
        "default mode must make exactly 1 provider call"
    );
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    // Confirm a tool_run row was logged
    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_count, 1, "one tool_run row should be logged");

    // The tool_run should reference vendor_spend
    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "vendor_spend");
}

// ── B6 test 2: narrate mode feeds redacted results back, stops on end_turn ───

#[tokio::test]
async fn narrate_mode_redacts_resident_payees() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await;

    // Round 1: model asks for vendor_spend (no filter → includes resident row in raw db)
    let round1 = tool_use_response("toolu_002", "vendor_spend", json!({}));
    // Round 2: model wraps up
    let round2 = text_response("Here is the vendor summary.");

    let provider = MockProvider::new("claude-opus-4-8", vec![round1, round2]);

    // We need to capture the messages sent in round 2 to verify redaction.
    // We do that by checking that the tool_run output_json does NOT contain "John Smith".
    let result = run_ask(&pool, &provider, "list all vendor spend", true)
        .await
        .unwrap();

    assert!(matches!(result.status, AskStatus::Ok));
    assert_eq!(
        provider.calls(),
        2,
        "narrate mode should make 2 calls (tool_use + end_turn)"
    );

    // The tool_run for vendor_spend should not expose John Smith because
    // residents are excluded by db::vendor_spend (is_resident = 0 filter).
    let output_json: Option<String> =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'vendor_spend'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    let output = output_json.unwrap_or_default();
    assert!(
        !output.contains("John Smith"),
        "resident payee must not appear in vendor_spend output"
    );
}

// ── B6 test 3: refusal handling ───────────────────────────────────────────────

#[tokio::test]
async fn refusal_sets_failed_status() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let provider = MockProvider::new("claude-opus-4-8", vec![refusal_response()]);

    let result = run_ask(&pool, &provider, "do something bad", false)
        .await
        .unwrap();

    assert!(
        matches!(result.status, AskStatus::Refusal),
        "refusal stop_reason should map to AskStatus::Refusal"
    );
    assert_eq!(result.tool_calls, 0);

    // task_run should be marked failed
    let status: String =
        sqlx::query_scalar("SELECT status FROM task_runs ORDER BY started_at DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "failed");
}

// ── B6 test 4: unknown tool name from model → graceful error, not a panic ────

#[tokio::test]
async fn unknown_tool_name_does_not_panic() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_999",
            "nonexistent_tool",
            json!({}),
        )],
    );

    // Should not panic; the engine logs the error and continues.
    let result = run_ask(&pool, &provider, "do something weird", false)
        .await
        .unwrap();

    assert!(matches!(result.status, AskStatus::Ok));
    // No tool_run logged for a successful execution, but the unknown tool
    // should be logged with an error
    let err_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_runs WHERE success = 0")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(err_count, 1, "failed tool call should be logged");
}

// ── B6 unit test: redact_residents ───────────────────────────────────────────

#[test]
fn redact_residents_replaces_resident_payees() {
    let mut val = json!([
        {"payee": "John Smith", "is_resident": 1, "amount": -900.0},
        {"payee": "7 Kings Landscaping", "is_resident": 0, "amount": -1500.0},
    ]);

    redact_residents(&mut val);

    let arr = val.as_array().unwrap();
    assert_eq!(
        arr[0]["payee"].as_str().unwrap(),
        "(resident)",
        "resident payee should be redacted"
    );
    assert_eq!(
        arr[1]["payee"].as_str().unwrap(),
        "7 Kings Landscaping",
        "vendor payee must not be redacted"
    );
}

#[test]
fn redact_residents_handles_nested_objects() {
    let mut val = json!({
        "results": [
            {"payee": "Alice Tenant", "is_resident": true},
            {"payee": "Bob Vendor", "is_resident": false},
        ]
    });
    redact_residents(&mut val);
    let results = val["results"].as_array().unwrap();
    assert_eq!(results[0]["payee"].as_str().unwrap(), "(resident)");
    assert_eq!(results[1]["payee"].as_str().unwrap(), "Bob Vendor");
}

// ── B6 unit test: every ToolSpec has additionalProperties: false ──────────────

#[test]
fn all_tool_specs_have_additional_properties_false() {
    for spec in tool_specs() {
        let schema = &spec.input_schema;
        let ap = schema
            .get("additionalProperties")
            .unwrap_or_else(|| panic!("tool '{}' schema missing additionalProperties", spec.name));
        assert_eq!(
            ap,
            &serde_json::Value::Bool(false),
            "tool '{}' additionalProperties must be false",
            spec.name
        );
    }
}

#[test]
fn all_tool_specs_are_valid_json_schemas() {
    for spec in tool_specs() {
        // Must have "type": "object"
        assert_eq!(
            spec.input_schema["type"].as_str().unwrap_or(""),
            "object",
            "tool '{}' schema type must be 'object'",
            spec.name
        );
        // Must be serializable
        serde_json::to_string(&spec).expect("ToolSpec must serialize to JSON");
    }
}

// ── delinquency_summary tool ────────────────────────────────────────────────

/// Insert one delinquency snapshot row for a property.
async fn insert_delinquency_snapshot(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    as_of: &str,
    amount: f64,
    units: i64,
    prepaid: f64,
) {
    sqlx::query(
        "INSERT INTO delinquency_snapshots \
         (id, property_id, as_of_date, delinquent_amount, delinquent_units, \
          prepaid_amount, source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of)
    .bind(amount)
    .bind(units)
    .bind(prepaid)
    .execute(pool)
    .await
    .unwrap();
}

/// Insert one collection snapshot row (carries the high_risk_units triage signal).
async fn insert_collection_snapshot(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    as_of: &str,
    high_risk_units: i64,
) {
    sqlx::query(
        "INSERT INTO collection_snapshots \
         (id, property_id, as_of_date, total_delinquent, delinquent_units, high_risk_units, \
          total_opportunity, pricing_opportunity, missed_fee_total, avg_on_time_pct, \
          source_file, source_row, created_at) \
         VALUES (?, ?, ?, 0, 0, ?, 0, 0, 0, 0, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of)
    .bind(high_risk_units)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn delinquency_summary_returns_latest_snapshot() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Two snapshots; the later as_of_date must win.
    insert_delinquency_snapshot(&pool, &prop_id, "2026-06-09", 11_000.0, 7, 500.0).await;
    insert_delinquency_snapshot(&pool, &prop_id, "2026-06-15", 12_345.0, 9, 600.0).await;
    insert_collection_snapshot(&pool, &prop_id, "2026-06-09", 4).await;

    let got = db::delinquency_summary(&pool, &prop_id).await.unwrap();
    let s = got.expect("snapshot should be present");
    assert_eq!(s.as_of_date, "2026-06-15", "latest as_of_date must win");
    assert_eq!(
        s.high_risk_units,
        Some(4),
        "high_risk_units from latest collection snapshot"
    );
    assert_eq!(
        s.high_risk_as_of.as_deref(),
        Some("2026-06-09"),
        "high_risk_as_of carries the collection snapshot's own date (differs from delinquency date)"
    );
    assert!((s.delinquent_amount - 12_345.0).abs() < 0.005);
    assert_eq!(s.delinquent_units, 9);
    assert!((s.prepaid_amount - 600.0).abs() < 0.005);
}

#[tokio::test]
async fn delinquency_summary_missing_feed_returns_none() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await; // gl seeded, but NO delinquency snapshot
    let got = db::delinquency_summary(&pool, &prop_id).await.unwrap();
    assert!(
        got.is_none(),
        "no snapshot => None (caller must not assume $0)"
    );
}

#[tokio::test]
async fn ask_delinquency_tool_logs_and_succeeds() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_delinquency_snapshot(&pool, &prop_id, "2026-06-15", 12_345.0, 9, 600.0).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "delinquency_summary",
            json!({"property": "Test Apartments"}),
        )],
    );

    let result = run_ask(&pool, &provider, "what's our delinquency?", false)
        .await
        .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "delinquency_summary");

    // Output carries the as-of date and the figure (grounding + freshness).
    let output: String = sqlx::query_scalar(
        "SELECT output_json FROM tool_runs WHERE tool_name = 'delinquency_summary'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(output.contains("2026-06-15"), "must surface the as-of date");
    assert!(
        output.contains("12345"),
        "must surface the delinquent amount"
    );
}

#[tokio::test]
async fn ask_delinquency_missing_feed_does_not_report_zero() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await; // property exists, but NO delinquency snapshot

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "delinquency_summary",
            json!({"property": "Test Apartments"}),
        )],
    );

    let result = run_ask(&pool, &provider, "what's our delinquency?", false)
        .await
        .unwrap();
    assert!(matches!(result.status, AskStatus::Ok));

    let output: String = sqlx::query_scalar(
        "SELECT output_json FROM tool_runs WHERE tool_name = 'delinquency_summary'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // C3 guard: explicit "no feed", and the no-row branch emits NO amount field at
    // all (not a zero). (The previous `!contains("delinquent_amount":0)` was vacuous —
    // the None-branch JSON has no delinquent_amount key, so it could never match.)
    assert!(
        output.contains("No receivables feed"),
        "missing feed must be stated explicitly, got: {output}"
    );
    assert!(
        !output.contains("delinquent_amount"),
        "no-feed branch must not emit any delinquent_amount figure, got: {output}"
    );
}

#[tokio::test]
async fn ask_delinquency_present_zero_is_flagged_unverified() {
    // C3 hole #2: a PRESENT $0/0-units row (e.g. a count-only feed) must NOT be
    // asserted as a confirmed zero — it is flagged unverified.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_delinquency_snapshot(&pool, &prop_id, "2026-06-15", 0.0, 0, 0.0).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "delinquency_summary",
            json!({"property": "Test Apartments"}),
        )],
    );
    run_ask(&pool, &provider, "what's our delinquency?", false)
        .await
        .unwrap();

    let output: String = sqlx::query_scalar(
        "SELECT output_json FROM tool_runs WHERE tool_name = 'delinquency_summary'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        output.contains("\"zero_is_unverified\":true"),
        "present $0 must be flagged unverified, got: {output}"
    );
}

#[tokio::test]
async fn ask_delinquency_discloses_divergent_high_risk_asof() {
    // Grounding/freshness: when high_risk_units comes from a different as-of than the
    // delinquency figures, BOTH dates must reach the reader (not be silently merged).
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_delinquency_snapshot(&pool, &prop_id, "2026-06-15", 12_345.0, 9, 600.0).await;
    insert_collection_snapshot(&pool, &prop_id, "2026-06-09", 4).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "delinquency_summary",
            json!({"property": "Test Apartments"}),
        )],
    );
    run_ask(&pool, &provider, "what's our delinquency?", false)
        .await
        .unwrap();

    let output: String = sqlx::query_scalar(
        "SELECT output_json FROM tool_runs WHERE tool_name = 'delinquency_summary'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        output.contains("2026-06-15") && output.contains("2026-06-09"),
        "both the delinquency date and the divergent high-risk as-of must be disclosed, got: {output}"
    );
}

// ── occupancy tool ──────────────────────────────────────────────────────────

/// Insert one rent-roll snapshot row for a property.
#[allow(clippy::too_many_arguments)]
async fn insert_rent_roll_snapshot(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    as_of: &str,
    occupied: i64,
    vacant: i64,
    leased: i64,
    notice: i64,
    down: i64,
) {
    sqlx::query(
        "INSERT INTO rent_roll_snapshots \
         (id, property_id, as_of_date, occupied_units, vacant_units, leased_units, \
          notice_units, down_units, market_rent_total, in_place_rent_total, \
          source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(as_of)
    .bind(occupied)
    .bind(vacant)
    .bind(leased)
    .bind(notice)
    .bind(down)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn occupancy_summary_returns_latest_snapshot() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Two snapshots; the later as_of_date must win.
    insert_rent_roll_snapshot(&pool, &prop_id, "2026-06-09", 90, 10, 5, 3, 1).await;
    insert_rent_roll_snapshot(&pool, &prop_id, "2026-06-15", 95, 5, 4, 2, 3).await;

    let got = db::occupancy_summary(&pool, &prop_id).await.unwrap();
    let s = got.expect("snapshot should be present");
    assert_eq!(s.as_of_date, "2026-06-15", "latest as_of_date must win");
    assert_eq!(s.occupied_units, 95);
    assert_eq!(s.vacant_units, 5);
    assert_eq!(s.leased_units, 4);
    assert_eq!(s.notice_units, 2);
    assert_eq!(s.down_units, 3);
}

#[tokio::test]
async fn occupancy_summary_missing_feed_returns_none() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await; // gl seeded, but NO rent-roll snapshot
    let got = db::occupancy_summary(&pool, &prop_id).await.unwrap();
    assert!(
        got.is_none(),
        "no snapshot => None (caller must not fabricate occupancy)"
    );
}

#[tokio::test]
async fn ask_occupancy_tool_logs_and_succeeds() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // down>0 so the denominator is actually exercised: canonical
    // occupancy = 153 / (153 + 15 + 4) = 153/172 = 88.95% → 89.0% (matches
    // ontology::occupancy_rate). A wrong denominator (153/168) would yield 91.1%.
    insert_rent_roll_snapshot(&pool, &prop_id, "2026-06-15", 153, 15, 4, 2, 4).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "occupancy",
            json!({"property": "Test Apartments"}),
        )],
    );

    let result = run_ask(&pool, &provider, "what's our occupancy?", false)
        .await
        .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "occupancy");

    // Output carries the as-of date and the CANONICAL occupancy pct (grounding + freshness).
    // Assert the explicit JSON field value — not a substring that occupied_units could satisfy —
    // so a wrong denominator (which would give 91.1) is actually caught.
    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'occupancy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(output.contains("2026-06-15"), "must surface the as-of date");
    assert!(
        output.contains("\"occupancy_pct\":89.0"),
        "must report canonical occupancy 89.0 (down in denominator), got: {output}"
    );
}

#[tokio::test]
async fn ask_occupancy_missing_feed_does_not_fabricate() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await; // property exists, but NO rent-roll snapshot

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "occupancy",
            json!({"property": "Test Apartments"}),
        )],
    );

    let result = run_ask(&pool, &provider, "what's our occupancy?", false)
        .await
        .unwrap();
    assert!(matches!(result.status, AskStatus::Ok));

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'occupancy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Missing-feed guard: explicit "no feed" + the positive null-occupancy JSON shape, and NO
    // occupancy_pct field. (Positive-shape asserts replace the prior brittle !contains("100"),
    // which a property named e.g. "Building 100" would have false-failed.)
    assert!(
        output.contains("No rent-roll feed"),
        "missing feed must be stated explicitly, got: {output}"
    );
    assert!(
        output.contains("\"occupancy\":null"),
        "no-feed branch must emit the null-occupancy shape, got: {output}"
    );
    assert!(
        !output.contains("occupancy_pct"),
        "no-feed branch must not emit an occupancy_pct field, got: {output}"
    );
}

#[tokio::test]
async fn ask_occupancy_zero_occupiable_base_reports_na_not_panic() {
    // Divide-by-zero branch: a present row with NO occupiable base (occupied+vacant+down == 0)
    // must yield "n/a", never a panic or a fabricated figure. ontology::occupancy_rate => None.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_rent_roll_snapshot(&pool, &prop_id, "2026-06-15", 0, 0, 0, 0, 0).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "occupancy",
            json!({"property": "Test Apartments"}),
        )],
    );
    let result = run_ask(&pool, &provider, "what's our occupancy?", false)
        .await
        .unwrap();
    assert!(matches!(result.status, AskStatus::Ok));

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'occupancy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // JSON occupancy_pct is the string "n/a" (not a number, not null-as-zero); rendered "n/a".
    assert!(
        output.contains("\"occupancy_pct\":\"n/a\""),
        "zero occupiable base must yield occupancy_pct \"n/a\", got: {output}"
    );
    assert!(
        output.contains("n/a"),
        "rendered table must show n/a, got: {output}"
    );
    // The "n/a" occupancy_pct shape above is the positive proof; no fabricated numeric figure exists
    // because occupancy_pct is the string "n/a", not a number.
}

// ── budget_variance tool ──────────────────────────────────────────────────────

/// Insert one gl_budgets row for (property, period label, account).
async fn insert_gl_budget(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    period_label: &str,
    account_code: &str,
    account_name: &str,
    category: &str,
    amount: f64,
) {
    let period_id = db::upsert_period(pool, period_label).await.unwrap();
    sqlx::query(
        "INSERT INTO gl_budgets \
         (id, property_id, period_id, account_code, account_name, category, amount, \
          source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(&period_id)
    .bind(account_code)
    .bind(account_name)
    .bind(category)
    .bind(amount)
    .execute(pool)
    .await
    .unwrap();
}

/// Insert one gl_actuals row for (property, period label, account).
async fn insert_gl_actual(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    period_label: &str,
    account_code: &str,
    account_name: &str,
    category: &str,
    amount: f64,
) {
    let period_id = db::upsert_period(pool, period_label).await.unwrap();
    sqlx::query(
        "INSERT INTO gl_actuals \
         (id, property_id, period_id, account_code, account_name, category, amount, \
          source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(&period_id)
    .bind(account_code)
    .bind(account_name)
    .bind(category)
    .bind(amount)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn budget_variance_over_budget_expense_is_unfavorable() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Expense account over budget: budget 1000, actual 1200 → +200, +20%, UNFAVORABLE.
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        1200.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one account expected");
    let r = &rows[0];
    assert_eq!(r.account_code, "6250-0400");
    assert!((r.variance - 200.0).abs() < 0.005, "variance must be +200");
    assert_eq!(r.variance_pct, Some(20.0), "20% over budget (200/1000*100)");
    assert!(r.class_known, "Utilities is a known Expense class");
    assert!(r.is_unfavorable, "expense over budget must be unfavorable");
}

#[tokio::test]
async fn budget_variance_under_collected_income_is_unfavorable() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Income account under-collected: budget 1000, actual 800 → -200, UNFAVORABLE
    // (under plan on the revenue side). is_unfavorable IS reported here.
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0010",
        "Market Rent",
        "Rental Income",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0010",
        "Market Rent",
        "Rental Income",
        800.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert!(
        (r.variance - (-200.0)).abs() < 0.005,
        "variance must be -200"
    );
    assert!(r.class_known, "Rental Income is a known Revenue class");
    assert!(
        r.is_unfavorable,
        "income under-collected (actual < budget) must be unfavorable"
    );
}

#[tokio::test]
async fn budget_variance_under_budget_expense_is_favorable() {
    // FAVORABLE expense branch: a sign inversion here would otherwise pass
    // undetected (the unfavorable tests can't catch it). Expense under budget
    // (actual < budget) must be favorable, with the class resolved.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        800.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert!(
        (r.variance - (-200.0)).abs() < 0.005,
        "variance must be -200"
    );
    assert!(r.class_known, "Utilities is a known Expense class");
    assert!(
        !r.is_unfavorable,
        "expense under budget (actual < budget) must be favorable"
    );
}

#[tokio::test]
async fn budget_variance_over_collected_income_is_favorable() {
    // FAVORABLE revenue branch: income over-collected (actual > budget) must be
    // favorable — pins the other half of the revenue sign against inversion.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0010",
        "Market Rent",
        "Rental Income",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0010",
        "Market Rent",
        "Rental Income",
        1200.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert!((r.variance - 200.0).abs() < 0.005, "variance must be +200");
    assert!(r.class_known, "Rental Income is a known Revenue class");
    assert!(
        !r.is_unfavorable,
        "income over-collected (actual > budget) must be favorable"
    );
}

#[tokio::test]
async fn budget_variance_contra_revenue_concession_deeper_is_unfavorable() {
    // Contra-revenue: Concessions are Revenue-class but stored NEGATIVE. A deeper
    // concession than planned (actual -50 < budget -20) is under-collected on the
    // revenue line, so it must be UNFAVORABLE — this is the sign the NOI bridge
    // relies on. variance = actual - budget = -50 - (-20) = -30.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0200",
        "Concessions",
        "Concessions",
        -20.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "5012-0200",
        "Concessions",
        "Concessions",
        -50.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert!((r.variance - (-30.0)).abs() < 0.005, "variance must be -30");
    assert!(r.class_known, "Concessions is a known Revenue class");
    assert!(
        r.is_unfavorable,
        "a deeper-than-budget concession (actual < budget on a contra-revenue line) must be unfavorable"
    );
}

#[tokio::test]
async fn budget_variance_unmapped_omits_favorability() {
    // Unmapped accounts cannot be classified income vs expense, so the tool must
    // NOT assert a favorability: the JSON omits is_unfavorable (class_known false)
    // and the rendered table shows "(unclassified)" rather than a guessed flag.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "1130-0010",
        "Cash in Escrow - Tax",
        "Unmapped",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "1130-0010",
        "Cash in Escrow - Tax",
        "Unmapped",
        1200.0,
    )
    .await;

    // db layer: class is unknown, favorability not meaningful.
    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].class_known,
        "Unmapped account must report class_known == false"
    );

    // tool layer (JSON persisted to tool_runs): the JSON OMITS is_unfavorable for
    // an unclassifiable account — the model must never receive a guessed favorability.
    // (The rendered-table "(unclassified)" marker is unit-tested in ask.rs, where the
    // private render fn is reachable; tool_runs persists only output_json.)
    let output = render_budget_variance_via_tool(&pool, &prop_id, "2026-05").await;
    assert!(
        !output.contains("is_unfavorable"),
        "Unmapped account must omit is_unfavorable from the JSON, got: {output}"
    );
    // Positive shape: the row IS present (raw figures reported), just without a flag.
    assert!(
        output.contains("1130-0010") && output.contains("\"category\":\"Unmapped\""),
        "Unmapped account must still report raw figures, got: {output}"
    );
}

#[tokio::test]
async fn budget_variance_zero_budget_yields_no_pct_no_panic() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Divide-by-zero guard: budget 0, actual 500 → variance_pct None, no panic.
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "6300-0100",
        "Repairs",
        "Repairs & Maintenance",
        0.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "6300-0100",
        "Repairs",
        "Repairs & Maintenance",
        500.0,
    )
    .await;

    let rows = db::budget_variance(&pool, &prop_id, "2026-05")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert!((r.variance - 500.0).abs() < 0.005);
    assert_eq!(
        r.variance_pct, None,
        "budget==0 must yield None, not a fake 0%/∞"
    );

    // The rendered table shows n/a for the pct, never a panic or fabricated number.
    let output = render_budget_variance_via_tool(&pool, &prop_id, "2026-05").await;
    assert!(
        output.contains("n/a"),
        "zero-budget row must render n/a, got: {output}"
    );
}

/// Drive the budget_variance tool end-to-end and return the tool_run output_json.
async fn render_budget_variance_via_tool(
    pool: &sqlx::SqlitePool,
    _prop_id: &str,
    period: &str,
) -> String {
    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_bv",
            "budget_variance",
            json!({"property": "Test Apartments", "period": period}),
        )],
    );
    run_ask(pool, &provider, "budget variance?", false)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'budget_variance'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn ask_budget_variance_tool_logs_and_surfaces_period_and_variance() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_gl_budget(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        1000.0,
    )
    .await;
    insert_gl_actual(
        &pool,
        &prop_id,
        "2026-05",
        "6250-0400",
        "Electric",
        "Utilities",
        1200.0,
    )
    .await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_bv",
            "budget_variance",
            json!({"property": "Test Apartments", "period": "2026-05"}),
        )],
    );
    let result = run_ask(&pool, &provider, "are we over budget?", false)
        .await
        .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "budget_variance");

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'budget_variance'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Grounding: the period must always be present.
    assert!(output.contains("2026-05"), "must surface the period");
    // Non-vacuous: assert the literal variance figure and the favorability flag,
    // not a substring another field could satisfy.
    assert!(
        output.contains("\"variance\":200"),
        "must report +200 dollar variance, got: {output}"
    );
    assert!(
        output.contains("\"is_unfavorable\":true"),
        "expense over budget must be flagged unfavorable, got: {output}"
    );
}

#[tokio::test]
async fn ask_budget_variance_missing_feed_does_not_report_on_budget() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await; // property exists, but NO gl_budgets/gl_actuals for the period

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_bv",
            "budget_variance",
            json!({"property": "Test Apartments", "period": "2026-05"}),
        )],
    );
    let result = run_ask(&pool, &provider, "are we over budget?", false)
        .await
        .unwrap();
    assert!(matches!(result.status, AskStatus::Ok));

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'budget_variance'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Missing-feed guard: explicit "No budget feed", and the period is still named.
    assert!(
        output.contains("No budget feed"),
        "missing budget feed must be stated explicitly, got: {output}"
    );
    assert!(output.contains("2026-05"), "must still name the period");
    // Positive-shape assertion: the no-feed branch emits a null budget_variance and
    // NO per-account figures — it must never fabricate an on-budget / zero-variance row.
    assert!(
        !output.contains("\"accounts\""),
        "no-feed branch must not emit any account variance rows, got: {output}"
    );
    // The guard text explicitly INSTRUCTS not to assume on-budget; assert that
    // instruction is present (positive shape) rather than banning the phrase.
    assert!(
        output.contains("Do not assume on-budget"),
        "no-feed branch must instruct against assuming on-budget, got: {output}"
    );
    // And it must not fabricate a zero-variance figure for any account.
    assert!(
        !output.contains("\"variance\":0"),
        "no-feed branch must not fabricate a $0 variance, got: {output}"
    );
}

#[tokio::test]
async fn ask_search_transactions_unscoped_empty_query_errors() {
    // Grounding guard: empty query + no property + no period must NOT return an
    // unscoped cross-property dump — it errors asking the caller to scope.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await;
    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "search_transactions",
            json!({}),
        )],
    );
    run_ask(&pool, &provider, "show transactions", false)
        .await
        .unwrap();
    let err: Option<String> = sqlx::query_scalar(
        "SELECT error_message FROM tool_runs WHERE tool_name = 'search_transactions'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        err.unwrap_or_default().contains("needs a scope"),
        "unscoped empty search_transactions must error asking for a scope"
    );
}

// ── turnover tool ─────────────────────────────────────────────────────────────

/// Insert one turn_costs row for a property.
async fn insert_turn_cost(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    unit: &str,
    turn_date: &str,
    turn_cost_total: f64,
    vacancy_days: i64,
    total_turn_impact: f64,
) {
    sqlx::query(
        "INSERT INTO turn_costs \
         (id, property_id, unit, turn_date, turn_cost_total, vacancy_days, \
          total_turn_impact, source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(unit)
    .bind(turn_date)
    .bind(turn_cost_total)
    .bind(vacancy_days)
    .bind(total_turn_impact)
    .execute(pool)
    .await
    .unwrap();
}

/// Insert one turn_costs row with NULLABLE cost / vacancy_days (None → SQL NULL),
/// to exercise the "unparseable → NULL, excluded from average" path.
async fn insert_turn_cost_opt(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    unit: &str,
    turn_date: &str,
    turn_cost_total: Option<f64>,
    vacancy_days: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO turn_costs \
         (id, property_id, unit, turn_date, turn_cost_total, vacancy_days, \
          total_turn_impact, source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(unit)
    .bind(turn_date)
    .bind(turn_cost_total)
    .bind(vacancy_days)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn turn_summary_aggregates_known_values() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // 3 turns: costs 100, 200, 300 -> total 600, avg 200; vacancy 10/20/30 -> avg 20.
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    insert_turn_cost(&pool, &prop_id, "U2", "2026-02-10", 200.0, 20, 500.0).await;
    insert_turn_cost(&pool, &prop_id, "U3", "2026-03-10", 300.0, 30, 600.0).await;

    let s = db::turn_summary(&pool, &prop_id, None)
        .await
        .unwrap()
        .expect("turn rows exist");
    assert_eq!(s.turn_count, 3);
    assert!((s.total_turn_cost - 600.0).abs() < 0.005, "total = 600");
    assert!(
        (s.avg_cost_per_turn.unwrap() - 200.0).abs() < 0.005,
        "avg cost/turn = 200"
    );
    assert!(
        (s.avg_vacancy_days.unwrap() - 20.0).abs() < 0.005,
        "avg vacancy days = 20"
    );
    assert_eq!(s.earliest_turn_date.as_deref(), Some("2026-01-10"));
    assert_eq!(s.latest_turn_date.as_deref(), Some("2026-03-10"));
}

#[tokio::test]
async fn turn_summary_null_cost_row_counts_but_is_excluded_from_average() {
    // A turn with NULL cost (e.g. source cell "N/A") is a real turn EVENT, so it
    // counts in turn_count, but it must NOT dilute avg_cost_per_turn toward $0:
    // the average is over PRICED turns only (AVG ignores NULLs).
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    insert_turn_cost(&pool, &prop_id, "U2", "2026-02-10", 300.0, 30, 600.0).await;
    // NULL cost: counts as an event, excluded from the cost average.
    insert_turn_cost_opt(&pool, &prop_id, "U3", "2026-03-10", None, Some(50)).await;

    let s = db::turn_summary(&pool, &prop_id, None)
        .await
        .unwrap()
        .expect("turn rows exist");
    assert_eq!(
        s.turn_count, 3,
        "all 3 events count, incl. the NULL-cost turn"
    );
    assert!(
        (s.total_turn_cost - 400.0).abs() < 0.005,
        "SUM ignores NULL cost: 100 + 300 = 400"
    );
    // AVG over the 2 PRICED turns = 200, NOT 400/3 = 133.3 (the diluted figure a
    // coerced-$0 NULL would have produced).
    assert!(
        (s.avg_cost_per_turn.unwrap() - 200.0).abs() < 0.005,
        "avg over priced turns only = 200, got {:?}",
        s.avg_cost_per_turn
    );
}

#[tokio::test]
async fn turn_summary_null_vacancy_excluded_from_vacancy_average() {
    // A NULL vacancy_days must be excluded from avg_vacancy_days (AVG ignores
    // NULLs), so it does not read as a 0-day turn.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    insert_turn_cost(&pool, &prop_id, "U2", "2026-02-10", 200.0, 20, 500.0).await;
    // Priced turn, but NULL vacancy_days.
    insert_turn_cost_opt(&pool, &prop_id, "U3", "2026-03-10", Some(300.0), None).await;

    let s = db::turn_summary(&pool, &prop_id, None)
        .await
        .unwrap()
        .expect("turn rows exist");
    assert_eq!(s.turn_count, 3);
    // Vacancy avg over the 2 rows that HAVE a value = 15, NOT (10+20+0)/3 = 10.
    assert!(
        (s.avg_vacancy_days.unwrap() - 15.0).abs() < 0.005,
        "avg vacancy over non-NULL rows = 15, got {:?}",
        s.avg_vacancy_days
    );
    // All 3 costs are priced here, so the cost average covers all three.
    assert!(
        (s.avg_cost_per_turn.unwrap() - 200.0).abs() < 0.005,
        "avg cost = (100+200+300)/3 = 200"
    );
}

#[tokio::test]
async fn turn_summary_since_period_filters_window() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    insert_turn_cost(&pool, &prop_id, "U2", "2026-02-10", 200.0, 20, 500.0).await;
    insert_turn_cost(&pool, &prop_id, "U3", "2026-03-10", 300.0, 30, 600.0).await;

    // since 2026-02 excludes the Jan turn: 2 turns, total 500, avg 250.
    let s = db::turn_summary(&pool, &prop_id, Some("2026-02"))
        .await
        .unwrap()
        .expect("turn rows exist");
    assert_eq!(s.turn_count, 2, "since 2026-02 keeps Feb + Mar turns");
    assert!((s.total_turn_cost - 500.0).abs() < 0.005, "total = 500");
    assert!(
        (s.avg_cost_per_turn.unwrap() - 250.0).abs() < 0.005,
        "avg cost/turn = 250"
    );
}

#[tokio::test]
async fn turn_summary_missing_feed_returns_none() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await; // property exists, NO turn rows
    let got = db::turn_summary(&pool, &prop_id, None).await.unwrap();
    assert!(
        got.is_none(),
        "no turn rows => None (caller must not fabricate $0 / 0 turns)"
    );
}

#[tokio::test]
async fn turn_summary_empty_window_guards_divide_by_zero() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    // Rows exist, but all BEFORE the window -> turn_count 0, avg None (no /0).
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    let s = db::turn_summary(&pool, &prop_id, Some("2030-01"))
        .await
        .unwrap()
        .expect("rows exist => Some, even when window is empty");
    assert_eq!(s.turn_count, 0, "window excludes all turns");
    assert!((s.total_turn_cost - 0.0).abs() < 0.005);
    assert!(
        s.avg_cost_per_turn.is_none(),
        "divide-by-zero guard: avg is None when count == 0"
    );
}

#[tokio::test]
async fn ask_turnover_tool_logs_and_succeeds() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_turn_cost(&pool, &prop_id, "U1", "2026-01-10", 100.0, 10, 400.0).await;
    insert_turn_cost(&pool, &prop_id, "U2", "2026-02-10", 300.0, 30, 600.0).await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "turnover",
            json!({"property": "Test Apartments"}),
        )],
    );

    let result = run_ask(&pool, &provider, "what's our turn cost?", false)
        .await
        .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "turnover");

    // Output carries the avg cost per turn ((100+300)/2 = 200) — a literal figure.
    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'turnover'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        output.contains("\"avg_cost_per_turn\":200"),
        "must surface the avg cost per turn (200), got: {output}"
    );
    assert!(
        output.contains("\"turn_count\":2"),
        "must surface the turn count, got: {output}"
    );
}

#[tokio::test]
async fn ask_turnover_missing_feed_does_not_report_zero() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await; // property exists, but NO turn rows

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "turnover",
            json!({"property": "Test Apartments"}),
        )],
    );
    run_ask(&pool, &provider, "what's our turn cost?", false)
        .await
        .unwrap();

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'turnover'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        output.contains("No turnover data"),
        "missing feed must be stated explicitly, got: {output}"
    );
    assert!(
        !output.contains("turn_count"),
        "no-feed branch must not emit a turn_count figure, got: {output}"
    );
}

// ── unit_pnl tool ─────────────────────────────────────────────────────────────

/// Insert one unit_pnl row for a property.
#[allow(clippy::too_many_arguments)]
async fn insert_unit_pnl(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    unit: &str,
    period: &str,
    total_income: Option<f64>,
    direct_expense: Option<f64>,
    allocated_expense: Option<f64>,
    noi: Option<f64>,
) {
    sqlx::query(
        "INSERT INTO unit_pnl \
         (id, property_id, unit, period, total_income, direct_expense, allocated_expense, noi, \
          source_file, source_row, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-06-01')",
    )
    .bind(db::new_id())
    .bind(property_id)
    .bind(unit)
    .bind(period)
    .bind(total_income)
    .bind(direct_expense)
    .bind(allocated_expense)
    .bind(noi)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn unit_pnl_exact_lookup_returns_correct_noi() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2024",
        Some(19891.68),
        Some(0.0),
        Some(15616.64),
        Some(4275.04),
    )
    .await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20321.91),
        Some(0.0),
        Some(14739.81),
        Some(5582.10),
    )
    .await;

    // No period given → latest period (2025) wins.
    let latest = db::unit_pnl(&pool, &prop_id, "C00101", None)
        .await
        .unwrap()
        .expect("unit exists");
    assert_eq!(latest.period.as_deref(), Some("2025"));
    assert!(
        (latest.noi.unwrap() - 5582.10).abs() < 0.005,
        "latest NOI = 5582.10"
    );

    // Pinned period returns that exact year's NOI.
    let pinned = db::unit_pnl(&pool, &prop_id, "C00101", Some("2024"))
        .await
        .unwrap()
        .expect("unit exists for 2024");
    assert_eq!(pinned.period.as_deref(), Some("2024"));
    assert!(
        (pinned.noi.unwrap() - 4275.04).abs() < 0.005,
        "2024 NOI = 4275.04"
    );
}

#[tokio::test]
async fn unit_pnl_unit_code_normalization_resolves_padding_and_case() {
    // Stored "C00123"; "C123" and "c00123" must resolve to the SAME row.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00123",
        "2025",
        Some(10000.0),
        Some(0.0),
        Some(6000.0),
        Some(4000.0),
    )
    .await;

    for query_unit in ["C123", "C0123", "C00123", "c00123"] {
        let r = db::unit_pnl(&pool, &prop_id, query_unit, None)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("'{query_unit}' must resolve to stored C00123"));
        assert_eq!(r.unit, "C00123", "'{query_unit}' resolves to C00123");
        assert!(
            (r.noi.unwrap() - 4000.0).abs() < 0.005,
            "NOI = 4000 for '{query_unit}'"
        );
    }
}

#[tokio::test]
async fn unit_pnl_distinct_units_do_not_over_merge() {
    // Converse of the padding test: two genuinely DISTINCT stored units
    // (C00101 vs C00201) must NOT resolve to each other. A future over-merge in
    // normalize_unit_key would return the wrong unit's NOI — a decision-critical
    // bug — so this pins the boundary.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20000.0),
        Some(0.0),
        Some(14000.0),
        Some(6000.0),
    )
    .await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00201",
        "2025",
        Some(30000.0),
        Some(0.0),
        Some(18000.0),
        Some(12000.0),
    )
    .await;

    let r = db::unit_pnl(&pool, &prop_id, "C00101", None)
        .await
        .unwrap()
        .expect("C00101 exists");
    assert_eq!(r.unit, "C00101", "C00101 must NOT resolve to C00201");
    assert!(
        (r.noi.unwrap() - 6000.0).abs() < 0.005,
        "must return C00101's NOI (6000), not C00201's (12000)"
    );

    let r2 = db::unit_pnl(&pool, &prop_id, "C00201", None)
        .await
        .unwrap()
        .expect("C00201 exists");
    assert_eq!(r2.unit, "C00201", "C00201 must NOT resolve to C00101");
    assert!(
        (r2.noi.unwrap() - 12000.0).abs() < 0.005,
        "C00201 NOI = 12000"
    );
}

#[tokio::test]
async fn unit_pnl_latest_selection_skips_null_noi_snapshot() {
    // The latest period (2025) has a NULL NOI; an earlier period (2024) is priced.
    // unit_pnl(period=None) must return the latest PRICED period (2024), NOT the
    // NULL-NOI 2025 snapshot — this aligns the tool with the eval gold rule
    // (latest period WHERE noi IS NOT NULL) so they never disagree.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2024",
        Some(19000.0),
        Some(0.0),
        Some(15000.0),
        Some(4000.0),
    )
    .await;
    // Newest period, but NOI is NULL (e.g. an unparseable source cell).
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20000.0),
        None,
        None,
        None,
    )
    .await;

    let r = db::unit_pnl(&pool, &prop_id, "C00101", None)
        .await
        .unwrap()
        .expect("unit exists");
    assert_eq!(
        r.period.as_deref(),
        Some("2024"),
        "latest priced period (2024) wins over NULL-NOI 2025"
    );
    assert!(
        (r.noi.unwrap() - 4000.0).abs() < 0.005,
        "returns the 2024 real NOI (4000), not the 2025 NULL"
    );
}

#[tokio::test]
async fn unit_pnl_missing_unit_returns_none() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20000.0),
        Some(0.0),
        Some(14000.0),
        Some(6000.0),
    )
    .await;

    // A unit that does not exist must be None — caller must NOT fabricate $0 NOI.
    let got = db::unit_pnl(&pool, &prop_id, "C99999", None).await.unwrap();
    assert!(got.is_none(), "missing unit => None (no fabricated $0 NOI)");
}

#[tokio::test]
async fn unit_pnl_null_noi_is_preserved_not_zero() {
    // A NULL NOI (unparseable source cell) must surface as None, never as 0.0.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20000.0),
        None,
        None,
        None,
    )
    .await;

    let r = db::unit_pnl(&pool, &prop_id, "C00101", None)
        .await
        .unwrap()
        .expect("unit exists");
    assert_eq!(r.noi, None, "NULL NOI must stay None, not 0.0");
    assert_eq!(r.direct_expense, None);
}

#[tokio::test]
async fn ask_unit_pnl_tool_logs_and_succeeds() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00123",
        "2025",
        Some(10000.0),
        Some(0.0),
        Some(6000.0),
        Some(4000.0),
    )
    .await;

    // Query "C123" to also exercise normalization through the live tool path.
    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "unit_pnl",
            json!({"property": "Test Apartments", "unit": "C123"}),
        )],
    );

    let result = run_ask(
        &pool,
        &provider,
        "what was annual NOI for unit C123?",
        false,
    )
    .await
    .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "unit_pnl");

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'unit_pnl'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Literal NOI figure present, and the normalized unit resolved.
    assert!(
        output.contains("\"noi\":4000"),
        "must surface the unit NOI (4000), got: {output}"
    );
    assert!(
        output.contains("\"unit\":\"C00123\""),
        "must resolve normalized unit to stored C00123, got: {output}"
    );
    // No-resident-data-leak: unit-level financials only — no resident field.
    for leaked in ["resident", "tenant", "balance", "lease_id", "tenant_code"] {
        assert!(
            !output.to_lowercase().contains(leaked),
            "unit_pnl output must not expose resident data ('{leaked}'), got: {output}"
        );
    }
}

#[tokio::test]
async fn ask_unit_pnl_missing_unit_does_not_report_zero() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    insert_unit_pnl(
        &pool,
        &prop_id,
        "C00101",
        "2025",
        Some(20000.0),
        Some(0.0),
        Some(14000.0),
        Some(6000.0),
    )
    .await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "unit_pnl",
            json!({"property": "Test Apartments", "unit": "C99999"}),
        )],
    );
    run_ask(
        &pool,
        &provider,
        "what was annual NOI for unit C99999?",
        false,
    )
    .await
    .unwrap();

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'unit_pnl'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        output.contains("not found"),
        "missing unit must be stated explicitly, got: {output}"
    );
    assert!(
        !output.contains("\"noi\":0"),
        "missing-unit branch must not fabricate a $0 NOI, got: {output}"
    );
}

// ── recall tool (Flywheel B — calibrated track record) ────────────────────────

/// Insert one SCORED value-class call for a property. `origin_period` drives the age/decay;
/// `operator_outcome` is the human free-text that MUST NEVER reach the recall surface.
#[allow(clippy::too_many_arguments)]
async fn insert_scored_call(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    id: &str,
    origin_period: &str,
    call_type: &str,
    score: f64,
    outcome_summary: &str,
    operator_outcome: &str,
) {
    sqlx::query(
        "INSERT INTO calls \
         (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, \
          score, value_class, decision_kind, entities_json, outcome_summary, operator_outcome, \
          scored_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'scored', 't', ?, '{}', ?, 'value', NULL, '[]', ?, ?, 't', 't', 't')",
    )
    .bind(id)
    .bind(property_id)
    .bind(origin_period)
    .bind(call_type)
    .bind(origin_period) // mature_by — not material to recall
    .bind(score)
    .bind(outcome_summary)
    .bind(operator_outcome)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn ask_recall_tool_reports_calibrated_track_record() {
    // Seed enough SCORED calls (at the current period → age 0, n_eff == count) so the
    // stat does NOT abstain (n_eff >= ABSTAIN_N_EFF = 5). The recall tool derives
    // today_period from db::now_iso()[..7], so we seed origin_period to the SAME month.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    let period = db::now_iso()[..7].to_string(); // YYYY-MM, matches the tool's today_period
    for i in 0..6 {
        insert_scored_call(
            &pool,
            &prop_id,
            &format!("rc{i}"),
            &period,
            "t12_reversion",
            1.0,
            "reverted toward T12 as predicted",
            "SECRET_OPERATOR_FREETEXT", // must never surface
        )
        .await;
    }

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "recall",
            json!({"property": "Test Apartments", "call_type": "t12_reversion"}),
        )],
    );

    let result = run_ask(
        &pool,
        &provider,
        "how reliable have our t12 reversion calls been?",
        false,
    )
    .await
    .unwrap();
    assert_eq!(result.tool_calls, 1);
    assert!(matches!(result.status, AskStatus::Ok));

    let tool_name: String = sqlx::query_scalar("SELECT tool_name FROM tool_runs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tool_name, "recall", "the recall tool_run must be logged");

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'recall'")
            .fetch_one(&pool)
            .await
            .unwrap();
    // Non-vacuous: with n_eff == 6 the stat does NOT abstain → the calibrated posterior
    // and the effective sample size are both present.
    assert!(
        output.contains("\"abstain\":false"),
        "n_eff=6 must NOT abstain, got: {output}"
    );
    assert!(
        output.contains("posterior_mean"),
        "non-abstaining stat must surface the calibrated posterior_mean, got: {output}"
    );
    assert!(
        output.contains("\"n_eff\":6"),
        "must surface the effective sample size (6), got: {output}"
    );
    // A neighbor (the visible denominator) must be present with its outcome + drill id.
    assert!(
        output.contains("reverted toward T12 as predicted"),
        "a neighbor outcome summary must be present, got: {output}"
    );
    assert!(
        output.contains("\"id\":\"rc0\"") || output.contains("\"id\":\"rc5\""),
        "a neighbor's drill-to-source id must be present, got: {output}"
    );
    // Privacy: human operator free-text must NEVER leak into the recall surface.
    assert!(
        !output.contains("SECRET_OPERATOR_FREETEXT") && !output.contains("operator_outcome"),
        "operator_outcome free-text must not leak into recall output, got: {output}"
    );
}

#[tokio::test]
async fn ask_recall_tool_abstains_on_thin_history() {
    // A single scored call → n_eff = 1 < 5 → the stat ABSTAINS: the JSON must carry
    // abstain:true and NO posterior_mean (a thin record must not present a confident
    // number as reliable), while the neighbor is still shown (visible denominator).
    let pool = db::connect("sqlite::memory:").await.unwrap();
    let prop_id = seed_db(&pool).await;
    let period = db::now_iso()[..7].to_string();
    insert_scored_call(
        &pool,
        &prop_id,
        "lonely1",
        &period,
        "t12_reversion",
        1.0,
        "single prior reversion call",
        "ANOTHER_SECRET_NOTE",
    )
    .await;

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "recall",
            json!({"property": "Test Apartments", "call_type": "t12_reversion"}),
        )],
    );
    run_ask(
        &pool,
        &provider,
        "how reliable have our t12 reversion calls been?",
        false,
    )
    .await
    .unwrap();

    let output: String =
        sqlx::query_scalar("SELECT output_json FROM tool_runs WHERE tool_name = 'recall'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        output.contains("\"abstain\":true"),
        "thin history (n_eff=1) must abstain, got: {output}"
    );
    // No confident number presented as reliable: the abstaining JSON omits posterior_mean.
    assert!(
        !output.contains("posterior_mean"),
        "abstaining branch must NOT present a posterior_mean as reliable, got: {output}"
    );
    assert!(
        output.contains("\"n_eff\":1"),
        "abstaining branch still surfaces the (thin) effective sample, got: {output}"
    );
    // The neighbor is still shown (K4 visible denominator), but never the operator free-text.
    assert!(
        output.contains("single prior reversion call"),
        "abstaining branch must still show neighbors, got: {output}"
    );
    assert!(
        !output.contains("ANOTHER_SECRET_NOTE") && !output.contains("operator_outcome"),
        "operator_outcome free-text must not leak even when abstaining, got: {output}"
    );
}

#[tokio::test]
async fn ask_recall_unknown_property_errors_with_valid_names() {
    // Unknown property → the not-found→list-valid grounding error (same pattern as the
    // other property-scoped tools); the tool_run is logged with that error.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    seed_db(&pool).await; // creates "Test Apartments"

    let provider = MockProvider::new(
        "claude-opus-4-8",
        vec![tool_use_response(
            "toolu_001",
            "recall",
            json!({"property": "Ghost Property"}),
        )],
    );
    run_ask(
        &pool,
        &provider,
        "how reliable are calls at Ghost Property?",
        false,
    )
    .await
    .unwrap();

    let err: Option<String> =
        sqlx::query_scalar("SELECT error_message FROM tool_runs WHERE tool_name = 'recall'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let err = err.unwrap_or_default();
    assert!(
        err.contains("Property not found") && err.contains("Test Apartments"),
        "unknown property must error with the valid-names list, got: {err}"
    );
}
