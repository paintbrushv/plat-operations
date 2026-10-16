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
    Row, SqliteConnection, SqlitePool,
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

impl Snapshot {
    /// Rules a snapshot must satisfy on its own: a canonical calendar date and
    /// non-negative counts that reconcile to `unit_count`. [`Dataset::validate`]
    /// adds the check that the date falls inside the dataset's period.
    pub fn validate(&self, unit_count: i64) -> Result<()> {
        let day = NaiveDate::parse_from_str(&self.as_of_date, "%Y-%m-%d")
            .map_err(|_| error("INVALID_INPUT", "Invalid snapshot date"))?;
        if day.format("%Y-%m-%d").to_string() != self.as_of_date {
            return Err(error("INVALID_INPUT", "Use a canonical snapshot date"));
        }
        let counts = [self.occupied_units, self.vacant_units, self.down_units];
        if counts.iter().any(|n| *n < 0 || *n > unit_count)
            || counts.iter().try_fold(0_i64, |sum, n| sum.checked_add(*n)) != Some(unit_count)
        {
            return Err(snapshot_mismatch());
        }
        Ok(())
    }
}

fn snapshot_mismatch() -> super::ExactError {
    error(
        "SNAPSHOT_MISMATCH",
        "Snapshot must cover this period and reconcile to the property unit count",
    )
}

impl Dataset {
    /// Everything a new import must satisfy, including the refusal of control,
    /// invisible format and non-space whitespace characters in the property
    /// and account fields.
    pub fn validate(&self) -> Result<()> {
        self.check(true)
    }

    /// [`Self::validate`] for a revision that is already stored: the same
    /// rules except the character refusal, which applies to new imports only.
    /// A revision written before that rule stays reviewable; `review_period`
    /// flags it with the review reason `invisible_characters` instead.
    pub fn validate_stored(&self) -> Result<()> {
        self.check(false)
    }

    /// Whether the property, or any account code, name or category, holds a
    /// character the import refuses (see [`is_hidden_character`]).
    pub fn has_hidden_characters(&self) -> bool {
        self.property.chars().any(is_hidden_character)
            || self.actuals.iter().chain(&self.budgets).any(|row| {
                [&row.account_code, &row.account_name, &row.category]
                    .iter()
                    .any(|value| value.chars().any(is_hidden_character))
            })
    }

    fn check(&self, characters: bool) -> Result<()> {
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
        if self.property != self.property.trim() {
            return Err(error(
                "INVALID_INPUT",
                "Property has leading or trailing whitespace; remove it before import",
            ));
        }
        let period = crate::db::normalize_period_label(&self.period)
            .map_err(|_| error("INVALID_INPUT", "Invalid YYYY-MM period"))?;
        if period != self.period {
            return Err(error("INVALID_INPUT", "Period must use YYYY-MM"));
        }
        if let Some(s) = &self.snapshot {
            s.validate(self.unit_count)?;
            if !s.as_of_date.starts_with(&format!("{}-", self.period)) {
                return Err(snapshot_mismatch());
            }
        }
        if characters {
            // The property keys the revision chain: a hidden character in it
            // would start a second chain that looks identical to the first.
            if let Some(c) = self.property.chars().find(|c| is_hidden_character(*c)) {
                return Err(error(
                    "INVALID_INPUT",
                    format!(
                        "property contains the invisible character U+{:04X}; remove it from the export before import",
                        c as u32
                    ),
                ));
            }
            refuse_hidden_characters("actuals", &self.actuals)?;
            refuse_hidden_characters("budgets", &self.budgets)?;
        }
        variance::compute(&self.actuals, &self.budgets)?;
        Ok(())
    }
}

/// Unicode format characters (general category Cf), the line and paragraph
/// separators U+2028 and U+2029, and the invisible fillers and variation
/// selectors that render as nothing (combining grapheme joiner, Mongolian free
/// variation selectors, variation selectors 1-16, Hangul fillers, braille
/// blank), as inclusive code point ranges. `char::is_control` covers category
/// Cc only and std has no Cf predicate, so the list is explicit. These
/// characters are invisible in a spreadsheet, so two accounts can look
/// identical and not compare equal (a byte-order mark pasted into a category is
/// the usual way in). Whitespace other than U+0020 is refused separately, in
/// [`is_hidden_character`].
const FORMAT_RANGES: [(u32, u32); 27] = [
    (0x00AD, 0x00AD),
    (0x034F, 0x034F),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x115F, 0x1160),
    (0x180B, 0x180F),
    (0x200B, 0x200F),
    (0x2028, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0x2800, 0x2800),
    (0x3164, 0x3164),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

/// Whether `c` is a control character, an invisible format character, or any
/// whitespace other than the plain space U+0020 (a no-break space, an en or em
/// space, an ideographic space and the line separators look like a space but
/// do not compare equal to one).
pub fn is_hidden_character(c: char) -> bool {
    let code = c as u32;
    c.is_control()
        || (c.is_whitespace() && c != ' ')
        || FORMAT_RANGES
            .iter()
            .any(|(low, high)| (*low..=*high).contains(&code))
}

