//! Synthetic chart of a list: one candle series built from many symbols.
//!
//! Two ways to combine (`A + B + C ...`):
//! - [`Mode::Index`] — every symbol is rebased to 100 at the start of the
//!   window and the series are averaged (equal weight).  Shows how the group
//!   moves together; a 60 000 USDT coin does not drown a 0.5 USDT one.
//! - [`Mode::Sum`] — raw prices added up, the literal `A+B+C`.
//!
//! Open / close are exact for the chosen formula.  High / low are the same
//! formula applied to each symbol's high / low, which is an upper / lower
//! **bound** of the true extremes of the composite (the symbols do not peak
//! at the same instant).  Pure functions, no I/O.

use std::collections::HashMap;

use crate::markets::Ohlc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Index,
    Sum,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Self::Index => Self::Sum,
            Self::Sum => Self::Index,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Index => "Index (equal weight, 100 = start)",
            Self::Sum => "Sum of prices",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Index => "Index",
            Self::Sum => "Sum",
        }
    }
}

pub struct Composite {
    pub candles: Vec<Ohlc>,
    /// How many symbols went into the series.
    pub used: usize,
    /// Symbols left out: no data, or history too short for the window.
    pub excluded: Vec<String>,
}

/// A symbol must cover at least this share of the timeline to be used.
const MIN_COVERAGE: f64 = 0.9;

