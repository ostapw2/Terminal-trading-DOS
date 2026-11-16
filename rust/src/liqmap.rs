//! Estimated liquidation levels: where open positions would be liquidated
//! if the price got there.  An ESTIMATE from public data, not the exchange's
//! book of positions (nobody outside the exchange has that).
//!
//! The model (the one Coinglass-style maps use, kept simple and stated):
//!   * every period, a rise in open interest is new positions opened at that
//!     period's price — as many longs as shorts (each contract has both);
//!   * they are spread over typical leverages ([`LEVERAGES`]); a position
//!     at leverage `L` entered at `p` is liquidated near
//!     `p·(1 − 1/L + m)` (long) or `p·(1 + 1/L − m)` (short), `m` the
//!     maintenance margin;
//!   * a fall in open interest closes positions, all in proportion;
//!   * once the price has traded through a level, its positions are gone.
//!
//! What is left is the "fuel" still waiting: longs below the price, shorts
//! above it.  Real leverage choices, cross margin and partial closes are
//! unknown, so read the result as where liquidations are LIKELY to cluster.

use crate::markets::Ohlc;

/// Leverage and the share of new positions assumed to take it.
pub const LEVERAGES: [(f64, f64); 5] = [
    (5.0, 0.15),
    (10.0, 0.30),
    (25.0, 0.30),
    (50.0, 0.15),
    (100.0, 0.10),
];
/// Maintenance margin rate (Binance's lowest tier for the big coins).
pub const MAINT: f64 = 0.004;
/// Bucket width as a share of the last price.
const BUCKET: f64 = 0.001;

/// One price bucket: the dollars that would be liquidated there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    /// The bucket's middle.
    pub price: f64,
    pub long_usd: f64,
    pub short_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiqMap {
    pub symbol: String,
    /// Bucket width in price.
    pub step: f64,
    /// Sorted by price.
    pub levels: Vec<Level>,
    /// The price the map was made at.
    pub price: f64,
    /// The data it covers.
    pub from_ms: i64,
    pub to_ms: i64,
}

impl LiqMap {
    /// Longs (below the price) and shorts (above) still waiting, in USD.
    pub fn totals(&self) -> (f64, f64) {
        self.levels
            .iter()
            .fold((0.0, 0.0), |(l, s), x| (l + x.long_usd, s + x.short_usd))
    }
}

struct Position {
    liq: f64,
    coins: f64,
    long: bool,
}

pub fn long_liq_price(entry: f64, lev: f64) -> f64 {
    entry * (1.0 - 1.0 / lev + MAINT)
}

pub fn short_liq_price(entry: f64, lev: f64) -> f64 {
    entry * (1.0 + 1.0 / lev - MAINT)
}

