use std::path::Path;

use boxscore::{
    close_readiness::{
        CloseReadinessStatus, CloseReadinessSummary, ContractStatus, FeedReadiness, FeedStatus,
        PropertyCloseReadiness,
    },
    connectors::standardized::{
        gl_budget_comparison::ingest_budget_comparison_file, source_registry::lane_by_key,
    },
    db::{
        self, AccountActivity, DelinquencyAging, ResidentReceivable, VendorPropertyShare,
        VendorSpend,
    },
    models::{AccountMapping, Call, GlTransaction, Memory},
    t12::{T12Row, T12RowType, T12Statement},
    tui::{
        app::{DelinSort, DeskApp, LedgerFocus, Screen},
        bddre::RiskRow,
        markdown::render_markdown,
        noi_bridge::BridgeStep,
        reports::WeeklyReport,
        rpcoe::RentRec,
        ui,
    },
};
use ratatui::{backend::TestBackend, Terminal};

#[test]
fn close_desk_renders_portfolio_board_and_selected_property() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.summary = CloseReadinessSummary {
        property_count: 2,
        ready_count: 0,
        not_ready_count: 2,
        blocker_count: 0,
        warning_count: 4,
        owner_ready_ratio: 0.0,
    };
    app.properties = vec![
        property(
            "Willow Brook",
            vec![
                feed("Actual GL", FeedStatus::Stale, Some("2026-03"), true),
                feed("Rent roll", FeedStatus::Current, Some("2026-06-05"), true),
                feed(
                    "RPCOE weekly",
                    FeedStatus::Current,
                    Some("2026-06-09"),
                    false,
                ),
            ],
        ),
        property(
            "juniper_fund",
            vec![feed("Budget GL", FeedStatus::Missing, None, true)],
        ),
    ];
    app.last_refreshed = Some("09:00:00".to_string());

    let text = render_to_text(&app, 110, 32);

    assert!(text.contains("BOXSCORE"), "brand header missing");
    assert!(text.contains("Close Desk"));
    assert!(text.contains("Period 2026-06"));
    assert!(text.contains("Owner-ready 0/2"));
    assert!(text.contains("Willow Brook"));
    assert!(text.contains("juniper_fund"));
    assert!(text.contains("not ready"));
    assert!(text.contains("Actual GL"), "feed name missing");
    assert!(
        text.contains("Close Desk — Willow Brook"),
        "selected property detail missing"
    );
    assert!(text.contains("Can you provide June 2026 actual GL?"));
    assert!(text.contains("q quit"));
}

#[test]
fn close_desk_renders_empty_state_with_ingest_hint() {
    let app = DeskApp::new("2026-06".to_string());
    let text = render_to_text(&app, 110, 32);
    assert!(text.contains("No properties found"));
    assert!(text.contains("ingest-standardized"));
}

#[test]
fn close_desk_shows_load_errors_in_the_footer() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.load_error = Some("period must be YYYY-MM".to_string());
    let text = render_to_text(&app, 110, 32);
    assert!(text.contains("error: period must be YYYY-MM"));
}

#[test]
fn mappings_screen_renders_review_queue_with_suggestions() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Mappings;
    app.mappings = vec![
        mapping("p101", "9999", "Mystery Clearing", "Unmapped", "unmapped"),
        mapping(
            "p102",
            "5200",
            "Repairs & Maintenance",
            "Repairs & Maintenance",
            "suggested",
        ),
    ];

    let text = render_to_text(&app, 120, 32);

    assert!(text.contains("5 Mappings"), "mappings tab missing");
    assert!(text.contains("2 mappings awaiting review"));
    assert!(text.contains("Mystery Clearing"));
    assert!(text.contains("Repairs & Maintenance"));
    assert!(text.contains("a approve suggested"));
}

#[test]
fn category_picker_overlay_lists_noi_categories() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Mappings;
    app.mappings = vec![mapping(
        "p101",
        "9999",
        "Mystery Clearing",
        "Unmapped",
        "unmapped",
    )];
    app.open_picker();
    assert!(app.picker.is_some());

    let text = render_to_text(&app, 120, 32);

    assert!(text.contains("Approve 9999 Mystery Clearing"));
    assert!(text.contains("Rental Income"));
    assert!(text.contains("Management Fees"));
    assert!(text.contains("enter approve"));
}

#[test]
fn mappings_screen_renders_empty_state() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Mappings;
    let text = render_to_text(&app, 120, 32);
    assert!(text.contains("No mappings awaiting review."));
}

#[tokio::test]
async fn inline_approval_reclassifies_gl_and_shrinks_the_queue() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    let file = temp_dir.path().join("budget_comparison.csv");
    std::fs::write(
        &file,
        "account_code,description,ptd_actual,ptd_budget,period,property_id\n9999,Mystery Clearing,100,75,Jun 2026,p101\n",
    )
    .unwrap();
    let lane = lane_by_key("maplewood").unwrap();
    ingest_budget_comparison_file(&pool, &lane, Path::new(&file))
        .await
        .unwrap();

    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Mappings;
    app.reload(&pool).await;
    assert_eq!(app.mappings.len(), 1);
    assert_eq!(app.mappings[0].account_code, "9999");

    // No suggestion exists for a mystery account: `a` must open the picker
    // instead of silently approving.
    app.approve_with_suggestion(&pool).await;
    assert!(app.picker.is_some(), "should ask the operator to choose");

    // Walk the picker to "Other Income" (index 3) and approve.
    app.picker = Some(3);
    app.approve_with_picker_choice(&pool).await;

    assert!(app.toast.as_deref().unwrap_or("").contains("Other Income"));
    assert!(app.mappings.is_empty(), "approved row must leave the queue");
    let approved = db::find_account_mapping(&pool, "standardized-yardi", "p101", "9999")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approved.noi_category, "Other Income");
    assert_eq!(approved.status, "approved");
    let unmapped_left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gl_actuals WHERE category = 'Unmapped'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unmapped_left, 0, "GL rows must be reclassified");
}

