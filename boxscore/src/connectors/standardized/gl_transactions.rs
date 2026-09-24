use std::collections::HashMap;
use std::fs::File;

use anyhow::{Context, Result};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use sqlx::SqlitePool;
use tracing::info;

use crate::{
    connectors::standardized::{source_registry::PropertyLane, StandardizedIngestSummary},
    db,
};

pub const SOURCE_FILE_NAME: &str = "gl_transactions.parquet";

/// Ingest the `gl_transactions.parquet` file for a single property lane.
///
/// Behavior contract:
/// - Reads columns **by name** from each row; tolerates absent `property_name`.
/// - Converts `period` TIMESTAMP(MICROS) → YYYY-MM.
/// - Converts `date` TIMESTAMP(NANOS or MICROS) → YYYY-MM-DD or NULL.
/// - `is_resident` = 1 when payee matches the pattern `...(t<digits>)` suffix.
/// - Idempotent: DELETE by source_file before inserting, inside one transaction.
/// - Batches inserts in groups of 200 rows.
/// - Emits ONE summary gap when any rows were skipped (not one per row).
/// - `since_period`: if Some, only rows with period >= value are inserted
///   (comparison is lexicographic on YYYY-MM strings, which is correct).
pub async fn ingest_gl_transactions_for_lane(
    pool: &SqlitePool,
    lane: &PropertyLane,
    since_period: Option<&str>,
) -> Result<StandardizedIngestSummary> {
    let path = lane.standardized_path.join(SOURCE_FILE_NAME);
    let source_file = path.to_string_lossy().to_string();

    let task_run_id = db::create_task_run(
        pool,
        "standardized_txn_ingest",
        &format!(
            "Ingest GL transactions for {} from {}",
            lane.display_name,
            path.display()
        ),
    )
    .await?;

    let property_id = db::upsert_property(
        pool,
        &lane.display_name,
        "Unknown",
        i64::from(lane.unit_count_hint.unwrap_or_default()),
        "Example Sponsor",
        "Unknown",
    )
    .await?;

    // --- Read the parquet file (blocking, done on current thread) -----------
    let file = File::open(&path)
        .with_context(|| format!("failed to open GL transactions parquet: {}", path.display()))?;
    let reader = SerializedFileReader::new(file)
        .with_context(|| format!("failed to parse parquet file: {}", path.display()))?;
    let row_iter = reader
        .get_row_iter(None)
        .with_context(|| "failed to create row iterator")?;

    // Collect all parsed rows (and skip-count) before touching the DB so we
    // hold the file lock only while reading, not during the DB transaction.
    struct ParsedRow {
        entity_code: String,
        account_code: String,
        txn_date: Option<String>,
        period: String,
        payee: String,
        is_resident: i64,
        control: Option<String>,
        reference: Option<String>,
        amount: f64,
        remarks: Option<String>,
        source_row: i64,
    }

    let mut parsed_rows: Vec<ParsedRow> = Vec::new();
    let mut rows_skipped: usize = 0;

    for (idx, row_result) in row_iter.enumerate() {
        let source_row = idx as i64 + 1;

        if source_row % 100_000 == 0 {
            info!(
                lane = %lane.property_key,
                rows_read = source_row,
                "GL transactions read progress"
            );
        }

        let row = match row_result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(source_row, err = %e, "failed to read parquet row; skipping");
                rows_skipped += 1;
                continue;
            }
        };

        // Build a name→field map so we can read by name, not position.
        let fields: HashMap<&str, &Field> = row
            .get_column_iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();

        // --- period (required): TIMESTAMP(MICROS) → YYYY-MM ------------------
        // Depending on writer metadata the row API surfaces this as
        // TimestampMicros/TimestampMillis, or as a raw Long. A raw Long is
        // usually microseconds, but some writers emit nanoseconds — if the
        // micro interpretation lands outside the plausible year range, retry
        // as nanos rather than silently skipping the entire file.
        let period_secs = match fields.get("period") {
            Some(Field::TimestampMicros(micros)) => Some(*micros / 1_000_000),
            Some(Field::Long(raw)) => {
                let as_micros = *raw / 1_000_000;
                if epoch_secs_to_ym(as_micros).is_some() {
                    Some(as_micros)
                } else {
                    Some(*raw / 1_000_000_000)
                }
            }
            Some(Field::TimestampMillis(millis)) => Some(*millis / 1_000),
            _ => None,
        };
        let period = match period_secs.and_then(epoch_secs_to_ym) {
            Some(p) => p,
            None => {
                rows_skipped += 1;
                continue;
            }
        };

        // Apply --since filter (lexicographic on YYYY-MM).
        if let Some(since) = since_period {
            if period.as_str() < since {
                continue;
            }
        }

        // --- entity_code (property_id column in source) ----------------------
        let entity_code = match fields.get("property_id") {
            Some(Field::Str(s)) => s.clone(),
            _ => String::new(),
        };

        // --- account_code (required) -----------------------------------------
        let account_code = match fields.get("account_code") {
            Some(Field::Str(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                rows_skipped += 1;
                continue;
            }
        };

        // --- txn_date (optional): TIMESTAMP(NANOS or MICROS) → YYYY-MM-DD ----
        // Nanosecond timestamps have no row-API variant and arrive as raw
        // Long nanos; microsecond ones arrive as TimestampMicros.
        let txn_date_secs = match fields.get("date") {
            Some(Field::Long(nanos)) => Some(*nanos / 1_000_000_000),
            Some(Field::TimestampMicros(micros)) => Some(*micros / 1_000_000),
            Some(Field::TimestampMillis(millis)) => Some(*millis / 1_000),
            _ => None,
        };
        let txn_date = txn_date_secs.and_then(epoch_secs_to_ymd);

        // --- payee / person_description --------------------------------------
        let payee = match fields.get("person_description") {
            Some(Field::Str(s)) => s.clone(),
            _ => String::new(),
        };

        // --- is_resident: payee ends with `(t<digits>)` ----------------------
        let is_resident = if is_resident_payee(&payee) {
            1i64
        } else {
            0i64
        };

        // --- control ---------------------------------------------------------
        let control = match fields.get("control") {
            Some(Field::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };

        // --- reference -------------------------------------------------------
        let reference = match fields.get("reference") {
            Some(Field::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };

        // --- amount (use `amount` column; fall back to debit-credit) ---------
        let amount = match fields.get("amount") {
            Some(Field::Double(v)) if v.is_finite() => *v,
            _ => {
                let debit = match fields.get("debit") {
                    Some(Field::Double(v)) => *v,
                    _ => 0.0,
                };
                let credit = match fields.get("credit") {
                    Some(Field::Double(v)) => *v,
                    _ => 0.0,
                };
                debit - credit
            }
        };

        // --- remarks ---------------------------------------------------------
        let remarks = match fields.get("remarks") {
            Some(Field::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };

        parsed_rows.push(ParsedRow {
            entity_code,
            account_code,
            txn_date,
            period,
            payee,
            is_resident,
            control,
            reference,
            amount,
            remarks,
            source_row,
        });
    }

    info!(
        lane = %lane.property_key,
        rows_parsed = parsed_rows.len(),
        rows_skipped,
        "GL transactions parse complete; starting DB ingest"
    );

    // --- DB transaction: DELETE then batched INSERT -------------------------
    const BATCH_SIZE: usize = 200;
    let rows_to_insert = parsed_rows.len();

    let mut tx = pool.begin().await?;

    // Idempotency: replace previously loaded rows for this file — but only
    // within the --since window when one is given, so a windowed refresh
    // never destroys older history that this run will not re-insert.
    match since_period {
        Some(since) => {
            sqlx::query("DELETE FROM gl_transactions WHERE source_file = ? AND period >= ?")
                .bind(&source_file)
                .bind(since)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM gl_transactions WHERE source_file = ?")
                .bind(&source_file)
                .execute(&mut *tx)
                .await?;
        }
    }

    for (batch_idx, chunk) in parsed_rows.chunks(BATCH_SIZE).enumerate() {
        if batch_idx > 0 && batch_idx % 500 == 0 {
            info!(
                lane = %lane.property_key,
                rows_inserted = batch_idx * BATCH_SIZE,
                "GL transactions insert progress"
            );
        }

        // Build a single multi-VALUES INSERT for the batch.
        // 15 binds per row: id, property_id, entity_code, account_code,
        //   txn_date, period, payee, is_resident, control, reference,
        //   amount, remarks, source_file, source_row, created_at
        let placeholders = chunk
            .iter()
            .map(|_| "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO gl_transactions \
             (id, property_id, entity_code, account_code, txn_date, period, payee, \
              is_resident, control, reference, amount, remarks, source_file, source_row, created_at) \
             VALUES {placeholders}"
        );
        let now = db::now_iso();
        let mut q = sqlx::query(&sql);
        for row in chunk {
            q = q
                .bind(db::new_id())
                .bind(&property_id)
                .bind(&row.entity_code)
                .bind(&row.account_code)
                .bind(&row.txn_date)
                .bind(&row.period)
                .bind(&row.payee)
                .bind(row.is_resident)
                .bind(&row.control)
                .bind(&row.reference)
                .bind(row.amount)
                .bind(&row.remarks)
                .bind(&source_file)
                .bind(row.source_row)
                .bind(&now);
        }
        q.execute(&mut *tx).await?;
    }

    tx.commit().await?;

    info!(
        lane = %lane.property_key,
        rows_inserted = rows_to_insert,
        "GL transactions DB ingest complete"
    );

    // --- Create a single aggregate gap if any rows were skipped -------------
    let mut gaps_created = 0;
    if rows_skipped > 0 {
        db::insert_gap(
            pool,
            &task_run_id,
            "gl_transaction_parse_skip",
            "low",
            &format!(
                "{rows_skipped} GL transaction rows skipped during ingest of {} (unparseable period or missing account code).",
                path.display()
            ),
            "Skipped rows are excluded from the transaction ledger and drill-down.",
            "Re-export the parquet file from the ETL and re-run ingest-standardized transactions.",
        )
        .await?;
        gaps_created = 1;
    }

    db::complete_task_run(
        pool,
        &task_run_id,
        "completed",
        Some(if gaps_created == 0 { 0.90 } else { 0.75 }),
        Some(&format!(
            "Inserted {rows_to_insert} GL transaction rows from {} with {rows_skipped} skipped rows.",
            path.display()
        )),
    )
    .await?;

    Ok(StandardizedIngestSummary {
        lane: lane.property_key.clone(),
        source_file,
        rows_seen: rows_to_insert + rows_skipped,
        rows_inserted: rows_to_insert,
        rows_skipped,
        gaps_created,
    })
}

// ---------------------------------------------------------------------------
// Timestamp helpers
// ---------------------------------------------------------------------------

/// Convert seconds-since-epoch → "YYYY-MM".
/// Returns None for implausible years (far-past sentinels, pre-1990, post-2100).
fn epoch_secs_to_ym(secs: i64) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp(secs, 0)?;
    let year = dt.format("%Y").to_string().parse::<i64>().ok()?;
    if !(1990..=2100).contains(&year) {
        return None;
    }
    Some(dt.format("%Y-%m").to_string())
}

/// Convert seconds-since-epoch → "YYYY-MM-DD".
/// Returns None for implausible years (far-past sentinels, pre-1990, post-2100).
fn epoch_secs_to_ymd(secs: i64) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp(secs, 0)?;
    let year = dt.format("%Y").to_string().parse::<i64>().ok()?;
    if !(1990..=2100).contains(&year) {
        return None;
    }
    Some(dt.format("%Y-%m-%d").to_string())
}

// ---------------------------------------------------------------------------
// Resident payee detection
// ---------------------------------------------------------------------------

/// Returns true when the payee string ends with the pattern `(t<digits>)`.
/// This is Yardi's tenant-ledger marker (e.g. `Holder (t0171778)`).
///
/// Implemented without a regex crate: hand-rolled suffix check.
pub fn is_resident_payee(payee: &str) -> bool {
    // Fast path: must end with ')'
    if !payee.ends_with(')') {
        return false;
    }
    // Find the matching '('
    let Some(open_paren) = payee.rfind('(') else {
        return false;
    };
    let inner = &payee[open_paren + 1..payee.len() - 1]; // between '(' and ')'
                                                         // Must start with 't' followed by at least one digit
    let Some(rest) = inner.strip_prefix('t') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}
