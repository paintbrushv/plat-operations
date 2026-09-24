//! Boxscore workbench rendering. Pure over [`DeskApp`] so it can be tested
//! with ratatui's `TestBackend`.

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
    Frame,
};

use super::app::{DeskApp, LedgerFocus, Screen};
use super::bddre::RiskRow;
use super::rpcoe::RentRec;
use crate::{
    account_review,
    close_readiness::{FeedStatus, PropertyCloseReadiness},
    db::DelinquencyAging,
    ontology,
    t12::{compact_dollars, T12RowType},
};

const ACCENT: Color = Color::Cyan;
const SEL_BG: Color = Color::Rgb(20, 50, 80);
const POSITIVE: Color = Color::Rgb(80, 200, 120);
const NEGATIVE: Color = Color::Rgb(220, 80, 80);
const WARNING: Color = Color::Yellow;

fn rounded_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title.into())
}

fn rounded_block_focused(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(title.into())
}

fn screen_label(screen: Screen) -> &'static str {
    match screen {
        Screen::CloseDesk => "Close Desk",
        Screen::Ledger => "Ledger",
        Screen::Statements => "Statements",
        Screen::NoiBridge => "NOI Bridge",
        Screen::Vendors => "Vendors",
        Screen::Mappings => "Mappings",
        Screen::Delinquency => "Delinquency",
        Screen::Renewals => "Renewals",
        Screen::TrackRecord => "Track Record",
        Screen::Reports => "Reports",
        Screen::Ask => "Ask",
    }
}

pub fn draw(frame: &mut Frame, app: &DeskApp) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    draw_footer(frame, app, footer);

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(24), Constraint::Min(0)]).areas(body);

    draw_sidebar(frame, app, sidebar_area);

    let [kpi_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(main_area);

    draw_kpi_strip(frame, app, kpi_area);

    match app.screen {
        Screen::CloseDesk => draw_close_desk(frame, app, content_area),
        Screen::Mappings => draw_mappings(frame, app, content_area),
        Screen::Ledger => draw_ledger(frame, app, content_area),
        Screen::Statements => draw_statements(frame, app, content_area),
        Screen::NoiBridge => draw_noi_bridge(frame, app, content_area),
        Screen::Vendors => draw_vendors(frame, app, content_area),
        Screen::Delinquency => draw_delinquency(frame, app, content_area),
        Screen::Renewals => draw_renewals(frame, app, content_area),
        Screen::TrackRecord => draw_track_record(frame, app, content_area),
        Screen::Reports => draw_reports(frame, app, content_area),
        Screen::Ask => draw_ask(frame, app, content_area),
    }

    if app.picker.is_some() {
        draw_category_picker(frame, app, frame.area());
    }

    // The help overlay renders last so it sits centered above any screen.
    if app.help_open {
        draw_help(frame, frame.area());
    }
}

/// Centered modal listing the full keymap and a one-line description of every
/// screen. Modeled on [`draw_category_picker`]: `Clear` + a centered rect with
/// a focused rounded block. Dismissed by `?`, `Esc`, or `q` (handled in mod.rs).
fn draw_help(frame: &mut Frame, area: Rect) {
    // (key, description) rows for the global keymap.
    let keymap: &[(&str, &str)] = &[
        (
            "1-9 / 0",
            "jump to a sidebar section (0 = 10th, NOI Bridge)",
        ),
        ("Tab / Shift+Tab", "cycle to the next / previous section"),
        (
            "h / ←",
            "focus the sidebar (then j/k to move, enter/l to select)",
        ),
        ("j / k", "select next / previous row"),
        ("g / G", "jump to first / last row"),
        ("PgUp / PgDn", "scroll the report body (Reports screen)"),
        ("[ / ]", "shift the period back / forward one month"),
        (
            "< / >",
            "previous / next property (Delinquency, Renewals, NOI Bridge)",
        ),
        ("s", "cycle the sort order (Delinquency)"),
        ("f", "cycle the expiration-window filter (Renewals)"),
        ("a / e", "approve suggested / edit category (Mappings)"),
        ("/", "filter (Ledger, Vendors)"),
        ("r", "refresh / reload the active data"),
        (":", "ask the model a question"),
        ("?", "toggle this help overlay"),
        ("q / Esc", "quit (or step back from a sub-pane / overlay)"),
    ];

    // One-line description of each of the 10 screens, in sidebar order.
    let screens: &[(&str, &str)] = &[
        (
            "1 Close Desk",
            "portfolio close-readiness board + summary band",
        ),
        ("2 Ledger", "GL accounts and transaction drill-down"),
        ("3 Statements", "trailing-12 income statement"),
        ("4 Vendors", "vendor spend, concentration, and transactions"),
        (
            "5 Mappings",
            "review + approve GL account → NOI category mappings",
        ),
        (
            "6 Delinquency",
            "aged receivables, aging buckets, BDDRE risk watchlist",
        ),
        (
            "7 Renewals",
            "lease-expiration funnel + per-unit renewal recommendations",
        ),
        (
            "8 Track Record",
            "batting averages (memories) + recent scored calls",
        ),
        ("9 Reports", "RPCOE / BDDRE weekly markdown report browser"),
        (
            "0 NOI Bridge",
            "budget → actual NOI waterfall (favorable green, unfavorable red)",
        ),
    ];

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  Keymap",
        Style::new().fg(ACCENT).bold(),
    )));
    for (k, desc) in keymap {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<16}"), Style::new().fg(ACCENT)),
            Span::styled((*desc).to_string(), Style::new().dim()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Screens",
        Style::new().fg(ACCENT).bold(),
    )));
    for (label, desc) in screens {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<16}"), Style::new().bold()),
            Span::styled((*desc).to_string(), Style::new().dim()),
        ]));
    }

    let height = (lines.len() + 2) as u16;
    let [popup] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(82)])
        .flex(Flex::Center)
        .areas(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(rounded_block_focused(" Help · ? or esc to close ")),
        popup,
    );
}

