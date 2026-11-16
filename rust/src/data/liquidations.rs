//! Liquidations: orders the exchange forced on traders whose margin ran out.
//!
//! Source: Binance USDⓈ-M Futures `!forceOrder@arr` (free, every symbol).
//! Binance sends at most ONE liquidation per symbol per second — the largest
//! one — so every total here is a lower bound, never the whole picture.
//!
//! A long that is liquidated becomes a forced SELL; a short, a forced BUY.
//! Spot has no liquidations: a Spot chart shows those of the Futures symbol
//! with the same name (prices differ by the basis, a few hundredths of a
//! percent).

use std::collections::{HashMap, VecDeque};

use crate::markets::Ohlc;

/// How far back liquidations are kept (memory and database).
pub const KEEP_MS: i64 = 3 * 24 * 3_600_000;
/// Most liquidations kept per symbol.
const PER_SYMBOL_MAX: usize = 20_000;
/// Most rows of the market-wide feed (a busy day is tens of thousands).
const FEED_MAX: usize = 200_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Liquidation {
    pub symbol: String,
    /// `true`: a long was liquidated (forced sell).
    pub long: bool,
    /// Average fill price (the order price when nothing filled yet).
    pub price: f64,
    /// Filled quantity (the order quantity when nothing filled yet).
    pub qty: f64,
    pub time_ms: i64,
}

impl Liquidation {
    pub fn usd(&self) -> f64 {
        self.price * self.qty
    }

    fn same(&self, o: &Liquidation) -> bool {
        self.time_ms == o.time_ms
            && self.long == o.long
            && self.price == o.price
            && self.qty == o.qty
            && self.symbol == o.symbol
    }
}

fn num(v: &serde_json::Value, key: &str) -> Option<f64> {
    let x = v.get(key)?;
    x.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| x.as_f64())
}

fn parse_order(o: &serde_json::Value) -> Option<Liquidation> {
    let symbol = o.get("s")?.as_str()?.to_ascii_uppercase();
    let long = match o.get("S")?.as_str()? {
        "SELL" => true,
        "BUY" => false,
        _ => return None,
    };
    let filled = num(o, "z").filter(|q| *q > 0.0);
    let avg = num(o, "ap").filter(|p| *p > 0.0);
    let (price, qty) = match (avg, filled) {
        (Some(p), Some(q)) => (p, q),
        _ => (num(o, "p")?, num(o, "q")?),
    };
    if !(price > 0.0 && qty > 0.0 && price.is_finite() && qty.is_finite()) {
        return None;
    }
    let time_ms = o.get("T").and_then(|t| t.as_i64())?;
    Some(Liquidation {
        symbol,
        long,
        price,
        qty,
        time_ms,
    })
}

/// One `forceOrder` message: an event, an array of events, or a combined
/// stream wrapper (`{"stream":..,"data":..}`).
pub fn parse_force_orders(text: &str) -> Vec<Liquidation> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let v = v.get("data").cloned().unwrap_or(v);
    let events: Vec<&serde_json::Value> = match v.as_array() {
        Some(a) => a.iter().collect(),
        None => vec![&v],
    };
    events
        .into_iter()
        .filter_map(|e| parse_order(e.get("o")?))
        .collect()
}

/// Liquidations heard this run (and loaded from the database at start).
#[derive(Default)]
pub struct LiqStore {
    by_symbol: HashMap<String, VecDeque<Liquidation>>,
    /// Every symbol, newest last.
    feed: VecDeque<Liquidation>,
    /// Heard live, not yet written to the database.
    unsaved: Vec<Liquidation>,
    /// When the newest live one arrived (wall clock, ms).
    pub last_live_ms: Option<i64>,
}

impl LiqStore {
    /// Add one.  `live` ones are also queued for the database.  Repeats (the
    /// same event loaded and heard, or written by another window) are dropped.
    pub fn push(&mut self, l: Liquidation, live: bool) {
        let list = self.by_symbol.entry(l.symbol.clone()).or_default();
        if list.iter().rev().take(16).any(|o| o.same(&l)) {
            return;
        }
        // Keep each list in time order (a late one slides into place).
        let at = list
            .iter()
            .rposition(|o| o.time_ms <= l.time_ms)
            .map_or(0, |i| i + 1);
        list.insert(at, l.clone());
        if list.len() > PER_SYMBOL_MAX {
            list.pop_front();
        }
        let at = self
            .feed
            .iter()
            .rposition(|o| o.time_ms <= l.time_ms)
            .map_or(0, |i| i + 1);
        self.feed.insert(at, l.clone());
        if self.feed.len() > FEED_MAX {
            self.feed.pop_front();
        }
        if live {
            self.last_live_ms = Some(l.time_ms.max(self.last_live_ms.unwrap_or(0)));
            self.unsaved.push(l);
        }
    }