/// Refuse a control or format character in an account code, name or category,
/// naming the list, row index (0-based), field and code point.
fn refuse_hidden_characters(kind: &str, rows: &[Line]) -> Result<()> {
    for (index, row) in rows.iter().enumerate() {
        for (field, value) in [
            ("account_code", &row.account_code),
            ("account_name", &row.account_name),
            ("category", &row.category),
        ] {
            if let Some(c) = value.chars().find(|c| is_hidden_character(*c)) {
                return Err(error(
                    "INVALID_INPUT",
                    format!(
                        "{kind}[{index}].{field} contains the invisible character U+{:04X}; remove it from the export before import",
                        c as u32
                    ),
                ));
            }
        }
    }
    Ok(())
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
-- One row per catalog invocation, success or refusal. No foreign keys: a refusal
-- may name a revision that does not exist. input_json is NULL unless the host
-- sets BOXSCORE_TRACE_INPUTS=1.
CREATE TABLE exact_invocations (
 trace_id TEXT PRIMARY KEY, function TEXT NOT NULL, version TEXT NOT NULL, producer_version TEXT NOT NULL,
 caller TEXT NOT NULL, input_sha256 TEXT NOT NULL, output_sha256 TEXT, status TEXT NOT NULL, error_code TEXT,
 revision_id TEXT, report_id TEXT, input_json TEXT, started_at TEXT NOT NULL, finished_at TEXT NOT NULL
);
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
        "exact_invocations",
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
    if app == APPLICATION_ID && version == 1 {
        pool.close().await;
        return Err(error(
            "SCHEMA_MISMATCH",
            format!(
                "This database was created by exact-cent schema 1; this build needs schema {SCHEMA}. \
                 Re-import its periods into a new database (boxscore-exact init) or rebuild it by reviewed copy migration from the legacy source"
            ),
        ));
    }
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
    import_validated(pool, data, parent, reason, sources).await
}

/// [`import`] for rows that already exist in a legacy database: every rule
/// except the character refusal (see [`Dataset::validate_stored`]), so a
/// legacy byte-order mark or no-break space does not block a reviewed
/// migration. `review_period` flags the stored revision `invisible_characters`.
pub async fn import_legacy(pool: &SqlitePool, data: &Dataset, sources: &Value) -> Result<String> {
    data.validate_stored()?;
    import_validated(pool, data, None, None, sources).await
}

async fn import_validated(
    pool: &SqlitePool,
    data: &Dataset,
    parent: Option<&str>,
    reason: Option<&str>,
    sources: &Value,
) -> Result<String> {
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
    let mut conn = pool.acquire().await?;
    load_on(&mut conn, id).await
}

