//! Prices of the same coin on many exchanges: what to buy it at (the ask)
//! and sell it at (the bid) on each, from the exchanges' public tickers (no
//! keys).  Every exchange answers in its own shape; each parser here turns
//! one into [`Quote`]s of USDT pairs named the Binance way (`BTCUSDT`), so
//! `crate::crossx` can lay them side by side.
//!
//! Spot and perpetual markets are listed separately: an exchange's perpetual
//! is a different instrument from its spot coin.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

/// A market of an exchange.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Exch {
    Binance,
    BinanceFut,
    Bybit,
    BybitFut,
    Okx,
    OkxSwap,
    Gate,
    Bitget,
    Kucoin,
}

pub const ALL: [Exch; 9] = [
    Exch::Binance,
    Exch::BinanceFut,
    Exch::Bybit,
    Exch::BybitFut,
    Exch::Okx,
    Exch::OkxSwap,
    Exch::Gate,
    Exch::Bitget,
    Exch::Kucoin,
];

impl Exch {
    /// The short code used in instrument names (`BTCUSDT@BYBIT`).
    pub fn code(self) -> &'static str {
        match self {
            Exch::Binance => "BINANCE",
            Exch::BinanceFut => "BINANCEF",
            Exch::Bybit => "BYBIT",
            Exch::BybitFut => "BYBITF",
            Exch::Okx => "OKX",
            Exch::OkxSwap => "OKXF",
            Exch::Gate => "GATE",
            Exch::Bitget => "BITGET",
            Exch::Kucoin => "KUCOIN",
        }
    }

    pub fn from_code(c: &str) -> Option<Exch> {
        ALL.into_iter().find(|e| e.code() == c)
    }

    pub fn is_perp(self) -> bool {
        matches!(self, Exch::BinanceFut | Exch::BybitFut | Exch::OkxSwap)
    }

    /// Taker fee at the lowest VIP tier, bps of the dollars (rounded up to
    /// what the public schedules say; a market maker pays less).
    pub fn taker_bps(self) -> f64 {
        match self {
            Exch::Binance => 10.0,
            Exch::BinanceFut => 5.0,
            Exch::Bybit => 10.0,
            Exch::BybitFut => 5.5,
            Exch::Okx => 10.0,
            Exch::OkxSwap => 5.0,
            Exch::Gate => 20.0,
            Exch::Bitget => 10.0,
            Exch::Kucoin => 10.0,
        }
    }
}

/// `BTCUSDT@BYBIT`: a coin on one exchange as a ledger instrument.
pub fn instrument(symbol: &str, ex: Exch) -> String {
    format!("{symbol}@{}", ex.code())
}

