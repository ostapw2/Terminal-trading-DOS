//! The judge: does a condition really change the odds, or is it chance?
//!
//! Question: when `cond` turns true (> 0) at candle t, is `target` higher h
//! candles later more often than usual?  Every answer is checked four ways,
//! because searching many conditions finds good-looking ones in pure noise
//! (Bailey, López de Prado 2014; Harvey, Liu, Zhu 2016; White 2000):
//!
//! 1. **Events** are the candles where the condition turns on, at least h
//!    apart: one long episode is one event, not twenty.
//! 2. **Time split:** the older part (default 70 %) and the newer one are
//!    judged separately; a real effect shows in both, in the same direction.
//! 3. **Chaos baseline:** the same test with the condition shifted around
//!    in time (circularly: each series keeps its own structure, only the
//!    link between them is broken), many times; p = how often a shifted
//!    condition does at least as well.
//! 4. **Number of tries:** the p threshold is 5 % divided by the number of
//!    questions asked so far (Bonferroni), and the report says so.
//!
//! The odds come with a Wilson 95 % interval.  The signal is read at the
//! close of t and the outcome is close t+h over close t: no look-ahead, and
//! no fees or slippage either (this judges odds, not a strategy).  Pure.

pub const MIN_EVENTS: usize = 20;

/// A share as a percent that stays readable when tiny (`0.0012%`, `5e-6%`).
pub fn pct(x: f64) -> String {
    let p = x * 100.0;
    if p >= 0.1 {
        format!("{p:.1}%")
    } else if p >= 0.0001 {
        format!("{p:.4}%")
    } else {
        format!("{p:.0e}%")
    }
}
pub const DEFAULT_SHIFTS: usize = 500;
/// Most shifts the chaos test runs (for a strict threshold after many tries).
pub const MAX_SHIFTS: usize = 1_000_000;
const Z: f64 = 1.96;

/// Wilson score interval of `k` successes in `n` at 95 %.
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (n, p) = (n as f64, k as f64 / n as f64);
    let d = 1.0 + Z * Z / n;
    let c = (p + Z * Z / (2.0 * n)) / d;
    let h = Z * (p * (1.0 - p) / n + Z * Z / (4.0 * n * n)).sqrt() / d;
    ((c - h).max(0.0), (c + h).min(1.0))
}

/// One period's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Side {
    pub events: usize,
    /// Events after which the target was higher.
    pub up: usize,
    /// up / events, with its 95 % interval.
    pub hit: f64,
    pub hit_ci: (f64, f64),
    /// Mean outcome after an event, % (95 % interval).
    pub mean: f64,
    pub mean_ci: (f64, f64),
    /// The same over every candle of the period: what "usual" is.
    pub base_hit: f64,
    pub base_mean: f64,
    pub candles: usize,
}