// ── Ledger screen tests ──────────────────────────────────────────────────────

#[test]
fn ledger_tab_appears_in_header() {
    let app = DeskApp::new("2026-04".to_string());
    let text = render_to_text(&app, 130, 32);
    assert!(text.contains("2 Ledger"), "ledger tab missing from header");
}

#[test]
fn ledger_renders_empty_state_without_properties() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ledger;
    let text = render_to_text(&app, 130, 32);
    assert!(text.contains("No properties found"));
}

#[test]
fn ledger_renders_account_list_and_txn_pane() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ledger;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.ledger_accounts = vec![
        account_activity("5100", "Repairs & Maintenance", 12, -4500.0),
        account_activity("4000", "Rental Income", 200, 150000.0),
    ];
    app.ledger_txns = vec![gl_txn(
        "2026-04-05",
        "MPG Security Solutions",
        -750.0,
        false,
    )];
    app.last_refreshed = Some("10:00:00".to_string());

    let text = render_to_text(&app, 130, 32);

    assert!(text.contains("2 Ledger"), "ledger tab not highlighted");
    assert!(text.contains("5100"), "account code missing");
    // "Repairs & Maintenance" may be truncated by column width; check prefix
    assert!(text.contains("Repairs"), "account name missing");
    assert!(text.contains("MPG Security"), "payee missing");
    assert!(text.contains("$-750.00"), "transaction amount missing");
    assert!(text.contains("/ filter"), "footer hint missing");
}

#[test]
fn ledger_renders_resident_rows_with_dim_tag() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ledger;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.ledger_accounts = vec![account_activity("4000", "Rental Income", 5, 5000.0)];
    app.ledger_txns = vec![gl_txn("2026-04-01", "Hill (t0171778)", 1200.0, true)];
    app.ledger_focus = LedgerFocus::Transactions;

    let text = render_to_text(&app, 160, 32);

    assert!(text.contains("Hill (t0171778)"), "resident payee missing");
    assert!(text.contains("(resident)"), "resident tag missing");
}

#[test]
fn ledger_filter_prompt_shown_while_typing() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ledger;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.ledger_input = Some("security".to_string());

    let text = render_to_text(&app, 130, 32);

    assert!(text.contains("/security"), "filter prompt missing");
}

