//! T12 (trailing-twelve-month) statement assembly.
//!
//! Assembles a compact income statement across 12 rolling periods using
//! category-level GL actuals. NOI = revenue − expenses; unmapped rows are
//! excluded from the NOI total but disclosed as a final row so nothing
//! silently disappears from operator review.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{db, ontology};

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Compact dollar formatting for the T12 table.
///
/// * Under $10k absolute → `$9,999` or `-$9,999`
/// * Under $1M absolute  → `$12.3k` or `-$12.3k`
/// * $1M+ absolute       → `$1.23M` or `-$1.23M`
pub fn compact_dollars(v: f64) -> String {
    let abs = v.abs();
    let sign = if v < 0.0 { "-" } else { "" };
    if abs < 10_000.0 {
        // Integer with comma separator for thousands
        let rounded = abs.round() as i64;
        if rounded >= 1_000 {
            format!("{sign}${},{:03}", rounded / 1000, rounded % 1000)
        } else {
            format!("{sign}${rounded}")
        }
    } else if abs < 1_000_000.0 {
        format!("{sign}${:.1}k", abs / 1_000.0)
    } else {
        format!("{sign}${:.2}M", abs / 1_000_000.0)
    }
}

// ── Period window ─────────────────────────────────────────────────────────────

/// Returns `n` period labels (YYYY-MM) ending at `end_period` inclusive,
/// computed via year×12+month arithmetic.
///
/// Example: end_period="2026-04", n=12 → ["2025-05", …, "2026-04"]
pub fn period_window(end_period: &str, n: usize) -> Vec<String> {
    let (year, month) = db::parse_period_label(end_period).expect("valid end_period");
    let end_ordinal = year * 12 + (month - 1); // 0-based month ordinal
    (0..n as i64)
        .rev()
        .map(|offset| {
            let ord = end_ordinal - offset;
            let (y, m) = (ord.div_euclid(12), ord.rem_euclid(12) + 1);
            format!("{y:04}-{m:02}")
        })
        .collect()
}

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum T12RowType {
    Revenue,
    Expense,
    Noi,
    Unmapped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct T12Row {
    pub label: String,
    pub row_type: T12RowType,
    /// One value per period; same order as `T12Statement::periods`.
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct T12Statement {
    pub property: String,
    pub end_period: String,
    /// Twelve period labels, earliest first.
    pub periods: Vec<String>,
    pub rows: Vec<T12Row>,
}

// ── Assembly ──────────────────────────────────────────────────────────────────

/// Build a T12 statement for `property_name`, ending at `end_period` (or the
/// latest period with actuals for that property if `None`).
pub async fn assemble_t12(
    pool: &SqlitePool,
    property_name: &str,
    end_period: Option<&str>,
) -> Result<T12Statement> {
    let property = db::require_property_by_name(pool, property_name).await?;

    // Resolve end period.
    let end_period: String = match end_period {
        Some(p) => db::normalize_period_label(p)?,
        None => {
            // Find the latest period label with gl_actuals for this property.
            let label: Option<String> = sqlx::query_scalar(
                "SELECT p.label FROM gl_actuals a
                 JOIN periods p ON p.id = a.period_id
                 WHERE a.property_id = ?
                 ORDER BY p.label DESC
                 LIMIT 1",
            )
            .bind(&property.id)
            .fetch_optional(pool)
            .await?;
            label.ok_or_else(|| anyhow!("no gl_actuals found for property: {property_name}"))?
        }
    };

    let periods = period_window(&end_period, 12);

    // Fetch category totals across these 12 periods.
    let category_months = db::category_totals_by_period(pool, &property.id, &periods).await?;

    // Build a lookup: category → period → total.
    let mut cat_period: std::collections::HashMap<String, std::collections::HashMap<String, f64>> =
        std::collections::HashMap::new();
    for cm in &category_months {
        cat_period
            .entry(cm.noi_category.clone())
            .or_default()
            .insert(cm.period_label.clone(), cm.total);
    }

    // Build rows in ontology order: revenue categories first, then expense.
    let mut rows: Vec<T12Row> = Vec::new();
    let mut noi_vec: Vec<f64> = vec![0.0; 12];
    let mut unmapped_vec: Vec<f64> = vec![0.0; 12];
    let mut has_unmapped = false;

    for &category in ontology::NOI_CATEGORIES.iter() {
        let row_type = match ontology::account_class(category) {
            ontology::AccountClass::Revenue => T12RowType::Revenue,
            ontology::AccountClass::Expense => T12RowType::Expense,
            ontology::AccountClass::Unmapped => T12RowType::Unmapped,
        };

        let values: Vec<f64> = periods
            .iter()
            .map(|p| {
                cat_period
                    .get(category)
                    .and_then(|m| m.get(p))
                    .copied()
                    .unwrap_or(0.0)
            })
            .collect();

        // Skip rows with no activity across all 12 months.
        if values.iter().all(|&v| v == 0.0) {
            continue;
        }

        // Accumulate into NOI.
        for (i, &v) in values.iter().enumerate() {
            match row_type {
                T12RowType::Revenue => noi_vec[i] += v,
                T12RowType::Expense => noi_vec[i] -= v,
                _ => {}
            }
        }

        rows.push(T12Row {
            label: category.to_string(),
            row_type,
            values,
        });
    }

    // Handle Unmapped separately (not in NOI_CATEGORIES by name, stored as "Unmapped").
    if let Some(umap) = cat_period.get("Unmapped") {
        let values: Vec<f64> = periods
            .iter()
            .map(|p| umap.get(p).copied().unwrap_or(0.0))
            .collect();
        if values.iter().any(|&v| v != 0.0) {
            unmapped_vec = values.clone();
            has_unmapped = true;
        }
    }

    // NOI row.
    rows.push(T12Row {
        label: "NOI".to_string(),
        row_type: T12RowType::Noi,
        values: noi_vec,
    });

    // Unmapped disclosure row (dimmed in UI).
    if has_unmapped {
        rows.push(T12Row {
            label: "Unmapped (excluded)".to_string(),
            row_type: T12RowType::Unmapped,
            values: unmapped_vec,
        });
    }

    Ok(T12Statement {
        property: property_name.to_string(),
        end_period: end_period.clone(),
        periods,
        rows,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, variance};

    #[test]
    fn test_period_window() {
        let window = period_window("2026-04", 12);
        assert_eq!(window.len(), 12);
        assert_eq!(window[0], "2025-05");
        assert_eq!(window[11], "2026-04");
    }

    #[test]
    fn test_period_window_year_boundary() {
        let window = period_window("2026-01", 3);
        assert_eq!(window, vec!["2025-11", "2025-12", "2026-01"]);
    }

    #[test]
    fn test_compact_dollars() {
        // Under $10k
        assert_eq!(compact_dollars(950.0), "$950");
        assert_eq!(compact_dollars(9_999.0), "$9,999");
        assert_eq!(compact_dollars(1_000.0), "$1,000");
        // $10k–$1M
        assert_eq!(compact_dollars(12_345.0), "$12.3k");
        assert_eq!(compact_dollars(45_000.0), "$45.0k");
        // $1M+
        assert_eq!(compact_dollars(1_234_567.0), "$1.23M");
        // Negatives
        assert_eq!(compact_dollars(-45_000.0), "-$45.0k");
        assert_eq!(compact_dollars(-950.0), "-$950");
        assert_eq!(compact_dollars(-1_234_567.0), "-$1.23M");
    }

    #[tokio::test]
    async fn test_noi_consistency() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        // Insert a property.
        sqlx::query(
            "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
             VALUES ('p1', 'Test Property', 'Test', 100, 'Example Sponsor', 'PM', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Insert two periods.
        let pid1 = db::upsert_period(&pool, "2026-03").await.unwrap();
        let pid2 = db::upsert_period(&pool, "2026-04").await.unwrap();

        // Revenue: Rental Income 10000 in both periods.
        for (pid, amount) in [(&pid1, 10000.0f64), (&pid2, 10000.0f64)] {
            sqlx::query(
                "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
                 VALUES (?, 'p1', ?, '4000', 'Rental Income', 'Rental Income', ?, 'seed.csv', 1, '2026-01-01')",
            )
            .bind(db::new_id())
            .bind(pid)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Expense: Repairs & Maintenance 3000 in period 1, 4000 in period 2.
        for (pid, amount) in [(&pid1, 3000.0f64), (&pid2, 4000.0f64)] {
            sqlx::query(
                "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
                 VALUES (?, 'p1', ?, '5100', 'Repairs & Maint', 'Repairs & Maintenance', ?, 'seed.csv', 2, '2026-01-01')",
            )
            .bind(db::new_id())
            .bind(pid)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Unmapped: should be excluded from NOI.
        for (pid, amount) in [(&pid1, 500_000.0f64), (&pid2, 0.0f64)] {
            sqlx::query(
                "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
                 VALUES (?, 'p1', ?, '3000', 'Balance Sheet', 'Unmapped', ?, 'seed.csv', 3, '2026-01-01')",
            )
            .bind(db::new_id())
            .bind(pid)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
        }

        let stmt = assemble_t12(&pool, "Test Property", Some("2026-04"))
            .await
            .unwrap();

        // The 12-window ends at 2026-04 → periods[10] = 2026-03, periods[11] = 2026-04.
        let idx_mar = stmt.periods.iter().position(|p| p == "2026-03").unwrap();
        let idx_apr = stmt.periods.iter().position(|p| p == "2026-04").unwrap();

        let noi_row = stmt
            .rows
            .iter()
            .find(|r| r.row_type == T12RowType::Noi)
            .expect("NOI row present");

        // NOI = revenue - expense (unmapped excluded)
        assert_eq!(noi_row.values[idx_mar], 10000.0 - 3000.0); // 7000
        assert_eq!(noi_row.values[idx_apr], 10000.0 - 4000.0); // 6000

        // Cross-check with compute_noi_bridge for 2026-04.
        let actuals_apr: Vec<crate::models::GlLine> =
            sqlx::query_as("SELECT * FROM gl_actuals WHERE property_id = 'p1' AND period_id = ?")
                .bind(&pid2)
                .fetch_all(&pool)
                .await
                .unwrap();
        let variances = variance::compute_account_variances(&actuals_apr, &[]);
        let bridge = variance::compute_noi_bridge(&variances);
        assert_eq!(bridge.actual_noi, noi_row.values[idx_apr]);

        // Unmapped row should be disclosed.
        let unmapped_row = stmt
            .rows
            .iter()
            .find(|r| r.row_type == T12RowType::Unmapped);
        assert!(unmapped_row.is_some(), "unmapped disclosure row present");
        assert_eq!(unmapped_row.unwrap().values[idx_mar], 500_000.0);
    }

    #[tokio::test]
    async fn contra_revenue_concessions_reduce_t12_noi() {
        // Concessions are stored NEGATIVE (contra-revenue) in gl_actuals,
        // matching the standardized Yardi exports. They classify as Revenue
        // and must therefore reduce NOI, not inflate it.
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
             VALUES ('p1', 'Test Property', 'Test', 100, 'Example Sponsor', 'PM', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let pid = db::upsert_period(&pool, "2026-04").await.unwrap();
        for (code, name, category, amount) in [
            ("4000", "Rental Income", "Rental Income", 10_000.0f64),
            ("4990", "Concessions", "Concessions", -1_500.0),
            ("4995", "Bad Debt", "Bad Debt", -500.0),
        ] {
            sqlx::query(
                "INSERT INTO gl_actuals (id, property_id, period_id, account_code, account_name, category, amount, source_file, source_row, created_at)
                 VALUES (?, 'p1', ?, ?, ?, ?, ?, 'seed.csv', 1, '2026-01-01')",
            )
            .bind(db::new_id())
            .bind(&pid)
            .bind(code)
            .bind(name)
            .bind(category)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
        }

        let stmt = assemble_t12(&pool, "Test Property", Some("2026-04"))
            .await
            .unwrap();
        let idx = stmt.periods.iter().position(|p| p == "2026-04").unwrap();
        let noi = stmt
            .rows
            .iter()
            .find(|r| r.row_type == T12RowType::Noi)
            .unwrap();
        assert_eq!(noi.values[idx], 8_000.0, "NOI = 10000 - 1500 - 500");
    }
}
