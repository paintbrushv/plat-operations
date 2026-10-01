//! Versioned, deliberately synthetic September PMS export profiles.
//!
//! These profiles describe invented Yardi/ResMan CSV layouts. They are an
//! executable acceptance rehearsal, not connectors for real manager exports.
//! All writes require a `Synthetic ` property and a `SYN-` external ID.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};

use crate::{
    db,
    ontology::{account_class, AccountClass, NOI_CATEGORIES},
};

pub const BOUNDARY_VERSION: &str = "boxscore.synthetic-pms-handoff/1";
pub const YARDI_PROFILE: &str = "yardi.synthetic-gl/1";
pub const RESMAN_PROFILE: &str = "resman.synthetic-gl/1";

const YARDI_HEADERS: [&str; 9] = [
    "property_id",
    "period",
    "posting_date",
    "entry_id",
    "revision",
    "account_code",
    "account_name",
    "debit",
    "credit",
];
const RESMAN_HEADERS: [&str; 9] = [
    "property_code",
    "accounting_month",
    "effective_date",
    "transaction_id",
    "revision_no",
    "gl_account",
    "description",
    "debit_amount",
    "credit_amount",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedProfile {
    pub profile_version: String,
    pub source_namespace: String,
    pub account_categories: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffBoundary {
    pub schema_version: String,
    pub property_name: String,
    pub external_property_id: String,
    pub period: String,
    pub cutover_on: String,
    pub before: FeedProfile,
    pub after: FeedProfile,
}

impl HandoffBoundary {
    pub fn from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read synthetic boundary: {}", path.display()))?;
        let boundary: Self = serde_json::from_str(&text)?;
        boundary.validate()?;
        Ok(boundary)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != BOUNDARY_VERSION {
            bail!("unsupported synthetic boundary version");
        }
        if !self.property_name.starts_with("Synthetic ")
            || !self.external_property_id.starts_with("SYN-")
        {
            bail!("synthetic property name and external ID required");
        }
        let (year, month) = db::parse_period_label(&self.period)?;
        let cutover = parse_date(&self.cutover_on)?;
        if i64::from(cutover.year()) != year || i64::from(cutover.month()) != month {
            bail!("cutover date must fall inside the declared period");
        }
        if self.before.source_namespace == self.after.source_namespace {
            bail!("outgoing and incoming source namespaces must differ");
        }
        for profile in [&self.before, &self.after] {
            if !matches!(
                profile.profile_version.as_str(),
                YARDI_PROFILE | RESMAN_PROFILE
            ) || !profile.source_namespace.starts_with("synthetic/")
                || profile.account_categories.is_empty()
            {
                bail!("unsupported or incomplete synthetic source profile");
            }
            for (account, category) in &profile.account_categories {
                if account.trim().is_empty() || !NOI_CATEGORIES.contains(&category.as_str()) {
                    bail!("synthetic account mapping is missing or unapproved");
                }
            }
        }
        Ok(())
    }

    pub fn feed(&self, side: &str) -> Result<&FeedProfile> {
        match side {
            "before" => Ok(&self.before),
            "after" => Ok(&self.after),
            _ => bail!("feed side must be before or after"),
        }
    }
}

use chrono::Datelike;

#[derive(Debug, Clone)]
struct Posting {
    record_id: String,
    revision: i64,
    effective_date: NaiveDate,
    account_code: String,
    account_name: String,
    amount_cents: i64,
    category: String,
    source_row: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportSummary {
    pub property: String,
    pub period: String,
    pub profile_version: String,
    pub source_namespace: String,
    pub rows_seen: usize,
    pub revisions_accepted: usize,
    pub exact_retries: usize,
    pub net_noi_change_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyntheticClose {
    pub id: String,
    pub property: String,
    pub period: String,
    pub issued_at: String,
    pub issued_actual_noi_cents: i64,
    pub issued_budget_noi_cents: i64,
    pub current_actual_noi_cents: i64,
    pub current_budget_noi_cents: i64,
    pub accepted_revision_count_at_issue: i64,
    pub synthetic_only: bool,
}

fn parse_date(value: &str) -> Result<NaiveDate> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid ISO posting date: {value}"))?;
    if date.format("%Y-%m-%d").to_string() != value {
        bail!("posting date must be canonical YYYY-MM-DD");
    }
    Ok(date)
}

fn parse_cents(value: &str) -> Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(0);
    }
    let (whole, fraction) = value
        .split_once('.')
        .ok_or_else(|| anyhow!("money must have exactly two decimal places"))?;
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() != 2
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        bail!("money must be nonnegative with exactly two decimal places");
    }
    whole
        .parse::<i64>()?
        .checked_mul(100)
        .and_then(|n| n.checked_add(fraction.parse::<i64>().ok()?))
        .ok_or_else(|| anyhow!("money exceeds supported cent range"))
}

