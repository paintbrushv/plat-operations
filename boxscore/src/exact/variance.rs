use super::{error, money::Money, Result};
use crate::ontology::{account_class, AccountClass};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub amount: Money,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVariance {
    pub account_code: String,
    pub account_name: String,
    pub category: String,
    pub actual: Money,
    pub budget: Money,
    pub variance: Money,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variance {
    pub by_account: Vec<AccountVariance>,
    pub noi_bridge: BTreeMap<String, Money>,
    pub review_reasons: Vec<String>,
}

pub fn compute(actuals: &[Line], budgets: &[Line]) -> Result<Variance> {
    if actuals.len() + budgets.len() > 100_000 {
        return Err(error("INPUT_LIMIT", "Too many GL rows"));
    }
    let mut totals: BTreeMap<(String, String, String), [Money; 2]> = BTreeMap::new();
    for (rows, side) in [(actuals, 0), (budgets, 1)] {
        for row in rows {
            if row.account_code.is_empty()
                || row.account_code.len() > 64
                || row.account_name.len() > 256
                || row.category.trim().is_empty()
                || row.category.len() > 128
            {
                return Err(error(
                    "INVALID_INPUT",
                    "Invalid account identity or category",
                ));
            }
            let values = totals
                .entry((
                    row.account_code.clone(),
                    row.account_name.clone(),
                    row.category.clone(),
                ))
                .or_default();
            values[side] = values[side].checked_add(row.amount)?;
        }
    }
    let mut rows = Vec::new();
    let (mut revenue, mut expense, mut unmapped) =
        ([Money::ZERO; 2], [Money::ZERO; 2], [Money::ZERO; 2]);
    for ((code, name, category), amounts) in totals {
        let target = match account_class(&category) {
            AccountClass::Revenue => &mut revenue,
            AccountClass::Expense => &mut expense,
            AccountClass::Unmapped => &mut unmapped,
        };
        for side in 0..2 {
            target[side] = target[side].checked_add(amounts[side])?;
        }
        rows.push(AccountVariance {
            account_code: code,
            account_name: name,
            category,
            actual: amounts[0],
            budget: amounts[1],
            variance: amounts[0].checked_sub(amounts[1])?,
        });
    }
    let noi = [
        revenue[0].checked_sub(expense[0])?,
        revenue[1].checked_sub(expense[1])?,
    ];
    let mut bridge = BTreeMap::new();
    for (name, values) in [("revenue", revenue), ("expenses", expense), ("noi", noi)] {
        bridge.insert(format!("actual_{name}"), values[0]);
        bridge.insert(format!("budget_{name}"), values[1]);
        let variance_name = if name == "expenses" { "expense" } else { name };
        bridge.insert(
            format!("{variance_name}_variance"),
            values[0].checked_sub(values[1])?,
        );
    }
    bridge.insert("unmapped_actual".into(), unmapped[0]);
    bridge.insert("unmapped_budget".into(), unmapped[1]);
    let mut review_reasons = Vec::new();
    if expense.iter().any(|m| m.cents() < 0) {
        review_reasons.push("negative_net_expenses".into());
    }
    Ok(Variance {
        by_account: rows,
        noi_bridge: bridge,
        review_reasons,
    })
}