impl Side {
    /// Does the interval of the odds leave the usual odds out?
    pub fn clear_of_base(&self) -> bool {
        self.events > 0 && (self.hit_ci.0 > self.base_hit || self.hit_ci.1 < self.base_hit)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub horizon: usize,
    pub older: Side,
    pub newer: Side,
    /// Share of shifted conditions with an edge at least as large (whole
    /// history), and how many shifts were tried.
    pub p_chaos: f64,
    pub shifts: usize,
    /// Questions asked so far (this one included) and the p threshold.
    pub tries: usize,
    pub alpha: f64,
    pub survives: bool,
    /// One sentence why.
    pub reason: String,
    /// The moves (%, close t+h over close t) after every event, and after
    /// every candle (the usual), whole history: the two distributions a
    /// chart compares.
    pub moves: Vec<f64>,
    pub usual: Vec<f64>,
}

/// Percent change `h` candles ahead of each t (NaN where unknown).
pub fn outcomes(target: &[f64], h: usize) -> Vec<f64> {
    (0..target.len())
        .map(|t| match (target.get(t), target.get(t + h)) {
            (Some(&a), Some(&b)) if a.is_finite() && b.is_finite() && a > 0.0 => {
                (b / a - 1.0) * 100.0
            }
            _ => f64::NAN,
        })
        .collect()
}

/// Candles where `cond` turns on (from off), at least `h` apart, with a
/// known outcome, inside `[from, to)` with the outcome inside it too.
fn events(cond: &[f64], out: &[f64], h: usize, from: usize, to: usize) -> Vec<usize> {
    let mut v = Vec::new();
    let mut next_ok = from;
    for t in from..to.min(cond.len()) {
        let on = cond[t].is_finite() && cond[t] > 0.0;
        let was = t > 0 && cond[t - 1].is_finite() && cond[t - 1] > 0.0;
        if on && !was && t >= next_ok && t + h < to && out[t].is_finite() {
            v.push(t);
            next_ok = t + h.max(1);
        }
    }
    v
}

fn side(cond: &[f64], out: &[f64], h: usize, from: usize, to: usize) -> Side {
    let ev = events(cond, out, h, from, to);
    let vals: Vec<f64> = ev.iter().map(|&t| out[t]).collect();
    let up = vals.iter().filter(|v| **v > 0.0).count();
    let n = vals.len();
    let mean = if n > 0 {
        vals.iter().sum::<f64>() / n as f64
    } else {
        f64::NAN
    };
    let sd = if n > 1 {
        (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        f64::NAN
    };
    let half = Z * sd / (n as f64).sqrt();
    let base: Vec<f64> = (from..to.saturating_sub(h))
        .filter_map(|t| out.get(t).copied().filter(|v| v.is_finite()))
        .collect();
    let bn = base.len().max(1) as f64;
    Side {
        events: n,
        up,
        hit: if n > 0 {
            up as f64 / n as f64
        } else {
            f64::NAN
        },
        hit_ci: wilson(up, n),
        mean,
        mean_ci: (mean - half, mean + half),
        base_hit: base.iter().filter(|v| **v > 0.0).count() as f64 / bn,
        base_mean: base.iter().sum::<f64>() / bn,
        candles: base.len(),
    }
}

/// The quick look a search takes at a candidate: events and edge (odds
/// after the event minus the usual odds) on the older part only, so the
/// newer part stays unseen until the full [`judge`].
pub fn screen(cond: &[f64], target: &[f64], h: usize, older_share: f64) -> (usize, f64) {
    let n = cond.len().min(target.len());
    let out = outcomes(&target[..n], h);
    let cut = ((n as f64) * older_share.clamp(0.3, 0.9)) as usize;
    let s = side(&cond[..n], &out, h, 0, cut);
    let e = if s.events == 0 {
        0.0
    } else {
        s.hit - s.base_hit
    };
    (s.events, e)
}

/// Events and edge (odds after the event minus the usual odds) of `cond`
/// inside `[from, to)`, with `out` from [`outcomes`]: the cheap form a
/// search runs many times over the same outcomes.
pub fn edge_in(cond: &[f64], out: &[f64], h: usize, from: usize, to: usize) -> (usize, f64) {
    let s = side(cond, out, h, from, to);
    let e = if s.events == 0 {
        0.0
    } else {
        s.hit - s.base_hit
    };
    (s.events, e)
}

/// Hit rate minus the usual rate over the whole history.
fn edge(cond: &[f64], out: &[f64], h: usize) -> f64 {
    let s = side(cond, out, h, 0, cond.len());
    if s.events == 0 {
        0.0
    } else {
        s.hit - s.base_hit
    }
}

/// Judge `cond` (aligned with `target`, same length) at horizon `h`.
/// `tries`: questions asked so far, this one included (at least 1).
pub fn judge(
    cond: &[f64],
    target: &[f64],
    h: usize,
    older_share: f64,
    shifts: usize,
    tries: usize,
) -> Result<Verdict, String> {
    let n = cond.len().min(target.len());
    if h == 0 || h * 4 >= n {
        return Err(format!(
            "horizon {h} needs more than {} candles (have {n})",
            h * 4
        ));
    }
    let (cond, target) = (&cond[..n], &target[..n]);
    let out = outcomes(target, h);
    let cut = ((n as f64) * older_share.clamp(0.3, 0.9)) as usize;
    let older = side(cond, &out, h, 0, cut);
    let newer = side(cond, &out, h, cut, n);
    let observed = edge(cond, &out, h);
    let tries = tries.max(1);
    let alpha = 0.05 / tries as f64;
    let same_way = (older.hit - older.base_hit).signum() == (newer.hit - newer.base_hit).signum();
    let plausible = older.events >= MIN_EVENTS
        && newer.events >= MIN_EVENTS
        && same_way
        && newer.clear_of_base();
    // Chaos: shift the condition around the circle by amounts that keep it
    // well away from where it was (more than h, less than n - h).  p can
    // not go below 1/(shifts+1): a candidate that passed the other checks
    // gets as many shifts as its threshold needs (2/alpha, capped), and the
    // run stops as soon as the threshold is out of reach.
    let need = if plausible {
        ((2.0 / alpha).ceil() as usize).clamp(shifts, MAX_SHIFTS)
    } else {
        shifts
    };
    let span = n - 2 * h - 1;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ n as u64;
    let mut as_good = 0usize;
    let mut tried = 0usize;
    let mut shifted = vec![0.0; n];
    while tried < need {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let s = h + 1 + (seed % span as u64) as usize;
        for (t, v) in shifted.iter_mut().enumerate() {
            *v = cond[(t + s) % n];
        }
        let e = edge(&shifted, &out, h);
        tried += 1;
        if observed != 0.0 && e * observed.signum() >= observed.abs() {
            as_good += 1;
        }
        // Past the minimum, stop once even a clean rest could not pass.
        if tried >= shifts && (as_good + 1) as f64 / (need + 1) as f64 > alpha {
            break;
        }
    }
    let p_chaos = if observed == 0.0 {
        1.0
    } else {
        (as_good + 1) as f64 / (tried + 1) as f64
    };
    use crate::i18n::pick;
    let (survives, reason) = if older.events < MIN_EVENTS || newer.events < MIN_EVENTS {
        (
            false,
            pick(
                format!(
                    "too few events ({} older, {} newer; need {MIN_EVENTS} each): longer history or a looser condition",
                    older.events, newer.events
                ),
                format!(
                    "замало подій ({} у старій, {} у новій; треба по {MIN_EVENTS}): довша історія або м'якша умова",
                    older.events, newer.events
                ),
            ),
        )
    } else if !same_way {
        (
            false,
            pick(
                "the effect flips between older and newer: not stable".to_string(),
                "ефект змінює знак між старою й новою частиною: нестійкий".to_string(),
            ),
        )
    } else if !newer.clear_of_base() {
        (
            false,
            pick(
                "on the newer part the odds are within the usual".to_string(),
                "на новій частині шанси в межах звичайних".to_string(),
            ),
        )
    } else if p_chaos > alpha {
        (
            false,
            pick(
                format!(
                    "a meaningless shifted condition does as well {} of the time (needs < {} after {tries} question(s))",
                    pct(p_chaos),
                    pct(alpha)
                ),
                format!(
                    "безглузда зсунута умова так само добра в {} випадків (треба < {} після {tries} питань)",
                    pct(p_chaos),
                    pct(alpha)
                ),
            ),
        )
    } else {
        (
            true,
            pick(
                format!(
                    "holds: both parts, newer clear of usual, chance {} < {} ({tries} question(s))",
                    pct(p_chaos),
                    pct(alpha)
                ),
                format!(
                    "тримається: обидві частини, нова поза звичайним, випадок {} < {} ({tries} питань)",
                    pct(p_chaos),
                    pct(alpha)
                ),
            ),
        )
    };
    let moves: Vec<f64> = events(cond, &out, h, 0, n)
        .iter()
        .map(|&t| out[t])
        .collect();
    let usual: Vec<f64> = out.iter().copied().filter(|v| v.is_finite()).collect();
    Ok(Verdict {
        horizon: h,
        older,
        newer,
        p_chaos,
        shifts: tried,
        tries,
        alpha,
        survives,
        reason,
        moves,
        usual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 1_000_000) as f64 / 1_000_000.0
        }
    }

    /// A random walk and a random on/off condition (about 1 in 25 candles).
    fn noise(seed: u64, n: usize) -> (Vec<f64>, Vec<f64>) {
        let mut r = Rng(seed);
        let mut p = 100.0;
        let mut target = Vec::with_capacity(n);
        let mut cond = Vec::with_capacity(n);
        for _ in 0..n {
            p *= 1.0 + (r.next() - 0.5) * 0.02;
            target.push(p);
            cond.push(if r.next() < 0.04 { 1.0 } else { 0.0 });
        }
        (cond, target)
    }

    #[test]
    fn wilson_matches_known_values() {
        let (lo, hi) = wilson(50, 100);
        assert!(
            (lo - 0.4038).abs() < 1e-3 && (hi - 0.5962).abs() < 1e-3,
            "{lo} {hi}"
        );
        assert_eq!(wilson(0, 0), (0.0, 1.0));
        let (lo, hi) = wilson(0, 10);
        assert!(lo == 0.0 && hi > 0.25 && hi < 0.35);
    }

    #[test]
    fn a_real_effect_survives_and_noise_does_not() {
        let n = 3000;
        let h = 5;
        // Noise: a condition with no link to the target.
        let (cond, target) = noise(11, n);
        let v = judge(&cond, &target, h, 0.7, DEFAULT_SHIFTS, 1).unwrap();
        assert!(!v.survives, "noise survived: {}", v.reason);
        // Plant an effect: after the condition turns on, the target climbs.
        let mut target2 = target.clone();
        let mut lift = 1.0;
        for t in 2..n {
            // The condition turned on at t-1 (read at its close): the
            // jump comes at t, inside the h candles that are judged.
            if cond[t - 2] <= 0.0 && cond[t - 1] > 0.0 {
                lift *= 1.03;
            }
            target2[t] = target[t] * lift;
        }
        let v = judge(&cond, &target2, h, 0.7, DEFAULT_SHIFTS, 1).unwrap();
        assert!(v.survives, "{} | {:?}", v.reason, v.newer);
        assert!(v.newer.hit > v.newer.base_hit && v.p_chaos < 0.01);
        // A strong effect still holds after 1000 questions: the chaos test
        // runs as many shifts as the stricter threshold needs.
        let many = judge(&cond, &target2, h, 0.7, DEFAULT_SHIFTS, 1000).unwrap();
        assert!(
            many.survives && many.shifts >= 40_000,
            "{} ({} shifts)",
            many.reason,
            many.shifts
        );
        assert!(many.reason.contains("1000"), "{}", many.reason);
        // Noise after 1000 questions stops early: the threshold is out of reach.
        let quick = judge(&cond, &target, h, 0.7, DEFAULT_SHIFTS, 1000).unwrap();
        assert!(
            !quick.survives && quick.shifts <= DEFAULT_SHIFTS,
            "{}",
            quick.shifts
        );
    }

    #[test]
    fn pure_noise_rarely_passes() {
        // Twenty unrelated conditions: at 5 % a wrong "holds" is rare, and
        // the newer-part check makes it rarer still.
        let passed = (1..=20)
            .filter(|&seed| {
                let (cond, target) = noise(seed * 7919, 2000);
                judge(&cond, &target, 3, 0.7, 200, 1).unwrap().survives
            })
            .count();
        assert!(passed <= 2, "{passed} of 20 noise conditions passed");
    }

    #[test]
    fn events_are_onsets_at_least_h_apart_with_no_look_ahead() {
        let cond = [0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let target: Vec<f64> = (0..12).map(|i| 100.0 + i as f64).collect();
        let out = outcomes(&target, 2);
        // Onsets at 1, 5, 8; 5 is 4 after 1 (>= 2) so kept; 8 kept; the
        // outcome of an event must lie inside the period.
        assert_eq!(events(&cond, &out, 2, 0, 12), vec![1, 5, 8]);
        assert_eq!(events(&cond, &out, 2, 0, 9), vec![1, 5], "8+2 is outside");
        assert_eq!(
            events(&cond, &out, 5, 0, 12),
            vec![1],
            "5 is too close to 1"
        );
        assert!(
            judge(&cond, &target, 3, 0.7, 10, 1).is_err(),
            "history too short"
        );
    }
}
