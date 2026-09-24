//! Boxscore workbench application state.

use sqlx::SqlitePool;

use crate::{
    account_review::{self, MappingApproval},
    close_readiness::{self, CloseReadinessSummary, PropertyCloseReadiness},
    db::{
        self, AccountActivity, DelinquencyAging, PortfolioRollup, ResidentReceivable, UnitLeaseRow,
    },
    models::{AccountMapping, Call, GlTransaction, Memory},
    ontology,
    t12::{self, T12Statement},
    tui::{
        bddre::RiskRow,
        noi_bridge::{budget_noi_from_variances, BridgeStep},
        rpcoe::RentRec,
    },
};

/// Screens that appear as leaf nodes in the sidebar tree (Ask is not a tree node).
pub const SIDEBAR_SCREENS: &[Screen] = &[
    Screen::CloseDesk,
    Screen::Ledger,
    Screen::Statements,
    Screen::Vendors,
    Screen::Mappings,
    Screen::Delinquency,
    Screen::Renewals,
    Screen::TrackRecord,
    Screen::Reports,
    // 10th sidebar entry → triggers R1 (hotkeys past 9). Until F6.2 maps
    // hotkey `0` to section 10, it is reachable via Tab / BackTab cycling.
    Screen::NoiBridge,
];

/// A navigable leaf node in the sidebar property tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarItem {
    pub property_idx: usize,
    pub screen: Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    CloseDesk,
    Mappings,
    Ledger,
    Statements,
    /// Visual NOI bridge: budget-NOI → actual-NOI waterfall with horizontal
    /// block bars (favorable green up, unfavorable red down) per property/period.
    NoiBridge,
    Vendors,
    /// Delinquency / Collections: aged receivables, aging buckets, BDDRE
    /// risk watchlist, and a short delinquency trend.
    Delinquency,
    /// Renewals / Lease-Expiration: expirations-by-window funnel, per-unit
    /// renewal recommendations (current vs market vs recommended rent), and the
    /// estimated renewal opportunity (underpriced units).
    Renewals,
    /// Read-only track record: batting averages (memories) + recent calls.
    TrackRecord,
    /// Weekly report browser — RPCOE/BDDRE markdown reader.
    Reports,
    /// Results viewer for a completed `run_ask` query.
    Ask,
}

/// Which pane the cursor is in on the Ledger screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerFocus {
    Accounts,
    Transactions,
}

/// Sort order for the per-resident receivables table on the Delinquency screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelinSort {
    /// Worst-first by amount owed (default).
    Owed,
    /// Alphabetical by resident name.
    Name,
    /// Grouped by resident status.
    Status,
}

impl DelinSort {
    /// Cycle Owed → Name → Status → Owed.
    pub fn next(self) -> Self {
        match self {
            DelinSort::Owed => DelinSort::Name,
            DelinSort::Name => DelinSort::Status,
            DelinSort::Status => DelinSort::Owed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DelinSort::Owed => "owed",
            DelinSort::Name => "name",
            DelinSort::Status => "status",
        }
    }
}

pub struct DeskApp {
    pub screen: Screen,
    pub period: String,
    pub summary: CloseReadinessSummary,
    /// Portfolio-wide roll-up rendered as a compact band atop the Close Desk
    /// (units, owner-ready ratio, occupancy range, NOI Δ vs budget, delinquency).
    pub portfolio: PortfolioRollup,
    pub properties: Vec<PropertyCloseReadiness>,
    pub selected: usize,
    pub mappings: Vec<AccountMapping>,
    pub mapping_selected: usize,
    /// When `Some`, the category picker overlay is open; the value is the
    /// highlighted index into [`ontology::NOI_CATEGORIES`].
    pub picker: Option<usize>,
    pub last_refreshed: Option<String>,
    pub load_error: Option<String>,
    pub toast: Option<String>,
    /// When `true`, the `?` help overlay modal is open. It captures all input
    /// (like the category picker) and renders centered over any screen.
    pub help_open: bool,

    // ── Statements screen state ──────────────────────────────────────────────
    /// The assembled T12 statement (None until first load).
    pub statements: Option<T12Statement>,
    /// Index into `properties` for the statements property selector.
    pub statements_property: usize,
    /// The end period currently displayed on the Statements screen.
    pub statements_end_period: Option<String>,

    // ── NOI Bridge screen state ───────────────────────────────────────────────
    /// Index into `properties` for the NOI-bridge property selector.
    pub bridge_property: usize,
    /// The period currently displayed on the NOI Bridge screen (None until
    /// resolved to the latest period with actuals for that property).
    pub bridge_period: Option<String>,
    /// The assembled budget→actual waterfall steps (empty until first load).
    pub bridge_steps: Vec<BridgeStep>,
    /// Budget NOI anchor for the current property/period.
    pub bridge_budget_noi: f64,
    /// Actual NOI anchor for the current property/period.
    pub bridge_actual_noi: f64,

    // ── Ledger screen state ───────────────────────────────────────────────
    /// Index into `properties` for the ledger property selector.
    pub ledger_property: usize,
    /// Account-level summary rows for the selected property + period.
    pub ledger_accounts: Vec<AccountActivity>,
    /// Cursor within the account list pane.
    pub ledger_account_selected: usize,
    /// Transaction rows for the currently selected account (or filter result).
    pub ledger_txns: Vec<GlTransaction>,
    /// Cursor within the transaction pane.
    pub ledger_txn_selected: usize,
    /// Which pane has keyboard focus on the Ledger screen.
    pub ledger_focus: LedgerFocus,
    /// `Some(text)` when a filter has been applied; `None` means showing the
    /// selected account's transactions.
    pub ledger_filter: Option<String>,
    /// `Some(text)` while the operator is typing a `/` search query; `None`
    /// when the typing mode is not active.
    pub ledger_input: Option<String>,

    // ── Vendors screen state ─────────────────────────────────────────────────
    /// Ranked vendor spend list (fetched on screen entry).
    pub vendors: Vec<db::VendorSpend>,
    /// Cursor within the vendor list.
    pub vendor_selected: usize,
    /// Recent transactions for the selected vendor (15 rows).
    pub vendor_txns: Vec<GlTransaction>,
    /// `Some(3|6|12)` = T-window in months; `None` = all time.
    pub vendors_window_months: Option<i64>,
    /// `None` = all properties, `Some(i)` = index into `properties`.
    pub vendors_property: Option<usize>,
    /// Applied payee filter (LIKE substring).
    pub vendors_filter: Option<String>,
    /// `Some(text)` while the operator is typing a `/` payee filter.
    pub vendors_input: Option<String>,

    // ── Ask palette state ────────────────────────────────────────────────────
    /// `Some(text)` while the operator is typing a `:` ask query; `None` when
    /// the ask input mode is not active.
    pub ask_input: Option<String>,
    /// The rendered table lines returned by `run_ask` (for `Screen::Ask`).
    pub ask_output: Vec<String>,
    /// The question that produced the current `ask_output`.
    pub ask_question: String,
    /// Scroll offset (line index) for the Ask results view.
    pub ask_scroll: usize,
    /// Visible line count of the Ask results viewport, updated by the event
    /// loop after each draw so scroll bounds can be exact.
    pub ask_viewport: usize,
    // ── Track Record screen state ────────────────────────────────────────────
    /// Track-record memories (memory_type = 'track_record'), most recent 100.
    pub track_record_memories: Vec<Memory>,
    /// All calls ordered newest-first (capped at display in UI).
    pub track_record_calls: Vec<Call>,