fn draw_header(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let active_prop_name = app
        .active_sidebar_item()
        .and_then(|item| app.properties.get(item.property_idx))
        .map(|p| p.property.as_str())
        .unwrap_or("—");

    let mut spans = vec![
        Span::raw("  "),
        Span::styled("‹ ", Style::new().fg(ACCENT)),
        Span::styled(app.period.clone(), Style::new().bold()),
        Span::styled(" ›", Style::new().fg(ACCENT)),
        Span::raw("   "),
        Span::styled(active_prop_name.to_string(), Style::new().bold()),
        Span::styled(" › ", Style::new().dim()),
        Span::styled(screen_label(app.screen), Style::new().fg(ACCENT)),
        Span::styled(
            format!(
                "   Owner-ready {}/{} · {} blockers",
                app.summary.ready_count, app.summary.property_count, app.summary.blocker_count,
            ),
            Style::new().dim(),
        ),
    ];

    if let Some(refreshed) = &app.last_refreshed {
        spans.push(Span::styled(
            format!("   refreshed {refreshed}"),
            Style::new().dim(),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_kpi_strip(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let mut spans = Vec::new();
    if let Some(context) = screen_context(app) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(context, Style::new().dim()));
    }

    let Some(item) = app.active_sidebar_item() else {
        if !spans.is_empty() {
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        return;
    };
    let Some(prop) = app.properties.get(item.property_idx) else {
        if !spans.is_empty() {
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        return;
    };

    let as_of = prop
        .feeds
        .iter()
        .find(|f| f.name == "Actual GL")
        .and_then(|f| f.latest_period_or_date.as_deref());

    let close_status = if prop.owner_ready {
        Span::styled("✓ owner-ready", Style::new().fg(POSITIVE))
    } else {
        Span::styled(
            format!("not ready · {} blockers", prop.blockers),
            Style::new().fg(NEGATIVE),
        )
    };

    if spans.is_empty() {
        spans.push(Span::raw("  "));
    } else {
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled(prop.property.clone(), Style::new().bold()));
    spans.push(Span::raw(format!(" — {}u · Close: ", prop.unit_count)));
    spans.push(close_status);

    if prop.warning_count > 0 {
        spans.push(Span::styled(
            format!(" · {} warnings", prop.warning_count),
            Style::new().fg(WARNING),
        ));
    }

    if let Some(date) = as_of {
        spans.push(Span::styled(
            format!("   (as of GL: {date})"),
            Style::new().dim(),
        ));
    } else {
        spans.push(Span::styled("   (ops data missing)", Style::new().dim()));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn screen_context(app: &DeskApp) -> Option<String> {
    match app.screen {
        Screen::CloseDesk => None,
        Screen::Mappings => Some(format!("{} mappings awaiting review", app.mappings.len())),
        Screen::Ledger => {
            let property_name = app
                .properties
                .get(app.ledger_property)
                .map(|p| p.property.as_str())
                .unwrap_or("—");
            Some(format!(
                "{property_name} · {} · {} active accounts",
                app.period,
                app.ledger_accounts.len()
            ))
        }
        Screen::Statements => {
            let property_name = app
                .properties
                .get(app.statements_property)
                .map(|p| p.property.as_str())
                .unwrap_or("—");
            let end_period = app
                .statements
                .as_ref()
                .map(|s| s.end_period.as_str())
                .or(app.statements_end_period.as_deref())
                .unwrap_or("—");
            Some(format!("{property_name} · T12 ending {end_period}"))
        }
        Screen::NoiBridge => {
            let property_name = app
                .properties
                .get(app.bridge_property)
                .map(|p| p.property.as_str())
                .unwrap_or("—");
            let period = app.bridge_period.as_deref().unwrap_or("—");
            let delta = app.bridge_actual_noi - app.bridge_budget_noi;
            Some(format!(
                "{property_name} · {period} · Budget ${:.0}k → Actual ${:.0}k (Δ ${:.0}k)",
                app.bridge_budget_noi / 1_000.0,
                app.bridge_actual_noi / 1_000.0,
                delta / 1_000.0,
            ))
        }
        Screen::Vendors => {
            let scope = app
                .vendors_property
                .and_then(|i| app.properties.get(i))
                .map(|p| p.property.as_str())
                .unwrap_or("all properties");
            Some(format!(
                "{} · {} · {} vendors",
                scope,
                app.vendors_window_label(),
                app.vendors.len()
            ))
        }
        Screen::Delinquency => {
            let prop = app.properties.get(app.delin_property);
            let property_name = prop.map(|p| p.property.as_str()).unwrap_or("—");
            let owed_k = app.delin_aging.delinquent_total() / 1000.0;
            let cnt = app.delin_aging.delinquent_cnt();
            let pct_str = prop
                .filter(|p| p.unit_count > 0)
                .map(|p| format!(" ({:.1}%)", cnt as f64 / p.unit_count as f64 * 100.0))
                .unwrap_or_default();
            Some(format!(
                "{property_name} · {cnt} delinquent{pct_str} · ${owed_k:.0}k owed · {} on watchlist",
                app.delin_watchlist.len()
            ))
        }
        Screen::Renewals => {
            let property_name = app
                .properties
                .get(app.renew_property)
                .map(|p| p.property.as_str())
                .unwrap_or("—");
            let (monthly, _annual, underpriced) = app.renew_opportunity;
            Some(format!(
                "{property_name} · {} expiring ≤30d · ${monthly:.0}/mo uplift · {underpriced} underpriced",
                app.renew_windows[0]
            ))
        }
        Screen::TrackRecord => Some(format!(
            "{} batting-avg entries · {} calls",
            app.track_record_memories.len(),
            app.track_record_calls.len()
        )),
        Screen::Reports => Some(format!("{} reports", app.reports.len())),
        Screen::Ask => {
            let question = if app.ask_question.is_empty() {
                "—"
            } else {
                app.ask_question.as_str()
            };
            Some(format!("ask: {question}"))
        }
    }
}

fn draw_sidebar(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("▍", Style::new().fg(ACCENT)),
        Span::styled("BOXSCORE", Style::new().fg(ACCENT).bold()),
    ]));
    lines.push(Line::from(Span::styled(
        format!("Period {}", app.period),
        Style::new().dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("1 Close Desk", Style::new().dim())));
    lines.push(Line::from(Span::styled("2 Ledger", Style::new().dim())));
    lines.push(Line::from(Span::styled("3 Statements", Style::new().dim())));
    lines.push(Line::from(Span::styled("4 Vendors", Style::new().dim())));
    lines.push(Line::from(Span::styled("5 Mappings", Style::new().dim())));
    lines.push(Line::from(Span::styled(
        "6 Delinquency",
        Style::new().dim(),
    )));
    lines.push(Line::from(Span::styled("7 Renewals", Style::new().dim())));
    lines.push(Line::from(Span::styled(
        "8 Track Record",
        Style::new().dim(),
    )));
    lines.push(Line::from(Span::styled("9 Reports", Style::new().dim())));
    // 10th section: hotkey `0` (single digits only cover 1-9).
    lines.push(Line::from(Span::styled("0 NOI Bridge", Style::new().dim())));
    lines.push(Line::from(""));

    let items = app.sidebar_items();

    for (prop_idx, prop) in app.properties.iter().enumerate() {
        let (icon, icon_style) = if prop.owner_ready {
            ("✓", Style::new().fg(POSITIVE))
        } else if prop.blockers == 0 && prop.warning_count > 0 {
            ("⚠", Style::new().fg(WARNING))
        } else {
            ("✗", Style::new().fg(NEGATIVE))
        };

        let prop_name: String = prop.property.chars().take(13).collect();
        lines.push(Line::from(vec![
            Span::raw("▼ "),
            Span::styled(prop_name, Style::new().bold()),
            Span::raw("  "),
            Span::styled(icon.to_string(), icon_style),
        ]));

        let summary = if prop.blockers > 0 {
            format!("  {}u · {} blkr", prop.unit_count, prop.blockers)
        } else if prop.warning_count > 0 {
            format!("  {}u · {} warn", prop.unit_count, prop.warning_count)
        } else {
            format!("  {}u · ready", prop.unit_count)
        };
        let summary: String = summary.chars().take(inner_w).collect();
        lines.push(Line::from(Span::styled(summary, Style::new().dim())));

        for &screen in crate::tui::app::SIDEBAR_SCREENS {
            let item_idx = items
                .iter()
                .position(|i| i.property_idx == prop_idx && i.screen == screen);
            let is_cursor = item_idx == Some(app.sidebar_cursor);
            let is_active = app.screen == screen
                && app.active_sidebar_item().map(|i| i.property_idx) == Some(prop_idx);

            let num = DeskApp::section_number(screen).unwrap_or(0);
            // Single-digit hotkeys only cover 1-9; the 10th section's hotkey is `0`.
            let hotkey = if num == 10 {
                "0".to_string()
            } else {
                num.to_string()
            };
            let bullet = if is_active { "●" } else { "○" };
            let label = format!("  {} {} {}", bullet, hotkey, screen_label(screen));
            let label: String = label.chars().take(inner_w).collect();

            let style = if is_cursor && app.sidebar_focused {
                Style::new().fg(Color::Black).bg(ACCENT)
            } else if is_active {
                Style::new().fg(ACCENT)
            } else {
                Style::new().dim()
            };

            lines.push(Line::from(Span::styled(label, style)));
        }
        lines.push(Line::from(""));
    }

    let content_height = inner.height.saturating_sub(2);
    let content_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: content_height,
    };
    let content_lines: Vec<Line> = lines.into_iter().take(content_height as usize).collect();
    frame.render_widget(Paragraph::new(content_lines), content_area);

    let sep_y = inner.y + content_height;
    if sep_y < inner.y + inner.height {
        let sep: String = "─".repeat(inner.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(sep, Style::new().dim()))),
            Rect {
                x: inner.x,
                y: sep_y,
                width: inner.width,
                height: 1,
            },
        );
    }

    let ask_y = inner.y + content_height + 1;
    if ask_y < inner.y + inner.height {
        let ask_text = if let Some(ref text) = app.ask_input {
            format!(":{text}▌")
        } else {
            ": ask a question…".to_string()
        };
        let ask_display: String = ask_text.chars().take(inner.width as usize).collect();
        let ask_style = if app.ask_input.is_some() {
            Style::new().fg(ACCENT)
        } else {
            Style::new().dim()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(ask_display, ask_style))),
            Rect {
                x: inner.x,
                y: ask_y,
                width: inner.width,
                height: 1,
            },
        );
    }
}

