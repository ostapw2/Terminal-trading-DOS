//! Footprint / volume-at-price aggregation from tick-level aggTrades.
//!
//! Each footprint candle holds, for one timeframe bucket, the volume of
//! aggressive BUYS and aggressive SELLS at every price level (rounded to
//! `tick_size`).  Built from `binance::AggTrade` stream:
//!
//!   * `is_buyer_maker == true`  → buyer was passive; SELLER hit the bid →
//!     this trade is an aggressive SELL → adds to `sell_volume`.
//!   * `is_buyer_maker == false` → seller was passive; BUYER lifted the ask
//!     → aggressive BUY → adds to `buy_volume`.
//!
//! POC (point of control) = price level with highest total volume.

use std::collections::BTreeMap;

use crate::data::binance::AggTrade;
use crate::markets::{Ohlc, Timeframe};

/// One price level inside a footprint candle.
#[derive(Clone, Debug)]
pub struct PriceLevel {
    pub price: f64,
    pub buy_volume: f64,
    pub sell_volume: f64,
}

impl PriceLevel {
    pub fn total(&self) -> f64 {
        self.buy_volume + self.sell_volume
    }
    pub fn delta(&self) -> f64 {
        self.buy_volume - self.sell_volume
    }
}

/// One candle augmented with per-price-level breakdown.
#[derive(Clone, Debug)]
pub struct FootprintCandle {
    pub bucket_start_ms: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub levels: Vec<PriceLevel>, // sorted ascending by price
    pub total_buy: f64,
    pub total_sell: f64,
    pub poc: f64, // price level with max total volume
}

impl FootprintCandle {
    pub fn delta(&self) -> f64 {
        self.total_buy - self.total_sell
    }
}

/// Round a price to the nearest multiple of `tick_size`.  Real Binance
/// trade prices are exact ticks; using `round` (not `floor`) recovers the
/// integer multiplier despite IEEE float representation errors.
fn bucket(price: f64, tick_size: f64) -> f64 {
    if tick_size <= 0.0 {
        return price;
    }
    (price / tick_size).round() * tick_size
}

/// Convert a UNIX millisecond timestamp to the start of its TF bucket.
fn bucket_start(time_ms: i64, tf_secs: u64) -> i64 {
    if tf_secs == 0 {
        return time_ms;
    }
    let tf_ms = (tf_secs * 1000) as i64;
    (time_ms / tf_ms) * tf_ms
}