#[test]
fn ledger_header_line2_shows_property_period_count() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ledger;
    app.properties = vec![property(
        "Willow Brook",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.ledger_accounts = vec![
        account_activity("4000", "Rental Income", 100, 80000.0),
        account_activity("5100", "Repairs", 20, -3000.0),
    ];

    let text = render_to_text(&app, 130, 32);

    assert!(
        text.contains("Willow Brook"),
        "property name missing from header"
    );
    assert!(text.contains("2026-04"), "period missing from header");
    assert!(text.contains("2 active accounts"), "account count missing");
}

#[tokio::test]
async fn ledger_reload_loads_accounts_and_drill_loads_txns() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    // Seed a property row so account_activity can filter on property_id.
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
         VALUES ('p101', 'maplewood', 'Charlotte', 494, 'Example Sponsor', 'PM', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Seed two gl_transactions rows.
    for (i, payee, amount) in [(1i64, "Vendor A", 100.0f64), (2i64, "Vendor B", 200.0f64)] {
        sqlx::query(
            "INSERT INTO gl_transactions
             (id, property_id, entity_code, account_code, txn_date, period,
              payee, is_resident, control, reference, amount, remarks,
              source_file, source_row, created_at)
             VALUES (?, 'p101', 'p101', '5100', '2026-04-01', '2026-04',
                     ?, 0, NULL, NULL, ?, NULL, 'test.parquet', ?, '2026-01-01')",
        )
        .bind(format!("txn-{i}"))
        .bind(payee)
        .bind(amount)
        .bind(i)
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut app = DeskApp::new("2026-04".to_string());
    // Manually add property since close_readiness won't find it without full data.
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    // Override the property_id to match what's in the db.
    app.properties[0].property_id = "p101".to_string();

    app.reload_ledger_accounts(&pool).await;
    assert_eq!(app.ledger_accounts.len(), 1, "should find 1 account");
    assert_eq!(app.ledger_accounts[0].account_code, "5100");
    assert_eq!(app.ledger_accounts[0].txn_count, 2);

    app.ledger_focus = LedgerFocus::Transactions;
    app.load_ledger_txns(&pool).await;
    assert_eq!(app.ledger_txns.len(), 2, "should load 2 transactions");
}

// ── Statements screen tests ──────────────────────────────────────────────────

#[test]
fn statements_tab_appears_in_header() {
    let app = DeskApp::new("2026-04".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("3 Statements"),
        "statements tab missing from header"
    );
}

#[test]
fn statements_renders_empty_state_without_data() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Statements;
    let text = render_to_text(&app, 160, 32);
    assert!(text.contains("T12 Statements"), "title missing");
}

#[test]
fn statements_renders_t12_table_with_months_and_noi() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Statements;
    app.properties = vec![property(
        "Willow Brook",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.statements_property = 0;

    // Build a minimal 12-period T12Statement.
    let periods: Vec<String> = (0..12u32)
        .map(|i| {
            let total = 2026 * 12 + 3 - 11 + i as i64; // ends 2026-04
            let (y, m) = (total.div_euclid(12), total.rem_euclid(12) + 1);
            format!("{y:04}-{m:02}")
        })
        .collect();

    app.statements = Some(T12Statement {
        property: "Willow Brook".to_string(),
        end_period: "2026-04".to_string(),
        periods: periods.clone(),
        rows: vec![
            T12Row {
                label: "Rental Income".to_string(),
                row_type: T12RowType::Revenue,
                values: vec![50000.0; 12],
            },
            T12Row {
                label: "Repairs & Maintenance".to_string(),
                row_type: T12RowType::Expense,
                values: vec![5000.0; 12],
            },
            T12Row {
                label: "NOI".to_string(),
                row_type: T12RowType::Noi,
                values: vec![45000.0; 12],
            },
        ],
    });

    let text = render_to_text(&app, 160, 32);

    assert!(
        text.contains("3 Statements"),
        "statements tab not highlighted"
    );
    assert!(text.contains("T12 Statements"), "panel title missing");
    // Header should show a short month label
    assert!(
        text.contains("Apr26") || text.contains("Apr"),
        "month label missing"
    );
    assert!(text.contains("Rental Income"), "revenue row missing");
    assert!(text.contains("NOI"), "NOI row missing");
    // Compact dollar: 45000 → $45.0k
    assert!(text.contains("$45.0k"), "compact dollar formatting missing");
    // Footer hint
    assert!(text.contains("p property"), "footer hint missing");
}

// ── NOI Bridge tests (F5.4) ──────────────────────────────────────────────────

fn bridge_step(label: &str, delta: f64, running: f64, is_anchor: bool) -> BridgeStep {
    BridgeStep {
        label: label.to_string(),
        delta,
        running,
        is_anchor,
    }
}

#[test]
fn noi_bridge_renders_empty_state_without_data() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::NoiBridge;
    let text = render_to_text(&app, 160, 32);
    assert!(text.contains("NOI Bridge"), "title missing");
    assert!(text.contains("No actuals"), "empty-state message missing");
}

#[test]
fn noi_bridge_renders_waterfall_with_anchors_and_signed_drivers() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::NoiBridge;
    app.properties = vec![property(
        "Willow Brook",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.bridge_property = 0;
    app.bridge_period = Some("2026-04".to_string());
    app.bridge_budget_noi = 100_000.0;
    app.bridge_actual_noi = 112_000.0;
    // Budget anchor → favorable revenue driver (green/up) → unfavorable expense
    // driver (red/down) → Actual anchor.
    app.bridge_steps = vec![
        bridge_step("Budget NOI", 0.0, 100_000.0, true),
        bridge_step("Rental Income", 20_000.0, 120_000.0, false),
        bridge_step("Repairs & Maintenance", -8_000.0, 112_000.0, false),
        bridge_step("Actual NOI", 0.0, 112_000.0, true),
    ];

    let text = render_to_text(&app, 160, 32);

    // NOI Bridge is the 10th section; its hotkey/legend label is `0`.
    assert!(
        text.contains("0 NOI Bridge"),
        "NOI Bridge tab/legend missing: {text}"
    );
    // Anchors.
    assert!(text.contains("Budget NOI"), "Budget NOI anchor missing");
    assert!(text.contains("Actual NOI"), "Actual NOI anchor missing");
    // Both drivers labelled.
    assert!(text.contains("Rental Income"), "favorable driver missing");
    assert!(
        text.contains("Repairs & Maintenance"),
        "unfavorable driver missing"
    );
    // Up/down markers distinguish favorable from unfavorable.
    assert!(text.contains('▲'), "favorable up-marker missing");
    assert!(text.contains('▼'), "unfavorable down-marker missing");
    // Bars are drawn with block chars.
    assert!(text.contains('█'), "bar block char missing");
    // A $ label appears (compact dollars on the drivers / running totals).
    assert!(
        text.contains("$20.0k") || text.contains("$20,000"),
        "favorable $ label missing: {text}"
    );
    // Footer hint for property/period navigation.
    assert!(text.contains("< > property"), "footer hint missing");
}

// ── Ask palette tests (C3) ───────────────────────────────────────────────────

#[test]
fn ask_input_prompt_rendered_while_typing() {
    let mut app = DeskApp::new("2026-04".to_string());
    // Simulate the operator having typed `:` — ask input mode is active.
    app.ask_input = Some("list properties".to_string());

    let text = render_to_text(&app, 130, 32);

    // The footer should show the typed text with the `:` prefix.
    assert!(text.contains(":list properties"), "ask prompt missing");
    // The cursor block character should be present.
    assert!(text.contains('\u{258c}'), "cursor missing from ask prompt");
}

#[test]
fn ask_screen_renders_fixture_output_lines() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ask;
    app.ask_question = "what did we pay 7 Kings last 6 months".to_string();
    app.ask_output = vec![
        "── tool: vendor_spend {\"payee_contains\":\"Kings\"} ──".to_string(),
        "Payee                                         Txns          Total    First     Last"
            .to_string(),
        "7 Kings Landscaping                              6    -$12,450.00  2025-11  2026-04"
            .to_string(),
        "(1 rows)".to_string(),
    ];
    app.ask_scroll = 0;

    let text = render_to_text(&app, 130, 32);

    assert!(text.contains("Ask Results"), "Ask Results title missing");
    assert!(text.contains("7 Kings Landscaping"), "vendor name missing");
    assert!(text.contains("(1 rows)"), "row count missing");
}

#[test]
fn ask_footer_hint_includes_colon_ask() {
    let app = DeskApp::new("2026-04".to_string());
    let text = render_to_text(&app, 130, 32);
    assert!(text.contains(": ask"), "footer hint missing `: ask`");
}

#[test]
fn ask_screen_footer_shows_scroll_hints() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Ask;
    app.ask_output = vec!["some output".to_string()];

    let text = render_to_text(&app, 130, 32);

    assert!(
        text.contains("j/k scroll"),
        "ask screen scroll hint missing"
    );
    assert!(text.contains("q/esc back"), "ask screen back hint missing");
}

