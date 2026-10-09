//! Binance public market data client.
//!
//!   GET https://api.binance.com/api/v3/klines?symbol={S}&interval=1d&limit=100
//!
//! Public endpoints (klines / ticker / depth) need NO API key.  Authenticated
//! endpoints (account / trade / withdraw) require key + HMAC-SHA256 signed
//! secret — out of scope for v0.1, the keys live in settings as a
//! placeholder for the trading layer.
//!
//! Response shape:
//!   [
//!     [
//!       1499040000000,      // open time (ms)
//!       "0.01634790",       // open
//!       "0.80000000",       // high
//!       "0.01575800",       // low
//!       "0.01577100",       // close
//!       "148976.11427815",  // volume (base asset)
//!       1499644799999,      // close time
//!       "2434.19055334",    // quote asset volume
//!       308,                // trades count
//!       "1756.87402397",    // taker buy base
//!       "28.46694368",      // taker buy quote
//!       "17928899.62484339" // ignore
//!     ],
//!     ...
//!   ]

use std::time::Duration;

use crate::data::demo;
use crate::markets::Ohlc;

#[derive(Debug)]
pub enum BinanceError {
    Http(Box<ureq::Error>),
    Io(std::io::Error),
    Parse(String),
}

impl std::fmt::Display for BinanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinanceError::Http(e) => write!(f, "http: {}", e),
            BinanceError::Io(e) => write!(f, "io: {}", e),
            BinanceError::Parse(s) => write!(f, "parse: {}", s),
        }
    }
}

impl std::error::Error for BinanceError {}

/// Fetch the most recent `limit` daily klines for `symbol` (e.g. "BTCUSDT").
/// `interval` is one of `1m 3m 5m 15m 30m 1h 4h 1d 1w 1M`.
pub fn fetch_klines(
    symbol: &str,
    interval: &str,
    limit: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::klines(symbol, interval, limit, demo::now_ms()));
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/klines",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/klines",
    };
    let url = format!(
        "{}?symbol={}&interval={}&limit={}",
        base, symbol, interval, limit
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent("dos/0.1.0")
        .build();

    let response = agent
        .get(&url)
        .call()
        .map_err(|e| BinanceError::Http(Box::new(e)))?;
    let raw: serde_json::Value = response.into_json().map_err(BinanceError::Io)?;

    let arr = raw
        .as_array()
        .ok_or_else(|| BinanceError::Parse("response is not an array".into()))?;

    let mut out = Vec::with_capacity(arr.len());
    for row in arr {
        let row = row
            .as_array()
            .ok_or_else(|| BinanceError::Parse("kline row is not array".into()))?;
        if row.len() < 6 {
            return Err(BinanceError::Parse("kline row too short".into()));
        }
        let open_time = row[0].as_i64().unwrap_or(0);
        let open = parse_f64(&row[1])?;
        let high = parse_f64(&row[2])?;
        let low = parse_f64(&row[3])?;
        let close = parse_f64(&row[4])?;
        let volume = parse_f64(&row[5])?;
        out.push(Ohlc {
            open,
            high,
            low,
            close,
            volume: volume.max(0.0),
            date: format_unix_ms(open_time),
            time_ms: open_time,
            is_gap: false,
        });
    }
    Ok(out)
}

/// One aggregated trade from `/api/v3/aggTrades`.  `is_buyer_maker` tells us
/// who initiated the trade: `true` → buyer was passive (resting on book),
/// the SELLER hit the bid → aggressive SELL.  `false` → aggressive BUY.
/// This is what we use to classify volume into bid (sell) vs ask (buy)
/// for a footprint chart.
#[derive(Clone, Debug)]
pub struct AggTrade {
    pub price: f64,
    pub qty: f64,
    pub time_ms: i64,
    pub is_buyer_maker: bool,
}

