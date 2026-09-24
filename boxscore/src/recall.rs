//! Recall service (B): structured similarity over past calls + a calibrated stat,
//! surfaced at the point of decision. No embeddings — transparent, debuggable matching.
use crate::calibration;
use crate::models::Call;
use anyhow::Result;
use sqlx::SqlitePool;

pub struct RecallCtx {
    pub property_id: String,
    pub call_type: Option<String>,
    pub decision_kind: Option<String>,
    pub entity_keys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Neighbor {
    pub id: String,
    pub summary: String,
    pub score: Option<f64>,
    pub status: String,
    pub age_months: f64,
    pub similarity: f64,
}

pub struct Recall {
    pub stat: calibration::CalibratedStat,
    pub neighbors: Vec<Neighbor>,
}

/// Absolute whole-month distance between two YYYY-MM periods.
pub fn months_between(a_period: &str, b_period: &str) -> f64 {
    fn ym(p: &str) -> (i64, i64) {
        let (y, m) = p.split_once('-').unwrap_or(("0", "0"));
        (y.parse().unwrap_or(0), m.parse().unwrap_or(0))
    }
    let (ay, am) = ym(a_period);
    let (by, bm) = ym(b_period);
    (((ay * 12 + am) - (by * 12 + bm)).abs()) as f64
}

/// Structured similarity in [0,1]: property is a hard filter (caller guarantees same property).
/// Weighted: decision_kind match 0.5, entity-key overlap up to 0.5.
pub fn similarity(ctx: &RecallCtx, call: &Call) -> f64 {
    let mut s = 0.0;
    if ctx.decision_kind.is_some() && ctx.decision_kind == call.decision_kind {
        s += 0.5;
    }
    if !ctx.entity_keys.is_empty() {
        let hay = call.entities_json.clone().unwrap_or_default();
        let hits = ctx
            .entity_keys
            .iter()
            .filter(|k| {
                let needle = k.split(':').next_back().unwrap_or(k);
                hay.contains(needle)
            })
            .count();
        s += 0.5 * (hits as f64 / ctx.entity_keys.len() as f64);
    }
    s
}

pub async fn recall(
    pool: &SqlitePool,
    ctx: &RecallCtx,
    today_period: &str,
    k: usize,
) -> Result<Recall> {
    let call_type = ctx
        .call_type
        .clone()
        .unwrap_or_else(|| "decision".to_string());
    let rows = crate::db::fetch_calls_for_recall(pool, &ctx.property_id, &call_type).await?;

    // Calibrated stat from VALUE-class scored calls only.
    // compliance/human-only calls are excluded by the filter below, not in SQL.
    let points: Vec<calibration::ScoredPoint> = rows
        .iter()
        .filter(|c| {
            c.status == "scored"
                && c.score.is_some()
                && c.value_class.as_deref() != Some("compliance")
        })
        .map(|c| calibration::ScoredPoint {
            score: c.score.unwrap(),
            age_months: months_between(today_period, &c.origin_period),
        })
        .collect();
    let stat = calibration::calibrated_stat(
        &points,
        calibration::PRIOR_MEAN,
        calibration::PRIOR_STRENGTH,
        calibration::HALF_LIFE_MONTHS,
        calibration::ABSTAIN_N_EFF,
    );

    // Neighbors: include open/pending (visible denominator), age-stamped, recency-adjusted rank.
    let mut neighbors: Vec<Neighbor> = rows
        .iter()
        .map(|c| {
            let age = months_between(today_period, &c.origin_period);
            let sim = similarity(ctx, c);
            Neighbor {
                id: c.id.clone(),
                summary: c
                    .outcome_summary
                    .clone()
                    .unwrap_or_else(|| c.status.clone()),
                score: c.score,
                status: c.status.clone(),
                age_months: age,
                similarity: sim,
            }
        })
        .collect();
    // recency-adjusted: similarity decayed by age so a stale near-match ranks below a fresh one.
    neighbors.sort_by(|a, b| {
        let ra =
            a.similarity * calibration::decay_weight(a.age_months, calibration::HALF_LIFE_MONTHS);
        let rb =
            b.similarity * calibration::decay_weight(b.age_months, calibration::HALF_LIFE_MONTHS);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    neighbors.truncate(k);
    Ok(Recall { stat, neighbors })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_between_counts_calendar_months() {
        assert!((months_between("2026-06", "2026-05") - 1.0).abs() < 1e-9);
        assert!((months_between("2027-01", "2026-01") - 12.0).abs() < 1e-9);
        assert!((months_between("2026-05", "2026-06") - 1.0).abs() < 1e-9); // absolute
    }

    #[test]
    fn similarity_rewards_kind_and_entity_overlap() {
        let ctx = RecallCtx {
            property_id: "p".into(),
            call_type: Some("decision".into()),
            decision_kind: Some("renewal_override".into()),
            entity_keys: vec!["unit:2BR".into()],
        };
        let mut base = sample_call();
        base.decision_kind = Some("renewal_override".into());
        base.entities_json = Some("[{\"type\":\"unit\",\"id\":\"2BR\"}]".into());
        let s_match = similarity(&ctx, &base);
        let mut other = base.clone();
        other.decision_kind = Some("capex".into());
        other.entities_json = Some("[]".into());
        assert!(s_match > similarity(&ctx, &other));
    }

    #[tokio::test]
    async fn recall_abstains_on_thin_history_but_returns_neighbors() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        sqlx::query("INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, score, value_class, decision_kind, entities_json, scored_at, created_at, updated_at) VALUES ('n1', ?, '2026-04', 'decision', 'scored', 't', '2026-05', '{}', 1.0, 'value', 'renewal_override', '[{\"type\":\"unit\",\"id\":\"2BR\"}]', 't', 't', 't')")
            .bind(&pid).execute(&pool).await.unwrap();
        let ctx = RecallCtx {
            property_id: pid.clone(),
            call_type: Some("decision".into()),
            decision_kind: Some("renewal_override".into()),
            entity_keys: vec!["unit:2BR".into()],
        };
        let r = recall(&pool, &ctx, "2026-06", 5).await.unwrap();
        assert!(r.stat.abstain); // n_eff=1 < 5
        assert_eq!(r.neighbors.len(), 1);
        assert!(r.neighbors[0].similarity > 0.5);
    }

    fn sample_call() -> Call {
        Call {
            id: "x".into(),
            property_id: "p".into(),
            origin_period: "2026-05".into(),
            call_type: "decision".into(),
            status: "open".into(),
            made_at: "t".into(),
            mature_by: "2026-06".into(),
            confidence: None,
            payload_json: "{}".into(),
            outcome_json: None,
            score: None,
            outcome_summary: None,
            scored_at: None,
            source_task_run_id: None,
            confounded: false,
            decision_kind: None,
            entities_json: None,
            outcome_mode: None,
            value_class: None,
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
}
