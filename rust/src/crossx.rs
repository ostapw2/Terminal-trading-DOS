//! The same coin on several exchanges: where to buy it (the lowest ask) and
//! where to sell it (the highest bid), what the gap is, and what is left
//! after both exchanges' taker fees.  Pure: quotes in, rows out.
//!
//! Reading the numbers honestly: the gap is what one buy and one sell at the
//! quoted touch would make at once on funded accounts at both exchanges
//! (coins on one, dollars on the other).  Moving the coins back (withdrawal
//! fees and minutes) is not counted, and the depth behind the touch is not
//! known: a thin touch fills a few hundred dollars, not a few thousand.

use std::collections::HashMap;

use crate::exchanges::{Exch, Quote};

/// What kind of pair the two sides make.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// Spot against spot on two exchanges.
    Cross,
    /// Spot against a perpetual (the basis, whichever exchanges).
    Basis,
    /// Two perpetuals.
    PerpCross,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Opp {
    pub symbol: String,
    pub kind: Kind,
    pub buy: Exch,
    pub sell: Exch,
    pub ask: f64,
    pub bid: f64,
    /// (bid / ask - 1), bps.
    pub gap_bps: f64,
    /// The gap less both takers, bps.
    pub net_bps: f64,
    /// The smaller of the two 24h dollar volumes.
    pub vol_min: f64,
}

/// A side wider than this is a dead book, not a price (bps).
pub const MAX_SPREAD_BPS: f64 = 60.0;
/// A gap wider than this is another asset under the same ticker (bps).
pub const MAX_GAP_BPS: f64 = 1_500.0;

pub fn kind_of(a: Exch, b: Exch) -> Kind {
    match (a.is_perp(), b.is_perp()) {
        (false, false) => Kind::Cross,
        (true, true) => Kind::PerpCross,
        _ => Kind::Basis,
    }
}

