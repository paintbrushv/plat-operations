//! Natural-language query layer for `boxscore ask`.
//!
//! Default mode (one API call): model → tool calls → local execution → stdout.
//! Narrate mode (≤4 rounds): tool results fed back to model with PII redaction.

use anyhow::Result;
use chrono::{Local, NaiveDate};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tracing::warn;

use crate::{
    close_readiness,
    db::{self, VendorSpend},
    model_provider::{ChatMessage, CompletionResponse, ContentBlock, ToolSpec, ToolUseProvider},
    ontology, recall, t12,
    tools::log_tool_run,
};

// ── Tool registry ─────────────────────────────────────────────────────────────

/// Build the twelve tool specs the model can call.
pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_properties".to_string(),
            description: "List all properties in the portfolio with their unit counts, entity codes, and GL period range. Call this first when the question is about the portfolio, property names, or you are unsure which property the user means.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "account_activity".to_string(),
            description: "Return per-account AGGREGATE transaction totals (one row per account_code, summed) for a property and period (YYYY-MM). Call this for total GL activity by account, account balances, or expense-category breakdowns for a specific month. Returns aggregates per account — NOT individual transaction rows; do NOT use this to find a single specific or largest transaction (use search_transactions for that).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name (matched case-insensitively)."
                    },
                    "period": {
                        "type": "string",
                        "description": "Period as YYYY-MM, e.g. '2026-03'."
                    }
                },
                "required": ["property", "period"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "search_transactions".to_string(),
            description: "Search INDIVIDUAL transaction rows in the ledger (full-text across payee, remarks, and account_code). Returns individual line items. Call this to find a single specific or LARGEST/smallest transaction, specific line items, transactions by payee/keyword, or to look up individual charges. Use this — NOT account_activity — whenever the question is about one or a few actual transactions rather than per-account totals. To find the LARGEST transaction(s) for a property/period, set order=\"amount\" (rows come back largest-first by absolute amount); pass a period (YYYY-MM) to scope to one month. 'query' is OPTIONAL — omit it to consider ALL transactions for the property/period (e.g. for the single largest transaction).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional search needle (matched case-insensitively against payee, remarks, account_code). Omit to consider all transactions for the property/period."
                    },
                    "property": {
                        "type": "string",
                        "description": "Optional property name to scope the search."
                    },
                    "period": {
                        "type": "string",
                        "description": "Optional period as YYYY-MM to scope the search to one month."
                    },
                    "order": {
                        "type": "string",
                        "enum": ["amount", "recent"],
                        "description": "Result ordering. \"amount\" returns largest-first by absolute amount (use to find the largest transaction). \"recent\" (default) returns newest-first by date."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows to return (default 50, cap 200).",
                        "minimum": 1,
                        "maximum": 200
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "t12_statement".to_string(),
            description: "Return a trailing-twelve-month (T12) income statement for a property. Call this for questions about annual NOI, revenue trends, or 12-month income statements.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name (matched case-insensitively)."
                    },
                    "end_period": {
                        "type": "string",
                        "description": "Optional end period as YYYY-MM (defaults to the latest period with actuals)."
                    }
                },
                "required": ["property"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "vendor_spend".to_string(),
            description: "Aggregate payments to vendors/payees grouped by payee, ordered by total spend, with a per-property breakdown for each vendor. Call this for any question about payments to a vendor/payee, vendor comparisons, contractor costs, 'what did we pay X', or which properties a vendor was paid at.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Optional property name to scope results."
                    },
                    "since_period": {
                        "type": "string",
                        "description": "Optional start period as YYYY-MM; only transactions at or after this period are included."
                    },
                    "payee_contains": {
                        "type": "string",
                        "description": "Optional substring filter on payee name."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows to return (default 50, cap 200).",
                        "minimum": 1,
                        "maximum": 200
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "close_readiness".to_string(),
            description: "Assess portfolio close readiness for a period. Call this for questions about whether properties are ready to close, missing feeds, blockers, or close status.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "period": {
                        "type": "string",
                        "description": "Period as YYYY-MM, e.g. '2026-05'."
                    }
                },
                "required": ["period"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "delinquency_summary".to_string(),
            description: "Report current delinquency / accounts-receivable exposure for ONE property as a dated snapshot: total delinquent dollars, delinquent units, prepaid, and high-risk units. Call this for questions about delinquency, past-due/AR balances, or how much/how many residents are behind. The result carries an as-of date. If no receivables feed exists it says so (never reports $0); and a reported $0 is flagged as UNVERIFIED (a count-only feed can load as $0), not asserted as a confirmed zero. Property-level aggregate; read-only decision-support.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Willow Brook'."
                    }
                },
                "required": ["property"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "occupancy".to_string(),
            description: "Report current physical occupancy for ONE property as a dated snapshot: occupied/vacant/down units and occupancy % (down units are in the denominator, matching the variance engine). Call this for questions about occupancy, vacancy, or leased units. The result carries an as-of date and a staleness caution if the snapshot is old. If no rent-roll feed exists it says so (never a fabricated 100%/0%). Property-level aggregate; read-only decision-support.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Willow Brook'."
                    }
                },
                "required": ["property"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "budget_variance".to_string(),
            description: "Report budget-vs-actual variance by account for ONE property and a single period (YYYY-MM): budget, actual, dollar variance, % variance, and an over/under-budget flag with the correct favorability sign (over budget is unfavorable for expenses; under-collected is unfavorable for income). The output ALWAYS names the period. Call this for budget questions, 'are we over/under budget', or which categories missed plan. If no budget feed exists for that period it says so explicitly and never reports 'on budget' by assumption.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Willow Brook'."
                    },
                    "period": {
                        "type": "string",
                        "description": "Period as YYYY-MM, e.g. '2026-05'."
                    }
                },
                "required": ["property", "period"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "turnover".to_string(),
            description: "Report turn-cost / turnover metrics for ONE property from its turn_costs feed: number of turns, total make-ready cost, average make-ready cost per turn, and average vacancy days. Optionally restrict to turns on/after a since_period (YYYY-MM). Call this for questions about turn cost, turnover, make-ready spend, or vacancy days. To answer 'which property has the highest turn cost (per turn)', call this once PER property and compare the avg_cost_per_turn figures. If no turn feed exists for the property it says so explicitly (never a fabricated $0 / 0 turns); a window that excludes every turn reports 0 turns with avg cost as n/a.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Willow Brook'."
                    },
                    "since_period": {
                        "type": "string",
                        "description": "Optional start period as YYYY-MM; only turns with a turn_date at or after this month are included."
                    }
                },
                "required": ["property"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "unit_pnl".to_string(),
            description: "Look up ONE unit's per-unit P&L for a property from the unit_pnl feed: total income, direct + allocated expense, and NOI for a single unit. Call this for questions about a specific unit's NOI, income, or expenses (e.g. 'annual NOI for unit C00123'). By default it returns the LATEST period on record; pass period (a year like '2024') to pin a specific year. The unit code is matched robustly — 'C123', 'C0123', and 'C00123' all resolve to the same unit. If the unit is not found at the property it says so explicitly (never a fabricated $0 NOI). This is UNIT-LEVEL financials only — it never returns any resident name, code, or balance.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Maplewood Commons'."
                    },
                    "unit": {
                        "type": "string",
                        "description": "Unit code, e.g. 'C00123'. Matched robustly across zero-padding (C123 = C00123)."
                    },
                    "period": {
                        "type": "string",
                        "description": "Optional period (a year like '2024'); omit to get the latest period on record."
                    }
                },
                "required": ["property", "unit"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "recall".to_string(),
            description: "Report the HARNESS'S OWN calibrated track record for a property — how reliable its past predictions/calls of a given type have been (a shrunk, time-decayed hit-rate with a 90% interval), plus the most similar past decisions/calls (age-stamped, with their outcomes). Call this for 'how reliable / what's our track record / how confident should I be in the harness's NOI/reversion/delinquency calls for property X'. Abstains when there's too little history (n_eff<5) rather than guessing. NOT property financial data — use the other tools for that.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "property": {
                        "type": "string",
                        "description": "Property name, e.g. 'Willow Brook'."
                    },
                    "call_type": {
                        "type": "string",
                        "enum": ["noi_diagnosis", "t12_reversion", "delinquency_risk", "renewal_rec", "decision"],
                        "description": "Which kind of call to score the track record over (default: decision)."
                    },
                    "decision_kind": {
                        "type": "string",
                        "description": "Optional decision kind to weight similarity toward, e.g. renewal_override, concession, capex."
                    }
                },
                "required": ["property"],
                "additionalProperties": false
            }),
        },
    ]
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Assemble the system prompt from live db state.
pub async fn build_system_prompt(pool: &SqlitePool) -> Result<String> {
    let properties = db::list_properties(pool).await?;
    let today = Local::now().format("%Y-%m-%d").to_string();
    let current_period = Local::now().format("%Y-%m").to_string();

    let mut catalog = String::new();
    for prop in &properties {
        // Find GL period range from gl_transactions
        let range: Option<(String, String)> = sqlx::query_as(
            "SELECT MIN(period) AS first_period, MAX(period) AS last_period
             FROM gl_transactions WHERE property_id = ?",
        )
        .bind(&prop.id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        let period_range = range
            .map(|(f, l)| format!("{f} – {l}"))
            .unwrap_or_else(|| "no GL data".to_string());
        catalog.push_str(&format!(
            "  - {}: {} units, entity={}, GL={}\n",
            prop.name, prop.unit_count, prop.owner_entity, period_range
        ));
    }

    if catalog.is_empty() {
        catalog = "  (no properties loaded yet)\n".to_string();
    }

    // K5: select by relevance (value-class call types) + reliability (highest scored-N first),
    // 3 entries per type.  Storage convention: scope=call_type, key=property_id; passing ""
    // as property_id returns portfolio-wide records (matching the previous behaviour of the
    // legacy recent_track_record_memories call while adding reliability ordering).
    const RELEVANT_TYPES: &[&str] = &["noi_diagnosis", "t12_reversion", "delinquency_risk"];
    let track = db::select_track_record_for_context(pool, "", RELEVANT_TYPES, 3).await?;
    let track_record_block = if !track.is_empty() {
        let mut block =
            "\n\nHarness track record (your past calls, scored against actuals):\n".to_string();
        for (headline, _) in &track {
            block.push_str(&format!("- {}\n", headline));
        }
        block.push_str(
            "Weight your confidence by this record; if a call type has a poor batting average, hedge accordingly.\n",
        );
        block
    } else {
        String::new()
    };

    Ok(format!(
        "You are Boxscore, a local-first multifamily NOI variance intelligence harness. \
You help operators answer questions about their property GL data using tool calls. \
You have access to tools that query a local SQLite database — no data leaves the machine.\n\
\n\
Portfolio (as of {today}, current period {current_period}):\n\
{catalog}\n\
Rules:\n\
- Answer ONLY by calling tools. Never invent numbers.\n\
- Prefer one tool call; use several only when the question genuinely spans multiple tools.\n\
- If a property name is ambiguous, call list_properties first, then pick the best match.\n\
- Today's date is {today}. Current period is {current_period}.\
{track_record_block}"
    ))
}

// ── Table rendering ───────────────────────────────────────────────────────────

fn render_vendor_spend_table(rows: &[VendorSpend]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:>10} {:>14} {:>8} {:>8}\n",
        "Payee", "Txns", "Total", "First", "Last"
    ));
    out.push_str(&format!(
        "{:-<40} {:->10} {:->14} {:->8} {:->8}\n",
        "", "", "", "", ""
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<40} {:>10} {:>14} {:>8} {:>8}\n",
            truncate(&row.payee, 40),
            row.txn_count,
            format_money(row.total),
            row.first_period,
            row.last_period,
        ));
        // Per-property breakdown when the vendor spans more than one.
        if row.by_property.len() > 1 {
            for share in &row.by_property {
                out.push_str(&format!(
                    "  └ {:<37} {:>10} {:>14}\n",
                    truncate(&share.property, 37),
                    share.txn_count,
                    format_money(share.total),
                ));
            }
        }
    }
    out.push_str(&format!("({} rows)\n", rows.len()));
    out
}