    /// This symbol's liquidations, oldest first (Spot and Futures share a name).
    pub fn of(&self, symbol: &str) -> Option<&VecDeque<Liquidation>> {
        self.by_symbol.get(symbol)
    }

    /// Every symbol, oldest first.
    pub fn feed(&self) -> &VecDeque<Liquidation> {
        &self.feed
    }

    pub fn take_unsaved(&mut self) -> Vec<Liquidation> {
        std::mem::take(&mut self.unsaved)
    }

    /// Drop what is older than [`KEEP_MS`].
    pub fn prune(&mut self, now_ms: i64) {
        let cut = now_ms - KEEP_MS;
        for list in self.by_symbol.values_mut() {
            while list.front().is_some_and(|l| l.time_ms < cut) {
                list.pop_front();
            }
        }
        self.by_symbol.retain(|_, l| !l.is_empty());
        while self.feed.front().is_some_and(|l| l.time_ms < cut) {
            self.feed.pop_front();
        }
    }
}

/// Totals over liquidations at or after `since_ms`.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub long_usd: f64,
    pub short_usd: f64,
    pub count: usize,
}

pub fn totals<'a>(list: impl IntoIterator<Item = &'a Liquidation>, since_ms: i64) -> Totals {
    let mut t = Totals::default();
    for l in list.into_iter().filter(|l| l.time_ms >= since_ms) {
        if l.long {
            t.long_usd += l.usd();
        } else {
            t.short_usd += l.usd();
        }
        t.count += 1;
    }
    t
}

/// A liquidation placed on a chart: the visible candle it happened in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiqMark {
    /// Index into the visible slice.
    pub col: usize,
    pub price: f64,
    pub usd: f64,
    pub long: bool,
}

/// Place `liqs` (oldest first) on the visible candles: candle `i` covers
/// `[t_i, t_next)`, the last one a candle's length.  Gap columns take none;
/// those outside the window are left out.
pub fn marks<'a>(slice: &[Ohlc], liqs: impl IntoIterator<Item = &'a Liquidation>) -> Vec<LiqMark> {
    let cols: Vec<(usize, i64)> = slice
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.is_gap)
        .map(|(i, c)| (i, c.time_ms))
        .collect();
    let Some(&(_, first)) = cols.first() else {
        return Vec::new();
    };
    // A candle's length: the shortest step between neighbours.
    let span = cols
        .windows(2)
        .map(|w| w[1].1 - w[0].1)
        .filter(|d| *d > 0)
        .min();
    let Some(span) = span else {
        return Vec::new();
    };
    let end = cols.last().map_or(first, |c| c.1) + span;
    let mut out = Vec::new();
    for l in liqs {
        if l.time_ms < first || l.time_ms >= end {
            continue;
        }
        // The last candle starting at or before it.
        let k = cols.partition_point(|c| c.1 <= l.time_ms);
        let Some(&(col, start)) = k.checked_sub(1).and_then(|k| cols.get(k)) else {
            continue;
        };
        // A candle followed by a gap still ends a candle's length later.
        if l.time_ms >= start + span && cols.get(k).is_some_and(|n| n.1 > start + span) {
            continue;
        }
        out.push(LiqMark {
            col,
            price: l.price,
            usd: l.usd(),
            long: l.long,
        });
    }
    out
}

