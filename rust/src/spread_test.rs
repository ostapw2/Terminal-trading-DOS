//! Does the "spreads" rule make money?  The rule: two coins that moved as
//! one over the last hour (r >= `MIN_R` on one-minute changes) whose price
//! ratio is `MIN_Z` or more sd away from its own hour's mean; buy the cheap
//! one, sell the rich one, equal dollars, and leave at +2 % / -1 % of the
//! basket's dollars or after 4 hours ([`crate::basket::DEFAULT_EXIT`]).
//!
//! This replays exactly that on one-minute history: every `STEP` minutes it
//! looks for such pairs, opens each (a pair rests `COOLDOWN` minutes after
//! it was traded, as the rule's cooling does), and follows the basket
//! minute by minute to its exit.  Fees: a maker fill in (2 bps a leg) and,
//! out, a maker fill on the target and a taker fill (10 bps a leg) on the
//! stop and the clock.  Entries are assumed filled at the close of the
//! signal minute (the live limits may miss some): an optimistic simplification,
//! stated wherever the result is shown.  Pure: series in, numbers out.

use serde::{Deserialize, Serialize};

pub const KV_KEY: &str = "spread_test";
pub const MIN_R: f64 = 0.8;
pub const MIN_Z: f64 = 2.0;
pub const LOOKBACK: usize = 60;
pub const STEP: usize = 5;
pub const COOLDOWN: usize = 120;
pub const TARGET_PCT: f64 = 2.0;
pub const STOP_PCT: f64 = 1.0;
pub const MAX_MIN: usize = 240;
pub const FEE_MAKER_BPS: f64 = 2.0;
pub const FEE_TAKER_BPS: f64 = 10.0;

/// How a basket leaves: its dollars' target and stop (%), and the clock (minutes).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub target_pct: f64,
    pub stop_pct: f64,
    pub max_min: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            target_pct: TARGET_PCT,
            stop_pct: STOP_PCT,
            max_min: MAX_MIN,
        }
    }
}

/// The exits tried when looking for a better one.
pub fn grid() -> Vec<Params> {
    let mut v = Vec::new();
    for target_pct in [0.25, 0.5, 1.0, 2.0] {
        for stop_pct in [0.5, 1.0] {
            for max_min in [60, 240] {
                v.push(Params {
                    target_pct,
                    stop_pct,
                    max_min,
                });
            }
        }
    }
    v
}

/// The default exit's replay, and the best other exit chosen on the older
/// two thirds of the history and then judged on the newest third.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpreadReport {
    pub default: SpreadTest,
    pub best: Option<(Params, SpreadTest, SpreadTest)>,
}

/// The report: the rule as it runs, and an honest look for a better exit.
pub fn report(series: &[(String, Vec<f64>)], at_ms: i64) -> SpreadReport {
    let default = run(series, at_ms);
    let n = series.iter().map(|s| s.1.len()).min().unwrap_or(0);
    let cut = n * 2 / 3;
    let mut best: Option<(Params, SpreadTest, SpreadTest)> = None;
    if cut > LOOKBACK + MAX_MIN + 60 && n > cut + LOOKBACK + 120 {
        let older: Vec<(String, Vec<f64>)> = series
            .iter()
            .map(|(s, c)| (s.clone(), c[c.len() - n..c.len() - n + cut].to_vec()))
            .collect();
        // The newest third, with the hour before it for the rule's lookback.
        let newer: Vec<(String, Vec<f64>)> = series
            .iter()
            .map(|(s, c)| (s.clone(), c[c.len() - (n - cut) - LOOKBACK..].to_vec()))
            .collect();
        for p in grid() {
            let a = run_params(&older, p, at_ms, 10);
            if a.trades < 30 {
                continue;
            }
            if best.as_ref().is_none_or(|b| a.mean_pct > b.1.mean_pct) {
                best = Some((p, a, SpreadTest::default()));
            }
        }
        if let Some((p, a, _)) = best.take() {
            let b = run_params(&newer, p, at_ms, 10);
            best = Some((p, a, b));
        }
    }
    SpreadReport { default, best }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpreadTest {
    /// Coins and minutes of history used.
    pub coins: usize,
    pub minutes: usize,
    /// Pairs that qualified at least once, trades made, and how they ended.
    pub pairs: usize,
    pub trades: usize,
    pub wins: usize,
    pub by_target: usize,
    pub by_stop: usize,
    pub by_clock: usize,
    /// Mean and worst net result of a trade, % of the basket's dollars.
    pub mean_pct: f64,
    pub worst_pct: f64,
    /// Sum of the nets, % of one basket (what a dollar basket made in all).
    pub total_pct: f64,
    /// Unix ms the test ran.
    pub at_ms: i64,
}

impl SpreadTest {
    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }
}

