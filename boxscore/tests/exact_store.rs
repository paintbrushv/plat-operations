use boxscore::exact::{
    money::Money,
    store::{self, Dataset, Snapshot},
    variance::Line,
};
use sqlx::Row;

fn dataset() -> Dataset {
    Dataset {
        property: "Synthetic Oak".into(),
        period: "2026-05".into(),
        currency: "USD".into(),
        expense_convention: "positive_costs".into(),
        unit_count: 10,
        actuals: vec![Line {
            account_code: "4000".into(),
            account_name: "Rent".into(),
            category: "rental income".into(),
            amount: "100.10".parse().unwrap(),
        }],
        budgets: vec![Line {
            account_code: "4000".into(),
            account_name: "Rent".into(),
            category: "rental income".into(),
            amount: "90.05".parse().unwrap(),
        }],
        snapshot: Some(Snapshot {
            as_of_date: "2026-05-31".into(),
            occupied_units: 9,
            vacant_units: 1,
            down_units: 0,
            market_rent_total: Some("100.10".parse().unwrap()),
            in_place_rent_total: None,
            delinquent_amount: None,
            prepaid_amount: None,
            concessions_amount: None,
        }),
    }
}

#[tokio::test]
async fn import_persists_cents_and_issues_immutable_correction_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("cents.sqlite");
    let pool = store::create(&path).await.unwrap();
    let data = dataset();
    let first = store::import(
        &pool,
        &data,
        None,
        None,
        &serde_json::json!({"synthetic":"hash"}),
    )
    .await
    .unwrap();
    let cell = sqlx::query(
        "SELECT amount_cents, typeof(amount_cents) AS kind FROM exact_gl WHERE kind='actual'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cell.get::<i64, _>("amount_cents"), 10010);
    assert_eq!(cell.get::<String, _>("kind"), "integer");
    let report = store::issue(&pool, &first).await.unwrap();
    let original = serde_json::to_string(&report).unwrap();
    let repeated = store::issue(&pool, &first).await.unwrap();
    assert_ne!(report["report_id"], repeated["report_id"]);
    let mut correction = data.clone();
    correction.actuals[0].amount = "100.12".parse::<Money>().unwrap();
    let second = store::import(
        &pool,
        &correction,
        Some(&first),
        Some("Synthetic two-cent correction"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let corrected = store::issue(&pool, &second).await.unwrap();
    assert_eq!(corrected["changes"]["actual_noi"], "0.02");
    assert_eq!(corrected["supersedes_revision"], first);
    let stored: String = sqlx::query_scalar("SELECT body FROM exact_reports WHERE id=?")
        .bind(report["report_id"].as_str().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, original);
    assert!(sqlx::query("UPDATE exact_gl SET amount_cents=0")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("DELETE FROM exact_reports")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("INSERT INTO gl_actuals (amount) VALUES (1.0)")
        .execute(&pool)
        .await
        .is_err());
    assert!(store::import(
        &pool,
        &data,
        Some(&first),
        Some("stale correction"),
        &serde_json::json!({})
    )
    .await
    .is_err());
    assert!(store::create(&path).await.is_err());
    pool.close().await;
    assert!(
        boxscore::db::connect(&format!("sqlite://{}", path.display()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalid_snapshot_and_excess_precision_never_create_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.snapshot.as_mut().unwrap().occupied_units = 100;
    assert!(
        store::import(&pool, &data, None, None, &serde_json::json!({}))
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM exact_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let raw = serde_json::to_string(&dataset())
        .unwrap()
        .replace("100.10", "100.101");
    assert!(serde_json::from_str::<Dataset>(&raw).is_err());
}

#[tokio::test]
async fn missing_budget_is_excluded_and_overflow_import_is_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.budgets.clear();
    let revision = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    let report = store::review(&pool, &revision).await.unwrap();
    assert_eq!(report["variance"]["noi_bridge"]["actual_revenue"], "0.00");
    assert_eq!(report["excluded_accounts"][0]["line"]["amount"], "100.10");
    assert_eq!(report["status"], "review_required");
    data.actuals[0].amount = "92233720368547758.07".parse().unwrap();
    data.actuals.push(Line {
        amount: "0.01".parse().unwrap(),
        ..data.actuals[0].clone()
    });
    assert!(store::import(
        &pool,
        &data,
        Some(&revision),
        Some("overflow"),
        &serde_json::json!({})
    )
    .await
    .is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM exact_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn coverage_uses_account_codes_and_preserves_complete_totals() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.budgets[0].account_name = "Different budget label".into();
    data.budgets[0].category = " Rental Income ".into();
    let id = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    let report = store::review(&pool, &id).await.unwrap();
    assert_eq!(report["variance"]["noi_bridge"]["noi_variance"], "10.05");
    assert!(report["excluded_accounts"].as_array().unwrap().is_empty());
    assert_eq!(
        report["variance"]["by_account"].as_array().unwrap().len(),
        1
    );
    data.budgets[0].category = "repairs".into();
    assert_eq!(
        store::import(
            &pool,
            &data,
            Some(&id),
            Some("conflicting map"),
            &serde_json::json!({})
        )
        .await
        .unwrap_err()
        .code,
        "ACCOUNT_MAPPING_CONFLICT"
    );
}

/// `issue` checks for a successor, reviews and inserts inside one immediate
/// transaction. The race it closes (a correction committed by another
/// connection between the check and the insert) is not deterministically
/// testable from a unit test, so this pins the observable ordering instead:
/// the latest revision issues, a correction then makes the old id refuse
/// `SUPERSEDED` with nothing stored on it, and every refusal rolls back so the
/// same single-connection pool still issues afterwards.
#[tokio::test]
async fn issue_runs_in_a_transaction_and_refuses_a_superseded_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("cents.sqlite"))
        .await
        .unwrap();
    let data = dataset();
    let first = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    // The latest revision issues, and an unknown id rolls back cleanly.
    store::issue(&pool, &first).await.unwrap();
    assert_eq!(
        store::issue(&pool, "no-such-revision")
            .await
            .unwrap_err()
            .code,
        "NOT_FOUND"
    );

    let mut correction = data.clone();
    correction.actuals[0].amount = "100.12".parse::<Money>().unwrap();
    let second = store::import(
        &pool,
        &correction,
        Some(&first),
        Some("Synthetic two-cent correction"),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let reports = |id: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM exact_reports WHERE revision_id=?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(reports(first.clone()).await, 1);

    let refusal = store::issue(&pool, &first).await.unwrap_err();
    assert_eq!(refusal.code, "SUPERSEDED");
    assert_eq!(
        refusal.message,
        format!("Revision {first} (revision 1) is superseded; the latest is {second} (revision 2)")
    );
    assert_eq!(reports(first.clone()).await, 1);
    assert_eq!(reports(second.clone()).await, 0);

    // The refusals left no transaction open: the latest still issues, twice.
    store::issue(&pool, &second).await.unwrap();
    store::issue(&pool, &second).await.unwrap();
    assert_eq!(reports(second).await, 2);
}

fn refusal(data: &Dataset) -> String {
    let error = data.validate().unwrap_err();
    assert_eq!(error.code, "INVALID_INPUT");
    error.message
}

// UPL-102: invisible characters in an account identity or category refuse at
// import, naming the list, row, field and code point.
#[test]
fn control_and_format_characters_in_account_fields_refuse() {
    // The UPL-102 repro: a byte-order mark glued to a category.
    let mut data = dataset();
    data.actuals[0].category = "\u{FEFF}rental income".into();
    let message = refusal(&data);
    assert!(
        message.contains("actuals[0].category") && message.contains("U+FEFF"),
        "{message}"
    );

    // A zero-width space in an account code, in a budget row.
    let mut data = dataset();
    data.budgets[0].account_code = "40\u{200B}00".into();
    let message = refusal(&data);
    assert!(
        message.contains("budgets[0].account_code") && message.contains("U+200B"),
        "{message}"
    );

    // A control character in an account name (here a tab), second row.
    let mut data = dataset();
    let mut second = data.actuals[0].clone();
    second.account_code = "4010".into();
    second.account_name = "Late\tFees".into();
    data.actuals.push(second);
    let message = refusal(&data);
    assert!(
        message.contains("actuals[1].account_name") && message.contains("U+0009"),
        "{message}"
    );

    // The ranges the issue names, and a few more format characters.
    for c in [
        '\u{200B}', '\u{200F}', '\u{2028}', '\u{202E}', '\u{2060}', '\u{2064}', '\u{FEFF}',
        '\u{00AD}', '\u{0000}', '\u{007F}', '\u{0085}',
    ] {
        let mut data = dataset();
        data.actuals[0].account_name = format!("Rent{c}");
        let message = refusal(&data);
        assert!(
            message.contains(&format!("U+{:04X}", c as u32)),
            "{message}"
        );
    }

    // Ordinary non-ASCII text and punctuation still import.
    for ok in [
        "Caf\u{e9} income",
        "Rent & Fees",
        "Zimmerm\u{e4}dchen",
        "\u{4e2d}\u{6587}",
    ] {
        let mut data = dataset();
        data.actuals[0].account_name = ok.into();
        data.validate().unwrap();
    }
}

// Whitespace other than a plain space looks like a space and does not compare
// equal to one: "Rental\u{A0}Income" and "Rental Income" are two categories.
#[test]
fn whitespace_other_than_a_plain_space_refuses_and_is_flagged_on_stored_data() {
    let mut data = dataset();
    data.actuals[0].category = "Rental\u{A0}Income".into();
    data.budgets[0].category = "Rental\u{A0}Income".into();
    let message = refusal(&data);
    assert!(
        message.contains("actuals[0].category") && message.contains("U+00A0"),
        "{message}"
    );
    assert!(data.validate_stored().is_ok());
    assert!(data.has_hidden_characters());

    // Every Unicode space, the line separators and the invisible fillers.
    for c in [
        '\u{A0}', '\u{1680}', '\u{2000}', '\u{2005}', '\u{200A}', '\u{202F}', '\u{205F}',
        '\u{3000}', '\u{2028}', '\u{2029}', '\u{034F}', '\u{180B}', '\u{180C}', '\u{180D}',
        '\u{180F}', '\u{FE00}', '\u{FE0F}', '\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}',
        '\u{2800}',
    ] {
        let mut data = dataset();
        data.budgets[0].account_name = format!("Rent{c}Fees");
        let message = refusal(&data);
        assert!(
            message.contains(&format!("U+{:04X}", c as u32)),
            "{message}"
        );
        assert!(data.has_hidden_characters(), "U+{:04X}", c as u32);
    }
    // A plain space, and the characters next to the ranges, are fine.
    for ok in ["Rental Income", "Rent\u{FE10}", "Rent\u{180}", "a b  c"] {
        let mut data = dataset();
        data.actuals[0].category = ok.into();
        data.budgets[0].category = ok.into();
        data.validate().unwrap();
        assert!(!data.has_hidden_characters(), "{ok:?}");
    }
}

// The property keys the revision chain: a hidden character in it would start a
// second chain for what looks like the same property.
#[tokio::test]
async fn a_property_with_a_hidden_character_is_refused_instead_of_forking_a_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.snapshot = None;
    let first = store::import(&pool, &data, None, None, &serde_json::json!({}))
        .await
        .unwrap();
    for hidden in [
        "Sample\u{200B}Gardens",
        "Sample\u{A0}Gardens",
        "Sample\u{FEFF}Gardens",
    ] {
        let mut forked = data.clone();
        forked.property = hidden.into();
        let message = refusal(&forked);
        assert!(
            message.starts_with("property contains the invisible character U+"),
            "{message}"
        );
        assert!(forked.has_hidden_characters());
        assert!(forked.validate_stored().is_ok());
        let error = store::import(&pool, &forked, None, None, &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "INVALID_INPUT");
    }
    let message = {
        let mut forked = data.clone();
        forked.property = "Sample\u{200B}Gardens".into();
        refusal(&forked)
    };
    assert!(message.contains("U+200B"), "{message}");
    // Nothing was stored for the lookalikes: one revision, one chain.
    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM exact_revisions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(revisions, 1, "{first}");
    assert!(!data.has_hidden_characters());
}

// The character rule applies to new imports. A revision stored before the rule
// (here written straight through SQL, bypassing `validate`, as old data would
// be) is still reviewed, and the review says it holds invisible characters.
#[tokio::test]
async fn a_revision_stored_with_an_invisible_character_is_still_reviewable() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = store::create(&tmp.path().join("exact.sqlite"))
        .await
        .unwrap();
    let mut data = dataset();
    data.snapshot = None;
    data.actuals[0].account_name = "Re\u{200B}nt".into();
    assert!(data.validate().is_err());
    assert!(data.validate_stored().is_ok());
    assert!(data.has_hidden_characters());
    assert!(!dataset().has_hidden_characters());
    // New imports refuse and store nothing.
    assert_eq!(
        store::import(&pool, &data, None, None, &serde_json::json!({}))
            .await
            .unwrap_err()
            .code,
        "INVALID_INPUT"
    );

    let id = "legacy-revision";
    let hash = boxscore::exact::digest(&serde_json::to_vec(&data).unwrap());
    sqlx::query("INSERT INTO exact_revisions VALUES (?,?,?,?,?,?,?,?,?,?)")
        .bind(id)
        .bind(&data.property)
        .bind(&data.period)
        .bind(1_i64)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(data.unit_count)
        .bind(&hash)
        .bind("{}")
        .bind("2026-05-31T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
    for (kind, rows) in [("actual", &data.actuals), ("budget", &data.budgets)] {
        for (i, row) in rows.iter().enumerate() {
            sqlx::query("INSERT INTO exact_gl VALUES (?,?,?,?,?,?,?)")
                .bind(id)
                .bind(kind)
                .bind(i as i64)
                .bind(&row.account_code)
                .bind(&row.account_name)
                .bind(&row.category)
                .bind(row.amount.cents())
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    let review = store::review(&pool, id).await.unwrap();
    assert_eq!(review["status"], "review_required");
    let reasons = review["variance"]["review_reasons"].as_array().unwrap();
    assert!(
        reasons.contains(&serde_json::json!("invisible_characters")),
        "{reasons:?}"
    );
}