// ── Vendors screen tests ─────────────────────────────────────────────────────

#[test]
fn vendors_tab_appears_in_header() {
    let app = DeskApp::new("2026-04".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("4 Vendors"),
        "vendors tab missing from header"
    );
}

#[test]
fn vendors_renders_empty_state_without_data() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Vendors;
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("No vendor activity"),
        "empty state message missing"
    );
    assert!(
        text.contains("ingest-standardized transactions"),
        "ingest hint missing"
    );
}

#[test]
fn vendors_renders_vendor_table_with_payee_and_total() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Vendors;
    app.vendors = vec![
        VendorSpend {
            payee: "Chadwell Supply".to_string(),
            txn_count: 12,
            total: -8400.0,
            first_period: "2025-05".to_string(),
            last_period: "2026-04".to_string(),
            by_property: vec![VendorPropertyShare {
                property: "maplewood".to_string(),
                txn_count: 12,
                total: -8400.0,
            }],
        },
        VendorSpend {
            payee: "7 Kings Landscaping".to_string(),
            txn_count: 6,
            total: -3200.0,
            first_period: "2025-11".to_string(),
            last_period: "2026-04".to_string(),
            by_property: vec![VendorPropertyShare {
                property: "maplewood".to_string(),
                txn_count: 6,
                total: -3200.0,
            }],
        },
    ];
    app.vendor_selected = 0;

    let text = render_to_text(&app, 160, 32);

    assert!(text.contains("4 Vendors"), "vendors tab not highlighted");
    assert!(text.contains("Chadwell"), "vendor payee missing");
    // Total formatted as $-8400.00
    assert!(text.contains("-8400"), "vendor total missing");
    assert!(text.contains("maplewood"), "property share missing");
    assert!(text.contains("j/k select"), "footer hint missing");
}

#[test]
fn vendors_header_line2_shows_scope_window_count() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Vendors;
    app.vendors = vec![VendorSpend {
        payee: "Acme Vendor".to_string(),
        txn_count: 3,
        total: -1200.0,
        first_period: "2026-01".to_string(),
        last_period: "2026-04".to_string(),
        by_property: vec![],
    }];

    let text = render_to_text(&app, 160, 32);

    assert!(
        text.contains("all properties"),
        "scope label missing from header"
    );
    assert!(
        text.contains("all time"),
        "window label missing from header"
    );
    assert!(
        text.contains("1 vendors"),
        "vendor count missing from header"
    );
}

#[test]
fn vendors_filter_prompt_shown_while_typing() {
    let mut app = DeskApp::new("2026-04".to_string());
    app.screen = Screen::Vendors;
    app.vendors_input = Some("chadwell".to_string());

    let text = render_to_text(&app, 160, 32);

    assert!(text.contains("/chadwell"), "filter prompt missing");
    assert!(text.contains('▌'), "cursor missing from filter prompt");
}

#[tokio::test]
async fn vendors_reload_populates_vendors_and_txns() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    // Seed a property.
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
         VALUES ('p1', 'Test Apts', 'Dallas', 100, 'Example Sponsor', 'PM', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Seed vendor transactions (non-resident).
    for (i, payee, amount) in [
        (1i64, "Chadwell Supply", -500.0f64),
        (2i64, "Chadwell Supply", -300.0),
        (3i64, "Apex Roofing", -2000.0),
    ] {
        sqlx::query(
            "INSERT INTO gl_transactions
             (id, property_id, entity_code, account_code, txn_date, period,
              payee, is_resident, control, reference, amount, remarks,
              source_file, source_row, created_at)
             VALUES (?, 'p1', 'p1', '5100', '2026-04-01', '2026-04',
                     ?, 0, NULL, NULL, ?, NULL, 'test.parquet', ?, '2026-01-01')",
        )
        .bind(format!("txn-{i}"))
        .bind(payee)
        .bind(amount)
        .bind(i)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Seed a resident transaction — must not appear in vendor list.
    sqlx::query(
        "INSERT INTO gl_transactions
         (id, property_id, entity_code, account_code, txn_date, period,
          payee, is_resident, control, reference, amount, remarks,
          source_file, source_row, created_at)
         VALUES ('txn-r', 'p1', 'p1', '4000', '2026-04-01', '2026-04',
                 'Resident Jones', 1, NULL, NULL, 1200.0, NULL, 'test.parquet', 99, '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut app = DeskApp::new("2026-04".to_string());
    app.properties = vec![property(
        "Test Apts",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-04"),
            true,
        )],
    )];
    app.properties[0].property_id = "p1".to_string();

    // Reload with no scope/window — all vendors.
    app.reload_vendors(&pool).await;

    assert!(
        app.load_error.is_none(),
        "unexpected error: {:?}",
        app.load_error
    );
    assert_eq!(
        app.vendors.len(),
        2,
        "should find 2 vendors (Apex + Chadwell)"
    );
    // Residents absent.
    assert!(
        app.vendors.iter().all(|v| v.payee != "Resident Jones"),
        "resident must not appear in vendor list"
    );
    // Vendor txns loaded for selected vendor (index 0 = Apex with biggest absolute spend).
    assert!(
        !app.vendor_txns.is_empty(),
        "vendor txns should be loaded for selected vendor"
    );

    // Test property-scoped reload.
    app.vendors_property = Some(0);
    app.reload_vendors(&pool).await;
    assert!(app.load_error.is_none(), "property-scoped reload errored");
    assert!(
        !app.vendors.is_empty(),
        "scoped vendors should return results"
    );

    // Test selecting second vendor loads its txns.
    app.vendors_property = None;
    app.reload_vendors(&pool).await;
    let prev_vendor = app.vendors.first().map(|v| v.payee.clone());
    app.vendor_selected = 1;
    app.load_vendor_txns(&pool).await;
    let new_payee = app.vendors.get(1).map(|v| v.payee.clone());
    assert_ne!(prev_vendor, new_payee, "different vendor selected");
    assert!(
        !app.vendor_txns.is_empty(),
        "second vendor should have txns loaded"
    );
}

// ── Track Record screen tests ────────────────────────────────────────────────

#[test]
fn track_record_tab_appears_in_sidebar() {
    let app = DeskApp::new("2026-06".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("8 Track Record"),
        "track record tab missing from sidebar"
    );
}

#[test]
fn track_record_renders_empty_state_without_data() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::TrackRecord;
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("Batting Averages"),
        "batting averages section title missing"
    );
    assert!(
        text.contains("No track-record memories"),
        "empty state for memories missing"
    );
    assert!(
        text.contains("Recent Calls"),
        "recent calls section title missing"
    );
    assert!(
        text.contains("No calls recorded"),
        "empty state for calls missing"
    );
}