/// Staleness caution for a dated snapshot (rent-roll, receivables, …). Returns `Some(note)` when the
/// snapshot's as-of date is more than `stale_days` old relative to `today`, so a months-old snapshot is
/// never reported as "current" without a flag. Returns `None` on a fresh snapshot or an unparseable date
/// (never fabricate a caution we can't ground). Pure (today injected) so it's deterministically tested.
fn freshness_note(as_of_date: &str, today: NaiveDate, stale_days: i64) -> Option<String> {
    let as_of =
        NaiveDate::parse_from_str(as_of_date.get(..10).unwrap_or(as_of_date), "%Y-%m-%d").ok()?;
    let age = (today - as_of).num_days();
    if age > stale_days {
        Some(format!(
            "⚠ This snapshot is {age} days old (as of {as_of_date}) — it may not reflect the current state.\n"
        ))
    } else {
        None
    }
}

fn render_delinquency_table(property: &str, s: &db::DelinquencySummary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Delinquency — {property} (as of {})\n",
        s.as_of_date
    ));
    out.push_str(&format!("{:-<52}\n", ""));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Total delinquent",
        format_money(s.delinquent_amount)
    ));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Delinquent units", s.delinquent_units
    ));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Prepaid",
        format_money(s.prepaid_amount)
    ));
    if let Some(hr) = s.high_risk_units {
        // Disclose the high-risk count's OWN as-of date — it can differ from the
        // delinquency figures' date; a stale count must never read as same-dated.
        let label = match &s.high_risk_as_of {
            Some(d) if d != &s.as_of_date => format!("High-risk units (as of {d})"),
            _ => "High-risk units".to_string(),
        };
        out.push_str(&format!("{label:<26} {hr:>24}\n"));
    }
    // C3 (degraded-feed) guard: a PRESENT $0/0-units row is not a verified zero —
    // a count-only AR feed loads as $0 (validator D3) and this table carries no
    // aging-bucket provenance. Flag it as unverified rather than asserting "$0".
    if s.delinquent_amount == 0.0 && s.delinquent_units == 0 {
        out.push_str(
            "⚠ Reported $0 / 0 units is UNVERIFIED — a count-only AR feed reports $0. \
             Confirm the source includes 31+ aging detail before treating this as zero delinquency.\n",
        );
    }
    out
}