fn changes(c: &[f64]) -> Vec<f64> {
    let mut v = vec![0.0; c.len()];
    for i in 1..c.len() {
        if c[i - 1] > 0.0 {
            v[i] = c[i] / c[i - 1] - 1.0;
        }
    }
    v
}

/// Pearson r of two slices (0 when one does not move).
fn corr(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa <= 1e-18 || sbb <= 1e-18 {
        0.0
    } else {
        sab / (saa * sbb).sqrt()
    }
}

/// Replay the rule on `series` (per coin, one close per minute, oldest
/// first; every series the same length and the same minutes).
pub fn run(series: &[(String, Vec<f64>)], at_ms: i64) -> SpreadTest {
    run_params(series, Params::default(), at_ms, STEP)
}

/// [`run`] with another exit and another step between looks.
pub fn run_params(series: &[(String, Vec<f64>)], p: Params, at_ms: i64, step: usize) -> SpreadTest {
    let (target_pct, stop_pct, max_min) = (p.target_pct, p.stop_pct, p.max_min);
    let n = series.iter().map(|s| s.1.len()).min().unwrap_or(0);
    let mut out = SpreadTest {
        coins: series.len(),
        minutes: n,
        at_ms,
        ..Default::default()
    };
    if series.len() < 2 || n < LOOKBACK + max_min {
        return out;
    }
    let px: Vec<&[f64]> = series.iter().map(|s| &s.1[s.1.len() - n..]).collect();
    let ch: Vec<Vec<f64>> = px.iter().map(|p| changes(p)).collect();
    let mut last_trade: std::collections::HashMap<(usize, usize), usize> = Default::default();
    let mut traded_pairs: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut nets: Vec<f64> = Vec::new();
    let mut t = LOOKBACK;
    while t < n - 1 {
        for i in 0..px.len() {
            for j in i + 1..px.len() {
                if last_trade.get(&(i, j)).is_some_and(|&l| t < l + COOLDOWN) {
                    continue;
                }
                let w = t + 1 - LOOKBACK;
                if corr(&ch[i][w + 1..=t], &ch[j][w + 1..=t]) < MIN_R {
                    continue;
                }
                // The ratio against its hour's mean, in sd.
                let ratio: Vec<f64> = (w..=t).map(|k| px[i][k] / px[j][k]).collect();
                let m = ratio.iter().sum::<f64>() / ratio.len() as f64;
                let sd = (ratio.iter().map(|r| (r - m).powi(2)).sum::<f64>() / ratio.len() as f64)
                    .sqrt();
                if sd <= 0.0 {
                    continue;
                }
                let z = (ratio[ratio.len() - 1] - m) / sd;
                if z.abs() < MIN_Z {
                    continue;
                }
                // Ratio i/j high: i is rich (sell), j cheap (buy).
                let (long, short) = if z > 0.0 { (j, i) } else { (i, j) };
                let (l0, s0) = (px[long][t], px[short][t]);
                let mut exit_pct = 0.0;
                let mut how = 2u8; // 0 target, 1 stop, 2 clock
                let end = (t + max_min).min(n - 1);
                #[allow(clippy::needless_range_loop)]
                for k in t + 1..=end {
                    let pct = ((px[long][k] / l0 - 1.0) - (px[short][k] / s0 - 1.0)) / 2.0 * 100.0;
                    exit_pct = pct;
                    if pct >= target_pct {
                        how = 0;
                        break;
                    }
                    if pct <= -stop_pct {
                        how = 1;
                        break;
                    }
                }
                // A basket still running at the end of the data is not counted.
                if how == 2 && end < t + max_min {
                    continue;
                }
                let fee_pct = (2.0 * FEE_MAKER_BPS
                    + 2.0
                        * if how == 0 {
                            FEE_MAKER_BPS
                        } else {
                            FEE_TAKER_BPS
                        })
                    / 2.0
                    / 100.0;
                let net = exit_pct - fee_pct;
                last_trade.insert((i, j), t);
                traded_pairs.insert((i, j));
                nets.push(net);
                out.trades += 1;
                out.wins += usize::from(net > 0.0);
                match how {
                    0 => out.by_target += 1,
                    1 => out.by_stop += 1,
                    _ => out.by_clock += 1,
                }
            }
        }
        t += step;
    }
    out.pairs = traded_pairs.len();
    if !nets.is_empty() {
        out.total_pct = nets.iter().sum();
        out.mean_pct = out.total_pct / nets.len() as f64;
        out.worst_pct = nets.iter().copied().fold(f64::INFINITY, f64::min);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic noise.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 100_000) as f64 / 100_000.0 - 0.5
        }
    }

    /// Two coins that share a random walk; B also holds a mean-reverting
    /// gap (a slow wave plus noise), so their ratio swings and comes back.
    fn tied(n: usize, gap_amp: f64, seed: u64) -> Vec<(String, Vec<f64>)> {
        let mut r = Rng(seed);
        let mut a = 100.0f64;
        let mut va = Vec::new();
        let mut vb = Vec::new();
        let mut gap = 0.0f64;
        for _ in 0..n {
            let common = r.next() * 0.04;
            // Ornstein-Uhlenbeck gap: a slow pull back to 0.
            gap = gap * 0.995 + r.next() * gap_amp;
            a *= 1.0 + common;
            let b = 50.0 * (a / 100.0) * (1.0 + gap);
            va.push(a);
            vb.push(b);
        }
        // A third coin keeps the pair from being the only one.
        vec![("AAA".into(), va), ("BBB".into(), vb)]
    }

    #[test]
    fn a_mean_reverting_gap_pays_and_pure_noise_does_not() {
        let tied_pair = run(&tied(20_000, 0.012, 7), 0);
        assert!(tied_pair.trades >= 10, "{tied_pair:?}");
        assert!(tied_pair.mean_pct > 0.0, "{tied_pair:?}");
        // The target is twice the stop: fewer wins than losses can still pay.
        assert!(
            tied_pair.total_pct > 0.0 && tied_pair.by_target > 0,
            "{tied_pair:?}"
        );
        // Two unrelated walks never pass the correlation gate.
        let mut r = Rng(99);
        let walk = |r: &mut Rng| {
            let mut p = 10.0;
            (0..2_000)
                .map(|_| {
                    p *= 1.0 + r.next() * 0.004;
                    p
                })
                .collect::<Vec<f64>>()
        };
        let noise = run(&[("X".into(), walk(&mut r)), ("Y".into(), walk(&mut r))], 0);
        assert_eq!(noise.trades, 0, "{noise:?}");
    }

    #[test]
    fn too_little_history_or_one_coin_tests_nothing() {
        assert_eq!(run(&tied(100, 0.02, 1), 0).trades, 0);
        assert_eq!(run(&tied(2_000, 0.02, 1)[..1], 0).trades, 0);
    }

    #[test]
    fn the_report_judges_the_chosen_exit_on_history_it_was_not_chosen_on() {
        let r = report(&tied(9_000, 0.012, 3), 0);
        assert!(r.default.trades > 0, "{:?}", r.default);
        let (p, older, newer) = r.best.expect("a best exit");
        assert!(grid().contains(&p));
        assert!(older.trades >= 30 && newer.minutes < older.minutes + 200);
        // Short data: no split, no choice.
        assert!(report(&tied(400, 0.012, 3), 0).best.is_none());
    }
}
