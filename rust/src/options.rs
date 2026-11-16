//! Options as an arbitrage source (Binance European options, `/eapi`):
//! the chain's quotes, put-call parity per strike (a conversion or a
//! reversal against the spot), and box spreads between two strikes (four
//! options, no spot).  Pure: quotes in, findings out; `crate::data::options`
//! fetches, `crate::autobasket` opens the baskets.
//!
//! Honesty: an edge here is bid / ask at one moment, net of the exchange's
//! fees and of the carry to expiry at the risk-free rate the exchange
//! quotes.  It is held to expiry: the paper ledger settles an expired
//! option at its intrinsic value from the index ([`settle`]), not at a
//! quote.  Real minimum size is 0.01 contract (one coin a contract for
//! BTC / ETH / SOL / BNB, 100 XRP, 1000 DOGE); paper sizes freely and says
//! so.  Nothing here trades live.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The kv key the fetching window saves the picture under (JSON [`Options`]).
pub const KV_KEY: &str = "options_report";
/// Binance's fees, bps of the underlying notional: an option leg (taker;
/// maker is 2), a spot leg (VIP0).  Settlement of an exercised option
/// costs 1.5 bps more; the plan says so.
pub const OPTION_FEE_BPS: f64 = 3.0;
pub const SPOT_FEE_BPS: f64 = 10.0;
/// Findings kept per kind.
pub const MAX_FINDINGS: usize = 40;
/// Only quotes with at least this bid (USDT) count: an empty side is 0.
const MIN_QUOTE: f64 = 1e-9;
/// The least an expired contract is marked (see [`settle`]).
pub const SETTLED_FLOOR: f64 = 1e-6;
const MS_PER_DAY: f64 = 86_400_000.0;

/// One contract's quote.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Contract {
    /// "BTC-261225-85000-C".
    pub symbol: String,
    /// "BTCUSDT".
    pub underlying: String,
    pub expiry_ms: i64,
    pub strike: f64,
    pub call: bool,
    /// Coins per contract (1 for BTC, 100 for XRP, 1000 for DOGE).
    pub unit: f64,
    /// Quotes are per contract (`unit` coins), USDT.
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    /// Contracts traded today.
    pub volume: f64,
    /// Marked volatility, if the exchange gave it.
    pub iv: f64,
    /// The risk-free rate the exchange marks with (0.05 = 5 % a year).
    pub rate: f64,
}

impl Contract {
    /// "BTC-261225-85000-C" -> (underlying "BTCUSDT", expiry ms, strike, call).
    pub fn parse_symbol(symbol: &str) -> Option<(String, i64, f64, bool)> {
        let mut it = symbol.split('-');
        let base = it.next()?;
        let date = it.next()?;
        let strike: f64 = it.next()?.parse().ok()?;
        let side = it.next()?;
        if it.next().is_some() || base.is_empty() || date.len() != 6 {
            return None;
        }
        let call = match side {
            "C" => true,
            "P" => false,
            _ => return None,
        };
        let yy: i64 = date[0..2].parse().ok()?;
        let mm: u32 = date[2..4].parse().ok()?;
        let dd: u32 = date[4..6].parse().ok()?;
        // Binance options expire at 08:00 UTC.
        let days = days_from_civil(2000 + yy, mm, dd)?;
        let expiry_ms = days * 86_400_000 + 8 * 3_600_000;
        Some((format!("{base}USDT"), expiry_ms, strike, call))
    }

    pub fn mid(&self) -> Option<f64> {
        (self.bid > MIN_QUOTE && self.ask > MIN_QUOTE).then(|| (self.bid + self.ask) / 2.0)
    }

    /// What one contract is worth at expiry, from the index: `unit`
    /// coins' worth of the strike's payoff (prices are per contract,
    /// strikes per coin).
    pub fn intrinsic(&self, index: f64) -> f64 {
        let per_coin = if self.call {
            (index - self.strike).max(0.0)
        } else {
            (self.strike - index).max(0.0)
        };
        per_coin * self.unit.max(1e-9)
    }

    pub fn quoted(&self) -> bool {
        self.bid > MIN_QUOTE && self.ask > MIN_QUOTE && self.ask >= self.bid
    }