/// Build the map from `candles` and open interest `oi` (coins, `close`),
/// both oldest first on the same periods.  Readings without a candle at
/// their time are skipped.  `None` when there is too little to say anything.
pub fn estimate(symbol: &str, candles: &[Ohlc], oi: &[Ohlc]) -> Option<LiqMap> {
    let candles: Vec<&Ohlc> = candles.iter().filter(|c| !c.is_gap).collect();
    let price = candles.last()?.close;
    if oi.len() < 2 || price.is_nan() || price <= 0.0 {
        return None;
    }
    let mut open: Vec<Position> = Vec::new();
    let mut prev_oi: Option<f64> = None;
    let mut next = 0usize; // first candle not yet applied
    for r in oi {
        // Candles up to this reading wipe the levels they traded through.
        while next < candles.len() && candles[next].time_ms <= r.time_ms {
            let c = candles[next];
            open.retain(|p| {
                if p.long {
                    c.low > p.liq
                } else {
                    c.high < p.liq
                }
            });
            next += 1;
        }
        let Some(at) = next.checked_sub(1).map(|i| candles[i]) else {
            continue;
        };
        let now = r.close;
        if !(now.is_finite() && now >= 0.0) {
            continue;
        }
        if let Some(before) = prev_oi {
            let d = now - before;
            if d > 0.0 {
                let entry = at.close;
                for (lev, share) in LEVERAGES {
                    let coins = d * share;
                    open.push(Position {
                        liq: long_liq_price(entry, lev),
                        coins,
                        long: true,
                    });
                    open.push(Position {
                        liq: short_liq_price(entry, lev),
                        coins,
                        long: false,
                    });
                }
            } else if d < 0.0 && before > 0.0 {
                let keep = (now / before).clamp(0.0, 1.0);
                for p in &mut open {
                    p.coins *= keep;
                }
            }
        }
        prev_oi = Some(now);
    }
    // The candles after the last reading.
    for c in &candles[next..] {
        open.retain(|p| {
            if p.long {
                c.low > p.liq
            } else {
                c.high < p.liq
            }
        });
    }
    let step = price * BUCKET;
    let mut buckets: std::collections::BTreeMap<i64, (f64, f64)> = Default::default();
    for p in open.iter().filter(|p| p.coins > 0.0) {
        // Longs above the price or shorts below it were wiped already.
        if (p.long && p.liq >= price) || (!p.long && p.liq <= price) {
            continue;
        }
        let k = (p.liq / step).floor() as i64;
        let e = buckets.entry(k).or_default();
        if p.long {
            e.0 += p.coins * p.liq;
        } else {
            e.1 += p.coins * p.liq;
        }
    }
    Some(LiqMap {
        symbol: symbol.to_string(),
        step,
        levels: buckets
            .into_iter()
            .map(|(k, (l, s))| Level {
                price: (k as f64 + 0.5) * step,
                long_usd: l,
                short_usd: s,
            })
            .collect(),
        price,
        from_ms: oi.first().map_or(0, |r| r.time_ms),
        to_ms: candles.last().map_or(0, |c| c.time_ms),
    })
}

/// Longs below and shorts above within `pct` % of the price, USD.
pub fn within(map: &LiqMap, pct: f64) -> (f64, f64) {
    let (lo, hi) = (
        map.price * (1.0 - pct / 100.0),
        map.price * (1.0 + pct / 100.0),
    );
    map.levels.iter().fold((0.0, 0.0), |(l, s), x| {
        (
            l + if x.price >= lo && x.price < map.price {
                x.long_usd
            } else {
                0.0
            },
            s + if x.price <= hi && x.price > map.price {
                x.short_usd
            } else {
                0.0
            },
        )
    })
}

