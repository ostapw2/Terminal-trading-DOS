//! Binance European options, public REST (`https://eapi.binance.com`), no
//! key: the listed contracts (`/eapi/v1/exchangeInfo`, once an hour), every
//! contract's bid / ask / last and the index (`/eapi/v1/ticker`, one
//! request for the whole chain) and the marks' volatility and risk-free
//! rate (`/eapi/v1/mark`).  Weight: about 5 + 5 + 5 a fetch on a limit of
//! 400 a minute, separate from Spot's.  Demo mode makes a chain from the
//! demo prices (rule 24: the network layer only).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::data::binance::{get_json, parse_f64, BinanceError};
use crate::data::demo;
use crate::options::{black_scholes, symbol_for, Contract};

const BASE: &str = "https://eapi.binance.com/eapi/v1";
/// The listing is asked again after this long.
const LISTING_TTL_MS: i64 = 3_600_000;

/// The chain with the index per underlying ("BTCUSDT" -> price).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    pub contracts: Vec<Contract>,
    pub index: HashMap<String, f64>,
}

/// What the listing says of a contract: coins a contract, expiry.
#[derive(Clone, Debug, PartialEq)]
struct Listed {
    unit: f64,
    expiry_ms: i64,
    strike: f64,
    call: bool,
    underlying: String,
}

fn listing() -> &'static Mutex<(i64, HashMap<String, Listed>)> {
    static L: OnceLock<Mutex<(i64, HashMap<String, Listed>)>> = OnceLock::new();
    L.get_or_init(|| Mutex::new((0, HashMap::new())))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The listed contracts, from the cache when it is fresh.
fn fetch_listing(now: i64) -> Result<HashMap<String, Listed>, BinanceError> {
    if let Ok(l) = listing().lock() {
        if !l.1.is_empty() && now - l.0 < LISTING_TTL_MS {
            return Ok(l.1.clone());
        }
    }
    let raw = get_json(&format!("{BASE}/exchangeInfo"))?;
    let arr = raw
        .get("optionSymbols")
        .and_then(|v| v.as_array())
        .ok_or_else(|| BinanceError::Parse("exchangeInfo: no optionSymbols".into()))?;
    let mut out = HashMap::with_capacity(arr.len());
    for row in arr {
        let symbol = row
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if row.get("status").and_then(|v| v.as_str()) != Some("TRADING") {
            continue;
        }
        let Some((underlying, expiry_ms, strike, call)) = Contract::parse_symbol(symbol) else {
            continue;
        };
        let unit = row.get("unit").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let expiry_ms = row
            .get("expiryDate")
            .and_then(|v| v.as_i64())
            .unwrap_or(expiry_ms);
        let strike = row
            .get("strikePrice")
            .map(parse_f64)
            .and_then(Result::ok)
            .unwrap_or(strike);
        let underlying = row
            .get("underlying")
            .and_then(|v| v.as_str())
            .map_or(underlying, str::to_string);
        out.insert(
            symbol.to_string(),
            Listed {
                unit: if unit > 0.0 { unit } else { 1.0 },
                expiry_ms,
                strike,
                call,
                underlying,
            },
        );
    }
    if let Ok(mut l) = listing().lock() {
        *l = (now, out.clone());
    }
    Ok(out)
}