/// Combine per-symbol candles into one series.  The timeline is the longest
/// input; a symbol missing a candle repeats its previous close (flat candle).
pub fn combine(series: &[(String, Vec<Ohlc>)], mode: Mode) -> Composite {
    let mut excluded: Vec<String> = Vec::new();
    let usable: Vec<&(String, Vec<Ohlc>)> = series
        .iter()
        .filter(|(sym, c)| {
            let ok = !c.is_empty() && c.iter().all(|k| k.time_ms > 0 && !k.is_gap);
            if !ok {
                excluded.push(sym.clone());
            }
            ok
        })
        .collect();
    let Some(reference) = usable.iter().max_by_key(|(_, c)| c.len()) else {
        return Composite {
            candles: Vec::new(),
            used: 0,
            excluded,
        };
    };
    let timeline: Vec<(i64, &str)> = reference
        .1
        .iter()
        .map(|k| (k.time_ms, k.date.as_str()))
        .collect();

    // Align every symbol on the timeline: Vec of (o, h, l, c, quote volume).
    type Row = (f64, f64, f64, f64, f64);
    let mut aligned: Vec<Vec<Row>> = Vec::new();
    for (sym, candles) in &usable {
        let by_time: HashMap<i64, &Ohlc> = candles.iter().map(|k| (k.time_ms, k)).collect();
        let present = timeline
            .iter()
            .filter(|(t, _)| by_time.contains_key(t))
            .count();
        if (present as f64) < MIN_COVERAGE * timeline.len() as f64 {
            excluded.push(sym.clone());
            continue;
        }
        let mut rows: Vec<Row> = Vec::with_capacity(timeline.len());
        let mut prev_close: Option<f64> = None;
        let first = by_time
            .get(
                &timeline
                    .iter()
                    .map(|(t, _)| *t)
                    .find(|t| by_time.contains_key(t))
                    .unwrap_or(0),
            )
            .map(|k| k.open)
            .unwrap_or(0.0);
        for (t, _) in &timeline {
            let row = match by_time.get(t) {
                Some(k) => (k.open, k.high, k.low, k.close, k.volume * k.close),
                None => {
                    let p = prev_close.unwrap_or(first);
                    (p, p, p, p, 0.0)
                }
            };
            prev_close = Some(row.3);
            rows.push(row);
        }
        if rows[0].0 <= 0.0 {
            excluded.push(sym.clone());
            continue;
        }
        aligned.push(rows);
    }
    let used = aligned.len();
    if used == 0 {
        return Composite {
            candles: Vec::new(),
            used: 0,
            excluded,
        };
    }

    let bases: Vec<f64> = aligned.iter().map(|r| r[0].0).collect();
    let n = used as f64;
    let candles = timeline
        .iter()
        .enumerate()
        .map(|(i, (t, date))| {
            let pick = |f: fn(&Row) -> f64| -> f64 {
                match mode {
                    Mode::Index => {
                        aligned
                            .iter()
                            .zip(&bases)
                            .map(|(r, b)| f(&r[i]) / b * 100.0)
                            .sum::<f64>()
                            / n
                    }
                    Mode::Sum => aligned.iter().map(|r| f(&r[i])).sum::<f64>(),
                }
            };
            Ohlc {
                open: pick(|r| r.0),
                high: pick(|r| r.1),
                low: pick(|r| r.2),
                close: pick(|r| r.3),
                volume: aligned.iter().map(|r| r[i].4).sum(),
                date: (*date).to_string(),
                time_ms: *t,
                is_gap: false,
            }
        })
        .collect();
    Composite {
        candles,
        used,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(t: i64, o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 2.0,
            date: format!("d{t}"),
            time_ms: t,
            is_gap: false,
        }
    }

    fn s(name: &str, v: Vec<Ohlc>) -> (String, Vec<Ohlc>) {
        (name.to_string(), v)
    }

    #[test]
    fn sum_adds_raw_prices() {
        let a = s(
            "A",
            vec![k(1, 10.0, 12.0, 9.0, 11.0), k(2, 11.0, 13.0, 10.0, 12.0)],
        );
        let b = s(
            "B",
            vec![
                k(1, 100.0, 120.0, 90.0, 110.0),
                k(2, 110.0, 130.0, 100.0, 120.0),
            ],
        );
        let c = combine(&[a, b], Mode::Sum);
        assert_eq!(c.used, 2);
        let first = &c.candles[0];
        assert_eq!(
            (first.open, first.high, first.low, first.close),
            (110.0, 132.0, 99.0, 121.0)
        );
        assert_eq!(first.time_ms, 1);
        assert_eq!(first.date, "d1");
    }

    #[test]
    fn index_rebases_each_symbol_to_100() {
        // A doubles, B stays: equal-weight index ends at (200 + 100) / 2.
        let a = s(
            "A",
            vec![k(1, 1.0, 1.0, 1.0, 1.0), k(2, 1.0, 2.0, 1.0, 2.0)],
        );
        let b = s(
            "B",
            vec![
                k(1, 5000.0, 5000.0, 5000.0, 5000.0),
                k(2, 5000.0, 5000.0, 5000.0, 5000.0),
            ],
        );
        let c = combine(&[a, b], Mode::Index);
        assert!((c.candles[0].open - 100.0).abs() < 1e-9, "starts at 100");
        assert!(
            (c.candles[1].close - 150.0).abs() < 1e-9,
            "{}",
            c.candles[1].close
        );
    }

    #[test]
    fn short_history_is_excluded_not_guessed() {
        let long: Vec<Ohlc> = (1..=10).map(|t| k(t, 1.0, 1.0, 1.0, 1.0)).collect();
        let short: Vec<Ohlc> = (8..=10).map(|t| k(t, 1.0, 1.0, 1.0, 1.0)).collect();
        let c = combine(
            &[s("L", long), s("NEW", short), s("EMPTY", vec![])],
            Mode::Index,
        );
        assert_eq!(c.used, 1);
        assert_eq!(c.excluded, vec!["EMPTY".to_string(), "NEW".to_string()]);
        assert_eq!(c.candles.len(), 10);
    }

    #[test]
    fn a_missing_candle_repeats_the_previous_close() {
        // 10 candles, B misses t=5: coverage 90% is still accepted.
        let a: Vec<Ohlc> = (1..=10).map(|t| k(t, 10.0, 10.0, 10.0, 10.0)).collect();
        let b: Vec<Ohlc> = (1..=10)
            .filter(|t| *t != 5)
            .map(|t| k(t, 20.0 + t as f64, 40.0, 1.0, 20.0 + t as f64))
            .collect();
        let c = combine(&[s("A", a), s("B", b)], Mode::Sum);
        assert_eq!(c.used, 2);
        // t=5: B repeats the close of t=4 (24) as a flat candle.
        assert_eq!(c.candles[4].close, 10.0 + 24.0);
        assert_eq!(c.candles[4].high, 10.0 + 24.0);
    }

    #[test]
    fn nothing_usable_gives_an_empty_composite() {
        let c = combine(&[], Mode::Index);
        assert!(c.candles.is_empty() && c.used == 0);
        let gap = Ohlc {
            is_gap: true,
            ..k(1, 0.0, 0.0, 0.0, 0.0)
        };
        let c = combine(
            &[s("G", vec![gap]), s("NT", vec![k(0, 1.0, 1.0, 1.0, 1.0)])],
            Mode::Sum,
        );
        assert!(c.candles.is_empty());
        assert_eq!(
            c.excluded.len(),
            2,
            "gap sentinels and unknown times are refused"
        );
    }

    #[test]
    fn mode_cycles() {
        assert_eq!(Mode::Index.next(), Mode::Sum);
        assert_eq!(Mode::Sum.next(), Mode::Index);
    }
}
