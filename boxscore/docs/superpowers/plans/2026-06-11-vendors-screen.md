# Vendors Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `5 Vendors` in the desk TUI — ranked vendor spend with per-property shares and recent transactions for the selected vendor, filterable by payee and time window.

**Architecture:** One new screen following the established `Screen` enum / capture-pattern / pure-`ui.rs` conventions. Data comes from the existing `db::vendor_spend` (already returns `by_property` shares, residents excluded) plus one new exact-payee transaction helper.

**Conventions (non-negotiable, established):** fmt/test/clippy clean before commit; commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`; PII rule — residents never appear in vendor views (`is_resident = 0` everywhere); rendering pure over `&DeskApp` (TestBackend-testable); input-capture precedence picker > ledger filter > ask input > vendors filter.

---

## Task V1: `db::payee_transactions`

**Files:** Modify `boxscore/src/db.rs`; test in `boxscore/tests/ask_engine.rs` (the seeded-fixture helpers live there).

```rust
/// Recent transactions for one exact payee (vendor detail drill).
pub async fn payee_transactions(
    pool: &SqlitePool,
    payee: &str,
    property_id: Option<&str>,
    limit: i64,
) -> Result<Vec<GlTransaction>>;
// WHERE is_resident = 0 AND payee = ? [AND property_id = ?]
// ORDER BY txn_date DESC, source_row DESC LIMIT ? (cap 200)
```

- [ ] Test: seeded db returns the vendor's transactions newest-first, excludes residents, respects the property filter. Commit with V2 (one commit for the feature is fine).

## Task V2: `Screen::Vendors`

**Files:** Modify `boxscore/src/tui/{app,mod,ui}.rs`; tests in `boxscore/tests/desk_tui.rs`.

**State on `DeskApp`:**
```rust
pub vendors: Vec<db::VendorSpend>,
pub vendor_selected: usize,
pub vendor_txns: Vec<GlTransaction>,        // detail for the selected vendor
pub vendors_window_months: Option<i64>,     // Some(3|6|12) or None = all time
pub vendors_property: Option<usize>,        // None = all properties, Some(i) into properties
pub vendors_filter: Option<String>,         // applied payee filter
pub vendors_input: Option<String>,          // `/` typing mode
```

**Behavior:**
- Key `5` enters Vendors (and Tab cycle becomes CloseDesk→Mappings→Ledger→Statements→Vendors→CloseDesk); on entry call `reload_vendors(pool)`.
- `reload_vendors`: compute `since_period` from the window (latest GL period minus months-1, using the year*12 arithmetic from `shift_period`; None window → None since) and call `db::vendor_spend(pool, vendors_property_id, since, vendors_filter.as_deref(), 200)`; then `load_vendor_txns` for the selected vendor (`payee_transactions(..., 15)` — show 15).
- `j/k/g/G` move vendor selection (clamped, empty-safe) and refresh the detail pane; `p` cycles property scope All→each→All; `[`/`]` cycle the window presets All↔T12↔T6↔T3 (pick one direction each); `/` opens typing mode (same capture pattern as ledger, checked AFTER the ask input capture); Enter applies the filter, Esc cancels; Esc with an applied filter clears it before quitting semantics apply (mirror ledger's Esc-steps-back spirit: Esc clears filter if set, else quits).
- Layout: left 45% ranked vendor table (Payee, Txns, Total right-aligned with `format_money`-style); right 55% split vertically: top = per-property shares table for the selected vendor, bottom = recent transactions (date, account_code, amount, remarks truncated).
- Header line 2: ` {scope} · {window label} · {n} vendors` (+filter when set); footer hints; window labels: `all time`, `T12`, `T6`, `T3`.

- [ ] Render tests: tab shows `5 Vendors`; vendor table renders payee+total; shares render property names; filter typing shows `/{text}▌`; empty state ("No vendor activity. Run: boxscore ingest-standardized transactions --all").
- [ ] App test (in-memory db, reuse ask_engine-style seeding inline in desk_tui.rs): reload_vendors populates ranked vendors, selecting loads txns, property scope filters, resident payee absent.
- [ ] Gates: `cargo fmt`, full `cargo test` (112 currently pass), `cargo clippy --all-targets` zero warnings.
- [ ] Commit `feat(boxscore): vendors screen` and run the live pty smoke: `{ sleep 3; printf '5'; sleep 1; printf 'j'; sleep 1; printf 'q'; } | script -q /tmp/vendors_smoke.out sh -c 'stty rows 34 cols 140 2>/dev/null; ./target/debug/boxscore desk --period 2026-04'` — ANSI-strip and verify a real payee renders.

## Self-review notes
- Mutations: none — the screen is read-only.
- The ask layer and this screen now share `vendor_spend` and `payee_transactions`: one registry, two front doors.