fn draw_close_desk(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.properties.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Portfolio ",
            "No properties found in this database.",
            "boxscore ingest-standardized gl --all && boxscore ingest-standardized ops --all",
        );
        return;
    }
    // Carve a compact portfolio summary band off the top, above the existing
    // property board + detail. At cramped heights the band yields to the board
    // (a GP would rather see the asset list than a clipped header).
    const BAND_HEIGHT: u16 = 4;
    const MIN_BOARD_HEIGHT: u16 = 8;
    let board_area = if area.height >= BAND_HEIGHT + MIN_BOARD_HEIGHT {
        let [band, board] =
            Layout::vertical([Constraint::Length(BAND_HEIGHT), Constraint::Min(0)]).areas(area);
        draw_portfolio_band(frame, app, band);
        board
    } else {
        area
    };

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .areas(board_area);
    draw_property_board(frame, app, left);
    draw_property_detail(frame, app, right);
}

/// Days from today to the close of `period` (its month-end). Positive ⇒ still
/// open, 0 ⇒ closes today, negative ⇒ the close window has passed. Returns
/// `None` if the period label is unparseable.
fn days_to_close(period: &str) -> Option<i64> {
    let (year, month) = crate::db::parse_period_label(period).ok()?;
    // First day of the *next* month, then step back one day → this month-end.
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = chrono::NaiveDate::from_ymd_opt(ny as i32, nm as u32, 1)?;
    let month_end = first_next.pred_opt()?;
    let today = chrono::Local::now().date_naive();
    Some((month_end - today).num_days())
}

/// One-glance portfolio book health: a dense, labeled, colored metric strip
/// rendered above the Close Desk property board. Every cell ties to a
/// `PortfolioRollup` figure; readiness shows as a `●`-per-property heatmap,
/// NOI variance is colored favorable/unfavorable, delinquency is always red.
fn draw_portfolio_band(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let p = &app.portfolio;
    let sep = || Span::styled("  ·  ", Style::new().dim());

    // ── Owner-ready heatmap: one dot per property, green=ready / red=blocked.
    let ready_ratio = p.ready_ratio();
    let ratio_color = if ready_ratio >= 0.8 {
        POSITIVE
    } else if ready_ratio >= 0.5 {
        WARNING
    } else {
        NEGATIVE
    };
    let mut heatmap: Vec<Span> = Vec::new();
    for prop in &app.properties {
        let dot_color = if prop.owner_ready { POSITIVE } else { NEGATIVE };
        heatmap.push(Span::styled("●", Style::new().fg(dot_color)));
    }
    if heatmap.is_empty() {
        heatmap.push(Span::styled("—", Style::new().dim()));
    }

    // ── Occupancy range (min–max across properties).
    let occ_span = if p.occ_properties == 0 {
        Span::styled("—", Style::new().dim())
    } else if (p.occ_high - p.occ_low).abs() < 1e-9 {
        Span::styled(format!("{:.0}%", p.occ_high * 100.0), Style::new().bold())
    } else {
        Span::styled(
            format!("{:.0}–{:.0}%", p.occ_low * 100.0, p.occ_high * 100.0),
            Style::new().bold(),
        )
    };

    // ── NOI variance vs budget (favorable green / unfavorable red).
    let noi_var = p.noi_variance();
    let noi_pct = if p.noi_budget.abs() > f64::EPSILON {
        noi_var / p.noi_budget.abs() * 100.0
    } else {
        0.0
    };
    let noi_color = if noi_var >= 0.0 { POSITIVE } else { NEGATIVE };
    let noi_text = format!("{} ({:+.0}%)", compact_dollars(noi_var), noi_pct);

    // ── Days-to-close (WARNING when the window is tight, NEGATIVE if blown).
    let (dtc_text, dtc_color) = match days_to_close(&app.period) {
        Some(d) if d < 0 => (format!("{}d past", -d), NEGATIVE),
        Some(d) if d <= 5 => (format!("{d}d"), WARNING),
        Some(d) => (format!("{d}d"), ACCENT),
        None => ("—".to_string(), WARNING),
    };

    let label = |t: &str| Span::styled(t.to_string(), Style::new().dim());

    // Line 1 — book size + readiness + occupancy (the "how big / how ready" row).
    let mut line1: Vec<Span> = vec![
        label("Props "),
        Span::styled(p.property_count.to_string(), Style::new().bold()),
        sep(),
        label("Units "),
        Span::styled(format!("{}", p.total_units), Style::new().bold()),
        sep(),
        label("Owner-ready "),
        Span::styled(
            format!("{}/{}", p.owner_ready, p.property_count),
            Style::new().fg(ratio_color).bold(),
        ),
        Span::raw(" "),
    ];
    line1.extend(heatmap);
    line1.extend([
        sep(),
        label("Occ "),
        occ_span,
        sep(),
        label("Close "),
        Span::styled(dtc_text, Style::new().fg(dtc_color).bold()),
    ]);

    // Line 2 — the money row (in-place rent, NOI variance, delinquency exposure).
    let line2: Vec<Span> = vec![
        label("In-place "),
        Span::styled(
            format!("{}/mo", compact_dollars(p.inplace_rent_total)),
            Style::new().bold(),
        ),
        sep(),
        label("NOI Δ vs budget "),
        Span::styled(noi_text, Style::new().fg(noi_color).bold()),
        sep(),
        label("Delinq exposure "),
        Span::styled(
            compact_dollars(p.delinquent_total),
            Style::new().fg(NEGATIVE).bold(),
        ),
    ];

    let title = format!(" Portfolio — Book Health · {} ", app.period);
    frame.render_widget(
        Paragraph::new(vec![Line::from(line1), Line::from(line2)]).block(rounded_block(&title)),
        area,
    );
}

