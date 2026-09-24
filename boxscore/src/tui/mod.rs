//! Boxscore — the terminal workbench for the harness.
//!
//! Phase 1: the Close Desk, a portfolio close-readiness board.
//! Phase 2: inline account-mapping review — approve mappings with single
//! keystrokes instead of the CSV export/import roundtrip. Approval is the one
//! mutating action, and it is operator-initiated keystroke by keystroke:
//! human-in-the-loop by construction.

pub mod app;
pub mod bddre;
pub mod markdown;
pub mod noi_bridge;
pub mod reports;
pub mod rpcoe;
pub mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use sqlx::SqlitePool;

use crate::{ask, model_provider};
use app::{DeskApp, LedgerFocus, Screen};

/// Current month in `YYYY-MM`, local time — the default close period.
pub fn current_period() -> String {
    chrono::Local::now().format("%Y-%m").to_string()
}

pub async fn run_desk(pool: SqlitePool, period: String) -> Result<()> {
    let mut app = DeskApp::new(period);
    app.reload(&pool).await;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &pool).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut DeskApp,
    pool: &SqlitePool,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        // Keep the Ask viewport height current so scroll bounds are exact
        // (body area = total height minus header 2, footer 1, borders 2).
        if let Ok(size) = terminal.size() {
            app.ask_viewport = (size.height as usize).saturating_sub(5).max(1);
        }
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }

        // The help overlay captures input while open, BEFORE global nav so `?`
        // never navigates and a new operator can dismiss it with any of the
        // expected keys. (highest priority — same tier as the category picker.)
        if app.help_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => app.help_open = false,
                _ => {}
            }
            continue;
        }

        // The category picker captures input while open. (highest priority)
        if app.picker.is_some() {
            match key.code {
                KeyCode::Esc => app.close_picker(),
                KeyCode::Char('j') | KeyCode::Down => app.picker_next(),
                KeyCode::Char('k') | KeyCode::Up => app.picker_previous(),
                KeyCode::Enter => app.approve_with_picker_choice(pool).await,
                _ => {}
            }
            continue;
        }

        // The ledger filter input captures ALL keys while typing mode is active.
        if app.screen == Screen::Ledger && app.ledger_input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    app.ledger_input = None;
                    // Restore selected account's transactions if filter was not applied.
                    if app.ledger_filter.is_none() {
                        app.load_ledger_txns(pool).await;
                    }
                }
                KeyCode::Enter => {
                    let needle = app.ledger_input.take().unwrap_or_default();
                    if !needle.is_empty() {
                        app.apply_ledger_filter(pool, needle).await;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(ref mut text) = app.ledger_input {
                        text.pop();
                    }
                }
                KeyCode::Char(ch) => {
                    if let Some(ref mut text) = app.ledger_input {
                        text.push(ch);
                    }
                }
                _ => {}
            }
            continue;
        }

        // The vendors filter input captures ALL keys while typing mode is active.
        // Checked AFTER picker and ledger-filter, BEFORE ask input per precedence spec.
        if app.screen == Screen::Vendors && app.vendors_input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    app.vendors_input = None;
                    // If no filter was applied, leave vendors list unchanged.
                }
                KeyCode::Enter => {
                    let needle = app.vendors_input.take().unwrap_or_default();
                    if !needle.is_empty() {
                        app.vendors_filter = Some(needle);
                    } else {
                        app.vendors_filter = None;
                    }
                    app.reload_vendors(pool).await;
                }
                KeyCode::Backspace => {
                    if let Some(ref mut text) = app.vendors_input {
                        text.pop();
                    }
                }
                KeyCode::Char(ch) => {
                    if let Some(ref mut text) = app.vendors_input {
                        text.push(ch);
                    }
                }
                _ => {}
            }
            continue;
        }

        // The ask input captures ALL keys while `:` mode is active.
        // Checked AFTER picker and ledger-filter so they keep precedence.
        if app.ask_input.is_some() {
            match key.code {
                KeyCode::Esc => {
                    app.ask_input = None;
                }
                KeyCode::Enter => {
                    let question = app.ask_input.take().unwrap_or_default();
                    if !question.is_empty() {
                        run_ask_in_tui(terminal, app, pool, question).await;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(ref mut text) = app.ask_input {
                        text.pop();
                    }
                }
                KeyCode::Char(ch) => {
                    if let Some(ref mut text) = app.ask_input {
                        text.push(ch);
                    }
                }
                _ => {}
            }
            continue;
        }

        // Global section navigation — works in any focus state.
        match key.code {
            KeyCode::Tab => {
                app.cycle_section(true);
                reload_active_screen(app, pool).await;
                continue;
            }
            KeyCode::BackTab => {
                app.cycle_section(false);
                reload_active_screen(app, pool).await;
                continue;
            }
            KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                app.goto_section_index((c as u8 - b'1') as usize);
                reload_active_screen(app, pool).await;
                continue;
            }
            // `0` is the hotkey for the 10th sidebar section (zero-based index 9).
            KeyCode::Char('0') => {
                app.goto_section_index(9);
                reload_active_screen(app, pool).await;
                continue;
            }
            _ => {}
        }

        // Sidebar navigation: intercept j/k/Enter/arrows/: when sidebar has focus.
        if app.sidebar_focused {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    app.sidebar_move_next();
                    match app.screen {
                        Screen::Statements => app.reload_statements(pool).await,
                        Screen::NoiBridge => {
                            // New property → re-resolve its own latest period.
                            app.bridge_period = None;
                            app.reload_noi_bridge(pool).await;
                        }
                        Screen::Vendors => app.reload_vendors(pool).await,
                        Screen::Ledger => app.reload_ledger_accounts(pool).await,
                        Screen::Delinquency => app.reload_delinquency(pool).await,
                        Screen::Renewals => app.reload_renewals(pool).await,
                        Screen::TrackRecord => app.reload_track_record(pool).await,
                        Screen::Reports => app.reload_reports(),
                        _ => {}
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.sidebar_move_prev();
                    match app.screen {
                        Screen::Statements => app.reload_statements(pool).await,
                        Screen::NoiBridge => {
                            app.bridge_period = None;
                            app.reload_noi_bridge(pool).await;
                        }
                        Screen::Vendors => app.reload_vendors(pool).await,
                        Screen::Ledger => app.reload_ledger_accounts(pool).await,
                        Screen::Delinquency => app.reload_delinquency(pool).await,
                        Screen::Renewals => app.reload_renewals(pool).await,
                        Screen::TrackRecord => app.reload_track_record(pool).await,
                        Screen::Reports => app.reload_reports(),
                        _ => {}
                    }
                }
                KeyCode::Enter | KeyCode::Char('l') => {
                    app.sidebar_focused = false;
                }
                KeyCode::Char(':') => {
                    app.ask_input = Some(String::new());
                }
                KeyCode::Char('r') => app.reload(pool).await,
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload(pool).await;
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload(pool).await;
                }
                _ => {}
            }
            continue;
        }

        // On the Ask results screen, Esc/q returns to the previous screen.
        if app.screen == Screen::Ask {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    app.screen = app.previous_screen;
                }
                KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                KeyCode::Char('k') | KeyCode::Up => app.select_previous(),
                KeyCode::Char('g') | KeyCode::Home => app.select_first(),
                KeyCode::Char('G') | KeyCode::End => app.select_last(),
                // `:` from Ask screen opens a new ask input.
                KeyCode::Char(':') => {
                    app.ask_input = Some(String::new());
                }
                _ => {}
            }
            continue;
        }

        // Global `:` opens the ask input from any non-modal, non-Ask screen.
        if key.code == KeyCode::Char(':') {
            app.ask_input = Some(String::new());
            continue;
        }

        // Global `?` opens the help overlay from any non-modal screen.
        if key.code == KeyCode::Char('?') {
            app.help_open = true;
            continue;
        }

        match key.code {
            // Esc inside the Ledger transaction pane steps back to the
            // accounts pane instead of killing a long-running session.
            KeyCode::Esc
                if app.screen == Screen::Ledger
                    && app.ledger_focus == LedgerFocus::Transactions =>
            {
                app.ledger_focus = LedgerFocus::Accounts;
            }
            // Esc on Vendors clears an applied filter first; only quits if none.
            KeyCode::Esc if app.screen == Screen::Vendors && app.vendors_filter.is_some() => {
                app.vendors_filter = None;
                app.reload_vendors(pool).await;
            }
            KeyCode::Char('h') | KeyCode::Left
                if !matches!(app.screen, Screen::Ledger)
                    || app.ledger_focus == LedgerFocus::Accounts =>
            {
                app.sidebar_focused = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('j') | KeyCode::Down => app.select_next(),
            KeyCode::Char('k') | KeyCode::Up => app.select_previous(),
            KeyCode::Char('g') | KeyCode::Home => app.select_first(),
            KeyCode::Char('G') | KeyCode::End => app.select_last(),
            KeyCode::Char('r') => app.reload(pool).await,
            _ => {}
        }

        match app.screen {
            Screen::CloseDesk => match key.code {
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload(pool).await;
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload(pool).await;
                }
                _ => {}
            },
            Screen::Mappings => match key.code {
                KeyCode::Char('a') => app.approve_with_suggestion(pool).await,
                KeyCode::Char('e') | KeyCode::Enter => app.open_picker(),
                _ => {}
            },
            Screen::Ledger => match key.code {
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload_ledger_accounts(pool).await;
                    if app.ledger_filter.is_some() {
                        let filter = app.ledger_filter.clone().unwrap();
                        app.apply_ledger_filter(pool, filter).await;
                    } else {
                        app.load_ledger_txns(pool).await;
                    }
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload_ledger_accounts(pool).await;
                    if app.ledger_filter.is_some() {
                        let filter = app.ledger_filter.clone().unwrap();
                        app.apply_ledger_filter(pool, filter).await;
                    } else {
                        app.load_ledger_txns(pool).await;
                    }
                }
                KeyCode::Char('l') | KeyCode::Enter => {
                    app.ledger_focus = LedgerFocus::Transactions;
                    if app.ledger_filter.is_none() {
                        app.load_ledger_txns(pool).await;
                    }
                }
                KeyCode::Char('h') => {
                    app.ledger_focus = LedgerFocus::Accounts;
                }
                KeyCode::Char('/') => {
                    app.ledger_input = Some(String::new());
                }
                _ => {}
            },
            Screen::Statements => match key.code {
                KeyCode::Char('[') => {
                    app.statements_shift_end_period(-1);
                    app.reload_statements(pool).await;
                }
                KeyCode::Char(']') => {
                    app.statements_shift_end_period(1);
                    app.reload_statements(pool).await;
                }
                _ => {}
            },
            Screen::NoiBridge => match key.code {
                KeyCode::Char('<') | KeyCode::Char('H') => {
                    app.bridge_cycle_property(-1);
                    app.bridge_period = None; // re-resolve to the new property's latest
                    app.reload_noi_bridge(pool).await;
                }
                KeyCode::Char('>') | KeyCode::Char('L') => {
                    app.bridge_cycle_property(1);
                    app.bridge_period = None;
                    app.reload_noi_bridge(pool).await;
                }
                KeyCode::Char('[') => {
                    app.bridge_shift_period(-1);
                    app.reload_noi_bridge(pool).await;
                }
                KeyCode::Char(']') => {
                    app.bridge_shift_period(1);
                    app.reload_noi_bridge(pool).await;
                }
                _ => {}
            },
            Screen::Vendors => match key.code {
                KeyCode::Char('[') => {
                    app.vendors_cycle_window_back();
                    app.reload_vendors(pool).await;
                }
                KeyCode::Char(']') => {
                    app.vendors_cycle_window();
                    app.reload_vendors(pool).await;
                }
                KeyCode::Char('/') => {
                    app.vendors_input = Some(String::new());
                }
                KeyCode::Enter => {
                    // Move selection down (same as j) — or reload txns if already there.
                    if !app.vendors.is_empty() {
                        app.vendor_selected = (app.vendor_selected + 1).min(app.vendors.len() - 1);
                        app.load_vendor_txns(pool).await;
                    }
                }
                _ => {
                    // j/k/g/G are handled by select_next etc. above; after each
                    // navigation on Vendors we also refresh the txn detail pane.
                    if matches!(
                        key.code,
                        KeyCode::Char('j')
                            | KeyCode::Char('k')
                            | KeyCode::Char('g')
                            | KeyCode::Char('G')
                            | KeyCode::Down
                            | KeyCode::Up
                            | KeyCode::Home
                            | KeyCode::End
                    ) {
                        app.load_vendor_txns(pool).await;
                    }
                }
            },
            Screen::Delinquency => match key.code {
                KeyCode::Char('s') => {
                    app.delin_sort = app.delin_sort.next();
                    app.sort_delin_residents();
                    app.toast = Some(format!("sort: {}", app.delin_sort.label()));
                }
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload_delinquency(pool).await;
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload_delinquency(pool).await;
                }
                KeyCode::Char('<') | KeyCode::Char('H') => {
                    app.delin_cycle_property(-1);
                    app.reload_delinquency(pool).await;
                }
                KeyCode::Char('>') | KeyCode::Char('L') => {
                    app.delin_cycle_property(1);
                    app.reload_delinquency(pool).await;
                }
                _ => {}
            },
            Screen::Renewals => match key.code {
                KeyCode::Char('f') => {
                    app.renew_cycle_window_filter();
                    app.reload_renewals(pool).await;
                    app.toast = Some(format!("window: {}", app.renew_window_label()));
                }
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload_renewals(pool).await;
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload_renewals(pool).await;
                }
                KeyCode::Char('<') | KeyCode::Char('H') => {
                    app.renew_cycle_property(-1);
                    app.reload_renewals(pool).await;
                }
                KeyCode::Char('>') | KeyCode::Char('L') => {
                    app.renew_cycle_property(1);
                    app.reload_renewals(pool).await;
                }
                _ => {}
            },
            // Track Record is display-only; no screen-specific key actions.
            Screen::TrackRecord => {}
            Screen::Reports => match key.code {
                KeyCode::PageDown => {
                    let max_scroll = app.report_line_count.saturating_sub(1) as u16;
                    app.report_scroll = app.report_scroll.saturating_add(10).min(max_scroll);
                }
                KeyCode::PageUp => {
                    app.report_scroll = app.report_scroll.saturating_sub(10);
                }
                KeyCode::Char('[') => {
                    app.shift_period(-1);
                    app.reload_reports();
                }
                KeyCode::Char(']') => {
                    app.shift_period(1);
                    app.reload_reports();
                }
                _ => {}
            },
            // Ask results screen key handling is done in the dedicated block above.
            Screen::Ask => {}
        }
    }
}

