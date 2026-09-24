use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuggestedMapping {
    pub category: String,
    pub confidence_score: f64,
    pub reason: String,
}

pub fn suggest_category(account_code: &str, account_name: &str) -> Option<SuggestedMapping> {
    let name = account_name.to_ascii_lowercase();
    let code = account_code.trim();

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

    let suggestion = if contains_any(&name, &["concession", "conces"]) {
        ("Concessions", 0.85, "Matched concession pattern")
    } else if contains_any(&name, &["bad debt", "write off", "write-off"]) {
        ("Bad Debt", 0.85, "Matched bad debt pattern")
    } else if contains_any(&name, &["rent"]) && !contains_any(&name, &["pet", "trash", "utility"]) {
        ("Rental Income", 0.80, "Matched rent income pattern")
    } else if contains_any(
        &name,
        &["other income", "fee", "garage", "carport", "pet", "trash"],
    ) {
        ("Other Income", 0.70, "Matched other income pattern")
    } else if contains_any(&name, &["payroll", "salary", "wage"]) {
        ("Payroll", 0.85, "Matched payroll pattern")
    } else if contains_any(
        &name,
        &["repair", "maintenance", "make ready", "make-ready"],
    ) {
        (
            "Repairs & Maintenance",
            0.85,
            "Matched repairs and maintenance pattern",
        )
    } else if contains_any(&name, &["utility", "electric", "water", "gas", "sewer"]) {
        ("Utilities", 0.80, "Matched utility pattern")
    } else if contains_any(&name, &["marketing", "advertising"]) {
        ("Marketing", 0.85, "Matched marketing pattern")
    } else if contains_any(&name, &["admin", "office", "legal", "professional"]) {
        ("Administrative", 0.75, "Matched administrative pattern")
    } else if contains_any(&name, &["tax"]) || code.starts_with("60") {
        ("Taxes", 0.75, "Matched taxes pattern")
    } else if contains_any(&name, &["insurance"]) {
        ("Insurance", 0.85, "Matched insurance pattern")
    } else if contains_any(&name, &["management fee", "mgmt fee"]) {
        ("Management Fees", 0.85, "Matched management fee pattern")
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
