//! Small, pure indicators over closed bars.  No state: each call recomputes
//! from the slice (bar history is capped at 1024, so this is cheap).

use super::Bar;

/// Mean of the last `period` closes; `None` while there are fewer bars.
pub fn sma(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period {
        return None;
    }
    let s: f64 = bars[bars.len() - period..].iter().map(|b| b.close).sum();
    Some(s / period as f64)
}

/// Wilder's RSI of the closes (0..=100).  Needs `period + 1` bars.  A series
/// with no losses reads 100, one with no movement at all reads 50.
pub fn rsi(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period + 1 {
        return None;
    }
    let (mut gain, mut loss) = (0.0, 0.0);
    for w in bars[..=period].windows(2) {
        let d = w[1].close - w[0].close;
        if d >= 0.0 {
            gain += d;
        } else {
            loss -= d;
        }
    }
    let p = period as f64;
    let (mut avg_gain, mut avg_loss) = (gain / p, loss / p);
    for w in bars[period..].windows(2) {
        let d = w[1].close - w[0].close;
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        avg_gain = (avg_gain * (p - 1.0) + g) / p;
        avg_loss = (avg_loss * (p - 1.0) + l) / p;
    }
    Some(if avg_loss == 0.0 {
        if avg_gain == 0.0 {
            50.0
        } else {
            100.0
        }
    } else {
        100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
    })
}

/// Highest high of the `n` bars BEFORE the last one (the last bar is the one
/// being judged, so it must not break its own channel).
pub fn prior_high(bars: &[Bar], n: usize) -> Option<f64> {
    prior(bars, n).map(|s| s.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max))
}

/// Lowest low of the `n` bars before the last one.
pub fn prior_low(bars: &[Bar], n: usize) -> Option<f64> {
    prior(bars, n).map(|s| s.iter().map(|b| b.low).fold(f64::INFINITY, f64::min))
}

/// Average true range over `period` bars (Wilder's smoothing: the mean of
/// the first `period` true ranges, then each newer one folded in);
/// `None` while there are fewer than `period + 1` bars.
pub fn atr(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period + 1 {
        return None;
    }
    let tr: Vec<f64> = bars
        .windows(2)
        .map(|w| {
            (w[1].high - w[1].low)
                .max((w[1].high - w[0].close).abs())
                .max((w[1].low - w[0].close).abs())
        })
        .collect();
    let p = period as f64;
    let mut a = tr[..period].iter().sum::<f64>() / p;
    for t in &tr[period..] {
        a = (a * (p - 1.0) + t) / p;
    }
    Some(a)
}

fn prior(bars: &[Bar], n: usize) -> Option<&[Bar]> {
    if n == 0 || bars.len() < n + 1 {
        return None;
    }
    let end = bars.len() - 1;
    Some(&bars[end - n..end])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn atr_of_a_steady_range_is_that_range() {
        let b: Vec<Bar> = (0..30)
            .map(|i| Bar {
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
                time_ms: i,
            })
            .collect();
        assert!(atr(&b, 14).is_some_and(|a| (a - 2.0).abs() < 1e-9));
        assert!(atr(&b[..14], 14).is_none());
        assert!(atr(&b, 0).is_none());
    }

    pub fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 1.0,
                time_ms: i as i64 * 60_000,
            })
            .collect()
    }

    #[test]
    fn sma_averages_the_last_n_closes_and_waits_for_enough_bars() {
        let b = bars(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(sma(&b, 3), Some(4.0));
        assert_eq!(sma(&b, 5), Some(3.0));
        assert_eq!(sma(&b, 6), None);
        assert_eq!(sma(&b, 0), None);
    }

    #[test]
    fn rsi_reads_extremes_and_neutral() {
        let up: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        assert_eq!(rsi(&bars(&up), 14), Some(100.0), "only gains");
        let down: Vec<f64> = (0..30).map(|i| 100.0 - i as f64).collect();
        assert!(rsi(&bars(&down), 14).unwrap() < 1e-9, "only losses");
        assert_eq!(rsi(&bars(&vec![50.0; 30]), 14), Some(50.0), "flat");
        assert_eq!(rsi(&bars(&up[..14]), 14), None, "needs period + 1 bars");
        // A known textbook-style check: alternating +1/-1 around a base is ~50.
        let alt: Vec<f64> = (0..60)
            .map(|i| 100.0 + if i % 2 == 0 { 0.0 } else { 1.0 })
            .collect();
        let v = rsi(&bars(&alt), 14).unwrap();
        assert!((v - 50.0).abs() < 5.0, "{v}");
    }

    #[test]
    fn rsi_stays_in_range_on_noisy_data() {
        let noisy: Vec<f64> = (0..200)
            .map(|i| 100.0 + ((i * 37 % 17) as f64) - 8.0)
            .collect();
        for p in [2usize, 5, 14, 50] {
            let v = rsi(&bars(&noisy), p).unwrap();
            assert!((0.0..=100.0).contains(&v), "period {p}: {v}");
        }
    }

    #[test]
    fn prior_channel_excludes_the_bar_being_judged() {
        // closes 10,11,12,13,50: the last bar spikes; its own high must not count.
        let b = bars(&[10.0, 11.0, 12.0, 13.0, 50.0]);
        assert_eq!(
            prior_high(&b, 4),
            Some(14.0),
            "highest of the 4 earlier bars (13 + 1)"
        );
        assert_eq!(prior_low(&b, 4), Some(9.0));
        assert_eq!(prior_high(&b, 2), Some(14.0));
        assert_eq!(prior_high(&b, 5), None, "needs n + 1 bars");
        assert_eq!(prior_low(&b, 0), None);
    }
}
