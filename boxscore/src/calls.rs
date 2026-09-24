//! The Call Ledger: lifecycle types + domain scorers for the compounding flywheel.
use crate::models::Call;
use crate::variance::VarianceAnalysisResult;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const T12_MIN_HISTORY: usize = 6;
const T12_DEV_PCT: f64 = 0.20;
const T12_DEV_ABS: f64 = 1000.0;
const T12_TOP_N: usize = 8;

pub fn next_period(period: &str) -> Result<String> {
    let (y, m) = period
        .split_once('-')
        .ok_or_else(|| anyhow!("bad period: {period}"))?;
    let year: i32 = y.parse()?;
    let month: u32 = m.parse()?;
    let (ny, nm) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    Ok(format!("{ny:04}-{nm:02}"))
}

pub fn prev_period(period: &str) -> Result<String> {
    let (y, m) = period
        .split_once('-')
        .ok_or_else(|| anyhow!("bad period: {period}"))?;
    let year: i32 = y.parse()?;
    let month: u32 = m.parse()?;
    let (py, pm) = if month <= 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    };
    Ok(format!("{py:04}-{pm:02}"))
}

/// Bootstraps the harness's track record from historical GL.
///
/// Walks `lookback` periods back from `through` (inclusive), in chronological
/// order, running `analyze_variance` for each period — skipping any period that
/// has no GL data rather than failing outright. Then scores all matured calls
/// across those periods. Fully idempotent: re-running over already-processed
/// periods is a no-op (the emission and scoring guards in those functions
/// prevent double-counting).
///
/// Returns `(analyzed, scored)`.
pub async fn backfill_noi_diagnosis(
    pool: &SqlitePool,
    property: &str,
    through: &str,
    lookback: u32,
) -> Result<(usize, usize)> {
    // Build the period list: start at `through`, walk back `lookback` times,
    // then reverse to chronological order (oldest first).
    let mut periods = vec![through.to_string()];
    let mut p = through.to_string();
    for _ in 0..lookback {
        p = prev_period(&p)?;
        periods.push(p.clone());
    }
    periods.reverse();

    let report_dir = std::env::temp_dir().join("boxscore_backfill");
    std::fs::create_dir_all(&report_dir).ok();

    let mut analyzed = 0usize;
    for period in &periods {
        match crate::variance::analyze_variance(
            pool,
            crate::variance::VarianceRequest {
                property: property.to_string(),
                period: period.clone(),
            },
            &report_dir,
        )
        .await
        {
            Ok(_) => analyzed += 1,
            Err(e) => tracing::warn!(
                period = %period,
                error = %e,
                "backfill: variance skipped for period"
            ),
        }
    }

    let mut scored = 0usize;
    for period in &periods {
        scored += score_due_calls(pool, period).await?;
    }

    Ok((analyzed, scored))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackRecord {
    pub property_id: String,
    pub call_type: String,
    pub n: usize,
    pub batting_avg: f64,
    pub calibration_gap: f64,
}

impl TrackRecord {
    pub fn from_scored(property_id: &str, call_type: &str, calls: &[Call]) -> Self {
        // Confounded calls (outcome changed by an intervention) are excluded from the
        // batting average so a successful mitigation never scores as a miss (migration 010, K2).
        let scores: Vec<f64> = calls
            .iter()
            .filter(|c| !c.confounded)
            .filter_map(|c| c.score)
            .collect();
        let confs: Vec<f64> = calls
            .iter()
            .filter(|c| !c.confounded)
            .filter_map(|c| c.confidence)
            .collect();
        let n = scores.len();
        let batting_avg = if n == 0 {
            0.0
        } else {
            scores.iter().sum::<f64>() / n as f64
        };
        let mean_conf = if confs.is_empty() {
            0.0
        } else {
            confs.iter().sum::<f64>() / confs.len() as f64
        };
        TrackRecord {
            property_id: property_id.to_string(),
            call_type: call_type.to_string(),
            n,
            batting_avg,
            calibration_gap: mean_conf - batting_avg,
        }
    }

    pub fn headline(&self) -> String {
        format!(
            "{}: {:.0}% ({} scored, calibration {:+.2})",
            self.call_type,
            self.batting_avg * 100.0,
            self.n,
            self.calibration_gap
        )
    }
}

/// Result of scoring one matured call against actuals.
pub struct Scored {
    pub score: f64,
    pub outcome_json: serde_json::Value,
    pub outcome_summary: String,
}

/// A domain scorer. Returns `Ok(None)` when the outcome data for the call's
/// `mature_by` period is not yet available/fresh (the call stays `open`).
#[async_trait]
pub trait Scorer {
    async fn score(&self, pool: &SqlitePool, call: &Call) -> Result<Option<Scored>>;
}

/// Scores `noi_diagnosis` calls against the next-period GL actuals.
///
/// Freshness gate: returns `Ok(None)` if the `mature_by` period has no actuals yet.
/// Hit/miss logic compares magnitudes (sign-convention agnostic):
/// - `"normalize"` → next period's absolute amount is smaller than baseline
/// - `"worsen"`    → next period's absolute amount is larger
/// - anything else → amounts are within ±10% of each other ("persist")
pub struct NoiDiagnosisScorer;

#[async_trait]
impl Scorer for NoiDiagnosisScorer {
    async fn score(&self, pool: &SqlitePool, call: &Call) -> Result<Option<Scored>> {
        // Freshness gate: only score when the mature_by period has GL actuals.
        if !crate::db::period_has_actuals(pool, &call.property_id, &call.mature_by).await? {
            return Ok(None);
        }
        let payload: serde_json::Value = serde_json::from_str(&call.payload_json)?;
        let account_code = payload["account_code"].as_str().unwrap_or_default();
        let baseline = payload["baseline_actual"].as_f64().unwrap_or(0.0);
        let direction = payload["expected_direction"]
            .as_str()
            .unwrap_or("normalize");

        let next_actual = crate::db::account_actual_for_period(
            pool,
            &call.property_id,
            &call.mature_by,
            account_code,
        )
        .await?;

        // Compare magnitudes (sign conventions vary by account; magnitude is robust).
        let b = baseline.abs();
        let n = next_actual.abs();
        let hit = match direction {
            "normalize" => n < b,
            "worsen" => n > b,
            _ /* persist */ => (n - b).abs() <= 0.10 * b.max(1.0),
        };
        let score = if hit { 1.0 } else { 0.0 };
        let summary = format!(
            "{} {}: {:.0} -> {:.0} ({}) {}",
            payload["category"].as_str().unwrap_or(""),
            account_code,
            b,
            n,
            direction,
            if hit { "HIT" } else { "MISS" }
        );
        Ok(Some(Scored {
            score,
            outcome_json: serde_json::json!({ "baseline": b, "next_actual": n, "hit": hit }),
            outcome_summary: summary,
        }))
    }
}

/// Auto-emit one low-confidence `noi_diagnosis` call per top negative driver:
/// "this overage will normalize next period." Scored later against next-period GL.
pub async fn emit_noi_diagnosis_calls(
    pool: &SqlitePool,
    property_id: &str,
    analysis: &VarianceAnalysisResult,
) -> Result<usize> {
    // Idempotency guard: if calls for this property+period already exist, skip emission.
    // The first variance run for a period emits the calls; re-runs are no-ops,
    // preserving the original predictions as history.
    if crate::db::calls_exist_for(pool, property_id, &analysis.period, "noi_diagnosis").await? {
        return Ok(0);
    }
    let mature_by = next_period(&analysis.period)?;
    let mut emitted = 0;
    for driver in analysis.top_negative_drivers.iter().take(5) {
        // Learn the direction from the harness's own track record: if past "normalize" calls
        // for this category+property were usually wrong (structural costs like Taxes/Insurance that
        // don't revert), flip to "persist" when the Beta posterior gives P(rate < 0.5) > 0.75.
        // Defaults to "normalize" when no property-scoped history exists (safe shrinkage).
        let expected_direction =
            match crate::db::category_normalize_rate(pool, property_id, &driver.category).await? {
                Some((rate, n)) => {
                    // Build Beta posterior: alpha = successes + k*m0, beta = failures + k*(1-m0).
                    // With k=8, m0=0.5 (prior constants from calibration module).
                    let k = crate::calibration::PRIOR_STRENGTH;
                    let m0 = crate::calibration::PRIOR_MEAN;
                    let alpha = rate * n as f64 + k * m0;
                    let beta_param = (1.0 - rate) * n as f64 + k * (1.0 - m0);
                    if crate::calibration::prob_below(0.5, alpha, beta_param) > 0.75 {
                        "persist"
                    } else {
                        "normalize"
                    }
                }
                _ => "normalize",
            };
        let payload = serde_json::json!({
            "account_code": driver.account_code,
            "account_name": driver.account_name,
            "category": driver.category,
            "baseline_actual": driver.actual,
            "expected_direction": expected_direction,
        });
        crate::db::insert_call(
            pool,
            property_id,
            &analysis.period,
            "noi_diagnosis",
            &mature_by,
            Some(0.40),
            &payload.to_string(),
            Some(&analysis.task_run_id),
        )
        .await?;
        emitted += 1;
    }
    Ok(emitted)
}

/// Emit `t12_reversion` calls for `period`: P&L accounts whose month-`period` actual deviates
/// materially from their trailing-12-month mean ("this will revert toward the mean next month").
/// Idempotent per (property, period, t12_reversion).
pub async fn emit_t12_reversion_calls(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<usize> {
    if crate::db::calls_exist_for(pool, property_id, period, "t12_reversion").await? {
        return Ok(0);
    }
    let mature_by = next_period(period)?;
    let accounts = crate::db::pl_accounts_for_period(pool, property_id, period).await?;
    // (account_code, account_name, category, mean, actual, dev)
    let mut candidates: Vec<(String, String, String, f64, f64, f64)> = Vec::new();
    for (code, name) in accounts {
        // Skip non-P&L accounts (balance-sheet lines that leak into the Yardi budget-comparison
        // universe, e.g. deposits/receivables) — only emit for accounts with a real NOI category.
        let category = match crate::db::category_for_account(pool, property_id, &code).await? {
            Some(c) if c != "Unmapped" => c,
            _ => continue,
        };
        let Some(actual) = crate::db::monthly_actual(pool, property_id, period, &code).await?
        else {
            continue;
        };
        let Some((mean, n)) = crate::db::t12_mean(pool, property_id, period, &code).await? else {
            continue;
        };
        if n < T12_MIN_HISTORY {
            continue;
        }
        let dev = actual - mean;
        if dev.abs() <= (T12_DEV_PCT * mean.abs()).max(T12_DEV_ABS) {
            continue;
        }
        candidates.push((code, name, category, mean, actual, dev));
    }
    candidates.sort_by(|a, b| {
        b.5.abs()
            .partial_cmp(&a.5.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(T12_TOP_N);
    let mut emitted = 0;
    for (code, name, category, mean, actual, _dev) in candidates {
        let payload = serde_json::json!({
            "account_code": code,
            "account_name": name,
            "category": category,
            "baseline_mean": mean,
            "actual_at_origin": actual,
        });
        crate::db::insert_call(
            pool,
            property_id,
            period,
            "t12_reversion",
            &mature_by,
            Some(0.5),
            &payload.to_string(),
            None,
        )
        .await?;
        emitted += 1;
    }
    Ok(emitted)
}

/// Scores `delinquency_risk` calls against receivables in the mature_by period.
///
/// Freshness gate: returns `Ok(None)` if no receivables snapshot exists for the mature_by period.
/// Scoring:
/// - With a `probability` field → Brier-based score: `1 - (p - actual)²`
/// - Without probability → directional hit: 1.0 if resident became delinquent, 0.0 otherwise.
pub struct DelinquencyRiskScorer;

#[async_trait]
impl Scorer for DelinquencyRiskScorer {
    async fn score(&self, pool: &SqlitePool, call: &Call) -> Result<Option<Scored>> {
        // Freshness gate: need a receivables snapshot for the mature_by period.
        if !crate::db::receivables_snapshot_exists(pool, &call.property_id, &call.mature_by).await?
        {
            return Ok(None);
        }
        let payload: serde_json::Value = serde_json::from_str(&call.payload_json)?;
        let resident_code = payload["resident_code"].as_str().unwrap_or_default();
        let prob = payload["probability"].as_f64();

        let owed = crate::db::unit_delinquent_total(
            pool,
            &call.property_id,
            resident_code,
            &call.mature_by,
        )
        .await?;
        let became_delinquent = owed > 0.0;
        let actual = if became_delinquent { 1.0 } else { 0.0 };

        // With a probability -> Brier-based score (1 - (p-actual)^2). Without -> directional hit
        // (the call asserts "risky", so a hit = actually became delinquent).
        let score = match prob {
            Some(p) => 1.0 - (p - actual).powi(2),
            None => actual,
        };
        let summary = format!(
            "{}: predicted risk{} -> owed {:.0} ({})",
            resident_code,
            prob.map(|p| format!(" {:.0}%", p * 100.0))
                .unwrap_or_default(),
            owed,
            if became_delinquent {
                "delinquent HIT"
            } else {
                "current MISS"
            }
        );
        Ok(Some(Scored {
            score,
            outcome_json: serde_json::json!({ "owed": owed, "became_delinquent": became_delinquent }),
            outcome_summary: summary,
        }))
    }
}

/// Scores `renewal_rec` calls against the rent-roll actuals in the mature_by period.
///
/// Freshness gate: returns `Ok(None)` if no leases snapshot exists for the mature_by period.
/// Scoring:
/// - Resident still on roll (same unit + resident_code): closeness score = `(1 - |actual - rec| / rec).max(0)`
/// - Resident absent (did not renew): score 0.0
pub struct RenewalRecScorer;

#[async_trait]
impl Scorer for RenewalRecScorer {
    async fn score(&self, pool: &SqlitePool, call: &Call) -> Result<Option<Scored>> {
        if !crate::db::leases_snapshot_exists(pool, &call.property_id, &call.mature_by).await? {
            return Ok(None);
        }
        let payload: serde_json::Value = serde_json::from_str(&call.payload_json)?;
        let unit = payload["unit_label"].as_str().unwrap_or_default();
        let resident = payload["resident_code"].as_str().unwrap_or_default();
        let rec = payload["recommended_rent"].as_f64().unwrap_or(0.0);

        let actual_rent =
            crate::db::unit_lease_rent(pool, &call.property_id, unit, resident, &call.mature_by)
                .await?;
        let (score, summary, outcome) = match actual_rent {
            Some(actual) => {
                // Renewed: score on closeness of actual to recommended.
                let closeness = if rec > 0.0 {
                    (1.0 - (actual - rec).abs() / rec).max(0.0)
                } else {
                    0.0
                };
                (
                    closeness,
                    format!("{unit}/{resident}: renewed at {actual:.0} vs rec {rec:.0} (closeness {:.2})", closeness),
                    serde_json::json!({ "renewed": true, "actual_rent": actual, "recommended_rent": rec }),
                )
            }
            None => (
                0.0,
                format!("{unit}/{resident}: did not renew (resident absent)"),
                serde_json::json!({ "renewed": false, "recommended_rent": rec }),
            ),
        };
        Ok(Some(Scored {
            score,
            outcome_json: outcome,
            outcome_summary: summary,
        }))
    }
}

/// Scores `t12_reversion` calls against the `monthly_actuals` table.
///
/// Freshness gate: returns `Ok(None)` when no `monthly_actuals` row exists for the
/// `mature_by` period. Score is graded: the fraction of the original deviation from the
/// trailing-12 mean that closed. 1.0 = fully reverted (or past) the mean, 0.0 = no
/// reversion at all.
pub struct T12ReversionScorer;

#[async_trait]
impl Scorer for T12ReversionScorer {
    async fn score(&self, pool: &SqlitePool, call: &Call) -> Result<Option<Scored>> {
        // Freshness gate: need monthly_actuals for the mature_by month.
        if !crate::db::monthly_actuals_exist(pool, &call.property_id, &call.mature_by).await? {
            return Ok(None);
        }
        let payload: serde_json::Value = serde_json::from_str(&call.payload_json)?;
        let account_code = payload["account_code"].as_str().unwrap_or_default();
        let mean = payload["baseline_mean"].as_f64().unwrap_or(0.0);
        let actual0 = payload["actual_at_origin"].as_f64().unwrap_or(0.0);
        let actual1 =
            crate::db::monthly_actual(pool, &call.property_id, &call.mature_by, account_code)
                .await?
                .unwrap_or(0.0);

        let dev0 = actual0 - mean;
        let dev1 = actual1 - mean;
        // Graded reversion: fraction of the deviation that closed. 1.0 = fully reverted (or past) the mean.
        let score = if dev0.abs() < 1e-9 {
            1.0
        } else {
            ((dev0.abs() - dev1.abs()) / dev0.abs()).clamp(0.0, 1.0)
        };
        let summary = format!(
            "{} {}: dev {:.0} -> {:.0} (reverted {:.0}%)",
            payload["account_name"].as_str().unwrap_or(""),
            account_code,
            dev0,
            dev1,
            score * 100.0
        );
        Ok(Some(Scored {
            score,
            outcome_json: serde_json::json!({
                "baseline_mean": mean,
                "actual_at_origin": actual0,
                "next_actual": actual1,
                "reverted_fraction": score,
            }),
            outcome_summary: summary,
        }))
    }
}

fn scorer_for(call_type: &str) -> Option<Box<dyn Scorer + Send + Sync>> {
    match call_type {
        "noi_diagnosis" => Some(Box::new(NoiDiagnosisScorer)),
        "delinquency_risk" => Some(Box::new(DelinquencyRiskScorer)),
        "renewal_rec" => Some(Box::new(RenewalRecScorer)),
        "t12_reversion" => Some(Box::new(T12ReversionScorer)),
        _ => None,
    }
}

/// Score every matured, still-open call for `period`. Freshness-gated per scorer.
/// Writes the result back to the call and records a `track_record` memory.
pub async fn score_due_calls(pool: &SqlitePool, period: &str) -> Result<usize> {
    let period = crate::db::normalize_period_label(period)?;
    let due = crate::db::calls_due_for_scoring(pool, &period).await?;
    let mut scored = 0;
    for call in due {
        let Some(scorer) = scorer_for(&call.call_type) else {
            continue;
        };
        if let Some(result) = scorer.score(pool, &call).await? {
            crate::db::mark_call_scored(
                pool,
                &call.id,
                result.score,
                &result.outcome_json.to_string(),
                &result.outcome_summary,
            )
            .await?;
            // Recall surface: refresh the track-record memory for this lane+type,
            // but ONLY for value-class call types. Compliance calls (e.g. renewal_rec)
            // produce empty-set headlines that would mislead the Ask prompt ("0% (0 scored)").
            if scorer_value_class(&call.call_type) != "compliance" {
                let recent =
                    crate::db::fetch_scored_calls(pool, &call.property_id, &call.call_type, 20)
                        .await?;
                let tr = TrackRecord::from_scored(&call.property_id, &call.call_type, &recent);
                crate::db::upsert_memory(
                    pool,
                    "track_record",
                    &call.call_type,
                    &call.property_id,
                    &tr.headline(),
                    tr.batting_avg,
                    None,
                )
                .await?;
            }
            scored += 1;
        }
    }
    Ok(scored)
}

/// Backfill the t12_reversion track record across history: for each period in
/// [from .. through] (chronological), emit T12-reversion calls then score matured ones.
/// If `from` is None, starts from the earliest monthly_actuals period + 12 months
/// (so the first emitted period has a full trailing window). Returns (periods_emitted, calls_scored).
pub async fn backfill_t12_reversion(
    pool: &SqlitePool,
    property: &str,
    through: &str,
    from: Option<&str>,
) -> Result<(usize, usize)> {
    let property_id = crate::db::require_property_by_name(pool, property)
        .await?
        .id;
    let start = match from {
        Some(f) => f.to_string(),
        None => {
            match crate::db::earliest_monthly_period(pool, &property_id).await? {
                Some(e) => {
                    let mut p = e;
                    for _ in 0..12 {
                        p = next_period(&p)?;
                    }
                    p
                } // earliest + 12 months
                None => return Ok((0, 0)),
            }
        }
    };
    // Build chronological period list start..=through.
    let mut periods = Vec::new();
    let mut p = start.clone();
    while p.as_str() <= through {
        periods.push(p.clone());
        p = next_period(&p)?;
        if periods.len() > 600 {
            break;
        } // safety
    }
    let mut emitted_periods = 0usize;
    for period in &periods {
        match emit_t12_reversion_calls(pool, &property_id, period).await {
            Ok(_) => emitted_periods += 1,
            Err(e) => tracing::warn!(period = %period, error = %e, "t12 backfill: emit skipped"),
        }
    }
    let mut scored = 0usize;
    for period in &periods {
        scored += score_due_calls(pool, period).await?;
    }
    Ok((emitted_periods, scored))
}

/// Per-category aggregate of scored calls (reversion-report output row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryStat {
    pub category: String,
    pub n: usize,
    pub mean_score: f64,
    pub pct_strong: f64,
}

/// Aggregate scored calls of `call_type` by NOI category (account_code -> gl_actuals.category).
/// `property` optional (name); None = all properties. Accounts with no known category bucket as "Unmapped".
pub async fn reversion_report(
    pool: &SqlitePool,
    call_type: &str,
    property: Option<&str>,
) -> Result<Vec<CategoryStat>> {
    let property_id = match property {
        Some(p) => Some(crate::db::require_property_by_name(pool, p).await?.id),
        None => None,
    };
    let calls = crate::db::scored_calls_of_type(pool, call_type, property_id.as_deref()).await?;
    // category -> (sum_score, n, n_strong)
    use std::collections::BTreeMap;
    let mut agg: BTreeMap<String, (f64, usize, usize)> = BTreeMap::new();
    for c in &calls {
        let payload: serde_json::Value =
            serde_json::from_str(&c.payload_json).unwrap_or(serde_json::Value::Null);
        let code = payload["account_code"].as_str().unwrap_or("");
        let cat = crate::db::category_for_account(pool, &c.property_id, code)
            .await?
            .unwrap_or_else(|| "Unmapped".to_string());
        let score = c.score.unwrap_or(0.0);
        let e = agg.entry(cat).or_insert((0.0, 0, 0));
        e.0 += score;
        e.1 += 1;
        e.2 += usize::from(score > 0.5);
    }
    let mut out: Vec<CategoryStat> = agg
        .into_iter()
        .map(|(category, (s, n, strong))| CategoryStat {
            category,
            n,
            mean_score: if n > 0 { s / n as f64 } else { 0.0 },
            pct_strong: if n > 0 { strong as f64 / n as f64 } else { 0.0 },
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.n));
    Ok(out)
}

/// Which statistics a scored call may enter. Only `value` outcomes feed the batting
/// average / calibration / prompt. `compliance` (e.g. closeness-to-recommendation) is
/// informational only.
// DEFERRED (K1): renewal_rec is reclassed to compliance, but the replacement *value* renewal
// scorer (retention >= N months + realized collected rent vs baseline) is not yet implemented.
// Until it is, renewals contribute no value signal to the flywheel.
pub fn scorer_value_class(call_type: &str) -> &'static str {
    match call_type {
        "renewal_rec" => "compliance", // measures "actual roll rent ≈ recommended" — the offer sets it
        _ => "value",
    }
}

/// Human resolution of a free-form decision call. Writes the operator's qualitative
/// outcome ONLY — never the `score` column that feeds TrackRecord/calibration/prompt.
pub async fn resolve_human(
    pool: &SqlitePool,
    call_id: &str,
    operator_outcome: &str,
    resolved_by: &str,
) -> Result<()> {
    // Firewall: only calls with outcome_mode='human' may be human-resolved.
    // Resolving an engine/auto call would plant a NULL score and silently
    // corrupt the track record.
    let outcome_mode: Option<String> =
        sqlx::query_scalar("SELECT outcome_mode FROM calls WHERE id = ?")
            .bind(call_id)
            .fetch_one(pool)
            .await?;
    if outcome_mode.as_deref() != Some("human") {
        anyhow::bail!(
            "resolve_human: call {call_id} has outcome_mode={:?}; only outcome_mode='human' calls may be human-resolved",
            outcome_mode
        );
    }
    let made_by: Option<String> =
        sqlx::query_scalar("SELECT source_surface FROM calls WHERE id = ?")
            .bind(call_id)
            .fetch_one(pool)
            .await?;
    let self_graded = made_by.as_deref() == Some(resolved_by);
    sqlx::query(
        "UPDATE calls SET status='scored', operator_outcome=?, resolved_by=?, self_graded=?, scored_at=?, updated_at=? WHERE id=?",
    )
    .bind(operator_outcome)
    .bind(resolved_by)
    .bind(self_graded)
    .bind(crate::db::now_iso())
    .bind(crate::db::now_iso())
    .bind(call_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Complete the K2 fix: mark open delinquency_risk predictions as confounded when the
/// resident was the subject of an intervention decision in the same window.
pub async fn mark_treated_confounded(
    pool: &SqlitePool,
    property_id: &str,
    resident_code: &str,
    window_period: &str,
) -> Result<u64> {
    let res = sqlx::query(
        "UPDATE calls SET confounded=1, updated_at=? WHERE property_id=? AND call_type='delinquency_risk' AND status='open' AND mature_by=? AND json_extract(payload_json,'$.resident_code')=?",
    )
    .bind(crate::db::now_iso())
    .bind(property_id)
    .bind(window_period)
    .bind(resident_code)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

fn col(headers: &csv::StringRecord, name: &str) -> Option<usize> {
    headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
}

fn field(rec: &csv::StringRecord, idx: Option<usize>) -> Option<&str> {
    idx.and_then(|i| rec.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Import per-unit recommendations from a BDDRE/RPCOE engine CSV into the ledger as
/// `delinquency_risk` or `renewal_rec` calls. Idempotent: if calls already exist for
/// (property, period, call_type) nothing is imported (re-runs never double-count).
pub async fn import_calls_from_csv(
    pool: &SqlitePool,
    call_type: &str,
    property: &str,
    period: &str,
    csv_path: &std::path::Path,
) -> Result<usize> {
    if call_type != "delinquency_risk" && call_type != "renewal_rec" {
        return Err(anyhow!(
            "unsupported call_type for import: {call_type} (expected delinquency_risk or renewal_rec)"
        ));
    }
    let property_id = crate::db::require_property_by_name(pool, property)
        .await?
        .id;
    if crate::db::calls_exist_for(pool, &property_id, period, call_type).await? {
        return Ok(0);
    }
    let mature_by = next_period(period)?;
    let mut reader = crate::parse::csv_reader_from_path(csv_path)?;
    let headers = reader.headers()?.clone();
    let mut imported = 0usize;
    for rec in reader.records() {
        let rec = rec?;
        let (confidence, payload) = match call_type {
            "delinquency_risk" => {
                let Some(rc) = field(&rec, col(&headers, "resident_code")) else {
                    continue;
                };
                let score = field(&rec, col(&headers, "predictive_risk_score"))
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                if score <= 0.0 {
                    continue;
                }
                let prob = (score / 100.0).clamp(0.0, 1.0);
                (
                    prob,
                    serde_json::json!({ "resident_code": rc, "probability": prob, "window_days": 45 }),
                )
            }
            _ /* renewal_rec */ => {
                let Some(unit) = field(&rec, col(&headers, "unit")) else {
                    continue;
                };
                let Some(rc) = field(&rec, col(&headers, "resident_code")) else {
                    continue;
                };
                let Some(rent) = field(&rec, col(&headers, "recommended_new_rent"))
                    .and_then(|s| s.parse::<f64>().ok())
                else {
                    continue;
                };
                let conf = match field(&rec, col(&headers, "confidence"))
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "high" => 0.9,
                    "medium" => 0.6,
                    "low" => 0.4,
                    _ => 0.6,
                };
                (
                    conf,
                    serde_json::json!({ "unit_label": unit, "resident_code": rc, "recommended_rent": rent }),
                )
            }
        };
        let call_id = crate::db::insert_call(
            pool,
            &property_id,
            period,
            call_type,
            &mature_by,
            Some(confidence),
            &payload.to_string(),
            None,
        )
        .await?;
        // Tag the value class immediately so fetch_scored_calls can gate correctly.
        let vc = scorer_value_class(call_type);
        sqlx::query("UPDATE calls SET value_class=? WHERE id=?")
            .bind(vc)
            .bind(&call_id)
            .execute(pool)
            .await?;
        imported += 1;
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_period_increments_month_and_year() {
        assert_eq!(next_period("2026-05").unwrap(), "2026-06");
        assert_eq!(next_period("2026-12").unwrap(), "2027-01");
    }

    #[test]
    fn prev_period_decrements_month_and_year() {
        assert_eq!(prev_period("2026-06").unwrap(), "2026-05");
        assert_eq!(prev_period("2026-01").unwrap(), "2025-12");
    }

    #[test]
    fn track_record_computes_batting_average_and_calibration() {
        let calls = vec![
            scored_call(1.0, Some(0.8)),
            scored_call(0.0, Some(0.8)),
            scored_call(1.0, Some(0.8)),
        ];
        let tr = TrackRecord::from_scored("p1", "noi_diagnosis", &calls);
        assert_eq!(tr.n, 3);
        assert!((tr.batting_avg - 0.6667).abs() < 0.001);
        // mean confidence 0.8 vs realized 0.667 -> overconfident by ~0.133
        assert!((tr.calibration_gap - 0.1333).abs() < 0.001);
    }

    #[tokio::test]
    async fn noi_scorer_hits_when_overage_normalizes() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        let period_id = crate::db::upsert_period(&pool, "2026-06").await.unwrap();
        sqlx::query("INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) VALUES (?, ?, ?, '5120', 'R&M', 'Repairs & Maintenance', 9000.0, 'f', 1, 't')")
            .bind(crate::db::new_id()).bind(&pid).bind(&period_id)
            .execute(&pool).await.unwrap();
        let call = Call {
            id: "c1".into(), property_id: pid, origin_period: "2026-05".into(),
            call_type: "noi_diagnosis".into(), status: "open".into(), made_at: "t".into(),
            mature_by: "2026-06".into(), confidence: Some(0.4),
            payload_json: r#"{"account_code":"5120","category":"Repairs & Maintenance","baseline_actual":18000.0,"expected_direction":"normalize"}"#.into(),
            outcome_json: None, score: None, outcome_summary: None, scored_at: None,
            source_task_run_id: None, confounded: false,
            decision_kind: None, entities_json: None, outcome_mode: None, value_class: None,
            acted_on: false, accepted_recall: None, source_surface: None, context_json: None,
            operator_outcome: None, resolved_by: None, self_graded: false, regime_tag: None,
            created_at: "t".into(), updated_at: "t".into(),
        };
        let scored = NoiDiagnosisScorer
            .score(&pool, &call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scored.score, 1.0);
    }

    fn scored_call(score: f64, confidence: Option<f64>) -> Call {
        Call {
            id: "x".into(),
            property_id: "p1".into(),
            origin_period: "2026-05".into(),
            call_type: "noi_diagnosis".into(),
            status: "scored".into(),
            made_at: "t".into(),
            mature_by: "2026-06".into(),
            confidence,
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

    #[test]
    fn from_scored_excludes_confounded_calls() {
        // A confounded miss (outcome changed by an intervention) must not drag the batting
        // average down — otherwise the harness learns to stop flagging risks it mitigates (K2).
        let mut confounded = scored_call(0.0, Some(0.8));
        confounded.confounded = true;
        let calls = vec![
            scored_call(1.0, Some(0.8)),
            scored_call(0.0, Some(0.8)),
            confounded,
        ];
        let tr = TrackRecord::from_scored("p1", "delinquency_risk", &calls);
        // Only the two non-confounded calls count: n = 2, batting = 0.5 (not 3 / 0.333).
        assert_eq!(tr.n, 2);
        assert!((tr.batting_avg - 0.5).abs() < 0.001);
    }

    #[tokio::test]
    async fn decision_call_columns_round_trip() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, decision_kind, outcome_mode, value_class, acted_on, created_at, updated_at) \
             VALUES (?, ?, '2026-05', 'decision', 'open', 't', '2026-06', '{}', 'capex', 'human', 'value', 1, 't', 't')",
        ).bind(crate::db::new_id()).bind(&pid).execute(&pool).await.unwrap();
        let rows: Vec<Call> =
            sqlx::query_as::<_, Call>("SELECT * FROM calls WHERE call_type = 'decision'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision_kind.as_deref(), Some("capex"));
        assert!(rows[0].acted_on);
    }

    #[test]
    fn renewal_rec_is_compliance_not_value() {
        assert_eq!(scorer_value_class("renewal_rec"), "compliance");
        assert_eq!(scorer_value_class("noi_diagnosis"), "value");
        assert_eq!(scorer_value_class("delinquency_risk"), "value");
    }

    #[tokio::test]
    async fn human_resolution_never_enters_batting_average() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        // a human-resolved decision call
        sqlx::query("INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, outcome_mode, value_class, created_at, updated_at) VALUES ('h1', ?, '2026-05', 'decision', 'open', 't', '2026-06', '{}', 'human', 'value', 't', 't')")
            .bind(&pid).execute(&pool).await.unwrap();
        resolve_human(&pool, "h1", "worked", "operator")
            .await
            .unwrap();
        let row: Call = sqlx::query_as("SELECT * FROM calls WHERE id='h1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.status, "scored");
        assert!(row.score.is_none()); // firewall: no score column written
        assert_eq!(row.operator_outcome.as_deref(), Some("worked"));
    }

    #[tokio::test]
    async fn compliance_calls_excluded_from_scored_feed() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        for (id, vc) in [("v1", "value"), ("c1", "compliance")] {
            sqlx::query("INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, score, value_class, scored_at, created_at, updated_at) VALUES (?, ?, '2026-05', 'renewal_rec', 'scored', 't', '2026-06', '{}', 1.0, ?, 't', 't', 't')")
                .bind(id).bind(&pid).bind(vc).execute(&pool).await.unwrap();
        }
        let scored = crate::db::fetch_scored_calls(&pool, &pid, "renewal_rec", 20)
            .await
            .unwrap();
        assert_eq!(scored.len(), 1); // only the value-class row
    }

    #[tokio::test]
    async fn null_value_class_scored_rows_stay_in_feed() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        // Insert a scored row with no value_class (NULL) — legacy engine calls
        sqlx::query("INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, score, scored_at, created_at, updated_at) VALUES ('n1', ?, '2026-05', 'noi_diagnosis', 'scored', 't', '2026-06', '{}', 1.0, 't', 't', 't')")
            .bind(&pid).execute(&pool).await.unwrap();
        let scored = crate::db::fetch_scored_calls(&pool, &pid, "noi_diagnosis", 20)
            .await
            .unwrap();
        assert_eq!(scored.len(), 1); // NULL value_class stays in the value feed
    }

    #[tokio::test]
    async fn mark_treated_confounded_only_touches_open_calls() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        for (id, status) in [("open1", "open"), ("scored1", "scored")] {
            sqlx::query(
                "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, created_at, updated_at) \
                 VALUES (?, ?, '2026-05', 'delinquency_risk', ?, 't', '2026-06', '{\"resident_code\":\"T-1\"}', 't', 't')",
            )
            .bind(id)
            .bind(&pid)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }
        let n = mark_treated_confounded(&pool, &pid, "T-1", "2026-06")
            .await
            .unwrap();
        assert_eq!(n, 1); // only the open call is confounded
        let open_conf: bool = sqlx::query_scalar("SELECT confounded FROM calls WHERE id='open1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let scored_conf: bool =
            sqlx::query_scalar("SELECT confounded FROM calls WHERE id='scored1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(open_conf, "open call must be confounded");
        assert!(!scored_conf, "already-scored call must NOT be confounded");
    }

    /// Finding 1: scoring a compliance-class renewal_rec call must NOT write a
    /// track_record memory. Before the fix, score_due_calls always called upsert_memory,
    /// producing a misleading "renewal_rec: 0% (0 scored)" headline in the Ask prompt.
    #[tokio::test]
    async fn compliance_renewal_rec_score_does_not_write_track_record_memory() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();

        // Insert a unit_lease row so the RenewalRecScorer freshness gate passes.
        sqlx::query(
            "INSERT INTO unit_leases (id, property_id, as_of_date, unit_label, resident_code, \
             resident_name, market_rent, charge_rent, source_file, source_row, created_at) \
             VALUES (?, ?, '2026-06-30', 'C101', 'R1', 'Tenant A', 1500.0, 1450.0, 'f', 1, 't')",
        )
        .bind(crate::db::new_id())
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();

        // Insert an open renewal_rec call with compliance value_class.
        let payload = r#"{"unit_label":"C101","resident_code":"R1","recommended_rent":1500.0}"#;
        sqlx::query(
            "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, \
             mature_by, payload_json, value_class, created_at, updated_at) \
             VALUES ('rr1', ?, '2026-05', 'renewal_rec', 'open', 't', '2026-06', ?, 'compliance', 't', 't')",
        )
        .bind(&pid)
        .bind(payload)
        .execute(&pool)
        .await
        .unwrap();

        // Score the due call for period 2026-06.
        let n = score_due_calls(&pool, "2026-06").await.unwrap();
        assert_eq!(n, 1, "one renewal_rec call should have been scored");

        // The fix: NO track_record memory may exist for renewal_rec.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM memories WHERE memory_type='track_record' AND scope='renewal_rec'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            count, 0,
            "compliance renewal_rec must not write a track_record memory"
        );
    }

    /// Finding 2a: resolve_human must refuse to resolve a call whose outcome_mode is
    /// not 'human', so an operator cannot accidentally plant a NULL score in the
    /// track record.
    #[tokio::test]
    async fn resolve_human_rejects_non_human_outcome_mode() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        // An open noi_diagnosis call — outcome_mode is NULL (engine/auto).
        sqlx::query(
            "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, \
             mature_by, payload_json, created_at, updated_at) \
             VALUES ('nd1', ?, '2026-05', 'noi_diagnosis', 'open', 't', '2026-06', '{}', 't', 't')",
        )
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();

        let result = resolve_human(&pool, "nd1", "worked", "operator").await;
        assert!(
            result.is_err(),
            "resolve_human must return Err for non-human outcome_mode"
        );

        // Status must be unchanged — still 'open'.
        let status: String = sqlx::query_scalar("SELECT status FROM calls WHERE id='nd1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            status, "open",
            "call status must remain open after rejected resolve_human"
        );
    }
}