fn draw_property_board(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let header = Row::new(["Property", "Status", "Blk", "Warn"]).style(Style::new().dim());
    let rows = app.properties.iter().map(|property| {
        Row::new(vec![
            Cell::from(property.property.clone()),
            Cell::from(readiness_label(property)).style(Style::new().fg(readiness_color(property))),
            Cell::from(property.blockers.to_string()),
            Cell::from(property.warning_count.to_string()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(20),
            Constraint::Length(11),
            Constraint::Length(4),
            Constraint::Length(5),
        ],
    )
    .header(header)
    .block(rounded_block(" Portfolio "))
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_property_detail(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let Some(property) = app.selected_property() else {
        return;
    };
    let [feeds_area, questions_area] =
        Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(area);

    let feeds_title = format!(" Close Desk — {} ", property.property);
    let block = rounded_block(&feeds_title);
    let inner = block.inner(feeds_area);
    frame.render_widget(block, feeds_area);

    // Column widths: icon(2) + name(16) + period(10) + note(rest)
    let note_w = inner.width.saturating_sub(2 + 1 + 16 + 1 + 10 + 1) as usize;
    let header_line = Line::from(vec![
        Span::styled("  ", Style::new()),
        Span::styled(format!("{:<16}", "Feed"), Style::new().dim()),
        Span::raw(" "),
        Span::styled(format!("{:<10}", "Latest"), Style::new().dim()),
        Span::raw(" "),
        Span::styled(
            format!("{:<width$}", "Note", width = note_w.max(4)),
            Style::new().dim(),
        ),
    ]);

    let mut lines: Vec<Line> = vec![header_line];
    for feed in &property.feeds {
        let (icon, icon_style) = match feed.status {
            FeedStatus::Current => ("✓", Style::new().fg(POSITIVE)),
            FeedStatus::Stale => ("⚠", Style::new().fg(WARNING)),
            FeedStatus::Missing => ("✗", Style::new().fg(NEGATIVE)),
        };
        let period = feed.latest_period_or_date.as_deref().unwrap_or("—");
        let note: String = feed.note.chars().take(note_w.max(4)).collect();
        let row_style = if feed.required_for_owner_report {
            Style::new()
        } else {
            Style::new().dim()
        };
        lines.push(Line::from(vec![
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(format!("{:<16}", feed.name), row_style),
            Span::raw(" "),
            Span::styled(format!("{:<10}", period), row_style),
            Span::raw(" "),
            Span::styled(note, row_style),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);

    let questions: Vec<Line> = if property.operator_questions.is_empty() {
        vec![Line::from(Span::styled(
            "No close-readiness questions for this period.",
            Style::new().dim(),
        ))]
    } else {
        property
            .operator_questions
            .iter()
            .map(|question| Line::from(format!("• {question}")))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(questions)
            .wrap(Wrap { trim: false })
            .block(rounded_block(" Operator Questions ")),
        questions_area,
    );
}

fn draw_mappings(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.mappings.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Account Mappings ",
            "No mappings awaiting review.",
            "New unmapped accounts appear here after the next GL ingest.",
        );
        return;
    }
    let header = Row::new(["Scope", "Code", "Account", "Suggested", "Conf", "Status"])
        .style(Style::new().dim());
    let rows = app.mappings.iter().map(|mapping| {
        let suggested = account_review::suggested_category_for(mapping)
            .filter(|category| !category.trim().is_empty());
        let suggested_cell = match &suggested {
            Some(category) => Cell::from(category.clone()).style(Style::new().fg(POSITIVE)),
            None => Cell::from("—").style(Style::new().dim()),
        };
        Row::new(vec![
            Cell::from(mapping.property_scope.clone()),
            Cell::from(mapping.account_code.clone()),
            Cell::from(mapping.account_name.clone()),
            suggested_cell,
            Cell::from(format!("{:.0}%", mapping.confidence_score * 100.0)),
            Cell::from(mapping.status.clone()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Min(24),
            Constraint::Length(22),
            Constraint::Length(5),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(rounded_block(
        " Account Mappings — approve to reclassify GL ",
    ))
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.mapping_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_category_picker(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let Some(picker_index) = app.picker else {
        return;
    };
    let account = app
        .selected_mapping()
        .map(|mapping| format!(" {} {} → ", mapping.account_code, mapping.account_name))
        .unwrap_or_default();
    let height = (ontology::NOI_CATEGORIES.len() + 2) as u16;
    let [popup] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(44)])
        .flex(Flex::Center)
        .areas(popup);
    frame.render_widget(Clear, popup);
    let items: Vec<ListItem> = ontology::NOI_CATEGORIES
        .iter()
        .map(|category| ListItem::new(format!("  {category}")))
        .collect();
    let picker_title = format!(" Approve{account}");
    let list = List::new(items)
        .block(rounded_block_focused(&picker_title))
        .highlight_style(Style::new().bold().fg(Color::Black).bg(ACCENT));
    let mut state = ListState::default().with_selected(Some(picker_index));
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_empty_state(frame: &mut Frame, area: Rect, title: &str, message: &str, hint: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(format!("  {message}")),
            Line::from(""),
            Line::from(Span::styled(format!("  {hint}"), Style::new().fg(ACCENT))),
        ])
        .block(rounded_block(title)),
        area,
    );
}

fn draw_ledger(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.properties.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Ledger ",
            "No properties found in this database.",
            "boxscore ingest-standardized gl --all",
        );
        return;
    }
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    draw_ledger_accounts(frame, app, left);
    draw_ledger_transactions(frame, app, right);
}

fn draw_ledger_accounts(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let accounts_focused = app.ledger_focus == LedgerFocus::Accounts;
    let title = if accounts_focused {
        " Accounts ▌"
    } else {
        " Accounts "
    };
    if app.ledger_accounts.is_empty() {
        draw_empty_state(
            frame,
            area,
            title,
            "No GL transactions for this property + period.",
            "boxscore ingest-standardized transactions --all",
        );
        return;
    }
    let header = Row::new(["Code", "Account", "Txns", "Total"]).style(Style::new().dim());
    let rows = app.ledger_accounts.iter().map(|acct| {
        Row::new(vec![
            Cell::from(acct.account_code.clone()),
            Cell::from(acct.account_name.clone()),
            Cell::from(acct.txn_count.to_string()),
            Cell::from(format!("${:.0}", acct.total))
                .style(Style::new().fg(amount_color(acct.total))),
        ])
    });
    let block_style = if accounts_focused {
        Style::new().fg(ACCENT)
    } else {
        Style::default()
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Min(16),
            Constraint::Length(5),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(block_style)
            .title(title),
    )
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.ledger_account_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_ledger_transactions(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let txns_focused = app.ledger_focus == LedgerFocus::Transactions;
    let filter_label = app
        .ledger_filter
        .as_deref()
        .map(|f| format!(" filter: /{f}"))
        .unwrap_or_default();
    let title = format!(" Transactions{filter_label} ");
    let block_style = if txns_focused {
        Style::new().fg(ACCENT)
    } else {
        Style::default()
    };
    if app.ledger_txns.is_empty() {
        draw_empty_state(
            frame,
            area,
            &title,
            "Select an account and press enter, or use / to search.",
            "",
        );
        return;
    }
    let header = Row::new(["Date", "Payee", "Amount", "Remarks"]).style(Style::new().dim());
    let rows = app.ledger_txns.iter().map(|txn| {
        let date = txn.txn_date.as_deref().unwrap_or("—");
        let payee = if txn.is_resident != 0 {
            format!("{} (resident)", txn.payee)
        } else {
            txn.payee.clone()
        };
        let amount_str = format!("${:.2}", txn.amount);
        let remarks = txn
            .remarks
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect::<String>();
        let payee_cell = if txn.is_resident != 0 {
            Cell::from(payee).style(Style::new().dim())
        } else {
            Cell::from(payee)
        };
        Row::new(vec![
            Cell::from(date.to_string()),
            payee_cell,
            Cell::from(amount_str).style(Style::new().fg(amount_color(txn.amount))),
            Cell::from(remarks),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Min(24),
            Constraint::Length(12),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(block_style)
            .title(title),
    )
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.ledger_txn_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_statements(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let Some(ref stmt) = app.statements else {
        draw_empty_state(
            frame,
            area,
            " T12 Statements ",
            "No T12 data loaded. Select a property with data.",
            "boxscore ingest-standardized gl --all",
        );
        return;
    };

    // Build header: label column (24) + 12 period columns (8 each).
    let label_w = 24u16;
    let col_w = 8u16;

    // Short period labels: e.g. "2025-05" → "May25", "2026-04" → "Apr26"
    let month_abbrs = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let short_labels: Vec<String> = stmt
        .periods
        .iter()
        .map(|p| {
            if let Ok((year, month)) = crate::db::parse_period_label(p) {
                let abbr = month_abbrs.get((month - 1) as usize).unwrap_or(&"???");
                format!("{}{}", abbr, year % 100)
            } else {
                p.clone()
            }
        })
        .collect();

    // Build rows for the table.
    let header_cells: Vec<Cell> = std::iter::once(Cell::from("Category"))
        .chain(short_labels.iter().map(|l| Cell::from(l.clone())))
        .collect();
    let header = Row::new(header_cells).style(Style::new().dim());

    let sep_cells: Vec<Cell> = std::iter::once(Cell::from("─".repeat(label_w as usize)))
        .chain((0..12).map(|_| Cell::from("─".repeat(col_w as usize))))
        .collect();
    let sep_row = Row::new(sep_cells).style(Style::new().dim());

    let mut rows: Vec<Row> = Vec::new();
    let mut last_type: Option<T12RowType> = None;
    for row in &stmt.rows {
        let needs_sep = matches!(
            (&last_type, &row.row_type),
            (Some(T12RowType::Expense), T12RowType::Noi)
                | (Some(T12RowType::Noi), T12RowType::Unmapped)
        );
        if needs_sep {
            rows.push(sep_row.clone());
        }
        last_type = Some(row.row_type.clone());

        let label = row.label.chars().take(label_w as usize).collect::<String>();
        let cells: Vec<Cell> = std::iter::once(Cell::from(label))
            .chain(row.values.iter().map(|&v| {
                Cell::from(compact_dollars(v)).style(Style::new().fg(if v >= 0.0 {
                    POSITIVE
                } else {
                    NEGATIVE
                }))
            }))
            .collect();

        let row_widget = Row::new(cells);
        rows.push(match row.row_type {
            T12RowType::Noi => row_widget.style(Style::new().bold().fg(ACCENT)),
            T12RowType::Unmapped => row_widget.style(Style::new().dim()),
            _ => row_widget,
        });
    }

    let mut widths = vec![Constraint::Length(label_w)];
    for _ in 0..12 {
        widths.push(Constraint::Length(col_w));
    }

    let table = Table::new(rows, widths)
        .header(header)
        .block(rounded_block(" T12 Statements "));
    frame.render_widget(table, area);
}

fn format_money(amount: f64) -> String {
    format!("${:.2}", amount)
}

/// F5.4 — the LP artifact: a budget-NOI → actual-NOI waterfall.
///
/// One row per [`BridgeStep`]: a left category label, a horizontal block bar
/// offset to its running baseline (favorable green / unfavorable red, anchors
/// in ACCENT), then a right-aligned signed `$` delta and the running total.
/// Bars are scaled by `noi_bridge::scale_bars` to a shared axis so every bar is
/// directly comparable in magnitude.
fn draw_noi_bridge(frame: &mut Frame, app: &DeskApp, area: Rect) {
    use crate::tui::noi_bridge::scale_bars;

    if app.bridge_steps.is_empty() {
        let period = app.bridge_period.as_deref().unwrap_or("this period");
        draw_empty_state(
            frame,
            area,
            " NOI Bridge ",
            &format!("No actuals for {period} — ingest GL or pick another period."),
            "boxscore ingest-standardized gl --all   ·   [ ] period   < > property",
        );
        return;
    }

    let property_name = app
        .properties
        .get(app.bridge_property)
        .map(|p| p.property.as_str())
        .unwrap_or("—");
    let period = app.bridge_period.as_deref().unwrap_or("—");

    let inner_w = area.width.saturating_sub(2) as usize; // borders
                                                         // Column budget: label | bar | numbers. Keep the bar generous but always
                                                         // leave room for the $ columns so labels never collide with the chart.
    let label_w = 22usize.min(inner_w.saturating_sub(28).max(8));
    let num_w = 26usize.min(inner_w.saturating_sub(label_w + 4));
    let bar_w = inner_w
        .saturating_sub(label_w)
        .saturating_sub(num_w)
        .saturating_sub(2)
        .max(1);

    let bars = scale_bars(&app.bridge_steps, bar_w as u16);

    let total_delta = app.bridge_actual_noi - app.bridge_budget_noi;
    let mut lines: Vec<Line> = Vec::with_capacity(app.bridge_steps.len() + 2);

    // Header line: property · period · the favorable/unfavorable delta.
    let delta_word = if total_delta >= 0.0 {
        "favorable"
    } else {
        "unfavorable"
    };
    let delta_style = if total_delta >= 0.0 {
        POSITIVE
    } else {
        NEGATIVE
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("{property_name}  ·  {period}  ·  NOI Δ "),
            Style::new().dim(),
        ),
        Span::styled(
            format!("{} ({delta_word})", compact_dollars(total_delta)),
            Style::new().fg(delta_style).bold(),
        ),
    ]));
    lines.push(Line::from(""));

    for (i, step) in app.bridge_steps.iter().enumerate() {
        let (offset, len) = bars.get(i).copied().unwrap_or((0, 0));
        let offset = offset.max(0) as usize;
        let len = len.max(0) as usize;

        // Label column, left-aligned and clipped.
        let label = {
            let mut s: String = step.label.chars().take(label_w).collect();
            while s.chars().count() < label_w {
                s.push(' ');
            }
            s
        };
        let label_span = if step.is_anchor {
            Span::styled(label, Style::new().fg(ACCENT).bold())
        } else {
            Span::styled(label, Style::new().dim())
        };

        // Bar: `offset` blanks then `len` block chars, color by NOI impact.
        let bar_color = if step.is_anchor {
            ACCENT
        } else if step.delta >= 0.0 {
            POSITIVE
        } else {
            NEGATIVE
        };
        let pad = " ".repeat(offset.min(bar_w));
        let fill = "█".repeat(len.min(bar_w.saturating_sub(offset.min(bar_w))));
        let trail = " ".repeat(
            bar_w
                .saturating_sub(offset.min(bar_w))
                .saturating_sub(fill.chars().count()),
        );

        // Numbers column: signed delta (drivers) + running total, right-aligned.
        let num_text = if step.is_anchor {
            format!("{:>}", compact_dollars(step.running))
        } else {
            // The ▲/▼ marker already signals direction, so the magnitude is
            // shown unsigned (▼ $42.0k, not ▼ -$42.0k) for a clean LP read.
            let arrow = if step.delta >= 0.0 { "▲" } else { "▼" };
            format!(
                "{arrow} {}  → {}",
                compact_dollars(step.delta.abs()),
                compact_dollars(step.running)
            )
        };
        let num = {
            let count = num_text.chars().count();
            if count >= num_w {
                num_text.chars().take(num_w).collect::<String>()
            } else {
                format!("{}{}", " ".repeat(num_w - count), num_text)
            }
        };
        let num_style = if step.is_anchor {
            Style::new().fg(ACCENT).bold()
        } else if step.delta >= 0.0 {
            Style::new().fg(POSITIVE)
        } else {
            Style::new().fg(NEGATIVE)
        };

        lines.push(Line::from(vec![
            label_span,
            Span::raw(" "),
            Span::raw(pad),
            Span::styled(fill, Style::new().fg(bar_color)),
            Span::raw(trail),
            Span::raw(" "),
            Span::styled(num, num_style),
        ]));
    }

    let title = format!(" NOI Bridge — Budget → Actual ({period}) ");
    frame.render_widget(Paragraph::new(lines).block(rounded_block(title)), area);
}

fn draw_vendors(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.vendors.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Vendors ",
            "No vendor activity.",
            "boxscore ingest-standardized transactions --all",
        );
        return;
    }

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);

    draw_vendor_list(frame, app, left);
    draw_vendor_detail(frame, app, right);
}

fn draw_vendor_list(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let header = Row::new(["Payee", "Txns", "Total"]).style(Style::new().dim());
    let rows = app.vendors.iter().map(|v| {
        let total_str = format_money(v.total);
        Row::new(vec![
            Cell::from(v.payee.clone()),
            Cell::from(v.txn_count.to_string()),
            Cell::from(total_str).style(Style::new().fg(amount_color(v.total))),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .block(rounded_block(" Vendors "))
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.vendor_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_vendor_detail(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let [shares_area, txns_area] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    // ── Per-property shares ───────────────────────────────────────────────────
    if let Some(vendor) = app.vendors.get(app.vendor_selected) {
        let payee_title = vendor.payee.chars().take(30).collect::<String>();
        let shares_title = format!(" {payee_title} — by property ");
        let shares_header = Row::new(["Property", "Txns", "Total"]).style(Style::new().dim());
        let shares_rows = vendor.by_property.iter().map(|share| {
            let total_str = format_money(share.total);
            Row::new(vec![
                Cell::from(share.property.clone()),
                Cell::from(share.txn_count.to_string()),
                Cell::from(total_str).style(Style::new().fg(amount_color(share.total))),
            ])
        });
        let shares_table = Table::new(
            shares_rows,
            [
                Constraint::Min(16),
                Constraint::Length(6),
                Constraint::Length(14),
            ],
        )
        .header(shares_header)
        .block(rounded_block(&shares_title));
        frame.render_widget(shares_table, shares_area);
    } else {
        frame.render_widget(rounded_block(" — by property "), shares_area);
    }

    // ── Recent transactions ───────────────────────────────────────────────────
    if app.vendor_txns.is_empty() {
        draw_empty_state(
            frame,
            txns_area,
            " Recent Transactions ",
            "No transactions for this vendor.",
            "",
        );
        return;
    }
    let txns_header = Row::new(["Date", "Code", "Amount", "Remarks"]).style(Style::new().dim());
    let txns_rows = app.vendor_txns.iter().map(|txn| {
        let date = txn.txn_date.as_deref().unwrap_or("—");
        let amount_str = format_money(txn.amount);
        let remarks = txn
            .remarks
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(38)
            .collect::<String>();
        Row::new(vec![
            Cell::from(date.to_string()),
            Cell::from(txn.account_code.clone()),
            Cell::from(amount_str).style(Style::new().fg(amount_color(txn.amount))),
            Cell::from(remarks),
        ])
    });
    let txns_table = Table::new(
        txns_rows,
        [
            Constraint::Length(11),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Min(18),
        ],
    )
    .header(txns_header)
    .block(rounded_block(" Recent Transactions "));
    frame.render_widget(txns_table, txns_area);
}

fn draw_delinquency(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.properties.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Delinquency ",
            "No properties found in this database.",
            "boxscore ingest-standardized collections --all",
        );
        return;
    }
    if app.delin_residents.is_empty()
        && app.delin_aging.delinquent_cnt() == 0
        && app.delin_aging.prepaid_cnt == 0
    {
        draw_empty_state(
            frame,
            area,
            " Delinquency ",
            &format!("No receivables for {}.", app.period),
            "Ingest aged_receivables.csv or press [ / ] to change period",
        );
        return;
    }

    // (a) aging strip · (b) residents + watchlist split · (c) trend row.
    let [aging_area, mid_area, trend_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .areas(area);

    draw_delin_aging_strip(frame, &app.delin_aging, aging_area);

    let [residents_area, watchlist_area] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .areas(mid_area);
    draw_delin_residents(frame, app, residents_area);
    draw_delin_watchlist(frame, &app.delin_watchlist, watchlist_area);

    draw_delin_trend(frame, &app.delin_trend, trend_area);
}

/// The five aging buckets + prepaid cell, colored by severity, with $ and count.
fn draw_delin_aging_strip(frame: &mut Frame, aging: &DelinquencyAging, area: Rect) {
    let cells = [
        Constraint::Ratio(1, 6),
        Constraint::Ratio(1, 6),
        Constraint::Ratio(1, 6),
        Constraint::Ratio(1, 6),
        Constraint::Ratio(1, 6),
        Constraint::Ratio(1, 6),
    ];
    let areas: [Rect; 6] = Layout::horizontal(cells).areas(area);

    // (label, amount, count, color)
    let buckets: [(&str, f64, i64, Color); 6] = [
        (
            "Current",
            aging.current_owed_total,
            aging.cnt_current,
            Color::Gray,
        ),
        ("0-30", aging.b0_30, aging.cnt_0_30, NEGATIVE),
        ("31-60", aging.b31_60, aging.cnt_31_60, WARNING),
        ("61-90", aging.b61_90, aging.cnt_61_90, NEGATIVE),
        ("90+", aging.b90_plus, aging.cnt_90_plus, NEGATIVE),
        ("Prepaid", aging.prepaid_total, aging.prepaid_cnt, POSITIVE),
    ];

    for (i, (label, amount, count, color)) in buckets.iter().enumerate() {
        // Current bucket is dim (owed-but-not-late); 31-60 WARNING; 61-90/90+ red.
        let amount_style = if *label == "Current" {
            Style::new().dim()
        } else {
            Style::new().fg(*color).bold()
        };
        let dollar = compact_dollars(*amount);
        let body = Paragraph::new(vec![
            Line::from(Span::styled(dollar, amount_style)),
            Line::from(Span::styled(format!("{count} units"), Style::new().dim())),
        ])
        .block(rounded_block(format!(" {label} ")));
        frame.render_widget(body, areas[i]);
    }
}

/// Per-resident table: worst-first, owed in NEGATIVE red, prepaids POSITIVE green.
fn draw_delin_residents(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let title = format!(
        " Residents — {} owed ({} sort) ",
        app.delin_residents.len(),
        app.delin_sort.label()
    );
    if app.delin_residents.is_empty() {
        draw_empty_state(
            frame,
            area,
            &title,
            "No per-resident receivables.",
            "Ingest aged_receivables.csv",
        );
        return;
    }
    let header = Row::new(["Resident", "Status", "Days", "Owed"]).style(Style::new().dim());
    let rows = app.delin_residents.iter().map(|r| {
        let name: String = r
            .resident_name
            .as_deref()
            .unwrap_or(&r.resident_code)
            .chars()
            .take(22)
            .collect();
        let status: String = r
            .resident_status
            .as_deref()
            .unwrap_or("—")
            .chars()
            .take(10)
            .collect();
        let days = r
            .days_late
            .map(|d| d.to_string())
            .unwrap_or_else(|| "—".to_string());
        // Prepaid (credit) shows the negative current_owed in green; otherwise
        // the delinquent balance in red.
        let is_prepaid = r.current_owed.map(|c| c < 0.0).unwrap_or(false);
        let (owed_val, owed_style) = if is_prepaid {
            (r.current_owed.unwrap_or(0.0), Style::new().fg(POSITIVE))
        } else {
            (r.total_delinquent, Style::new().fg(NEGATIVE).bold())
        };
        let owed = compact_dollars(owed_val);
        Row::new(vec![
            Cell::from(name),
            Cell::from(status),
            Cell::from(days),
            Cell::from(owed).style(owed_style),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(16),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(rounded_block(title))
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.delin_resident_sel));
    frame.render_stateful_widget(table, area, &mut state);
}

/// BDDRE high-risk watchlist: risk score + tier + recommended action, worst-first.
fn draw_delin_watchlist(frame: &mut Frame, watchlist: &[RiskRow], area: Rect) {
    let title = format!(" Risk Watchlist ({}) ", watchlist.len());
    if watchlist.is_empty() {
        draw_empty_state(
            frame,
            area,
            &title,
            "No BDDRE risk rows for this property.",
            "Run weekly-ops to generate bddre_intervention_queue.csv",
        );
        return;
    }
    let header = Row::new(["Unit", "Resident", "Risk", "Action"]).style(Style::new().dim());
    let rows = watchlist.iter().take(40).map(|r| {
        let name: String = r.resident_name.chars().take(16).collect();
        let action: String = if r.top_action.is_empty() {
            "watch".to_string()
        } else {
            r.top_action.chars().take(28).collect()
        };
        let tier_style = match r.risk_tier.as_str() {
            "High" => Style::new().fg(NEGATIVE).bold(),
            "Medium" => Style::new().fg(WARNING),
            _ => Style::new().dim(),
        };
        Row::new(vec![
            Cell::from(r.unit.clone()),
            Cell::from(name),
            Cell::from(format!("{:.0} {}", r.risk_score, r.risk_tier)).style(tier_style),
            Cell::from(action),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(12),
            Constraint::Length(11),
            Constraint::Min(16),
        ],
    )
    .header(header)
    .block(rounded_block(title))
    .row_highlight_style(Style::new().bold().bg(SEL_BG));
    frame.render_widget(table, area);
}

/// Compact delinquency-trend sparkline with first/last $ labels.
fn draw_delin_trend(frame: &mut Frame, trend: &[(String, f64)], area: Rect) {
    if trend.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  no delinquency snapshot history",
                Style::new().dim(),
            )))
            .block(rounded_block(" Delinquency Trend ")),
            area,
        );
        return;
    }
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = trend
        .iter()
        .map(|(_, v)| *v)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let spark: String = trend
        .iter()
        .map(|(_, v)| {
            let idx = ((v / max) * (bars.len() - 1) as f64).round() as usize;
            bars[idx.min(bars.len() - 1)]
        })
        .collect();

    let first = trend.first().map(|(_, v)| *v).unwrap_or(0.0);
    let last = trend.last().map(|(_, v)| *v).unwrap_or(0.0);
    // Falling delinquency is favorable (green); rising is unfavorable (red).
    let arrow_style = if last <= first {
        Style::new().fg(POSITIVE)
    } else {
        Style::new().fg(NEGATIVE)
    };
    let arrow = if last <= first {
        "▼ improving"
    } else {
        "▲ rising"
    };

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(compact_dollars(first), Style::new().dim()),
        Span::raw("  "),
        Span::styled(spark, Style::new().fg(ACCENT)),
        Span::raw("  "),
        Span::styled(compact_dollars(last), Style::new().bold()),
        Span::raw("   "),
        Span::styled(arrow, arrow_style),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(rounded_block(" Delinquency Trend ")),
        area,
    );
}

fn draw_renewals(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.properties.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Renewals ",
            "No properties found in this database.",
            "boxscore ingest-standardized rent-roll --all",
        );
        return;
    }
    if app.renew_leases.is_empty() && app.renew_recs.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Renewals ",
            &format!("No lease or renewal data for {}.", app.period),
            "Ingest rent_roll.csv / run weekly-ops, or press [ / ] to change period",
        );
        return;
    }

    // (a) window funnel + opportunity badge · (b) per-unit renewal table.
    let [funnel_area, table_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(6)]).areas(area);

    draw_renew_funnel(frame, app, funnel_area);
    draw_renew_table(frame, app, table_area);
}

/// Expiration funnel (≤30 / 31-60 / 61-90 / 90+) as four bar cells, urgent in
/// red and widening windows cooler, plus the renewal-opportunity badge.
fn draw_renew_funnel(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let [windows_area, badge_area] =
        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)]).areas(area);

    let cells: [Rect; 4] = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .areas(windows_area);

    // (label, count, color) — soonest expiry is the most urgent (red), the
    // 90+ window is the coolest.
    let windows: [(&str, usize, Color); 4] = [
        ("≤30d", app.renew_windows[0], NEGATIVE),
        ("31-60d", app.renew_windows[1], WARNING),
        ("61-90d", app.renew_windows[2], ACCENT),
        ("90+d", app.renew_windows[3], Color::Gray),
    ];
    let max = windows.iter().map(|(_, c, _)| *c).max().unwrap_or(0).max(1);
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    for (i, (label, count, color)) in windows.iter().enumerate() {
        // Highlight the active window filter, if any.
        let is_filtered = app.renew_window_filter == Some(i);
        let idx = ((*count as f64 / max as f64) * (bars.len() - 1) as f64).round() as usize;
        let bar: String = std::iter::repeat_n(bars[idx.min(bars.len() - 1)], 8).collect();
        let count_style = if *count > 0 {
            Style::new().fg(*color).bold()
        } else {
            Style::new().dim()
        };
        let title = if is_filtered {
            format!(" ▸{label} ")
        } else {
            format!(" {label} ")
        };
        let body = Paragraph::new(vec![
            Line::from(Span::styled(format!("{count}"), count_style)),
            Line::from(Span::styled(bar, Style::new().fg(*color))),
        ])
        .block(rounded_block(title));
        frame.render_widget(body, cells[i]);
    }

    // Opportunity badge: monthly + annual uplift, underpriced count.
    let (monthly, annual, underpriced) = app.renew_opportunity;
    let badge = Paragraph::new(vec![
        Line::from(vec![
            Span::raw("+"),
            Span::styled(compact_dollars(monthly), Style::new().fg(POSITIVE).bold()),
            Span::styled("/mo", Style::new().dim()),
        ]),
        Line::from(vec![
            Span::raw("+"),
            Span::styled(compact_dollars(annual), Style::new().fg(POSITIVE).bold()),
            Span::styled("/yr", Style::new().dim()),
            Span::raw("  "),
            Span::styled(format!("{underpriced} under"), Style::new().fg(WARNING)),
        ]),
    ])
    .block(rounded_block(" Renewal Opportunity "));
    frame.render_widget(badge, badge_area);
}