fn parse_revision(value: &str) -> Result<i64> {
    let revision = value.trim().parse::<i64>()?;
    if revision < 1 || value.trim() != revision.to_string() {
        bail!("source revision must be a canonical positive integer");
    }
    Ok(revision)
}

struct RawPosting<'a> {
    external_id: &'a str,
    period: &'a str,
    date: &'a str,
    record_id: &'a str,
    revision: &'a str,
    account_code: &'a str,
    account_name: &'a str,
    debit: &'a str,
    credit: &'a str,
}

fn parse_posting(
    boundary: &HandoffBoundary,
    side: &str,
    source_row: i64,
    raw: RawPosting<'_>,
) -> Result<Posting> {
    let RawPosting {
        external_id,
        period,
        date,
        record_id,
        revision,
        account_code,
        account_name,
        debit,
        credit,
    } = raw;
    if external_id.trim() != boundary.external_property_id || period.trim() != boundary.period {
        bail!("source property or accounting period does not match boundary");
    }
    let effective_date = parse_date(date.trim())?;
    if effective_date.format("%Y-%m").to_string() != boundary.period {
        bail!("posting date and accounting period differ");
    }
    let cutover = parse_date(&boundary.cutover_on)?;
    if (effective_date < cutover) != (side == "before") {
        bail!("source feed is not authoritative for posting date");
    }
    let feed = boundary.feed(side)?;
    let account_code = account_code.trim();
    let category = feed
        .account_categories
        .get(account_code)
        .ok_or_else(|| anyhow!("unmapped source account: {account_code}"))?;
    if record_id.trim().is_empty() || account_name.trim().is_empty() {
        bail!("source record ID and account name are required");
    }
    let debit = parse_cents(debit)?;
    let credit = parse_cents(credit)?;
    if (debit == 0) == (credit == 0) {
        bail!("exactly one of debit or credit must be positive");
    }
    if matches!(account_class(category), AccountClass::Unmapped) {
        bail!("unmapped account cannot enter NOI");
    }
    // This versioned Boxscore bridge stores revenue and costs as positive
    // natural amounts, matching compute_noi_bridge's current convention.
    let amount_cents = match account_class(category) {
        AccountClass::Revenue => credit - debit,
        AccountClass::Expense => debit - credit,
        AccountClass::Unmapped => unreachable!(),
    };
    Ok(Posting {
        record_id: record_id.trim().to_string(),
        revision: parse_revision(revision)?,
        effective_date,
        account_code: account_code.to_string(),
        account_name: account_name.trim().to_string(),
        amount_cents,
        category: category.clone(),
        source_row,
    })
}

fn read_postings(boundary: &HandoffBoundary, side: &str, path: &Path) -> Result<Vec<Posting>> {
    let feed = boundary.feed(side)?;
    let mut reader = csv::ReaderBuilder::new()
        .flexible(false)
        .from_path(path)
        .with_context(|| format!("cannot read synthetic PMS CSV: {}", path.display()))?;
    let expected = match feed.profile_version.as_str() {
        YARDI_PROFILE => &YARDI_HEADERS[..],
        RESMAN_PROFILE => &RESMAN_HEADERS[..],
        _ => bail!("unsupported synthetic PMS adapter version"),
    };
    if reader.headers()?.iter().collect::<Vec<_>>() != expected {
        bail!("CSV headers do not match pinned synthetic PMS adapter version");
    }
    let mut out = Vec::new();
    for (index, row) in reader.records().enumerate() {
        let row = row?;
        let field = |i| row.get(i).unwrap_or("");
        out.push(
            parse_posting(
                boundary,
                side,
                index as i64 + 2,
                RawPosting {
                    external_id: field(0),
                    period: field(1),
                    date: field(2),
                    record_id: field(3),
                    revision: field(4),
                    account_code: field(5),
                    account_name: field(6),
                    debit: field(7),
                    credit: field(8),
                },
            )
            .with_context(|| format!("invalid synthetic PMS source row {}", index + 2))?,
        );
    }
    if out.is_empty() {
        bail!("synthetic PMS export is empty");
    }
    Ok(out)
}

