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

#[test]
fn matching_codes_tolerate_names_and_category_format_but_refuse_conflicts() {
    let a = line("4000", " Rental Income ", "100.10");
    let mut b = line("4000", "rental income", "90.00");
    b.account_name = "Budget rent label".into();
    let result = compute(std::slice::from_ref(&a), std::slice::from_ref(&b)).unwrap();
    assert_eq!(result.by_account.len(), 1);
    assert_eq!(result.by_account[0].variance.to_string(), "10.10");
    b.category = "repairs".into();
    assert_eq!(
        compute(&[a], &[b]).unwrap_err().code,
        "ACCOUNT_MAPPING_CONFLICT"
    );
}

#[test]
fn whitespace_padded_account_codes_refuse_instead_of_splitting_accounts() {
    // A trailing space on one side's export must not create a phantom second
    // account that reports a full actual and a full budget variance while the
    // status stays "calculated".
    let actuals = vec![line("4000", "rental income", "100.00")];
    for padded in ["4000 ", " 4000", "\t4000"] {
        let budgets = vec![line(padded, "rental income", "90.00")];
        let refused = compute(&actuals, &budgets).unwrap_err();
        assert_eq!(refused.code, "INVALID_INPUT");
        assert!(refused.message.contains("whitespace"));
        assert_eq!(
            compute(&budgets, &actuals).unwrap_err().code,
            "INVALID_INPUT"
        );
    }
    let clean = compute(&actuals, &[line("4000", "rental income", "90.00")]).unwrap();
    assert_eq!(clean.by_account.len(), 1);
    assert_eq!(clean.noi_bridge["noi_variance"].to_string(), "10.00");
}