    /// "BTC 25DEC 85000 C".
    pub fn words(&self) -> String {
        format!(
            "{} {} {} {}",
            self.underlying.trim_end_matches("USDT"),
            expiry_words(self.expiry_ms),
            fmt_strike(self.strike),
            if self.call { "C" } else { "P" }
        )
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// (year, month, day) of a day count since 1970-01-01 (inverse of
/// `days_from_civil`).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32, d as u32)
}

/// "25DEC" from the expiry.
pub fn expiry_words(expiry_ms: i64) -> String {
    let (_, m, d) = civil_from_days(expiry_ms.div_euclid(86_400_000));
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    format!("{d}{}", MONTHS[(m as usize).clamp(1, 12) - 1])
}

/// The exchange's symbol for a contract: "BTC-261225-85000-C".
pub fn symbol_for(base: &str, expiry_ms: i64, strike: f64, call: bool) -> String {
    let (y, m, d) = civil_from_days(expiry_ms.div_euclid(86_400_000));
    format!(
        "{base}-{:02}{m:02}{d:02}-{}-{}",
        y.rem_euclid(100),
        fmt_strike(strike),
        if call { "C" } else { "P" }
    )
}

fn fmt_strike(k: f64) -> String {
    if k >= 100.0 || k.fract() == 0.0 {
        format!("{k:.0}")
    } else {
        format!("{k}")
    }
}

/// Which way round a parity trade goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Buy the spot, buy the put, sell the call: the options are rich.
    Conversion,
    /// Sell the spot, sell the put, buy the call: the options are cheap.
    Reversal,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Conversion => "conversion",
            Side::Reversal => "reversal",
        }
    }
}

/// Put-call parity at one strike: the forward the options imply against the
/// fair forward (spot plus carry), bid / ask at the moment of the fetch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parity {
    pub underlying: String,
    pub expiry_ms: i64,
    pub strike: f64,
    pub call: String,
    pub put: String,
    pub unit: f64,
    pub side: Side,
    /// The index (spot) the edge was measured against.
    pub spot: f64,
    /// The fair forward: spot grown at `rate` to expiry.
    pub forward: f64,
    pub rate: f64,
    pub days: f64,
    /// Gross edge, bps of spot, before fees.
    pub gross_bps: f64,
    /// Fees of the three legs, bps of spot.
    pub fees_bps: f64,
    /// What the quotes imply: the forward the options lock in.
    pub implied_forward: f64,
}

impl Parity {
    pub fn net_bps(&self) -> f64 {
        self.gross_bps - self.fees_bps
    }
    /// The net edge as a yearly rate, % (what the capital earns to expiry).
    pub fn annual_pct(&self) -> f64 {
        if self.days <= 0.0 {
            return 0.0;
        }
        self.net_bps() / 100.0 * 365.0 / self.days
    }
    /// A stable key: "BTC-261225-85000:conversion".
    pub fn key(&self) -> String {
        format!(
            "{}-{}-{}:{}",
            self.underlying.trim_end_matches("USDT"),
            self.expiry_ms / 86_400_000,
            fmt_strike(self.strike),
            self.side.label()
        )
    }
    pub fn words(&self) -> String {
        format!(
            "{} {} {} {}",
            self.underlying.trim_end_matches("USDT"),
            expiry_words(self.expiry_ms),
            fmt_strike(self.strike),
            self.side.label()
        )
    }
    /// The legs for one coin of spot: (symbol, long, coins).  Option legs
    /// are in contracts (`unit` coins each).
    pub fn legs(&self, coins: f64) -> Vec<(String, bool, f64)> {
        let contracts = coins / self.unit;
        match self.side {
            Side::Conversion => vec![
                (self.underlying.clone(), true, coins),
                (self.put.clone(), true, contracts),
                (self.call.clone(), false, contracts),
            ],
            Side::Reversal => vec![
                (self.underlying.clone(), false, coins),
                (self.put.clone(), false, contracts),
                (self.call.clone(), true, contracts),
            ],
        }
    }
}

/// A box spread: at expiry it pays `high - low` whatever the index does, so
/// its price against that payout is a loan at an implied rate.  Four
/// options, no spot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoxSpread {
    pub underlying: String,
    pub expiry_ms: i64,
    pub low: f64,
    pub high: f64,
    pub unit: f64,
    /// Symbols: call / put at the low strike, call / put at the high one.
    pub call_low: String,
    pub put_low: String,
    pub call_high: String,
    pub put_high: String,
    /// True: buy the box (pay `cost`, receive the payout): the box is cheap.
    /// False: sell it (receive `cost`, pay the payout at expiry).
    pub buy: bool,
    /// What the box costs (buy) or brings (sell), per coin.
    pub cost: f64,
    pub rate: f64,
    pub days: f64,
    /// The edge against the fair price of the payout, bps of spot.
    pub gross_bps: f64,
    pub fees_bps: f64,
    pub spot: f64,
}