/// Build per-candle footprint from tick-level aggTrades.  Returns one entry
/// per TF bucket that contains at least one trade, ordered oldest → newest.
/// `tick_size` controls price-level rounding (use Binance `PRICE_FILTER`).
pub fn build_from_trades(
    trades: &[AggTrade],
    tf: Timeframe,
    tick_size: f64,
) -> Vec<FootprintCandle> {
    if trades.is_empty() {
        return Vec::new();
    }
    let tf_secs = tf.duration_secs();
    let mut buckets: BTreeMap<i64, BTreeMap<i64, PriceLevel>> = BTreeMap::new();
    let mut ohlc: BTreeMap<i64, (f64, f64, f64, f64, i64)> = BTreeMap::new();
    // bucket_start → (open, high, low, close, last_time_ms)
    let scale = if tick_size > 0.0 {
        (1.0 / tick_size).max(1.0)
    } else {
        1.0
    };

    for t in trades {
        let bs = bucket_start(t.time_ms, tf_secs);
        let p = bucket(t.price, tick_size);
        let key = (p * scale).round() as i64;
        let level = buckets
            .entry(bs)
            .or_default()
            .entry(key)
            .or_insert_with(|| PriceLevel {
                price: p,
                buy_volume: 0.0,
                sell_volume: 0.0,
            });
        if t.is_buyer_maker {
            level.sell_volume += t.qty;
        } else {
            level.buy_volume += t.qty;
        }
        let entry = ohlc
            .entry(bs)
            .or_insert((t.price, t.price, t.price, t.price, t.time_ms));
        if t.time_ms < entry.4 {
            entry.0 = t.price;
        }
        if t.price > entry.1 {
            entry.1 = t.price;
        }
        if t.price < entry.2 {
            entry.2 = t.price;
        }
        // Latest (newest time) wins for close.
        if t.time_ms >= entry.4 {
            entry.3 = t.price;
            entry.4 = t.time_ms;
        }
    }

    let mut out: Vec<FootprintCandle> = Vec::with_capacity(buckets.len());
    for (bs, level_map) in buckets {
        let levels: Vec<PriceLevel> = level_map.into_values().collect();
        let total_buy: f64 = levels.iter().map(|l| l.buy_volume).sum();
        let total_sell: f64 = levels.iter().map(|l| l.sell_volume).sum();
        let poc = levels
            .iter()
            .max_by(|a, b| {
                a.total()
                    .partial_cmp(&b.total())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|l| l.price)
            .unwrap_or(0.0);
        let (o, h, l, c, _) = ohlc.get(&bs).copied().unwrap_or((0.0, 0.0, 0.0, 0.0, 0));
        out.push(FootprintCandle {
            bucket_start_ms: bs,
            o,
            h,
            l,
            c,
            levels,
            total_buy,
            total_sell,
            poc,
        });
    }
    out
}

impl FootprintCandle {
    /// A candle with no tick data: OHLC only, no levels.  Rendered as a blank
    /// column — never as an invented volume distribution.
    pub fn empty(bucket_start_ms: i64, o: &Ohlc) -> Self {
        FootprintCandle {
            bucket_start_ms,
            o: o.open,
            h: o.high,
            l: o.low,
            c: o.close,
            levels: Vec::new(),
            total_buy: 0.0,
            total_sell: 0.0,
            poc: 0.0,
        }
    }

    pub fn has_data(&self) -> bool {
        !self.levels.is_empty()
    }
}

/// Merge `parts` (any number, oldest first) into one candle keyed `start_ms`,
/// summing volume per price level.  OHLC comes from `ohlc`, the canonical
/// kline.  No parts → an empty candle.
fn merge(start_ms: i64, ohlc: &Ohlc, parts: &[&FootprintCandle]) -> FootprintCandle {
    let mut by_price: BTreeMap<i64, PriceLevel> = BTreeMap::new();
    for fc in parts {
        for lvl in &fc.levels {
            // Prices are exact ticks; 1e-8 resolution is plenty to key them.
            let key = (lvl.price * 1e8).round() as i64;
            let e = by_price.entry(key).or_insert_with(|| PriceLevel {
                price: lvl.price,
                buy_volume: 0.0,
                sell_volume: 0.0,
            });
            e.buy_volume += lvl.buy_volume;
            e.sell_volume += lvl.sell_volume;
        }
    }
    let levels: Vec<PriceLevel> = by_price.into_values().collect();
    let total_buy = levels.iter().map(|l| l.buy_volume).sum();
    let total_sell = levels.iter().map(|l| l.sell_volume).sum();
    let poc = levels
        .iter()
        .max_by(|a, b| {
            a.total()
                .partial_cmp(&b.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|l| l.price)
        .unwrap_or(0.0);
    FootprintCandle {
        bucket_start_ms: start_ms,
        o: ohlc.open,
        h: ohlc.high,
        l: ohlc.low,
        c: ohlc.close,
        levels,
        total_buy,
        total_sell,
        poc,
    }
}

/// Duration (ms) of one kline column for `tf`.  Providers have no 15 s kline
/// and serve 1 m for it, so a 15 s timeframe still has 60 s columns.
fn kline_ms(tf: Timeframe) -> i64 {
    (tf.duration_secs().max(60) * 1000) as i64
}

/// Attach real footprint data to klines by TIME: a kline gets the trades whose
/// bucket falls inside its `[time_ms, time_ms + kline_len)` window.  Klines
/// with no tick data (older candles, gaps in the trade stream) get an EMPTY
/// footprint — nothing is invented.  Output is 1:1 with `klines`.
pub fn align_to_klines(
    klines: &[Ohlc],
    real: &[FootprintCandle],
    tf: Timeframe,
) -> Vec<FootprintCandle> {
    let len = kline_ms(tf);
    klines
        .iter()
        .map(|k| {
            if k.is_gap || k.time_ms <= 0 {
                return FootprintCandle::empty(k.time_ms, k);
            }
            let parts: Vec<&FootprintCandle> = real
                .iter()
                .filter(|fc| {
                    fc.bucket_start_ms >= k.time_ms && fc.bucket_start_ms < k.time_ms + len
                })
                .collect();
            merge(k.time_ms, k, &parts)
        })
        .collect()
}

/// Re-key an aligned footprint onto a transformed OHLC series (date/session
/// filters, day gaps, zoom-out aggregation): each `working` candle takes all
/// footprint candles from its own start up to the next real candle's start.
/// Output is 1:1 with `working`, so footprint columns, axis, separators,
/// scrollbar and hover all index the same series (audit W2).
pub fn project_onto(working: &[Ohlc], aligned: &[FootprintCandle]) -> Vec<FootprintCandle> {
    // `aligned` is time-ordered (it mirrors the kline order).
    let mut out = Vec::with_capacity(working.len());
    let mut next_real: Vec<Option<i64>> = vec![None; working.len()];
    let mut upcoming: Option<i64> = None;
    for (i, w) in working.iter().enumerate().rev() {
        next_real[i] = upcoming;
        if !w.is_gap && w.time_ms > 0 {
            upcoming = Some(w.time_ms);
        }
    }
    for (i, w) in working.iter().enumerate() {
        if w.is_gap || w.time_ms <= 0 {
            out.push(FootprintCandle::empty(w.time_ms, w));
            continue;
        }
        let end = next_real[i].unwrap_or(i64::MAX);
        let parts: Vec<&FootprintCandle> = aligned
            .iter()
            .filter(|fc| fc.bucket_start_ms >= w.time_ms && fc.bucket_start_ms < end)
            .collect();
        out.push(merge(w.time_ms, w, &parts));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(price: f64, qty: f64, time_ms: i64, is_buyer_maker: bool) -> AggTrade {
        AggTrade {
            price,
            qty,
            time_ms,
            is_buyer_maker,
        }
    }

    #[test]
    fn aggregates_buys_and_sells_per_price_level() {
        let trades = vec![
            t(60000.10, 1.0, 1_000, false), // buy
            t(60000.10, 0.5, 2_000, true),  // sell at same price
            t(60000.20, 2.0, 3_000, false), // buy at higher price
            t(60000.10, 0.3, 4_000, false), // another buy at first level
        ];
        let footprints = build_from_trades(&trades, Timeframe::M1, 0.10);
        assert_eq!(footprints.len(), 1);
        let fp = &footprints[0];
        assert_eq!(fp.levels.len(), 2);
        let l0 = fp
            .levels
            .iter()
            .find(|l| (l.price - 60000.10).abs() < 1e-6)
            .unwrap();
        assert!((l0.buy_volume - 1.3).abs() < 1e-6);
        assert!((l0.sell_volume - 0.5).abs() < 1e-6);
        let l1 = fp
            .levels
            .iter()
            .find(|l| (l.price - 60000.20).abs() < 1e-6)
            .unwrap();
        assert!((l1.buy_volume - 2.0).abs() < 1e-6);
        assert_eq!(l1.sell_volume, 0.0);
        assert!((fp.total_buy - 3.3).abs() < 1e-6);
        assert!((fp.total_sell - 0.5).abs() < 1e-6);
    }

    #[test]
    fn empty_trades_returns_empty() {
        let v = build_from_trades(&[], Timeframe::M1, 0.10);
        assert!(v.is_empty());
    }

    #[test]
    fn poc_is_highest_volume_price() {
        let trades = vec![
            t(100.0, 1.0, 1_000, false),
            t(101.0, 5.0, 2_000, false), // POC
            t(102.0, 2.0, 3_000, true),
        ];
        let fps = build_from_trades(&trades, Timeframe::M1, 1.0);
        assert_eq!(fps.len(), 1);
        assert!((fps[0].poc - 101.0).abs() < 1e-6);
    }

    fn kline(time_ms: i64, close: f64) -> Ohlc {
        Ohlc {
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            date: String::new(),
            time_ms,
            is_gap: false,
        }
    }

    #[test]
    fn align_matches_by_time_not_by_close() {
        // Two 1m klines with the SAME close but different trades.
        let trades = vec![
            t(100.0, 1.0, 60_000, false), // minute 1: buy 1
            t(100.0, 2.0, 120_000, true), // minute 2: sell 2
        ];
        let real = build_from_trades(&trades, Timeframe::M1, 1.0);
        let klines = vec![kline(60_000, 100.0), kline(120_000, 100.0)];
        let aligned = align_to_klines(&klines, &real, Timeframe::M1);
        assert_eq!(aligned.len(), 2);
        assert!((aligned[0].total_buy - 1.0).abs() < 1e-9 && aligned[0].total_sell == 0.0);
        assert!((aligned[1].total_sell - 2.0).abs() < 1e-9 && aligned[1].total_buy == 0.0);
    }

    #[test]
    fn unmatched_klines_are_empty_not_invented() {
        let klines = vec![kline(60_000, 100.0)];
        let aligned = align_to_klines(&klines, &[], Timeframe::M1);
        assert!(!aligned[0].has_data());
        assert_eq!(aligned[0].total_buy + aligned[0].total_sell, 0.0);
    }

    #[test]
    fn fifteen_second_buckets_merge_into_minute_klines() {
        let trades = vec![
            t(100.0, 1.0, 60_000, false),
            t(100.0, 1.0, 75_000, false),
            t(100.0, 1.0, 90_000, false),
        ];
        let real = build_from_trades(&trades, Timeframe::S15, 1.0);
        assert_eq!(real.len(), 3);
        let aligned = align_to_klines(&[kline(60_000, 100.0)], &real, Timeframe::S15);
        assert!((aligned[0].total_buy - 3.0).abs() < 1e-9);
    }

    #[test]
    fn project_onto_follows_aggregation_and_gaps() {
        let klines = vec![kline(60_000, 1.0), kline(120_000, 1.0), kline(180_000, 1.0)];
        let trades = vec![
            t(1.0, 1.0, 60_000, false),
            t(1.0, 1.0, 120_000, false),
            t(1.0, 1.0, 180_000, false),
        ];
        let aligned = align_to_klines(
            &klines,
            &build_from_trades(&trades, Timeframe::M1, 1.0),
            Timeframe::M1,
        );
        // Working series: [candle@60k+120k merged] [gap] [candle@180k].
        let working = vec![
            kline(60_000, 1.0),
            Ohlc {
                is_gap: true,
                ..Ohlc::default()
            },
            kline(180_000, 1.0),
        ];
        let p = project_onto(&working, &aligned);
        assert_eq!(p.len(), 3);
        assert!(
            (p[0].total_buy - 2.0).abs() < 1e-9,
            "first column spans two klines"
        );
        assert!(!p[1].has_data());
        assert!((p[2].total_buy - 1.0).abs() < 1e-9);
    }
}