fn same_source_identity(row: &SqliteRow, feed: &FeedProfile, posting: &Posting) -> bool {
    row.get::<String, _>("profile_version") == feed.profile_version
        && row.get::<String, _>("effective_date") == posting.effective_date.to_string()
        && row.get::<String, _>("account_code") == posting.account_code
        && row.get::<String, _>("account_name") == posting.account_name
        && row.get::<String, _>("category") == posting.category
}

pub async fn import_file(
    pool: &SqlitePool,
    boundary: &HandoffBoundary,
    side: &str,
    path: &Path,
) -> Result<ImportSummary> {
    boundary.validate()?;
    let feed = boundary.feed(side)?;
    let postings = read_postings(boundary, side, path)?;
    let property_id = db::upsert_property(
        pool,
        &boundary.property_name,
        "Synthetic",
        0,
        "Synthetic rehearsal",
        "Synthetic handoff",
    )
    .await?;
    let period_id = db::upsert_period(pool, &boundary.period).await?;
    let mut tx = pool.begin().await?;
    let boundary_json = serde_json::to_string(boundary)?;
    let pinned: Option<String> = sqlx::query_scalar(
        "SELECT boundary_json FROM pms_handoff_boundaries WHERE property_id = ? AND period_id = ?",
    )
    .bind(&property_id)
    .bind(&period_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(pinned) = pinned {
        if pinned != boundary_json {
            bail!("synthetic handoff boundary differs from pinned property-period profile");
        }
    } else {
        sqlx::query(
            "INSERT INTO pms_handoff_boundaries (property_id, period_id, boundary_json, registered_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&property_id).bind(&period_id).bind(&boundary_json).bind(db::now_iso())
        .execute(&mut *tx).await?;
    }
    let mut accepted = 0;
    let mut retries = 0;
    let mut net_noi_change = 0_i64;
    for posting in &postings {
        let exact = sqlx::query(
            "SELECT profile_version, effective_date, account_code, account_name, category, amount_cents \
             FROM pms_source_revisions WHERE property_id = ? AND period_id = ? AND source_namespace = ? \
             AND source_record_id = ? AND revision = ?",
        )
        .bind(&property_id).bind(&period_id).bind(&feed.source_namespace).bind(&posting.record_id)
        .bind(posting.revision).fetch_optional(&mut *tx).await?;
        if let Some(exact) = exact {
            if same_source_identity(&exact, feed, posting)
                && exact.get::<i64, _>("amount_cents") == posting.amount_cents
            {
                retries += 1;
                continue;
            }
            bail!("source revision already accepted with different content");
        }
        let latest = sqlx::query(
            "SELECT profile_version, revision, effective_date, account_code, account_name, category, amount_cents \
             FROM pms_source_revisions WHERE property_id = ? AND period_id = ? AND source_namespace = ? AND source_record_id = ? \
             ORDER BY revision DESC LIMIT 1",
        )
        .bind(&property_id).bind(&period_id).bind(&feed.source_namespace).bind(&posting.record_id)
        .fetch_optional(&mut *tx).await?;
        let previous_amount = if let Some(previous) = latest {
            let prior_revision: i64 = previous.get("revision");
            if !same_source_identity(&previous, feed, posting)
                || posting.revision != prior_revision + 1
            {
                bail!("correction must advance one revision of the same source fact");
            }
            previous.get::<i64, _>("amount_cents")
        } else {
            if posting.revision != 1 {
                bail!("first source revision must be 1");
            }
            0
        };
        let delta = posting
            .amount_cents
            .checked_sub(previous_amount)
            .ok_or_else(|| anyhow!("correction delta exceeds supported cent range"))?;
        let gl_id = db::new_id();
        let delta_real = delta as f64 / 100.0;
        if cents_from_real(delta_real)? != delta {
            bail!("source amount loses cents in Boxscore REAL storage");
        }
        sqlx::query(
            "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&gl_id).bind(&property_id).bind(&period_id).bind(&posting.account_code)
        .bind(&posting.account_name).bind(&posting.category).bind(delta_real)
        .bind(path.to_string_lossy().as_ref()).bind(posting.source_row).bind(db::now_iso())
        .execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO pms_source_revisions (id, property_id, period_id, profile_version, source_namespace, source_record_id, revision, effective_date, account_code, account_name, category, amount_cents, source_file, source_row, accepted_at, gl_actual_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(db::new_id()).bind(&property_id).bind(&period_id).bind(&feed.profile_version)
        .bind(&feed.source_namespace).bind(&posting.record_id).bind(posting.revision)
        .bind(posting.effective_date.to_string()).bind(&posting.account_code)
        .bind(&posting.account_name).bind(&posting.category).bind(posting.amount_cents)
        .bind(path.to_string_lossy().as_ref()).bind(posting.source_row).bind(db::now_iso())
        .bind(&gl_id).execute(&mut *tx).await?;
        accepted += 1;
        let noi_delta = match account_class(&posting.category) {
            AccountClass::Revenue => delta,
            AccountClass::Expense => delta
                .checked_neg()
                .ok_or_else(|| anyhow!("expense correction exceeds supported cent range"))?,
            AccountClass::Unmapped => unreachable!(),
        };
        net_noi_change = net_noi_change
            .checked_add(noi_delta)
            .ok_or_else(|| anyhow!("NOI delta exceeds supported cent range"))?;
    }
    tx.commit().await?;
    Ok(ImportSummary {
        property: boundary.property_name.clone(),
        period: boundary.period.clone(),
        profile_version: feed.profile_version.clone(),
        source_namespace: feed.source_namespace.clone(),
        rows_seen: postings.len(),
        revisions_accepted: accepted,
        exact_retries: retries,
        net_noi_change_cents: net_noi_change,
    })
}

fn cents_from_real(value: f64) -> Result<i64> {
    let scaled = value * 100.0;
    if !scaled.is_finite()
        || (scaled - scaled.round()).abs() > 0.0001
        || scaled < i64::MIN as f64
        || scaled > i64::MAX as f64
    {
        bail!("stored Boxscore amount is not exact cents");
    }
    Ok(scaled.round() as i64)
}

async fn noi_cents<'e, E>(executor: E, table: &str, property_id: &str, period: &str) -> Result<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    if !matches!(table, "gl_actuals" | "gl_budgets") {
        bail!("unsupported NOI table");
    }
    let rows = sqlx::query(&format!(
        "SELECT g.category, SUM(g.amount) AS amount FROM {table} g \
         JOIN periods p ON p.id = g.period_id \
         WHERE g.property_id = ? AND p.label = ? GROUP BY g.category",
    ))
    .bind(property_id)
    .bind(period)
    .fetch_all(executor)
    .await?;
    if rows.is_empty() {
        bail!("period has no {table} rows");
    }
    let mut total = 0_i64;
    for row in rows {
        let category: String = row.get("category");
        let amount = cents_from_real(row.get::<f64, _>("amount"))?;
        let noi_amount = match account_class(&category) {
            AccountClass::Revenue => amount,
            AccountClass::Expense if amount >= 0 => -amount,
            AccountClass::Expense => {
                bail!("negative expense total conflicts with synthetic adapter sign contract")
            }
            AccountClass::Unmapped => continue,
        };
        total = total
            .checked_add(noi_amount)
            .ok_or_else(|| anyhow!("NOI exceeds cent range"))?;
    }
    Ok(total)
}

pub async fn seal_synthetic_close(
    pool: &SqlitePool,
    boundary: &HandoffBoundary,
) -> Result<SyntheticClose> {
    boundary.validate()?;
    let (year, month) = db::parse_period_label(&boundary.period)?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_day_after_period = NaiveDate::from_ymd_opt(next_year as i32, next_month as u32, 1)
        .ok_or_else(|| anyhow!("invalid period end"))?;
    if Utc::now().date_naive() < first_day_after_period {
        bail!("synthetic period cannot be sealed before month end");
    }
    let property = db::require_property_by_name(pool, &boundary.property_name).await?;
    let period = db::period_by_label(pool, &boundary.period)
        .await?
        .ok_or_else(|| anyhow!("period has not been imported"))?;
    let mut tx = pool.begin().await?;
    let boundary_json = serde_json::to_string(boundary)?;
    let pinned: Option<String> = sqlx::query_scalar(
        "SELECT boundary_json FROM pms_handoff_boundaries WHERE property_id = ? AND period_id = ?",
    )
    .bind(&property.id)
    .bind(&period.id)
    .fetch_optional(&mut *tx)
    .await?;
    if pinned.as_deref() != Some(boundary_json.as_str()) {
        bail!("synthetic handoff boundary differs from pinned property-period profile");
    }
    let counts: Vec<i64> = {
        let mut counts = Vec::new();
        for namespace in [
            &boundary.before.source_namespace,
            &boundary.after.source_namespace,
        ] {
            counts.push(sqlx::query_scalar(
                "SELECT COUNT(*) FROM pms_source_revisions WHERE property_id = ? AND period_id = ? AND source_namespace = ?",
            ).bind(&property.id).bind(&period.id).bind(namespace).fetch_one(&mut *tx).await?);
        }
        counts
    };
    if counts.contains(&0) {
        bail!("both manager feeds must be represented before synthetic close");
    }
    let unmatched_actuals: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM gl_actuals g LEFT JOIN pms_source_revisions r ON r.gl_actual_id = g.id \
         WHERE g.property_id = ? AND g.period_id = ? AND r.id IS NULL",
    )
    .bind(&property.id)
    .bind(&period.id)
    .fetch_one(&mut *tx)
    .await?;
    if unmatched_actuals != 0 {
        bail!("synthetic close contains GL actuals outside accepted PMS revisions");
    }
    let actual = noi_cents(&mut *tx, "gl_actuals", &property.id, &boundary.period).await?;
    let budget = noi_cents(&mut *tx, "gl_budgets", &property.id, &boundary.period).await?;
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO pms_synthetic_closes (id, property_id, period_id, issued_at, issued_actual_noi_cents, issued_budget_noi_cents, before_namespace, after_namespace, accepted_revision_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id).bind(&property.id).bind(&period.id).bind(db::now_iso())
    .bind(actual).bind(budget).bind(&boundary.before.source_namespace)
    .bind(&boundary.after.source_namespace).bind(counts.iter().sum::<i64>())
    .execute(&mut *tx).await?;
    tx.commit().await?;
    synthetic_close(pool, &boundary.property_name, &boundary.period).await
}

pub async fn synthetic_close(
    pool: &SqlitePool,
    property_name: &str,
    period: &str,
) -> Result<SyntheticClose> {
    let property = db::require_property_by_name(pool, property_name).await?;
    if !property_name.starts_with("Synthetic ") {
        bail!("synthetic close requires a synthetic property");
    }
    let row = sqlx::query(
        "SELECT c.id, c.issued_at, c.issued_actual_noi_cents, c.issued_budget_noi_cents, c.accepted_revision_count \
         FROM pms_synthetic_closes c JOIN periods p ON p.id = c.period_id \
         WHERE c.property_id = ? AND p.label = ?",
    ).bind(&property.id).bind(period).fetch_one(pool).await?;
    Ok(SyntheticClose {
        id: row.get("id"),
        property: property_name.to_string(),
        period: period.to_string(),
        issued_at: row.get("issued_at"),
        issued_actual_noi_cents: row.get("issued_actual_noi_cents"),
        issued_budget_noi_cents: row.get("issued_budget_noi_cents"),
        current_actual_noi_cents: noi_cents(pool, "gl_actuals", &property.id, period).await?,
        current_budget_noi_cents: noi_cents(pool, "gl_budgets", &property.id, period).await?,
        accepted_revision_count_at_issue: row.get("accepted_revision_count"),
        synthetic_only: true,
    })
}