impl BoxSpread {
    pub fn payout(&self) -> f64 {
        self.high - self.low
    }
    pub fn net_bps(&self) -> f64 {
        self.gross_bps - self.fees_bps
    }
    pub fn annual_pct(&self) -> f64 {
        if self.days <= 0.0 {
            return 0.0;
        }
        self.net_bps() / 100.0 * 365.0 / self.days
    }
    pub fn key(&self) -> String {
        format!(
            "{}-{}-{}/{}:{}",
            self.underlying.trim_end_matches("USDT"),
            self.expiry_ms / 86_400_000,
            fmt_strike(self.low),
            fmt_strike(self.high),
            if self.buy { "buy" } else { "sell" }
        )
    }
    pub fn words(&self) -> String {
        format!(
            "{} {} box {}/{} {}",
            self.underlying.trim_end_matches("USDT"),
            expiry_words(self.expiry_ms),
            fmt_strike(self.low),
            fmt_strike(self.high),
            if self.buy { "buy" } else { "sell" }
        )
    }
    /// The legs for `coins` of underlying: a long box is long the low call,
    /// short the high call, short the low put, long the high put.
    pub fn legs(&self, coins: f64) -> Vec<(String, bool, f64)> {
        let c = coins / self.unit;
        let b = self.buy;
        vec![
            (self.call_low.clone(), b, c),
            (self.call_high.clone(), !b, c),
            (self.put_low.clone(), !b, c),
            (self.put_high.clone(), b, c),
        ]
    }
}

/// The picture at `at_ms`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub at_ms: i64,
    /// Contracts quoted on both sides / listed.
    pub quoted: usize,
    pub listed: usize,
    pub underlyings: Vec<String>,
    /// Strikes with both a call and a put quoted (parities looked at).
    pub strikes: usize,
    pub boxes_tried: usize,
    /// The best first (net edge).
    pub parity: Vec<Parity>,
    pub boxes: Vec<BoxSpread>,
}

impl Options {
    pub fn is_empty(&self) -> bool {
        self.parity.is_empty() && self.boxes.is_empty()
    }
}

/// The call and the put at one strike.
type Sides<'a> = (Option<&'a Contract>, Option<&'a Contract>);

/// Years to expiry, never negative.
fn years(expiry_ms: i64, now_ms: i64) -> f64 {
    ((expiry_ms - now_ms) as f64 / MS_PER_DAY / 365.0).max(0.0)
}