/// The best buy-here-sell-there row of every coin quoted on two or more
/// markets, `net_bps >= min_net_bps` only, widest first.  `min_vol`: both
/// sides must trade at least this many dollars a day (an exchange that does
/// not report volume counts as 0).
pub fn find(
    quotes: &HashMap<Exch, Vec<Quote>>,
    min_vol: f64,
    min_net_bps: f64,
    allow: impl Fn(Kind) -> bool,
) -> Vec<Opp> {
    let mut by_sym: HashMap<&str, Vec<(Exch, &Quote)>> = HashMap::new();
    for (ex, qs) in quotes {
        for q in qs {
            let spread = (q.ask / q.bid - 1.0) * 1e4;
            // Stablecoins, wrapped coins, leveraged and stock tokens are not
            // this kind of arbitrage.
            if !crate::scanner::tradable(&q.symbol) {
                continue;
            }
            if spread > MAX_SPREAD_BPS || q.quote_volume < min_vol {
                continue;
            }
            by_sym.entry(q.symbol.as_str()).or_default().push((*ex, q));
        }
    }
    let mut out = Vec::new();
    for (sym, v) in by_sym {
        if v.len() < 2 {
            continue;
        }
        let mut best: Option<Opp> = None;
        for (be, bq) in &v {
            for (se, sq) in &v {
                if be == se {
                    continue;
                }
                let kind = kind_of(*be, *se);
                if !allow(kind) {
                    continue;
                }
                let gap = (sq.bid / bq.ask - 1.0) * 1e4;
                if !(0.0..=MAX_GAP_BPS).contains(&gap) {
                    continue;
                }
                let net = gap - be.taker_bps() - se.taker_bps();
                if best.as_ref().is_none_or(|b| net > b.net_bps) {
                    best = Some(Opp {
                        symbol: sym.to_string(),
                        kind,
                        buy: *be,
                        sell: *se,
                        ask: bq.ask,
                        bid: sq.bid,
                        gap_bps: gap,
                        net_bps: net,
                        vol_min: bq.quote_volume.min(sq.quote_volume),
                    });
                }
            }
        }
        if let Some(b) = best.filter(|b| b.net_bps >= min_net_bps) {
            out.push(b);
        }
    }
    out.sort_by(|a, b| {
        b.net_bps
            .total_cmp(&a.net_bps)
            .then(a.symbol.cmp(&b.symbol))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(sym: &str, bid: f64, ask: f64, vol: f64) -> Quote {
        Quote {
            symbol: sym.into(),
            bid,
            ask,
            last: (bid + ask) / 2.0,
            quote_volume: vol,
        }
    }

    fn book(rows: &[(Exch, Quote)]) -> HashMap<Exch, Vec<Quote>> {
        let mut m: HashMap<Exch, Vec<Quote>> = HashMap::new();
        for (e, qq) in rows {
            m.entry(*e).or_default().push(qq.clone());
        }
        m
    }

    #[test]
    fn it_buys_the_lowest_ask_and_sells_the_highest_bid_net_of_both_fees() {
        let m = book(&[
            (Exch::Binance, q("XUSDT", 99.9, 100.0, 1e7)),
            (Exch::Bybit, q("XUSDT", 100.9, 101.0, 1e7)),
            (Exch::Okx, q("XUSDT", 100.2, 100.3, 1e7)),
        ]);
        let v = find(&m, 1e6, -1e9, |_| true);
        assert_eq!(v.len(), 1);
        let o = &v[0];
        assert_eq!((o.buy, o.sell), (Exch::Binance, Exch::Bybit));
        assert!((o.gap_bps - 90.0).abs() < 1.0, "{o:?}");
        // 90 bps gap, 10 + 10 of takers.
        assert!((o.net_bps - 70.0).abs() < 1.0, "{o:?}");
        assert_eq!(o.kind, Kind::Cross);
    }

    #[test]
    fn dead_books_thin_coins_and_other_assets_are_left_out() {
        // A 100 bps wide book on one side.
        let wide = book(&[
            (Exch::Binance, q("XUSDT", 99.0, 100.0, 1e7)),
            (Exch::Bybit, q("XUSDT", 105.0, 105.1, 1e7)),
        ]);
        let v = find(&wide, 1e6, -1e9, |_| true);
        assert!(
            v.iter()
                .all(|o| o.buy != Exch::Binance || o.sell != Exch::Bybit),
            "{v:?}"
        );
        // A volume floor.
        let thin = book(&[
            (Exch::Binance, q("XUSDT", 99.9, 100.0, 5e5)),
            (Exch::Bybit, q("XUSDT", 101.0, 101.1, 1e7)),
        ]);
        assert!(find(&thin, 1e6, -1e9, |_| true).is_empty());
        // The same ticker 2x apart is not the same asset.
        let other = book(&[
            (Exch::Binance, q("XUSDT", 99.9, 100.0, 1e7)),
            (Exch::Bybit, q("XUSDT", 200.0, 200.1, 1e7)),
        ]);
        assert!(find(&other, 1e6, -1e9, |_| true).is_empty());
        // One market only.
        let alone = book(&[(Exch::Binance, q("XUSDT", 99.9, 100.0, 1e7))]);
        assert!(find(&alone, 0.0, -1e9, |_| true).is_empty());
    }

    #[test]
    fn the_minimum_net_and_the_kind_filter_apply() {
        let m = book(&[
            (Exch::Binance, q("XUSDT", 99.9, 100.0, 1e7)),
            (Exch::BybitFut, q("XUSDT", 100.4, 100.45, 1e7)),
        ]);
        let all = find(&m, 1e6, -1e9, |_| true);
        assert_eq!(all[0].kind, Kind::Basis);
        assert!(
            find(&m, 1e6, 100.0, |_| true).is_empty(),
            "40 bps gap, 15.5 of fees"
        );
        assert!(find(&m, 1e6, -1e9, |k| k == Kind::Cross).is_empty());
    }
}