/// Canonical physical-occupancy percentage rounded to 1 decimal, as a fraction-free
/// percent. Uses `ontology::occupancy_rate` (down_units IN the denominator) so the ask
/// agent reports the SAME figure as the variance engine. `None` (no occupiable base) =>
/// the divide-by-zero guard, surfaced as "n/a" — never a fabricated 100%/0%.
fn occupancy_pct_rounded(s: &db::OccupancySnapshot) -> Option<f64> {
    ontology::occupancy_rate(s.occupied_units, s.vacant_units, s.down_units)
        .map(|rate| (rate * 100.0 * 10.0).round() / 10.0)
}

fn render_occupancy_table(property: &str, s: &db::OccupancySnapshot) -> String {
    // Occupiable base = occupied + vacant + down (canonical denominator). NOT the
    // property's unit_count — model/excluded units aren't here — so don't call it "total".
    let occupiable = s.occupied_units + s.vacant_units + s.down_units;
    let pct = match occupancy_pct_rounded(s) {
        Some(p) => format!("{p:.1}%"),
        None => "n/a".to_string(),
    };
    let mut out = String::new();
    // ALWAYS surface the as-of date — a snapshot read without its date is ungrounded.
    out.push_str(&format!(
        "Occupancy — {property} (as of {})\n",
        s.as_of_date
    ));
    out.push_str(&format!("{:-<52}\n", ""));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Occupied units", s.occupied_units
    ));
    out.push_str(&format!("{:<26} {:>24}\n", "Vacant units", s.vacant_units));
    out.push_str(&format!("{:<26} {:>24}\n", "Leased units", s.leased_units));
    out.push_str(&format!("{:<26} {:>24}\n", "Notice units", s.notice_units));
    out.push_str(&format!("{:<26} {:>24}\n", "Down units", s.down_units));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Occupied + vacant + down", occupiable
    ));
    out.push_str(&format!("{:<26} {pct:>24}\n", "Occupancy %"));
    out
}

fn render_budget_variance_table(
    property: &str,
    period: &str,
    rows: &[db::BudgetVarianceRow],
) -> String {
    let mut out = String::new();
    // ALWAYS surface the period — a variance read without its period is ungrounded.
    out.push_str(&format!("Budget variance — {property} ({period})\n"));
    out.push_str(&format!(
        "{:<12} {:<28} {:>14} {:>14} {:>14} {:>9} {}\n",
        "Code", "Account", "Budget", "Actual", "Variance", "Var %", "Flag"
    ));
    out.push_str(&format!(
        "{:-<12} {:-<28} {:->14} {:->14} {:->14} {:->9} {:-<12}\n",
        "", "", "", "", "", "", ""
    ));
    for r in rows {
        // Divide-by-zero guard: budget == 0 → "n/a", never a fabricated 0%/∞.
        let pct = match r.variance_pct {
            Some(p) => format!("{p:.1}%"),
            None => "n/a".to_string(),
        };
        // Only label over/under when the income/expense class is known; an
        // unclassifiable (Unmapped) account reports raw figures with no flag.
        let flag = if !r.class_known {
            "(unclassified)".to_string()
        } else if r.is_unfavorable {
            "UNFAVORABLE".to_string()
        } else {
            "favorable".to_string()
        };
        out.push_str(&format!(
            "{:<12} {:<28} {:>14} {:>14} {:>14} {:>9} {}\n",
            truncate(&r.account_code, 12),
            truncate(&r.account_name, 28),
            format_money(r.budget),
            format_money(r.actual),
            format_money(r.variance),
            pct,
            flag
        ));
    }
    out
}

fn render_turnover_table(property: &str, since: Option<&str>, s: &db::TurnSummary) -> String {
    let mut out = String::new();
    // Name the window so a turn figure is never read without its time scope.
    let window = match (since, &s.earliest_turn_date, &s.latest_turn_date) {
        (Some(p), _, Some(latest)) => format!("turns {p} → {latest}"),
        (Some(p), _, None) => format!("turns from {p}"),
        (None, Some(earliest), Some(latest)) => format!("turns {earliest} → {latest}"),
        _ => "all turns".to_string(),
    };
    out.push_str(&format!("Turnover — {property} ({window})\n"));
    out.push_str(&format!("{:-<52}\n", ""));
    out.push_str(&format!("{:<26} {:>24}\n", "Turns", s.turn_count));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Total turn cost",
        format_money(s.total_turn_cost)
    ));
    // Divide-by-zero guard: avg is None when the window holds no turns → "n/a".
    let avg = match s.avg_cost_per_turn {
        Some(v) => format_money(v),
        None => "n/a".to_string(),
    };
    out.push_str(&format!("{:<26} {avg:>24}\n", "Avg cost per turn"));
    let avg_vac = match s.avg_vacancy_days {
        Some(v) => format!("{v:.1}"),
        None => "n/a".to_string(),
    };
    out.push_str(&format!("{:<26} {avg_vac:>24}\n", "Avg vacancy days"));
    if s.turn_count == 0 {
        out.push_str(
            "Note: the property HAS turn data, but no turns fall in the requested window.\n",
        );
    }
    out
}

fn render_unit_pnl_table(property: &str, p: &db::UnitPnl) -> String {
    let mut out = String::new();
    let period = p.period.as_deref().unwrap_or("n/a");
    // UNIT-LEVEL ONLY: unit + period + economics; never any resident field.
    out.push_str(&format!(
        "Unit P&L — {} · unit {} ({})\n",
        property, p.unit, period
    ));
    out.push_str(&format!("{:-<52}\n", ""));
    let money = |v: Option<f64>| match v {
        Some(x) => format_money(x),
        None => "n/a".to_string(),
    };
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Total income",
        money(p.total_income)
    ));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Direct expense",
        money(p.direct_expense)
    ));
    out.push_str(&format!(
        "{:<26} {:>24}\n",
        "Allocated expense",
        money(p.allocated_expense)
    ));
    out.push_str(&format!("{:<26} {:>24}\n", "NOI", money(p.noi)));
    out
}

