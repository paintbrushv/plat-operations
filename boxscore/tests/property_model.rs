//! Flywheel C integration tests: per-property knowledge derived from the calls ledger by
//! query (read-only), exercised end-to-end through the DB read helpers so the locked ledger
//! invariants (confounded exclusion, compliance segregation, n_eff abstention) carry.

use boxscore::db;
use boxscore::property_model::{self, FactKind};
use boxscore::recall::{self, RecallCtx};

/// Insert `n` scored calls for a property+type with a fixed score and origin period.
/// `value_class`/`confounded` are parameterized so tests can prove the K-filters carry.
#[allow(clippy::too_many_arguments)]
async fn seed_scored(
    pool: &sqlx::SqlitePool,
    property_id: &str,
    call_type: &str,
    score: f64,
    origin_period: &str,
    value_class: Option<&str>,
    confounded: bool,
    n: usize,
) {
    for i in 0..n {
        let id = format!(
            "{property_id}-{call_type}-{origin_period}-{score}-{i}-{confounded}-{value_class:?}"
        );
        sqlx::query(
            "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, confidence, score, value_class, confounded, scored_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'scored', 't', '2026-06', '{}', 0.6, ?, ?, ?, 't', 't', 't')",
        )
        .bind(&id)
        .bind(property_id)
        .bind(origin_period)
        .bind(call_type)
        .bind(score)
        .bind(value_class)
        .bind(confounded)
        .execute(pool)
        .await
        .unwrap();
    }
}

fn per_property_fact(
    model: &property_model::PropertyModel,
    call_type: &str,
) -> property_model::DerivedFact {
    model
        .facts
        .iter()
        .find(|f| f.kind == FactKind::PerPropertyPosterior && f.call_type == call_type)
        .cloned()
        .expect("expected a per-property posterior fact")
}

#[tokio::test]
async fn diverging_property_flagged_inline_property_not() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let terrace = db::upsert_property(&pool, "Willow Brook", "Austin", 100, "U", "U")
        .await
        .unwrap();
    let juniper_fund = db::upsert_property(&pool, "juniper_fund", "Austin", 100, "U", "U")
        .await
        .unwrap();
    let maplewood = db::upsert_property(&pool, "Maplewood", "Atlanta", 100, "U", "U")
        .await
        .unwrap();

    // Leave-one-out baselines (H1 fix): each property is compared to the REST-OF-PORTFOLIO.
    // Maplewood: 30 reliable reverts -> rest-of-portfolio (juniper_fund+Maplewood) centers at 0.5,
    //   so TC diverges high. juniper_fund: 15/30 = 0.5 -> its rest-of-portfolio (TC+Maplewood)
    //   also centers near 0.5, so juniper_fund is in-line. Maplewood anchor keeps both pools powered.
    seed_scored(
        &pool,
        &terrace,
        "t12_reversion",
        1.0,
        "2026-05",
        None,
        false,
        30,
    )
    .await;
    seed_scored(
        &pool,
        &juniper_fund,
        "t12_reversion",
        1.0,
        "2026-05",
        None,
        false,
        15,
    )
    .await;
    seed_scored(
        &pool,
        &juniper_fund,
        "t12_reversion",
        0.0,
        "2026-05",
        None,
        false,
        15,
    )
    .await;
    seed_scored(
        &pool,
        &maplewood,
        "t12_reversion",
        1.0,
        "2026-05",
        None,
        false,
        100,
    )
    .await;
    seed_scored(
        &pool,
        &maplewood,
        "t12_reversion",
        0.0,
        "2026-05",
        None,
        false,
        100,
    )
    .await;

    let tc = property_model::build_property_model(&pool, "Willow Brook", "2026-06")
        .await
        .unwrap();
    let f_tc = per_property_fact(&tc, "t12_reversion");
    assert!(!f_tc.abstained, "TC has n_eff>=5: {f_tc:?}");
    assert!(
        f_tc.diverges,
        "TC reverts more reliably than rest-of-portfolio -> diverge: {f_tc:?}"
    );
    assert!(f_tc.note.contains("track-record divergence"));

    let ss = property_model::build_property_model(&pool, "juniper_fund", "2026-06")
        .await
        .unwrap();
    let f_ss = per_property_fact(&ss, "t12_reversion");
    assert!(!f_ss.abstained, "juniper_fund has n_eff>=5: {f_ss:?}");
    assert!(
        !f_ss.diverges,
        "juniper_fund sits on its rest-of-portfolio mean -> in-line: {f_ss:?}"
    );
}