/// The whole chain, quoted: three requests.
pub fn fetch_chain() -> Result<Chain, BinanceError> {
    if demo::is_on() {
        return Ok(demo_chain(demo::now_ms()));
    }
    let now = now_ms();
    let listed = fetch_listing(now)?;
    let tickers = get_json(&format!("{BASE}/ticker"))?;
    let rows = tickers
        .as_array()
        .ok_or_else(|| BinanceError::Parse("ticker: not an array".into()))?;
    let mut contracts = Vec::with_capacity(rows.len());
    let mut index: HashMap<String, f64> = HashMap::new();
    let num = |row: &serde_json::Value, key: &str| -> f64 {
        row.get(key)
            .map(parse_f64)
            .and_then(Result::ok)
            .unwrap_or(0.0)
    };
    for row in rows {
        let symbol = row
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let Some(l) = listed.get(symbol) else {
            continue;
        };
        let exercise = num(row, "exercisePrice");
        if exercise > 0.0 {
            index.entry(l.underlying.clone()).or_insert(exercise);
        }
        contracts.push(Contract {
            symbol: symbol.to_string(),
            underlying: l.underlying.clone(),
            expiry_ms: l.expiry_ms,
            strike: l.strike,
            call: l.call,
            unit: l.unit,
            bid: num(row, "bidPrice"),
            ask: num(row, "askPrice"),
            last: num(row, "lastPrice"),
            volume: num(row, "volume"),
            iv: 0.0,
            rate: 0.0,
        });
    }
    // The marks: volatility and the rate (the rate drives the fair forward).
    if let Ok(marks) = get_json(&format!("{BASE}/mark")) {
        if let Some(rows) = marks.as_array() {
            let mut by: HashMap<&str, (f64, f64)> = HashMap::with_capacity(rows.len());
            for row in rows {
                if let Some(s) = row.get("symbol").and_then(|v| v.as_str()) {
                    by.insert(s, (num(row, "markIV"), num(row, "riskFreeInterest")));
                }
            }
            for c in &mut contracts {
                if let Some(&(iv, rate)) = by.get(c.symbol.as_str()) {
                    c.iv = iv;
                    c.rate = rate;
                }
            }
        }
    }
    Ok(Chain { contracts, index })
}

/// Demo: BTC and ETH, three expiries, nine strikes around the spot, priced
/// by Black-Scholes at a volatility of 55 % with a 0.1 % spread, and one
/// strike an expiry whose call is quoted 0.6 % of spot too rich (a
/// conversion to find).  Deterministic in time like the demo prices.
pub fn demo_chain(now_ms: i64) -> Chain {
    let mut contracts = Vec::new();
    let mut index = HashMap::new();
    let day = now_ms.div_euclid(86_400_000);
    for base in ["BTC", "ETH"] {
        let underlying = format!("{base}USDT");
        let spot = demo::price_at(&underlying, now_ms);
        index.insert(underlying.clone(), spot);
        for (n, days_out) in [7i64, 30, 60].into_iter().enumerate() {
            let expiry_ms = (day + days_out) * 86_400_000 + 8 * 3_600_000;
            let t = (expiry_ms - now_ms) as f64 / 86_400_000.0 / 365.0;
            let step = if base == "BTC" { 1_000.0 } else { 50.0 };
            let atm = (spot / step).round() * step;
            for k in -4..=4 {
                let strike = atm + k as f64 * step;
                for call in [true, false] {
                    let fair = black_scholes(spot, strike, t, 0.55, 0.05, call);
                    let rich = call && k == 1 && n == 1;
                    let mid = fair + if rich { spot * 0.006 } else { 0.0 };
                    let half = (mid * 0.0005).max(spot * 1e-5);
                    contracts.push(Contract {
                        symbol: symbol_for(base, expiry_ms, strike, call),
                        underlying: underlying.clone(),
                        expiry_ms,
                        strike,
                        call,
                        unit: 1.0,
                        bid: (mid - half).max(0.0),
                        ask: mid + half,
                        last: mid,
                        volume: 1.0,
                        iv: 0.55,
                        rate: 0.05,
                    });
                }
            }
        }
    }
    Chain { contracts, index }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::findings;

    #[test]
    fn the_demo_chain_has_one_conversion_per_underlying() {
        let now = 1_792_300_000_000;
        let ch = demo_chain(now);
        assert_eq!(ch.contracts.len(), 2 * 3 * 9 * 2);
        assert_eq!(ch.index.len(), 2);
        assert!(ch.contracts.iter().all(|c| c.quoted() && c.expiry_ms > now));
        let o = findings(&ch.contracts, &ch.index, now);
        assert_eq!(o.strikes, 54);
        let conv: Vec<&str> = o
            .parity
            .iter()
            .filter(|p| p.side == crate::options::Side::Conversion)
            .map(|p| p.underlying.as_str())
            .collect();
        assert!(
            conv.contains(&"BTCUSDT") && conv.contains(&"ETHUSDT"),
            "{conv:?}"
        );
        assert!(o
            .parity
            .iter()
            .all(|p| p.net_bps() > 0.0 && p.net_bps() < 60.0));
        // The rich call makes boxes with its strike mispriced too.
        assert!(!o.boxes.is_empty());
        assert!(o.parity[0].call.starts_with("BTC-") || o.parity[0].call.starts_with("ETH-"));
    }
}
