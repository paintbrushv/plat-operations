use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use uuid::Uuid;

use crate::models::*;

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("invalid database url: {database_url}"))?
        .busy_timeout(Duration::from_secs(5))
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to connect to database: {database_url}"))
}

pub async fn init_database(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL)",
    )
    .execute(pool)
    .await?;
    for (version, migration) in [
        ("001", include_str!("../migrations/001_initial_schema.sql")),
        (
            "002",
            include_str!("../migrations/002_account_mappings.sql"),
        ),
        (
            "003",
            include_str!("../migrations/003_collection_snapshots.sql"),
        ),
        ("004", include_str!("../migrations/004_gl_transactions.sql")),
        ("005", include_str!("../migrations/005_calls.sql")),
        (
            "006",
            include_str!("../migrations/006_unit_receivables.sql"),
        ),
        ("007", include_str!("../migrations/007_unit_leases.sql")),
        ("008", include_str!("../migrations/008_monthly_actuals.sql")),
        (
            "009",
            include_str!("../migrations/009_receivables_days_late.sql"),
        ),
        (
            "010",
            include_str!("../migrations/010_calls_confounded.sql"),
        ),
        (
            "011",
            include_str!("../migrations/011_decision_capture.sql"),
        ),
        ("012", include_str!("../migrations/012_turn_costs.sql")),
        ("013", include_str!("../migrations/013_unit_pnl.sql")),
        (
            "014",
            include_str!("../migrations/014_synthetic_pms_handoff.sql"),
        ),
    ] {
        let applied = sqlx::query("SELECT version FROM schema_migrations WHERE version = ?")
            .bind(version)
            .fetch_optional(pool)
            .await?
            .is_some();
        if applied {
            continue;
        }
        for statement in migration.split(';') {
            let statement = statement.trim();
            if !statement.is_empty() {
                sqlx::query(statement).execute(pool).await?;
            }
        }
        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)")
            .bind(version)
            .bind(now_iso())
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn create_task_run(
    pool: &SqlitePool,
    task_type: &str,
    user_prompt: &str,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO task_runs (id, task_type, user_prompt, status, started_at) VALUES (?, ?, ?, 'running', ?)",
    )
    .bind(&id)
    .bind(task_type)
    .bind(user_prompt)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn complete_task_run(
    pool: &SqlitePool,
    task_run_id: &str,
    status: &str,
    confidence_score: Option<f64>,
    summary: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE task_runs SET status = ?, confidence_score = ?, summary = ?, completed_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(confidence_score)
    .bind(summary)
    .bind(now_iso())
    .bind(task_run_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_property_by_name(pool: &SqlitePool, name: &str) -> Result<Option<Property>> {
    sqlx::query_as::<_, Property>("SELECT * FROM properties WHERE lower(name) = lower(?)")
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(Into::into)
}

pub async fn require_property_by_name(pool: &SqlitePool, name: &str) -> Result<Property> {
    find_property_by_name(pool, name)
        .await?
        .ok_or_else(|| anyhow!("property not found: {name}"))
}

pub async fn upsert_property(
    pool: &SqlitePool,
    name: &str,
    market: &str,
    unit_count: i64,
    owner_entity: &str,
    property_manager: &str,
) -> Result<String> {
    if let Some(property) = find_property_by_name(pool, name).await? {
        sqlx::query(
            "UPDATE properties SET market = ?, unit_count = ?, owner_entity = ?, property_manager = ? WHERE id = ?",
        )
        .bind(market)
        .bind(unit_count)
        .bind(owner_entity)
        .bind(property_manager)
        .bind(&property.id)
        .execute(pool)
        .await?;
        return Ok(property.id);
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(market)
    .bind(unit_count)
    .bind(owner_entity)
    .bind(property_manager)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn upsert_period(pool: &SqlitePool, label: &str) -> Result<String> {
    // Normalize before storing so "2026-6" and "2026-06" resolve to one period row.
    let normalized = normalize_period_label(label)?;
    if let Some(period) = sqlx::query_as::<_, Period>("SELECT * FROM periods WHERE label = ?")
        .bind(&normalized)
        .fetch_optional(pool)
        .await?
    {
        return Ok(period.id);
    }
    let (year, month) = parse_period_label(&normalized)?;
    let id = new_id();
    sqlx::query("INSERT INTO periods (id, year, month, label) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(year)
        .bind(month)
        .bind(&normalized)
        .execute(pool)
        .await?;
    Ok(id)
}

pub fn parse_period_label(label: &str) -> Result<(i64, i64)> {
    let mut parts = label.trim().split('-');
    let year = parts
        .next()
        .ok_or_else(|| anyhow!("period must be YYYY-MM"))?
        .parse::<i64>()?;
    let month = parts
        .next()
        .ok_or_else(|| anyhow!("period must be YYYY-MM"))?
        .parse::<i64>()?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return Err(anyhow!("period must be YYYY-MM with month 01-12"));
    }
    if !(1990..=2100).contains(&year) {
        return Err(anyhow!(
            "period year {year} is outside the supported range 1990-2100"
        ));
    }
    Ok((year, month))
}

pub fn normalize_period_label(label: &str) -> Result<String> {
    let (year, month) = parse_period_label(label)?;
    Ok(format!("{year:04}-{month:02}"))
}

pub async fn period_by_label(pool: &SqlitePool, label: &str) -> Result<Option<Period>> {
    sqlx::query_as::<_, Period>("SELECT * FROM periods WHERE label = ?")
        .bind(label)
        .fetch_optional(pool)
        .await
        .map_err(Into::into)
}

pub async fn insert_gap(
    pool: &SqlitePool,
    task_run_id: &str,
    gap_type: &str,
    severity: &str,
    description: &str,
    why_it_matters: &str,
    proposed_resolution: &str,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO gaps (id, task_run_id, gap_type, severity, description, why_it_matters, proposed_resolution, status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?)",
    )
    .bind(&id)
    .bind(task_run_id)
    .bind(gap_type)
    .bind(severity)
    .bind(description)
    .bind(why_it_matters)
    .bind(proposed_resolution)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

#[derive(Debug, Clone)]
pub struct NewEvidence<'a> {
    pub task_run_id: &'a str,
    pub source_type: &'a str,
    pub source_table: &'a str,
    pub source_id: Option<&'a str>,
    pub source_file: Option<&'a str>,
    pub source_row: Option<i64>,
    pub claim: &'a str,
}

pub async fn insert_evidence(pool: &SqlitePool, evidence: NewEvidence<'_>) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO evidence_items (id, task_run_id, source_type, source_table, source_id, source_file, source_row, claim, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(evidence.task_run_id)
    .bind(evidence.source_type)
    .bind(evidence.source_table)
    .bind(evidence.source_id)
    .bind(evidence.source_file)
    .bind(evidence.source_row)
    .bind(evidence.claim)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn insert_question(
    pool: &SqlitePool,
    task_run_id: &str,
    question: &str,
    reason: &str,
    priority: i64,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO operator_questions (id, task_run_id, question, reason, priority, status, created_at) VALUES (?, ?, ?, ?, ?, 'open', ?)",
    )
    .bind(&id)
    .bind(task_run_id)
    .bind(question)
    .bind(reason)
    .bind(priority)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn upsert_memory(
    pool: &SqlitePool,
    memory_type: &str,
    scope: &str,
    key: &str,
    value: &str,
    confidence_score: f64,
    source_task_run_id: Option<&str>,
) -> Result<String> {
    let existing_id =
        sqlx::query("SELECT id FROM memories WHERE memory_type = ? AND scope = ? AND key = ?")
            .bind(memory_type)
            .bind(scope)
            .bind(key)
            .fetch_optional(pool)
            .await?
            .map(|row| row.get::<String, _>("id"));
    if let Some(id) = existing_id {
        sqlx::query(
            "UPDATE memories SET value = ?, confidence_score = ?, source_task_run_id = ?, updated_at = ? WHERE id = ?",
        )
        .bind(value)
        .bind(confidence_score)
        .bind(source_task_run_id)
        .bind(now_iso())
        .bind(&id)
        .execute(pool)
        .await?;
        return Ok(id);
    }
    let id = new_id();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO memories (id, memory_type, scope, key, value, confidence_score, source_task_run_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(memory_type)
    .bind(scope)
    .bind(key)
    .bind(value)
    .bind(confidence_score)
    .bind(source_task_run_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

// ── Calls (call ledger) ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn insert_call(
    pool: &SqlitePool,
    property_id: &str,
    origin_period: &str,
    call_type: &str,
    mature_by: &str,
    confidence: Option<f64>,
    payload_json: &str,
    source_task_run_id: Option<&str>,
) -> Result<String> {
    let id = new_id();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, confidence, payload_json, source_task_run_id, created_at, updated_at) VALUES (?, ?, ?, ?, 'open', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(origin_period)
    .bind(call_type)
    .bind(&now)
    .bind(mature_by)
    .bind(confidence)
    .bind(payload_json)
    .bind(source_task_run_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn insert_decision_call(
    pool: &SqlitePool,
    id: &str,
    req: &crate::capture::CaptureReq,
    status: &str,
    mature_by: &str,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO calls \
         (id, property_id, origin_period, call_type, status, made_at, mature_by, confidence, \
          payload_json, decision_kind, entities_json, outcome_mode, value_class, acted_on, \
          accepted_recall, source_surface, context_json, created_at, updated_at) \
         VALUES (?, ?, ?, 'decision', ?, ?, ?, ?, '{}', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(&req.property_id)
    .bind(&req.origin_period)
    .bind(status)
    .bind(now)
    .bind(mature_by)
    .bind(req.confidence)
    .bind(&req.decision_kind)
    .bind(&req.entities_json)
    .bind(&req.outcome_mode)
    .bind(&req.value_class)
    .bind(req.acted_on)
    .bind(req.accepted_recall)
    .bind(&req.source_surface)
    .bind(&req.context_json)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_calls(pool: &SqlitePool) -> Result<Vec<Call>> {
    sqlx::query_as::<_, Call>("SELECT * FROM calls ORDER BY made_at DESC")
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

/// Returns `true` if any call exists for the given property+origin_period+call_type combination.
/// Used as an idempotency guard before bulk-emitting auto-generated calls.
pub async fn calls_exist_for(
    pool: &SqlitePool,
    property_id: &str,
    origin_period: &str,
    call_type: &str,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM calls WHERE property_id = ? AND origin_period = ? AND call_type = ?",
    )
    .bind(property_id)
    .bind(origin_period)
    .bind(call_type)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0 > 0).unwrap_or(false))
}

pub async fn calls_due_for_scoring(pool: &SqlitePool, period: &str) -> Result<Vec<Call>> {
    sqlx::query_as::<_, Call>(
        "SELECT * FROM calls WHERE status = 'open' AND mature_by <= ? ORDER BY mature_by ASC",
    )
    .bind(period)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn mark_call_scored(
    pool: &SqlitePool,
    id: &str,
    score: f64,
    outcome_json: &str,
    outcome_summary: &str,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE calls SET status = 'scored', score = ?, outcome_json = ?, outcome_summary = ?, scored_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(score)
    .bind(outcome_json)
    .bind(outcome_summary)
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fetch_scored_calls(
    pool: &SqlitePool,
    property_id: &str,
    call_type: &str,
    limit: i64,
) -> Result<Vec<Call>> {
    // value_class IS NOT 'compliance' is null-safe: NULL IS NOT 'compliance' → true,
    // so legacy engine calls with value_class = NULL stay in the value feed.
    // Only explicit 'compliance' rows (e.g. renewal_rec closeness) are excluded.
    sqlx::query_as::<_, Call>(
        "SELECT * FROM calls WHERE property_id = ? AND call_type = ? AND status = 'scored' AND confounded = 0 AND value_class IS NOT 'compliance' ORDER BY scored_at DESC LIMIT ?",
    )
    .bind(property_id)
    .bind(call_type)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Read-only helper for the property_model (C) query layer — the single-query baseline.
///
/// Returns scored, non-confounded, value-class calls for ONE `call_type` across ALL
/// properties in a SINGLE query, collapsing the per-property N+1 the rest-of-portfolio
/// baseline previously issued. Uses the SAME locked value-feed predicate as
/// `fetch_scored_calls` (`confounded = 0 AND value_class IS NOT 'compliance'`, null-safe so
/// legacy NULL engine rows stay in — K1/K2). Each returned row carries its `property_id`, so
/// the caller partitions the subject property's calls from the rest-of-portfolio in Rust
/// (leave-one-out) with no further queries. Ordering matches `fetch_scored_calls`
/// (`scored_at DESC`), so filtering the global result per property yields the same
/// per-property ordering the old per-property fetches produced (result-equivalent).
pub async fn fetch_scored_calls_all_properties(
    pool: &SqlitePool,
    call_type: &str,
    limit: i64,
) -> Result<Vec<Call>> {
    sqlx::query_as::<_, Call>(
        "SELECT * FROM calls WHERE call_type = ? AND status = 'scored' AND confounded = 0 AND value_class IS NOT 'compliance' ORDER BY scored_at DESC LIMIT ?",
    )
    .bind(call_type)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Read-only helper for the property_model (C) query layer.
///
/// Returns scored, non-confounded calls for a property across ALL call types,
/// optionally restricted by value_class. With `value_class_filter = None` the
/// null-safe value feed is returned (`value_class IS NOT 'compliance'`, so legacy
/// NULL engine rows stay in — identical predicate to `fetch_scored_calls`, K1).
/// With `Some("compliance")` only explicit compliance rows are returned (used to
/// surface value-vs-compliance drift, which abstains until compliance scorers exist).
pub async fn fetch_property_scored_calls(
    pool: &SqlitePool,
    property_id: &str,
    value_class_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<Call>> {
    let sql = match value_class_filter {
        Some(_) => "SELECT * FROM calls WHERE property_id = ? AND status = 'scored' AND confounded = 0 AND value_class = ? ORDER BY scored_at DESC LIMIT ?",
        None => "SELECT * FROM calls WHERE property_id = ? AND status = 'scored' AND confounded = 0 AND value_class IS NOT 'compliance' ORDER BY scored_at DESC LIMIT ?",
    };
    let mut q = sqlx::query_as::<_, Call>(sql).bind(property_id);
    if let Some(vc) = value_class_filter {
        q = q.bind(vc);
    }
    q.bind(limit).fetch_all(pool).await.map_err(Into::into)
}

pub async fn list_properties(pool: &SqlitePool) -> Result<Vec<Property>> {
    sqlx::query_as::<_, Property>("SELECT * FROM properties ORDER BY name")
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

pub async fn list_task_runs(pool: &SqlitePool) -> Result<Vec<TaskRun>> {
    sqlx::query_as::<_, TaskRun>("SELECT * FROM task_runs ORDER BY started_at DESC")
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

pub async fn list_gaps(pool: &SqlitePool) -> Result<Vec<Gap>> {
    sqlx::query_as::<_, Gap>("SELECT * FROM gaps ORDER BY created_at DESC")
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

pub async fn list_questions(pool: &SqlitePool) -> Result<Vec<OperatorQuestion>> {
    sqlx::query_as::<_, OperatorQuestion>(
        "SELECT * FROM operator_questions ORDER BY status, priority ASC, created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_capabilities(pool: &SqlitePool) -> Result<Vec<Capability>> {
    sqlx::query_as::<_, Capability>(
        "SELECT * FROM capability_backlog ORDER BY priority ASC, created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_memories(pool: &SqlitePool) -> Result<Vec<Memory>> {
    sqlx::query_as::<_, Memory>("SELECT * FROM memories ORDER BY updated_at DESC")
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

pub async fn recent_track_record_memories(pool: &SqlitePool, limit: i64) -> Result<Vec<Memory>> {
    sqlx::query_as::<_, Memory>(
        "SELECT * FROM memories WHERE memory_type = 'track_record' ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Parse the scored-call count from a track-record headline.
///
/// Headline format: `"<call_type>: <pct>% (<N> scored, calibration <±gap>)"`.
/// Returns `N` or `0` if the pattern is absent.
fn parse_scored_n(headline: &str) -> i64 {
    // Find " scored" and scan back to the opening parenthesis or space.
    let Some(scored_pos) = headline.find(" scored") else {
        return 0;
    };
    let before = &headline[..scored_pos];
    let start = before
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |p| p + 1);
    before[start..].parse::<i64>().unwrap_or(0)
}

/// Select track-record memories for prompt injection.
///
/// Returns `(headline, n_eff)` pairs sorted by `n_eff` (parsed scored-call count)
/// descending, capped at `per_type_cap` per call type.
///
/// Storage convention (set by `calls::score_due_calls`):
///   - `scope` = call_type (e.g. `"noi_diagnosis"`)
///   - `key`   = property_id
///
/// When `property_id` is empty the property filter is skipped (portfolio-wide
/// view), which matches the behaviour of the legacy `recent_track_record_memories`
/// call in the system-prompt builder.
pub async fn select_track_record_for_context(
    pool: &SqlitePool,
    property_id: &str,
    call_types: &[&str],
    per_type_cap: i64,
) -> Result<Vec<(String, f64)>> {
    if call_types.is_empty() {
        return Ok(vec![]);
    }

    // Build a dynamic IN-list placeholder string.
    let placeholders = call_types
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");

    let sql = if property_id.is_empty() {
        format!(
            "SELECT scope, value FROM memories \
             WHERE memory_type = 'track_record' AND scope IN ({placeholders})"
        )
    } else {
        format!(
            "SELECT scope, value FROM memories \
             WHERE memory_type = 'track_record' AND scope IN ({placeholders}) AND key = ?"
        )
    };

    let mut query = sqlx::query_as::<_, (String, String)>(&sql);
    for ct in call_types {
        query = query.bind(*ct);
    }
    if !property_id.is_empty() {
        query = query.bind(property_id);
    }

    let rows: Vec<(String, String)> = query.fetch_all(pool).await?;

    // Group by call_type (scope), keep highest-N entries, cap per type.
    let mut by_type: std::collections::HashMap<String, Vec<(String, i64)>> =
        std::collections::HashMap::new();
    for (scope, value) in rows {
        let n = parse_scored_n(&value);
        by_type.entry(scope).or_default().push((value, n));
    }

    let mut result: Vec<(String, i64)> = Vec::new();
    for (_scope, mut entries) in by_type {
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        for (headline, n) in entries.into_iter().take(per_type_cap as usize) {
            result.push((headline, n));
        }
    }

    // Overall sort: highest-N first.
    result.sort_by_key(|e| std::cmp::Reverse(e.1));

    Ok(result.into_iter().map(|(h, n)| (h, n as f64)).collect())
}

#[derive(Debug, Clone)]
pub struct NewAccountMapping<'a> {
    pub source_system: &'a str,
    pub property_scope: &'a str,
    pub account_code: &'a str,
    pub account_name: &'a str,
    pub noi_category: &'a str,
    pub confidence_score: f64,
    pub status: &'a str,
}

pub async fn upsert_account_mapping(
    pool: &SqlitePool,
    mapping: NewAccountMapping<'_>,
) -> Result<String> {
    let existing_id = sqlx::query(
        "SELECT id FROM account_mappings WHERE source_system = ? AND property_scope = ? AND account_code = ?",
    )
    .bind(mapping.source_system)
    .bind(mapping.property_scope)
    .bind(mapping.account_code)
    .fetch_optional(pool)
    .await?
    .map(|row| row.get::<String, _>("id"));

    if let Some(id) = existing_id {
        sqlx::query(
            "UPDATE account_mappings SET account_name = ?, noi_category = ?, confidence_score = ?, status = ?, updated_at = ? WHERE id = ?",
        )
        .bind(mapping.account_name)
        .bind(mapping.noi_category)
        .bind(mapping.confidence_score)
        .bind(mapping.status)
        .bind(now_iso())
        .bind(&id)
        .execute(pool)
        .await?;
        return Ok(id);
    }

    let id = new_id();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO account_mappings (id, source_system, property_scope, account_code, account_name, noi_category, confidence_score, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(mapping.source_system)
    .bind(mapping.property_scope)
    .bind(mapping.account_code)
    .bind(mapping.account_name)
    .bind(mapping.noi_category)
    .bind(mapping.confidence_score)
    .bind(mapping.status)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn find_account_mapping(
    pool: &SqlitePool,
    source_system: &str,
    property_scope: &str,
    account_code: &str,
) -> Result<Option<AccountMapping>> {
    sqlx::query_as::<_, AccountMapping>(
        "SELECT * FROM account_mappings WHERE source_system = ? AND property_scope = ? AND account_code = ?",
    )
    .bind(source_system)
    .bind(property_scope)
    .bind(account_code)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// Mappings still awaiting operator review (anything not yet approved).
pub async fn list_unapproved_mappings(pool: &SqlitePool) -> Result<Vec<AccountMapping>> {
    sqlx::query_as::<_, AccountMapping>(
        "SELECT * FROM account_mappings
         WHERE status != 'approved'
         ORDER BY property_scope, account_code",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_unmapped_accounts(pool: &SqlitePool) -> Result<Vec<UnmappedAccount>> {
    sqlx::query_as::<_, UnmappedAccount>(
        "SELECT account_code, account_name, category, COUNT(*) AS line_count
         FROM (
           SELECT account_code, account_name, category FROM gl_actuals WHERE category = 'Unmapped'
           UNION ALL
           SELECT account_code, account_name, category FROM gl_budgets WHERE category = 'Unmapped'
         )
         GROUP BY account_code, account_name, category
         ORDER BY line_count DESC, account_code",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Count distinct GL accounts (actuals + budgets) active for a property in a
/// period that have NO approved operator review decision.
///
/// "Mapping missing" should mean *unreviewed*, not *excluded*: an account an
/// operator has explicitly reviewed and approved as `Unmapped` (a balance-sheet
/// line, capitalized repair, mortgage interest, etc.) is a finished decision,
/// not a gap. This counts only accounts that lack an approved `account_mappings`
/// row, so a fully reviewed chart returns 0 and the variance confidence score
/// is not penalized for honest, intentional exclusions.
pub async fn count_unreviewed_gl_accounts(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<i64> {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM (
           SELECT DISTINCT g.account_code
           FROM (
             SELECT account_code, property_id, period_id FROM gl_actuals
             UNION ALL
             SELECT account_code, property_id, period_id FROM gl_budgets
           ) g
           JOIN periods pr ON pr.id = g.period_id
           WHERE g.property_id = ?
             AND pr.label = ?
             AND NOT EXISTS (
               SELECT 1 FROM account_mappings m
               WHERE m.account_code = g.account_code
                 AND m.status = 'approved'
             )
         )",
    )
    .bind(property_id)
    .bind(period)
    .fetch_one(pool)
    .await?;
    Ok(count.0)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct DelinquencySummary {
    pub as_of_date: String,
    pub delinquent_amount: f64,
    pub delinquent_units: i64,
    pub prepaid_amount: f64,
    /// High-risk unit count from the latest `collection_snapshots` row (collections
    /// triage signal). `None` if no collection snapshot exists.
    pub high_risk_units: Option<i64>,
    /// As-of date of the `collection_snapshots` row that `high_risk_units` came from.
    /// May differ from `as_of_date` (the delinquency figures' date); callers MUST
    /// surface it so a stale high-risk count is never read as same-dated.
    pub high_risk_as_of: Option<String>,
}

/// Latest delinquency snapshot for a property, by `as_of_date`, plus the latest
/// high-risk unit count from `collection_snapshots` (with its own as-of date).
///
/// Returns `None` when no receivables snapshot exists for the property. Callers
/// MUST treat `None` as "no feed" and say so — never as `$0` delinquency. Note a
/// PRESENT row with `delinquent_amount == 0` is NOT a verified zero: a count-only
/// AR feed loads as `$0` (validator D3), so a reported `$0` must be flagged as
/// unverified rather than asserted (this table carries no aging-bucket provenance).
/// Columns are NOT NULL (migration 001), so no COALESCE/silent-zero is applied.
pub async fn delinquency_summary(
    pool: &SqlitePool,
    property_id: &str,
) -> Result<Option<DelinquencySummary>> {
    sqlx::query_as::<_, DelinquencySummary>(
        "SELECT d.as_of_date AS as_of_date,
                d.delinquent_amount AS delinquent_amount,
                d.delinquent_units  AS delinquent_units,
                d.prepaid_amount    AS prepaid_amount,
                (SELECT c.high_risk_units FROM collection_snapshots c
                 WHERE c.property_id = d.property_id
                 ORDER BY c.as_of_date DESC LIMIT 1) AS high_risk_units,
                (SELECT c.as_of_date FROM collection_snapshots c
                 WHERE c.property_id = d.property_id
                 ORDER BY c.as_of_date DESC LIMIT 1) AS high_risk_as_of
         FROM delinquency_snapshots d
         WHERE d.property_id = ?
         ORDER BY d.as_of_date DESC
         LIMIT 1",
    )
    .bind(property_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct OccupancySnapshot {
    pub as_of_date: String,
    pub occupied_units: i64,
    pub vacant_units: i64,
    pub leased_units: i64,
    pub notice_units: i64,
    pub down_units: i64,
}

/// Latest rent-roll occupancy snapshot for a property, by `as_of_date`.
///
/// Returns `None` when no rent-roll feed exists for the property. Callers MUST
/// treat `None` as "no feed" and say so — never fabricate a 100%/0% occupancy.
/// Columns are NOT NULL (migration 001), so no COALESCE/silent-zero is applied.
pub async fn occupancy_summary(
    pool: &SqlitePool,
    property_id: &str,
) -> Result<Option<OccupancySnapshot>> {
    sqlx::query_as::<_, OccupancySnapshot>(
        "SELECT as_of_date,
                occupied_units,
                vacant_units,
                leased_units,
                notice_units,
                down_units
         FROM rent_roll_snapshots
         WHERE property_id = ?
         ORDER BY as_of_date DESC
         LIMIT 1",
    )
    .bind(property_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct TurnSummary {
    /// Number of turn rows (make-ready EVENTS) in the window — counts every turn,
    /// including ones whose cost is NULL (unpriced). This is the turnover *volume*.
    pub turn_count: i64,
    /// Sum of `turn_cost_total` across the window (SQLite SUM ignores NULL costs).
    pub total_turn_cost: f64,
    /// Average make-ready cost over PRICED turns only — `AVG(turn_cost_total)`,
    /// which ignores NULL-cost rows so an unpriced turn does not dilute it toward
    /// $0. `None` when no priced turn falls in the window (also the divide-by-zero
    /// guard). NOTE the denominators differ on purpose: `turn_count` is all
    /// events, but `avg_cost_per_turn` is over priced turns only.
    pub avg_cost_per_turn: Option<f64>,
    /// Average vacancy days over turns that carry a vacancy_days value
    /// (`AVG(vacancy_days)`, ignoring NULLs). `None` when none do.
    pub avg_vacancy_days: Option<f64>,
    /// Earliest turn_date in the window (provenance for the reported figures).
    pub earliest_turn_date: Option<String>,
    /// Latest turn_date in the window.
    pub latest_turn_date: Option<String>,
}

/// Aggregate turn-cost (turnover) metrics for a property, optionally restricted to
/// turns on/after `since_period` (a YYYY-MM label, compared against the first 7
/// chars of `turn_date`).
///
/// Returns `None` when the property has NO turn rows at all (in or out of window).
/// Callers MUST treat `None` as "no turnover feed" and say so — never fabricate a
/// $0 / 0-turn result. When rows exist but the window excludes them all,
/// `turn_count == 0`, `total_turn_cost == 0`, and `avg_cost_per_turn == None`.
///
/// `turn_count` counts all turn events (the turnover volume); `avg_cost_per_turn`
/// is `AVG(turn_cost_total)` over PRICED turns only (NULL costs ignored), so an
/// unpriced turn never silently drags the average toward $0.
pub async fn turn_summary(
    pool: &SqlitePool,
    property_id: &str,
    since_period: Option<&str>,
) -> Result<Option<TurnSummary>> {
    // Missing-feed guard: distinguish "no turn rows ever" (None) from "rows exist
    // but the window is empty" (Some with turn_count == 0).
    let total_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM turn_costs WHERE property_id = ?")
            .bind(property_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0);
    if total_rows == 0 {
        return Ok(None);
    }

    let since = match since_period {
        Some(p) => Some(normalize_period_label(p)?),
        None => None,
    };

    // substr(turn_date, 1, 7) compares the YYYY-MM prefix against the window. A
    // NULL turn_date is excluded from a since-window (it cannot be placed in time).
    // turn_count = COUNT(*) (all events); avg_cost_per_turn = AVG(turn_cost_total)
    // and avg_vacancy_days = AVG(vacancy_days) — SQLite AVG ignores NULLs, so an
    // unpriced/undated-cost turn neither dilutes the average nor reads as $0/0d.
    // Both AVGs and the SUM return NULL when the window is empty (the natural
    // divide-by-zero guard), surfacing as `None` / 0.0.
    // Column aliases match TurnSummary's field names so the row maps via FromRow.
    // COALESCE(SUM(...), 0.0) keeps total_turn_cost non-NULL (empty window → 0.0);
    // the two AVGs stay nullable (None when the window has no priced/dated rows).
    let summary = sqlx::query_as::<_, TurnSummary>(
        "SELECT COUNT(*) AS turn_count,
                COALESCE(SUM(turn_cost_total), 0.0) AS total_turn_cost,
                AVG(turn_cost_total) AS avg_cost_per_turn,
                AVG(vacancy_days) AS avg_vacancy_days,
                MIN(turn_date) AS earliest_turn_date,
                MAX(turn_date) AS latest_turn_date
         FROM turn_costs
         WHERE property_id = ?
           AND (? IS NULL OR (turn_date IS NOT NULL AND substr(turn_date, 1, 7) >= ?))",
    )
    .bind(property_id)
    .bind(since.as_deref())
    .bind(since.as_deref())
    .fetch_one(pool)
    .await?;

    Ok(Some(summary))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct UnitPnl {
    /// The unit as stored in the source feed (e.g. "C00101"). UNIT-LEVEL ONLY —
    /// this struct never carries a resident name, code, or balance.
    pub unit: String,
    /// The period for the row — a year (e.g. "2025") for the annual feed.
    pub period: Option<String>,
    pub total_income: Option<f64>,
    pub direct_expense: Option<f64>,
    pub allocated_expense: Option<f64>,
    pub noi: Option<f64>,
}

/// Normalize a unit code for robust matching: uppercase, then split the leading
/// alpha prefix from the trailing numeric part and compare the prefix plus the
/// INTEGER value of the digits. This makes "C123", "C0123", "C00123", and
/// "c00123" all resolve to the same canonical key `("C", 123)`, and a purely
/// numeric unit like "1010" to `("", 1010)`. Units that do not fit the
/// `<alpha-prefix><digits>` shape fall back to their uppercased, trimmed form so
/// they still match themselves exactly.
fn normalize_unit_key(unit: &str) -> String {
    let upper = unit.trim().to_ascii_uppercase();
    let digit_start = upper.find(|c: char| c.is_ascii_digit());
    match digit_start {
        Some(start) => {
            let (prefix, digits) = upper.split_at(start);
            // Only treat the tail as a number when it is ALL digits; otherwise the
            // code has interior non-digits (e.g. "1612-A") and we match verbatim.
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                // Parse to drop leading zeros; fall back to the raw form if the
                // digit run overflows u64 (not expected for unit codes).
                match digits.parse::<u64>() {
                    Ok(value) => format!("{prefix}{value}"),
                    Err(_) => upper,
                }
            } else {
                upper
            }
        }
        None => upper,
    }
}

/// Look up ONE unit's P&L for a property. When `period` is given, the row for
/// that exact period is returned; otherwise the latest PRICED period — the
/// greatest period whose `noi` is non-NULL (lexicographic max, correct for YYYY
/// and YYYY-MM labels) — is returned. Selecting the latest priced period (rather
/// than the raw latest) means a NULL-NOI snapshot is never returned as "the
/// answer", and it matches the gold rule the eval (`scripts/ask_eval_run.py`)
/// uses, so tool and gold never diverge. If EVERY matched period has a NULL NOI,
/// the raw latest period is returned so the unit is still surfaced (with a NULL
/// NOI the renderer shows as "n/a") rather than reported as missing.
///
/// Unit matching is normalization-robust: "C123", "C0123", "C00123", and
/// "c00123" all resolve to the same stored unit (see [`normalize_unit_key`]).
///
/// Returns `None` when the unit is not found at the property (the missing-unit
/// guard — callers MUST treat `None` as "unit not found" and say so, never
/// fabricate a $0 NOI). UNIT-LEVEL FINANCIALS ONLY — no resident data is read or
/// returned.
pub async fn unit_pnl(
    pool: &SqlitePool,
    property_id: &str,
    unit: &str,
    period: Option<&str>,
) -> Result<Option<UnitPnl>> {
    // Fetch all rows for the property and resolve the unit in Rust so the match
    // is normalization-robust (SQL alone cannot strip a variable leading-zero
    // pad). The per-property row count is small (one row per unit per year), so
    // this is cheap.
    let rows = sqlx::query_as::<_, UnitPnl>(
        "SELECT unit, period, total_income, direct_expense, allocated_expense, noi
         FROM unit_pnl
         WHERE property_id = ?",
    )
    .bind(property_id)
    .fetch_all(pool)
    .await?;

    let want_key = normalize_unit_key(unit);
    let mut matches: Vec<UnitPnl> = rows
        .into_iter()
        .filter(|r| normalize_unit_key(&r.unit) == want_key)
        .collect();
    if matches.is_empty() {
        return Ok(None);
    }

    match period {
        Some(p) => {
            let p = p.trim();
            Ok(matches.into_iter().find(|r| r.period.as_deref() == Some(p)))
        }
        None => {
            // Latest PRICED period wins: the greatest period whose noi is
            // non-NULL. Period labels are YYYY or YYYY-MM, both lexicographically
            // ordered; a NULL period sorts last (least). This skips a NULL-NOI
            // newest snapshot so it is never returned as the answer, and matches
            // the eval gold rule (latest period WHERE noi IS NOT NULL).
            matches.sort_by(|a, b| a.period.cmp(&b.period));
            if let Some(priced) = matches.iter().rev().find(|r| r.noi.is_some()) {
                return Ok(Some(priced.clone()));
            }
            // Every period for this unit has a NULL NOI: fall back to the raw
            // latest so the unit is still surfaced (NOI renders as "n/a") rather
            // than mis-reported as a missing unit.
            Ok(matches.pop())
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct AccountActivity {
    pub account_code: String,
    pub account_name: String,
    pub txn_count: i64,
    pub total: f64,
}

/// Per-account transaction activity for a property + period. The account
/// name is resolved from the most recent `gl_actuals` row for that code,
/// falling back to `account_mappings` (matched on the Yardi entity code)
/// for properties that have only run the transactions ingest.
pub async fn account_activity(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<AccountActivity>> {
    let period = normalize_period_label(period)?;
    sqlx::query_as::<_, AccountActivity>(
        "SELECT t.account_code AS account_code,
                COALESCE(
                    (SELECT a.account_name FROM gl_actuals a
                     WHERE a.property_id = t.property_id
                       AND a.account_code = t.account_code
                     ORDER BY a.created_at DESC LIMIT 1),
                    (SELECT m.account_name FROM account_mappings m
                     WHERE m.account_code = t.account_code
                     ORDER BY m.updated_at DESC
                     LIMIT 1),
                    '') AS account_name,
                COUNT(*) AS txn_count,
                SUM(t.amount) AS total
         FROM gl_transactions t
         WHERE t.property_id = ? AND t.period = ?
         GROUP BY t.account_code
         ORDER BY t.account_code",
    )
    .bind(property_id)
    .bind(period)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Transactions for one account in one period, ordered by txn_date.
/// Capped at 500 rows to keep the ledger drill responsive.
pub async fn list_account_transactions(
    pool: &SqlitePool,
    property_id: &str,
    account_code: &str,
    period: &str,
) -> Result<Vec<GlTransaction>> {
    let period = normalize_period_label(period)?;
    sqlx::query_as::<_, GlTransaction>(
        "SELECT * FROM gl_transactions
         WHERE property_id = ? AND account_code = ? AND period = ?
         ORDER BY txn_date, source_row
         LIMIT 500",
    )
    .bind(property_id)
    .bind(account_code)
    .bind(period)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Returns true if there is at least one `gl_actuals` row for the given
/// property and period label. Used by scorers as a freshness gate.
pub async fn period_has_actuals(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_actuals a JOIN periods p ON a.period_id = p.id WHERE a.property_id = ? AND p.label = ?",
    )
    .bind(property_id)
    .bind(period)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0 > 0).unwrap_or(false))
}

/// Sum of `amount` for a specific `account_code` in one property + period.
/// Returns 0.0 when no rows match.
pub async fn account_actual_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
    account_code: &str,
) -> Result<f64> {
    let row: Option<(Option<f64>,)> = sqlx::query_as(
        "SELECT SUM(a.amount) FROM gl_actuals a JOIN periods p ON a.period_id = p.id WHERE a.property_id = ? AND p.label = ? AND a.account_code = ?",
    )
    .bind(property_id)
    .bind(period)
    .bind(account_code)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.0).unwrap_or(0.0))
}

// ── T12 helpers ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct CategoryMonth {
    pub noi_category: String,
    pub period_label: String,
    pub total: f64,
}

/// Aggregate `gl_actuals` by `(category, period_label)` for a given property
/// and a set of period labels (the 12 labels produced by `period_window`).
///
/// Returns rows for every category that has non-zero activity, including
/// "Unmapped". The caller is responsible for filtering/ordering.
pub async fn category_totals_by_period(
    pool: &SqlitePool,
    property_id: &str,
    periods: &[String],
) -> Result<Vec<CategoryMonth>> {
    if periods.is_empty() {
        return Ok(Vec::new());
    }
    // Build a parameterised IN clause: (?,?,?,...) — one bind per period.
    let placeholders = periods.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT a.category AS noi_category, p.label AS period_label, SUM(a.amount) AS total
         FROM gl_actuals a
         JOIN periods p ON p.id = a.period_id
         WHERE a.property_id = ?
           AND p.label IN ({placeholders})
         GROUP BY a.category, p.label
         ORDER BY a.category, p.label"
    );
    let mut query = sqlx::query_as::<_, CategoryMonth>(&sql).bind(property_id);
    for period in periods {
        query = query.bind(period);
    }
    query.fetch_all(pool).await.map_err(Into::into)
}

/// One NOI category's actual-vs-budget result for a single (property, period),
/// with `variance` already normalized to **NOI impact** so a positive value is
/// always *favorable* to NOI.
///
/// Sign convention mirrors `variance.rs::noi_impact` / `ontology::account_class`:
/// for Revenue categories favorable = actual over budget, so
/// `variance = actual - budget`; for Expense categories favorable = actual
/// under budget, so `variance = -(actual - budget)` = `budget - actual`.
/// `Unmapped` categories are excluded entirely (they never reach the bridge),
/// matching the NOI definition used everywhere else in the book.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CategoryVariance {
    pub category: String,
    pub actual: f64,
    pub budget: f64,
    /// Signed NOI impact of (actual − budget): positive is favorable to NOI.
    pub variance: f64,
}

/// Actual-vs-budget by NOI category for one property and period, with each
/// category's `variance` carrying its **favorable-positive** NOI impact.
///
/// Reads `gl_actuals` and `gl_budgets` for the period, merges by category in
/// Rust (FULL-OUTER emulation so a category present in only one side still
/// appears), then applies the `ontology::account_class` sign convention.
/// `Unmapped` categories are dropped so balance-sheet rows cannot distort the
/// bridge. Categories are returned ordered by `|variance|` descending so the
/// biggest mover reads first.
pub async fn category_variance_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<CategoryVariance>> {
    let actuals = category_totals_one(pool, "gl_actuals", property_id, period).await?;
    let budgets = category_totals_one(pool, "gl_budgets", property_id, period).await?;

    // Merge by category: (actual, budget). BTreeMap keeps a stable order before
    // we re-sort by magnitude, so ties between equal-|variance| rows are
    // deterministic (alphabetical by category).
    let mut merged = std::collections::BTreeMap::<String, (f64, f64)>::new();
    for (category, total) in actuals {
        merged.entry(category).or_default().0 += total;
    }
    for (category, total) in budgets {
        merged.entry(category).or_default().1 += total;
    }

    let mut out: Vec<CategoryVariance> = merged
        .into_iter()
        .filter_map(|(category, (actual, budget))| {
            // raw delta is actual − budget; NOI impact flips sign for expenses.
            let raw = actual - budget;
            let variance = match crate::ontology::account_class(&category) {
                crate::ontology::AccountClass::Revenue => raw,
                crate::ontology::AccountClass::Expense => -raw,
                // Unmapped never reaches the bridge.
                crate::ontology::AccountClass::Unmapped => return None,
            };
            Some(CategoryVariance {
                category,
                actual,
                budget,
                variance,
            })
        })
        .collect();

    // Biggest swing first — the bridge tells its story in magnitude order.
    out.sort_by(|a, b| b.variance.abs().total_cmp(&a.variance.abs()));
    Ok(out)
}

/// The most recent period label that has `gl_actuals` rows for this property.
///
/// Resolved **per property** (never a global MAX) so a property whose latest
/// close differs from the book's newest period still bridges its own newest
/// actuals. Returns `None` when the property has no actuals at all.
pub async fn latest_actual_period(pool: &SqlitePool, property_id: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT p.label
         FROM gl_actuals a
         JOIN periods p ON p.id = a.period_id
         WHERE a.property_id = ?
         ORDER BY p.label DESC
         LIMIT 1",
    )
    .bind(property_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(label,)| label))
}

/// Category → SUM(amount) for one property/period from `gl_actuals|gl_budgets`.
async fn category_totals_one(
    pool: &SqlitePool,
    table: &str,
    property_id: &str,
    period: &str,
) -> Result<Vec<(String, f64)>> {
    if !matches!(table, "gl_actuals" | "gl_budgets") {
        return Err(anyhow!("unsupported category-variance table: {table}"));
    }
    sqlx::query_as(&format!(
        "SELECT g.category AS category, SUM(g.amount) AS total
         FROM {table} g
         JOIN periods p ON p.id = g.period_id
         WHERE g.property_id = ? AND p.label = ?
         GROUP BY g.category"
    ))
    .bind(property_id)
    .bind(period)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

// ── Budget variance (per-account, ask agent) ───────────────────────────────────

/// One account's budget-vs-actual result for a single (property, period).
///
/// `variance = actual - budget` is the **raw** dollar delta (positive = actual
/// over budget) so callers always see the signed difference regardless of class.
/// `is_unfavorable` carries the **financial** read of that delta, which depends
/// on the account's income/expense class (see `budget_variance`):
///
/// - Expense (over budget): unfavorable when `actual > budget`.
/// - Revenue (under-collected): unfavorable when `actual < budget`.
///
/// `variance_pct` is `None` when `budget == 0` (divide-by-zero guard — no fake
/// 0%/∞ is emitted); otherwise `(variance / budget) * 100`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct BudgetVarianceRow {
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub budget: f64,
    pub actual: f64,
    /// Raw dollar delta: `actual - budget` (positive = actual over budget).
    pub variance: f64,
    /// `variance / budget * 100`, or `None` when `budget == 0`.
    pub variance_pct: Option<f64>,
    /// Financial favorability of the delta given the account's income/expense
    /// class. `Unmapped` accounts (class unknown) report `false` here AND set
    /// `class_known = false` so the caller never asserts a favorability it cannot
    /// justify.
    pub is_unfavorable: bool,
    /// Whether the income/expense class was resolvable from `category`. When
    /// `false`, `is_unfavorable` is not meaningful and must be omitted by callers.
    pub class_known: bool,
}

/// Per-account budget-vs-actual variance for one property and period.
///
/// Joins `gl_budgets` and `gl_actuals` on `(property_id, period_id, account_code)`
/// for the given period label, FULL-OUTER-emulated in Rust so an account present
/// on only one side (budget with no actual, or actual with no budget) still
/// appears. Amounts are SUM-aggregated per account_code in case a side has
/// multiple lines for the same code.
///
/// Favorability uses `ontology::account_class(category)` — the SAME classifier
/// the NOI bridge uses — so the sign is consistent with the rest of the book:
/// Revenue accounts are unfavorable when under-collected (`actual < budget`);
/// Expense accounts are unfavorable when over budget (`actual > budget`).
/// `Unmapped` accounts cannot be classified, so `is_unfavorable` is left `false`
/// and `class_known = false` (callers must not assert favorability for them).
///
/// Returns an **empty Vec** when neither table has any row for this
/// property+period. Callers MUST treat EMPTY as "no budget feed for this period"
/// and say so — never as "$0 variance / on budget".
pub async fn budget_variance(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<BudgetVarianceRow>> {
    let period = normalize_period_label(period)?;

    /// (account_code, account_name, category, amount) per side.
    type SideRow = (String, String, String, f64);

    let load = |table: &'static str| {
        let period = period.clone();
        async move {
            sqlx::query_as::<_, SideRow>(&format!(
                "SELECT g.account_code AS account_code,
                        g.account_name AS account_name,
                        g.category     AS category,
                        SUM(g.amount)  AS amount
                 FROM {table} g
                 JOIN periods p ON p.id = g.period_id
                 WHERE g.property_id = ? AND p.label = ?
                 -- account_name/category are taken non-aggregated: a single
                 -- account_code carries one category within a property+period
                 -- (Yardi chart invariant), so SUM(amount) groups deterministically.
                 GROUP BY g.account_code"
            ))
            .bind(property_id)
            .bind(&period)
            .fetch_all(pool)
            .await
        }
    };

    let budgets = load("gl_budgets").await?;
    let actuals = load("gl_actuals").await?;

    // Merge by account_code: (name, category, budget, actual). BTreeMap keeps a
    // deterministic order before we re-sort by magnitude. Budget side seeds the
    // name/category; actual fills in for actual-only accounts.
    let mut merged = std::collections::BTreeMap::<String, (String, String, f64, f64)>::new();
    for (code, name, category, amount) in budgets {
        let entry = merged
            .entry(code)
            .or_insert_with(|| (name.clone(), category.clone(), 0.0, 0.0));
        entry.0 = name;
        entry.1 = category;
        entry.2 += amount;
    }
    for (code, name, category, amount) in actuals {
        let entry = merged
            .entry(code)
            .or_insert_with(|| (name.clone(), category.clone(), 0.0, 0.0));
        // Don't clobber a budget-seeded name/category, but fill blanks.
        if entry.0.is_empty() {
            entry.0 = name;
        }
        if entry.1.is_empty() {
            entry.1 = category;
        }
        entry.3 += amount;
    }

    let mut out: Vec<BudgetVarianceRow> = merged
        .into_iter()
        .map(|(account_code, (account_name, category, budget, actual))| {
            let variance = actual - budget;
            let variance_pct = if budget == 0.0 {
                None
            } else {
                Some((variance / budget) * 100.0)
            };
            let (is_unfavorable, class_known) = match crate::ontology::account_class(&category) {
                crate::ontology::AccountClass::Revenue => (actual < budget, true),
                crate::ontology::AccountClass::Expense => (actual > budget, true),
                crate::ontology::AccountClass::Unmapped => (false, false),
            };
            BudgetVarianceRow {
                account_code,
                account_name,
                category,
                budget,
                actual,
                variance,
                variance_pct,
                is_unfavorable,
                class_known,
            }
        })
        .collect();

    // Biggest dollar swing first — the worst miss reads at the top.
    out.sort_by(|a, b| b.variance.abs().total_cmp(&a.variance.abs()));
    Ok(out)
}

/// Escape LIKE wildcards so user-supplied needles match literally.
fn escape_like_pattern(needle: &str) -> String {
    needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Substring search across payee, remarks, and account_code.
/// `%` and `_` in the needle are escaped and match literally.
/// Search individual ledger rows.
///
/// - `needle`: optional full-text needle matched (LIKE-escaped) against
///   payee/remarks/account_code. When empty/None the text clause is DROPPED
///   entirely, so all rows for the property/period are returned (rather than
///   searching for an empty string).
/// - `period`: optional period filter; normalized to YYYY-MM before matching
///   the zero-padded `period` column.
/// - `order_by_amount`: when true, order by ABS(amount) DESC (largest first,
///   sign-agnostic) so callers can find the largest transaction; when false,
///   keep the default newest-first ordering (txn_date DESC, source_row).
pub async fn search_transactions(
    pool: &SqlitePool,
    property_id: Option<&str>,
    needle: &str,
    period: Option<&str>,
    order_by_amount: bool,
    limit: i64,
) -> Result<Vec<GlTransaction>> {
    let needle = needle.trim();
    // Normalize so a model-emitted "2026-5" cannot silently filter wrong
    // against the zero-padded period column.
    let period = period.map(normalize_period_label).transpose()?;
    let period = period.as_deref();

    let pattern = (!needle.is_empty()).then(|| format!("%{}%", escape_like_pattern(needle)));

    let mut sql = String::from("SELECT * FROM gl_transactions WHERE 1 = 1");
    if pattern.is_some() {
        sql.push_str(
            " AND (payee LIKE ? ESCAPE '\\'
                   OR remarks LIKE ? ESCAPE '\\'
                   OR account_code LIKE ? ESCAPE '\\')",
        );
    }
    if property_id.is_some() {
        sql.push_str(" AND property_id = ?");
    }
    if period.is_some() {
        sql.push_str(" AND period = ?");
    }
    if order_by_amount {
        sql.push_str(" ORDER BY ABS(amount) DESC, source_row LIMIT ?");
    } else {
        sql.push_str(" ORDER BY txn_date DESC, source_row LIMIT ?");
    }

    let mut query = sqlx::query_as::<_, GlTransaction>(&sql);
    if let Some(pattern) = &pattern {
        query = query.bind(pattern).bind(pattern).bind(pattern);
    }
    if let Some(property_id) = property_id {
        query = query.bind(property_id);
    }
    if let Some(period) = period {
        query = query.bind(period);
    }
    query.bind(limit).fetch_all(pool).await.map_err(Into::into)
}

/// Recent transactions for one exact payee (vendor detail drill).
/// - Residents (`is_resident = 0`) are always excluded.
/// - Results ordered newest-first (txn_date DESC, source_row DESC).
/// - Capped at 200 rows; callers pass a lower limit (e.g., 15) for the TUI pane.
pub async fn payee_transactions(
    pool: &SqlitePool,
    payee: &str,
    property_id: Option<&str>,
    limit: i64,
) -> Result<Vec<GlTransaction>> {
    let capped_limit = limit.min(200);
    let mut sql = String::from(
        "SELECT * FROM gl_transactions
         WHERE is_resident = 0 AND payee = ?",
    );
    if property_id.is_some() {
        sql.push_str(" AND property_id = ?");
    }
    sql.push_str(" ORDER BY txn_date DESC, source_row DESC LIMIT ?");

    let mut query = sqlx::query_as::<_, GlTransaction>(&sql).bind(payee);
    if let Some(pid) = property_id {
        query = query.bind(pid);
    }
    query
        .bind(capped_limit)
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

// ── Vendor spend ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VendorPropertyShare {
    pub property: String,
    pub txn_count: i64,
    pub total: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VendorSpend {
    pub payee: String,
    pub txn_count: i64,
    pub total: f64,
    pub first_period: String,
    pub last_period: String,
    /// Per-property breakdown, ordered by absolute spend descending.
    pub by_property: Vec<VendorPropertyShare>,
}

#[derive(Debug, sqlx::FromRow)]
struct VendorSpendRow {
    payee: String,
    property: String,
    txn_count: i64,
    total: f64,
    first_period: String,
    last_period: String,
}

/// Aggregate vendor payments from `gl_transactions`, with a per-property
/// breakdown.
///
/// - Residents (`is_resident = 1`) are always excluded.
/// - Vendors are ordered by absolute spend descending and capped at 200.
/// - `payee_contains` is LIKE-escaped (reuses `escape_like_pattern`).
pub async fn vendor_spend(
    pool: &SqlitePool,
    property_id: Option<&str>,
    since_period: Option<&str>,
    payee_contains: Option<&str>,
    limit: i64,
) -> Result<Vec<VendorSpend>> {
    let capped_limit = limit.min(200);
    // Normalize so a model-emitted "2025-1" cannot silently filter wrong
    // against the zero-padded period column.
    let since_period = since_period.map(normalize_period_label).transpose()?;
    let since_period = since_period.as_deref();

    let mut filters = String::new();
    if property_id.is_some() {
        filters.push_str(" AND t.property_id = ?");
    }
    if since_period.is_some() {
        filters.push_str(" AND t.period >= ?");
    }
    let payee_pattern = payee_contains.map(|p| format!("%{}%", escape_like_pattern(p)));
    if payee_pattern.is_some() {
        filters.push_str(" AND t.payee LIKE ? ESCAPE '\\'");
    }

    // One pass grouped by payee+property; the IN-subquery limits to the
    // top payees by absolute total so the cap applies to vendors, not rows.
    let sql = format!(
        "WITH per AS (
            SELECT t.payee AS payee,
                   COALESCE(p.name, t.entity_code) AS property,
                   COUNT(*) AS txn_count,
                   SUM(t.amount) AS total,
                   MIN(t.period) AS first_period,
                   MAX(t.period) AS last_period
            FROM gl_transactions t
            LEFT JOIN properties p ON p.id = t.property_id
            WHERE t.is_resident = 0 AND t.payee != ''{filters}
            GROUP BY t.payee, COALESCE(p.name, t.entity_code)
         )
         SELECT payee, property, txn_count, total, first_period, last_period
         FROM per
         WHERE payee IN (
            SELECT payee FROM per GROUP BY payee
            ORDER BY ABS(SUM(total)) DESC LIMIT ?
         )"
    );

    let mut query = sqlx::query_as::<_, VendorSpendRow>(&sql);
    if let Some(pid) = property_id {
        query = query.bind(pid);
    }
    if let Some(sp) = since_period {
        query = query.bind(sp);
    }
    if let Some(pp) = &payee_pattern {
        query = query.bind(pp);
    }
    let rows = query.bind(capped_limit).fetch_all(pool).await?;

    // Fold payee+property rows into one VendorSpend per payee.
    let mut order: Vec<String> = Vec::new();
    let mut folded: std::collections::HashMap<String, VendorSpend> =
        std::collections::HashMap::new();
    for row in rows {
        let entry = folded.entry(row.payee.clone()).or_insert_with(|| {
            order.push(row.payee.clone());
            VendorSpend {
                payee: row.payee.clone(),
                txn_count: 0,
                total: 0.0,
                first_period: row.first_period.clone(),
                last_period: row.last_period.clone(),
                by_property: Vec::new(),
            }
        });
        entry.txn_count += row.txn_count;
        entry.total += row.total;
        if row.first_period < entry.first_period {
            entry.first_period = row.first_period.clone();
        }
        if row.last_period > entry.last_period {
            entry.last_period = row.last_period.clone();
        }
        entry.by_property.push(VendorPropertyShare {
            property: row.property,
            txn_count: row.txn_count,
            total: row.total,
        });
    }
    let mut result: Vec<VendorSpend> = order
        .into_iter()
        .filter_map(|payee| folded.remove(&payee))
        .collect();
    for vendor in &mut result {
        vendor
            .by_property
            .sort_by(|a, b| b.total.abs().total_cmp(&a.total.abs()));
    }
    result.sort_by(|a, b| b.total.abs().total_cmp(&a.total.abs()));
    Ok(result)
}

// ── Unit receivables ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn insert_unit_receivable(
    pool: &SqlitePool,
    property_id: &str,
    as_of_date: &str,
    resident_code: &str,
    resident_name: Option<&str>,
    resident_status: Option<&str>,
    total_delinquent: f64,
    current_owed: Option<f64>,
    days_late: Option<i64>,
    source_file: &str,
    source_row: i64,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO unit_receivables (id, property_id, as_of_date, resident_code, resident_name, resident_status, total_delinquent, current_owed, days_late, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(as_of_date)
    .bind(resident_code)
    .bind(resident_name)
    .bind(resident_status)
    .bind(total_delinquent)
    .bind(current_owed)
    .bind(days_late)
    .bind(source_file)
    .bind(source_row)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn receivables_snapshot_exists(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<bool> {
    let pat = format!("{}%", escape_like_pattern(period));
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM unit_receivables WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\'",
    )
    .bind(property_id)
    .bind(pat)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0 > 0).unwrap_or(false))
}

pub async fn unit_delinquent_total(
    pool: &SqlitePool,
    property_id: &str,
    resident_code: &str,
    period: &str,
) -> Result<f64> {
    let pat = format!("{}%", escape_like_pattern(period));
    let row: Option<(Option<f64>,)> = sqlx::query_as(
        "SELECT total_delinquent FROM unit_receivables WHERE property_id = ? AND resident_code = ? AND as_of_date LIKE ? ESCAPE '\\' ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(resident_code)
    .bind(pat)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.0).unwrap_or(0.0))
}

/// One per-resident receivable row (latest snapshot within a period).
#[derive(Debug, Clone)]
pub struct ResidentReceivable {
    pub property_id: String,
    pub resident_code: String,
    pub resident_name: Option<String>,
    pub resident_status: Option<String>,
    pub total_delinquent: f64,
    pub current_owed: Option<f64>,
    pub days_late: Option<i64>,
    pub as_of_date: String,
}

/// Aging roll-up for one property + period, bucketed on `days_late`.
#[derive(Debug, Clone, Default)]
pub struct DelinquencyAging {
    pub current_owed_total: f64,
    pub b0_30: f64,
    pub b31_60: f64,
    pub b61_90: f64,
    pub b90_plus: f64,
    pub cnt_current: i64,
    pub cnt_0_30: i64,
    pub cnt_31_60: i64,
    pub cnt_61_90: i64,
    pub cnt_90_plus: i64,
    pub prepaid_total: f64,
    pub prepaid_cnt: i64,
}

impl DelinquencyAging {
    /// Total dollars owed across all delinquency buckets (excludes prepaids).
    pub fn delinquent_total(&self) -> f64 {
        self.b0_30 + self.b31_60 + self.b61_90 + self.b90_plus
    }

    /// Count of truly-delinquent units (days_late > 0; excludes current-but-owed).
    /// Matches the population covered by `delinquent_total()`.
    pub fn delinquent_cnt(&self) -> i64 {
        self.cnt_0_30 + self.cnt_31_60 + self.cnt_61_90 + self.cnt_90_plus
    }
}

/// List per-resident receivables for the latest snapshot within `period`,
/// worst-first (highest `total_delinquent` at the top). Uses a per-property
/// MAX(as_of_date) subquery so a property that has not refreshed is not
/// shadowed by a globally newer date.
pub async fn list_receivables_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<ResidentReceivable>> {
    /// (resident_code, resident_name, resident_status, total_delinquent,
    /// current_owed, days_late, as_of_date) as returned by the query.
    type ReceivableRow = (
        String,
        Option<String>,
        Option<String>,
        f64,
        Option<f64>,
        Option<i64>,
        String,
    );
    let pat = format!("{}%", escape_like_pattern(period));
    let rows: Vec<ReceivableRow> = sqlx::query_as(
        "SELECT resident_code, resident_name, resident_status, total_delinquent, current_owed, days_late, as_of_date \
         FROM unit_receivables \
         WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\' \
           AND as_of_date = ( \
               SELECT MAX(as_of_date) FROM unit_receivables \
               WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\' \
           ) \
         ORDER BY total_delinquent DESC, resident_code ASC",
    )
    .bind(property_id)
    .bind(&pat)
    .bind(property_id)
    .bind(&pat)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                resident_code,
                resident_name,
                resident_status,
                total_delinquent,
                current_owed,
                days_late,
                as_of_date,
            )| {
                ResidentReceivable {
                    property_id: property_id.to_string(),
                    resident_code,
                    resident_name,
                    resident_status,
                    total_delinquent,
                    current_owed,
                    days_late,
                    as_of_date,
                }
            },
        )
        .collect())
}

/// Compute the aging roll-up for a property + period from the latest snapshot.
///
/// Buckets are keyed on `days_late`: `<=0` → current-but-owed, `1-30`, `31-60`,
/// `61-90`, `>90`. Rows with NULL `days_late` are treated as aged (90+) so a
/// legacy ingest that predates migration 009 still surfaces its balances rather
/// than silently dropping them. Prepaids (`current_owed < 0`) feed the prepaid
/// cells regardless of bucket.
pub async fn delinquency_aging_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<DelinquencyAging> {
    let rows = list_receivables_for_period(pool, property_id, period).await?;
    let mut aging = DelinquencyAging::default();
    for r in &rows {
        // Prepaid: a credit balance, no delinquency. Counted separately.
        if let Some(owed) = r.current_owed {
            if owed < 0.0 {
                aging.prepaid_total += -owed;
                aging.prepaid_cnt += 1;
                continue;
            }
        }
        // Skip non-delinquent, non-prepaid rows entirely (no balance to age).
        if r.total_delinquent <= 0.0 {
            continue;
        }
        let amt = r.total_delinquent;
        match r.days_late {
            Some(d) if d <= 0 => {
                aging.current_owed_total += amt;
                aging.cnt_current += 1;
            }
            Some(d) if d <= 30 => {
                aging.b0_30 += amt;
                aging.cnt_0_30 += 1;
            }
            Some(d) if d <= 60 => {
                aging.b31_60 += amt;
                aging.cnt_31_60 += 1;
            }
            Some(d) if d <= 90 => {
                aging.b61_90 += amt;
                aging.cnt_61_90 += 1;
            }
            // >90 or NULL (unknown age — surface as aged rather than hide it).
            _ => {
                aging.b90_plus += amt;
                aging.cnt_90_plus += 1;
            }
        }
    }
    Ok(aging)
}

/// Recent delinquency-amount trend for a property, oldest → newest. Returns up
/// to `last_n` `(as_of_date, delinquent_amount)` points in chronological order.
pub async fn delinquency_trend(
    pool: &SqlitePool,
    property_id: &str,
    last_n: i64,
) -> Result<Vec<(String, f64)>> {
    let mut rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT as_of_date, delinquent_amount FROM delinquency_snapshots \
         WHERE property_id = ? ORDER BY as_of_date DESC LIMIT ?",
    )
    .bind(property_id)
    .bind(last_n)
    .fetch_all(pool)
    .await?;
    rows.reverse(); // chronological (oldest first) for left→right rendering
    Ok(rows)
}

// ── Unit leases ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn insert_unit_lease(
    pool: &SqlitePool,
    property_id: &str,
    as_of_date: &str,
    unit_label: &str,
    resident_code: Option<&str>,
    resident_name: Option<&str>,
    market_rent: Option<f64>,
    charge_rent: Option<f64>,
    source_file: &str,
    source_row: i64,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO unit_leases (id, property_id, as_of_date, unit_label, resident_code, resident_name, market_rent, charge_rent, source_file, source_row, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(as_of_date)
    .bind(unit_label)
    .bind(resident_code)
    .bind(resident_name)
    .bind(market_rent)
    .bind(charge_rent)
    .bind(source_file)
    .bind(source_row)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn leases_snapshot_exists(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<bool> {
    let pat = format!("{}%", escape_like_pattern(period));
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM unit_leases WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\'",
    )
    .bind(property_id)
    .bind(pat)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0 > 0).unwrap_or(false))
}

pub async fn unit_lease_rent(
    pool: &SqlitePool,
    property_id: &str,
    unit_label: &str,
    resident_code: &str,
    period: &str,
) -> Result<Option<f64>> {
    let pat = format!("{}%", escape_like_pattern(period));
    let row: Option<(Option<f64>,)> = sqlx::query_as(
        "SELECT charge_rent FROM unit_leases WHERE property_id = ? AND unit_label = ? AND resident_code = ? AND as_of_date LIKE ? ESCAPE '\\' ORDER BY as_of_date DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(unit_label)
    .bind(resident_code)
    .bind(pat)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.0))
}

/// One per-unit lease row (latest snapshot within a period) carrying the
/// current charge rent and the market rent. The spread (`market - charge`) is
/// the renewal upside; negative spread means the unit is priced over market.
#[derive(Debug, Clone)]
pub struct UnitLeaseRow {
    pub unit_label: String,
    pub resident_code: Option<String>,
    pub resident_name: Option<String>,
    pub market_rent: Option<f64>,
    pub charge_rent: Option<f64>,
    pub as_of_date: String,
}

/// List per-unit leases for the latest snapshot within `period`, biggest upside
/// first (largest `market_rent - charge_rent` at the top). Uses a per-property
/// MAX(as_of_date) subquery so a property that has not refreshed is not shadowed
/// by a globally newer date (mirrors `list_receivables_for_period`).
pub async fn list_leases_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<UnitLeaseRow>> {
    /// (unit_label, resident_code, resident_name, market_rent, charge_rent,
    /// as_of_date) as returned by the query.
    type LeaseRow = (
        String,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        String,
    );
    let pat = format!("{}%", escape_like_pattern(period));
    let rows: Vec<LeaseRow> = sqlx::query_as(
        "SELECT unit_label, resident_code, resident_name, market_rent, charge_rent, as_of_date \
         FROM unit_leases \
         WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\' \
           AND as_of_date = ( \
               SELECT MAX(as_of_date) FROM unit_leases \
               WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\' \
           ) \
         ORDER BY (COALESCE(market_rent, 0) - COALESCE(charge_rent, 0)) DESC, unit_label ASC",
    )
    .bind(property_id)
    .bind(&pat)
    .bind(property_id)
    .bind(&pat)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(unit_label, resident_code, resident_name, market_rent, charge_rent, as_of_date)| {
                UnitLeaseRow {
                    unit_label,
                    resident_code,
                    resident_name,
                    market_rent,
                    charge_rent,
                    as_of_date,
                }
            },
        )
        .collect())
}

/// Aggregate the renewal opportunity across lease rows. Returns
/// `(monthly_uplift, annual_uplift, underpriced_count)` where a unit is
/// "underpriced" when `charge_rent < 0.95 * market_rent`. The monthly rent uplift is
/// the sum of `(market - charge)` over the underpriced units only (money on the
/// table at renewal); annual is `monthly * 12`. Pure — testable without a DB.
pub fn lease_opportunity(rows: &[UnitLeaseRow]) -> (f64, f64, usize) {
    let mut monthly = 0.0;
    let mut underpriced = 0usize;
    for r in rows {
        let (Some(market), Some(charge)) = (r.market_rent, r.charge_rent) else {
            continue;
        };
        if market > 0.0 && charge < 0.95 * market {
            monthly += market - charge;
            underpriced += 1;
        }
    }
    (monthly, monthly * 12.0, underpriced)
}

// ---------------------------------------------------------------------------
// monthly_actuals — T12 reversion flywheel (migration 008)
// ---------------------------------------------------------------------------

pub async fn insert_monthly_actual(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
    account_code: &str,
    account_name: Option<&str>,
    amount: f64,
    source_file: &str,
) -> Result<String> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO monthly_actuals (id, property_id, period, account_code, account_name, amount, source_file, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(property_id)
    .bind(period)
    .bind(account_code)
    .bind(account_name)
    .bind(amount)
    .bind(source_file)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(id)
}

/// Sum of monthly_actuals for one (property, period, account). None if no rows.
pub async fn monthly_actual(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
    account_code: &str,
) -> Result<Option<f64>> {
    let row: Option<(Option<f64>,)> = sqlx::query_as(
        "SELECT SUM(amount) FROM monthly_actuals WHERE property_id = ? AND period = ? AND account_code = ?",
    )
    .bind(property_id)
    .bind(period)
    .bind(account_code)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.0))
}

/// Mean of the account's monthly amount over the 12 periods strictly BEFORE `period`.
/// Returns (mean, months_present); months with no row are simply absent (not zero).
/// None if zero of the 12 prior months are present.
pub async fn t12_mean(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
    account_code: &str,
) -> Result<Option<(f64, usize)>> {
    // Build the 12 prior YYYY-MM via crate::calls::prev_period.
    let mut p = period.to_string();
    let mut periods = Vec::with_capacity(12);
    for _ in 0..12 {
        p = crate::calls::prev_period(&p)?;
        periods.push(p.clone());
    }
    let mut sum = 0.0;
    let mut n = 0usize;
    for per in &periods {
        if let Some(v) = monthly_actual(pool, property_id, per, account_code).await? {
            sum += v;
            n += 1;
        }
    }
    if n == 0 {
        Ok(None)
    } else {
        Ok(Some((sum / n as f64, n)))
    }
}

pub async fn monthly_actuals_exist(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT COUNT(*) FROM monthly_actuals WHERE property_id = ? AND period = ?")
            .bind(property_id)
            .bind(period)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.0 > 0).unwrap_or(false))
}

/// The dominant NOI category for an account_code in a property (from gl_actuals), if known.
pub async fn category_for_account(
    pool: &SqlitePool,
    property_id: &str,
    account_code: &str,
) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT category FROM gl_actuals WHERE property_id = ? AND account_code = ? AND category IS NOT NULL AND category != '' GROUP BY category ORDER BY COUNT(*) DESC LIMIT 1",
    )
    .bind(property_id)
    .bind(account_code)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Historical accuracy of past "normalize" `noi_diagnosis` calls for an NOI category:
/// `(mean_score, n)` over scored calls whose payload `expected_direction` was "normalize".
/// `None` if no such scored calls exist yet. Lets the emitter learn that some categories
/// (e.g. Taxes, Insurance) systematically do NOT normalize and predict "persist" instead.
pub async fn category_normalize_rate(
    pool: &SqlitePool,
    property_id: &str,
    category: &str,
) -> Result<Option<(f64, usize)>> {
    let row: Option<(Option<f64>, i64)> = sqlx::query_as(
        "SELECT AVG(score), COUNT(*) FROM calls WHERE call_type = 'noi_diagnosis' AND status = 'scored' AND property_id = ? AND json_extract(payload_json, '$.category') = ? AND json_extract(payload_json, '$.expected_direction') = 'normalize'",
    )
    .bind(property_id)
    .bind(category)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((Some(avg), n)) if n > 0 => Some((avg, n as usize)),
        _ => None,
    })
}

/// All scored calls of a given call_type, optionally scoped to one property.
pub async fn scored_calls_of_type(
    pool: &SqlitePool,
    call_type: &str,
    property_id: Option<&str>,
) -> Result<Vec<Call>> {
    match property_id {
        Some(pid) => sqlx::query_as::<_, Call>(
            "SELECT * FROM calls WHERE call_type = ? AND status = 'scored' AND property_id = ? ORDER BY scored_at DESC",
        )
        .bind(call_type)
        .bind(pid)
        .fetch_all(pool)
        .await
        .map_err(Into::into),
        None => sqlx::query_as::<_, Call>(
            "SELECT * FROM calls WHERE call_type = ? AND status = 'scored' ORDER BY scored_at DESC",
        )
        .bind(call_type)
        .fetch_all(pool)
        .await
        .map_err(Into::into),
    }
}

/// Earliest period present in monthly_actuals for a property (YYYY-MM), if any.
pub async fn earliest_monthly_period(
    pool: &SqlitePool,
    property_id: &str,
) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT MIN(period) FROM monthly_actuals WHERE property_id = ?")
            .bind(property_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|r| r.0))
}

// ── Portfolio roll-up (F4 — Close Desk summary band) ──────────────────────────

/// One-glance portfolio roll-up for the Close Desk summary band. Every figure is
/// an honest DB aggregate over all properties for a single `period`; occupancy,
/// in-place rent, and delinquency use the latest snapshot *per property* within
/// the period (never a global MAX), so a property that has not yet refreshed is
/// not shadowed by a globally newer date.
#[derive(Debug, Clone, Default)]
pub struct PortfolioRollup {
    /// Number of properties in the book.
    pub property_count: i64,
    /// Sum of `properties.unit_count`.
    pub total_units: i64,
    /// Owner-ready property count (passed in from the close-readiness summary).
    pub owner_ready: i64,
    /// How many properties contributed an occupancy reading this period (the
    /// denominator behind `occ_low`/`occ_high`). Zero ⇒ no occupancy data.
    pub occ_properties: i64,
    /// Lowest physical occupancy across properties (0.0..=1.0).
    pub occ_low: f64,
    /// Highest physical occupancy across properties (0.0..=1.0).
    pub occ_high: f64,
    /// Sum of latest `in_place_rent_total` across properties (monthly $).
    pub inplace_rent_total: f64,
    /// Portfolio NOI from actuals (Σ revenue − Σ expense over the period).
    pub noi_actual: f64,
    /// Portfolio NOI from budget (same sign convention).
    pub noi_budget: f64,
    /// Sum of latest `delinquency_snapshots.delinquent_amount` per property.
    pub delinquent_total: f64,
}

impl PortfolioRollup {
    /// Owner-ready ratio (0.0..=1.0); 0.0 when the book is empty.
    pub fn ready_ratio(&self) -> f64 {
        if self.property_count == 0 {
            0.0
        } else {
            self.owner_ready as f64 / self.property_count as f64
        }
    }

    /// NOI variance vs budget (favorable when positive — actual NOI over budget).
    pub fn noi_variance(&self) -> f64 {
        self.noi_actual - self.noi_budget
    }
}

/// Aggregate the whole book into a [`PortfolioRollup`] for `period`.
///
/// `owner_ready` is passed in from the already-computed close-readiness summary
/// (`CloseReadinessSummary.ready_count`) to avoid recomputing readiness here.
///
/// NOI sign convention mirrors `variance.rs`/`ontology::account_class`: revenue
/// categories add to NOI, expense categories subtract, and `Unmapped` is
/// excluded — so a positive `noi_variance()` always means favorable.
pub async fn portfolio_rollup(
    pool: &SqlitePool,
    period: &str,
    owner_ready: i64,
) -> Result<PortfolioRollup> {
    let properties = list_properties(pool).await?;
    let property_count = properties.len() as i64;
    let total_units: i64 = properties.iter().map(|p| p.unit_count).sum();

    let like = format!("{}%", escape_like_pattern(period));

    let mut occ_low = f64::INFINITY;
    let mut occ_high = f64::NEG_INFINITY;
    let mut occ_properties: i64 = 0;
    let mut inplace_rent_total = 0.0;
    let mut delinquent_total = 0.0;

    for property in &properties {
        // Latest rent-roll snapshot for this property within the period.
        let rr: Option<(i64, i64, i64, f64)> = sqlx::query_as(
            "SELECT occupied_units, vacant_units, down_units, in_place_rent_total
             FROM rent_roll_snapshots
             WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\'
             ORDER BY as_of_date DESC LIMIT 1",
        )
        .bind(&property.id)
        .bind(&like)
        .fetch_optional(pool)
        .await?;
        if let Some((occupied, vacant, down, in_place)) = rr {
            inplace_rent_total += in_place;
            if let Some(rate) = crate::ontology::occupancy_rate(occupied, vacant, down) {
                occ_low = occ_low.min(rate);
                occ_high = occ_high.max(rate);
                occ_properties += 1;
            }
        }

        // Latest delinquency snapshot for this property within the period.
        let delin: Option<(f64,)> = sqlx::query_as(
            "SELECT delinquent_amount
             FROM delinquency_snapshots
             WHERE property_id = ? AND as_of_date LIKE ? ESCAPE '\\'
             ORDER BY as_of_date DESC LIMIT 1",
        )
        .bind(&property.id)
        .bind(&like)
        .fetch_optional(pool)
        .await?;
        if let Some((amount,)) = delin {
            delinquent_total += amount;
        }
    }

    if occ_properties == 0 {
        occ_low = 0.0;
        occ_high = 0.0;
    }

    let noi_actual = portfolio_noi(pool, "gl_actuals", period).await?;
    let noi_budget = portfolio_noi(pool, "gl_budgets", period).await?;

    Ok(PortfolioRollup {
        property_count,
        total_units,
        owner_ready,
        occ_properties,
        occ_low,
        occ_high,
        inplace_rent_total,
        noi_actual,
        noi_budget,
        delinquent_total,
    })
}

/// Portfolio NOI = Σ(revenue categories) − Σ(expense categories) across all
/// properties for `period`, read from `table` (`gl_actuals` or `gl_budgets`).
/// `Unmapped` is excluded, matching the NOI bridge in `variance.rs`.
async fn portfolio_noi(pool: &SqlitePool, table: &str, period: &str) -> Result<f64> {
    if !matches!(table, "gl_actuals" | "gl_budgets") {
        return Err(anyhow!("unsupported portfolio NOI table: {table}"));
    }
    let rows: Vec<(String, f64)> = sqlx::query_as(&format!(
        "SELECT g.category AS category, SUM(g.amount) AS total
         FROM {table} g
         JOIN periods p ON p.id = g.period_id
         WHERE p.label = ?
         GROUP BY g.category"
    ))
    .bind(period)
    .fetch_all(pool)
    .await?;
    let mut noi = 0.0;
    for (category, total) in rows {
        match crate::ontology::account_class(&category) {
            crate::ontology::AccountClass::Revenue => noi += total,
            crate::ontology::AccountClass::Expense => noi -= total,
            crate::ontology::AccountClass::Unmapped => {}
        }
    }
    Ok(noi)
}

/// Distinct (account_code, account_name) present for a (property, period).
pub async fn pl_accounts_for_period(
    pool: &SqlitePool,
    property_id: &str,
    period: &str,
) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT account_code, account_name FROM monthly_actuals WHERE property_id = ? AND period = ? ORDER BY account_code",
    )
    .bind(property_id)
    .bind(period)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(c, n)| (c, n.unwrap_or_default()))
        .collect())
}

/// Fetch all calls for the recall service: scored AND open, for one property+call_type,
/// confounded=0, newest origin_period first, limit 200. Neighbors include open/pending
/// calls (visible denominator) as well as scored ones for the calibrated stat.
pub async fn fetch_calls_for_recall(
    pool: &SqlitePool,
    property_id: &str,
    call_type: &str,
) -> Result<Vec<Call>> {
    sqlx::query_as::<_, Call>(
        "SELECT * FROM calls WHERE property_id = ? AND call_type = ? AND confounded = 0 ORDER BY origin_period DESC LIMIT 200",
    )
    .bind(property_id).bind(call_type).fetch_all(pool).await.map_err(Into::into)
}

pub async fn fetch_pending_outcomes(pool: &SqlitePool) -> Result<Vec<Call>> {
    let today = now_iso();
    let ym = &today[..7]; // YYYY-MM
    sqlx::query_as::<_, Call>(
        "SELECT * FROM calls WHERE call_type='decision' AND status='open' AND outcome_mode != 'auto' AND mature_by <= ? ORDER BY mature_by ASC",
    )
    .bind(ym)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_outcomes_lists_matured_unresolved_human_calls() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_database(&pool).await.unwrap();
        let pid = upsert_property(&pool, "P", "Austin", 100, "U", "U")
            .await
            .unwrap();
        sqlx::query("INSERT INTO calls (id, property_id, origin_period, call_type, status, made_at, mature_by, payload_json, outcome_mode, created_at, updated_at) VALUES ('p1', ?, '2026-01', 'decision', 'open', 't', '2026-02', '{}', 'human', 't', 't')")
            .bind(&pid).execute(&pool).await.unwrap();
        let pending = fetch_pending_outcomes(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "p1");
    }
}