#[tokio::test]
async fn single_property_abstains_no_external_baseline_h1() {
    // H1 regression: ONE populated property, well-powered (n_eff>=5), NO other properties.
    // The per-property fact must ABSTAIN against the absent rest-of-portfolio baseline,
    // never emit "in-line with portfolio" (which would compare the property to itself).
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let only = db::upsert_property(&pool, "Only Property", "Austin", 100, "U", "U")
        .await
        .unwrap();
    seed_scored(
        &pool,
        &only,
        "t12_reversion",
        1.0,
        "2026-06",
        None,
        false,
        20,
    )
    .await;

    let m = property_model::build_property_model(&pool, "Only Property", "2026-06")
        .await
        .unwrap();
    let f = per_property_fact(&m, "t12_reversion");
    assert!(
        f.abstained,
        "single-property state has no external baseline -> abstain: {f:?}"
    );
    assert!(!f.diverges);
    assert!(
        f.note.contains("no external portfolio baseline"),
        "must name the absent baseline, not claim in-line: {f:?}"
    );
}

#[tokio::test]
async fn thin_rest_of_portfolio_abstains_h1() {
    // H1 regression: subject powered, but the rest-of-portfolio is thin (n_eff < 5).
    // Do not diverge against a prior-pinned (0.5) baseline -> abstain.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let a = db::upsert_property(&pool, "Property A", "Austin", 100, "U", "U")
        .await
        .unwrap();
    let b = db::upsert_property(&pool, "Property B", "Austin", 100, "U", "U")
        .await
        .unwrap();
    seed_scored(&pool, &a, "t12_reversion", 1.0, "2026-06", None, false, 20).await;
    // Rest-of-portfolio for A is just B, with only 3 scored calls -> n_eff < 5.
    seed_scored(&pool, &b, "t12_reversion", 0.0, "2026-06", None, false, 3).await;

    let m = property_model::build_property_model(&pool, "Property A", "2026-06")
        .await
        .unwrap();
    let f = per_property_fact(&m, "t12_reversion");
    assert!(f.abstained, "thin external baseline -> abstain: {f:?}");
    assert!(!f.diverges);
    assert!(f.note.contains("no external portfolio baseline"));
}

#[tokio::test]
async fn thin_property_abstains_with_no_claim() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let p = db::upsert_property(&pool, "Thin Property", "Austin", 100, "U", "U")
        .await
        .unwrap();
    // 3 effective scored calls -> n_eff < 5 -> abstain.
    seed_scored(&pool, &p, "t12_reversion", 1.0, "2026-05", None, false, 3).await;

    let m = property_model::build_property_model(&pool, "Thin Property", "2026-06")
        .await
        .unwrap();
    let f = per_property_fact(&m, "t12_reversion");
    assert!(f.abstained);
    assert!(!f.diverges);
    assert!(f.note.contains("insufficient n"));
}

#[tokio::test]
async fn confounded_and_compliance_rows_do_not_move_posterior() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let p = db::upsert_property(&pool, "Filter Property", "Austin", 100, "U", "U")
        .await
        .unwrap();

    // 10 valid hits.
    seed_scored(&pool, &p, "t12_reversion", 1.0, "2026-05", None, false, 10).await;
    let before = property_model::build_property_model(&pool, "Filter Property", "2026-06")
        .await
        .unwrap();
    let pm_before = per_property_fact(&before, "t12_reversion")
        .posterior
        .unwrap();

    // 40 confounded misses (K2) + 40 explicit-compliance misses (K1). Must be excluded.
    seed_scored(&pool, &p, "t12_reversion", 0.0, "2026-05", None, true, 40).await;
    seed_scored(
        &pool,
        &p,
        "t12_reversion",
        0.0,
        "2026-05",
        Some("compliance"),
        false,
        40,
    )
    .await;

    let after = property_model::build_property_model(&pool, "Filter Property", "2026-06")
        .await
        .unwrap();
    let pm_after = per_property_fact(&after, "t12_reversion")
        .posterior
        .unwrap();

    assert!(
        (pm_before.posterior_mean - pm_after.posterior_mean).abs() < 1e-9,
        "confounded/compliance rows leaked into the posterior: {pm_before:?} vs {pm_after:?}"
    );
    // n_eff must be unchanged by the 80 excluded rows (it reflects only the 10 valid
    // rows, time-decayed: 10 * 0.5^(1/7) ≈ 9.06).
    assert!(
        (pm_after.n_eff - pm_before.n_eff).abs() < 1e-9,
        "n_eff must not grow from confounded/compliance rows: {pm_before:?} vs {pm_after:?}"
    );
    assert!(
        pm_after.n_eff < 10.0 && pm_after.n_eff > 9.0,
        "n_eff reflects only the 10 valid decayed rows: {pm_after:?}"
    );
}