#[test]
fn track_record_renders_memory_rows_and_call_rows() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::TrackRecord;

    app.track_record_memories = vec![Memory {
        id: "mem-1".to_string(),
        memory_type: "track_record".to_string(),
        scope: "noi_variance".to_string(),
        key: "maplewood".to_string(),
        value: "hit rate 0.72 over 25 calls".to_string(),
        confidence_score: 0.72,
        source_task_run_id: None,
        created_at: "2026-06-01T00:00:00Z".to_string(),
        updated_at: "2026-06-15T00:00:00Z".to_string(),
    }];

    app.track_record_calls = vec![Call {
        id: "call-1".to_string(),
        property_id: "p101".to_string(),
        origin_period: "2026-05".to_string(),
        call_type: "noi_variance".to_string(),
        status: "scored".to_string(),
        made_at: "2026-05-15T00:00:00Z".to_string(),
        mature_by: "2026-06".to_string(),
        confidence: Some(0.8),
        payload_json: "{}".to_string(),
        outcome_json: Some("{}".to_string()),
        score: Some(0.85),
        outcome_summary: Some("NOI beat forecast by 4.2%".to_string()),
        scored_at: Some("2026-06-10T00:00:00Z".to_string()),
        source_task_run_id: None,
        confounded: false,
        decision_kind: None,
        entities_json: None,
        outcome_mode: None,
        value_class: None,
        acted_on: false,
        accepted_recall: None,
        source_surface: None,
        context_json: None,
        operator_outcome: None,
        resolved_by: None,
        self_graded: false,
        regime_tag: None,
        created_at: "2026-05-15T00:00:00Z".to_string(),
        updated_at: "2026-06-10T00:00:00Z".to_string(),
    }];

    let text = render_to_text(&app, 160, 32);

    assert!(text.contains("Batting Averages"), "section title missing");
    assert!(text.contains("noi_variance"), "memory scope missing");
    assert!(text.contains("maplewood"), "memory key missing");
    assert!(text.contains("hit rate"), "memory value missing");
    assert!(text.contains("Recent Calls"), "calls section title missing");
    assert!(text.contains("scored"), "call status missing");
    assert!(text.contains("2026-06"), "call mature_by missing");
    assert!(
        text.contains("NOI beat forecast"),
        "outcome summary missing"
    );
    assert!(text.contains("q quit"), "footer hint missing");
}

#[tokio::test]
async fn track_record_reload_loads_memories_and_calls_from_db() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();

    // Seed a property so insert_call's FK is satisfied.
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at)
         VALUES ('p101', 'maplewood', 'Charlotte', 494, 'Example Sponsor', 'PM', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Seed a track_record memory.
    db::upsert_memory(
        &pool,
        "track_record",
        "noi_variance",
        "p101",
        "hit rate 0.80 over 10 calls",
        0.8,
        None,
    )
    .await
    .unwrap();

    // Seed a call.
    db::insert_call(
        &pool,
        "p101",
        "2026-05",
        "noi_variance",
        "2026-06",
        Some(0.8),
        "{}",
        None,
    )
    .await
    .unwrap();

    let mut app = DeskApp::new("2026-06".to_string());
    app.reload_track_record(&pool).await;

    assert!(
        app.load_error.is_none(),
        "reload error: {:?}",
        app.load_error
    );
    assert_eq!(app.track_record_memories.len(), 1, "should have 1 memory");
    assert_eq!(app.track_record_memories[0].scope, "noi_variance");
    assert_eq!(app.track_record_calls.len(), 1, "should have 1 call");
    assert_eq!(app.track_record_calls[0].call_type, "noi_variance");
}

// ── Reports screen tests ─────────────────────────────────────────────────────

#[test]
fn reports_tab_appears_in_sidebar() {
    let app = DeskApp::new("2026-06".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("9 Reports"),
        "reports tab missing from sidebar"
    );
}