/// Per-unit renewal table, most-urgent-first: current vs market vs recommended
/// rent, increase %, confidence, expiry, top driver. Underpriced units flagged.
fn draw_renew_table(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let title = format!(
        " Renewals — {} units ({}) ",
        app.renew_recs.len(),
        app.renew_window_label()
    );
    if app.renew_recs.is_empty() {
        draw_empty_state(
            frame,
            area,
            &title,
            "No renewal recommendations in this window.",
            "Press f to change the expiration window, or run weekly-ops",
        );
        return;
    }
    let header = Row::new([
        "", "Unit", "Resident", "Cur", "Mkt", "Rec", "+%", "Conf", "Expiry", "Driver",
    ])
    .style(Style::new().dim());

    let rows = app.renew_recs.iter().map(renew_row);

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),  // urgency / underpriced marker
            Constraint::Length(8),  // unit
            Constraint::Min(14),    // resident
            Constraint::Length(7),  // current
            Constraint::Length(7),  // market
            Constraint::Length(7),  // recommended
            Constraint::Length(6),  // increase %
            Constraint::Length(6),  // confidence
            Constraint::Length(10), // expiry
            Constraint::Min(18),    // driver
        ],
    )
    .header(header)
    .block(rounded_block(title))
    .row_highlight_style(Style::new().bold().bg(SEL_BG))
    .highlight_symbol("▌");
    let mut state = TableState::default().with_selected(Some(app.renew_sel));
    frame.render_stateful_widget(table, area, &mut state);
}