async fn load_on(conn: &mut SqliteConnection, id: &str) -> Result<Dataset> {
    let revision = sqlx::query("SELECT * FROM exact_revisions WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| error("NOT_FOUND", "Unknown revision"))?;
    let mut sides = [Vec::new(), Vec::new()];
    for row in sqlx::query("SELECT * FROM exact_gl WHERE revision_id=? ORDER BY kind,ordinal")
        .bind(id)
        .fetch_all(&mut *conn)
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
        .fetch_optional(&mut *conn)
        .await?
    {
        let mut money = BTreeMap::new();
        for row in
            sqlx::query("SELECT field,amount_cents FROM exact_snapshot_money WHERE revision_id=?")
                .bind(id)
                .fetch_all(&mut *conn)
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

/// [`load`], then check the canonical dataset against the revision's stored
/// `input_sha256`. A row changed after import refuses with `INTEGRITY_ERROR`
/// instead of feeding altered numbers to a calculation. Does not run
/// [`Dataset::validate`]; callers do that on data they now know is unaltered.
pub async fn load_verified(pool: &SqlitePool, id: &str) -> Result<Dataset> {
    let mut conn = pool.acquire().await?;
    load_verified_on(&mut conn, id).await
}

async fn load_verified_on(conn: &mut SqliteConnection, id: &str) -> Result<Dataset> {
    let data = load_on(conn, id).await?;
    let expected: String =
        sqlx::query_scalar("SELECT input_sha256 FROM exact_revisions WHERE id=?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    if digest(&serde_json::to_vec(&data)?) != expected {
        return Err(error(
            "INTEGRITY_ERROR",
            "Stored period no longer matches its input hash",
        ));
    }
    Ok(data)
}

pub async fn review(pool: &SqlitePool, id: &str) -> Result<Value> {
    let mut conn = pool.acquire().await?;
    review_on(&mut conn, id).await
}

async fn review_on(conn: &mut SqliteConnection, id: &str) -> Result<Value> {
    let data = load_verified_on(conn, id).await?;
    data.validate_stored()?;
    let row = sqlx::query("SELECT * FROM exact_revisions WHERE id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
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
    // A revision stored before new imports refused these characters is still
    // reviewed, and the reason says it holds some.
    if data.has_hidden_characters() {
        result.review_reasons.push("invisible_characters".into());
    }
    let parent: Option<String> = row.get("parent_id");
    let mut changes = BTreeMap::new();
    if let Some(previous_id) = &parent {
        let previous = load_on(conn, previous_id).await?;
        let previous_hash: String =
            sqlx::query_scalar("SELECT input_sha256 FROM exact_revisions WHERE id=?")
                .bind(previous_id)
                .fetch_one(&mut *conn)
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

/// Refuse a revision that has a successor: some stored revision names it as
/// its `parent_id`. The message names the latest revision of the chain, found
/// by following `parent_id` links forward. An unknown id has no successor and
/// falls through to the review, which refuses it as `NOT_FOUND`.
async fn refuse_superseded(conn: &mut SqliteConnection, id: &str) -> Result<()> {
    let Some(mut latest) = next_revision(conn, id).await? else {
        return Ok(());
    };
    let mut seen = std::collections::BTreeSet::from([id.to_string()]);
    while let Some(next) = next_revision(conn, &latest.0).await? {
        if !seen.insert(latest.0.clone()) {
            return Err(error("INTEGRITY_ERROR", "Revision chain contains a cycle"));
        }
        latest = next;
    }
    let (latest_id, latest_number) = latest;
    let number: i64 = sqlx::query_scalar("SELECT revision FROM exact_revisions WHERE id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    Err(error(
        "SUPERSEDED",
        format!(
            "Revision {id} (revision {number}) is superseded; the latest is {latest_id} (revision {latest_number})"
        ),
    ))
}

/// The revision whose `parent_id` is `id`, with its revision number.
async fn next_revision(conn: &mut SqliteConnection, id: &str) -> Result<Option<(String, i64)>> {
    let row = sqlx::query("SELECT id,revision FROM exact_revisions WHERE parent_id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    row.map(|row| Ok((row.try_get("id")?, row.try_get("revision")?)))
        .transpose()
}

/// Issue a report for the latest revision of a property and period. A
/// superseded revision refuses `SUPERSEDED`; `review_required` revisions still
/// issue, and issuing the latest again makes another report.
///
/// The successor check, the review and the insert run on one connection in an
/// immediate transaction, which takes SQLite's write lock before the check. A
/// correction committed by another connection (another process on the same
/// database file) therefore either lands before the check, and the issue
/// refuses, or waits until the report is stored; it cannot slip in between.
pub async fn issue(pool: &SqlitePool, id: &str) -> Result<Value> {
    // The `Transaction` guard rolls back when dropped, on an error or when the
    // caller's future is cancelled, so no open transaction outlives the call.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let result = issue_in_transaction(&mut tx, id).await?;
    tx.commit().await?;
    Ok(result)
}

async fn issue_in_transaction(conn: &mut SqliteConnection, id: &str) -> Result<Value> {
    refuse_superseded(conn, id).await?;
    let mut result = review_on(conn, id).await?;
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
        .execute(&mut *conn)
        .await?;
    Ok(result)
}

/// Stored revisions with their issued reports, oldest first. Read-only.
pub async fn list_revisions(
    pool: &SqlitePool,
    property: Option<&str>,
    period: Option<&str>,
) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT id,property,period,revision,parent_id,reason,unit_count,input_sha256,sources,created_at \
         FROM exact_revisions WHERE (?1 IS NULL OR property=?1) AND (?2 IS NULL OR period=?2) \
         ORDER BY property,period,revision",
    )
    .bind(property)
    .bind(period)
    .fetch_all(pool)
    .await?;
    let mut revisions = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        let reports = sqlx::query(
            "SELECT id,body_sha256,created_at FROM exact_reports WHERE revision_id=? ORDER BY created_at,id",
        )
        .bind(&id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|report| {
            Ok(json!({
                "id": report.try_get::<String, _>("id")?,
                "body_sha256": report.try_get::<String, _>("body_sha256")?,
                "created_at": report.try_get::<String, _>("created_at")?,
            }))
        })
        .collect::<Result<Vec<Value>>>()?;
        revisions.push(json!({
            "id": id,
            "property": row.try_get::<String, _>("property")?,
            "period": row.try_get::<String, _>("period")?,
            "revision": row.try_get::<i64, _>("revision")?,
            "parent_id": row.try_get::<Option<String>, _>("parent_id")?,
            "reason": row.try_get::<Option<String>, _>("reason")?,
            "unit_count": row.try_get::<i64, _>("unit_count")?,
            "input_sha256": row.try_get::<String, _>("input_sha256")?,
            "sources": serde_json::from_str::<Value>(&row.try_get::<String, _>("sources")?)?,
            "created_at": row.try_get::<String, _>("created_at")?,
            "reports": reports,
        }));
    }
    Ok(revisions)
}
