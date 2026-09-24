use boxscore::db;

/// Task 10 — end-to-end closed loop: capture → pending queue → human resolve → recall.
///
/// Adaptation note: `mature_by` is overridden to `"2026-01"` (a past period) so
/// `db::fetch_pending_outcomes` (which gates on `mature_by <= current YYYY-MM`) can
/// find the row. The capex default horizon is Periods(6) from `"2026-01"` = `"2026-07"`,
/// which would be in the future and invisible to the pending query.
#[tokio::test]
async fn decision_capture_resolve_and_recall_closes_the_loop() {
    use boxscore::{
        calls,
        capture::{self, CaptureReq},
        db,
        recall::{self, RecallCtx},
    };
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = db::upsert_property(
        &pool,
        "maplewood",
        "Atlanta",
        494,
        "Example Sponsor",
        "Example Sponsor",
    )
    .await
    .unwrap();

    // 1. Capture a free-form capex decision (human outcome mode).
    let id = capture::record(
        &pool,
        CaptureReq {
            property_id: pid.clone(),
            origin_period: "2026-01".into(),
            decision_kind: "capex".into(),
            entities_json: "[{\"type\":\"account\",\"code\":\"5120\"}]".into(),
            outcome_mode: "human".into(),
            value_class: "value".into(),
            confidence: Some(0.5),
            acted_on: true,
            accepted_recall: Some(true),
            source_surface: "ask".into(),
            context_json: None,
            mature_by: Some("2026-01".into()),
        },
    )
    .await
    .unwrap();

    // 2. It surfaces in the resolution queue once matured.
    let pending = db::fetch_pending_outcomes(&pool).await.unwrap();
    assert!(pending.iter().any(|c| c.id == id));

    // 3. Resolve it (human) — must NOT write the score column.
    calls::resolve_human(&pool, &id, "roof held, no leaks", "operator")
        .await
        .unwrap();
    let row: boxscore::models::Call = sqlx::query_as("SELECT * FROM calls WHERE id=?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.status, "scored");
    assert!(row.score.is_none());

    // 4. Recall returns the decision as a neighbor (human outcome is surfaced, not scored).
    let r = recall::recall(
        &pool,
        &RecallCtx {
            property_id: pid,
            call_type: Some("decision".into()),
            decision_kind: Some("capex".into()),
            entity_keys: vec!["account:5120".into()],
        },
        "2026-03",
        5,
    )
    .await
    .unwrap();
    assert!(r.neighbors.iter().any(|n| n.id == id));
    assert!(r.stat.abstain); // human-only, no value score -> thin -> abstain on the point estimate
}

/// Task 7 — K5 relevance/reliability selection.
///
/// In the actual codebase the track-record upsert convention is:
///   memory_type = "track_record", scope = <call_type>, key = <property_id>.
/// (calls.rs score_due_calls writes it that way; calls_ledger tests confirm
///  `m.scope == "noi_diagnosis"`.)
///
/// select_track_record_for_context queries:
///   WHERE memory_type = 'track_record'
///     AND scope IN (call_types)       -- call_type lives in scope
///     AND (property_id is empty OR key = property_id)
/// … parses N from "(N scored …)" and returns highest-N first.
#[tokio::test]
async fn track_record_selection_prefers_reliable_over_recent() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let pid = db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
        .await
        .unwrap();

    // High-N noi_diagnosis (N=40) — should rank first.
    db::upsert_memory(
        &pool,
        "track_record",
        "noi_diagnosis", // scope = call_type (actual convention)
        &pid,            // key   = property_id
        "noi_diagnosis: 70% (40 scored, calibration +0.05)",
        1.0,
        None,
    )
    .await
    .unwrap();

    // Low-N t12_reversion (N=3) — more recent but less reliable.
    db::upsert_memory(
        &pool,
        "track_record",
        "t12_reversion",
        &pid,
        "t12_reversion: 55% (3 scored, calibration -0.10)",
        1.0,
        None,
    )
    .await
    .unwrap();

    let sel =
        db::select_track_record_for_context(&pool, &pid, &["noi_diagnosis", "t12_reversion"], 5)
            .await
            .unwrap();

    assert!(
        !sel.is_empty(),
        "expected at least one track-record entry returned"
    );
    assert_eq!(
        sel.first().map(|(h, _)| h.contains("noi_diagnosis")),
        Some(true),
        "highest-N entry (noi_diagnosis, N=40) should rank first; got: {:?}",
        sel
    );
}
