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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_physical_occupancy_with_down_units_in_denominator() {
        let rate = occupancy_rate(153, 15, 4).unwrap();
        assert!((rate - 0.8895).abs() < 0.001);
    }
}