/// Render the harness's OWN calibrated track record for a property (Flywheel B recall).
/// META, not property financials: a shrunk, time-decayed hit-rate with a 90% interval, then
/// the most similar past calls (age-stamped, with their scored/pending outcome). When the stat
/// abstains (n_eff < ABSTAIN_N_EFF) we DO NOT print a hit-rate — an explicit ABSTAIN line goes in
/// its place so a thin record can't be read as a confident number. Only recall's Neighbor fields
/// are rendered (summary = outcome_summary or status) — never operator_outcome free-text.
fn render_recall(
    property: &str,
    call_type: &str,
    today_period: &str,
    r: &recall::Recall,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Harness track record — {property} · {call_type} calls (as of {today_period})\n"
    ));
    out.push_str(&format!("{:-<60}\n", ""));
    let pct = |v: f64| format!("{:.1}%", v * 100.0);
    if r.stat.abstain {
        // Too little history — never present the shrunk-to-prior mean as a reliable rate.
        out.push_str(&format!(
            "⚠ ABSTAIN: too little history (n_eff={:.1} < {}) — abstaining; showing neighbors only.\n",
            r.stat.n_eff,
            crate::calibration::ABSTAIN_N_EFF as i64
        ));
    } else {
        out.push_str(&format!(
            "{:<28} {:>20}\n",
            "Calibrated hit-rate",
            pct(r.stat.posterior_mean)
        ));
        out.push_str(&format!(
            "{:<28} {:>20}\n",
            "90% interval",
            format!("[{}, {}]", pct(r.stat.lo90), pct(r.stat.hi90))
        ));
        out.push_str(&format!(
            "{:<28} {:>20}\n",
            "Effective sample (n_eff)",
            format!("{:.1}", r.stat.n_eff)
        ));
    }
    // Neighbors are the visible denominator (K4): age-stamped, open/pending ones included.
    if r.neighbors.is_empty() {
        out.push_str("\nNo comparable past calls on record.\n");
    } else {
        out.push_str(&format!(
            "\nMost similar past calls ({}):\n",
            r.neighbors.len()
        ));
        out.push_str(&format!(
            "{:<34} {:>7} {:>9} {:>6} {:>5}  {}\n",
            "Outcome", "Score", "Status", "AgeMo", "Sim", "id"
        ));
        out.push_str(&format!(
            "{:-<34} {:->7} {:->9} {:->6} {:->5}  {:-<10}\n",
            "", "", "", "", "", ""
        ));
        for n in &r.neighbors {
            // pending calls carry no score yet — show "pending", never a fabricated 0.
            let score = match n.score {
                Some(s) => format!("{s:.2}"),
                None => "pending".to_string(),
            };
            out.push_str(&format!(
                "{:<34} {:>7} {:>9} {:>6.0} {:>5.2}  {}\n",
                truncate(&n.summary, 34),
                score,
                truncate(&n.status, 9),
                n.age_months,
                n.similarity,
                n.id,
            ));
        }
    }
    out
}

fn render_account_activity_table(rows: &[db::AccountActivity]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<12} {:<40} {:>10} {:>14}\n",
        "Code", "Account", "Txns", "Total"
    ));
    out.push_str(&format!(
        "{:-<12} {:-<40} {:->10} {:->14}\n",
        "", "", "", ""
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<12} {:<40} {:>10} {:>14}\n",
            truncate(&row.account_code, 12),
            truncate(&row.account_name, 40),
            row.txn_count,
            format_money(row.total),
        ));
    }
    out.push_str(&format!("({} rows)\n", rows.len()));
    out
}

fn render_transactions_table(rows: &[crate::models::GlTransaction]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<12} {:<12} {:<30} {:>14} {:<20}\n",
        "Date", "Period", "Payee", "Amount", "Remarks"
    ));
    out.push_str(&format!(
        "{:-<12} {:-<12} {:-<30} {:->14} {:-<20}\n",
        "", "", "", "", ""
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<12} {:<12} {:<30} {:>14} {:<20}\n",
            row.txn_date.as_deref().unwrap_or(""),
            row.period,
            truncate(&row.payee, 30),
            format_money(row.amount),
            truncate(row.remarks.as_deref().unwrap_or(""), 20),
        ));
    }
    out.push_str(&format!("({} rows)\n", rows.len()));
    out
}

fn render_t12_table(stmt: &t12::T12Statement) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "T12 Statement — {} (ending {})\n",
        stmt.property, stmt.end_period
    ));
    // Header row
    out.push_str(&format!("{:<28}", "Category"));
    for p in &stmt.periods {
        out.push_str(&format!(" {:>10}", p));
    }
    out.push('\n');
    // Separator
    out.push_str(&format!("{:-<28}", ""));
    for _ in &stmt.periods {
        out.push_str(&format!(" {:->10}", ""));
    }
    out.push('\n');
    // Rows
    for row in &stmt.rows {
        out.push_str(&format!("{:<28}", truncate(&row.label, 28)));
        for v in &row.values {
            out.push_str(&format!(" {:>10}", t12::compact_dollars(*v)));
        }
        out.push('\n');
    }
    out.push_str(&format!("({} rows)\n", stmt.rows.len()));
    out
}

fn render_close_readiness_table(
    summary: &close_readiness::CloseReadinessSummary,
    properties: &[close_readiness::PropertyCloseReadiness],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Close Readiness: {}/{} owner-ready, {} blockers, {} warnings\n\n",
        summary.ready_count, summary.property_count, summary.blocker_count, summary.warning_count,
    ));
    out.push_str(&format!(
        "{:<30} {:<12} {:>8} {:>8}\n",
        "Property", "Status", "Blockers", "Warnings"
    ));
    out.push_str(&format!("{:-<30} {:-<12} {:->8} {:->8}\n", "", "", "", ""));
    for prop in properties {
        let status = match prop.status {
            close_readiness::CloseReadinessStatus::Ready => "ready",
            close_readiness::CloseReadinessStatus::NotReady => "not ready",
        };
        out.push_str(&format!(
            "{:<30} {:<12} {:>8} {:>8}\n",
            truncate(&prop.property, 30),
            status,
            prop.blockers,
            prop.warning_count,
        ));
    }
    out.push_str(&format!("({} rows)\n", properties.len()));
    out
}

fn render_properties_table(
    properties: &[crate::models::Property],
    period_ranges: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<30} {:>6} {:<20} {:<18}\n",
        "Property", "Units", "Entity", "GL Range"
    ));
    out.push_str(&format!("{:-<30} {:->6} {:-<20} {:-<18}\n", "", "", "", ""));
    for (prop, (first, last)) in properties.iter().zip(period_ranges.iter()) {
        let range = if first.is_empty() {
            "no data".to_string()
        } else {
            format!("{first} – {last}")
        };
        out.push_str(&format!(
            "{:<30} {:>6} {:<20} {:<18}\n",
            truncate(&prop.name, 30),
            prop.unit_count,
            truncate(&prop.owner_entity, 20),
            range,
        ));
    }
    out.push_str(&format!("({} rows)\n", properties.len()));
    out
}

fn format_money(v: f64) -> String {
    let abs = v.abs();
    let sign = if v < 0.0 { "-" } else { "" };
    let int = abs.trunc() as i64;
    let cents = ((abs.fract() * 100.0).round()) as u64;
    // Build with comma separators
    let s = format!("{int}");
    let mut with_commas = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            with_commas.push(',');
        }
        with_commas.push(ch);
    }
    let int_str: String = with_commas.chars().rev().collect();
    format!("{sign}${int_str}.{cents:02}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ── PII redaction ─────────────────────────────────────────────────────────────

/// Walk a JSON value and replace `payee` fields in objects that also carry
/// `is_resident: 1` (or `is_resident: true`) with `"(resident)"`.
pub fn redact_residents(value: &mut Value) {
    match value {
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                redact_residents(v);
            }
        }
        Value::Object(map) => {
            let is_resident = map
                .get("is_resident")
                .map(|v| v.as_i64() == Some(1) || v.as_bool() == Some(true))
                .unwrap_or(false);
            if is_resident {
                if let Some(payee) = map.get_mut("payee") {
                    *payee = Value::String("(resident)".to_string());
                }
            }
            // Recurse into all values
            for v in map.values_mut() {
                redact_residents(v);
            }
        }
        _ => {}
    }
}

