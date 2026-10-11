use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuggestedMapping {
    pub category: String,
    pub confidence_score: f64,
    pub reason: String,
}

pub fn suggest_category(account_code: &str, account_name: &str) -> Option<SuggestedMapping> {
    let name = account_name.to_ascii_lowercase();
    let _ = account_code; // identity only; never evidence for a category

    // Balance-sheet and below-the-line accounts must never be auto-mapped
    // into NOI categories ("Property Tax Payable" is a liability, not an
    // operating tax expense). Returning None routes them to operator review.
    if contains_any(
        &name,
        &[
            "payable",
            "receivable",
            "capital contribution",
            "owner distribution",
            "escrow",
            "depreciation",
            "amortization",
            "mortgage",
            "loan principal",
            "security deposit",
            "accrued",
            "prepaid",
            "suspense",
            "clearing",
            "intercompany",
        ],
    ) {
        return None;
    }

    // Order matters: the specific expense phrases come before the generic
    // "fee" revenue pattern so "Management Fees" can never be classified as
    // Other Income. Single-word needles match on word boundaries (prefix of a
    // token), so "rent" matches "Rent"/"Rental" but not "Current". No rule
    // keys on the account code: a chart-of-accounts prefix is one vendor's
    // convention, not evidence of what the account is.
    let suggestion = if matches_any(&name, &["management fee", "mgmt fee"]) {
        ("Management Fees", 0.85, "Matched management fee pattern")
    } else if matches_any(&name, &["concession"]) {
        ("Concessions", 0.85, "Matched concession pattern")
    } else if matches_any(&name, &["bad debt", "write off", "write-off", "writeoff"]) {
        ("Bad Debt", 0.85, "Matched bad debt pattern")
    } else if matches_any(&name, &["rent"]) && !matches_any(&name, &["pet", "trash", "utilit"]) {
        ("Rental Income", 0.80, "Matched rent income pattern")
    } else if matches_any(
        &name,
        &["other income", "fee", "garage", "carport", "pet", "trash"],
    ) {
        ("Other Income", 0.70, "Matched other income pattern")
    } else if matches_any(&name, &["payroll", "salar", "wage"]) {
        ("Payroll", 0.85, "Matched payroll pattern")
    } else if matches_any(
        &name,
        &["repair", "maintenance", "make ready", "make-ready"],
    ) {
        (
            "Repairs & Maintenance",
            0.85,
            "Matched repairs and maintenance pattern",
        )
    } else if matches_any(&name, &["utilit", "electric", "water", "gas", "sewer"]) {
        ("Utilities", 0.80, "Matched utility pattern")
    } else if matches_any(&name, &["marketing", "advertising"]) {
        ("Marketing", 0.85, "Matched marketing pattern")
    } else if matches_any(&name, &["admin", "office", "legal", "professional"]) {
        ("Administrative", 0.75, "Matched administrative pattern")
    } else if matches_any(&name, &["tax"]) {
        ("Taxes", 0.75, "Matched taxes pattern")
    } else if matches_any(&name, &["insurance"]) {
        ("Insurance", 0.85, "Matched insurance pattern")
    } else {
        return None;
    };

    Some(SuggestedMapping {
        category: suggestion.0.to_string(),
        confidence_score: suggestion.1,
        reason: suggestion.2.to_string(),
    })
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

/// A multi-word needle matches as a phrase anywhere in the name. A
/// single-word needle must start a word of the name, so `rent` matches
/// "Rent", "Rental" and "Rents" but not "Current", and `fee` matches "Fee"
/// and "Fees" but not "Coffee".
fn matches_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| {
        if needle.contains(' ') || needle.contains('-') {
            value.contains(needle)
        } else {
            value
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word.starts_with(needle))
        }
    })
}
