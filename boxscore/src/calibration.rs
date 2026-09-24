//! Pure calibration math for the decision-outcome flywheel.
//! Beta-Binomial shrinkage on exponentially time-decayed scored calls. No DB, no I/O.

pub const PRIOR_STRENGTH: f64 = 8.0;
pub const PRIOR_MEAN: f64 = 0.5;
pub const HALF_LIFE_MONTHS: f64 = 7.0;
pub const ABSTAIN_N_EFF: f64 = 5.0;

#[derive(Debug, Clone, Copy)]
pub struct ScoredPoint {
    pub score: f64,
    pub age_months: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct CalibratedStat {
    pub posterior_mean: f64,
    pub n_eff: f64,
    pub lo90: f64,
    pub hi90: f64,
    pub abstain: bool,
}

/// Exponential time-decay weight: 1.0 at age 0, 0.5 at one half-life.
pub fn decay_weight(age_months: f64, half_life_months: f64) -> f64 {
    if half_life_months <= 0.0 {
        return 1.0;
    }
    0.5_f64.powf(age_months.max(0.0) / half_life_months)
}

/// Beta-Binomial posterior over time-decayed scored calls.
/// `s = Σ w·score`, `n_eff = Σ w`; posterior mean = (s + k·m0)/(n_eff + k).
/// 90% credible interval from Beta(α, β) with α = s + k·m0, β = (n_eff − s) + k·(1 − m0).
pub fn calibrated_stat(
    points: &[ScoredPoint],
    prior_mean: f64,
    prior_strength: f64,
    half_life_months: f64,
    abstain_below_n_eff: f64,
) -> CalibratedStat {
    let mut s = 0.0;
    let mut n_eff = 0.0;
    for p in points {
        let w = decay_weight(p.age_months, half_life_months);
        s += w * p.score.clamp(0.0, 1.0);
        n_eff += w;
    }
    let k = prior_strength;
    let m0 = prior_mean;
    let posterior_mean = (s + k * m0) / (n_eff + k);
    let alpha = (s + k * m0).max(1e-6);
    let beta = ((n_eff - s) + k * (1.0 - m0)).max(1e-6);
    let (lo90, hi90) = beta_interval_90(alpha, beta);
    CalibratedStat {
        posterior_mean,
        n_eff,
        lo90,
        hi90,
        abstain: n_eff < abstain_below_n_eff,
    }
}

/// 90% equal-tailed interval of Beta(α, β) via bisection on the regularized incomplete beta.
fn beta_interval_90(alpha: f64, beta: f64) -> (f64, f64) {
    (inv_betai(0.05, alpha, beta), inv_betai(0.95, alpha, beta))
}

fn inv_betai(target: f64, a: f64, b: f64) -> f64 {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if betai(mid, a, b) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// P(X < threshold) where X ~ Beta(alpha, beta) — used for the NOI normalize-direction flip.
/// Returns the regularized incomplete beta I_threshold(alpha, beta).
pub fn prob_below(threshold: f64, alpha: f64, beta: f64) -> f64 {
    betai(threshold, alpha, beta)
}

/// Regularized incomplete beta I_x(a,b) via Lentz's continued fraction (Numerical Recipes).
fn betai(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_beta = ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b);
    let lx = a * x.ln() + b * (1.0 - x).ln() - ln_beta;
    let front = lx.exp() / a;
    if x < (a + 1.0) / (a + b + 2.0) {
        front * betacf(x, a, b)
    } else {
        let front_b = lx.exp() / b;
        1.0 - front_b * betacf(1.0 - x, b, a)
    }
}

/// Lentz's modified continued fraction for the incomplete beta.
fn betacf(x: f64, a: f64, b: f64) -> f64 {
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0_f64;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < 1e-30 {
        d = 1e-30;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1_i32..200 {
        let mf = f64::from(m);
        let m2 = 2.0 * mf;
        // Even step
        let aa = mf * (b - mf) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < 1e-30 {
            d = 1e-30;
        }
        c = 1.0 + aa / c;
        if c.abs() < 1e-30 {
            c = 1e-30;
        }
        d = 1.0 / d;
        h *= d * c;
        // Odd step
        let aa2 = -(a + mf) * (qab + mf) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa2 * d;
        if d.abs() < 1e-30 {
            d = 1e-30;
        }
        c = 1.0 + aa2 / c;
        if c.abs() < 1e-30 {
            c = 1e-30;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-10 {
            break;
        }
    }
    h
}

/// Lanczos ln Γ(x) — clean standard form; verified: ln_gamma(5) ≈ 3.178053.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.120_865_097_386_617_9e-2,
        -0.539_523_938_495_3e-5,
    ];
    let tmp = (x + 0.5) * (x + 5.5).ln() - (x + 5.5);
    let mut ser = 1.000_000_000_190_015;
    let mut y = x;
    for g in G {
        y += 1.0;
        ser += g / y;
    }
    tmp + (2.506_628_274_631_000_5 * ser / x).ln()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Bin {
    pub lo: f64,
    pub hi: f64,
    pub n: usize,
    pub mean_conf: f64,
    pub mean_score: f64,
}

pub fn reliability_bins(pairs: &[(f64, f64)], n_bins: usize) -> Vec<Bin> {
    let nb = n_bins.max(1);
    (0..nb)
        .map(|i| {
            let lo = i as f64 / nb as f64;
            let hi = (i + 1) as f64 / nb as f64;
            let in_bin: Vec<&(f64, f64)> = pairs
                .iter()
                .filter(|(c, _)| {
                    let last = i + 1 == nb;
                    *c >= lo && (*c < hi || (last && *c <= hi))
                })
                .collect();
            let n = in_bin.len();
            let (mc, ms) = if n == 0 {
                (0.0, 0.0)
            } else {
                (
                    in_bin.iter().map(|(c, _)| c).sum::<f64>() / n as f64,
                    in_bin.iter().map(|(_, s)| s).sum::<f64>() / n as f64,
                )
            };
            Bin {
                lo,
                hi,
                n,
                mean_conf: mc,
                mean_score: ms,
            }
        })
        .collect()
}

/// Expected Calibration Error: Σ (n_bin/N) |mean_conf − mean_score|.
pub fn ece(pairs: &[(f64, f64)]) -> f64 {
    if pairs.is_empty() {
        return 0.0;
    }
    let n = pairs.len() as f64;
    reliability_bins(pairs, 10)
        .iter()
        .map(|b| (b.n as f64 / n) * (b.mean_conf - b.mean_score).abs())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ece_zero_for_perfectly_calibrated() {
        // confidence == realized in every bin -> ECE 0.
        let pairs: Vec<(f64, f64)> = vec![(0.1, 0.1), (0.5, 0.5), (0.9, 0.9)];
        assert!(ece(&pairs) < 1e-9);
    }

    #[test]
    fn ece_positive_for_overconfident() {
        let pairs: Vec<(f64, f64)> = vec![(0.9, 0.1), (0.9, 0.2)];
        assert!(ece(&pairs) > 0.5);
    }

    #[test]
    fn decay_weight_halves_at_half_life() {
        assert!((decay_weight(0.0, 7.0) - 1.0).abs() < 1e-9);
        assert!((decay_weight(7.0, 7.0) - 0.5).abs() < 1e-9);
        assert!((decay_weight(14.0, 7.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn small_n_shrinks_toward_prior_and_abstains() {
        // 2 fresh perfect scores, prior mean 0.5, strength 8 -> ~ (2 + 8*0.5)/(2+8) = 0.6, abstain (n_eff<5).
        let pts = vec![
            ScoredPoint {
                score: 1.0,
                age_months: 0.0,
            },
            ScoredPoint {
                score: 1.0,
                age_months: 0.0,
            },
        ];
        let s = calibrated_stat(
            &pts,
            PRIOR_MEAN,
            PRIOR_STRENGTH,
            HALF_LIFE_MONTHS,
            ABSTAIN_N_EFF,
        );
        assert!((s.posterior_mean - 0.6).abs() < 1e-6);
        assert!((s.n_eff - 2.0).abs() < 1e-9);
        assert!(s.abstain);
        assert!(s.lo90 < s.posterior_mean && s.hi90 > s.posterior_mean);
    }

    #[test]
    fn large_fresh_n_converges_to_local_rate_and_stops_abstaining() {
        // 30 fresh scores at 0.8 -> posterior near 0.8, not abstaining.
        let pts: Vec<ScoredPoint> = (0..30)
            .map(|_| ScoredPoint {
                score: 0.8,
                age_months: 0.0,
            })
            .collect();
        let s = calibrated_stat(
            &pts,
            PRIOR_MEAN,
            PRIOR_STRENGTH,
            HALF_LIFE_MONTHS,
            ABSTAIN_N_EFF,
        );
        assert!((s.posterior_mean - (24.0 + 4.0) / (30.0 + 8.0)).abs() < 1e-6); // (s + k*m0)/(n+k)
        assert!(!s.abstain);
    }

    #[test]
    fn old_points_decay_out_of_n_eff() {
        // One 0-month and one 70-month point: n_eff ~ 1 + 0.5^10 ≈ 1.001.
        let pts = vec![
            ScoredPoint {
                score: 1.0,
                age_months: 0.0,
            },
            ScoredPoint {
                score: 0.0,
                age_months: 70.0,
            },
        ];
        let s = calibrated_stat(
            &pts,
            PRIOR_MEAN,
            PRIOR_STRENGTH,
            HALF_LIFE_MONTHS,
            ABSTAIN_N_EFF,
        );
        assert!(s.n_eff < 1.01 && s.n_eff > 0.99);
    }

    #[test]
    fn betai_matches_known_values() {
        assert!((betai(0.5, 1.0, 1.0) - 0.5).abs() < 1e-6); // uniform
        assert!((betai(0.5, 2.0, 2.0) - 0.5).abs() < 1e-6); // symmetric
        assert!(betai(0.9, 2.0, 5.0) > betai(0.5, 2.0, 5.0)); // monotone
    }

    #[test]
    fn prob_below_half_on_uniform_prior() {
        // Beta(1,1) is uniform; P(X < 0.5) = 0.5 exactly.
        assert!((prob_below(0.5, 1.0, 1.0) - 0.5).abs() < 1e-6);
        // Beta(1,9) is strongly skewed left; P(X < 0.5) >> 0.75.
        assert!(prob_below(0.5, 1.0, 9.0) > 0.75);
        // Beta(9,1) is strongly skewed right; P(X < 0.5) < 0.25.
        assert!(prob_below(0.5, 9.0, 1.0) < 0.25);
    }
}