/// Build one renewal table row from an RPCOE recommendation.
fn renew_row(r: &RentRec) -> Row<'static> {
    let name: String = r.resident_name.chars().take(16).collect();
    // Underpriced: current rent below 95% of market (money on the table).
    let underpriced = r.market_rent > 0.0 && r.current_rent < 0.95 * r.market_rent;
    // Urgent: lease expiring within 30 days.
    let urgent = r.days_to_expiration.map(|d| d <= 30).unwrap_or(false);

    // Leading marker: ! (urgent, red) takes precedence over ▲ (underpriced).
    let (marker, marker_style) = if urgent {
        ("!", Style::new().fg(NEGATIVE).bold())
    } else if underpriced {
        ("▲", Style::new().fg(WARNING))
    } else {
        (" ", Style::new())
    };

    // Current rent shows red when underpriced (below market).
    let cur_style = if underpriced {
        Style::new().fg(NEGATIVE)
    } else {
        Style::new()
    };

    // Recommended rent / increase % in green when the rec is an increase.
    let increase_style = if r.recommended_increase_pct > 0.0 {
        Style::new().fg(POSITIVE).bold()
    } else {
        Style::new().dim()
    };

    let conf_style = match r.confidence.as_str() {
        "High" => Style::new().fg(POSITIVE),
        "Medium" => Style::new().fg(WARNING),
        _ => Style::new().dim(),
    };

    let expiry: String = if r.lease_expiration.is_empty() {
        "—".to_string()
    } else {
        r.lease_expiration.chars().take(10).collect()
    };
    // Concession flag: surface a relevant concession inline with a ◆ glyph.
    // A line beginning "No concession" means none; everything else (an active
    // concession, or one recommended/optional) is worth flagging at the table.
    let has_concession =
        !r.concession.is_empty() && !r.concession.to_lowercase().starts_with("no concession");
    let driver_body: String = if r.top_driver.is_empty() {
        "—".to_string()
    } else {
        r.top_driver.chars().take(30).collect()
    };
    let driver: String = if has_concession {
        format!("◆ {driver_body}")
    } else {
        driver_body
    };

    Row::new(vec![
        Cell::from(marker).style(marker_style),
        Cell::from(r.unit.clone()),
        Cell::from(name),
        Cell::from(compact_dollars(r.current_rent)).style(cur_style),
        Cell::from(compact_dollars(r.market_rent)),
        Cell::from(compact_dollars(r.recommended_new_rent)).style(increase_style),
        Cell::from(format!("{:+.1}%", r.recommended_increase_pct)).style(increase_style),
        Cell::from(r.confidence.clone()).style(conf_style),
        Cell::from(expiry),
        Cell::from(driver),
    ])
}