/// Reload data for whichever screen is currently active.
/// Called after global section-nav (Tab / number keys) to populate the new screen.
async fn reload_active_screen(app: &mut DeskApp, pool: &SqlitePool) {
    match app.screen {
        Screen::Ledger => {
            app.reload_ledger_accounts(pool).await;
            app.load_ledger_txns(pool).await;
        }
        Screen::Statements => app.reload_statements(pool).await,
        Screen::NoiBridge => {
            // Re-resolve the period for whatever property is now in scope.
            app.bridge_period = None;
            app.reload_noi_bridge(pool).await;
        }
        Screen::Vendors => app.reload_vendors(pool).await,
        Screen::Delinquency => app.reload_delinquency(pool).await,
        Screen::Renewals => app.reload_renewals(pool).await,
        Screen::TrackRecord => app.reload_track_record(pool).await,
        Screen::Reports => app.reload_reports(),
        Screen::CloseDesk | Screen::Mappings | Screen::Ask => {}
    }
}

/// Execute `run_ask` in the TUI context.
///
/// 1. Draws an "asking the model…" footer frame so the operator sees progress.
/// 2. Resolves a provider lazily (API key, or Claude Code subscription); on
///    failure sets a footer error and returns without changing the screen.
/// 3. Consumes the rendered sections from `AskResult` into `Screen::Ask`.
async fn run_ask_in_tui(
    terminal: &mut DefaultTerminal,
    app: &mut DeskApp,
    pool: &SqlitePool,
    question: String,
) {
    // Draw the progress frame BEFORE the await so the operator sees feedback.
    app.toast = Some("asking the model\u{2026}".to_string());
    let _ = terminal.draw(|frame| ui::draw(frame, app));
    app.toast = None;

    // Lazy provider — desk startup must succeed with neither key nor CLI.
    let provider = match model_provider::provider_from_env() {
        Ok(p) => p,
        Err(_) => {
            app.load_error = Some("ask needs ANTHROPIC_API_KEY or Claude Code on PATH".to_string());
            return;
        }
    };

    // run_ask returns its rendered sections; nothing touches stdout, so the
    // alt-screen stays clean and there is no fd juggling to deadlock on.
    match ask::run_ask(pool, provider.as_ref(), &question, false).await {
        Ok(result) => {
            app.previous_screen = app.screen;
            app.ask_question = question;
            app.ask_output = result
                .rendered
                .iter()
                .flat_map(|section| section.lines().map(str::to_string))
                .collect();
            app.ask_scroll = 0;
            app.screen = Screen::Ask;
        }
        Err(e) => {
            app.load_error = Some(format!("ask failed: {e}"));
        }
    }
}
