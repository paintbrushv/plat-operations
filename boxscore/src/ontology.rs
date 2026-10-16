use serde::{Deserialize, Serialize};

/// The reviewed NOI categories an operator can map an account to.
pub const NOI_CATEGORIES: [&str; 12] = [
    "Rental Income",
    "Concessions",
    "Bad Debt",
    "Other Income",
    "Payroll",
    "Repairs & Maintenance",
    "Utilities",
    "Marketing",
    "Administrative",
    "Taxes",
    "Insurance",
    "Management Fees",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountClass {
    Revenue,
    Expense,
    /// Not yet reviewed. Excluded from NOI totals so balance-sheet rows
    /// (capital contributions, payables, escrows) cannot distort an
    /// owner-facing bridge before an operator maps them.
    Unmapped,
}

/// The canonical spelling of a reviewed NOI category, matched without regard
/// to case or surrounding whitespace. `None` means the string is not one of
/// `NOI_CATEGORIES` or `Unmapped`, so an ingest must refuse it rather than let
/// `account_class` fold it into expenses.
pub fn canonical_category(category: &str) -> Option<&'static str> {
    let wanted = category.trim();
    if wanted.eq_ignore_ascii_case("unmapped") {
        return Some("Unmapped");
    }
    NOI_CATEGORIES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(wanted))
}

pub fn account_class(category: &str) -> AccountClass {
    match category.trim().to_ascii_lowercase().as_str() {
        "rental income" | "concessions" | "bad debt" | "other income" => AccountClass::Revenue,
        "unmapped" => AccountClass::Unmapped,
        _ => AccountClass::Expense,
    }
}

pub fn is_controllable_expense(category: &str) -> bool {
    matches!(
        category.trim().to_ascii_lowercase().as_str(),
        "payroll" | "repairs & maintenance" | "marketing" | "administrative" | "management fees"
    )
}

pub fn occupancy_rate(occupied_units: i64, vacant_units: i64, down_units: i64) -> Option<f64> {
    let total = occupied_units + vacant_units + down_units;
    (total > 0).then(|| occupied_units as f64 / total as f64)
}

pub fn economic_occupancy(in_place_rent_total: f64, market_rent_total: f64) -> Option<f64> {
    (market_rent_total.abs() > f64::EPSILON).then(|| in_place_rent_total / market_rent_total)
}

/// The footer-token rule shared by the standardized-CSV validator and the
/// exact-cent core's `inspect_export`.
///
/// Tokens that mark a Yardi total/subtotal/footer row that must never be a unit.
/// `total` is matched exactly (avoid resident names like "Total Wine LLC");
/// the multi-word phrases use substring `contains`.
pub const FOOTER_TOKENS: &[&str] = &[
    "total",
    "all properties",
    "grand total",
    "subtotal",
    "summary",
];

/// True when a cell carries a vendor total/subtotal/footer token: `total`
/// as an exact (case-insensitive) match, the multi-word phrases anywhere.
pub fn is_footer_cell(cell: &str) -> bool {
    FOOTER_TOKENS.iter().any(|token| footer_match(cell, token))
}

/// Whether `cell` matches one footer `token` under the rule above.
pub fn footer_match(cell: &str, token: &str) -> bool {
    let s = cell.to_ascii_lowercase();
    let s = s.trim();
    if token == "total" {
        s == token
    } else {
        s.contains(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_category_folds_case_and_refuses_unknown_names() {
        assert_eq!(canonical_category(" rental income "), Some("Rental Income"));
        assert_eq!(
            canonical_category("REPAIRS & MAINTENANCE"),
            Some("Repairs & Maintenance")
        );
        assert_eq!(canonical_category("unmapped"), Some("Unmapped"));
        for unknown in [
            "Total",
            "Net Operating Income",
            "Repairs and Maintenance",
            "",
        ] {
            assert_eq!(canonical_category(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn computes_physical_occupancy_with_down_units_in_denominator() {
        let rate = occupancy_rate(153, 15, 4).unwrap();
        assert!((rate - 0.8895).abs() < 0.001);
    }
}