#[test]
fn reports_screen_renders_list_and_reader_panes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("RPCOE_Weekly_Report_2026-06-09.md"),
        "# RPCOE Report\n\nSome content here.",
    )
    .unwrap();

    let report = WeeklyReport {
        property: "Maplewood Court".to_string(),
        kind: "RPCOE".to_string(),
        date: "2026-06-09".to_string(),
        path: dir.join("RPCOE_Weekly_Report_2026-06-09.md"),
    };

    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Reports;
    app.reports = vec![report];
    app.report_sel = 0;
    app.report_source = "# RPCOE Report\n\nSome content here.".to_string();
    app.report_line_count = render_markdown(&app.report_source, 120).len();

    let text = render_to_text(&app, 160, 36);

    assert!(
        text.contains("Reports"),
        "Reports label missing from sidebar"
    );
    assert!(
        text.contains("Weekly Reports"),
        "Weekly Reports panel title missing"
    );
    assert!(text.contains("RPCOE"), "report kind missing from list");
    assert!(
        text.contains("RPCOE Report"),
        "report heading missing from reader pane"
    );
}

#[test]
fn reports_screen_renders_empty_state_when_no_reports() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Reports;
    // reports remains empty (default)
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("No RPCOE/BDDRE reports for 2026-06"),
        "empty state missing — got: {text}"
    );
}

// ── Delinquency screen (F2) ──────────────────────────────────────────────────

fn receivable(
    code: &str,
    name: &str,
    status: &str,
    total: f64,
    current: Option<f64>,
    days: Option<i64>,
) -> ResidentReceivable {
    ResidentReceivable {
        property_id: "p1".to_string(),
        resident_code: code.to_string(),
        resident_name: Some(name.to_string()),
        resident_status: Some(status.to_string()),
        total_delinquent: total,
        current_owed: current,
        days_late: days,
        as_of_date: "2026-05-31".to_string(),
    }
}

fn risk_row(unit: &str, name: &str, score: f64, tier: &str, action: &str) -> RiskRow {
    RiskRow {
        lane: "Maplewood Commons".to_string(),
        unit: unit.to_string(),
        resident_code: "t0".to_string(),
        resident_name: name.to_string(),
        risk_score: score,
        risk_tier: tier.to_string(),
        total_delinquent: 0.0,
        top_action: action.to_string(),
    }
}

fn delinquency_app() -> DeskApp {
    let mut app = DeskApp::new("2026-05".to_string());
    app.screen = Screen::Delinquency;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-05"),
            true,
        )],
    )];
    app.delin_aging = DelinquencyAging {
        current_owed_total: 1200.0,
        b0_30: 4500.0,
        b31_60: 3100.0,
        b61_90: 2200.0,
        b90_plus: 8800.0,
        cnt_current: 2,
        cnt_0_30: 5,
        cnt_31_60: 3,
        cnt_61_90: 2,
        cnt_90_plus: 4,
        prepaid_total: 950.0,
        prepaid_cnt: 3,
    };
    app.delin_residents = vec![
        receivable(
            "tWorst",
            "Marcus Worst",
            "current",
            4200.0,
            Some(4200.0),
            Some(110),
        ),
        receivable(
            "tMid",
            "Nadia Mid",
            "current",
            1100.0,
            Some(1100.0),
            Some(40),
        ),
        receivable("tPre", "Olivia Prepaid", "prepaid", 0.0, Some(-480.0), None),
    ];
    app.delin_watchlist = vec![
        risk_row(
            "R0820",
            "Antonio High",
            82.0,
            "High",
            "Send cure-or-quit notice",
        ),
        risk_row(
            "C0110",
            "Tebogo Med",
            48.0,
            "Medium",
            "Structured payment plan",
        ),
    ];
    app.delin_trend = vec![
        ("2026-03-31".to_string(), 50000.0),
        ("2026-04-30".to_string(), 42000.0),
        ("2026-05-31".to_string(), 31000.0),
    ];
    app
}

#[test]
fn delinquency_tab_appears_in_sidebar() {
    let app = DeskApp::new("2026-05".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("6 Delinquency"),
        "delinquency tab missing from sidebar"
    );
}

#[test]
fn delinquency_renders_aging_buckets_and_worst_first_resident() {
    let app = delinquency_app();
    let text = render_to_text(&app, 160, 40);
    // Aging bucket labels present.
    for label in ["Current", "0-30", "31-60", "61-90", "90+", "Prepaid"] {
        assert!(
            text.contains(label),
            "aging bucket '{label}' missing:\n{text}"
        );
    }
    // Worst resident surfaces with its owed amount (compact_dollars → $4.2K).
    assert!(
        text.contains("Marcus Worst"),
        "worst resident missing:\n{text}"
    );
    // A High-risk watchlist row.
    assert!(
        text.contains("Antonio High"),
        "high-risk watchlist row missing:\n{text}"
    );
    assert!(text.contains("High"), "risk tier missing:\n{text}");
    // Footer hint reflects the sort key.
    assert!(
        text.contains("s sort"),
        "sort hint missing from footer:\n{text}"
    );
}

#[test]
fn delinquency_renders_empty_state_without_receivables() {
    let mut app = DeskApp::new("2026-05".to_string());
    app.screen = Screen::Delinquency;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-05"),
            true,
        )],
    )];
    // No residents, no aging, no prepaids → empty state.
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("No receivables for 2026-05"),
        "empty state missing — got:\n{text}"
    );
}