    // ── Reports screen state ─────────────────────────────────────────────────
    /// Discovered weekly report files, sorted newest-first.
    pub reports: Vec<crate::tui::reports::WeeklyReport>,
    /// Cursor within the reports list.
    pub report_sel: usize,
    /// Vertical scroll offset for the report reader pane.
    pub report_scroll: u16,
    /// Raw markdown source for the currently selected report (rendered at draw time).
    pub report_source: String,
    /// Stable line count for scroll clamping (render_markdown does not wrap, so this
    /// is width-independent).
    pub report_line_count: usize,

    // ── Delinquency screen state ─────────────────────────────────────────────
    /// Index into `properties` for the delinquency property selector.
    pub delin_property: usize,
    /// Aging roll-up for the selected property + period.
    pub delin_aging: DelinquencyAging,
    /// Per-resident receivables (sorted per `delin_sort`).
    pub delin_residents: Vec<ResidentReceivable>,
    /// Cursor within the resident table.
    pub delin_resident_sel: usize,
    /// Active sort order for the resident table.
    pub delin_sort: DelinSort,
    /// BDDRE risk watchlist rows (lane CSV; demo falls back to all lanes).
    pub delin_watchlist: Vec<RiskRow>,
    /// Recent delinquency-amount trend `(as_of_date, delinquent_amount)`.
    pub delin_trend: Vec<(String, f64)>,

    // ── Renewals screen state ────────────────────────────────────────────────
    /// Index into `properties` for the renewals property selector.
    pub renew_property: usize,
    /// Per-unit lease rows (DB) for the selected property + period, biggest
    /// market-vs-charge upside first.
    pub renew_leases: Vec<UnitLeaseRow>,
    /// RPCOE renewal recommendations (lane CSV), most-urgent-first, after the
    /// active window filter is applied.
    pub renew_recs: Vec<RentRec>,
    /// Cursor within the renewal recommendations table.
    pub renew_sel: usize,
    /// Expiration-window counts `[<=30, 31-60, 61-90, 90+]` (unfiltered).
    pub renew_windows: [usize; 4],
    /// Active expiration-window filter: `None` = all, `Some(0..=3)` = a window.
    pub renew_window_filter: Option<usize>,
    /// Renewal opportunity `(monthly_uplift, annual_uplift, underpriced_cnt)`.
    pub renew_opportunity: (f64, f64, usize),

    /// The screen that was active before entering `Screen::Ask`; used by
    /// `Esc`/`q` on the Ask screen to return instead of quitting.
    pub previous_screen: Screen,
    /// Flat cursor index into `sidebar_items()` — drives active property + screen.
    pub sidebar_cursor: usize,
    /// True when keyboard focus is in the sidebar; false when in the main panel.
    pub sidebar_focused: bool,
}

impl DeskApp {
    pub fn new(period: String) -> Self {
        Self {
            screen: Screen::CloseDesk,
            period,
            summary: empty_summary(),
            portfolio: PortfolioRollup::default(),
            properties: Vec::new(),
            selected: 0,
            mappings: Vec::new(),
            mapping_selected: 0,
            picker: None,
            last_refreshed: None,
            load_error: None,
            toast: None,
            help_open: false,
            statements: None,
            statements_property: 0,
            statements_end_period: None,
            bridge_property: 0,
            bridge_period: None,
            bridge_steps: Vec::new(),
            bridge_budget_noi: 0.0,
            bridge_actual_noi: 0.0,
            ledger_property: 0,
            ledger_accounts: Vec::new(),
            ledger_account_selected: 0,
            ledger_txns: Vec::new(),
            ledger_txn_selected: 0,
            ledger_focus: LedgerFocus::Accounts,
            ledger_filter: None,
            ledger_input: None,
            vendors: Vec::new(),
            vendor_selected: 0,
            vendor_txns: Vec::new(),
            vendors_window_months: None,
            vendors_property: None,
            vendors_filter: None,
            vendors_input: None,
            ask_input: None,
            ask_output: Vec::new(),
            ask_question: String::new(),
            ask_scroll: 0,
            ask_viewport: 30,
            track_record_memories: Vec::new(),
            track_record_calls: Vec::new(),
            reports: Vec::new(),
            report_sel: 0,
            report_scroll: 0,
            report_source: String::new(),
            report_line_count: 0,
            delin_property: 0,
            delin_aging: DelinquencyAging::default(),
            delin_residents: Vec::new(),
            delin_resident_sel: 0,
            delin_sort: DelinSort::Owed,
            delin_watchlist: Vec::new(),
            delin_trend: Vec::new(),
            renew_property: 0,
            renew_leases: Vec::new(),
            renew_recs: Vec::new(),
            renew_sel: 0,
            renew_windows: [0; 4],
            renew_window_filter: None,
            renew_opportunity: (0.0, 0.0, 0),
            previous_screen: Screen::CloseDesk,
            sidebar_cursor: 0,
            sidebar_focused: false,
        }
    }

    /// Refresh all screens from the database. Errors land in the status line
    /// instead of tearing the terminal down mid-session.
    pub async fn reload(&mut self, pool: &SqlitePool) {
        let readiness = close_readiness::assess_portfolio(pool, &self.period).await;
        let mappings = db::list_unapproved_mappings(pool).await;
        match (readiness, mappings) {
            (Ok((summary, properties)), Ok(mappings)) => {
                self.summary = summary;
                self.properties = properties;
                self.mappings = mappings;
                self.selected = self.selected.min(self.properties.len().saturating_sub(1));
                let sidebar_len = self.sidebar_items().len();
                if sidebar_len > 0 {
                    self.sidebar_cursor = self.sidebar_cursor.min(sidebar_len - 1);
                }
                self.mapping_selected = self
                    .mapping_selected
                    .min(self.mappings.len().saturating_sub(1));
                self.last_refreshed = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                self.load_error = None;
            }
            (Err(err), _) | (_, Err(err)) => {
                self.load_error = Some(err.to_string());
            }
        }
        // Portfolio roll-up for the Close Desk summary band. Reuses the
        // already-computed owner-ready count so readiness is not re-derived.
        match db::portfolio_rollup(pool, &self.period, self.summary.ready_count as i64).await {
            Ok(rollup) => self.portfolio = rollup,
            Err(err) => {
                // A rollup failure must not blank the whole desk — degrade to a
                // zero band and surface the error in the status line.
                self.portfolio = PortfolioRollup::default();
                if self.load_error.is_none() {
                    self.load_error = Some(err.to_string());
                }
            }
        }
        // Load ledger account activity for the selected ledger property.
        self.reload_ledger_accounts(pool).await;
        // Load delinquency screen data.
        self.reload_delinquency(pool).await;
        // Load renewals screen data.
        self.reload_renewals(pool).await;
        // Load track record data.
        self.reload_track_record(pool).await;
        // Discover weekly reports from disk.
        self.reload_reports();
        // Build the budget→actual NOI bridge for the selected property.
        self.reload_noi_bridge(pool).await;
    }

