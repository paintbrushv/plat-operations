//! Append-only exact-cent period imports and issued reports.
use super::{
    digest, error,
    money::Money,
    variance::{self, Line},
    Result, APPLICATION_ID, CONTRACT, SCHEMA,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{collections::BTreeMap, fs::OpenOptions, path::Path, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub as_of_date: String,
    pub occupied_units: i64,
    pub vacant_units: i64,
    pub down_units: i64,
    pub market_rent_total: Option<Money>,
    pub in_place_rent_total: Option<Money>,
    pub delinquent_amount: Option<Money>,
    pub prepaid_amount: Option<Money>,
    pub concessions_amount: Option<Money>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub property: String,
    pub period: String,
    pub currency: String,
    pub expense_convention: String,
    pub unit_count: i64,
    pub actuals: Vec<Line>,
    pub budgets: Vec<Line>,
    pub snapshot: Option<Snapshot>,
}

impl Dataset {
    pub fn validate(&self) -> Result<()> {
        if self.property.trim().is_empty()
            || self.property.len() > 120
            || self.currency != "USD"
            || self.expense_convention != "positive_costs"
            || !(1..=10_000_000).contains(&self.unit_count)
        {
            return Err(error(
                "INVALID_INPUT",
                "Require a property, USD, positive_costs, and a credible unit count",
            ));
        }
        let period = crate::db::normalize_period_label(&self.period)
            .map_err(|_| error("INVALID_INPUT", "Invalid YYYY-MM period"))?;
        if period != self.period {
            return Err(error("INVALID_INPUT", "Period must use YYYY-MM"));
        }
        if let Some(s) = &self.snapshot {
            let day = NaiveDate::parse_from_str(&s.as_of_date, "%Y-%m-%d")
                .map_err(|_| error("INVALID_INPUT", "Invalid snapshot date"))?;
            if day.format("%Y-%m-%d").to_string() != s.as_of_date {
                return Err(error("INVALID_INPUT", "Use a canonical snapshot date"));
            }
            let counts = [s.occupied_units, s.vacant_units, s.down_units];
            if !s.as_of_date.starts_with(&format!("{}-", self.period))
                || counts.iter().any(|n| *n < 0 || *n > self.unit_count)
                || counts.iter().sum::<i64>() != self.unit_count
            {
                return Err(error(
                    "SNAPSHOT_MISMATCH",
                    "Snapshot must cover this period and reconcile to the property unit count",
                ));
            }
        }
        variance::compute(&self.actuals, &self.budgets)?;
        Ok(())
    }
}

const DDL: &str = r#"
CREATE TABLE exact_revisions (
 id TEXT PRIMARY KEY, property TEXT NOT NULL, period TEXT NOT NULL, revision INTEGER NOT NULL,
 parent_id TEXT UNIQUE REFERENCES exact_revisions(id), reason TEXT, unit_count INTEGER NOT NULL,
 input_sha256 TEXT NOT NULL, sources TEXT NOT NULL, created_at TEXT NOT NULL,
 UNIQUE(property, period, revision)
);
CREATE TABLE exact_gl (
 revision_id TEXT NOT NULL REFERENCES exact_revisions(id), kind TEXT NOT NULL CHECK(kind IN ('actual','budget')),
 ordinal INTEGER NOT NULL, account_code TEXT NOT NULL, account_name TEXT NOT NULL, category TEXT NOT NULL,
 amount_cents INTEGER NOT NULL CHECK(typeof(amount_cents)='integer' AND amount_cents != -9223372036854775808),
 PRIMARY KEY(revision_id,kind,ordinal)
);
CREATE TABLE exact_snapshots (
 revision_id TEXT PRIMARY KEY REFERENCES exact_revisions(id), as_of_date TEXT NOT NULL,
 occupied_units INTEGER NOT NULL, vacant_units INTEGER NOT NULL, down_units INTEGER NOT NULL
);
CREATE TABLE exact_snapshot_money (
 revision_id TEXT NOT NULL REFERENCES exact_snapshots(revision_id), field TEXT NOT NULL,
 amount_cents INTEGER NOT NULL CHECK(typeof(amount_cents)='integer' AND amount_cents != -9223372036854775808),
 PRIMARY KEY(revision_id, field)
);
CREATE TABLE exact_reports (
 id TEXT PRIMARY KEY, revision_id TEXT NOT NULL REFERENCES exact_revisions(id), body TEXT NOT NULL,
 body_sha256 TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE exact_migrations (source_sha256 TEXT PRIMARY KEY, review_json TEXT NOT NULL, differences_json TEXT NOT NULL);
CREATE TABLE archived_reports (id TEXT PRIMARY KEY, original_body TEXT NOT NULL, original_path TEXT, original_body_sha256 TEXT NOT NULL);
-- Read-only names prevent pre-cents writers from silently interpreting cents as dollars.
CREATE VIEW schema_migrations AS SELECT 'exact/1' AS version, '' AS applied_at;
CREATE VIEW properties AS SELECT DISTINCT property AS id, property AS name, unit_count, '' AS market,
 '' AS owner_entity, '' AS property_manager, '' AS created_at FROM exact_revisions;
CREATE VIEW periods AS SELECT DISTINCT period AS id, period AS label, 0 AS year, 0 AS month FROM exact_revisions;
CREATE VIEW gl_actuals AS SELECT * FROM exact_gl WHERE kind='actual';
CREATE VIEW gl_budgets AS SELECT * FROM exact_gl WHERE kind='budget';
CREATE VIEW rent_roll_snapshots AS SELECT * FROM exact_snapshots;
"#;

pub async fn create(path: &Path) -> Result<SqlitePool> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| error("DESTINATION_EXISTS", "Use a new writable database path"))?;
    drop(file);
    let pool = connect_unchecked(path, false).await?;
    sqlx::query(&format!("PRAGMA application_id={APPLICATION_ID}"))
        .execute(&pool)
        .await?;
    sqlx::raw_sql(DDL).execute(&pool).await?;
    for table in [
        "exact_revisions",
        "exact_gl",
        "exact_snapshots",
        "exact_snapshot_money",
        "exact_reports",
        "exact_migrations",
        "archived_reports",
    ] {
        for action in ["UPDATE", "DELETE"] {
            sqlx::raw_sql(&format!("CREATE TRIGGER {table}_no_{action} BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT,'immutable exact record'); END;"))
                .execute(&pool).await?;
        }
    }
    sqlx::query(&format!("PRAGMA user_version={SCHEMA}"))
        .execute(&pool)
        .await?;
    Ok(pool)
}
async fn connect_unchecked(path: &Path, read_only: bool) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(read_only)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}
pub async fn open(path: &Path, read_only: bool) -> Result<SqlitePool> {
    let pool = connect_unchecked(path, read_only).await?;
    let app: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&pool)
        .await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await?;
    if app != APPLICATION_ID || version != SCHEMA {
        pool.close().await;
        return Err(error(
            "SCHEMA_MISMATCH",
            "Use an exact-cent database or run reviewed copy migration",
        ));
    }
    Ok(pool)
}