/// Put-call parities and box spreads from the chain.  `spot`: the index
/// per underlying ("BTCUSDT" -> price); a contract whose underlying has no
/// index is skipped.  Both lists carry the best `MAX_FINDINGS` by net edge,
/// positive net edges only.
pub fn findings(chain: &[Contract], spot: &HashMap<String, f64>, now_ms: i64) -> Options {
    let mut by_strike: HashMap<(String, i64, u64), Sides<'_>> = HashMap::new();
    let mut underlyings: Vec<String> = Vec::new();
    let mut quoted = 0;
    for c in chain {
        if !underlyings.contains(&c.underlying) {
            underlyings.push(c.underlying.clone());
        }
        if !c.quoted() || c.expiry_ms <= now_ms {
            continue;
        }
        quoted += 1;
        let e = by_strike
            .entry((c.underlying.clone(), c.expiry_ms, c.strike.to_bits()))
            .or_default();
        if c.call {
            e.0 = Some(c);
        } else {
            e.1 = Some(c);
        }
    }
    underlyings.sort();
    let mut parity = Vec::new();
    // Per (underlying, expiry): the strikes with both sides, for the boxes.
    let mut pairs: HashMap<(String, i64), Vec<(&Contract, &Contract)>> = HashMap::new();
    let mut strikes = 0;
    for ((u, expiry_ms, _), (call, put)) in &by_strike {
        let (Some(call), Some(put)) = (call, put) else {
            continue;
        };
        let Some(&s) = spot.get(u) else {
            continue;
        };
        if !(s.is_finite() && s > 0.0) {
            continue;
        }
        strikes += 1;
        pairs
            .entry((u.clone(), *expiry_ms))
            .or_default()
            .push((call, put));
        let t = years(*expiry_ms, now_ms);
        let rate = call.rate;
        let forward = s * (rate * t).exp();
        let fees_bps = SPOT_FEE_BPS + 2.0 * OPTION_FEE_BPS;
        // Per coin: a quote is per contract of `unit` coins.
        let unit = call.unit.max(1e-9);
        let (cb, ca, pb, pa) = (
            call.bid / unit,
            call.ask / unit,
            put.bid / unit,
            put.ask / unit,
        );
        // Conversion locks a sale at K: receive K + call bid - put ask at
        // expiry for a coin bought at s now; fair is the forward.
        let conv = call.strike + cb - pa;
        let rev = call.strike + ca - pb;
        let base = Parity {
            underlying: u.clone(),
            expiry_ms: *expiry_ms,
            strike: call.strike,
            call: call.symbol.clone(),
            put: put.symbol.clone(),
            unit: call.unit.max(1e-9),
            side: Side::Conversion,
            spot: s,
            forward,
            rate,
            days: t * 365.0,
            gross_bps: 0.0,
            fees_bps,
            implied_forward: conv,
        };
        let conv_bps = (conv - forward) / s * 1e4;
        let rev_bps = (forward - rev) / s * 1e4;
        if conv_bps - fees_bps > 0.0 {
            parity.push(Parity {
                gross_bps: conv_bps,
                ..base.clone()
            });
        }
        if rev_bps - fees_bps > 0.0 {
            parity.push(Parity {
                side: Side::Reversal,
                gross_bps: rev_bps,
                implied_forward: rev,
                ..base
            });
        }
    }
    parity.sort_by(|a, b| b.net_bps().total_cmp(&a.net_bps()));
    parity.truncate(MAX_FINDINGS);

    let mut boxes = Vec::new();
    let mut boxes_tried = 0;
    for ((u, expiry_ms), mut v) in pairs {
        let Some(&s) = spot.get(&u) else {
            continue;
        };
        v.sort_by(|a, b| a.0.strike.total_cmp(&b.0.strike));
        let t = years(expiry_ms, now_ms);
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                boxes_tried += 1;
                let (cl, pl) = v[i];
                let (ch, ph) = v[j];
                let payout = ch.strike - cl.strike;
                if payout <= 0.0 {
                    continue;
                }
                let rate = cl.rate;
                let fair = payout * (-rate * t).exp();
                let fees_bps = 4.0 * OPTION_FEE_BPS;
                let unit = cl.unit.max(1e-9);
                // Buy the box: pay the asks, receive the bids (per coin).
                let buy_cost = (cl.ask - ch.bid - pl.bid + ph.ask) / unit;
                let sell_gets = (cl.bid - ch.ask - pl.ask + ph.bid) / unit;
                let base = BoxSpread {
                    underlying: u.clone(),
                    expiry_ms,
                    low: cl.strike,
                    high: ch.strike,
                    unit: cl.unit.max(1e-9),
                    call_low: cl.symbol.clone(),
                    put_low: pl.symbol.clone(),
                    call_high: ch.symbol.clone(),
                    put_high: ph.symbol.clone(),
                    buy: true,
                    cost: buy_cost,
                    rate,
                    days: t * 365.0,
                    gross_bps: (fair - buy_cost) / s * 1e4,
                    fees_bps,
                    spot: s,
                };
                if base.net_bps() > 0.0 {
                    boxes.push(base.clone());
                }
                let sell = BoxSpread {
                    buy: false,
                    cost: sell_gets,
                    gross_bps: (sell_gets - fair) / s * 1e4,
                    ..base
                };
                if sell.net_bps() > 0.0 {
                    boxes.push(sell);
                }
            }
        }
    }
    boxes.sort_by(|a, b| b.net_bps().total_cmp(&a.net_bps()));
    boxes.truncate(MAX_FINDINGS);
    Options {
        at_ms: now_ms,
        quoted,
        listed: chain.len(),
        underlyings,
        strikes,
        boxes_tried,
        parity,
        boxes,
    }
}

/// The mark the paper ledger should carry for a contract: its mid while it
/// trades; from expiry on, its intrinsic value from the index (settlement).
/// `None`: nothing known (no quote and no index).
pub fn settle(c: &Contract, index: Option<f64>, now_ms: i64) -> Option<f64> {
    if now_ms >= c.expiry_ms {
        // A worthless contract is marked a millionth, not zero: the ledger
        // takes no zero price, and the exit must fill.
        return index.map(|s| c.intrinsic(s).max(SETTLED_FLOOR));
    }
    c.mid()
        .or((c.last > MIN_QUOTE).then_some(c.last))
        .or(index.map(|s| c.intrinsic(s)))
}