#[tokio::test]
async fn regime_window_divergence_surfaced() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let p = db::upsert_property(&pool, "Regime Property", "Austin", 100, "U", "U")
        .await
        .unwrap();
    // Recent (2026-06, age 0) all hits; older (2025-01, age 17mo) all misses; both powered.
    seed_scored(&pool, &p, "t12_reversion", 1.0, "2026-06", None, false, 20).await;
    seed_scored(&pool, &p, "t12_reversion", 0.0, "2025-01", None, false, 40).await;

    let m = property_model::build_property_model(&pool, "Regime Property", "2026-06")
        .await
        .unwrap();
    let f = m
        .facts
        .iter()
        .find(|f| f.kind == FactKind::RegimeWindow && f.call_type == "t12_reversion")
        .expect("regime fact");
    assert!(!f.abstained, "both windows powered: {f:?}");
    assert!(
        f.diverges,
        "recent hits vs older misses must surface: {f:?}"
    );
}

#[tokio::test]
async fn data_gated_facts_abstain_today() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let p = db::upsert_property(&pool, "Gated Property", "Austin", 100, "U", "U")
        .await
        .unwrap();
    seed_scored(&pool, &p, "t12_reversion", 1.0, "2026-05", None, false, 20).await;

    let m = property_model::build_property_model(&pool, "Gated Property", "2026-06")
        .await
        .unwrap();
    let drift = m
        .facts
        .iter()
        .find(|f| f.kind == FactKind::ValueComplianceDrift)
        .expect("drift fact");
    assert!(drift.abstained, "no compliance rows -> abstain: {drift:?}");

    let dk = m
        .facts
        .iter()
        .find(|f| f.kind == FactKind::DecisionKind)
        .expect("decision-kind fact");
    assert!(dk.abstained, "no decision calls -> abstain: {dk:?}");
}

