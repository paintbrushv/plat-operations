use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

use crate::{
    connectors::standardized::{
        account_mapping::suggest_category, source_registry::built_in_lanes,
    },
    db::{self, NewAccountMapping},
    models::AccountMapping,
};

const REVIEW_SOURCE_SYSTEM: &str = "standardized-yardi";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountReviewRow {
    pub source_system: String,
    pub property_scope: String,
    pub account_code: String,
    pub account_name: String,
    pub current_category: String,
    pub suggested_category: String,
    pub confidence_score: f64,
    pub status: String,
    pub reviewed_category: String,
    pub review_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountReviewExportSummary {
    pub file: PathBuf,
    pub rows_exported: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountReviewImportSummary {
    pub file: PathBuf,
    pub rows_seen: usize,
    pub rows_imported: usize,
    pub rows_skipped: usize,
}

#[derive(Debug, Error, PartialEq)]
pub enum AccountReviewImportError {
    #[error("approved account review row {source_row} is missing reviewed_category")]
    ApprovedMissingReviewedCategory { source_row: i64 },
    #[error("approved account review row {source_row} is missing {field}")]
    ApprovedMissingRequiredField {
        source_row: i64,
        field: &'static str,
    },
}

pub async fn export_account_mapping_review(
    pool: &SqlitePool,
    file: &Path,
) -> Result<AccountReviewExportSummary> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create review directory: {}", parent.display()))?;
    }

    let mappings = db::list_unapproved_mappings(pool).await?;

    let mut writer = csv::Writer::from_path(file)
        .with_context(|| format!("failed to create account review CSV: {}", file.display()))?;

    for mapping in &mappings {
        writer.serialize(review_row_from_mapping(mapping))?;
    }
    writer.flush()?;

    Ok(AccountReviewExportSummary {
        file: file.to_path_buf(),
        rows_exported: mappings.len(),
    })
}

pub async fn import_account_mapping_review(
    pool: &SqlitePool,
    file: &Path,
) -> Result<AccountReviewImportSummary> {
    let mut reader = crate::parse::csv_reader_from_path(file)
        .with_context(|| format!("failed to open account review CSV: {}", file.display()))?;

    // Two passes: parse and validate the whole file before applying anything,
    // so a malformed row mid-file cannot leave the database half-imported.
    let mut rows = Vec::new();
    for (index, row) in reader.deserialize::<AccountReviewRow>().enumerate() {
        let source_row = index as i64 + 2;
        let row =
            row.with_context(|| format!("failed to parse account review row {source_row}"))?;
        if row.status.trim().eq_ignore_ascii_case("approved") {
            validate_approved_row(&row, source_row)?;
        }
        rows.push(row);
    }

    let mut rows_seen = 0;
    let mut rows_imported = 0;
    let mut rows_skipped = 0;
    for row in &rows {
        rows_seen += 1;
        if !row.status.trim().eq_ignore_ascii_case("approved") {
            rows_skipped += 1;
            continue;
        }

        approve_mapping(
            pool,
            MappingApproval {
                source_system: &row.source_system,
                property_scope: &row.property_scope,
                account_code: &row.account_code,
                account_name: &row.account_name,
                reviewed_category: &row.reviewed_category,
                review_notes: &row.review_notes,
            },
        )
        .await?;
        rows_imported += 1;
    }

    Ok(AccountReviewImportSummary {
        file: file.to_path_buf(),
        rows_seen,
        rows_imported,
        rows_skipped,
    })
}

/// One approval action: the unit shared by the CSV import workflow and the
/// Boxscore TUI's inline mapping review. Upserts the approved mapping,
/// reclassifies the scoped property's Unmapped GL rows, and records the
/// decision as a durable memory.
#[derive(Debug, Clone)]
pub struct MappingApproval<'a> {
    pub source_system: &'a str,
    pub property_scope: &'a str,
    pub account_code: &'a str,
    pub account_name: &'a str,
    pub reviewed_category: &'a str,
    pub review_notes: &'a str,
}

pub async fn approve_mapping(pool: &SqlitePool, approval: MappingApproval<'_>) -> Result<()> {
    let reviewed_category = approval.reviewed_category.trim();
    if reviewed_category.is_empty() {
        return Err(anyhow!("reviewed category must not be empty"));
    }
    db::upsert_account_mapping(
        pool,
        NewAccountMapping {
            source_system: approval.source_system.trim(),
            property_scope: approval.property_scope.trim(),
            account_code: approval.account_code.trim(),
            account_name: approval.account_name.trim(),
            noi_category: reviewed_category,
            confidence_score: 1.0,
            status: "approved",
        },
    )
    .await?;
    reclassify_existing_gl_rows(pool, &approval, reviewed_category).await?;
    record_account_mapping_memory(pool, &approval, reviewed_category).await?;
    Ok(())
}

/// The category the harness would propose for a mapping awaiting review.
pub fn suggested_category_for(mapping: &AccountMapping) -> Option<String> {
    if mapping.status == "suggested" {
        return Some(mapping.noi_category.clone());
    }
    suggest_category(&mapping.account_code, &mapping.account_name)
        .map(|suggestion| suggestion.category)
}