// ── Tool executor ─────────────────────────────────────────────────────────────

/// Execute a tool call against the local database.
/// Returns `(json_result, rendered_table)`.
async fn execute_tool(
    pool: &SqlitePool,
    tool_name: &str,
    input: &Value,
) -> Result<(Value, String), String> {
    match tool_name {
        "list_properties" => {
            let properties = db::list_properties(pool).await.map_err(|e| e.to_string())?;
            let mut period_ranges: Vec<(String, String)> = Vec::new();
            for prop in &properties {
                let range: Option<(String, String)> = sqlx::query_as(
                    "SELECT MIN(period) AS first_period, MAX(period) AS last_period
                     FROM gl_transactions WHERE property_id = ?",
                )
                .bind(&prop.id)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
                period_ranges.push(range.unwrap_or_else(|| ("".to_string(), "".to_string())));
            }
            let json_val = serde_json::to_value(&properties).unwrap_or_default();
            let rendered = render_properties_table(&properties, &period_ranges);
            Ok((json_val, rendered))
        }

        "account_activity" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            let period = input["period"].as_str().unwrap_or("").trim();
            if property_name.is_empty() || period.is_empty() {
                return Err("account_activity requires 'property' and 'period'".to_string());
            }
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            let rows = db::account_activity(pool, &property.id, period)
                .await
                .map_err(|e| e.to_string())?;
            let json_val = serde_json::to_value(&rows).unwrap_or_default();
            let rendered = render_account_activity_table(&rows);
            Ok((json_val, rendered))
        }

        "search_transactions" => {
            // 'query' is now optional — omit it to consider all transactions
            // for the property/period (e.g. for the single largest one).
            let query = input["query"].as_str().unwrap_or("").trim();
            let period = input["period"]
                .as_str()
                .map(str::trim)
                .filter(|p| !p.is_empty());
            let order_by_amount = input["order"].as_str() == Some("amount");
            let limit = input["limit"].as_i64().unwrap_or(50).min(200);
            let property_id = if let Some(pname) = input["property"].as_str() {
                let pname = pname.trim();
                if !pname.is_empty() {
                    match db::find_property_by_name(pool, pname).await {
                        Ok(Some(p)) => Some(p.id),
                        Ok(None) => {
                            let names = db::list_properties(pool)
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .map(|p| p.name)
                                .collect::<Vec<_>>()
                                .join(", ");
                            return Err(format!("Property not found: '{pname}'. Valid: {names}"));
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                } else {
                    None
                }
            } else {
                None
            };
            // Grounding guard: an empty query with no property and no period would
            // return an unscoped cross-property slice that could ground a confidently
            // wrong single-property answer. Require at least one scope.
            if query.is_empty() && property_id.is_none() && period.is_none() {
                return Err(
                    "search_transactions with no query needs a scope: pass a property, a period (YYYY-MM), or a search query."
                        .to_string(),
                );
            }
            let rows = db::search_transactions(
                pool,
                property_id.as_deref(),
                query,
                period,
                order_by_amount,
                limit,
            )
            .await
            .map_err(|e| e.to_string())?;
            let json_val = serde_json::to_value(&rows).unwrap_or_default();
            let rendered = render_transactions_table(&rows);
            Ok((json_val, rendered))
        }

        "t12_statement" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("t12_statement requires 'property'".to_string());
            }
            let end_period = input["end_period"].as_str();
            // Validate property exists first
            match db::find_property_by_name(pool, property_name).await {
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
                Ok(Some(_)) => {}
            }
            let stmt = t12::assemble_t12(pool, property_name, end_period)
                .await
                .map_err(|e| e.to_string())?;
            let json_val = serde_json::to_value(&stmt).unwrap_or_default();
            let rendered = render_t12_table(&stmt);
            Ok((json_val, rendered))
        }

        "vendor_spend" => {
            let limit = input["limit"].as_i64().unwrap_or(50).min(200);
            let property_id = if let Some(pname) = input["property"].as_str() {
                let pname = pname.trim();
                if !pname.is_empty() {
                    match db::find_property_by_name(pool, pname).await {
                        Ok(Some(p)) => Some(p.id),
                        Ok(None) => {
                            let names = db::list_properties(pool)
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .map(|p| p.name)
                                .collect::<Vec<_>>()
                                .join(", ");
                            return Err(format!("Property not found: '{pname}'. Valid: {names}"));
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let since_period = input["since_period"].as_str();
            let payee_contains = input["payee_contains"].as_str();
            let rows = db::vendor_spend(
                pool,
                property_id.as_deref(),
                since_period,
                payee_contains,
                limit,
            )
            .await
            .map_err(|e| e.to_string())?;
            let json_val = serde_json::to_value(&rows).unwrap_or_default();
            let rendered = render_vendor_spend_table(&rows);
            Ok((json_val, rendered))
        }

        "close_readiness" => {
            let period = input["period"].as_str().unwrap_or("").trim();
            if period.is_empty() {
                return Err("close_readiness requires 'period'".to_string());
            }
            let (summary, properties) = close_readiness::assess_portfolio(pool, period)
                .await
                .map_err(|e| e.to_string())?;
            let json_val = json!({
                "summary": serde_json::to_value(&summary).unwrap_or_default(),
                "properties": serde_json::to_value(&properties).unwrap_or_default(),
            });
            let rendered = render_close_readiness_table(&summary, &properties);
            Ok((json_val, rendered))
        }

        "delinquency_summary" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("delinquency_summary requires 'property'".to_string());
            }
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            match db::delinquency_summary(pool, &property.id)
                .await
                .map_err(|e| e.to_string())?
            {
                Some(s) => {
                    // A present $0/0-units row is unverified, not a confirmed zero (validator D3:
                    // a count-only feed loads as $0). Flag it so a reader can't treat it as zero.
                    let unverified_zero = s.delinquent_amount == 0.0 && s.delinquent_units == 0;
                    let json_val = json!({
                        "property": property.name,
                        "as_of_date": s.as_of_date,
                        "delinquent_amount": s.delinquent_amount,
                        "delinquent_units": s.delinquent_units,
                        "prepaid_amount": s.prepaid_amount,
                        "high_risk_units": s.high_risk_units,
                        "high_risk_as_of": s.high_risk_as_of,
                        "zero_is_unverified": unverified_zero,
                    });
                    let mut rendered = render_delinquency_table(&property.name, &s);
                    if let Some(note) = freshness_note(&s.as_of_date, Local::now().date_naive(), 45)
                    {
                        rendered.push_str(&note);
                    }
                    Ok((json_val, rendered))
                }
                // C3 / implicit-zero guard: missing receivables feed → say so, never report $0.
                None => {
                    let msg = format!(
                        "No receivables feed for {} — cannot report delinquency. \
                         Do not assume zero; the aged-receivables snapshot is missing for this property.",
                        property.name
                    );
                    let json_val = json!({
                        "property": property.name,
                        "delinquency": Value::Null,
                        "note": msg,
                    });
                    Ok((json_val, msg))
                }
            }
        }

        "occupancy" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("occupancy requires 'property'".to_string());
            }
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            match db::occupancy_summary(pool, &property.id)
                .await
                .map_err(|e| e.to_string())?
            {
                Some(s) => {
                    // Canonical occupancy via ontology::occupancy_rate (down_units IN the
                    // denominator) so this matches variance.rs/db.rs for the same property.
                    // None (no occupiable base) → "n/a"; the Option IS the divide-by-zero guard.
                    let occupancy_pct = match occupancy_pct_rounded(&s) {
                        Some(p) => Value::from(p),
                        None => Value::from("n/a"),
                    };
                    // Occupiable base = occupied + vacant + down — NOT the property's unit_count
                    // (model/excluded units aren't here), so it is not labeled "total units".
                    let occupiable = s.occupied_units + s.vacant_units + s.down_units;
                    let json_val = json!({
                        "property": property.name,
                        "as_of_date": s.as_of_date,
                        "occupancy_pct": occupancy_pct,
                        "occupied_units": s.occupied_units,
                        "vacant_units": s.vacant_units,
                        "occupied_plus_vacant_plus_down": occupiable,
                        "leased_units": s.leased_units,
                        "notice_units": s.notice_units,
                        "down_units": s.down_units,
                    });
                    let mut rendered = render_occupancy_table(&property.name, &s);
                    if let Some(note) = freshness_note(&s.as_of_date, Local::now().date_naive(), 45)
                    {
                        rendered.push_str(&note);
                    }
                    Ok((json_val, rendered))
                }
                // Missing-feed guard: no rent-roll feed → say so, never a fabricated 0%/100%.
                None => {
                    let msg = format!(
                        "No rent-roll feed for {} — cannot report occupancy. \
                         Do not assume any figure; the rent-roll snapshot is missing for this property.",
                        property.name
                    );
                    let json_val = json!({
                        "property": property.name,
                        "occupancy": Value::Null,
                        "note": msg,
                    });
                    Ok((json_val, msg))
                }
            }
        }

        "budget_variance" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("budget_variance requires 'property'".to_string());
            }
            let period = input["period"].as_str().unwrap_or("").trim();
            if period.is_empty() {
                return Err("budget_variance requires 'period' (YYYY-MM)".to_string());
            }
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            let rows = db::budget_variance(pool, &property.id, period)
                .await
                .map_err(|e| e.to_string())?;
            if rows.is_empty() {
                // Missing-feed guard: no budget/actual rows for this property+period →
                // say so explicitly. Never report "$0 variance / on budget".
                let msg = format!(
                    "No budget feed for {} for {period} — cannot report variance. \
                     Do not assume on-budget; the budget vs actual rows are missing for this period.",
                    property.name
                );
                let json_val = json!({
                    "property": property.name,
                    "period": period,
                    "budget_variance": Value::Null,
                    "note": msg,
                });
                return Ok((json_val, msg));
            }
            // Build per-account rows for JSON. `is_unfavorable` is OMITTED when the
            // income/expense class is unknown (Unmapped) so we never assert a
            // favorability we cannot justify — only raw figures are reported there.
            let accounts: Vec<Value> = rows
                .iter()
                .map(|r| {
                    let pct = match r.variance_pct {
                        Some(p) => Value::from((p * 10.0).round() / 10.0),
                        None => Value::from("n/a"),
                    };
                    let mut obj = json!({
                        "account_code": r.account_code,
                        "account_name": r.account_name,
                        "category": r.category,
                        "budget": r.budget,
                        "actual": r.actual,
                        "variance": r.variance,
                        "variance_pct": pct,
                    });
                    if r.class_known {
                        obj["is_unfavorable"] = Value::from(r.is_unfavorable);
                    }
                    obj
                })
                .collect();
            let json_val = json!({
                "property": property.name,
                "period": period,
                "accounts": accounts,
            });
            let rendered = render_budget_variance_table(&property.name, period, &rows);
            Ok((json_val, rendered))
        }

        "turnover" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("turnover requires 'property'".to_string());
            }
            let since_period = input["since_period"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            match db::turn_summary(pool, &property.id, since_period)
                .await
                .map_err(|e| e.to_string())?
            {
                Some(s) => {
                    // Divide-by-zero guard: avg is "n/a" when the window holds no turns.
                    let avg_cost = match s.avg_cost_per_turn {
                        Some(v) => Value::from((v * 100.0).round() / 100.0),
                        None => Value::from("n/a"),
                    };
                    let avg_vac = match s.avg_vacancy_days {
                        Some(v) => Value::from((v * 10.0).round() / 10.0),
                        None => Value::from("n/a"),
                    };
                    let json_val = json!({
                        "property": property.name,
                        "since_period": since_period,
                        "turn_count": s.turn_count,
                        "total_turn_cost": s.total_turn_cost,
                        "avg_cost_per_turn": avg_cost,
                        "avg_vacancy_days": avg_vac,
                        "earliest_turn_date": s.earliest_turn_date,
                        "latest_turn_date": s.latest_turn_date,
                    });
                    let rendered = render_turnover_table(&property.name, since_period, &s);
                    Ok((json_val, rendered))
                }
                // Missing-feed guard: no turn rows for this property → say so, never $0/0 turns.
                None => {
                    let msg = format!(
                        "No turnover data for {} — cannot report turn cost. \
                         Do not assume zero; the turn_costs feed is missing for this property.",
                        property.name
                    );
                    let json_val = json!({
                        "property": property.name,
                        "turnover": Value::Null,
                        "note": msg,
                    });
                    Ok((json_val, msg))
                }
            }
        }

        "unit_pnl" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("unit_pnl requires 'property'".to_string());
            }
            let unit = input["unit"].as_str().unwrap_or("").trim();
            if unit.is_empty() {
                return Err("unit_pnl requires 'unit'".to_string());
            }
            let period = input["period"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            match db::unit_pnl(pool, &property.id, unit, period)
                .await
                .map_err(|e| e.to_string())?
            {
                // UNIT-LEVEL FINANCIALS ONLY: the JSON carries unit + period +
                // economics — NO resident name/code/balance is ever joined in.
                Some(p) => {
                    let json_val = json!({
                        "property": property.name,
                        "unit": p.unit,
                        "period": p.period,
                        "total_income": p.total_income,
                        "direct_expense": p.direct_expense,
                        "allocated_expense": p.allocated_expense,
                        "noi": p.noi,
                    });
                    let rendered = render_unit_pnl_table(&property.name, &p);
                    Ok((json_val, rendered))
                }
                // Missing-unit guard: the unit is not in the feed → say so, never
                // fabricate a $0 NOI for a unit that does not exist.
                None => {
                    let msg = format!(
                        "Unit {unit} not found at {} — no per-unit P&L on record for it. \
                         Do not assume zero; confirm the unit code or that the unit_pnl feed \
                         covers it.",
                        property.name
                    );
                    let json_val = json!({
                        "property": property.name,
                        "unit": unit,
                        "unit_pnl": Value::Null,
                        "note": msg,
                    });
                    Ok((json_val, msg))
                }
            }
        }

        "recall" => {
            let property_name = input["property"].as_str().unwrap_or("").trim();
            if property_name.is_empty() {
                return Err("recall requires 'property'".to_string());
            }
            let property = match db::find_property_by_name(pool, property_name).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let names = db::list_properties(pool)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "Property not found: '{property_name}'. Valid: {names}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            // Optional weighting inputs; empty strings are treated as absent.
            let call_type = input["call_type"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let decision_kind = input["decision_kind"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            // SAME today_period derivation as the `calls recall` CLI verb: current month (UTC).
            let today_period = db::now_iso()[..7].to_string();
            let ctx = recall::RecallCtx {
                property_id: property.id.clone(),
                call_type: call_type.clone(),
                decision_kind,
                // No entity scoping from the agent surface (the CLI's --entities path); empty.
                entity_keys: Vec::new(),
            };
            let r = recall::recall(pool, &ctx, &today_period, 5)
                .await
                .map_err(|e| e.to_string())?;
            // recall defaults an absent call_type to "decision"; mirror that in the output label.
            let effective_call_type = call_type.unwrap_or_else(|| "decision".to_string());
            // Neighbors carry ONLY recall's Neighbor fields — never operator_outcome free-text.
            let neighbors: Vec<Value> = r
                .neighbors
                .iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "summary": n.summary,
                        "score": n.score,
                        "status": n.status,
                        "age_months": n.age_months,
                        "similarity": n.similarity,
                    })
                })
                .collect();
            // Abstention guard: on a thin record DO NOT emit a posterior_mean/interval — the JSON
            // carries only n_eff + abstain:true so a reader can't mistake a shrunk prior for a
            // reliable rate. With enough history the calibrated stat is reported in full.
            let stat = if r.stat.abstain {
                json!({
                    "abstain": true,
                    "n_eff": r.stat.n_eff,
                })
            } else {
                json!({
                    "abstain": false,
                    "posterior_mean": r.stat.posterior_mean,
                    "n_eff": r.stat.n_eff,
                    "lo90": r.stat.lo90,
                    "hi90": r.stat.hi90,
                })
            };
            let json_val = json!({
                "property": property.name,
                "call_type": effective_call_type,
                "today_period": today_period,
                "abstain": r.stat.abstain,
                "stat": stat,
                "neighbors": neighbors,
            });
            let rendered = render_recall(&property.name, &effective_call_type, &today_period, &r);
            Ok((json_val, rendered))
        }

        unknown => {
            // Derive the valid-tool list from the registry so it can't drift out of sync.
            let valid = tool_specs()
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!("Unknown tool: '{unknown}'. Valid tools: {valid}"))
        }
    }
}