    /// Returns the flat navigable item list for the sidebar tree.
    /// One entry per (property × SIDEBAR_SCREENS) combination.
    pub fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        for (property_idx, _) in self.properties.iter().enumerate() {
            for &screen in SIDEBAR_SCREENS {
                items.push(SidebarItem {
                    property_idx,
                    screen,
                });
            }
        }
        items
    }

    /// Returns the sidebar item the cursor currently points at, if any.
    pub fn active_sidebar_item(&self) -> Option<SidebarItem> {
        self.sidebar_items().get(self.sidebar_cursor).copied()
    }

    /// Sync `screen`, `selected`, `ledger_property`, `statements_property`,
    /// `bridge_property`, and `vendors_property` from the current
    /// `sidebar_cursor`.
    pub fn sync_from_sidebar(&mut self) {
        if let Some(item) = self.active_sidebar_item() {
            self.screen = item.screen;
            self.ledger_property = item.property_idx;
            self.statements_property = item.property_idx;
            self.bridge_property = item.property_idx;
            self.vendors_property = Some(item.property_idx);
            self.selected = item.property_idx;
        }
    }

    /// Move the sidebar cursor to the next item (clamped at the last item).
    pub fn sidebar_move_next(&mut self) {
        let count = self.sidebar_items().len();
        if count > 0 {
            self.sidebar_cursor = (self.sidebar_cursor + 1).min(count - 1);
            self.sync_from_sidebar();
        }
    }

    /// Move the sidebar cursor to the previous item (clamped at 0).
    pub fn sidebar_move_prev(&mut self) {
        self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
        self.sync_from_sidebar();
    }

    /// Navigate to `screen` for the property currently active in the sidebar.
    /// No-ops if no properties are loaded.
    pub fn sidebar_navigate_to(&mut self, screen: Screen) {
        let prop = self
            .active_sidebar_item()
            .map(|i| i.property_idx)
            .unwrap_or(0);
        let items = self.sidebar_items();
        if let Some(idx) = items
            .iter()
            .position(|i| i.property_idx == prop && i.screen == screen)
        {
            self.sidebar_cursor = idx;
            self.sync_from_sidebar();
        }
    }

    /// Reverse-sync: update `sidebar_cursor` to match the current `self.screen`
    /// and the active property. Called after external screen changes (Tab, hotkeys).
    pub fn sync_sidebar_from_screen(&mut self) {
        let prop = self
            .active_sidebar_item()
            .map(|i| i.property_idx)
            .unwrap_or(0);
        let target = self.screen;
        let items = self.sidebar_items();
        if let Some(idx) = items
            .iter()
            .position(|i| i.property_idx == prop && i.screen == target)
        {
            self.sidebar_cursor = idx;
        }
    }

    /// Reload the account-activity list for the current ledger property + period.
    pub async fn reload_ledger_accounts(&mut self, pool: &SqlitePool) {
        let property_id = self.ledger_property_id().map(str::to_string);
        let Some(property_id) = property_id else {
            self.ledger_accounts = Vec::new();
            return;
        };
        match db::account_activity(pool, &property_id, &self.period).await {
            Ok(accounts) => {
                self.ledger_accounts = accounts;
                self.ledger_account_selected = self
                    .ledger_account_selected
                    .min(self.ledger_accounts.len().saturating_sub(1));
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
            }
        }
    }

    /// Load the currently selected report into `report_source` and update
    /// `report_line_count` for scroll clamping. Resets scroll to 0.
    /// Actual rendering happens at draw time using the live pane width.
    fn load_selected_report(&mut self) {
        self.report_scroll = 0;
        if self.reports.is_empty() || self.report_sel >= self.reports.len() {
            self.report_source = String::new();
            self.report_line_count = 0;
            return;
        }
        self.report_source = self.reports[self.report_sel]
            .load()
            .unwrap_or_else(|e| format!("Failed to load report: {e}"));
        // width-stable line count — render_markdown does not wrap, so any
        // reasonable width yields the same number of lines.
        self.report_line_count =
            crate::tui::markdown::render_markdown(&self.report_source, 120).len();
    }

    /// Reload the weekly reports list filtered to the active period and load
    /// the selected report.
    ///
    /// The selection is reset to 0 because the filtered list changes wholesale
    /// when the period changes, so clamping the old index is meaningless.
    pub fn reload_reports(&mut self) {
        self.reports = crate::tui::reports::filter_by_period(
            crate::tui::reports::discover_weekly_reports(),
            &self.period,
        );
        self.report_sel = 0;
        self.load_selected_report();
    }

    /// The `property_id` for the currently selected delinquency property.
    pub fn delin_property_id(&self) -> Option<&str> {
        self.properties
            .get(self.delin_property)
            .map(|p| p.property_id.as_str())
    }

    /// Re-sort `delin_residents` in place per the active `delin_sort`, keeping
    /// the worst-first invariant for the default (Owed) order. Resets the cursor.
    pub fn sort_delin_residents(&mut self) {
        match self.delin_sort {
            DelinSort::Owed => {
                self.delin_residents.sort_by(|a, b| {
                    b.total_delinquent
                        .partial_cmp(&a.total_delinquent)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.resident_code.cmp(&b.resident_code))
                });
            }
            DelinSort::Name => {
                self.delin_residents.sort_by(|a, b| {
                    a.resident_name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.resident_name.as_deref().unwrap_or(""))
                        .then_with(|| a.resident_code.cmp(&b.resident_code))
                });
            }
            DelinSort::Status => {
                self.delin_residents.sort_by(|a, b| {
                    a.resident_status
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.resident_status.as_deref().unwrap_or(""))
                        .then_with(|| {
                            b.total_delinquent
                                .partial_cmp(&a.total_delinquent)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                });
            }
        }
        self.delin_resident_sel = self
            .delin_resident_sel
            .min(self.delin_residents.len().saturating_sub(1));
    }

    /// Reload the Delinquency screen for the selected property + period:
    /// per-resident receivables, aging buckets, the BDDRE risk watchlist, and a
    /// short delinquency trend.
    ///
    /// The watchlist reads the BDDRE lane CSVs. When the active property maps to
    /// a known lane (by display name), only that lane's rows are shown; otherwise
    /// (e.g. the synthetic demo portfolio, which has no lane CSVs) it falls back
    /// to the union of all lanes so the watchlist is never empty in a demo.
    pub async fn reload_delinquency(&mut self, pool: &SqlitePool) {
        let property_id = self.delin_property_id().map(str::to_string);
        let property_name = self
            .properties
            .get(self.delin_property)
            .map(|p| p.property.clone());

        let Some(property_id) = property_id else {
            self.delin_residents = Vec::new();
            self.delin_aging = DelinquencyAging::default();
            self.delin_trend = Vec::new();
            self.delin_watchlist = Vec::new();
            return;
        };

        match db::list_receivables_for_period(pool, &property_id, &self.period).await {
            Ok(residents) => self.delin_residents = residents,
            Err(err) => {
                self.load_error = Some(err.to_string());
                return;
            }
        }
        match db::delinquency_aging_for_period(pool, &property_id, &self.period).await {
            Ok(aging) => self.delin_aging = aging,
            Err(err) => {
                self.load_error = Some(err.to_string());
                return;
            }
        }
        match db::delinquency_trend(pool, &property_id, 6).await {
            Ok(trend) => self.delin_trend = trend,
            Err(err) => {
                self.load_error = Some(err.to_string());
                return;
            }
        }

        // BDDRE watchlist: prefer the lane matching the active property; fall
        // back to all lanes (demo portfolio has no per-property CSVs).
        let all = crate::tui::bddre::discover_intervention_queue();
        let scoped: Vec<RiskRow> = match property_name.as_deref() {
            Some(name) if all.iter().any(|r| r.lane == name) => {
                all.into_iter().filter(|r| r.lane == name).collect()
            }
            _ => all,
        };
        self.delin_watchlist = scoped;

        self.sort_delin_residents();
    }

    /// The `property_id` for the currently selected renewals property.
    pub fn renew_property_id(&self) -> Option<&str> {
        self.properties
            .get(self.renew_property)
            .map(|p| p.property_id.as_str())
    }

    /// Reload the Renewals screen for the selected property + period: per-unit
    /// lease rows (DB), the RPCOE renewal recommendations (lane CSV), the
    /// expiration-window funnel, and the renewal opportunity.
    ///
    /// Leases come from the DB (current vs market rent). Recommendations come
    /// from the RPCOE lane CSVs (recommended rent, %, confidence, drivers,
    /// expiry) — the DB has no expiry dates. When the active property maps to a
    /// known lane, only that lane's recs are shown; otherwise (the synthetic
    /// demo portfolio, which has no lane CSVs) it falls back to the union of all
    /// lanes so the screen is never empty in a demo. The window funnel and
    /// opportunity are computed from the unfiltered recs/leases; the active
    /// window filter then narrows the displayed recommendations.
    pub async fn reload_renewals(&mut self, pool: &SqlitePool) {
        let property_id = self.renew_property_id().map(str::to_string);
        let property_name = self
            .properties
            .get(self.renew_property)
            .map(|p| p.property.clone());

        let Some(property_id) = property_id else {
            self.renew_leases = Vec::new();
            self.renew_recs = Vec::new();
            self.renew_windows = [0; 4];
            self.renew_opportunity = (0.0, 0.0, 0);
            self.renew_sel = 0;
            return;
        };

        match db::list_leases_for_period(pool, &property_id, &self.period).await {
            Ok(leases) => self.renew_leases = leases,
            Err(err) => {
                self.load_error = Some(err.to_string());
                return;
            }
        }
        self.renew_opportunity = db::lease_opportunity(&self.renew_leases);

        // RPCOE recommendations: prefer the lane matching the active property;
        // fall back to all lanes (demo portfolio has no per-property CSVs).
        let all = crate::tui::rpcoe::discover_rpcoe_recommendations();
        let scoped: Vec<RentRec> = match property_name.as_deref() {
            Some(name) if all.iter().any(|r| r.lane == name) => {
                all.into_iter().filter(|r| r.lane == name).collect()
            }
            _ => all,
        };
        // Window counts come from the full (unfiltered) rec set so the funnel
        // always reflects the whole pipeline, not the current filter view.
        self.renew_windows = crate::tui::rpcoe::expiration_windows(&scoped);
        self.renew_recs = apply_window_filter(scoped, self.renew_window_filter);
        self.renew_sel = self.renew_sel.min(self.renew_recs.len().saturating_sub(1));
    }

    /// Reload track-record memories and the full calls list.
    pub async fn reload_track_record(&mut self, pool: &SqlitePool) {
        match db::recent_track_record_memories(pool, 100).await {
            Ok(memories) => self.track_record_memories = memories,
            Err(err) => self.load_error = Some(err.to_string()),
        }
        match db::list_calls(pool).await {
            Ok(calls) => self.track_record_calls = calls,
            Err(err) => self.load_error = Some(err.to_string()),
        }
    }

    /// Load transactions for the currently selected ledger account.
    pub async fn load_ledger_txns(&mut self, pool: &SqlitePool) {
        let property_id = self.ledger_property_id().map(str::to_string);
        let account_code = self
            .ledger_accounts
            .get(self.ledger_account_selected)
            .map(|a| a.account_code.clone());
        let (Some(property_id), Some(account_code)) = (property_id, account_code) else {
            self.ledger_txns = Vec::new();
            return;
        };
        match db::list_account_transactions(pool, &property_id, &account_code, &self.period).await {
            Ok(txns) => {
                self.ledger_txns = txns;
                self.ledger_txn_selected = 0;
                self.ledger_filter = None;
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
            }
        }
    }

    /// Apply a search filter — populates `ledger_txns` from `search_transactions`.
    pub async fn apply_ledger_filter(&mut self, pool: &SqlitePool, needle: String) {
        let property_id = self.ledger_property_id().map(str::to_string);
        match db::search_transactions(pool, property_id.as_deref(), &needle, None, false, 200).await
        {
            Ok(txns) => {
                self.ledger_txns = txns;
                self.ledger_txn_selected = 0;
                self.ledger_filter = Some(needle);
                self.ledger_focus = LedgerFocus::Transactions;
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
            }
        }
    }

    /// The `property_id` string for the currently selected ledger property.
    pub fn ledger_property_id(&self) -> Option<&str> {
        self.properties
            .get(self.ledger_property)
            .map(|p| p.property_id.as_str())
    }

    pub fn selected_property(&self) -> Option<&PropertyCloseReadiness> {
        self.properties.get(self.selected)
    }

    pub fn selected_mapping(&self) -> Option<&AccountMapping> {
        self.mappings.get(self.mapping_selected)
    }

    pub fn suggested_category_for_selected(&self) -> Option<String> {
        self.selected_mapping()
            .and_then(account_review::suggested_category_for)
            .filter(|category| !category.trim().is_empty())
    }

    /// Switch to a section by its index in SIDEBAR_SCREENS (no-op if out of range).
    /// Leaves the sidebar (content focus) and syncs the sidebar cursor.
    pub fn goto_section_index(&mut self, idx: usize) {
        if let Some(&screen) = SIDEBAR_SCREENS.get(idx) {
            self.screen = screen;
            self.sidebar_focused = false;
            self.sync_sidebar_from_screen();
        }
    }

    /// Cycle to the next/previous section in SIDEBAR_SCREENS (wrapping). Content focus.
    pub fn cycle_section(&mut self, forward: bool) {
        let order = SIDEBAR_SCREENS;
        let n = order.len();
        let cur = order.iter().position(|&s| s == self.screen).unwrap_or(0);
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.screen = order[next];
        self.sidebar_focused = false;
        self.sync_sidebar_from_screen();
    }

    /// 1-based number shown next to a section in the sidebar / used by hotkeys. None if not a section.
    pub fn section_number(screen: Screen) -> Option<usize> {
        SIDEBAR_SCREENS
            .iter()
            .position(|&s| s == screen)
            .map(|i| i + 1)
    }

    pub fn select_next(&mut self) {
        match self.screen {
            Screen::CloseDesk => {
                if !self.properties.is_empty() {
                    self.selected = (self.selected + 1).min(self.properties.len() - 1);
                }
            }
            Screen::Mappings => {
                if !self.mappings.is_empty() {
                    self.mapping_selected =
                        (self.mapping_selected + 1).min(self.mappings.len() - 1);
                }
            }
            Screen::Ledger => match self.ledger_focus {
                LedgerFocus::Accounts => {
                    if !self.ledger_accounts.is_empty() {
                        self.ledger_account_selected =
                            (self.ledger_account_selected + 1).min(self.ledger_accounts.len() - 1);
                    }
                }
                LedgerFocus::Transactions => {
                    if !self.ledger_txns.is_empty() {
                        self.ledger_txn_selected =
                            (self.ledger_txn_selected + 1).min(self.ledger_txns.len() - 1);
                    }
                }
            },
            Screen::Statements | Screen::NoiBridge | Screen::TrackRecord => {} // no cursor navigation
            Screen::Delinquency => {
                if !self.delin_residents.is_empty() {
                    self.delin_resident_sel =
                        (self.delin_resident_sel + 1).min(self.delin_residents.len() - 1);
                }
            }
            Screen::Renewals => {
                if !self.renew_recs.is_empty() {
                    self.renew_sel = (self.renew_sel + 1).min(self.renew_recs.len() - 1);
                }
            }
            Screen::Reports => {
                if !self.reports.is_empty() {
                    self.report_sel = (self.report_sel + 1).min(self.reports.len() - 1);
                    self.load_selected_report();
                }
            }
            Screen::Vendors => {
                if !self.vendors.is_empty() {
                    self.vendor_selected = (self.vendor_selected + 1).min(self.vendors.len() - 1);
                }
            }
            Screen::Ask => {
                if !self.ask_output.is_empty() {
                    let max_skip = self
                        .ask_output
                        .len()
                        .saturating_sub(self.ask_viewport.max(1));
                    self.ask_scroll = (self.ask_scroll + 1).min(max_skip);
                }
            }
        }
    }

    pub fn select_previous(&mut self) {
        match self.screen {
            Screen::CloseDesk => self.selected = self.selected.saturating_sub(1),
            Screen::Mappings => self.mapping_selected = self.mapping_selected.saturating_sub(1),
            Screen::Ledger => match self.ledger_focus {
                LedgerFocus::Accounts => {
                    self.ledger_account_selected = self.ledger_account_selected.saturating_sub(1);
                }
                LedgerFocus::Transactions => {
                    self.ledger_txn_selected = self.ledger_txn_selected.saturating_sub(1);
                }
            },
            Screen::Statements | Screen::NoiBridge | Screen::TrackRecord => {}
            Screen::Delinquency => {
                self.delin_resident_sel = self.delin_resident_sel.saturating_sub(1);
            }
            Screen::Renewals => {
                self.renew_sel = self.renew_sel.saturating_sub(1);
            }
            Screen::Reports => {
                if !self.reports.is_empty() {
                    self.report_sel = self.report_sel.saturating_sub(1);
                    self.load_selected_report();
                }
            }
            Screen::Vendors => {
                self.vendor_selected = self.vendor_selected.saturating_sub(1);
            }
            Screen::Ask => {
                self.ask_scroll = self.ask_scroll.saturating_sub(1);
            }
        }
    }

    pub fn select_first(&mut self) {
        match self.screen {
            Screen::CloseDesk => self.selected = 0,
            Screen::Mappings => self.mapping_selected = 0,
            Screen::Ledger => match self.ledger_focus {
                LedgerFocus::Accounts => self.ledger_account_selected = 0,
                LedgerFocus::Transactions => self.ledger_txn_selected = 0,
            },
            Screen::Statements | Screen::NoiBridge | Screen::TrackRecord => {}
            Screen::Delinquency => self.delin_resident_sel = 0,
            Screen::Renewals => self.renew_sel = 0,
            Screen::Reports => {
                if !self.reports.is_empty() {
                    self.report_sel = 0;
                    self.load_selected_report();
                }
            }
            Screen::Vendors => self.vendor_selected = 0,
            Screen::Ask => self.ask_scroll = 0,
        }
    }

    pub fn select_last(&mut self) {
        match self.screen {
            Screen::CloseDesk => self.selected = self.properties.len().saturating_sub(1),
            Screen::Mappings => self.mapping_selected = self.mappings.len().saturating_sub(1),
            Screen::Ledger => match self.ledger_focus {
                LedgerFocus::Accounts => {
                    self.ledger_account_selected = self.ledger_accounts.len().saturating_sub(1);
                }
                LedgerFocus::Transactions => {
                    self.ledger_txn_selected = self.ledger_txns.len().saturating_sub(1);
                }
            },
            Screen::Statements | Screen::NoiBridge | Screen::TrackRecord => {}
            Screen::Delinquency => {
                self.delin_resident_sel = self.delin_residents.len().saturating_sub(1);
            }
            Screen::Renewals => {
                self.renew_sel = self.renew_recs.len().saturating_sub(1);
            }
            Screen::Reports => {
                if !self.reports.is_empty() {
                    self.report_sel = self.reports.len() - 1;
                    self.load_selected_report();
                }
            }
            Screen::Vendors => {
                self.vendor_selected = self.vendors.len().saturating_sub(1);
            }
            Screen::Ask => {
                self.ask_scroll = self
                    .ask_output
                    .len()
                    .saturating_sub(self.ask_viewport.max(1));
            }
        }
    }

    /// Reload the T12 statement for the currently selected statements property.
    pub async fn reload_statements(&mut self, pool: &SqlitePool) {
        let property_name = self
            .properties
            .get(self.statements_property)
            .map(|p| p.property.clone());
        let Some(property_name) = property_name else {
            self.statements = None;
            return;
        };
        let end_period = self.statements_end_period.clone();
        match t12::assemble_t12(pool, &property_name, end_period.as_deref()).await {
            Ok(stmt) => {
                // Persist the resolved end_period so `[`/`]` can shift it.
                self.statements_end_period = Some(stmt.end_period.clone());
                self.statements = Some(stmt);
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
            }
        }
    }

    /// Reload the budget→actual NOI bridge for the selected property.
    ///
    /// Period is resolved **per property** to the latest period with actuals
    /// (never a global MAX), unless `bridge_period` is already pinned by the
    /// `[`/`]` keys. Empty/clean state when the property has no actuals.
    pub async fn reload_noi_bridge(&mut self, pool: &SqlitePool) {
        let property = self.properties.get(self.bridge_property).cloned();
        let Some(property) = property else {
            self.bridge_steps.clear();
            self.bridge_budget_noi = 0.0;
            self.bridge_actual_noi = 0.0;
            self.bridge_period = None;
            return;
        };

        // Resolve the period: an explicit pin wins; otherwise the property's
        // own latest period with actuals.
        let period = match self.bridge_period.clone() {
            Some(p) => Some(p),
            None => match db::latest_actual_period(pool, &property.property_id).await {
                Ok(p) => p,
                Err(err) => {
                    self.load_error = Some(err.to_string());
                    None
                }
            },
        };
        let Some(period) = period else {
            // No actuals anywhere for this property → clean empty state.
            self.bridge_steps.clear();
            self.bridge_budget_noi = 0.0;
            self.bridge_actual_noi = 0.0;
            self.bridge_period = None;
            return;
        };

        match db::category_variance_for_period(pool, &property.property_id, &period).await {
            Ok(variances) => {
                // Budget NOI directly from the (favorable-positive) rows; actual
                // NOI follows by adding the summed NOI-impact variance — which is
                // exactly actual_noi − budget_noi by construction.
                let budget_noi = budget_noi_from_variances(&variances);
                let delta: f64 = variances.iter().map(|v| v.variance).sum();
                let actual_noi = budget_noi + delta;
                const TOP_N: usize = 7;
                self.bridge_steps =
                    crate::tui::noi_bridge::build_bridge(budget_noi, &variances, TOP_N);
                self.bridge_budget_noi = budget_noi;
                self.bridge_actual_noi = actual_noi;
                self.bridge_period = Some(period);
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
                self.bridge_steps.clear();
                self.bridge_budget_noi = 0.0;
                self.bridge_actual_noi = 0.0;
                // Clear any stale period pin so the next property load
                // auto-resolves rather than masking data with a leftover period.
                self.bridge_period = None;
            }
        }
    }

    /// Reload the vendor spend list for the current scope + window + filter.
    pub async fn reload_vendors(&mut self, pool: &SqlitePool) {
        let property_id = self
            .vendors_property
            .and_then(|i| self.properties.get(i))
            .map(|p| p.property_id.clone());

        // Compute since_period: period arithmetic mirrors shift_period.
        let since_period = if let Some(months) = self.vendors_window_months {
            let period = &self.period;
            if let Ok((year, month)) = db::parse_period_label(period) {
                let total = year * 12 + (month - 1) - (months - 1);
                let (new_year, new_month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
                Some(format!("{new_year:04}-{new_month:02}"))
            } else {
                None
            }
        } else {
            None
        };

        match db::vendor_spend(
            pool,
            property_id.as_deref(),
            since_period.as_deref(),
            self.vendors_filter.as_deref(),
            200,
        )
        .await
        {
            Ok(vendors) => {
                self.vendors = vendors;
                self.vendor_selected = self
                    .vendor_selected
                    .min(self.vendors.len().saturating_sub(1));
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
                return;
            }
        }
        self.load_vendor_txns(pool).await;
    }

    /// Load recent transactions for the currently selected vendor.
    pub async fn load_vendor_txns(&mut self, pool: &SqlitePool) {
        let payee = self
            .vendors
            .get(self.vendor_selected)
            .map(|v| v.payee.clone());
        let Some(payee) = payee else {
            self.vendor_txns = Vec::new();
            return;
        };
        let property_id = self
            .vendors_property
            .and_then(|i| self.properties.get(i))
            .map(|p| p.property_id.clone());
        match db::payee_transactions(pool, &payee, property_id.as_deref(), 15).await {
            Ok(txns) => {
                self.vendor_txns = txns;
            }
            Err(err) => {
                self.load_error = Some(err.to_string());
            }
        }
    }

    /// Cycle the vendor property scope: All → each property → All.
    pub fn vendors_cycle_property(&mut self) {
        self.vendors_property = match self.vendors_property {
            None => {
                if self.properties.is_empty() {
                    None
                } else {
                    Some(0)
                }
            }
            Some(i) if i + 1 < self.properties.len() => Some(i + 1),
            Some(_) => None,
        };
    }

    /// Cycle the vendors time window: All ↔ T12 ↔ T6 ↔ T3 (forward only).
    pub fn vendors_cycle_window(&mut self) {
        self.vendors_window_months = match self.vendors_window_months {
            None => Some(12),
            Some(12) => Some(6),
            Some(6) => Some(3),
            Some(3) | Some(_) => None,
        };
    }

    /// Cycle the vendors time window in reverse: All ↔ T3 ↔ T6 ↔ T12.
    pub fn vendors_cycle_window_back(&mut self) {
        self.vendors_window_months = match self.vendors_window_months {
            None => Some(3),
            Some(3) => Some(6),
            Some(6) => Some(12),
            Some(12) | Some(_) => None,
        };
    }

    /// The human-readable label for the current vendors window.
    pub fn vendors_window_label(&self) -> &'static str {
        match self.vendors_window_months {
            None => "all time",
            Some(12) => "T12",
            Some(6) => "T6",
            Some(3) => "T3",
            Some(_) => "custom",
        }
    }

    /// Cycle to the next property on the Statements screen.
    pub fn statements_cycle_property(&mut self) {
        if self.properties.is_empty() {
            return;
        }
        self.statements_property = (self.statements_property + 1) % self.properties.len();
        // Reset end period so assemble_t12 picks the latest for the new property.
        self.statements_end_period = None;
        self.statements = None;
    }

    /// Shift the statements end period by `months` (positive = forward).
    pub fn statements_shift_end_period(&mut self, months: i64) {
        let end = match self
            .statements_end_period
            .as_deref()
            .or_else(|| self.statements.as_ref().map(|s| s.end_period.as_str()))
        {
            Some(p) => p.to_string(),
            None => {
                // Nothing loaded yet; use the app's current period.
                self.period.clone()
            }
        };
        let Ok((year, month)) = db::parse_period_label(&end) else {
            return;
        };
        let total = year * 12 + (month - 1) + months;
        let (new_year, new_month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        self.statements_end_period = Some(format!("{new_year:04}-{new_month:02}"));
    }

    /// Shift the NOI-bridge period by `months` (positive = forward). Pins
    /// `bridge_period` so the next reload uses the chosen period rather than
    /// re-resolving to the property's latest.
    pub fn bridge_shift_period(&mut self, months: i64) {
        let base = match self.bridge_period.as_deref() {
            Some(p) => p.to_string(),
            // Nothing resolved yet; fall back to the app's current period.
            None => self.period.clone(),
        };
        let Ok((year, month)) = db::parse_period_label(&base) else {
            return;
        };
        let total = year * 12 + (month - 1) + months;
        let (new_year, new_month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        self.bridge_period = Some(format!("{new_year:04}-{new_month:02}"));
    }

    /// Cycle the NOI-bridge property selector by `delta` (+1 / -1), clamped to
    /// the property list.
    pub fn bridge_cycle_property(&mut self, delta: isize) {
        if self.properties.is_empty() {
            return;
        }
        let len = self.properties.len() as isize;
        let cur = self.bridge_property as isize;
        let next = (cur + delta).clamp(0, len - 1);
        self.bridge_property = next as usize;
    }

    /// Cycle to the next property on the Ledger screen.
    pub fn ledger_cycle_property(&mut self) {
        if self.properties.is_empty() {
            return;
        }
        self.ledger_property = (self.ledger_property + 1) % self.properties.len();
        self.ledger_account_selected = 0;
        self.ledger_txns = Vec::new();
        self.ledger_txn_selected = 0;
        self.ledger_filter = None;
        self.ledger_focus = LedgerFocus::Accounts;
    }

    /// Cycle the delinquency property selector by `delta` (+1 / -1), clamped to
    /// the property list, resetting the resident cursor.
    pub fn delin_cycle_property(&mut self, delta: isize) {
        if self.properties.is_empty() {
            return;
        }
        let len = self.properties.len() as isize;
        let cur = self.delin_property as isize;
        let next = (cur + delta).clamp(0, len - 1);
        self.delin_property = next as usize;
        self.delin_resident_sel = 0;
    }

    /// Cycle the renewals property selector by `delta`, clamped to range.
    pub fn renew_cycle_property(&mut self, delta: isize) {
        if self.properties.is_empty() {
            return;
        }
        let len = self.properties.len() as isize;
        let cur = self.renew_property as isize;
        let next = (cur + delta).clamp(0, len - 1);
        self.renew_property = next as usize;
        self.renew_sel = 0;
    }

    /// Cycle the expiration-window filter: All → ≤30 → 31-60 → 61-90 → 90+ →
    /// All. Resets the cursor (the filtered list changes wholesale).
    pub fn renew_cycle_window_filter(&mut self) {
        self.renew_window_filter = match self.renew_window_filter {
            None => Some(0),
            Some(w) if w >= 3 => None,
            Some(w) => Some(w + 1),
        };
        self.renew_sel = 0;
    }

    /// Human label for the active expiration-window filter (for toasts/context).
    pub fn renew_window_label(&self) -> &'static str {
        match self.renew_window_filter {
            None => "all windows",
            Some(0) => "≤30 days",
            Some(1) => "31-60 days",
            Some(2) => "61-90 days",
            _ => "90+ days",
        }
    }

    /// Move the close period by whole months; invalid periods are left as-is.
    pub fn shift_period(&mut self, months: i64) {
        let Ok((year, month)) = db::parse_period_label(&self.period) else {
            return;
        };
        let total = year * 12 + (month - 1) + months;
        let (new_year, new_month) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        self.period = format!("{new_year:04}-{new_month:02}");
    }

    /// Open the category picker, pre-highlighting the suggested category.
    pub fn open_picker(&mut self) {
        if self.selected_mapping().is_none() {
            return;
        }
        let start = self
            .suggested_category_for_selected()
            .and_then(|suggested| {
                ontology::NOI_CATEGORIES
                    .iter()
                    .position(|category| category.eq_ignore_ascii_case(&suggested))
            })
            .unwrap_or(0);
        self.picker = Some(start);
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    pub fn picker_next(&mut self) {
        if let Some(index) = self.picker {
            self.picker = Some((index + 1) % ontology::NOI_CATEGORIES.len());
        }
    }

    pub fn picker_previous(&mut self) {
        if let Some(index) = self.picker {
            let len = ontology::NOI_CATEGORIES.len();
            self.picker = Some((index + len - 1) % len);
        }
    }

    /// Approve the selected mapping with the suggested category, if there is
    /// one; otherwise open the picker so the operator chooses explicitly.
    pub async fn approve_with_suggestion(&mut self, pool: &SqlitePool) {
        match self.suggested_category_for_selected() {
            Some(category) => self.approve_selected(pool, &category).await,
            None => {
                self.toast = Some("no suggestion — choose a category".to_string());
                self.open_picker();
            }
        }
    }

    /// Approve the selected mapping with the picker's highlighted category.
    pub async fn approve_with_picker_choice(&mut self, pool: &SqlitePool) {
        if let Some(index) = self.picker {
            let category = ontology::NOI_CATEGORIES[index].to_string();
            self.picker = None;
            self.approve_selected(pool, &category).await;
        }
    }

    async fn approve_selected(&mut self, pool: &SqlitePool, category: &str) {
        let Some(mapping) = self.selected_mapping().cloned() else {
            return;
        };
        let result = account_review::approve_mapping(
            pool,
            MappingApproval {
                source_system: &mapping.source_system,
                property_scope: &mapping.property_scope,
                account_code: &mapping.account_code,
                account_name: &mapping.account_name,
                reviewed_category: category,
                review_notes: "approved inline via Boxscore close desk",
            },
        )
        .await;
        match result {
            Ok(()) => {
                self.toast = Some(format!(
                    "approved {} ({}) → {category}",
                    mapping.account_code, mapping.account_name
                ));
                self.reload(pool).await;
            }
            Err(err) => {
                self.load_error = Some(format!("approve failed: {err}"));
            }
        }
    }
}

fn empty_summary() -> CloseReadinessSummary {
    CloseReadinessSummary {
        property_count: 0,
        ready_count: 0,
        not_ready_count: 0,
        blocker_count: 0,
        warning_count: 0,
        owner_ready_ratio: 0.0,
    }
}

/// Keep only recommendations whose `days_to_expiration` falls in the selected
/// expiration window (`Some(0)` = ≤30, `1` = 31-60, `2` = 61-90, `3` = 90+).
/// `None` (no filter) returns the recs unchanged. Recs with no day count are
/// dropped when a window is active (they belong to no window).
pub fn apply_window_filter(recs: Vec<RentRec>, filter: Option<usize>) -> Vec<RentRec> {
    let Some(w) = filter else {
        return recs;
    };
    recs.into_iter()
        .filter(|r| {
            r.days_to_expiration
                .map(|d| match w {
                    0 => d <= 30,
                    1 => (31..=60).contains(&d),
                    2 => (61..=90).contains(&d),
                    _ => d > 90,
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_periods_across_year_boundaries() {
        let mut app = DeskApp::new("2026-01".to_string());
        app.shift_period(-1);
        assert_eq!(app.period, "2025-12");
        app.shift_period(7);
        assert_eq!(app.period, "2026-07");
    }

    #[test]
    fn selection_is_clamped_to_property_count() {
        let mut app = DeskApp::new("2026-06".to_string());
        app.select_next();
        assert_eq!(app.selected, 0);
        app.select_last();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn picker_wraps_around_the_category_list() {
        let mut app = DeskApp::new("2026-06".to_string());
        app.picker = Some(0);
        app.picker_previous();
        assert_eq!(app.picker, Some(ontology::NOI_CATEGORIES.len() - 1));
        app.picker_next();
        assert_eq!(app.picker, Some(0));
    }
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;

    fn app_with_properties(count: usize) -> DeskApp {
        let mut app = DeskApp::new("2026-06".to_string());
        app.properties = (0..count)
            .map(|i| crate::close_readiness::PropertyCloseReadiness {
                property_id: format!("prop-{i}"),
                property: format!("Property {i}"),
                unit_count: 100,
                status: crate::close_readiness::CloseReadinessStatus::NotReady,
                owner_ready: false,
                blockers: 1,
                warning_count: 0,
                feeds: vec![],
                operator_questions: vec![],
                contract_status: crate::close_readiness::ContractStatus {
                    status: "NOT_RUN".to_string(),
                    error_count: 0,
                    warning_count: 0,
                    contract_set_version: "v1.1 (C1-C5)".to_string(),
                    error_messages: Vec::new(),
                },
            })
            .collect();
        app
    }

    #[test]
    fn sidebar_items_returns_screens_per_property() {
        let app = app_with_properties(2);
        let items = app.sidebar_items();
        let n = SIDEBAR_SCREENS.len();
        assert_eq!(items.len(), 2 * n); // 2 props × N screens
        assert_eq!(items[0].property_idx, 0);
        assert_eq!(items[0].screen, Screen::CloseDesk);
        assert_eq!(items[n].property_idx, 1);
        assert_eq!(items[n].screen, Screen::CloseDesk);
    }

    #[test]
    fn sidebar_move_next_clamps_at_end() {
        let mut app = app_with_properties(1);
        let last = SIDEBAR_SCREENS.len() - 1;
        app.sidebar_cursor = last; // last item for 1 property
        app.sidebar_move_next();
        assert_eq!(app.sidebar_cursor, last);
    }

    #[test]
    fn sidebar_move_prev_clamps_at_zero() {
        let mut app = app_with_properties(1);
        app.sidebar_cursor = 0;
        app.sidebar_move_prev();
        assert_eq!(app.sidebar_cursor, 0);
    }

    #[test]
    fn sync_from_sidebar_sets_screen_and_property_selectors() {
        let mut app = app_with_properties(2);
        let n = SIDEBAR_SCREENS.len();
        app.sidebar_cursor = n + 1; // prop 1, Ledger (prop1*N + 1)
        app.sync_from_sidebar();
        assert_eq!(app.screen, Screen::Ledger);
        assert_eq!(app.ledger_property, 1);
        assert_eq!(app.statements_property, 1);
        assert_eq!(app.vendors_property, Some(1));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn sidebar_navigate_to_finds_correct_index_for_current_property() {
        let mut app = app_with_properties(2);
        let n = SIDEBAR_SCREENS.len();
        app.sidebar_cursor = n + 1; // prop 1, Ledger (prop1*N + 1)
        app.sync_from_sidebar();
        app.sidebar_navigate_to(Screen::Statements);
        assert_eq!(app.sidebar_cursor, n + 2); // prop 1, Statements (prop1*N + 2)
        assert_eq!(app.screen, Screen::Statements);
    }

    #[test]
    fn sidebar_items_empty_when_no_properties() {
        let app = DeskApp::new("2026-06".to_string());
        assert!(app.sidebar_items().is_empty());
    }

    #[test]
    fn sidebar_navigate_to_noop_when_no_properties() {
        let mut app = DeskApp::new("2026-06".to_string());
        app.sidebar_navigate_to(Screen::Ledger);
        assert_eq!(app.sidebar_cursor, 0); // unchanged
    }
}

#[cfg(test)]
mod nav_hotkey_tests {
    use super::*;

    fn app_with_2_properties() -> DeskApp {
        let mut app = DeskApp::new("2026-06".to_string());
        app.properties = (0..2)
            .map(|i| crate::close_readiness::PropertyCloseReadiness {
                property_id: format!("prop-{i}"),
                property: format!("Property {i}"),
                unit_count: 100,
                status: crate::close_readiness::CloseReadinessStatus::NotReady,
                owner_ready: false,
                blockers: 0,
                warning_count: 0,
                feeds: vec![],
                operator_questions: vec![],
                contract_status: crate::close_readiness::ContractStatus {
                    status: "NOT_RUN".to_string(),
                    error_count: 0,
                    warning_count: 0,
                    contract_set_version: "v1.1 (C1-C5)".to_string(),
                    error_messages: Vec::new(),
                },
            })
            .collect();
        app
    }

    #[test]
    fn goto_section_index_navigates_to_reports() {
        let mut app = app_with_2_properties();
        // Reports is section 9 (index 8); NOI Bridge is now the 10th/last.
        app.goto_section_index(8);
        assert_eq!(app.screen, Screen::Reports);
        assert!(!app.sidebar_focused);
    }

    #[test]
    fn goto_section_index_navigates_to_noi_bridge_as_tenth() {
        let mut app = app_with_2_properties();
        let bridge_idx = SIDEBAR_SCREENS.len() - 1; // NOI Bridge is last (10th)
        assert_eq!(bridge_idx, 9, "NOI Bridge must be the 10th sidebar entry");
        app.goto_section_index(bridge_idx);
        assert_eq!(app.screen, Screen::NoiBridge);
        assert!(!app.sidebar_focused);
    }

    #[test]
    fn goto_section_index_navigates_to_delinquency() {
        let mut app = app_with_2_properties();
        app.goto_section_index(5); // 6th section
        assert_eq!(app.screen, Screen::Delinquency);
        assert!(!app.sidebar_focused);
    }

    #[test]
    fn goto_section_index_out_of_range_is_noop() {
        let mut app = app_with_2_properties();
        app.screen = Screen::CloseDesk;
        app.goto_section_index(SIDEBAR_SCREENS.len()); // out of range
        assert_eq!(app.screen, Screen::CloseDesk);
    }

    #[test]
    fn cycle_section_forward_from_last_wraps_to_close_desk() {
        // NOI Bridge is the last sidebar entry; Tab from it wraps to CloseDesk.
        let mut app = app_with_2_properties();
        app.screen = Screen::NoiBridge;
        app.sync_sidebar_from_screen();
        app.cycle_section(true);
        assert_eq!(app.screen, Screen::CloseDesk);
        assert!(!app.sidebar_focused);
    }

    #[test]
    fn cycle_section_backward_from_close_desk_wraps_to_noi_bridge() {
        let mut app = app_with_2_properties();
        app.screen = Screen::CloseDesk;
        app.sync_sidebar_from_screen();
        app.cycle_section(false);
        assert_eq!(app.screen, Screen::NoiBridge);
        assert!(!app.sidebar_focused);
    }

    #[test]
    fn tab_cycling_reaches_noi_bridge_the_tenth_screen() {
        // The 10th screen has no single-digit hotkey yet (F6.2 maps `0`), so it
        // MUST be reachable by Tab cycling. Walk forward until we hit it.
        let mut app = app_with_2_properties();
        app.screen = Screen::CloseDesk;
        app.sync_sidebar_from_screen();
        let mut reached = false;
        for _ in 0..SIDEBAR_SCREENS.len() {
            app.cycle_section(true);
            if app.screen == Screen::NoiBridge {
                reached = true;
                break;
            }
        }
        assert!(reached, "Tab cycling never reached the NOI Bridge screen");
    }

    #[test]
    fn section_number_close_desk_is_1() {
        assert_eq!(DeskApp::section_number(Screen::CloseDesk), Some(1));
    }

    #[test]
    fn section_number_noi_bridge_is_last() {
        // NOI Bridge is the 10th and final sidebar section.
        assert_eq!(
            DeskApp::section_number(Screen::NoiBridge),
            Some(SIDEBAR_SCREENS.len())
        );
        assert_eq!(DeskApp::section_number(Screen::NoiBridge), Some(10));
        assert_eq!(DeskApp::section_number(Screen::Reports), Some(9));
    }

    #[test]
    fn section_number_ask_is_none() {
        assert_eq!(DeskApp::section_number(Screen::Ask), None);
    }

    #[test]
    fn section_numbers_match_sidebar_screens_order() {
        // Verify 1-indexed mapping matches SIDEBAR_SCREENS order
        assert_eq!(DeskApp::section_number(Screen::CloseDesk), Some(1));
        assert_eq!(DeskApp::section_number(Screen::Ledger), Some(2));
        assert_eq!(DeskApp::section_number(Screen::Statements), Some(3));
        assert_eq!(DeskApp::section_number(Screen::Vendors), Some(4));
        assert_eq!(DeskApp::section_number(Screen::Mappings), Some(5));
        assert_eq!(DeskApp::section_number(Screen::Delinquency), Some(6));
        assert_eq!(DeskApp::section_number(Screen::Renewals), Some(7));
        assert_eq!(DeskApp::section_number(Screen::TrackRecord), Some(8));
        assert_eq!(DeskApp::section_number(Screen::Reports), Some(9));
        assert_eq!(DeskApp::section_number(Screen::NoiBridge), Some(10));
    }
}