/// `BTCUSDT@BYBIT` -> (`BTCUSDT`, Bybit).
pub fn split_instrument(name: &str) -> Option<(&str, Exch)> {
    let (s, c) = name.split_once('@')?;
    Some((s, Exch::from_code(c)?))
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quote {
    /// `BTCUSDT`.
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    /// Dollars traded in 24 h, 0 when the exchange's ticker does not say.
    pub quote_volume: f64,
}

fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A quote with sane prices only: positive, finite, not crossed.
fn sane(symbol: String, bid: f64, ask: f64, last: f64, vol: f64) -> Option<Quote> {
    let ok = |p: f64| p.is_finite() && p > 0.0;
    (ok(bid) && ok(ask) && ask >= bid && symbol.ends_with("USDT") && symbol.len() > 4).then(|| {
        Quote {
            symbol,
            bid,
            ask,
            last: if ok(last) { last } else { (bid + ask) / 2.0 },
            quote_volume: if vol.is_finite() { vol.max(0.0) } else { 0.0 },
        }
    })
}

/// Binance `ticker/24hr` (spot): bid, ask, last and dollars in one row.
pub fn parse_binance_24h(v: &Value) -> Vec<Quote> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    sane(
                        r["symbol"].as_str()?.to_string(),
                        num(&r["bidPrice"]),
                        num(&r["askPrice"]),
                        num(&r["lastPrice"]),
                        num(&r["quoteVolume"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Binance futures `ticker/bookTicker` (bid / ask only), with the dollars of
/// `ticker/24hr` by symbol.
pub fn parse_binance_fut(book: &Value, day: &Value) -> Vec<Quote> {
    let vols: HashMap<&str, (f64, f64)> = day
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some((
                        r["symbol"].as_str()?,
                        (num(&r["quoteVolume"]), num(&r["lastPrice"])),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    book.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let s = r["symbol"].as_str()?;
                    let (vol, last) = vols.get(s).copied().unwrap_or((0.0, 0.0));
                    sane(
                        s.to_string(),
                        num(&r["bidPrice"]),
                        num(&r["askPrice"]),
                        last,
                        vol,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Bybit v5 `market/tickers` (spot and linear alike).
pub fn parse_bybit(v: &Value) -> Vec<Quote> {
    v["result"]["list"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    sane(
                        r["symbol"].as_str()?.to_string(),
                        num(&r["bid1Price"]),
                        num(&r["ask1Price"]),
                        num(&r["lastPrice"]),
                        num(&r["turnover24h"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// OKX `market/tickers` (spot `BTC-USDT`, swap `BTC-USDT-SWAP`).
pub fn parse_okx(v: &Value) -> Vec<Quote> {
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let id = r["instId"].as_str()?;
                    let swap = id.ends_with("-SWAP");
                    let sym = id.strip_suffix("-SWAP").unwrap_or(id).replace('-', "");
                    // volCcy24h is in the quote currency for spot (dollars as
                    // is) but in the base coin for swaps (dollars ~ that x last);
                    // checked live 20.10 (ETH-USDT: 175 M either way).
                    let last = num(&r["last"]);
                    let vol = num(&r["volCcy24h"]);
                    sane(
                        sym,
                        num(&r["bidPx"]),
                        num(&r["askPx"]),
                        last,
                        if swap { vol * last } else { vol },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Gate.io spot `tickers` (`BTC_USDT`).
pub fn parse_gate(v: &Value) -> Vec<Quote> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    sane(
                        r["currency_pair"].as_str()?.replace('_', ""),
                        num(&r["highest_bid"]),
                        num(&r["lowest_ask"]),
                        num(&r["last"]),
                        num(&r["quote_volume"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Bitget v2 spot `tickers` (`BTCUSDT`).
pub fn parse_bitget(v: &Value) -> Vec<Quote> {
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    sane(
                        r["symbol"].as_str()?.to_string(),
                        num(&r["bidPr"]),
                        num(&r["askPr"]),
                        num(&r["lastPr"]),
                        num(&r["usdtVolume"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// KuCoin `allTickers` (`BTC-USDT`; `buy` is the bid, `sell` the ask).
pub fn parse_kucoin(v: &Value) -> Vec<Quote> {
    v["data"]["ticker"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    sane(
                        r["symbol"].as_str()?.replace('-', ""),
                        num(&r["buy"]),
                        num(&r["sell"]),
                        num(&r["last"]),
                        num(&r["volValue"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn get(url: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(12))
        .user_agent("dos/0.1.0")
        .build();
    agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

/// One exchange's quotes now (public REST; the demo has its own).
pub fn fetch(ex: Exch) -> Result<Vec<Quote>, String> {
    use crate::data::binance::get_json;
    if crate::data::demo::is_on() {
        return Ok(demo_quotes(ex));
    }
    Ok(match ex {
        Exch::Binance => parse_binance_24h(
            &get_json("https://api.binance.com/api/v3/ticker/24hr").map_err(|e| e.to_string())?,
        ),
        Exch::BinanceFut => {
            let book = get_json("https://fapi.binance.com/fapi/v1/ticker/bookTicker")
                .map_err(|e| e.to_string())?;
            let day = get_json("https://fapi.binance.com/fapi/v1/ticker/24hr")
                .map_err(|e| e.to_string())?;
            parse_binance_fut(&book, &day)
        }
        Exch::Bybit => parse_bybit(&get(
            "https://api.bybit.com/v5/market/tickers?category=spot",
        )?),
        Exch::BybitFut => parse_bybit(&get(
            "https://api.bybit.com/v5/market/tickers?category=linear",
        )?),
        Exch::Okx => parse_okx(&get(
            "https://www.okx.com/api/v5/market/tickers?instType=SPOT",
        )?),
        Exch::OkxSwap => parse_okx(&get(
            "https://www.okx.com/api/v5/market/tickers?instType=SWAP",
        )?),
        Exch::Gate => parse_gate(&get("https://api.gateio.ws/api/v4/spot/tickers")?),
        Exch::Bitget => parse_bitget(&get("https://api.bitget.com/api/v2/spot/market/tickers")?),
        Exch::Kucoin => parse_kucoin(&get("https://api.kucoin.com/api/v1/market/allTickers")?),
    })
}

/// The demo's exchanges: a few coins whose prices drift apart a little, so
/// the screens have something to show with no network.
fn demo_quotes(ex: Exch) -> Vec<Quote> {
    let t = crate::data::demo::now_ms() as f64 / 60_000.0;
    let skew = (ALL.iter().position(|e| *e == ex).unwrap_or(0) as f64 * 0.7 + t).sin() * 0.0035;
    [
        ("BTCUSDT", 67_000.0),
        ("ETHUSDT", 3_300.0),
        ("SOLUSDT", 160.0),
        ("DOGEUSDT", 0.16),
    ]
    .into_iter()
    .filter_map(|(s, p)| {
        let mid = p * (1.0 + skew);
        sane(s.to_string(), mid * 0.9997, mid * 1.0003, mid, 5.0e6)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_exchange_parser_reads_its_own_shape() {
        let b = parse_binance_24h(&json!([
            {"symbol":"BTCUSDT","bidPrice":"81399.9","askPrice":"81400.0","lastPrice":"81400","quoteVolume":"9e9"},
            {"symbol":"ETHBTC","bidPrice":"0.029","askPrice":"0.0292","lastPrice":"0.029","quoteVolume":"1"},
            {"symbol":"XUSDT","bidPrice":"0","askPrice":"0","lastPrice":"0","quoteVolume":"0"}
        ]));
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(
            (b[0].symbol.as_str(), b[0].bid, b[0].ask),
            ("BTCUSDT", 81399.9, 81400.0)
        );

        let f = parse_binance_fut(
            &json!([{"symbol":"ETHUSDT","bidPrice":"2380.57","askPrice":"2380.58"}]),
            &json!([{"symbol":"ETHUSDT","quoteVolume":"5e9","lastPrice":"2380.6"}]),
        );
        assert_eq!(f[0].quote_volume, 5e9);

        let y = parse_bybit(&json!({"result":{"list":[
            {"symbol":"SOLUSDT","bid1Price":"160.1","ask1Price":"160.2","lastPrice":"160.15","turnover24h":"1e7"}]}}));
        assert_eq!(y[0].symbol, "SOLUSDT");
        assert_eq!(y[0].ask, 160.2);

        let o = parse_okx(&json!({"data":[
            {"instId":"BTC-USDT","bidPx":"81399","askPx":"81401","last":"81400","volCcy24h":"100"},
            {"instId":"BTC-USDT-SWAP","bidPx":"81390","askPx":"81391","last":"81390","volCcy24h":"10"},
            {"instId":"PEOPLE-EUR","bidPx":"1","askPx":"2","last":"1","volCcy24h":"1"}]}));
        assert_eq!(
            o.iter().map(|q| q.symbol.as_str()).collect::<Vec<_>>(),
            ["BTCUSDT", "BTCUSDT"]
        );
        assert_eq!(o[0].quote_volume, 100.0, "spot: already dollars");
        assert!(
            (o[1].quote_volume - 813_900.0).abs() < 1.0,
            "swap: coins x price"
        );

        let g = parse_gate(
            &json!([{"currency_pair":"RATS_USDT","highest_bid":"1","lowest_ask":"1.1","last":"1.05","quote_volume":"45842"}]),
        );
        assert_eq!(g[0].symbol, "RATSUSDT");

        let bg = parse_bitget(
            &json!({"data":[{"symbol":"TRXUSDT","bidPr":"0.33232","askPr":"0.33233","lastPr":"0.33232","usdtVolume":"1220854"}]}),
        );
        assert_eq!(bg[0].quote_volume, 1_220_854.0);

        let k = parse_kucoin(
            &json!({"data":{"ticker":[{"symbol":"CLANKER-USDT","buy":"11.162","sell":"11.214","last":"11.28","volValue":"10384"}]}}),
        );
        assert_eq!(
            (k[0].symbol.as_str(), k[0].bid, k[0].ask),
            ("CLANKERUSDT", 11.162, 11.214)
        );
    }

    #[test]
    fn crossed_or_empty_quotes_are_dropped() {
        assert!(sane("BTCUSDT".into(), 101.0, 100.0, 100.0, 1.0).is_none());
        assert!(sane("BTCUSDT".into(), 0.0, 100.0, 100.0, 1.0).is_none());
        assert!(sane("BTCEUR".into(), 100.0, 101.0, 100.0, 1.0).is_none());
        assert!(
            sane("BTCUSDT".into(), 100.0, 101.0, f64::NAN, 1.0)
                .unwrap()
                .last
                > 100.0
        );
    }

    #[test]
    fn instrument_names_round_trip() {
        let n = instrument("BTCUSDT", Exch::Bybit);
        assert_eq!(n, "BTCUSDT@BYBIT");
        assert_eq!(split_instrument(&n), Some(("BTCUSDT", Exch::Bybit)));
        assert_eq!(split_instrument("BTCUSDT"), None);
        assert_eq!(split_instrument("BTCUSDT@NOPE"), None);
        for e in ALL {
            assert_eq!(Exch::from_code(e.code()), Some(e));
        }
    }
}