/// Black-Scholes price per coin (used by the demo chain and the tests).
pub fn black_scholes(spot: f64, strike: f64, years: f64, sigma: f64, rate: f64, call: bool) -> f64 {
    if years <= 0.0 || sigma <= 0.0 || spot <= 0.0 || strike <= 0.0 {
        let i = if call {
            (spot - strike).max(0.0)
        } else {
            (strike - spot).max(0.0)
        };
        return i;
    }
    let sq = years.sqrt();
    let d1 = ((spot / strike).ln() + (rate + sigma * sigma / 2.0) * years) / (sigma * sq);
    let d2 = d1 - sigma * sq;
    let df = (-rate * years).exp();
    if call {
        spot * norm_cdf(d1) - strike * df * norm_cdf(d2)
    } else {
        strike * df * norm_cdf(-d2) - spot * norm_cdf(-d1)
    }
}

/// The standard normal cdf (Abramowitz-Stegun 7.1.26 via erf, 1.5e-7).
fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(symbol: &str, bid: f64, ask: f64, unit: f64, rate: f64) -> Contract {
        let (underlying, expiry_ms, strike, call) = Contract::parse_symbol(symbol).unwrap();
        Contract {
            symbol: symbol.into(),
            underlying,
            expiry_ms,
            strike,
            call,
            unit,
            bid,
            ask,
            last: (bid + ask) / 2.0,
            volume: 1.0,
            iv: 0.5,
            rate,
        }
    }

    #[test]
    fn symbols_parse_and_expire_at_eight_utc() {
        let (u, e, k, c) = Contract::parse_symbol("BTC-261225-85000-C").unwrap();
        assert_eq!(u, "BTCUSDT");
        assert_eq!(k, 85_000.0);
        assert!(c);
        // 2026-12-25 08:00 UTC.
        assert_eq!(e, 1_798_185_600_000);
        assert_eq!(expiry_words(e), "25DEC");
        let (u, _, k, c) = Contract::parse_symbol("DOGE-261030-0.15-P").unwrap();
        assert_eq!((u.as_str(), k, c), ("DOGEUSDT", 0.15, false));
        assert_eq!(symbol_for("BTC", e, 85_000.0, true), "BTC-261225-85000-C");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_784), (2026, 11, 27));
        assert!(Contract::parse_symbol("BTCUSDT").is_none());
        assert!(Contract::parse_symbol("BTC-261225-85000-X").is_none());
        assert!(Contract::parse_symbol("BTC-2612-85000-C").is_none());
        let c = contract("ETH-261127-3000-C", 100.0, 110.0, 1.0, 0.05);
        assert_eq!(c.words(), "ETH 27NOV 3000 C");
        assert_eq!(c.mid(), Some(105.0));
        assert_eq!(c.intrinsic(3_250.0), 250.0);
        assert_eq!(c.intrinsic(2_900.0), 0.0);
    }

    #[test]
    fn fair_quotes_give_no_edge_and_a_rich_call_gives_a_conversion() {
        let now = 1_792_300_000_000; // 2026-10-18
        let s = 80_000.0;
        let (_, exp, _, _) = Contract::parse_symbol("BTC-261127-80000-C").unwrap();
        let t = (exp - now) as f64 / MS_PER_DAY / 365.0;
        let rate = 0.05;
        let fair = |k: f64, call: bool| black_scholes(s, k, t, 0.55, rate, call);
        let mut chain = Vec::new();
        for k in [70_000.0, 80_000.0, 90_000.0] {
            for call in [true, false] {
                let sym = format!("BTC-261127-{k:.0}-{}", if call { "C" } else { "P" });
                let p = fair(k, call);
                chain.push(contract(&sym, p * 0.999, p * 1.001, 1.0, rate));
            }
        }
        let mut spot = HashMap::new();
        spot.insert("BTCUSDT".to_string(), s);
        let o = findings(&chain, &spot, now);
        assert_eq!(o.strikes, 3);
        assert_eq!(o.boxes_tried, 3);
        assert!(o.parity.is_empty(), "fair quotes: {:?}", o.parity);
        assert!(o.boxes.is_empty(), "fair boxes: {:?}", o.boxes);
        // The 80000 call bid 1 % of spot too high: a conversion pays.
        let i = chain
            .iter()
            .position(|c| c.symbol == "BTC-261127-80000-C")
            .unwrap();
        chain[i].bid += s * 0.01;
        chain[i].ask += s * 0.01;
        let o = findings(&chain, &spot, now);
        assert_eq!(o.parity.len(), 1, "{:?}", o.parity);
        let p = &o.parity[0];
        assert_eq!(p.side, Side::Conversion);
        assert!(
            (p.gross_bps - 100.0).abs() < 3.0,
            "about 100 bps gross: {}",
            p.gross_bps
        );
        assert!((p.net_bps() - (p.gross_bps - 16.0)).abs() < 1e-9);
        assert!(
            p.annual_pct() > 5.0,
            "84 bps over 40 days: {}% a year",
            p.annual_pct()
        );
        assert_eq!(p.key(), "BTC-20784-80000:conversion");
        let legs = p.legs(0.5);
        assert_eq!(legs[0], ("BTCUSDT".to_string(), true, 0.5));
        assert_eq!(legs[1], ("BTC-261127-80000-P".to_string(), true, 0.5));
        assert_eq!(legs[2], ("BTC-261127-80000-C".to_string(), false, 0.5));
        // The rich call also makes the 70000/80000 box cheap to sell and
        // the 80000/90000 box cheap to buy.
        assert!(!o.boxes.is_empty());
        let b = &o.boxes[0];
        assert_eq!(b.legs(1.0).len(), 4);
        assert!(b.net_bps() > 0.0);
        assert!(b.words().contains("box"));
        // Unit: 100 XRP a contract -> 0.01 contract for one coin.
        let mut p2 = p.clone();
        p2.unit = 100.0;
        assert_eq!(p2.legs(1.0)[1].2, 0.01);
        // Quotes per contract of 100 coins: the same fair chain, every
        // premium a hundredfold, gives the same (no) edge.
        let hundred: Vec<Contract> = chain
            .iter()
            .map(|c| Contract {
                bid: c.bid * 100.0,
                ask: c.ask * 100.0,
                last: c.last * 100.0,
                unit: 100.0,
                ..c.clone()
            })
            .collect();
        let o100 = findings(&hundred, &spot, now);
        assert_eq!(o100.parity.len(), 1, "{:?}", o100.parity);
        assert!((o100.parity[0].gross_bps - p.gross_bps).abs() < 1e-6);
        assert_eq!(o100.boxes.len(), o.boxes.len());
        assert_eq!(
            hundred[0].intrinsic(90_000.0),
            100.0 * chain[0].intrinsic(90_000.0)
        );
        // Expired or half-quoted contracts are skipped.
        chain[i].bid = 0.0;
        let o = findings(&chain, &spot, now);
        assert!(o.parity.is_empty());
        assert_eq!(o.quoted, 5);
        let o = findings(&chain, &spot, exp + 1);
        assert_eq!(o.strikes, 0);
    }

    #[test]
    fn settlement_is_intrinsic_after_expiry_and_mid_before() {
        let c = contract("BTC-261127-80000-P", 1_000.0, 1_100.0, 1.0, 0.05);
        assert_eq!(settle(&c, Some(79_000.0), c.expiry_ms - 1), Some(1_050.0));
        assert_eq!(settle(&c, Some(79_000.0), c.expiry_ms), Some(1_000.0));
        assert_eq!(settle(&c, Some(90_000.0), c.expiry_ms), Some(SETTLED_FLOOR));
        assert_eq!(settle(&c, None, c.expiry_ms), None);
        let mut dead = c.clone();
        dead.bid = 0.0;
        dead.ask = 0.0;
        dead.last = 0.0;
        assert_eq!(
            settle(&dead, Some(79_000.0), 0),
            Some(1_000.0),
            "no quote: intrinsic"
        );
        // Black-Scholes sanity: put-call parity holds for the model itself.
        let (s, k, t, r) = (100.0, 95.0, 0.5, 0.03);
        let c = black_scholes(s, k, t, 0.4, r, true);
        let p = black_scholes(s, k, t, 0.4, r, false);
        assert!((c - p - (s - k * (-r * t).exp())).abs() < 1e-9);
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((norm_cdf(1.96) - 0.975).abs() < 1e-3);
    }
}
