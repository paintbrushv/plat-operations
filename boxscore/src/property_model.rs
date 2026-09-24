//! Flywheel C — a READ-ONLY query layer that derives per-property knowledge from the
//! existing `calls` ledger. NO new schema (the design spec forbids a `property_facts`
//! table; the ledger is already the seam — §2 "C will derive facts from the calls ledger
//! by query"). This module only reads scored value-class calls and reuses the calibration
//! math; it never writes, never scores, and never touches firewall semantics.
//!
//! Guardrails respected (the calls a fact may see):
//! - K1 (Goodhart): only the value feed enters a value claim — `value_class IS NOT
//!   'compliance'` (null-safe, so legacy NULL engine rows stay in; explicit compliance is
//!   excluded). Value-vs-compliance drift is surfaced as its own, separately-gated fact.
//! - K2 (treatment confounding): `confounded = 0` only.
//! - K3 (self-grading firewall): only auto-scored `score` rows count; human resolutions
//!   write `operator_outcome`, never `score`, so they never enter these posteriors.
//! - K5 (abstention): no claim when `n_eff < 5` — the property posterior abstains.
//! - K6 (horizon): only `status='scored'` calls (which by construction had a credible
//!   maturation horizon) are read; capture-and-surface kinds never reach a scored row.
//!
//! Pure functions take injected `&[Call]` slices so the derivation is unit-testable
//! without a live DB; thin async wrappers fetch via read-only `db` helpers.

use crate::calibration::{
    self, CalibratedStat, ABSTAIN_N_EFF, HALF_LIFE_MONTHS, PRIOR_MEAN, PRIOR_STRENGTH,
};
use crate::models::Call;
use crate::recall::months_between;
use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;

/// Value-class engine call types whose scored history C derives per-property knowledge from.
/// (Mirrors the calibration-eval set.)
///
/// `renewal_rec` is compliance-classed by the scorer and has no value-class scorer in use
/// today, so it never enters a value posterior. It is KEPT in this list (not dropped) so a
/// future value renewal scorer is picked up automatically AND so the per-property/regime
/// facts for it remain present (abstaining) in the output. Because the null-safe value-feed
/// predicate (`value_class IS NOT 'compliance'`) would admit a renewal_rec row whose
/// `value_class` is NULL, `is_value_scored` applies an explicit guard (change #2) that skips
/// NULL-class renewal_rec rows from the value feed, so a future emit-path regression that
/// writes a renewal_rec closeness-compliance score with `value_class = NULL` can never leak
/// into a VALUE posterior. A genuine value renewal_rec (explicit non-compliance class) is
/// still admitted automatically.
pub const VALUE_ENGINE_CALL_TYPES: &[&str] = &[
    "noi_diagnosis",
    "t12_reversion",
    "delinquency_risk",
    "renewal_rec",
];

/// Recent vs older split point for the regime (non-stationarity) view, in months from
/// `today_period`. Calls at age <= this are the "recent" window; older calls the "older"
/// window. Only surfaces the two windows + whether they diverge (no regime *inference*).
pub const REGIME_WINDOW_MONTHS: f64 = 6.0;

/// Serializable projection of a `CalibratedStat` (calibration.rs keeps the math; this is
/// just a JSON-friendly view so we never modify that module's derives).
#[derive(Debug, Clone, Serialize)]
pub struct StatView {
    pub posterior_mean: f64,
    pub n_eff: f64,
    pub lo90: f64,
    pub hi90: f64,
    pub abstain: bool,
}

impl From<CalibratedStat> for StatView {
    fn from(s: CalibratedStat) -> Self {
        StatView {
            posterior_mean: s.posterior_mean,
            n_eff: s.n_eff,
            lo90: s.lo90,
            hi90: s.hi90,
            abstain: s.abstain,
        }
    }
}

