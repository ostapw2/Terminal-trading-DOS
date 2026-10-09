//! Yahoo Finance unofficial chart API client.
//!
//!   GET https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?range={range}&interval=1d
//!
//! No auth, no API key.  Treat as best-effort — Yahoo can change the
//! response shape or rate-limit at any time.  Falls back to synthetic
//! data in `markets.rs` on error.

use std::time::Duration;

use serde::Deserialize;

use crate::markets::Ohlc;

/// Range strings Yahoo accepts: `1d`, `5d`, `1mo`, `3mo`, `6mo`, `1y`, `2y`,
/// `5y`, `10y`, `ytd`, `max`.
/// Interval: `1m 2m 5m 15m 30m 60m 90m 1h 1d 5d 1wk 1mo 3mo`.  Sub-day
/// intervals are limited to recent history (Yahoo restricts 1m to 7 days).
pub fn fetch(ticker: &str, range: &str, interval: &str) -> Result<Vec<Ohlc>, FetchError> {
    if crate::data::demo::is_on() {
        let now = crate::data::demo::now_ms();
        return Ok(crate::data::demo::candles(ticker, range, interval, now));
    }
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?range={}&interval={}",
        urlencode(ticker),
        urlencode(range),
        urlencode(interval),
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent("dos/0.1.0 (https://github.com/...)")
        .build();

    let response = agent
        .get(&url)
        .call()
        .map_err(|e| FetchError::Http(Box::new(e)))?;
    let body: ChartResponse = response.into_json().map_err(FetchError::Json)?;

    if let Some(err) = body.chart.error {
        return Err(FetchError::Yahoo(err.description));
    }
    let result = body
        .chart
        .result
        .and_then(|mut v| v.pop())
        .ok_or(FetchError::Empty)?;

    let timestamps = result.timestamp.ok_or(FetchError::Empty)?;
    let q = result
        .indicators
        .quote
        .into_iter()
        .next()
        .ok_or(FetchError::Empty)?;

    let mut out = Vec::with_capacity(timestamps.len());
    for (i, ts) in timestamps.iter().enumerate() {
        let (Some(open), Some(high), Some(low), Some(close), Some(volume)) = (
            q.open.get(i).and_then(|x| *x),
            q.high.get(i).and_then(|x| *x),
            q.low.get(i).and_then(|x| *x),
            q.close.get(i).and_then(|x| *x),
            q.volume.get(i).and_then(|x| *x),
        ) else {
            continue;
        };
        out.push(Ohlc {
            open,
            high,
            low,
            close,
            volume: volume as f64,
            date: format_unix_date(*ts),
            time_ms: (*ts as i64).saturating_mul(1000),
            is_gap: false,
        });
    }
    if out.is_empty() {
        return Err(FetchError::Empty);
    }
    Ok(out)
}

#[derive(Debug)]
pub enum FetchError {
    Http(Box<ureq::Error>),
    Json(std::io::Error),
    Yahoo(String),
    Empty,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Http(e) => write!(f, "http: {}", e),
            FetchError::Json(e) => write!(f, "json: {}", e),
            FetchError::Yahoo(s) => write!(f, "yahoo: {}", s),
            FetchError::Empty => write!(f, "empty result"),
        }
    }
}

impl std::error::Error for FetchError {}

// ─── JSON DTOs ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ChartResponse {
    chart: Chart,
}

#[derive(Deserialize)]
struct Chart {
    #[serde(default)]
    result: Option<Vec<ChartResult>>,
    #[serde(default)]
    error: Option<YahooError>,
}

#[derive(Deserialize)]
struct YahooError {
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct ChartResult {
    #[serde(default)]
    timestamp: Option<Vec<u64>>,
    indicators: Indicators,
}

#[derive(Deserialize)]
struct Indicators {
    quote: Vec<QuoteSeries>,
}

#[derive(Deserialize)]
struct QuoteSeries {
    #[serde(default)]
    open: Vec<Option<f64>>,
    #[serde(default)]
    high: Vec<Option<f64>>,
    #[serde(default)]
    low: Vec<Option<f64>>,
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    volume: Vec<Option<u64>>,
}

// ─── helpers ─────────────────────────────────────────────────────────

fn urlencode(s: &str) -> String {
    // RFC 3986 unreserved; everything else gets %XX-encoded.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

fn format_unix_date(secs: u64) -> String {
    crate::markets::unix_secs_to_iso_date(secs as i64)
}