// ── AskResult ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AskResult {
    pub tool_calls: usize,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: String,
    pub status: AskStatus,
    /// Rendered output sections (text preambles, tool headers, tables) in
    /// display order. The caller decides where they go — stdout for the CLI,
    /// the Ask screen for the TUI. Nothing inside run_ask writes to stdout.
    pub rendered: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum AskStatus {
    Ok,
    Refusal,
    MaxTokens,
    Failed(String),
}

// ── run_ask ───────────────────────────────────────────────────────────────────

/// Run a natural-language ask against the local database.
///
/// - Default mode: one API call, tool results rendered locally, nothing sent back.
/// - Narrate mode: ≤4 rounds, tool results returned to model with PII redacted.
pub async fn run_ask(
    pool: &SqlitePool,
    provider: &dyn ToolUseProvider,
    question: &str,
    narrate: bool,
) -> Result<AskResult> {
    if narrate {
        eprintln!("narrate mode: query results are sent to the model provider");
    }

    let task_run_id = db::create_task_run(pool, "ask", question).await?;
    let system = build_system_prompt(pool).await?;
    let tools = tool_specs();

    let first_msg = ChatMessage {
        role: "user".to_string(),
        content: json!(question),
    };

    let mut total_input_tokens: i64 = 0;
    let mut total_output_tokens: i64 = 0;
    let mut tool_calls: usize = 0;
    let mut rendered: Vec<String> = Vec::new();
    let model_name;

    if !narrate {
        // ── Default mode: one call only ───────────────────────────────────────
        let resp = match provider.complete(&system, &[first_msg], &tools).await {
            Ok(r) => r,
            Err(e) => {
                db::complete_task_run(pool, &task_run_id, "failed", None, Some(&e.to_string()))
                    .await
                    .ok();
                return Err(e);
            }
        };

        total_input_tokens += resp.usage.input_tokens;
        total_output_tokens += resp.usage.output_tokens;
        model_name = resp.model.clone();

        let status =
            handle_response_default(pool, &task_run_id, &resp, &mut tool_calls, &mut rendered)
                .await;

        let summary = format!(
            "{tool_calls} tool calls · {total_input_tokens} in / {total_output_tokens} out tokens · model {model_name}"
        );
        let db_status = match &status {
            AskStatus::Ok => "completed",
            AskStatus::Refusal => "failed",
            AskStatus::MaxTokens => "failed",
            AskStatus::Failed(_) => "failed",
        };
        db::complete_task_run(pool, &task_run_id, db_status, None, Some(&summary)).await?;

        return Ok(AskResult {
            tool_calls,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            model: model_name,
            status,
            rendered,
        });
    }

    // ── Narrate mode: agentic loop ≤4 rounds ─────────────────────────────────
    let mut messages: Vec<ChatMessage> = vec![first_msg];
    let mut rounds = 0;
    let mut final_status = AskStatus::Ok;
    model_name = provider.model().to_string();

    loop {
        if rounds >= 4 {
            warn!("narrate mode: reached 4-round limit");
            break;
        }

        let resp = match provider.complete(&system, &messages, &tools).await {
            Ok(r) => r,
            Err(e) => {
                final_status = AskStatus::Failed(e.to_string());
                break;
            }
        };

        total_input_tokens += resp.usage.input_tokens;
        total_output_tokens += resp.usage.output_tokens;
        rounds += 1;

        let stop_reason = resp.stop_reason.as_deref().unwrap_or("end_turn");

        // Check terminal stop reasons first.
        if stop_reason == "refusal" {
            let detail = resp
                .content
                .iter()
                .find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("Model refused to answer.");
            rendered.push(format!("Refusal: {detail}"));
            final_status = AskStatus::Refusal;
            break;
        }

        if stop_reason == "max_tokens" {
            rendered.push("Response truncated. Please retry with a simpler question.".to_string());
            final_status = AskStatus::MaxTokens;
            break;
        }

        // Collect any text preamble / narration.
        for block in &resp.content {
            if let ContentBlock::Text { text } = block {
                rendered.push(text.clone());
            }
        }

        if stop_reason == "end_turn" {
            break;
        }

        // Collect tool_use blocks
        let tool_uses: Vec<&ContentBlock> = resp
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .collect();

        if tool_uses.is_empty() {
            break;
        }

        // Append assistant turn
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: serde_json::to_value(&resp.content).unwrap_or_default(),
        });

        // Execute tools and collect tool_result content blocks
        let mut tool_result_blocks: Vec<Value> = Vec::new();

        for block in &tool_uses {
            if let ContentBlock::ToolUse { id, name, input } = block {
                tool_calls += 1;
                rendered.push(format!("── tool: {name} {input} ──"));

                match execute_tool(pool, name, input).await {
                    Ok((mut json_val, table)) => {
                        rendered.push(table);
                        // Redact residents before sending back
                        redact_residents(&mut json_val);
                        log_tool_run(
                            pool,
                            &task_run_id,
                            name,
                            input.clone(),
                            Some(json_val.clone()),
                            None,
                        )
                        .await
                        .ok();
                        tool_result_blocks.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": json_val.to_string()
                        }));
                    }
                    Err(err_msg) => {
                        rendered.push(format!("Tool error ({name}): {err_msg}"));
                        log_tool_run(
                            pool,
                            &task_run_id,
                            name,
                            input.clone(),
                            None,
                            Some(&err_msg),
                        )
                        .await
                        .ok();
                        tool_result_blocks.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": format!("Error: {err_msg}"),
                            "is_error": true
                        }));
                    }
                }
            }
        }

        // Append user turn with tool results
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!(tool_result_blocks),
        });
    }

    let summary = format!(
        "{tool_calls} tool calls · {total_input_tokens} in / {total_output_tokens} out tokens · model {model_name}"
    );
    let db_status = match &final_status {
        AskStatus::Ok => "completed",
        AskStatus::Refusal => "failed",
        AskStatus::MaxTokens => "failed",
        AskStatus::Failed(_) => "failed",
    };
    db::complete_task_run(pool, &task_run_id, db_status, None, Some(&summary)).await?;

    Ok(AskResult {
        tool_calls,
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        model: model_name,
        status: final_status,
        rendered,
    })
}