fn draw_track_record(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let [top_area, bottom_area] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    // ── Section 1: Batting Averages (track_record memories) ─────────────────
    if app.track_record_memories.is_empty() {
        draw_empty_state(
            frame,
            top_area,
            " Batting Averages ",
            "No track-record memories yet.",
            "Memories are created when calls are scored.",
        );
    } else {
        let header = Row::new(["Scope", "Key", "Headline"]).style(Style::new().dim());
        let rows = app.track_record_memories.iter().map(|m| {
            let value: String = m.value.chars().take(60).collect();
            Row::new(vec![
                Cell::from(m.scope.clone()),
                Cell::from(m.key.clone()),
                Cell::from(value),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(16),
                Constraint::Length(20),
                Constraint::Min(20),
            ],
        )
        .header(header)
        .block(rounded_block(" Batting Averages "))
        .row_highlight_style(Style::new().bold().bg(SEL_BG));
        frame.render_widget(table, top_area);
    }

    // ── Section 2: Recent Calls ──────────────────────────────────────────────
    if app.track_record_calls.is_empty() {
        draw_empty_state(
            frame,
            bottom_area,
            " Recent Calls ",
            "No calls recorded yet.",
            "Calls are created by the scoring engine.",
        );
    } else {
        let header =
            Row::new(["Type", "Status", "Mature By", "Score", "Outcome"]).style(Style::new().dim());
        let rows = app.track_record_calls.iter().take(20).map(|c| {
            let score = c
                .score
                .map(|s| format!("{:.2}", s))
                .unwrap_or_else(|| "—".to_string());
            let outcome: String = c
                .outcome_summary
                .as_deref()
                .unwrap_or("—")
                .chars()
                .take(48)
                .collect();
            let score_style = match c.score {
                Some(s) if s >= 0.7 => Style::new().fg(POSITIVE),
                Some(s) if s >= 0.4 => Style::new().fg(WARNING),
                Some(_) => Style::new().fg(NEGATIVE),
                None => Style::new().dim(),
            };
            Row::new(vec![
                Cell::from(c.call_type.clone()),
                Cell::from(c.status.clone()),
                Cell::from(c.mature_by.clone()),
                Cell::from(score).style(score_style),
                Cell::from(outcome),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(14),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(6),
                Constraint::Min(20),
            ],
        )
        .header(header)
        .block(rounded_block(" Recent Calls (last 20) "))
        .row_highlight_style(Style::new().bold().bg(SEL_BG));
        frame.render_widget(table, bottom_area);
    }
}

fn draw_reports(frame: &mut Frame, app: &DeskApp, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(area);

    // ── Left pane: report list ────────────────────────────────────────────────
    if app.reports.is_empty() {
        draw_empty_state(
            frame,
            left,
            " Weekly Reports ",
            &format!("No RPCOE/BDDRE reports for {}.", app.period),
            "Press [ / ] to browse other months",
        );
    } else {
        let items: Vec<ListItem> = app
            .reports
            .iter()
            .map(|r| ListItem::new(r.label()))
            .collect();
        let list = List::new(items)
            .block(rounded_block(" Weekly Reports "))
            .highlight_style(Style::new().bold().bg(SEL_BG));
        let mut state = ListState::default().with_selected(Some(app.report_sel));
        frame.render_stateful_widget(list, left, &mut state);
    }

    // ── Right pane: report reader ─────────────────────────────────────────────
    let reader_title = if let Some(report) = app.reports.get(app.report_sel) {
        format!(" {} ", report.label())
    } else {
        " — ".to_string()
    };

    // Reserve 1 column on the right edge for the scrollbar.
    let [content_area, scrollbar_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(right);

    // Render at the live inner width so wide tables use the full terminal.
    // The border is 1 col each side, so subtract 2.
    let inner_w = content_area.width.saturating_sub(2);
    let lines = crate::tui::markdown::render_markdown(&app.report_source, inner_w);
    let total_lines = lines.len();
    let paragraph = Paragraph::new(Text::from(lines))
        .scroll((app.report_scroll, 0))
        .block(rounded_block(reader_title));
    frame.render_widget(paragraph, content_area);

    // Vertical scrollbar
    let mut scrollbar_state = ScrollbarState::new(total_lines).position(app.report_scroll as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        scrollbar_area,
        &mut scrollbar_state,
    );
}

fn draw_ask(frame: &mut Frame, app: &DeskApp, area: Rect) {
    if app.ask_output.is_empty() {
        draw_empty_state(
            frame,
            area,
            " Ask Results ",
            "No results yet.",
            ": ask a question from any screen",
        );
        return;
    }

    let visible_height = area.height.saturating_sub(2) as usize; // subtract block borders
                                                                 // Clamp the skip so the viewport never scrolls past the last screenful;
                                                                 // the stored scroll value may overshoot since it can't see this height.
    let max_skip = app.ask_output.len().saturating_sub(visible_height);
    let scroll = app.ask_scroll.min(max_skip);
    let lines: Vec<Line> = app
        .ask_output
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|line| Line::from(line.as_str()))
        .collect();

    frame.render_widget(
        Paragraph::new(lines)
            .block(rounded_block(" Ask Results "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn amount_color(amount: f64) -> Color {
    if amount >= 0.0 {
        POSITIVE
    } else {
        NEGATIVE
    }
}

fn draw_footer(frame: &mut Frame, app: &DeskApp, area: Rect) {
    // Ask input takes priority when the operator is typing a query.
    if app.ask_input.is_some() && app.load_error.is_none() && app.toast.is_none() {
        let text = app.ask_input.as_deref().unwrap_or("");
        let prompt = format!(" :{text}\u{258c}");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(prompt, Style::new().fg(ACCENT)))),
            area,
        );
        return;
    }

    // Ledger typing mode takes priority: render the live filter prompt.
    if app.screen == Screen::Ledger && app.load_error.is_none() && app.toast.is_none() {
        if let Some(text) = &app.ledger_input {
            let prompt = format!(" /{text}▌");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(prompt, Style::new().fg(ACCENT)))),
                area,
            );
            return;
        }
    }

    // Vendors typing mode: render the live filter prompt.
    if app.screen == Screen::Vendors && app.load_error.is_none() && app.toast.is_none() {
        if let Some(text) = &app.vendors_input {
            let prompt = format!(" /{text}▌");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(prompt, Style::new().fg(ACCENT)))),
                area,
            );
            return;
        }
    }

    let line = if let Some(error) = &app.load_error {
        Line::from(Span::styled(
            format!(" error: {error}"),
            Style::new().fg(NEGATIVE),
        ))
    } else if let Some(toast) = &app.toast {
        Line::from(Span::styled(
            format!(" {toast}"),
            Style::new().fg(Color::Green),
        ))
    } else if app.sidebar_focused {
        Line::from(Span::styled(
            " sidebar  j/k navigate · enter/l select · [ ] period · : ask · ? help · q quit",
            Style::new().fg(ACCENT),
        ))
    } else {
        let hints = match (app.screen, app.picker.is_some()) {
            (_, true) => " j/k choose · enter approve · esc cancel",
            (Screen::CloseDesk, _) => {
                " 1-9/0 sections · tab cycle · [ ] period · j/k select · h sidebar · r refresh · : ask · ? help · q quit"
            }
            (Screen::Mappings, _) => {
                " 1-9/0 sections · tab cycle · j/k select · a approve suggested · enter category · r refresh · : ask · ? help · q quit"
            }
            (Screen::Ledger, _) => match app.ledger_focus {
                LedgerFocus::Accounts => {
                    " 1-9/0 sections · tab cycle · [ ] period · j/k select · enter/l drill · / filter · h sidebar · : ask · ? help · q quit"
                }
                LedgerFocus::Transactions => {
                    " 1-9/0 sections · tab cycle · [ ] period · j/k select · h accounts · / filter · : ask · ? help · q quit"
                }
            },
            (Screen::Statements, _) => {
                " 1-9/0 sections · tab cycle · p property · [ ] shift window · h sidebar · : ask · ? help · q quit"
            }
            (Screen::NoiBridge, _) => {
                " 1-9/0 sections · tab cycle · < > property · [ ] period · r refresh · : ask · ? help · q quit"
            }
            (Screen::Vendors, _) => {
                " 1-9/0 sections · tab cycle · [ ] window · j/k select · / filter · esc clear · h sidebar · : ask · ? help · q quit"
            }
            (Screen::Delinquency, _) => {
                " 1-9/0 sections · tab cycle · j/k select · s sort · < > property · [ ] period · r refresh · : ask · ? help · q quit"
            }
            (Screen::Renewals, _) => {
                " 1-9/0 sections · tab cycle · j/k select · f window · < > property · [ ] period · r refresh · : ask · ? help · q quit"
            }
            (Screen::TrackRecord, _) => {
                " 1-9/0 sections · tab cycle · j/k select · r refresh · h sidebar · : ask · ? help · q quit"
            }
            (Screen::Reports, _) => {
                " 1-9/0 sections · tab cycle · [ ] period · j/k select · PgUp/PgDn scroll · r refresh · : ask · ? help · q quit"
            }
            (Screen::Ask, _) => {
                " q/esc back · j/k scroll · g/G top/bottom · : ask · ? help"
            }
        };
        Line::from(Span::styled(hints, Style::new().dim()))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn readiness_label(property: &PropertyCloseReadiness) -> &'static str {
    if property.owner_ready {
        "owner-ready"
    } else {
        "not ready"
    }
}

fn readiness_color(property: &PropertyCloseReadiness) -> Color {
    if property.owner_ready {
        POSITIVE
    } else {
        NEGATIVE
    }
}
