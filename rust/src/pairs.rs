//! Statistical-arbitrage spread between two instruments.
//!
//! The spread is expressed as a **deviation in percent** from its own mean
//! over the loaded window, so 0 is "A and B are priced as they usually are
//! relative to each other" and +3 means A is 3% rich against B.
//!
//! - [`SpreadMode::Ratio`] — `A / B`, deviation from its mean ratio.
//! - [`SpreadMode::Hedged`] — OLS on log prices, `ln A = α + β ln B + e`;
//!   the deviation is the residual `e` expressed in percent (`e^e − 1`).
//!   Removes the part of A that simply follows B (β ≠ 1).
//!
//! Only closes are used and candles are matched by open time; times that
//! exist on one side only are dropped.  Pure functions, no I/O.

use crate::markets::Ohlc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpreadMode {
    Ratio,
    Hedged,
}

impl SpreadMode {
    pub fn next(self) -> Self {
        match self {
            Self::Ratio => Self::Hedged,
            Self::Hedged => Self::Ratio,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ratio => "Ratio A/B",
            Self::Hedged => "Beta-hedged (log OLS)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Ratio => "ratio",
            Self::Hedged => "beta",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Aligned candles used.
    pub n: usize,
    /// Latest deviation, percent.
    pub last: f64,
    /// Standard deviation of the deviation, percent points.
    pub sigma: f64,
    /// `(last - mean) / sigma`; 0 when sigma is 0.
    pub z: f64,
    /// Pearson correlation of the two log-return series.
    pub corr: f64,
    /// OLS slope of ln A on ln B (1.0 in Ratio mode it is still reported).
    pub beta: f64,
    pub min: f64,
    pub max: f64,
}

pub struct Spread {
    /// Deviation in percent per aligned candle.
    pub dev: Vec<f64>,
    pub time_ms: Vec<i64>,
    pub stats: Stats,
}

/// Fewer aligned points than this is not a statistic.
pub const MIN_POINTS: usize = 20;

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn std_dev(v: &[f64]) -> f64 {
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

fn correlation(x: &[f64], y: &[f64]) -> f64 {
    let (mx, my) = (mean(x), mean(y));
    let cov: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let vx: f64 = x.iter().map(|a| (a - mx).powi(2)).sum();
    let vy: f64 = y.iter().map(|b| (b - my).powi(2)).sum();
    if vx <= 0.0 || vy <= 0.0 {
        0.0
    } else {
        cov / (vx.sqrt() * vy.sqrt())
    }
}

/// Aligned closes of `a` and `b`: `(time, close_a, close_b)`.  Candles that
/// are gaps or have a non-positive close are skipped.
fn align(a: &[Ohlc], b: &[Ohlc]) -> Vec<(i64, f64, f64)> {
    let ok = |k: &&Ohlc| !k.is_gap && k.time_ms > 0 && k.close > 0.0;
    let by_time: std::collections::HashMap<i64, f64> =
        b.iter().filter(ok).map(|k| (k.time_ms, k.close)).collect();
    a.iter()
        .filter(ok)
        .filter_map(|k| by_time.get(&k.time_ms).map(|&cb| (k.time_ms, k.close, cb)))
        .collect()
}

/// Spread of `a` against `b`.  `None` when fewer than [`MIN_POINTS`]
/// candles line up.
pub fn spread(a: &[Ohlc], b: &[Ohlc], mode: SpreadMode) -> Option<Spread> {
    let rows = align(a, b);
    if rows.len() < MIN_POINTS {
        return None;
    }
    let ya: Vec<f64> = rows.iter().map(|r| r.1.ln()).collect();
    let xb: Vec<f64> = rows.iter().map(|r| r.2.ln()).collect();

    // OLS ln A = alpha + beta * ln B (always computed: it is reported too).
    let (mx, my) = (mean(&xb), mean(&ya));
    let sxx: f64 = xb.iter().map(|x| (x - mx).powi(2)).sum();
    let sxy: f64 = xb.iter().zip(&ya).map(|(x, y)| (x - mx) * (y - my)).sum();
    let beta = if sxx > 0.0 { sxy / sxx } else { 1.0 };
    let alpha = my - beta * mx;

    let dev: Vec<f64> = match mode {
        SpreadMode::Ratio => {
            let ratios: Vec<f64> = rows.iter().map(|r| r.1 / r.2).collect();
            let m = mean(&ratios);
            ratios.iter().map(|r| (r / m - 1.0) * 100.0).collect()
        }
        SpreadMode::Hedged => ya
            .iter()
            .zip(&xb)
            .map(|(y, x)| ((y - alpha - beta * x).exp() - 1.0) * 100.0)
            .collect(),
    };

    let ra: Vec<f64> = ya.windows(2).map(|w| w[1] - w[0]).collect();
    let rb: Vec<f64> = xb.windows(2).map(|w| w[1] - w[0]).collect();
    let sigma = std_dev(&dev);
    let m = mean(&dev);
    let last = *dev.last()?;
    let stats = Stats {
        n: rows.len(),
        last,
        sigma,
        z: if sigma > 1e-12 {
            (last - m) / sigma
        } else {
            0.0
        },
        corr: correlation(&ra, &rb),
        beta,
        min: dev.iter().cloned().fold(f64::INFINITY, f64::min),
        max: dev.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    };
    Some(Spread {
        dev,
        time_ms: rows.iter().map(|r| r.0).collect(),
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(closes: &[f64]) -> Vec<Ohlc> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Ohlc {
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1.0,
                date: String::new(),
                time_ms: 1_000 * (i as i64 + 1),
                is_gap: false,
            })
            .collect()
    }

    fn wave(n: usize, f: impl Fn(usize) -> f64) -> Vec<f64> {
        (0..n).map(f).collect()
    }

    #[test]
    fn identical_moves_give_a_flat_spread_and_unit_beta() {
        let b = wave(60, |i| 100.0 + i as f64);
        let a: Vec<f64> = b.iter().map(|x| x * 3.0).collect(); // A = 3 * B exactly
        for mode in [SpreadMode::Ratio, SpreadMode::Hedged] {
            let s = spread(&series(&a), &series(&b), mode).unwrap();
            assert!(s.dev.iter().all(|d| d.abs() < 1e-9), "{mode:?}: flat at 0");
            assert!((s.stats.beta - 1.0).abs() < 1e-9);
            assert!((s.stats.corr - 1.0).abs() < 1e-9);
            assert_eq!(s.stats.z, 0.0, "no variance, no z");
        }
    }

    #[test]
    fn a_shock_in_a_shows_as_percent_deviation_and_positive_z() {
        let b = vec![100.0; 40];
        let mut a = vec![50.0; 40];
        a[39] = 55.0; // A jumps 10% on the last candle
        let s = spread(&series(&a), &series(&b), SpreadMode::Ratio).unwrap();
        // Mean ratio = (39*0.5 + 0.55)/40 = 0.50125 -> last dev ~ +9.72%.
        assert!((s.stats.last - 9.7257).abs() < 1e-3, "{}", s.stats.last);
        assert!(
            s.stats.z > 5.0,
            "an outlier is many sigmas out: {}",
            s.stats.z
        );
        assert_eq!(s.stats.max, s.stats.last);
    }

    #[test]
    fn hedged_mode_removes_the_part_that_follows_b() {
        // A = B^2 (beta 2 in logs).  Ratio drifts, hedged residual stays 0.
        let b = wave(60, |i| 10.0 + i as f64 * 0.5);
        let a: Vec<f64> = b.iter().map(|x| x * x).collect();
        let ratio = spread(&series(&a), &series(&b), SpreadMode::Ratio).unwrap();
        let hedged = spread(&series(&a), &series(&b), SpreadMode::Hedged).unwrap();
        assert!(ratio.stats.sigma > 5.0, "ratio drifts with the trend");
        assert!(hedged.stats.sigma < 1e-6, "hedged is flat");
        assert!((hedged.stats.beta - 2.0).abs() < 1e-9);
    }

    #[test]
    fn candles_are_matched_by_time_and_short_overlap_is_refused() {
        let a = series(&wave(30, |i| 10.0 + i as f64));
        let mut b = series(&wave(30, |i| 20.0 + i as f64));
        b.iter_mut().for_each(|k| k.time_ms += 500); // nothing lines up
        assert!(spread(&a, &b, SpreadMode::Ratio).is_none());
        let few = series(&wave(MIN_POINTS - 1, |i| 1.0 + i as f64));
        assert!(spread(&few, &few, SpreadMode::Ratio).is_none());
    }

    #[test]
    fn gaps_and_bad_closes_are_skipped() {
        let mut a = series(&wave(40, |i| 10.0 + i as f64));
        let b = series(&wave(40, |i| 20.0 + i as f64));
        a[3].is_gap = true;
        a[7].close = 0.0;
        let s = spread(&a, &b, SpreadMode::Ratio).unwrap();
        assert_eq!(s.stats.n, 38);
    }

    #[test]
    fn anti_correlated_returns_give_negative_corr() {
        let b: Vec<f64> = (0..60)
            .map(|i| 100.0 + if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let a: Vec<f64> = (0..60)
            .map(|i| 100.0 - if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let s = spread(&series(&a), &series(&b), SpreadMode::Ratio).unwrap();
        assert!(s.stats.corr < -0.99, "{}", s.stats.corr);
    }

    #[test]
    fn mode_cycles() {
        assert_eq!(SpreadMode::Ratio.next(), SpreadMode::Hedged);
        assert_eq!(SpreadMode::Hedged.next(), SpreadMode::Ratio);
    }
}