/// Kinds of derived fact. Some abstain today purely for lack of data, by design.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// Property's calibrated posterior vs the pooled/portfolio posterior for a call_type.
    /// `diverges` = the property's 90% credible interval excludes the pooled mean.
    PerPropertyPosterior,
    /// Recent-window vs older-window posterior for a (property, call_type) — surfaces
    /// possible non-stationarity. `posterior` = recent window, `pooled` = older window.
    RegimeWindow,
    /// Value-class vs compliance-class posterior (Goodhart drift). Data-gated: abstains
    /// until compliance scorers produce rows.
    ValueComplianceDrift,
    /// Per-decision-kind posterior over scored free-form `decision` calls. Data-gated:
    /// abstains until decision calls accrue auto-scored value outcomes.
    DecisionKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct DerivedFact {
    pub kind: FactKind,
    pub call_type: String,
    /// Primary posterior: per-property (PerPropertyPosterior), recent window
    /// (RegimeWindow), value class (ValueComplianceDrift), or the decision_kind group.
    pub posterior: Option<StatView>,
    /// Comparison posterior: pooled (PerPropertyPosterior), older window (RegimeWindow),
    /// or compliance class (ValueComplianceDrift). None for DecisionKind.
    pub pooled: Option<StatView>,
    pub diverges: bool,
    pub abstained: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropertyModel {
    pub property: String,
    pub property_id: String,
    pub today_period: String,
    pub facts: Vec<DerivedFact>,
}

/// Which class feed a call may enter. Replaces the old `include_compliance: bool` flag,
/// whose `true` value confusingly meant "no class filter at all" (it admitted value AND
/// compliance rows), NOT "compliance only" — change #3. The variant names now state exactly
/// which rows each feed admits, and the compliance posterior is filtered from the rows it
/// is actually given rather than trusting a pre-filtered upstream slice.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassFeed {
    /// K1 value feed: explicit-compliance rows excluded; legacy NULL engine rows stay in
    /// (null-safe). Mirrors the locked `value_class IS NOT 'compliance'` predicate.
    Value,
    /// Compliance feed: ONLY explicit `value_class == "compliance"` rows. Makes the
    /// compliance posterior provably compliance-only — it never depends on the caller
    /// having pre-filtered the slice (change #3).
    Compliance,
}

/// The locked value-feed gate (K1/K2/K3): scored, non-confounded, score present, plus the
/// class selection for `feed`.
fn is_value_scored(c: &Call, feed: ClassFeed) -> bool {
    if c.status != "scored" || c.score.is_none() || c.confounded {
        return false;
    }
    // Change #2 — renewal_rec NULL-class guard. `renewal_rec` is compliance-classed by the
    // scorer and has no value-class scorer in use, so it only ever belongs in a VALUE
    // posterior when it carries an EXPLICIT non-compliance class. A renewal_rec row with
    // `value_class = NULL` (a future emit-path regression) is its closeness-COMPLIANCE score
    // and must never enter the value feed via the null-safe predicate. Skip such rows. (On
    // the Compliance feed a NULL-class row is excluded anyway, so this is a value-feed guard;
    // a genuine value renewal_rec with an explicit non-compliance class still passes.)
    if c.call_type == "renewal_rec" && c.value_class.is_none() {
        return false;
    }
    match feed {
        ClassFeed::Value => c.value_class.as_deref() != Some("compliance"),
        ClassFeed::Compliance => c.value_class.as_deref() == Some("compliance"),
    }
}

/// Build time-decayed scored points from the value feed (K1/K2/K3 filter applied here so
/// injected confounded/compliance rows are provably excluded).
fn collect_points(calls: &[Call], today_period: &str) -> Vec<calibration::ScoredPoint> {
    collect_points_feed(calls, today_period, ClassFeed::Value)
}

fn collect_points_feed(
    calls: &[Call],
    today_period: &str,
    feed: ClassFeed,
) -> Vec<calibration::ScoredPoint> {
    calls
        .iter()
        .filter(|c| is_value_scored(c, feed))
        .filter_map(|c| {
            c.score.map(|score| calibration::ScoredPoint {
                score,
                age_months: months_between(today_period, &c.origin_period),
            })
        })
        .collect()
}

