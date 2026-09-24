//! F5.2 — pure NOI-bridge waterfall computation.
//!
//! Turns a budget NOI and a set of favorable-positive category variances (from
//! `db::category_variance_for_period`) into an ordered list of waterfall steps:
//! a Budget-NOI anchor, the top-N drivers by magnitude (the rest folded into a
//! single "Other" step), and an Actual-NOI anchor. Each step carries its
//! running total so the render can draw each bar offset to its running
//! baseline, and the whole thing reconciles:
//!
//! ```text
//! Budget NOI + Σ favorable − Σ unfavorable = Actual NOI
//! ```
//!
//! Sign convention is inherited, never re-derived: `CategoryVariance.variance`
//! is already NOI impact (positive = favorable), matching
//! `variance.rs::noi_impact` / `ontology::account_class`.

use crate::db::CategoryVariance;

/// One row of the waterfall. Anchors (Budget/Actual NOI) carry the absolute NOI
/// level; driver steps carry the signed `delta` they contribute.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeStep {
    pub label: String,
    /// Signed NOI contribution of this step (0 for anchors).
    pub delta: f64,
    /// Running NOI total *after* this step is applied.
    pub running: f64,
    /// True for the Budget-NOI and Actual-NOI book-end rows.
    pub is_anchor: bool,
}

/// Build the waterfall: `Budget NOI` anchor → top-`top_n` drivers by
/// `|variance|` → folded `Other` (if any remain) → `Actual NOI` anchor.
///
/// `variances` is expected already ordered by magnitude (as
/// `category_variance_for_period` returns), but we sort defensively so callers
/// can't break the "biggest mover first" story.
pub fn build_bridge(
    budget_noi: f64,
    variances: &[CategoryVariance],
    top_n: usize,
) -> Vec<BridgeStep> {
    let mut sorted = variances.to_vec();
    sorted.sort_by(|a, b| b.variance.abs().total_cmp(&a.variance.abs()));

    let mut steps = Vec::with_capacity(sorted.len().min(top_n) + 3);
    let mut running = budget_noi;

    steps.push(BridgeStep {
        label: "Budget NOI".to_string(),
        delta: 0.0,
        running,
        is_anchor: true,
    });

    for cv in sorted.iter().take(top_n) {
        running += cv.variance;
        steps.push(BridgeStep {
            label: cv.category.clone(),
            delta: cv.variance,
            running,
            is_anchor: false,
        });
    }

    // Fold every remaining driver into a single "Other" step so the bridge
    // still reconciles exactly to Actual NOI without a long tail.
    let other: f64 = sorted.iter().skip(top_n).map(|cv| cv.variance).sum();
    if other.abs() > f64::EPSILON {
        running += other;
        steps.push(BridgeStep {
            label: "Other".to_string(),
            delta: other,
            running,
            is_anchor: false,
        });
    }

    steps.push(BridgeStep {
        label: "Actual NOI".to_string(),
        delta: 0.0,
        running,
        is_anchor: true,
    });

    steps
}

/// Budget NOI implied by a set of category variances.
///
/// Each `CategoryVariance` carries its raw `budget` total and category; NOI =
/// Σ(revenue budgets) − Σ(expense budgets), using the same
/// `ontology::account_class` split as everywhere else. `Unmapped` categories
/// were already dropped upstream by `category_variance_for_period`, so they
/// cannot reach here.
pub fn budget_noi_from_variances(variances: &[CategoryVariance]) -> f64 {
    variances
        .iter()
        .map(|cv| match crate::ontology::account_class(&cv.category) {
            crate::ontology::AccountClass::Revenue => cv.budget,
            crate::ontology::AccountClass::Expense => -cv.budget,
            crate::ontology::AccountClass::Unmapped => 0.0,
        })
        .sum()
}