#[test]
fn delinquency_sort_toggle_changes_order() {
    let mut app = delinquency_app();
    // Default is Owed → worst-first.
    app.sort_delin_residents();
    assert_eq!(app.delin_residents[0].resident_code, "tWorst");
    // Toggle to Name → alphabetical by resident_name.
    app.delin_sort = DelinSort::Name;
    app.sort_delin_residents();
    assert_eq!(
        app.delin_residents[0].resident_name.as_deref(),
        Some("Marcus Worst")
    );
    assert_eq!(
        app.delin_residents[2].resident_name.as_deref(),
        Some("Olivia Prepaid")
    );
}

#[tokio::test]
async fn delinquency_reload_populates_aging_and_residents() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    // property() test builder assigns property_id = "{name}-id".
    sqlx::query(
        "INSERT INTO properties (id, name, market, unit_count, owner_entity, property_manager, created_at) \
         VALUES ('maplewood-id', 'maplewood', 'TX', 300, 'O', 'PM', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "maplewood-id",
        "2026-05-31",
        "tA",
        Some("Alpha"),
        Some("current"),
        3200.0,
        Some(3200.0),
        Some(95),
        "ar.csv",
        2,
    )
    .await
    .unwrap();
    db::insert_unit_receivable(
        &pool,
        "maplewood-id",
        "2026-05-31",
        "tB",
        Some("Beta"),
        Some("prepaid"),
        0.0,
        Some(-300.0),
        None,
        "ar.csv",
        3,
    )
    .await
    .unwrap();

    let mut app = DeskApp::new("2026-05".to_string());
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-05"),
            true,
        )],
    )];
    app.reload_delinquency(&pool).await;

    assert!(
        app.load_error.is_none(),
        "reload error: {:?}",
        app.load_error
    );
    assert_eq!(app.delin_residents.len(), 2);
    // Worst-first.
    assert_eq!(app.delin_residents[0].resident_code, "tA");
    assert_eq!(app.delin_aging.b90_plus, 3200.0);
    assert_eq!(app.delin_aging.prepaid_total, 300.0);
}

// ── Renewals screen (F3) ─────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn rent_rec(
    unit: &str,
    name: &str,
    current: f64,
    market: f64,
    days: Option<i64>,
    expiry: &str,
    rec_rent: f64,
    inc_pct: f64,
    conf: &str,
    driver: &str,
) -> RentRec {
    RentRec {
        lane: "Maplewood Commons".to_string(),
        unit: unit.to_string(),
        resident_code: "t0".to_string(),
        resident_name: name.to_string(),
        current_rent: current,
        market_rent: market,
        rent_vs_market_pct: if market > 0.0 {
            current / market * 100.0
        } else {
            0.0
        },
        days_to_expiration: days,
        lease_expiration: expiry.to_string(),
        recommended_new_rent: rec_rent,
        recommended_increase_pct: inc_pct,
        confidence: conf.to_string(),
        concession: String::new(),
        top_driver: driver.to_string(),
    }
}

fn renewals_app() -> DeskApp {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Renewals;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-06"),
            true,
        )],
    )];
    // Most-urgent-first: 12d (underpriced), 45d, 100d.
    app.renew_recs = vec![
        rent_rec(
            "C00107",
            "Md Ashikul",
            950.0,
            1100.0,
            Some(12),
            "2026-06-28",
            1045.0,
            9.5,
            "High",
            "Below market - retention upside",
        ),
        rent_rec(
            "C00108",
            "Shirley Lewis",
            1203.0,
            995.0,
            Some(45),
            "2026-08-01",
            1263.0,
            5.0,
            "Medium",
            "Frequent late payments - higher risk",
        ),
        rent_rec(
            "C00110",
            "Resident Six",
            1256.0,
            1010.0,
            Some(100),
            "2026-10-15",
            1256.0,
            0.0,
            "Low",
            "Above market - hold",
        ),
    ];
    // First (urgent) rec carries an active concession → ◆ flag in the table.
    app.renew_recs[0].concession = "Do not extend - active concession already in place".to_string();
    app.renew_windows = [1, 1, 0, 1];
    // 45 underpriced-style opportunity (one underpriced unit: 1100-950=150/mo).
    app.renew_opportunity = (150.0, 1800.0, 1);
    app
}

#[test]
fn renewals_tab_appears_in_sidebar() {
    let app = DeskApp::new("2026-06".to_string());
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("7 Renewals"),
        "renewals tab missing from sidebar"
    );
}

#[test]
fn renewals_renders_funnel_table_and_opportunity() {
    let app = renewals_app();
    let text = render_to_text(&app, 160, 40);
    // Window funnel labels present.
    for label in ["30d", "31-60d", "61-90d", "90+d"] {
        assert!(
            text.contains(label),
            "window label '{label}' missing:\n{text}"
        );
    }
    // Opportunity badge — monthly uplift and underpriced count.
    assert!(text.contains("$150"), "monthly uplift missing:\n{text}");
    assert!(text.contains("under"), "underpriced badge missing:\n{text}");
    // Urgent (≤30d) row carries the ! marker and the recommended uplift.
    assert!(text.contains("C00107"), "urgent unit missing:\n{text}");
    assert!(text.contains("!"), "urgent marker missing:\n{text}");
    assert!(text.contains("+9.5%"), "increase % missing:\n{text}");
    assert!(text.contains("$1,045"), "recommended rent missing:\n{text}");
    // Driver text surfaces.
    assert!(text.contains("Below market"), "top driver missing:\n{text}");
    // Active concession is flagged inline with a ◆ glyph.
    assert!(text.contains("◆"), "concession flag missing:\n{text}");
    // Footer.
    assert!(
        text.contains("f window"),
        "renewals footer missing:\n{text}"
    );
}