fn stat(points: &[calibration::ScoredPoint]) -> CalibratedStat {
    calibration::calibrated_stat(
        points,
        PRIOR_MEAN,
        PRIOR_STRENGTH,
        HALF_LIFE_MONTHS,
        ABSTAIN_N_EFF,
    )
}

/// True when `point` falls outside the stat's 90% credible interval.
fn ci_excludes(s: &CalibratedStat, point: f64) -> bool {
    point < s.lo90 || point > s.hi90
}

/// True when two 90% credible intervals do not overlap.
fn intervals_disjoint(a: &CalibratedStat, b: &CalibratedStat) -> bool {
    a.hi90 < b.lo90 || b.hi90 < a.lo90
}

/// Per-property vs pooled posterior fact (the "living property model" signal). `pooled_calls`
/// MUST be the REST-OF-PORTFOLIO for the call_type (leave-one-out: the subject property's own
/// calls excluded by the caller) so the property is never compared against a baseline that
/// contains itself. When the rest-of-portfolio baseline is thin/absent (pooled n_eff < 5,
/// including the single-property case where it is empty and the stat is prior-pinned at 0.5),
/// the fact ABSTAINS — we do not compare a property to itself or to an empty/prior baseline.
pub fn per_property_posterior_fact(
    call_type: &str,
    property_calls: &[Call],
    pooled_calls: &[Call],
    today_period: &str,
) -> DerivedFact {
    let prop_pts = collect_points(property_calls, today_period);
    let pooled_pts = collect_points(pooled_calls, today_period);
    let prop = stat(&prop_pts);
    let pooled = stat(&pooled_pts);

    // Abstain unless BOTH the property and an external rest-of-portfolio baseline are powered.
    let abstained = prop.abstain || pooled.abstain;
    let diverges = !abstained && ci_excludes(&prop, pooled.posterior_mean);
    let note = if prop.abstain {
        format!(
            "abstain: insufficient n (n_eff={:.1} < {:.0})",
            prop.n_eff, ABSTAIN_N_EFF
        )
    } else if pooled.abstain {
        format!(
            "abstain: no external portfolio baseline (rest-of-portfolio n_eff={:.1} < {:.0})",
            pooled.n_eff, ABSTAIN_N_EFF
        )
    } else if diverges {
        format!(
            "track-record divergence: property 90% CI [{:.2}, {:.2}] excludes rest-of-portfolio mean {:.2} (property {:.0}% vs rest-of-portfolio {:.0}%)",
            prop.lo90,
            prop.hi90,
            pooled.posterior_mean,
            prop.posterior_mean * 100.0,
            pooled.posterior_mean * 100.0,
        )
    } else {
        format!(
            "in-line with rest-of-portfolio: 90% CI [{:.2}, {:.2}] contains rest-of-portfolio mean {:.2}",
            prop.lo90, prop.hi90, pooled.posterior_mean
        )
    };

    DerivedFact {
        kind: FactKind::PerPropertyPosterior,
        call_type: call_type.to_string(),
        posterior: Some(prop.into()),
        pooled: Some(pooled.into()),
        diverges,
        abstained,
        note,
    }
}