/// `$1.2M`, `$350K`, `$900`.
pub fn fmt_usd(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("${:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("${:.1}M", v / 1e6)
    } else if a >= 1e3 {
        format!("${:.0}K", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = r#"{"e":"forceOrder","E":1568014460893,"o":{"s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.014","p":"9910","ap":"9910.5","X":"FILLED","l":"0.014","z":"0.014","T":1568014460893}}"#;

    fn liq(sym: &str, long: bool, price: f64, qty: f64, t: i64) -> Liquidation {
        Liquidation {
            symbol: sym.into(),
            long,
            price,
            qty,
            time_ms: t,
        }
    }

    #[test]
    fn parses_event_array_and_wrapper() {
        let one = parse_force_orders(EVENT);
        assert_eq!(
            one,
            vec![liq("BTCUSDT", true, 9910.5, 0.014, 1568014460893)]
        );
        let arr = format!("[{EVENT},{}]", EVENT.replace("SELL", "BUY"));
        let two = parse_force_orders(&arr);
        assert_eq!(two.len(), 2);
        assert!(two[0].long && !two[1].long);
        let wrapped = format!(r#"{{"stream":"!forceOrder@arr","data":{EVENT}}}"#);
        assert_eq!(parse_force_orders(&wrapped).len(), 1);
        assert!(parse_force_orders("not json").is_empty());
        assert!(parse_force_orders(r#"{"e":"forceOrder"}"#).is_empty());
    }

    #[test]
    fn unfilled_order_uses_order_price_and_qty() {
        let text = EVENT
            .replace(r#""ap":"9910.5""#, r#""ap":"0""#)
            .replace(r#""z":"0.014""#, r#""z":"0""#);
        let l = &parse_force_orders(&text)[0];
        assert_eq!((l.price, l.qty), (9910.0, 0.014));
        let bad = EVENT.replace(r#""S":"SELL""#, r#""S":"X""#);
        assert!(parse_force_orders(&bad).is_empty());
    }

    #[test]
    fn store_dedups_orders_and_prunes() {
        let mut s = LiqStore::default();
        s.push(liq("BTCUSDT", true, 100.0, 1.0, 2_000), true);
        s.push(liq("BTCUSDT", true, 100.0, 1.0, 2_000), true);
        s.push(liq("BTCUSDT", false, 101.0, 1.0, 1_000), false);
        s.push(liq("ETHUSDT", true, 10.0, 3.0, 1_500), true);
        let btc = s.of("BTCUSDT").unwrap();
        assert_eq!(btc.len(), 2);
        assert_eq!(btc[0].time_ms, 1_000, "a late one slides into place");
        let times: Vec<i64> = s.feed().iter().map(|l| l.time_ms).collect();
        assert_eq!(times, vec![1_000, 1_500, 2_000]);
        assert_eq!(s.take_unsaved().len(), 2, "only live ones, once");
        assert!(s.take_unsaved().is_empty());
        s.prune(1_600 + KEEP_MS);
        assert_eq!(s.of("BTCUSDT").unwrap().len(), 1);
        assert!(s.of("ETHUSDT").is_none());
        assert_eq!(s.feed().len(), 1);
    }

    #[test]
    fn totals_split_longs_and_shorts() {
        let l = [
            liq("A", true, 10.0, 2.0, 5),
            liq("A", false, 10.0, 1.0, 6),
            liq("A", true, 10.0, 9.0, 1),
        ];
        let t = totals(&l, 5);
        assert_eq!((t.long_usd, t.short_usd, t.count), (20.0, 10.0, 2));
    }

    fn candle(t: i64) -> Ohlc {
        Ohlc {
            time_ms: t,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            ..Default::default()
        }
    }

    #[test]
    fn marks_land_in_their_candle() {
        let gap = Ohlc {
            is_gap: true,
            ..Default::default()
        };
        // Candles at 0, 60, 120, then a gap column, then 300 (a day break).
        let slice = vec![candle(0), candle(60), candle(120), gap, candle(300)];
        let l = [
            liq("A", true, 1.0, 1.0, -1),   // before the window
            liq("A", true, 1.0, 1.0, 0),    // col 0
            liq("A", false, 1.0, 2.0, 119), // col 1
            liq("A", true, 1.0, 1.0, 150),  // col 2
            liq("A", true, 1.0, 1.0, 200),  // in the break: none
            liq("A", true, 1.0, 1.0, 359),  // col 4, the last candle
            liq("A", true, 1.0, 1.0, 360),  // after the window
        ];
        let m = marks(&slice, &l);
        let cols: Vec<usize> = m.iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![0, 1, 2, 4]);
        assert!(!m[1].long && m[1].usd == 2.0);
        assert!(marks(&[candle(0)], &l).is_empty(), "one candle: no length");
        assert!(marks(&[], &l).is_empty());
    }

    #[test]
    fn usd_is_short() {
        assert_eq!(fmt_usd(1_234_567.0), "$1.2M");
        assert_eq!(fmt_usd(350_400.0), "$350K");
        assert_eq!(fmt_usd(900.0), "$900");
        assert_eq!(fmt_usd(2.5e9), "$2.50B");
    }
}