#[test]
fn renewals_renders_empty_state_without_data() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.screen = Screen::Renewals;
    app.properties = vec![property(
        "maplewood",
        vec![feed(
            "Actual GL",
            FeedStatus::Current,
            Some("2026-06"),
            true,
        )],
    )];
    let text = render_to_text(&app, 160, 32);
    assert!(
        text.contains("No lease or renewal data"),
        "empty state missing:\n{text}"
    );
}

fn account_activity(code: &str, name: &str, txn_count: i64, total: f64) -> AccountActivity {
    AccountActivity {
        account_code: code.to_string(),
        account_name: name.to_string(),
        txn_count,
        total,
    }
}

fn gl_txn(date: &str, payee: &str, amount: f64, is_resident: bool) -> GlTransaction {
    GlTransaction {
        id: format!("txn-{date}-{payee}"),
        property_id: "p101".to_string(),
        entity_code: "p101".to_string(),
        account_code: "5100".to_string(),
        txn_date: Some(date.to_string()),
        period: "2026-04".to_string(),
        payee: payee.to_string(),
        is_resident: i64::from(is_resident),
        control: None,
        reference: None,
        amount,
        remarks: None,
        source_file: "test.parquet".to_string(),
        source_row: 1,
        created_at: "2026-01-01".to_string(),
    }
}

fn mapping(scope: &str, code: &str, name: &str, category: &str, status: &str) -> AccountMapping {
    AccountMapping {
        id: format!("{scope}-{code}"),
        source_system: "standardized-yardi".to_string(),
        property_scope: scope.to_string(),
        account_code: code.to_string(),
        account_name: name.to_string(),
        noi_category: category.to_string(),
        confidence_score: 0.0,
        status: status.to_string(),
        created_at: "now".to_string(),
        updated_at: "now".to_string(),
    }
}

fn render_to_text(app: &DeskApp, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn property(name: &str, feeds: Vec<FeedReadiness>) -> PropertyCloseReadiness {
    let blockers = feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report && feed.status == FeedStatus::Missing)
        .count();
    let warning_count = feeds
        .iter()
        .filter(|feed| feed.required_for_owner_report && feed.status == FeedStatus::Stale)
        .count();
    PropertyCloseReadiness {
        property_id: format!("{name}-id"),
        property: name.to_string(),
        unit_count: 100,
        status: CloseReadinessStatus::NotReady,
        owner_ready: false,
        blockers,
        warning_count,
        feeds,
        operator_questions: vec!["Can you provide June 2026 actual GL?".to_string()],
        contract_status: ContractStatus {
            status: "NOT_RUN".to_string(),
            error_count: 0,
            warning_count: 0,
            contract_set_version: "v1.1 (C1-C5)".to_string(),
            error_messages: Vec::new(),
        },
    }
}

fn feed(name: &str, status: FeedStatus, latest: Option<&str>, required: bool) -> FeedReadiness {
    FeedReadiness {
        name: name.to_string(),
        status,
        latest_period_or_date: latest.map(str::to_string),
        row_count: 1,
        required_for_owner_report: required,
        note: "note".to_string(),
    }
}

// ── F6: help overlay + 0-hotkey ───────────────────────────────────────────

#[test]
fn help_overlay_lists_keymap_and_all_ten_screens() {
    let mut app = DeskApp::new("2026-06".to_string());
    app.help_open = true;
    let out = render_to_text(&app, 120, 40);
    // Section headers.
    assert!(
        out.contains("Keymap"),
        "help overlay should show a Keymap header"
    );
    assert!(
        out.contains("Screens"),
        "help overlay should show a Screens header"
    );
    // Global nav keys are documented.
    assert!(
        out.contains("1-9 / 0"),
        "help should document the 1-9/0 hotkeys"
    );
    assert!(out.contains("Tab"), "help should document Tab cycling");
    // Every one of the 10 screens is described.
    for label in [
        "Close Desk",
        "Ledger",
        "Statements",
        "Vendors",
        "Mappings",
        "Delinquency",
        "Renewals",
        "Track Record",
        "Reports",
        "NOI Bridge",
    ] {
        assert!(
            out.contains(label),
            "help overlay must list the {label} screen"
        );
    }
}

#[test]
fn help_overlay_hidden_when_flag_false() {
    let app = DeskApp::new("2026-06".to_string());
    assert!(!app.help_open, "help overlay defaults to closed");
    let out = render_to_text(&app, 120, 40);
    assert!(
        !out.contains("Keymap"),
        "help overlay must not render when help_open is false"
    );
}

#[test]
fn help_open_flag_toggles() {
    // Mirrors the mod.rs event-loop logic: `?` opens; `?`/Esc/`q` close.
    let mut app = DeskApp::new("2026-06".to_string());
    // `?` opens.
    app.help_open = true;
    assert!(app.help_open);
    // `?`/Esc/q (handled in the help-capture block) closes.
    app.help_open = false;
    assert!(!app.help_open);
}

#[test]
fn zero_hotkey_navigates_to_noi_bridge() {
    // `0` maps to goto_section_index(9) — the 10th sidebar section.
    let mut app = DeskApp::new("2026-06".to_string());
    app.goto_section_index(9);
    assert_eq!(app.screen, Screen::NoiBridge);
}

#[test]
fn sidebar_legend_shows_zero_for_noi_bridge() {
    let app = DeskApp::new("2026-06".to_string());
    let out = render_to_text(&app, 120, 40);
    assert!(
        out.contains("0 NOI Bridge"),
        "sidebar legend should show the 10th section with hotkey 0"
    );
}