/// Recent-window vs older-window posterior (non-stationarity surfacing only).
pub fn regime_window_fact(
    call_type: &str,
    property_calls: &[Call],
    today_period: &str,
    window_months: f64,
) -> DerivedFact {
    let mut recent = Vec::new();
    let mut older = Vec::new();
    for p in collect_points(property_calls, today_period) {
        if p.age_months <= window_months {
            recent.push(p);
        } else {
            older.push(p);
        }
    }
    let r = stat(&recent);
    let o = stat(&older);

    let abstained = r.abstain || o.abstain;
    let diverges = !abstained && intervals_disjoint(&r, &o);
    let note = if abstained {
        format!(
            "abstain: a window is thin (recent n_eff={:.1}, older n_eff={:.1}; need >= {:.0} each over a {:.0}-mo split)",
            r.n_eff, o.n_eff, ABSTAIN_N_EFF, window_months
        )
    } else if diverges {
        format!(
            "REGIME SHIFT surfaced: recent {:.0}% [{:.2}, {:.2}] vs older {:.0}% [{:.2}, {:.2}] — intervals disjoint",
            r.posterior_mean * 100.0,
            r.lo90,
            r.hi90,
            o.posterior_mean * 100.0,
            o.lo90,
            o.hi90,
        )
    } else {
        format!(
            "stationary: recent {:.0}% and older {:.0}% intervals overlap",
            r.posterior_mean * 100.0,
            o.posterior_mean * 100.0
        )
    };

    DerivedFact {
        kind: FactKind::RegimeWindow,
        call_type: call_type.to_string(),
        posterior: Some(r.into()),
        pooled: Some(o.into()),
        diverges,
        abstained,
        note,
    }
}

/// Value-class vs compliance-class posterior for a property (Goodhart drift). Data-gated:
/// abstains until a compliance scorer produces rows. Value side = the null-safe value feed;
/// compliance side = explicit compliance rows.
pub fn value_vs_compliance_fact(
    value_calls: &[Call],
    compliance_calls: &[Call],
    today_period: &str,
) -> DerivedFact {
    let value_pts = collect_points(value_calls, today_period);
    // Compliance side: filter to ONLY explicit compliance-class rows from whatever slice we
    // are handed (change #3) — the posterior is provably compliance-only and does not rely
    // on the caller having pre-filtered the slice.
    let comp_pts = collect_points_feed(compliance_calls, today_period, ClassFeed::Compliance);
    let v = stat(&value_pts);
    let c = stat(&comp_pts);

    // The fact is only meaningful when BOTH sides have signal; today compliance has none.
    let abstained = v.abstain || c.abstain;
    let diverges = !abstained && intervals_disjoint(&v, &c);
    let note = if c.n_eff < ABSTAIN_N_EFF {
        format!(
            "abstain: insufficient n — no compliance-class scored calls yet (compliance n_eff={:.1}); value-vs-compliance drift not computable",
            c.n_eff
        )
    } else if v.abstain {
        format!(
            "abstain: insufficient n on the value side (value n_eff={:.1} < {:.0})",
            v.n_eff, ABSTAIN_N_EFF
        )
    } else if diverges {
        format!(
            "DRIFT surfaced: value {:.0}% vs compliance {:.0}% — intervals disjoint (possible Goodhart)",
            v.posterior_mean * 100.0,
            c.posterior_mean * 100.0
        )
    } else {
        "no value-vs-compliance drift: intervals overlap".to_string()
    };

    DerivedFact {
        kind: FactKind::ValueComplianceDrift,
        call_type: "*".to_string(),
        posterior: Some(v.into()),
        pooled: Some(c.into()),
        diverges,
        abstained,
        note,
    }
}

/// Per-decision-kind posteriors over scored free-form `decision` calls (data-gated). Emits
/// one fact per decision_kind seen; if none exist, a single explicit abstention fact.
pub fn decision_kind_facts(value_calls: &[Call], today_period: &str) -> Vec<DerivedFact> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<Call>> = BTreeMap::new();
    for c in value_calls
        .iter()
        .filter(|c| c.call_type == "decision" && is_value_scored(c, ClassFeed::Value))
    {
        if let Some(kind) = c.decision_kind.clone() {
            groups.entry(kind).or_default().push(c.clone());
        }
    }
    if groups.is_empty() {
        return vec![DerivedFact {
            kind: FactKind::DecisionKind,
            call_type: "decision".to_string(),
            posterior: None,
            pooled: None,
            diverges: false,
            abstained: true,
            note: "abstain: insufficient n — no auto-scored value `decision` calls yet (decision-kind track records not computable)".to_string(),
        }];
    }
    groups
        .into_iter()
        .map(|(kind, calls)| {
            let pts = collect_points(&calls, today_period);
            let s = stat(&pts);
            let abstained = s.abstain;
            let note = if abstained {
                format!(
                    "abstain: insufficient n for decision_kind '{}' (n_eff={:.1} < {:.0})",
                    kind, s.n_eff, ABSTAIN_N_EFF
                )
            } else {
                format!(
                    "decision_kind '{}': {:.0}% [{:.2}, {:.2}] over n_eff={:.1}",
                    kind,
                    s.posterior_mean * 100.0,
                    s.lo90,
                    s.hi90,
                    s.n_eff
                )
            };
            DerivedFact {
                kind: FactKind::DecisionKind,
                call_type: format!("decision:{kind}"),
                posterior: Some(s.into()),
                pooled: None,
                diverges: false,
                abstained,
                note,
            }
        })
        .collect()
}