/// Handle a default-mode response: collect text blocks, execute tool_use
/// blocks locally, and append every rendered section to `out`.
async fn handle_response_default(
    pool: &SqlitePool,
    task_run_id: &str,
    resp: &CompletionResponse,
    tool_calls: &mut usize,
    out: &mut Vec<String>,
) -> AskStatus {
    let stop_reason = resp.stop_reason.as_deref().unwrap_or("end_turn");

    if stop_reason == "refusal" {
        let detail = resp
            .content
            .iter()
            .find_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("Model refused to answer.");
        out.push(format!("Refusal: {detail}"));
        return AskStatus::Refusal;
    }

    if stop_reason == "max_tokens" {
        out.push("Response truncated. Please retry with a simpler question.".to_string());
        return AskStatus::MaxTokens;
    }

    for block in &resp.content {
        if let ContentBlock::Text { text } = block {
            out.push(text.clone());
        }
    }

    // Execute all tool_use blocks locally
    for block in &resp.content {
        if let ContentBlock::ToolUse { id: _, name, input } = block {
            *tool_calls += 1;
            out.push(format!("── tool: {name} {input} ──"));

            match execute_tool(pool, name, input).await {
                Ok((json_val, table)) => {
                    out.push(table);
                    log_tool_run(pool, task_run_id, name, input.clone(), Some(json_val), None)
                        .await
                        .ok();
                }
                Err(err_msg) => {
                    out.push(format!("Tool error ({name}): {err_msg}"));
                    log_tool_run(pool, task_run_id, name, input.clone(), None, Some(&err_msg))
                        .await
                        .ok();
                }
            }
        }
    }

    AskStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_note_flags_stale_and_skips_fresh() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 29).unwrap();
        // Fresh: 17 days old at a 45-day threshold → no caution.
        assert!(freshness_note("2026-06-12", today, 45).is_none());
        // Stale: ~150 days old → caution mentions the age and the as-of date.
        let note = freshness_note("2026-02-01", today, 45).expect("stale → Some");
        assert!(
            note.contains("148 days old") && note.contains("2026-02-01"),
            "got: {note}"
        );
        // Boundary: exactly 45 days is not yet stale (> threshold required).
        assert!(freshness_note("2026-05-15", today, 45).is_none());
        // Unparseable date → None (never fabricate a caution we can't ground).
        assert!(freshness_note("not-a-date", today, 45).is_none());
    }

    fn bv_row(category: &str, budget: f64, actual: f64) -> db::BudgetVarianceRow {
        let variance = actual - budget;
        let variance_pct = if budget == 0.0 {
            None
        } else {
            Some((variance / budget) * 100.0)
        };
        let (is_unfavorable, class_known) = match ontology::account_class(category) {
            ontology::AccountClass::Revenue => (actual < budget, true),
            ontology::AccountClass::Expense => (actual > budget, true),
            ontology::AccountClass::Unmapped => (false, false),
        };
        db::BudgetVarianceRow {
            account_code: "1130-0010".to_string(),
            account_name: "Cash in Escrow - Tax".to_string(),
            category: category.to_string(),
            budget,
            actual,
            variance,
            variance_pct,
            is_unfavorable,
            class_known,
        }
    }

    #[test]
    fn render_budget_variance_marks_unclassified_unmapped() {
        // An Unmapped (unclassifiable) account must render "(unclassified)" — never a
        // guessed UNFAVORABLE/favorable flag. (tool_runs persists only output_json, so
        // this rendered-surface guarantee is asserted here where the fn is reachable.)
        let rows = vec![bv_row("Unmapped", 1000.0, 1200.0)];
        let out = render_budget_variance_table("Test Apartments", "2026-05", &rows);
        assert!(
            out.contains("(unclassified)"),
            "Unmapped row must render (unclassified), got: {out}"
        );
        assert!(
            !out.contains("UNFAVORABLE") && !out.contains("favorable"),
            "Unmapped row must not render a favorability flag, got: {out}"
        );
        // Period is always surfaced (grounding).
        assert!(out.contains("2026-05"), "period must be in the header");
    }

    #[test]
    fn render_budget_variance_shows_na_for_zero_budget() {
        let rows = vec![bv_row("Utilities", 0.0, 500.0)];
        let out = render_budget_variance_table("Test Apartments", "2026-05", &rows);
        assert!(
            out.contains("n/a"),
            "zero-budget row must render n/a, got: {out}"
        );
    }

    #[test]
    fn render_budget_variance_flags_unfavorable_and_favorable() {
        // Expense over budget → UNFAVORABLE; revenue over-collected → favorable.
        let over = render_budget_variance_table(
            "Test Apartments",
            "2026-05",
            &[bv_row("Utilities", 1000.0, 1200.0)],
        );
        assert!(
            over.contains("UNFAVORABLE"),
            "expense over budget flag, got: {over}"
        );
        let fav = render_budget_variance_table(
            "Test Apartments",
            "2026-05",
            &[bv_row("Rental Income", 1000.0, 1200.0)],
        );
        assert!(
            fav.contains("favorable") && !fav.contains("UNFAVORABLE"),
            "revenue over-collected flag, got: {fav}"
        );
    }
}
