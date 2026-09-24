//! Universal decision capture for the flywheel (A). Records a decision as a `calls` row
//! from any surface, with provenance, integrity flags, and a per-kind maturation horizon.
use anyhow::Result;
use sqlx::SqlitePool;

/// Maturation horizon per decision kind.
///
/// Kinds with no credible short-horizon outcome are `CaptureOnly` (surfaced in
/// recall, never scored into the batting average). `NextPeriod` means the
/// outcome is visible after one monthly close; `Periods(n)` gives the number
/// of periods to advance.
pub enum Horizon {
    NextPeriod,
    Periods(u32),
    CaptureOnly,
}

/// Per-kind maturation horizon lookup.
pub fn horizon_for(decision_kind: &str) -> Horizon {
    match decision_kind {
        "renewal_override" | "noi_action" | "concession" => Horizon::NextPeriod,
        "make_ready_scope" => Horizon::Periods(2),
        "vendor_change" => Horizon::Periods(3),
        "capex" => Horizon::Periods(6),
        "hold_sell" => Horizon::CaptureOnly,
        _ => Horizon::CaptureOnly,
    }
}

/// Request payload for [`record`].
pub struct CaptureReq {
    pub property_id: String,
    pub origin_period: String,
    pub decision_kind: String,
    pub entities_json: String,
    pub outcome_mode: String,
    pub value_class: String,
    pub confidence: Option<f64>,
    pub acted_on: bool,
    pub accepted_recall: Option<bool>,
    pub source_surface: String,
    pub context_json: Option<String>,
    pub mature_by: Option<String>,
}

/// Record a decision into the `calls` table and return its generated id.
///
/// Validates `outcome_mode` (must be `auto`, `metric_bound`, or `human`) and
/// `value_class` (must be `value` or `compliance`). Derives `status` and
/// `mature_by` from the per-kind [`Horizon`] unless `req.mature_by` overrides.
pub async fn record(pool: &SqlitePool, req: CaptureReq) -> Result<String> {
    if !matches!(req.outcome_mode.as_str(), "auto" | "metric_bound" | "human") {
        anyhow::bail!(
            "outcome_mode must be one of auto|metric_bound|human (got {:?})",
            req.outcome_mode
        );
    }
    if !matches!(req.value_class.as_str(), "value" | "compliance") {
        anyhow::bail!("value_class must be value|compliance");
    }

    let (status, mature_by) = match (horizon_for(&req.decision_kind), req.mature_by.clone()) {
        (_, Some(m)) => ("open".to_string(), m),
        (Horizon::CaptureOnly, None) => ("capture_only".to_string(), req.origin_period.clone()),
        (Horizon::NextPeriod, None) => (
            "open".to_string(),
            crate::calls::next_period(&req.origin_period)?,
        ),
        (Horizon::Periods(n), None) => ("open".to_string(), add_periods(&req.origin_period, n)?),
    };

    let id = crate::db::new_id();
    let now = crate::db::now_iso();
    crate::db::insert_decision_call(pool, &id, &req, &status, &mature_by, &now).await?;
    Ok(id)
}

/// Advance a period label by `n` monthly steps.
fn add_periods(period: &str, n: u32) -> Result<String> {
    let mut p = period.to_string();
    for _ in 0..n {
        p = crate::calls::next_period(&p)?;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_is_per_kind() {
        assert!(matches!(
            horizon_for("renewal_override"),
            Horizon::NextPeriod
        ));
        assert!(matches!(horizon_for("capex"), Horizon::Periods(_)));
        assert!(matches!(horizon_for("hold_sell"), Horizon::CaptureOnly));
        assert!(matches!(horizon_for("noi_action"), Horizon::NextPeriod));
    }

    #[tokio::test]
    async fn record_rejects_missing_outcome_mode() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        let req = CaptureReq {
            property_id: pid,
            origin_period: "2026-05".into(),
            decision_kind: "capex".into(),
            entities_json: "[]".into(),
            outcome_mode: "".into(),
            value_class: "value".into(),
            confidence: None,
            acted_on: true,
            accepted_recall: None,
            source_surface: "ask".into(),
            context_json: None,
            mature_by: None,
        };
        assert!(record(&pool, req).await.is_err());
    }

    #[tokio::test]
    async fn record_honors_mature_by_override() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        // capex defaults to Periods(6) -> 2026-11; an override must win and set status open.
        let req = CaptureReq {
            property_id: pid.clone(),
            origin_period: "2026-05".into(),
            decision_kind: "capex".into(),
            entities_json: "[]".into(),
            outcome_mode: "metric_bound".into(),
            value_class: "value".into(),
            confidence: None,
            acted_on: true,
            accepted_recall: None,
            source_surface: "ask".into(),
            context_json: None,
            mature_by: Some("2026-08".into()),
        };
        let id = record(&pool, req).await.unwrap();
        let row: crate::models::Call = sqlx::query_as("SELECT * FROM calls WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.mature_by, "2026-08"); // override won, not the +6 default 2026-11
        assert_eq!(row.status, "open");
    }

    #[tokio::test]
    async fn record_persists_and_sets_capture_only_status() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::init_database(&pool).await.unwrap();
        let pid = crate::db::upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        let req = CaptureReq {
            property_id: pid.clone(),
            origin_period: "2026-05".into(),
            decision_kind: "hold_sell".into(),
            entities_json: "[]".into(),
            outcome_mode: "human".into(),
            value_class: "value".into(),
            confidence: Some(0.5),
            acted_on: true,
            accepted_recall: Some(true),
            source_surface: "ask".into(),
            context_json: Some("{\"q\":\"sell?\"}".into()),
            mature_by: None,
        };
        let id = record(&pool, req).await.unwrap();
        let row: crate::models::Call = sqlx::query_as("SELECT * FROM calls WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.call_type, "decision");
        assert_eq!(row.decision_kind.as_deref(), Some("hold_sell"));
        // hold_sell is CaptureOnly -> no mature_by horizon, status stays "capture_only".
        assert_eq!(row.status, "capture_only");
    }
}