/// Assemble a full `PropertyModel` from injected slices (pure; DB-free for unit tests).
/// `pooled_by_type` maps call_type -> all-property scored calls for that type.
#[allow(clippy::too_many_arguments)]
pub fn build_from_calls(
    property: &str,
    property_id: &str,
    today_period: &str,
    per_type_property_calls: &[(String, Vec<Call>)],
    pooled_by_type: &[(String, Vec<Call>)],
    property_value_feed: &[Call],
    property_compliance_calls: &[Call],
) -> PropertyModel {
    let mut facts = Vec::new();
    for (call_type, prop_calls) in per_type_property_calls {
        let pooled = pooled_by_type
            .iter()
            .find(|(t, _)| t == call_type)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[]);
        facts.push(per_property_posterior_fact(
            call_type,
            prop_calls,
            pooled,
            today_period,
        ));
        facts.push(regime_window_fact(
            call_type,
            prop_calls,
            today_period,
            REGIME_WINDOW_MONTHS,
        ));
    }
    facts.push(value_vs_compliance_fact(
        property_value_feed,
        property_compliance_calls,
        today_period,
    ));
    facts.extend(decision_kind_facts(property_value_feed, today_period));

    PropertyModel {
        property: property.to_string(),
        property_id: property_id.to_string(),
        today_period: today_period.to_string(),
        facts,
    }
}

