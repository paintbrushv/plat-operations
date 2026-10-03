use boxscore::exact::{
    money::Money,
    variance::{compute, Line},
};
use std::str::FromStr;

fn money(value: &str) -> Money {
    Money::from_str(value).unwrap()
}
fn line(code: &str, category: &str, value: &str) -> Line {
    Line {
        account_code: code.into(),
        account_name: code.into(),
        category: category.into(),
        amount: money(value),
    }
}

#[test]
fn exact_cents_preserve_decimal_addition_credits_and_reversals() {
    assert_eq!(
        money("0.10")
            .checked_add(money("0.20"))
            .unwrap()
            .to_string(),
        "0.30"
    );
    assert_eq!(money("-0.01").to_string(), "-0.01");
    assert_eq!(
        money("12.34").checked_add(money("-12.34")).unwrap().cents(),
        0
    );
    assert_eq!(
        serde_json::to_string(&money("123.40")).unwrap(),
        "\"123.40\""
    );
    assert!(serde_json::from_str::<Money>("123.40").is_err());
}

#[test]
fn precision_and_overflow_refuse_without_rounding_or_wrapping() {
    for value in [
        "0.001",
        "1.230",
        "NaN",
        "inf",
        "1e2",
        "",
        " 1.00",
        "92233720368547758.08",
    ] {
        assert!(Money::from_str(value).is_err(), "accepted {value}");
    }
    let limit = money("92233720368547758.07");
    assert!(limit.checked_add(money("0.01")).is_err());
    assert!(money("-92233720368547758.07")
        .checked_sub(money("0.01"))
        .is_err());
}

#[test]
fn actual_budget_bridge_is_exact_and_discloses_unmapped_money() {
    let actuals = vec![
        line("4000", "rental income", "100.10"),
        line("4000", "rental income", "0.20"),
        line("6000", "repairs", "30.03"),
        line("6000", "repairs", "-5.01"),
        line("9999", "unmapped", "42.01"),
    ];
    let budgets = vec![
        line("4000", "rental income", "90.10"),
        line("6000", "repairs", "25.00"),
    ];
    let result = compute(&actuals, &budgets).unwrap();
    assert_eq!(result.noi_bridge["actual_noi"].to_string(), "75.28");
    assert_eq!(result.noi_bridge["noi_variance"].to_string(), "10.18");
    assert_eq!(result.noi_bridge["unmapped_actual"].to_string(), "42.01");
    assert_eq!(result.by_account[0].actual.to_string(), "100.30");
}

#[test]
fn aggregate_overflow_and_negative_expense_review_are_explicit() {
    assert!(compute(
        &[
            line("4000", "rental income", "92233720368547758.07"),
            line("4000", "rental income", "0.01")
        ],
        &[]
    )
    .is_err());
    let result = compute(&[line("6000", "repairs", "-30.00")], &[]).unwrap();
    assert_eq!(result.noi_bridge["actual_expenses"].to_string(), "-30.00");
    assert!(result
        .review_reasons
        .contains(&"negative_net_expenses".to_string()));
}