#[tokio::test]
async fn single_query_baseline_is_result_equivalent_to_per_property_n_plus_1() {
    // Change #1 proof: the new single-query helper + Rust partition must reproduce, BYTE for
    // BYTE, what the old per-property N+1 (one `fetch_scored_calls` per property, concatenated
    // in name order, leave-one-out) produced — both the partitioned row sets and the derived
    // posterior facts. Seed 3 properties with DIFFERENT scores/periods so the rest-of-
    // portfolio assembly order genuinely matters to the floating-point sum.
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let alpha = db::upsert_property(&pool, "Alpha", "Austin", 100, "U", "U")
        .await
        .unwrap();
    let bravo = db::upsert_property(&pool, "Bravo", "Austin", 100, "U", "U")
        .await
        .unwrap();
    let cibo = db::upsert_property(&pool, "Cibo", "Austin", 100, "U", "U")
        .await
        .unwrap();
    // Heterogeneous histories: scores and origin periods differ across properties.
    seed_scored(
        &pool,
        &alpha,
        "t12_reversion",
        1.0,
        "2026-05",
        None,
        false,
        20,
    )
    .await;
    seed_scored(
        &pool,
        &alpha,
        "t12_reversion",
        0.0,
        "2026-01",
        None,
        false,
        10,
    )
    .await;
    seed_scored(
        &pool,
        &bravo,
        "t12_reversion",
        1.0,
        "2026-04",
        None,
        false,
        25,
    )
    .await;
    seed_scored(
        &pool,
        &bravo,
        "t12_reversion",
        0.0,
        "2025-11",
        None,
        false,
        15,
    )
    .await;
    seed_scored(
        &pool,
        &cibo,
        "t12_reversion",
        0.5,
        "2026-03",
        None,
        false,
        30,
    )
    .await;

    let ct = "t12_reversion";
    let subject = &alpha;

    // OLD path: per-property fetches, rest-of-portfolio concatenated in list_properties order.
    let all_props = db::list_properties(&pool).await.unwrap();
    let old_subject = db::fetch_scored_calls(&pool, subject, ct, 100_000)
        .await
        .unwrap();
    let mut old_pooled: Vec<boxscore::models::Call> = Vec::new();
    for p in &all_props {
        if &p.id == subject {
            continue;
        }
        old_pooled.extend(
            db::fetch_scored_calls(&pool, &p.id, ct, 100_000)
                .await
                .unwrap(),
        );
    }

    // NEW path: one query, partition in Rust exactly as build_property_model does.
    use std::collections::HashMap;
    let all_ct = db::fetch_scored_calls_all_properties(&pool, ct, 100_000)
        .await
        .unwrap();
    let mut by_prop: HashMap<String, Vec<boxscore::models::Call>> = HashMap::new();
    for c in all_ct {
        by_prop.entry(c.property_id.clone()).or_default().push(c);
    }
    let new_subject = by_prop.remove(subject).unwrap_or_default();
    let mut new_pooled: Vec<boxscore::models::Call> = Vec::new();
    for p in &all_props {
        if &p.id == subject {
            continue;
        }
        if let Some(v) = by_prop.remove(&p.id) {
            new_pooled.extend(v);
        }
    }

    // Partition equality: same row id sequence (subject and pooled), in the same order.
    let ids = |v: &[boxscore::models::Call]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(&old_subject),
        ids(&new_subject),
        "subject partition diverged"
    );
    assert_eq!(
        ids(&old_pooled),
        ids(&new_pooled),
        "pooled partition diverged"
    );

    // Derived-fact equivalence: byte-identical posterior on both assemblies.
    let old_fact =
        property_model::per_property_posterior_fact(ct, &old_subject, &old_pooled, "2026-06");
    let new_fact =
        property_model::per_property_posterior_fact(ct, &new_subject, &new_pooled, "2026-06");
    let op = old_fact.posterior.as_ref().unwrap();
    let np = new_fact.posterior.as_ref().unwrap();
    let opl = old_fact.pooled.as_ref().unwrap();
    let npl = new_fact.pooled.as_ref().unwrap();
    assert_eq!(op.posterior_mean.to_bits(), np.posterior_mean.to_bits());
    assert_eq!(op.n_eff.to_bits(), np.n_eff.to_bits());
    assert_eq!(opl.posterior_mean.to_bits(), npl.posterior_mean.to_bits());
    assert_eq!(opl.n_eff.to_bits(), npl.n_eff.to_bits());
    assert_eq!(old_fact.diverges, new_fact.diverges);
    assert_eq!(old_fact.abstained, new_fact.abstained);
    assert_eq!(old_fact.note, new_fact.note);

    // And the end-to-end model (which now uses the single query) agrees with the old assembly.
    let model = property_model::build_property_model(&pool, "Alpha", "2026-06")
        .await
        .unwrap();
    let model_fact = per_property_fact(&model, ct);
    let mp = model_fact.posterior.as_ref().unwrap();
    assert_eq!(op.posterior_mean.to_bits(), mp.posterior_mean.to_bits());
    assert_eq!(op.n_eff.to_bits(), mp.n_eff.to_bits());
    assert_eq!(old_fact.note, model_fact.note);
}

#[tokio::test]
async fn recall_verb_path_returns_stat_and_neighbor_shape() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let p = db::upsert_property(&pool, "Recall Property", "Austin", 100, "U", "U")
        .await
        .unwrap();

    // One scored decision call with a matching entity.
    sqlx::query(
        "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, score, value_class, decision_kind, entities_json, outcome_summary, scored_at, created_at, updated_at) \
         VALUES ('d1', ?, '2026-04', 'decision', 'scored', 't', '2026-05', '{}', 1.0, 'value', 'renewal_override', '[{\"type\":\"unit\",\"id\":\"2BR\"}]', 'renewed at +4%', 't', 't', 't')",
    )
    .bind(&p)
    .execute(&pool)
    .await
    .unwrap();

    let ctx = RecallCtx {
        property_id: p.clone(),
        call_type: Some("decision".into()),
        decision_kind: Some("renewal_override".into()),
        entity_keys: vec!["unit:2BR".into()],
    };
    let r = recall::recall(&pool, &ctx, "2026-06", 5).await.unwrap();

    // n_eff = 1 -> abstain on the point estimate, but the neighbor is surfaced and matched.
    assert!(r.stat.abstain);
    assert_eq!(r.neighbors.len(), 1);
    assert_eq!(r.neighbors[0].id, "d1");
    assert_eq!(r.neighbors[0].summary, "renewed at +4%");
    assert!(r.neighbors[0].similarity > 0.5);
    assert!((r.neighbors[0].age_months - 2.0).abs() < 1e-9);
}