/// Async wrapper: resolve the property, fetch its scored value feed per call_type and the
/// pooled feed across all properties, then derive the model. Read-only throughout.
pub async fn build_property_model(
    pool: &SqlitePool,
    property_name: &str,
    today_period: &str,
) -> Result<PropertyModel> {
    use std::collections::HashMap;

    let prop = crate::db::require_property_by_name(pool, property_name).await?;
    let all_props = crate::db::list_properties(pool).await?;

    let mut per_type_property_calls: Vec<(String, Vec<Call>)> = Vec::new();
    let mut pooled_by_type: Vec<(String, Vec<Call>)> = Vec::new();
    for &ct in VALUE_ENGINE_CALL_TYPES {
        // Change #1 — single query per call_type instead of the old per-property N+1. Fetch
        // every property's scored value-class calls for `ct` at once (SAME locked predicate
        // as `fetch_scored_calls`), then partition in Rust. Because the global result keeps
        // `scored_at DESC` order, filtering it per property preserves each property's
        // ordering, so this is result-equivalent to the old per-property fetches.
        let all_ct = crate::db::fetch_scored_calls_all_properties(pool, ct, 100_000).await?;
        let mut by_prop: HashMap<String, Vec<Call>> = HashMap::new();
        for c in all_ct {
            by_prop.entry(c.property_id.clone()).or_default().push(c);
        }
        let prop_calls = by_prop.remove(&prop.id).unwrap_or_default();
        // Leave-one-out: the baseline is the REST-OF-PORTFOLIO, so a property is never
        // compared against a pool containing itself (H1 fix). Reassemble it property-by-
        // property in `all_props` (name) order — byte-identical to the old loop's
        // concatenation. In the single-property state this is empty → the per-property fact
        // abstains (no external baseline).
        let mut pooled: Vec<Call> = Vec::new();
        for p in &all_props {
            if p.id == prop.id {
                continue;
            }
            if let Some(v) = by_prop.remove(&p.id) {
                pooled.extend(v);
            }
        }
        per_type_property_calls.push((ct.to_string(), prop_calls));
        pooled_by_type.push((ct.to_string(), pooled));
    }

    // Property-wide feeds for the data-gated facts.
    let value_feed = crate::db::fetch_property_scored_calls(pool, &prop.id, None, 100_000).await?;
    let compliance_calls =
        crate::db::fetch_property_scored_calls(pool, &prop.id, Some("compliance"), 100_000).await?;

    Ok(build_from_calls(
        &prop.name,
        &prop.id,
        today_period,
        &per_type_property_calls,
        &pooled_by_type,
        &value_feed,
        &compliance_calls,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored_call(call_type: &str, score: f64, origin_period: &str) -> Call {
        Call {
            id: format!("c-{call_type}-{origin_period}-{score}"),
            property_id: "p".into(),
            origin_period: origin_period.into(),
            call_type: call_type.into(),
            status: "scored".into(),
            made_at: "t".into(),
            mature_by: "2026-06".into(),
            confidence: Some(0.5),
            payload_json: "{}".into(),
            outcome_json: None,
            score: Some(score),
            outcome_summary: None,
            scored_at: Some("t".into()),
            source_task_run_id: None,
            confounded: false,
            decision_kind: None,
            entities_json: None,
            outcome_mode: None,
            value_class: None, // legacy value feed (NULL stays in, K1)
            acted_on: false,
            accepted_recall: None,
            source_surface: None,
            context_json: None,
            operator_outcome: None,
            resolved_by: None,
            self_graded: false,
            regime_tag: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    fn many(call_type: &str, score: f64, origin_period: &str, n: usize) -> Vec<Call> {
        (0..n)
            .map(|i| {
                let mut c = scored_call(call_type, score, origin_period);
                c.id = format!("{call_type}-{origin_period}-{score}-{i}");
                c
            })
            .collect()
    }

    #[test]
    fn diverging_property_flagged_inline_not() {
        // Leave-one-out: the pool is the REST-OF-PORTFOLIO and EXCLUDES the subject.
        // High property: 20 hits at 1.0. Rest-of-portfolio is a powered low cohort ->
        // property CI excludes the baseline mean -> diverges.
        let high: Vec<Call> = many("t12_reversion", 1.0, "2026-05", 20);
        let rest_low = many("t12_reversion", 0.0, "2026-05", 60); // rest-of-portfolio, no subject
        let f = per_property_posterior_fact("t12_reversion", &high, &rest_low, "2026-06");
        assert!(!f.abstained);
        assert!(
            f.diverges,
            "high property must diverge from low rest-of-portfolio: {f:?}"
        );
        assert!(f.note.contains("track-record divergence"));

        // In-line property at 0.5; rest-of-portfolio also centered at 0.5 -> not diverging.
        let inline: Vec<Call> = many("t12_reversion", 0.5, "2026-05", 20);
        let mut rest_mid = many("t12_reversion", 1.0, "2026-05", 30);
        rest_mid.extend(many("t12_reversion", 0.0, "2026-05", 30)); // 50% rest, excludes subject
        let f2 = per_property_posterior_fact("t12_reversion", &inline, &rest_mid, "2026-06");
        assert!(!f2.abstained);
        assert!(!f2.diverges, "in-line property must NOT diverge: {f2:?}");
        assert!(f2.note.contains("in-line with rest-of-portfolio"));
    }

    #[test]
    fn thin_property_abstains_no_claim() {
        // 3 effective fresh scored calls -> n_eff < 5 -> abstain, no divergence claim.
        let thin = many("t12_reversion", 1.0, "2026-05", 3);
        let pooled = many("t12_reversion", 0.0, "2026-05", 60);
        let f = per_property_posterior_fact("t12_reversion", &thin, &pooled, "2026-06");
        assert!(f.abstained);
        assert!(!f.diverges);
        assert!(f.note.contains("insufficient n"));
    }

    #[test]
    fn single_property_empty_baseline_abstains_h1() {
        // H1 regression: subject is well-powered (n_eff>=5) but there is NO rest-of-portfolio
        // (single-property state). The fact must ABSTAIN against the absent external baseline,
        // never emit "in-line with portfolio" (which would be comparing the property to itself).
        let high = many("t12_reversion", 1.0, "2026-06", 20);
        let f = per_property_posterior_fact("t12_reversion", &high, &[], "2026-06");
        assert!(f.abstained, "empty external baseline must abstain: {f:?}");
        assert!(!f.diverges);
        assert!(
            f.note.contains("no external portfolio baseline"),
            "note must name the absent baseline, not claim in-line: {f:?}"
        );
    }

    #[test]
    fn thin_external_baseline_abstains_h1() {
        // H1 regression: subject powered, but rest-of-portfolio is thin (n_eff < 5). Do not
        // diverge against a prior-pinned (0.5) baseline -> abstain.
        let high = many("t12_reversion", 1.0, "2026-06", 20);
        let thin_rest = many("t12_reversion", 0.0, "2026-06", 3); // n_eff 3 < 5
        let f = per_property_posterior_fact("t12_reversion", &high, &thin_rest, "2026-06");
        assert!(f.abstained, "thin external baseline must abstain: {f:?}");
        assert!(!f.diverges);
        assert!(f.note.contains("no external portfolio baseline"));
    }

    #[test]
    fn confounded_and_compliance_excluded_from_posterior() {
        // Baseline: 10 fresh hits (age 0) -> high posterior, n_eff == 10 exactly.
        let mut calls = many("t12_reversion", 1.0, "2026-06", 10);
        let base = per_property_posterior_fact("t12_reversion", &calls, &calls, "2026-06");

        // Add 30 confounded misses + 30 compliance misses. If the filter leaks, posterior
        // would crater. It must not change (both excluded, K1/K2).
        let mut confounded = many("t12_reversion", 0.0, "2026-06", 30);
        for c in &mut confounded {
            c.confounded = true;
        }
        let mut compliance = many("t12_reversion", 0.0, "2026-06", 30);
        for c in &mut compliance {
            c.value_class = Some("compliance".into());
        }
        calls.extend(confounded);
        calls.extend(compliance);
        let after = per_property_posterior_fact("t12_reversion", &calls, &calls, "2026-06");

        assert!(
            (base.posterior.as_ref().unwrap().posterior_mean
                - after.posterior.as_ref().unwrap().posterior_mean)
                .abs()
                < 1e-9,
            "confounded/compliance rows must not move the posterior: {base:?} vs {after:?}"
        );
        assert!(
            (after.posterior.as_ref().unwrap().n_eff - 10.0).abs() < 1e-9,
            "n_eff must still reflect only the 10 valid rows"
        );
    }

    #[test]
    fn regime_window_surfaces_divergence() {
        // Recent window (fresh) all hits; older window (>6mo) all misses, both well-powered.
        let mut calls = many("t12_reversion", 1.0, "2026-06", 20); // age 0
        calls.extend(many("t12_reversion", 0.0, "2025-01", 40)); // age 17mo, older
        let f = regime_window_fact("t12_reversion", &calls, "2026-06", REGIME_WINDOW_MONTHS);
        assert!(!f.abstained, "both windows powered: {f:?}");
        assert!(
            f.diverges,
            "recent hits vs older misses must surface: {f:?}"
        );
        assert!(f.note.contains("REGIME SHIFT"));
    }

    #[test]
    fn value_vs_compliance_abstains_today() {
        // Value side powered, compliance side empty -> abstain (data-gated).
        let value = many("t12_reversion", 1.0, "2026-05", 20);
        let f = value_vs_compliance_fact(&value, &[], "2026-06");
        assert!(f.abstained);
        assert!(f.note.contains("no compliance-class scored calls"));
    }

    #[test]
    fn renewal_rec_null_class_excluded_from_value_posterior() {
        // Change #2: a future emit-path regression writes renewal_rec rows with
        // value_class = NULL (the closeness-COMPLIANCE score). The null-safe value-feed
        // predicate would admit them, but the guard skips NULL-class renewal_rec. With 20
        // such rows the subject would be well-powered IF they leaked; instead the value
        // feed is empty and the property side ABSTAINS — proving they never enter.
        let leaky_nulls = many("renewal_rec", 1.0, "2026-06", 20); // value_class = None
                                                                   // Powered rest-of-portfolio baseline of EXPLICIT value-class renewal_rec rows (so the
                                                                   // baseline is itself admitted by the guard and the abstention below is driven by the
                                                                   // subject's empty value feed, not an empty pool).
        let mut powered_pool = many("renewal_rec", 0.0, "2026-06", 60);
        for c in &mut powered_pool {
            c.value_class = Some("value".into());
        }
        let f = per_property_posterior_fact("renewal_rec", &leaky_nulls, &powered_pool, "2026-06");
        assert!(
            f.abstained,
            "NULL-class renewal_rec must not power a value posterior: {f:?}"
        );
        assert!(!f.diverges);
        assert!(
            (f.posterior.as_ref().unwrap().n_eff).abs() < 1e-9,
            "value feed must see 0 renewal_rec rows: {f:?}"
        );
        assert!(f.note.contains("insufficient n"));

        // Contrast: a GENUINE value renewal_rec (explicit non-compliance class) IS admitted,
        // so the future value-renewal scorer is picked up automatically.
        let mut genuine = many("renewal_rec", 1.0, "2026-06", 20);
        for c in &mut genuine {
            c.value_class = Some("value".into());
        }
        let g = per_property_posterior_fact("renewal_rec", &genuine, &powered_pool, "2026-06");
        assert!(
            !g.abstained,
            "explicit value-class renewal_rec must enter the value posterior: {g:?}"
        );
        assert!(g.posterior.as_ref().unwrap().n_eff > 15.0, "{g:?}");
    }

    #[test]
    fn compliance_side_is_compliance_only_even_from_mixed_slice() {
        // Change #3: hand the compliance side a MIXED slice (compliance hits + value
        // misses). The compliance posterior must reflect ONLY the compliance rows — it
        // must not depend on the caller having pre-filtered the slice.
        let mut mixed = many("t12_reversion", 1.0, "2026-06", 30); // compliance hits
        for c in &mut mixed {
            c.value_class = Some("compliance".into());
        }
        mixed.extend(many("t12_reversion", 0.0, "2026-06", 30)); // value misses (NULL class)

        let value_side = many("t12_reversion", 0.5, "2026-06", 30);
        let f = value_vs_compliance_fact(&value_side, &mixed, "2026-06");
        let comp = f.pooled.as_ref().unwrap();
        // 30 compliance hits only (n_eff ~ 30, not ~60); the 30 value misses are excluded,
        // so the mean stays high rather than being dragged toward 0.5.
        assert!(
            comp.n_eff > 25.0 && comp.n_eff < 31.0,
            "compliance n_eff must reflect only the 30 compliance rows, not all 60: {comp:?}"
        );
        assert!(
            comp.posterior_mean > 0.8,
            "value rows must not pollute the compliance-only posterior: {comp:?}"
        );
    }

    #[test]
    fn decision_kind_abstains_today() {
        // No `decision` call_type rows -> single explicit abstention fact.
        let value = many("t12_reversion", 1.0, "2026-05", 20);
        let facts = decision_kind_facts(&value, "2026-06");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].abstained);
        assert!(facts[0]
            .note
            .contains("no auto-scored value `decision` calls"));
    }
}