/// Map each step to an `(offset, len)` in terminal cells, scaled to `width`.
///
/// The horizontal axis spans the **running-total range** `[axis_min, axis_max]`
/// — the lowest and highest running NOI the bridge passes through — so the
/// driver bars (each a segment between consecutive running totals) fill the
/// chart and are directly comparable in magnitude. This is the waterfall
/// reading an LP expects: the swings between Budget NOI and Actual NOI are the
/// story, not their absolute distance from zero (a $40k swing on $800k NOI
/// would otherwise be an invisible sliver).
///
/// - A **driver** bar spans `[min(prev_running, running), max(...)]`; a +delta
///   sits to the right of its baseline, a −delta to the left.
/// - An **anchor** bar spans from the left edge of the axis to its own running
///   level, acting as a reference line for Budget NOI / Actual NOI.
///
/// Returns one `(offset, len)` per step, in order. A real movement always
/// paints at least one cell so it never renders invisibly.
pub fn scale_bars(steps: &[BridgeStep], width: u16) -> Vec<(i64, i64)> {
    let width = width.max(1) as f64;
    if steps.is_empty() {
        return Vec::new();
    }

    // Axis = the range of running totals the bridge actually traverses.
    let mut axis_min = f64::INFINITY;
    let mut axis_max = f64::NEG_INFINITY;
    for step in steps {
        axis_min = axis_min.min(step.running);
        axis_max = axis_max.max(step.running);
    }
    if !axis_min.is_finite() || !axis_max.is_finite() {
        return steps.iter().map(|_| (0, 0)).collect();
    }
    let span = (axis_max - axis_min).max(f64::EPSILON);
    let scale =
        |v: f64| -> i64 { (((v - axis_min) / span) * width).round().clamp(0.0, width) as i64 };

    let mut out = Vec::with_capacity(steps.len());
    let mut prev_running = steps.first().map(|s| s.running).unwrap_or(axis_min);
    for (i, step) in steps.iter().enumerate() {
        let (lo, hi) = if step.is_anchor {
            // Anchor: reference bar from the axis floor to its running level.
            (axis_min, step.running)
        } else {
            let a = prev_running;
            let b = step.running;
            (a.min(b), a.max(b))
        };
        let offset = scale(lo);
        let mut len = scale(hi) - offset;
        // A real movement (or any anchor) should always paint at least one cell.
        if len < 1 && (step.is_anchor || step.delta.abs() > f64::EPSILON) {
            len = 1;
        }
        // Never overflow the available width.
        if offset + len > width as i64 {
            len = (width as i64 - offset).max(0);
        }
        out.push((offset, len));
        // The leading Budget-NOI anchor is the first driver's pivot; every
        // later step advances the running baseline.
        if i > 0 || step.is_anchor {
            prev_running = step.running;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(category: &str, variance: f64) -> CategoryVariance {
        // actual/budget are irrelevant to the bridge math; only `variance`
        // (already NOI-impact-signed) matters here.
        CategoryVariance {
            category: category.to_string(),
            actual: 0.0,
            budget: 0.0,
            variance,
        }
    }

    #[test]
    fn bridge_reconciles_budget_plus_deltas_to_actual() {
        // Budget 100. Rental +20 (favorable), R&M -5 (unfavorable), Payroll -3.
        let variances = vec![
            cv("Rental Income", 20.0),
            cv("R&M", -5.0),
            cv("Payroll", -3.0),
        ];
        let steps = build_bridge(100.0, &variances, 10);

        // First is Budget anchor, last is Actual anchor.
        assert!(steps.first().unwrap().is_anchor);
        assert_eq!(steps.first().unwrap().label, "Budget NOI");
        assert!((steps.first().unwrap().running - 100.0).abs() < 1e-9);

        let actual = steps.last().unwrap();
        assert!(actual.is_anchor);
        assert_eq!(actual.label, "Actual NOI");
        // 100 + 20 - 5 - 3 = 112.
        assert!(
            (actual.running - 112.0).abs() < 1e-9,
            "actual {}",
            actual.running
        );

        // Σ driver deltas + budget == actual.
        let driver_sum: f64 = steps.iter().filter(|s| !s.is_anchor).map(|s| s.delta).sum();
        assert!((100.0 + driver_sum - 112.0).abs() < 1e-9);
    }

    #[test]
    fn top_n_folds_remaining_drivers_into_other() {
        let variances = vec![cv("A", 50.0), cv("B", -40.0), cv("C", 10.0), cv("D", -7.0)];
        let steps = build_bridge(0.0, &variances, 2);
        // Budget + A + B + Other + Actual = 5 steps.
        assert_eq!(steps.len(), 5);
        let labels: Vec<_> = steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["Budget NOI", "A", "B", "Other", "Actual NOI"]);
        // Other folds C(+10) + D(-7) = +3.
        let other = steps.iter().find(|s| s.label == "Other").unwrap();
        assert!((other.delta - 3.0).abs() < 1e-9, "other {}", other.delta);
        // Still reconciles: 0 + 50 - 40 + 3 = 13.
        assert!((steps.last().unwrap().running - 13.0).abs() < 1e-9);
    }

    #[test]
    fn drivers_are_ordered_biggest_first() {
        let variances = vec![cv("small", 2.0), cv("huge", -90.0), cv("mid", 30.0)];
        let steps = build_bridge(0.0, &variances, 10);
        let drivers: Vec<_> = steps
            .iter()
            .filter(|s| !s.is_anchor)
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(drivers, vec!["huge", "mid", "small"]);
    }

    #[test]
    fn no_other_step_when_all_drivers_shown() {
        let variances = vec![cv("A", 5.0), cv("B", -3.0)];
        let steps = build_bridge(10.0, &variances, 10);
        assert!(steps.iter().all(|s| s.label != "Other"));
    }

    #[test]
    fn scale_bars_fits_within_width_and_paints_movement() {
        let variances = vec![cv("Rental Income", 20.0), cv("R&M", -5.0)];
        let steps = build_bridge(100.0, &variances, 10);
        let bars = scale_bars(&steps, 40);
        assert_eq!(bars.len(), steps.len());
        for (offset, len) in &bars {
            assert!(*offset >= 0, "offset negative: {offset}");
            assert!(*len >= 0, "len negative: {len}");
            assert!(offset + len <= 40, "bar overflows width: {offset}+{len}");
        }
        // Every driver with a real delta paints at least one cell.
        for (i, step) in steps.iter().enumerate() {
            if step.delta.abs() > f64::EPSILON {
                assert!(bars[i].1 >= 1, "driver {} painted nothing", step.label);
            }
        }
    }

    #[test]
    fn scale_bars_handles_degenerate_flat_bridge() {
        // No variance at all: budget == actual, every running total identical.
        let steps = build_bridge(50.0, &[], 5);
        let bars = scale_bars(&steps, 30);
        assert_eq!(bars.len(), steps.len());
        for (offset, len) in &bars {
            assert!(offset + len <= 30);
        }
    }
}