pub async fn import(
    pool: &SqlitePool,
    data: &Dataset,
    parent: Option<&str>,
    reason: Option<&str>,
    sources: &Value,
) -> Result<String> {
    data.validate()?;
    let input_hash = digest(&serde_json::to_vec(data)?);
    let mut tx = pool.begin().await?;
    let latest = sqlx::query("SELECT id,revision,input_sha256 FROM exact_revisions WHERE property=? AND period=? ORDER BY revision DESC LIMIT 1")
        .bind(&data.property).bind(&data.period).fetch_optional(&mut *tx).await?;
    let revision = if let Some(previous) = latest {
        let id: String = previous.get("id");
        if parent.is_none() && previous.get::<String, _>("input_sha256") == input_hash {
            return Ok(id);
        }
        if parent != Some(id.as_str())
            || reason.is_none_or(|s| s.trim().is_empty() || s.len() > 2000)
        {
            return Err(error(
                "STALE_REVISION",
                "A correction requires the current revision and an explicit reason",
            ));
        }
        previous
            .get::<i64, _>("revision")
            .checked_add(1)
            .ok_or_else(|| error("INPUT_LIMIT", "Revision limit reached"))?
    } else {
        if parent.is_some() {
            return Err(error(
                "STALE_REVISION",
                "No current revision matches this correction",
            ));
        }
        1
    };
    let id = crate::db::new_id();
    sqlx::query("INSERT INTO exact_revisions VALUES (?,?,?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(&data.property)
        .bind(&data.period)
        .bind(revision)
        .bind(parent)
        .bind(reason)
        .bind(data.unit_count)
        .bind(&input_hash)
        .bind(serde_json::to_string(sources)?)
        .bind(crate::db::now_iso())
        .execute(&mut *tx)
        .await?;
    for (kind, rows) in [("actual", &data.actuals), ("budget", &data.budgets)] {
        for (i, row) in rows.iter().enumerate() {
            sqlx::query("INSERT INTO exact_gl VALUES (?,?,?,?,?,?,?)")
                .bind(&id)
                .bind(kind)
                .bind(i as i64)
                .bind(&row.account_code)
                .bind(&row.account_name)
                .bind(&row.category)
                .bind(row.amount.cents())
                .execute(&mut *tx)
                .await?;
        }
    }
    if let Some(s) = &data.snapshot {
        sqlx::query("INSERT INTO exact_snapshots VALUES (?,?,?,?,?)")
            .bind(&id)
            .bind(&s.as_of_date)
            .bind(s.occupied_units)
            .bind(s.vacant_units)
            .bind(s.down_units)
            .execute(&mut *tx)
            .await?;
        for (field, amount) in snapshot_money(s) {
            if let Some(value) = amount {
                sqlx::query("INSERT INTO exact_snapshot_money VALUES (?,?,?)")
                    .bind(&id)
                    .bind(field)
                    .bind(value.cents())
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(id)
}
fn snapshot_money(s: &Snapshot) -> [(&str, Option<Money>); 5] {
    [
        ("market_rent_total", s.market_rent_total),
        ("in_place_rent_total", s.in_place_rent_total),
        ("delinquent_amount", s.delinquent_amount),
        ("prepaid_amount", s.prepaid_amount),
        ("concessions_amount", s.concessions_amount),
    ]
}

pub async fn load(pool: &SqlitePool, id: &str) -> Result<Dataset> {
    let revision = sqlx::query("SELECT * FROM exact_revisions WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| error("NOT_FOUND", "Unknown revision"))?;
    let mut sides = [Vec::new(), Vec::new()];
    for row in sqlx::query("SELECT * FROM exact_gl WHERE revision_id=? ORDER BY kind,ordinal")
        .bind(id)
        .fetch_all(pool)
        .await?
    {
        let index = if row.get::<String, _>("kind") == "actual" {
            0
        } else {
            1
        };
        sides[index].push(Line {
            account_code: row.get("account_code"),
            account_name: row.get("account_name"),
            category: row.get("category"),
            amount: Money::from_cents(row.try_get("amount_cents")?)?,
        });
    }
    let snapshot = if let Some(s) = sqlx::query("SELECT * FROM exact_snapshots WHERE revision_id=?")
        .bind(id)
        .fetch_optional(pool)
        .await?
    {
        let mut money = BTreeMap::new();
        for row in
            sqlx::query("SELECT field,amount_cents FROM exact_snapshot_money WHERE revision_id=?")
                .bind(id)
                .fetch_all(pool)
                .await?
        {
            money.insert(
                row.get::<String, _>("field"),
                Money::from_cents(row.try_get("amount_cents")?)?,
            );
        }
        Some(Snapshot {
            as_of_date: s.get("as_of_date"),
            occupied_units: s.get("occupied_units"),
            vacant_units: s.get("vacant_units"),
            down_units: s.get("down_units"),
            market_rent_total: money.get("market_rent_total").copied(),
            in_place_rent_total: money.get("in_place_rent_total").copied(),
            delinquent_amount: money.get("delinquent_amount").copied(),
            prepaid_amount: money.get("prepaid_amount").copied(),
            concessions_amount: money.get("concessions_amount").copied(),
        })
    } else {
        None
    };
    let [actuals, budgets] = sides;
    Ok(Dataset {
        property: revision.get("property"),
        period: revision.get("period"),
        unit_count: revision.get("unit_count"),
        currency: "USD".into(),
        expense_convention: "positive_costs".into(),
        actuals,
        budgets,
        snapshot,
    })
}

pub async fn review(pool: &SqlitePool, id: &str) -> Result<Value> {
    let data = load(pool, id).await?;
    data.validate()?;
    let row = sqlx::query("SELECT * FROM exact_revisions WHERE id=?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if digest(&serde_json::to_vec(&data)?) != row.get::<String, _>("input_sha256") {
        return Err(error(
            "INTEGRITY_ERROR",
            "Stored period no longer matches its input hash",
        ));
    }
    let key = |l: &Line| l.account_code.clone();
    let actual_keys: std::collections::BTreeSet<_> = data.actuals.iter().map(&key).collect();
    let budget_keys: std::collections::BTreeSet<_> = data.budgets.iter().map(&key).collect();
    let mut excluded = Vec::new();
    for (kind, rows, other) in [
        ("actual", &data.actuals, &budget_keys),
        ("budget", &data.budgets, &actual_keys),
    ] {
        for line in rows.iter().filter(|l| !other.contains(&key(l))) {
            excluded.push(json!({"kind":kind,"line":line,"reason":if kind=="actual" {"missing_budget"} else {"missing_actual"}}));
        }
    }
    let covered_a: Vec<_> = data
        .actuals
        .iter()
        .filter(|l| budget_keys.contains(&key(l)))
        .cloned()
        .collect();
    let covered_b: Vec<_> = data
        .budgets
        .iter()
        .filter(|l| actual_keys.contains(&key(l)))
        .cloned()
        .collect();
    let mut result = variance::compute(&covered_a, &covered_b)?;
    if !excluded.is_empty() {
        result
            .review_reasons
            .push("incomplete_account_coverage".into());
    }
    if data.actuals.is_empty() {
        result.review_reasons.push("missing_actuals".into());
    }
    if data.budgets.is_empty() {
        result.review_reasons.push("missing_budgets".into());
    }
    if data.snapshot.is_none() {
        result
            .review_reasons
            .push("missing_occupancy_snapshot".into());
    }
    if data
        .actuals
        .iter()
        .chain(&data.budgets)
        .any(|l| l.category.trim().eq_ignore_ascii_case("unmapped"))
    {
        result.review_reasons.push("unmapped_accounts".into());
    }
    let parent: Option<String> = row.get("parent_id");
    let mut changes = BTreeMap::new();
    if let Some(previous_id) = &parent {
        let previous = load(pool, previous_id).await?;
        let previous_hash: String =
            sqlx::query_scalar("SELECT input_sha256 FROM exact_revisions WHERE id=?")
                .bind(previous_id)
                .fetch_one(pool)
                .await?;
        if digest(&serde_json::to_vec(&previous)?) != previous_hash {
            return Err(error(
                "INTEGRITY_ERROR",
                "Previous revision no longer matches its input hash",
            ));
        }
        let previous_a: std::collections::BTreeSet<_> = previous.actuals.iter().map(&key).collect();
        let previous_b: std::collections::BTreeSet<_> = previous.budgets.iter().map(&key).collect();
        let a: Vec<_> = previous
            .actuals
            .iter()
            .filter(|l| previous_b.contains(&key(l)))
            .cloned()
            .collect();
        let b: Vec<_> = previous
            .budgets
            .iter()
            .filter(|l| previous_a.contains(&key(l)))
            .cloned()
            .collect();
        let previous = variance::compute(&a, &b)?;
        for (field, current) in &result.noi_bridge {
            changes.insert(field, current.checked_sub(previous.noi_bridge[field])?);
        }
    }
    Ok(
        json!({"contract_version":CONTRACT,"revision_id":id,"property":data.property,"period":data.period,
        "currency":"USD","expense_convention":"positive_costs", "input_sha256":row.get::<String,_>("input_sha256"),
        "sources":serde_json::from_str::<Value>(&row.get::<String,_>("sources"))?,
        "supersedes_revision":parent,"correction_reason":row.get::<Option<String>,_>("reason"),"changes":changes,
        "status":if result.review_reasons.is_empty() {"calculated"} else {"review_required"},
        "variance":result,"bridge_scope":"matched_accounts_only","excluded_accounts":excluded,
        "snapshot":data.snapshot,"unit_count":data.unit_count,"human_approval":"not_granted"}),
    )
}

pub async fn issue(pool: &SqlitePool, id: &str) -> Result<Value> {
    let mut result = review(pool, id).await?;
    let report_id = crate::db::new_id();
    let issued_at = crate::db::now_iso();
    result["report_id"] = json!(report_id);
    result["issued_at"] = json!(issued_at);
    let body = serde_json::to_string(&result)?;
    sqlx::query("INSERT INTO exact_reports VALUES (?,?,?,?,?)")
        .bind(&report_id)
        .bind(id)
        .bind(&body)
        .bind(digest(body.as_bytes()))
        .bind(issued_at)
        .execute(pool)
        .await?;
    Ok(result)
}
