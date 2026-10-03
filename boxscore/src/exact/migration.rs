//! Reviewed copy migration. Originals and historical issued bodies are never rewritten.
use super::{
    digest, error,
    money::Money,
    store::{self, Dataset, Snapshot},
    variance::Line,
    Result, APPLICATION_ID,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub contract_version: String,
    pub source_sha256: String,
    pub reviewer: String,
    pub reviewed_at: String,
    pub rounding: String,
    pub acknowledge_negative_expenses: bool,
    pub acknowledge_snapshot_selection: bool,
    pub expense_convention: String,
}
pub fn file_digest(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut input = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn quiescent(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        if std::fs::metadata(&name).is_ok_and(|m| m.len() > 0) {
            return Err(error(
                "SOURCE_ACTIVE",
                "Close and checkpoint the legacy writer before copy migration",
            ));
        }
    }
    Ok(())
}
async fn legacy(path: &Path) -> Result<SqlitePool> {
    quiescent(path)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .create_if_missing(false),
        )
        .await?;
    let app: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&pool)
        .await?;
    if app == APPLICATION_ID {
        return Err(error("SCHEMA_MISMATCH", "Source already uses exact cents"));
    }
    Ok(pool)
}
async fn exists(pool: &SqlitePool, table: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
    )
    .bind(table)
    .fetch_one(pool)
    .await?
        > 0)
}