fn review_row_from_mapping(mapping: &AccountMapping) -> AccountReviewRow {
    let suggested_category = suggested_category_for(mapping).unwrap_or_default();

    AccountReviewRow {
        source_system: mapping.source_system.clone(),
        property_scope: mapping.property_scope.clone(),
        account_code: mapping.account_code.clone(),
        account_name: mapping.account_name.clone(),
        current_category: mapping.noi_category.clone(),
        suggested_category,
        confidence_score: mapping.confidence_score,
        status: mapping.status.clone(),
        reviewed_category: String::new(),
        review_notes: String::new(),
    }
}

fn validate_approved_row(row: &AccountReviewRow, source_row: i64) -> Result<()> {
    for (field, value) in [
        ("source_system", row.source_system.trim()),
        ("property_scope", row.property_scope.trim()),
        ("account_code", row.account_code.trim()),
        ("account_name", row.account_name.trim()),
    ] {
        if value.is_empty() {
            return Err(anyhow!(
                AccountReviewImportError::ApprovedMissingRequiredField { source_row, field }
            ));
        }
    }
    if row.reviewed_category.trim().is_empty() {
        return Err(anyhow!(
            AccountReviewImportError::ApprovedMissingReviewedCategory { source_row }
        ));
    }
    Ok(())
}

async fn reclassify_existing_gl_rows(
    pool: &SqlitePool,
    approval: &MappingApproval<'_>,
    reviewed_category: &str,
) -> Result<()> {
    // Mappings are property-scoped; the same account code can mean different
    // things at different properties. Only reclassify GL rows for the property
    // this review row belongs to. If the scope cannot be resolved to a known
    // property, leave GL rows untouched — the approved mapping still applies
    // on the next ingest for that scope.
    let Some(property_name) = property_name_for_scope(approval.property_scope) else {
        return Ok(());
    };
    update_gl_category(
        pool,
        "gl_actuals",
        approval,
        reviewed_category,
        &property_name,
    )
    .await?;
    update_gl_category(
        pool,
        "gl_budgets",
        approval,
        reviewed_category,
        &property_name,
    )
    .await?;
    Ok(())
}

fn property_name_for_scope(property_scope: &str) -> Option<String> {
    let scope = property_scope.trim();
    built_in_lanes().into_iter().find_map(|lane| {
        let matches = lane.property_key.eq_ignore_ascii_case(scope)
            || lane.display_name.eq_ignore_ascii_case(scope)
            || lane
                .primary_property_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(scope));
        matches.then_some(lane.display_name)
    })
}

async fn update_gl_category(
    pool: &SqlitePool,
    table: &str,
    approval: &MappingApproval<'_>,
    reviewed_category: &str,
    property_name: &str,
) -> Result<()> {
    if !matches!(table, "gl_actuals" | "gl_budgets") {
        return Err(anyhow!(
            "unsupported GL table for reclassification: {table}"
        ));
    }
    // An approved operator review is the authoritative classification for an
    // account, so it must overwrite whatever the ingest-time heuristic
    // (`suggest_category`) guessed — not only rows the heuristic left
    // 'Unmapped'. Restricting this UPDATE to `category = 'Unmapped'` made
    // import-review unable to *correct* a wrong suggestion (e.g. "Property
    // Management Fees" auto-mapped to Other Income, "Current Year
    // Distributions" auto-mapped to Rental Income), which forced the prior
    // pass to hand-edit boxscore.db and broke reproducibility. Reclassifying
    // every matching row makes a clean init -> ingest -> import-review flow
    // deterministic with no direct DB edits.
    sqlx::query(&format!(
        "UPDATE {table}
         SET category = ?
         WHERE account_code = ?
           AND account_name = ?
           AND property_id IN (SELECT id FROM properties WHERE lower(name) = lower(?))"
    ))
    .bind(reviewed_category)
    .bind(approval.account_code.trim())
    .bind(approval.account_name.trim())
    .bind(property_name)
    .execute(pool)
    .await?;
    Ok(())
}

async fn record_account_mapping_memory(
    pool: &SqlitePool,
    approval: &MappingApproval<'_>,
    reviewed_category: &str,
) -> Result<()> {
    let mut value = format!(
        "{} maps to {} for {} scope {}.",
        approval.account_code.trim(),
        reviewed_category,
        approval.source_system.trim(),
        approval.property_scope.trim()
    );
    if !approval.review_notes.trim().is_empty() {
        value.push_str(&format!(" Review notes: {}.", approval.review_notes.trim()));
    }
    db::upsert_memory(
        pool,
        "account_mapping",
        approval.property_scope.trim(),
        approval.account_code.trim(),
        &value,
        1.0,
        None,
    )
    .await?;
    Ok(())
}

pub async fn record_unmapped_account_for_review(
    pool: &SqlitePool,
    property_scope: &str,
    account_code: &str,
    account_name: &str,
) -> Result<()> {
    if let Some(mapping) =
        db::find_account_mapping(pool, REVIEW_SOURCE_SYSTEM, property_scope, account_code).await?
    {
        if mapping.status == "approved" {
            return Ok(());
        }
    }

    db::upsert_account_mapping(
        pool,
        NewAccountMapping {
            source_system: REVIEW_SOURCE_SYSTEM,
            property_scope,
            account_code,
            account_name,
            noi_category: "Unmapped",
            confidence_score: 0.0,
            status: "unmapped",
        },
    )
    .await?;
    Ok(())
}