/// Pull recent aggregated trades for a Binance Spot symbol.  Returns up to
/// `limit` trades (Binance hard-caps at 1000).  Order is oldest → newest.
pub fn fetch_agg_trades(
    symbol: &str,
    limit: u32,
    venue: Venue,
) -> Result<Vec<AggTrade>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::agg_trades(symbol, limit.min(1000), demo::now_ms()));
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/aggTrades",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/aggTrades",
    };
    let url = format!("{}?symbol={}&limit={}", base, symbol, limit);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .user_agent("dos/0.1.0")
        .build();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| BinanceError::Http(Box::new(e)))?;
    let raw: serde_json::Value = resp.into_json().map_err(BinanceError::Io)?;
    let arr = raw
        .as_array()
        .ok_or_else(|| BinanceError::Parse("aggTrades not array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for row in arr {
        let price = parse_f64(row.get("p").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let qty = parse_f64(row.get("q").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let time_ms = row.get("T").and_then(|v| v.as_i64()).unwrap_or(0);
        let is_buyer_maker = row.get("m").and_then(|v| v.as_bool()).unwrap_or(false);
        if price <= 0.0 || qty <= 0.0 {
            continue;
        }
        out.push(AggTrade {
            price,
            qty,
            time_ms,
            is_buyer_maker,
        });
    }
    Ok(out)
}

/// Same as `fetch_depth` but also returns `lastUpdateId` for diff-stream
/// synchronisation.
pub fn fetch_depth_with_id(
    symbol: &str,
    limit: u32,
    venue: Venue,
) -> Result<(crate::data::orderbook::OrderBookSnapshot, u64), BinanceError> {
    let limit = limit.clamp(5, 5000);
    if demo::is_on() {
        return Ok(demo::depth(symbol, limit));
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/depth",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/depth",
    };
    let url = format!("{}?symbol={}&limit={}", base, symbol, limit);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent("dos/0.1.0")
        .build();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| BinanceError::Http(Box::new(e)))?;
    let raw: serde_json::Value = resp.into_json().map_err(BinanceError::Io)?;
    crate::data::orderbook::parse_depth_with_id(&raw)
        .ok_or_else(|| BinanceError::Parse("depth: parse failed".into()))
}

/// Look up the price `tickSize` for one symbol via `/api/v3/exchangeInfo`.
/// Falls back to a heuristic (price magnitude) if the endpoint fails or the
/// symbol isn't found.
pub fn fetch_tick_size(symbol: &str, venue: Venue) -> Result<f64, BinanceError> {
    if demo::is_on() {
        return Ok(demo::tick_size(symbol));
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/exchangeInfo",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/exchangeInfo",
    };
    let url = format!("{}?symbol={}", base, symbol);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent("dos/0.1.0")
        .build();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| BinanceError::Http(Box::new(e)))?;
    let raw: serde_json::Value = resp.into_json().map_err(BinanceError::Io)?;
    let symbols = raw
        .get("symbols")
        .and_then(|v| v.as_array())
        .ok_or_else(|| BinanceError::Parse("exchangeInfo: no symbols".into()))?;
    for sym in symbols {
        let s = sym.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        if s != symbol {
            continue;
        }
        let filters = sym
            .get("filters")
            .and_then(|v| v.as_array())
            .ok_or_else(|| BinanceError::Parse("no filters".into()))?;
        for f in filters {
            let ftype = f.get("filterType").and_then(|v| v.as_str()).unwrap_or("");
            if ftype == "PRICE_FILTER" {
                if let Some(ts) = f.get("tickSize") {
                    if let Ok(v) = parse_f64(ts) {
                        if v > 0.0 {
                            return Ok(v);
                        }
                    }
                }
            }
        }
    }
    Err(BinanceError::Parse("PRICE_FILTER not found".into()))
}

/// Heuristic tick size when exchangeInfo isn't available — based on price
/// magnitude.  Better than guessing 0.01 for sub-dollar tokens.
pub fn heuristic_tick_size(price: f64) -> f64 {
    if price <= 0.0 || !price.is_finite() {
        return 0.01;
    }
    let mag = price.log10().floor();
    if mag >= 4.0 {
        // BTC, ETH-like: e.g. 60000 → 0.10
        0.10
    } else if mag >= 2.0 {
        0.01
    } else if mag >= 0.0 {
        0.0001
    } else if mag >= -2.0 {
        0.000001
    } else {
        0.0000001
    }
}

/// One row of `/api/v3/ticker/24hr` (or fapi `/fapi/v1/ticker/24hr`) — a
/// rolling 24-hour summary per trading pair.  We keep just the fields the
/// screener needs.
#[derive(Clone, Debug)]
pub struct TickerSummary {
    pub symbol: String, // e.g. "BTCUSDT"
    pub last_price: f64,
    pub change_pct: f64, // 24h % change
    pub high_24h: f64,
    pub low_24h: f64,
    pub volume_base: f64,  // base-asset volume
    pub volume_quote: f64, // quote-asset volume (USDT for *USDT pairs)
}

/// Source for the live screener.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Venue {
    Spot,
    Futures,
}

impl Venue {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Spot => "Spot",
            Self::Futures => "Futures",
        }
    }
}

/// Pull all 24h ticker summaries from Binance Spot or USDT-margined Futures.
/// Filters to pairs ending in "USDT" so quote_volume is in USD.
pub fn fetch_24h_summary(venue: Venue) -> Result<Vec<TickerSummary>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::summaries(demo::now_ms()));
    }
    let url = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/ticker/24hr",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/ticker/24hr",
    };
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .user_agent("dos/0.1.0")
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| BinanceError::Http(Box::new(e)))?;
    let raw: serde_json::Value = resp.into_json().map_err(BinanceError::Io)?;
    let arr = raw
        .as_array()
        .ok_or_else(|| BinanceError::Parse("24h response not array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for row in arr {
        let symbol = row
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !symbol.ends_with("USDT") {
            continue;
        }
        let last_price =
            parse_f64(row.get("lastPrice").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let change_pct = parse_f64(
            row.get("priceChangePercent")
                .unwrap_or(&serde_json::Value::Null),
        )
        .unwrap_or(0.0);
        let high_24h =
            parse_f64(row.get("highPrice").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let low_24h =
            parse_f64(row.get("lowPrice").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let volume_base =
            parse_f64(row.get("volume").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        let volume_quote =
            parse_f64(row.get("quoteVolume").unwrap_or(&serde_json::Value::Null)).unwrap_or(0.0);
        if last_price <= 0.0 {
            continue;
        }
        out.push(TickerSummary {
            symbol,
            last_price,
            change_pct,
            high_24h,
            low_24h,
            volume_base,
            volume_quote,
        });
    }
    Ok(out)
}

/// Map our display ticker to the Binance USDT pair convention.
///   BTC  → BTCUSDT
///   ETH  → ETHUSDT
///   ...
pub fn binance_symbol(ticker: &str) -> String {
    if ticker.ends_with("USDT") {
        ticker.to_string()
    } else {
        format!("{}USDT", ticker)
    }
}

fn parse_f64(v: &serde_json::Value) -> Result<f64, BinanceError> {
    if let Some(n) = v.as_f64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return s
            .parse::<f64>()
            .map_err(|e| BinanceError::Parse(format!("f64 from str: {}", e)));
    }
    Err(BinanceError::Parse(format!("not a number: {:?}", v)))
}

fn format_unix_ms(ms: i64) -> String {
    crate::markets::unix_secs_to_iso_date(ms / 1000)
}