fn convert(
    value: f64,
    table: &str,
    id: &str,
    field: &str,
    differences: &mut Vec<Value>,
) -> Result<Money> {
    if !value.is_finite() || value.abs() >= 70_368_744_177_664.0 {
        return Err(error(
            "AMBIGUOUS_LEGACY_MONEY",
            "Legacy float lacks cent precision or is non-finite; reconcile original sources first",
        ));
    }
    let raw = value.to_string();
    if let Ok(m) = raw.parse::<Money>() {
        return Ok(m);
    }
    let unsigned = raw.trim_start_matches('-');
    let (whole, fraction) = unsigned.split_once('.').ok_or_else(|| {
        error(
            "AMBIGUOUS_LEGACY_MONEY",
            "Unsupported legacy decimal representation",
        )
    })?;
    if fraction.len() < 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error(
            "AMBIGUOUS_LEGACY_MONEY",
            "Unsupported legacy decimal representation",
        ));
    }
    let mut rounded: Money = format!(
        "{}{whole}.{}",
        if value < 0.0 { "-" } else { "" },
        &fraction[..2]
    )
    .parse()?;
    if fraction.as_bytes()[2] >= b'5' {
        rounded = rounded.checked_add(if value < 0.0 { "-0.01" } else { "0.01" }.parse()?)?;
    }
    differences.push(json!({"kind":"rounding","table":table,"id":id,"field":field,"before":raw,"after":rounded,"transformation":"half_away_from_zero_to_cents"}));
    Ok(rounded)
}
async fn gl(
    pool: &SqlitePool,
    table: &str,
    property_id: &str,
    period_id: &str,
    differences: &mut Vec<Value>,
) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for row in sqlx::query(&format!("SELECT id,account_code,account_name,category,amount FROM {table} WHERE property_id=? AND period_id=? ORDER BY created_at,id"))
        .bind(property_id).bind(period_id).fetch_all(pool).await? {
        let id:String=row.try_get("id")?;
        let amount=convert(row.try_get("amount")?,table,&id,"amount",differences)?;
        let category:String=row.try_get("category")?;
        if amount.cents()<0 && matches!(crate::ontology::account_class(&category),crate::ontology::AccountClass::Expense) {
            differences.push(json!({"kind":"negative_expense","table":table,"id":id,"amount":amount,"action":"preserved_without_sign_flip"}));
        }
        lines.push(Line {account_code:row.try_get("account_code")?,account_name:row.try_get("account_name")?,category,amount});
    }
    Ok(lines)
}
async fn latest(
    pool: &SqlitePool,
    table: &str,
    property_id: &str,
    period: &str,
    differences: &mut Vec<Value>,
) -> Result<Option<sqlx::sqlite::SqliteRow>> {
    if !exists(pool, table).await? {
        return Ok(None);
    }
    let mut rows=sqlx::query(&format!("SELECT * FROM {table} WHERE property_id=? AND substr(as_of_date,1,7)=? ORDER BY as_of_date DESC,created_at DESC,id DESC"))
        .bind(property_id).bind(period).fetch_all(pool).await?;
    if rows.len() > 1 {
        differences.push(json!({"kind":"snapshot_selection","table":table,"property_id":property_id,"period":period,
            "selected_id":rows[0].get::<String,_>("id"),"rows_preserved_in_archive":rows.len(),"rule":"latest_as_of_then_created_at_then_id"}));
    }
    Ok(if rows.is_empty() {
        None
    } else {
        Some(rows.remove(0))
    })
}
fn snapshot_value(
    row: &sqlx::sqlite::SqliteRow,
    table: &str,
    field: &str,
    differences: &mut Vec<Value>,
) -> Result<Option<Money>> {
    let id: String = row.try_get("id")?;
    Ok(Some(convert(
        row.try_get(field)?,
        table,
        &id,
        field,
        differences,
    )?))
}
async fn scan(pool: &SqlitePool) -> Result<(Vec<Dataset>, Vec<Value>, BTreeMap<String, i64>)> {
    let mut differences = Vec::new();
    let mut datasets = Vec::new();
    let mut counts = BTreeMap::new();
    for row in sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(pool).await? {
        let name:String=row.try_get("name")?;
        // Quote names from the schema; never interpret them as SQL fragments.
        let count:i64=sqlx::query_scalar(&format!("SELECT count(*) FROM \"{}\"",name.replace('"',"\"\""))).fetch_one(pool).await?;
        counts.insert(name,count);
    }
    let keys=sqlx::query("SELECT DISTINCT p.id AS property_id,p.name,p.unit_count,d.id AS period_id,d.label FROM properties p JOIN (SELECT property_id,period_id FROM gl_actuals UNION SELECT property_id,period_id FROM gl_budgets) g ON g.property_id=p.id JOIN periods d ON d.id=g.period_id ORDER BY p.name,d.label").fetch_all(pool).await?;
    for key in keys {
        let property_id: String = key.try_get("property_id")?;
        let period_id: String = key.try_get("period_id")?;
        let period: String = key.try_get("label")?;
        let actuals = gl(
            pool,
            "gl_actuals",
            &property_id,
            &period_id,
            &mut differences,
        )
        .await?;
        let budgets = gl(
            pool,
            "gl_budgets",
            &property_id,
            &period_id,
            &mut differences,
        )
        .await?;
        let snapshot = if let Some(row) = latest(
            pool,
            "rent_roll_snapshots",
            &property_id,
            &period,
            &mut differences,
        )
        .await?
        {
            let delinquency = latest(
                pool,
                "delinquency_snapshots",
                &property_id,
                &period,
                &mut differences,
            )
            .await?;
            let leasing = latest(
                pool,
                "leasing_snapshots",
                &property_id,
                &period,
                &mut differences,
            )
            .await?;
            Some(Snapshot {
                as_of_date: row.try_get("as_of_date")?,
                occupied_units: row.try_get("occupied_units")?,
                vacant_units: row.try_get("vacant_units")?,
                down_units: row.try_get("down_units")?,
                market_rent_total: snapshot_value(
                    &row,
                    "rent_roll_snapshots",
                    "market_rent_total",
                    &mut differences,
                )?,
                in_place_rent_total: snapshot_value(
                    &row,
                    "rent_roll_snapshots",
                    "in_place_rent_total",
                    &mut differences,
                )?,
                delinquent_amount: delinquency
                    .as_ref()
                    .map(|r| {
                        snapshot_value(
                            r,
                            "delinquency_snapshots",
                            "delinquent_amount",
                            &mut differences,
                        )
                    })
                    .transpose()?
                    .flatten(),
                prepaid_amount: delinquency
                    .as_ref()
                    .map(|r| {
                        snapshot_value(
                            r,
                            "delinquency_snapshots",
                            "prepaid_amount",
                            &mut differences,
                        )
                    })
                    .transpose()?
                    .flatten(),
                concessions_amount: leasing
                    .as_ref()
                    .map(|r| {
                        snapshot_value(
                            r,
                            "leasing_snapshots",
                            "concessions_amount",
                            &mut differences,
                        )
                    })
                    .transpose()?
                    .flatten(),
            })
        } else {
            None
        };
        let data = Dataset {
            property: property_id,
            period,
            currency: "USD".into(),
            expense_convention: "positive_costs".into(),
            unit_count: key.try_get("unit_count")?,
            actuals,
            budgets,
            snapshot,
        };
        data.validate()?;
        datasets.push(data);
    }
    let actual_count: usize = datasets.iter().map(|d| d.actuals.len()).sum();
    let budget_count: usize = datasets.iter().map(|d| d.budgets.len()).sum();
    if actual_count as i64 != *counts.get("gl_actuals").unwrap_or(&0)
        || budget_count as i64 != *counts.get("gl_budgets").unwrap_or(&0)
    {
        return Err(error(
            "ORPHANED_GL",
            "Some GL rows lack a valid property or period; reconcile before migration",
        ));
    }
    Ok((datasets, differences, counts))
}
pub async fn plan(source: &Path) -> Result<Value> {
    let before = file_digest(source)?;
    let pool = legacy(source).await?;
    let (datasets, differences, counts) = scan(&pool).await?;
    pool.close().await;
    quiescent(source)?;
    if file_digest(source)? != before {
        return Err(error(
            "SOURCE_CHANGED",
            "Source changed during migration planning",
        ));
    }
    Ok(
        json!({"contract_version":"plat.ops-migration-plan/1","source_sha256":before,"periods":datasets.len(),"differences":differences,"source_table_counts":counts,
        "status":"review_required","destination":"new_copy_only","snapshot_rule":"latest_in_each_GL_period",
        "excluded_legacy_features":["transactions","collections analytics","unit leases and receivables","turn costs","unit P&L","calls and predictions","legacy T12 and dashboards"],
        "archive":"Full original database copied unchanged; issued report bodies also preserved in exact archive table"}),
    )
}
pub async fn migrate(source: &Path, destination: &Path, review: &Review) -> Result<Value> {
    if review.contract_version != "plat.ops-migration-review/1"
        || review.reviewer.trim().is_empty()
        || review.reviewer.len() > 200
        || !["reject", "half_away_from_zero"].contains(&review.rounding.as_str())
        || review.expense_convention != "positive_costs"
        || chrono::NaiveDate::parse_from_str(&review.reviewed_at, "%Y-%m-%d").is_err()
    {
        return Err(error(
            "REVIEW_REQUIRED",
            "Supply the supported migration review, reviewer, date, and explicit money convention",
        ));
    }
    let planned = plan(source).await?;
    if planned["source_sha256"] != review.source_sha256 {
        return Err(error(
            "STALE_REVIEW",
            "Review does not match the current source bytes",
        ));
    }
    for difference in planned["differences"].as_array().unwrap() {
        match difference["kind"].as_str().unwrap() {
            "rounding" if review.rounding == "reject" => {
                return Err(error(
                    "REVIEW_REQUIRED",
                    "Explicit rounding review required",
                ))
            }
            "negative_expense" if !review.acknowledge_negative_expenses => {
                return Err(error(
                    "REVIEW_REQUIRED",
                    "Negative expense signs require review; no signs will be flipped",
                ))
            }
            "snapshot_selection" if !review.acknowledge_snapshot_selection => {
                return Err(error(
                    "REVIEW_REQUIRED",
                    "Snapshot selection requires review",
                ))
            }
            _ => (),
        }
    }
    std::fs::create_dir(destination).map_err(|_| {
        error(
            "DESTINATION_EXISTS",
            "Migration requires a new destination directory",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    }
    let archive = destination.join("legacy.sqlite");
    std::fs::copy(source, &archive)?;
    if file_digest(&archive)? != review.source_sha256 {
        return Err(error(
            "SOURCE_CHANGED",
            "Copy differs from reviewed source; incomplete destination retained for inspection",
        ));
    }
    let original = legacy(&archive).await?;
    let (datasets, differences, _) = scan(&original).await?;
    let target = store::create(&destination.join("exact.sqlite")).await?;
    // Incomplete copies cannot be opened as released exact databases.
    sqlx::query("PRAGMA user_version=0")
        .execute(&target)
        .await?;
    let mut revisions = Vec::new();
    for data in datasets {
        revisions.push(
            store::import(
                &target,
                &data,
                None,
                None,
                &json!({"legacy_database_sha256":review.source_sha256,"migration_review":review}),
            )
            .await?,
        );
    }
    if exists(&original, "variance_report_artifacts").await? {
        for row in sqlx::query(
            "SELECT task_run_id,report_markdown,report_path FROM variance_report_artifacts",
        )
        .fetch_all(&original)
        .await?
        {
            let body: String = row.try_get("report_markdown")?;
            sqlx::query("INSERT INTO archived_reports VALUES (?,?,?,?)")
                .bind(row.try_get::<String, _>("task_run_id")?)
                .bind(&body)
                .bind(row.try_get::<String, _>("report_path")?)
                .bind(digest(body.as_bytes()))
                .execute(&target)
                .await?;
        }
    }
    sqlx::query("INSERT INTO exact_migrations VALUES (?,?,?)")
        .bind(&review.source_sha256)
        .bind(serde_json::to_string(review)?)
        .bind(serde_json::to_string(&differences)?)
        .execute(&target)
        .await?;
    original.close().await;
    let mut permissions = std::fs::metadata(&archive)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&archive, permissions)?;
    quiescent(source)?;
    if file_digest(source)? != review.source_sha256 {
        return Err(error(
            "SOURCE_CHANGED",
            "Original changed externally during migration",
        ));
    }
    let result = json!({"contract_version":"plat.ops-migration/1","status":"copied","source_sha256":review.source_sha256,
        "original_unchanged":true,"review":review,"plan":planned,"revisions":revisions});
    std::fs::write(
        destination.join("migration-report.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    sqlx::query(&format!("PRAGMA user_version={}", super::SCHEMA))
        .execute(&target)
        .await?;
    target.close().await;
    Ok(result)
}