/// One line for the order book: what a 1 % move would liquidate and the
/// biggest cluster within 5 %.
pub fn summary(map: &LiqMap) -> String {
    use crate::data::liquidations::fmt_usd;
    let (l, s) = within(map, 1.0);
    let biggest = map
        .levels
        .iter()
        .filter(|x| (x.price / map.price - 1.0).abs() <= 0.05)
        .map(|x| (x.price, x.long_usd.max(x.short_usd)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, usd)| *usd > 0.0);
    let mut out = format!(
        "est. liq within 1%: longs {} below, shorts {} above",
        fmt_usd(l),
        fmt_usd(s)
    );
    if let Some((p, usd)) = biggest {
        out.push_str(&format!(
            "; biggest {} at {} ({:+.1}%)",
            fmt_usd(usd),
            crate::widgets::fmt::fmt_price(p),
            (p / map.price - 1.0) * 100.0
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(t: i64, low: f64, high: f64, close: f64) -> Ohlc {
        Ohlc {
            time_ms: t,
            open: close,
            high,
            low,
            close,
            ..Default::default()
        }
    }

    fn r(t: i64, v: f64) -> Ohlc {
        Ohlc {
            time_ms: t,
            close: v,
            ..Default::default()
        }
    }

    #[test]
    fn liquidation_prices_follow_leverage() {
        assert!((long_liq_price(100.0, 10.0) - 90.4).abs() < 1e-9);
        assert!((short_liq_price(100.0, 10.0) - 109.6).abs() < 1e-9);
        assert!(long_liq_price(100.0, 100.0) > long_liq_price(100.0, 5.0));
    }

    #[test]
    fn new_interest_puts_longs_below_and_shorts_above() {
        let candles = [c(0, 99.9, 100.1, 100.0), c(1, 99.9, 100.1, 100.0)];
        let oi = [r(0, 1000.0), r(1, 1010.0)];
        let m = estimate("X", &candles, &oi).unwrap();
        let (longs, shorts) = m.totals();
        assert!(longs > 0.0 && shorts > 0.0);
        assert!(m
            .levels
            .iter()
            .all(|l| l.long_usd == 0.0 || l.price < 100.0));
        assert!(m
            .levels
            .iter()
            .all(|l| l.short_usd == 0.0 || l.price > 100.0));
        // 10 coins each side, all leverages: the 100x long sits near 99.4.
        let near = m
            .levels
            .iter()
            .find(|l| (l.price - long_liq_price(100.0, 100.0)).abs() < m.step)
            .unwrap();
        assert!((near.long_usd - 10.0 * 0.10 * long_liq_price(100.0, 100.0)).abs() < 1e-6);
        assert!(m.levels.windows(2).all(|w| w[0].price < w[1].price));
    }

    #[test]
    fn a_move_through_a_level_wipes_it_and_falling_interest_shrinks_the_rest() {
        let oi = [r(0, 1000.0), r(1, 1010.0), r(2, 1010.0)];
        let calm = [
            c(0, 100.0, 100.0, 100.0),
            c(1, 100.0, 100.0, 100.0),
            c(2, 100.0, 100.0, 100.0),
        ];
        let base = estimate("X", &calm, &oi).unwrap().totals();
        // After the entry the price dips to 98: the 100x and 50x longs (99.4,
        // 98.4) are gone, the 25x one (96.4) stays.
        let dip = [
            c(0, 100.0, 100.0, 100.0),
            c(1, 100.0, 100.0, 100.0),
            c(2, 98.0, 100.0, 100.0),
        ];
        let after = estimate("X", &dip, &oi).unwrap().totals();
        let lost =
            10.0 * 0.10 * long_liq_price(100.0, 100.0) + 10.0 * 0.15 * long_liq_price(100.0, 50.0);
        assert!((base.0 - after.0 - lost).abs() < 1e-6, "{base:?} {after:?}");
        assert!((base.1 - after.1).abs() < 1e-9, "shorts untouched");
        // Open interest falls by 5 coins: every position shrinks in proportion.
        let oi_down = [r(0, 1000.0), r(1, 1010.0), r(2, 1005.0)];
        let less = estimate("X", &calm, &oi_down).unwrap().totals();
        assert!((less.0 - base.0 * 1005.0 / 1010.0).abs() < 1e-6);
    }

    #[test]
    fn too_little_data_says_nothing() {
        assert!(estimate("X", &[], &[r(0, 1.0), r(1, 2.0)]).is_none());
        assert!(estimate("X", &[c(0, 1.0, 1.0, 1.0)], &[r(0, 1.0)]).is_none());
    }

    #[test]
    fn summary_counts_one_percent_and_names_the_biggest() {
        let lv = |price: f64, l: f64, s: f64| Level {
            price,
            long_usd: l,
            short_usd: s,
        };
        let map = LiqMap {
            symbol: "X".into(),
            step: 0.1,
            levels: vec![
                lv(97.0, 9e6, 0.0),
                lv(99.5, 2e6, 0.0),
                lv(100.5, 0.0, 3e6),
                lv(120.0, 0.0, 5e7),
            ],
            price: 100.0,
            from_ms: 0,
            to_ms: 0,
        };
        assert_eq!(within(&map, 1.0), (2e6, 3e6));
        let s = summary(&map);
        assert!(s.contains("longs $2.0M below, shorts $3.0M above"), "{s}");
        assert!(
            s.contains("biggest $9.0M at") && s.contains("(-3.0%)"),
            "{s}"
        );
    }
}
