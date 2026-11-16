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
    /// Binance refuses requests from this IP until this time (ms): 418 / 429,
    /// or our own pause when the minute's weight is nearly used up.
    Paused(i64),
}

impl std::fmt::Display for BinanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinanceError::Http(e) => write!(f, "http: {}", e),
            BinanceError::Io(e) => write!(f, "io: {}", e),
            BinanceError::Parse(s) => write!(f, "parse: {}", s),
            BinanceError::Paused(until) => {
                let left = ((until - now_ms()) / 1000).max(1);
                write!(
                    f,
                    "Binance limits requests from this computer: waiting {left} s before asking again"
                )
            }
        }
    }
}

impl std::error::Error for BinanceError {}

/// Fetch the most recent `limit` daily klines for `symbol` (e.g. "BTCUSDT").
/// `interval` is one of `1m 3m 5m 15m 30m 1h 4h 1d 1w 1M`.
///
/// Served from this process's candle cache when it can: a series asked for
/// before costs one small request for its newest candles ([`klines_cached`]).
pub fn fetch_klines(
    symbol: &str,
    interval: &str,
    limit: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    klines_cached(symbol, interval, limit, venue)
}

// ── candle cache ─────────────────────────────────────────────────────────

/// One cached series: oldest first; `complete` when the exchange had no
/// older candles than these.
struct CachedSeries {
    candles: Vec<Ohlc>,
    complete: bool,
    used: u64,
}

/// Venue and kind of a series in `market.db`'s catalogue.
fn disk_name(key: &CacheKey) -> (&'static str, &'static str) {
    if key.0 {
        ("BINANCEF", "perp")
    } else {
        ("BINANCE", "spot")
    }
}

/// The newest `want` candles the disk holds without a hole (any window, any
/// earlier run put them there).  Tests and demo never read the disk.
fn disk_load(key: &CacheKey, want: u32) -> Option<Vec<Ohlc>> {
    if cfg!(test) {
        return None;
    }
    let (venue, kind) = disk_name(key);
    let step = interval_ms(&key.2);
    Some(crate::data::marketdb::series_load(
        venue,
        kind,
        &key.1,
        &key.2,
        want as usize,
        Some(step),
    ))
}

/// Write the closed ones of `candles`: the whole series once, then only the
/// candles a top-up brought.
fn disk_save(key: &CacheKey, candles: &[Ohlc]) {
    if cfg!(test) {
        return;
    }
    let (venue, kind) = disk_name(key);
    let now = now_ms();
    let open_from = now - interval_ms(&key.2) + 1;
    crate::data::marketdb::series_save(venue, kind, &key.1, &key.2, candles, open_from, now);
}

/// Candles of `symbol` on disk for a first paint while the fetch runs.
pub fn klines_on_disk(symbol: &str, interval: &str, want: u32, venue: Venue) -> Vec<Ohlc> {
    if demo::is_on() {
        return Vec::new();
    }
    let key: CacheKey = (
        venue == Venue::Futures,
        symbol.to_string(),
        interval.to_string(),
    );
    disk_load(&key, want).unwrap_or_default()
}

type CacheKey = (bool, String, String);

/// Series kept at most (least recently used ones go first).
const CACHE_SERIES: usize = 256;

fn cache() -> &'static std::sync::Mutex<(u64, std::collections::HashMap<CacheKey, CachedSeries>)> {
    static C: std::sync::OnceLock<
        std::sync::Mutex<(u64, std::collections::HashMap<CacheKey, CachedSeries>)>,
    > = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Length of one candle of `interval` in ms (a month counts 31 days).
pub fn interval_ms(interval: &str) -> i64 {
    let (n, unit) = interval.split_at(interval.len().saturating_sub(1));
    let n: i64 = n.parse().unwrap_or(1).max(1);
    n * match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 7 * 86_400_000,
        "M" => 31 * 86_400_000,
        _ => 60_000,
    }
}

/// Put `fresh` (the newest candles, oldest first) over the end of `cached`:
/// candles from the first fresh one on are replaced (the forming candle
/// changes until it closes).
pub fn merge_tail(cached: &mut Vec<Ohlc>, fresh: Vec<Ohlc>) {
    let Some(from) = fresh.first().map(|c| c.time_ms) else {
        return;
    };
    cached.retain(|c| c.time_ms < from);
    cached.extend(fresh);
}

/// The newest `want` candles of a series: from the cache plus the candles
/// since its newest one (one request of a few candles, weight 1) when this
/// process asked for the series before; else the whole series, then kept.
/// A cache too far behind (more than a page of new candles) is refetched.
pub fn klines_cached(
    symbol: &str,
    interval: &str,
    want: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    let want = want.clamp(1, MAX_HISTORY);
    if demo::is_on() {
        return Ok(demo::klines(symbol, interval, want, demo::now_ms()));
    }
    let key: CacheKey = (
        venue == Venue::Futures,
        symbol.to_string(),
        interval.to_string(),
    );
    let mut have = {
        let c = cache()
            .lock()
            .map_err(|_| BinanceError::Parse("cache lock".into()))?;
        c.1.get(&key)
            .filter(|s| s.complete || s.candles.len() >= want as usize)
            .and_then(|s| s.candles.last().map(|l| l.time_ms))
    };
    // Not in memory: the last run may have left it on disk.
    let known = cache()
        .lock()
        .map(|c| c.1.contains_key(&key))
        .unwrap_or(true);
    if have.is_none() && !known {
        // The disk holds closed candles only: enough when it and the top-up
        // since its newest one cover what is asked.
        let covers = |o: &Vec<Ohlc>| {
            o.last().is_some_and(|l| {
                let tail = (now_ms() - l.time_ms) / interval_ms(interval);
                o.len() as i64 + tail >= want as i64
            })
        };
        if let Some(old) = disk_load(&key, want).filter(covers) {
            have = old.last().map(|l| l.time_ms);
            if let Ok(mut c) = cache().lock() {
                c.0 += 1;
                let used = c.0;
                c.1.insert(
                    key.clone(),
                    CachedSeries {
                        candles: old,
                        complete: false,
                        used,
                    },
                );
            }
        }
    }
    if let Some(last) = have {
        let behind = (now_ms() - last) / interval_ms(interval) + 2;
        if behind <= 1000 {
            let fresh =
                fetch_klines_range(symbol, interval, behind as u32, venue, Some(last), None)?;
            let mut c = cache()
                .lock()
                .map_err(|_| BinanceError::Parse("cache lock".into()))?;
            c.0 += 1;
            let tick = c.0;
            if let Some(s) = c.1.get_mut(&key) {
                merge_tail(&mut s.candles, fresh.clone());
                let keep = s.candles.len().saturating_sub(MAX_HISTORY as usize);
                s.candles.drain(..keep);
                s.used = tick;
                let from = s.candles.len().saturating_sub(want as usize);
                let out = s.candles[from..].to_vec();
                drop(c);
                disk_save(&key, &fresh);
                return Ok(out);
            }
        }
    }
    let full = if want <= 1000 {
        fetch_klines_range(symbol, interval, want, venue, None, None)?
    } else {
        fetch_history_pages(symbol, interval, want, venue)?
    };
    let mut c = cache()
        .lock()
        .map_err(|_| BinanceError::Parse("cache lock".into()))?;
    c.0 += 1;
    let tick = c.0;
    // A longer series already cached (asked for by someone else) stays.
    let keep_old = c.1.get(&key).is_some_and(|s| s.candles.len() > full.len());
    let mut saved = false;
    if !keep_old {
        c.1.insert(
            key.clone(),
            CachedSeries {
                complete: full.len() < want as usize,
                candles: full.clone(),
                used: tick,
            },
        );
        saved = true;
    }
    if c.1.len() > CACHE_SERIES {
        if let Some(old) =
            c.1.iter()
                .min_by_key(|(_, s)| s.used)
                .map(|(k, _)| k.clone())
        {
            c.1.remove(&old);
        }
    }
    drop(c);
    if saved {
        disk_save(&key, &full);
    }
    Ok(full)
}

/// The latest funding rate (per 8 h, a fraction: 0.0001 = 0.01 %) of every
/// USDT perpetual, one request (`premiumIndex`).
pub fn fetch_funding_rates() -> Result<std::collections::HashMap<String, f64>, BinanceError> {
    if demo::is_on() {
        return Ok([
            ("BTCUSDT".to_string(), 0.0001),
            ("ETHUSDT".to_string(), 0.00008),
        ]
        .into());
    }
    let raw = get_json("https://fapi.binance.com/fapi/v1/premiumIndex")?;
    let arr = raw
        .as_array()
        .ok_or_else(|| BinanceError::Parse("premiumIndex not an array".into()))?;
    let mut out = std::collections::HashMap::new();
    for row in arr {
        let (Some(sym), Some(rate)) = (
            row.get("symbol").and_then(|v| v.as_str()),
            row.get("lastFundingRate").and_then(|v| parse_f64(v).ok()),
        ) else {
            continue;
        };
        if sym.ends_with("USDT") && rate.is_finite() {
            out.insert(sym.to_string(), rate);
        }
    }
    Ok(out)
}

/// Funding settlements of a USDT perpetual from `start_ms` on, oldest first:
/// (time ms, rate as a fraction), pages of 1000 forward (weight 1 each).
pub fn fetch_funding_history(symbol: &str, start_ms: i64) -> Result<Vec<(i64, f64)>, BinanceError> {
    if demo::is_on() {
        return Ok(Vec::new());
    }
    let mut out: Vec<(i64, f64)> = Vec::new();
    let mut from = start_ms;
    for _ in 0..20 {
        let raw = get_json(&format!(
            "https://fapi.binance.com/fapi/v1/fundingRate?symbol={symbol}&startTime={from}&limit=1000"
        ))?;
        let page: Vec<(i64, f64)> = raw
            .as_array()
            .ok_or_else(|| BinanceError::Parse("fundingRate not an array".into()))?
            .iter()
            .filter_map(|r| {
                Some((
                    r.get("fundingTime")?.as_i64()?,
                    parse_f64(r.get("fundingRate")?).ok()?,
                ))
            })
            .collect();
        let Some(&(last, _)) = page.last() else {
            break;
        };
        let full = page.len() >= 1000;
        out.extend(page);
        if !full {
            break;
        }
        from = last + 1;
    }
    Ok(out)
}

/// The newest `limit` candles of one request, straight from the exchange (the
/// demo's own in demo mode), kept out of the candle cache: for bulk jobs over
/// hundreds of coins that must not push the charts' series out of it.
pub fn fetch_klines_uncached(
    symbol: &str,
    interval: &str,
    limit: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::klines(symbol, interval, limit, demo::now_ms()));
    }
    fetch_klines_range(symbol, interval, limit, venue, None, None)
}

/// The newest `want` candles through the disk store only: what `market.db`
/// holds plus the candles since its newest one, else the whole series; the
/// new ones written back.  Kept out of the memory cache, so bulk history
/// (hundreds of coins, days of minutes) does not push the charts' series
/// out of it.
pub fn fetch_klines_stored(
    symbol: &str,
    interval: &str,
    want: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    let want = want.clamp(1, MAX_HISTORY);
    if demo::is_on() {
        return Ok(demo::klines(symbol, interval, want, demo::now_ms()));
    }
    let key: CacheKey = (
        venue == Venue::Futures,
        symbol.to_string(),
        interval.to_string(),
    );
    let step = interval_ms(interval);
    if let Some(mut old) = disk_load(&key, want).filter(|o| !o.is_empty()) {
        let last = old.last().map_or(0, |l| l.time_ms);
        let behind = (now_ms() - last) / step + 2;
        if old.len() as i64 + behind - 2 >= want as i64 && behind <= 1000 {
            let fresh =
                fetch_klines_range(symbol, interval, behind as u32, venue, Some(last), None)?;
            disk_save(&key, &fresh);
            merge_tail(&mut old, fresh);
            let from = old.len().saturating_sub(want as usize);
            return Ok(old.split_off(from));
        }
    }
    let full = if want <= 1000 {
        fetch_klines_range(symbol, interval, want, venue, None, None)?
    } else {
        fetch_history_pages(symbol, interval, want, venue)?
    };
    disk_save(&key, &full);
    Ok(full)
}

/// Longest history [`fetch_klines_history`] gathers.
pub const MAX_HISTORY: u32 = 20_000;

/// Up to `total` candles, newest last: pages of 1000 going back in time
/// (one request each), as far as the exchange has them.
///
/// Cached like [`fetch_klines`]: asked again, it costs one small request.
pub fn fetch_klines_history(
    symbol: &str,
    interval: &str,
    total: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    klines_cached(symbol, interval, total, venue)
}

/// [`fetch_klines_history`] without the cache: pages of 1000 back in time.
fn fetch_history_pages(
    symbol: &str,
    interval: &str,
    total: u32,
    venue: Venue,
) -> Result<Vec<Ohlc>, BinanceError> {
    let total = total.min(MAX_HISTORY);
    let mut pages: Vec<Vec<Ohlc>> = Vec::new();
    let mut have = 0u32;
    let mut end: Option<i64> = None;
    while have < total {
        let page = fetch_klines_before(symbol, interval, (total - have).min(1000), venue, end)?;
        let Some(first) = page.first().map(|k| k.time_ms) else {
            break;
        };
        have += page.len() as u32;
        let short = page.len() < 1000;
        pages.push(page);
        if short {
            break; // the exchange has no older candles
        }
        end = Some(first - 1);
    }
    Ok(merge_pages(pages))
}

/// Pages of candles (any order) as one series: oldest first, each time once.
pub fn merge_pages(pages: Vec<Vec<Ohlc>>) -> Vec<Ohlc> {
    let mut all: Vec<Ohlc> = pages.into_iter().flatten().collect();
    all.sort_by_key(|k| k.time_ms);
    all.dedup_by_key(|k| k.time_ms);
    all
}

/// A futures reading over time (not candles): `FR:`, `OI:`, `LS:`, `TK:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuturesData {
    /// Funding rate, % per 8 h (about a year of history in one page).
    Funding,
    /// Open interest in coins (last 30 days).
    OpenInterest,
    /// Long/short ratio of accounts (last 30 days).
    LongShort,
    /// Taker buy/sell volume ratio (last 30 days).
    Taker,
}

/// Readings of `what` for `symbol`, oldest first, as flat candles
/// (open = high = low = close = the reading).  `period` (`5m 15m 30m 1h
/// 4h 1d`) applies to the 30-day series; funding comes every 8 h.  Pages
/// back in time up to `total` (the exchange keeps 30 days of the ratios).
pub fn fetch_futures_data(
    what: FuturesData,
    symbol: &str,
    period: &str,
    total: u32,
) -> Result<Vec<Ohlc>, BinanceError> {
    let total = total.min(MAX_HISTORY);
    let (path, page, field, time_key, scale) = match what {
        FuturesData::Funding => (
            "/fapi/v1/fundingRate",
            1000,
            "fundingRate",
            "fundingTime",
            100.0,
        ),
        FuturesData::OpenInterest => (
            "/futures/data/openInterestHist",
            500,
            "sumOpenInterest",
            "timestamp",
            1.0,
        ),
        FuturesData::LongShort => (
            "/futures/data/globalLongShortAccountRatio",
            500,
            "longShortRatio",
            "timestamp",
            1.0,
        ),
        FuturesData::Taker => (
            "/futures/data/takerlongshortRatio",
            500,
            "buySellRatio",
            "timestamp",
            1.0,
        ),
    };
    if demo::is_on() {
        let (kind, step) = match what {
            FuturesData::Funding => ("fr", 8 * 3_600_000),
            FuturesData::OpenInterest => ("oi", period_ms(period)),
            FuturesData::LongShort => ("ls", period_ms(period)),
            FuturesData::Taker => ("tk", period_ms(period)),
        };
        return Ok(demo::futures_readings(
            kind,
            symbol,
            step,
            total.min(page * 4),
            demo::now_ms(),
        ));
    }
    let mut pages: Vec<Vec<Ohlc>> = Vec::new();
    let mut have = 0u32;
    let mut end: Option<i64> = None;
    while have < total {
        let mut url = format!(
            "https://fapi.binance.com{path}?symbol={symbol}&limit={}",
            (total - have).min(page)
        );
        if what != FuturesData::Funding {
            url.push_str(&format!("&period={period}"));
        }
        if let Some(e) = end {
            url.push_str(&format!("&endTime={e}"));
        }
        let raw = get_json(&url)?;
        let rows = raw
            .as_array()
            .ok_or_else(|| BinanceError::Parse("response is not an array".into()))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let t = r[time_key].as_i64().unwrap_or(0);
            let v = parse_f64(&r[field])? * scale;
            if t > 0 && v.is_finite() {
                out.push(Ohlc {
                    open: v,
                    high: v,
                    low: v,
                    close: v,
                    date: format_unix_ms(t),
                    time_ms: t,
                    ..Default::default()
                });
            }
        }
        let Some(first) = out.iter().map(|k| k.time_ms).min() else {
            break;
        };
        have += out.len() as u32;
        let short = (out.len() as u32) < page;
        pages.push(out);
        if short {
            break;
        }
        end = Some(first - 1);
    }
    Ok(merge_pages(pages))
}

fn period_ms(period: &str) -> i64 {
    match period {
        "5m" => 300_000,
        "15m" => 900_000,
        "30m" => 1_800_000,
        "1h" => 3_600_000,
        "2h" => 7_200_000,
        "4h" => 14_400_000,
        "6h" => 21_600_000,
        "12h" => 43_200_000,
        _ => 86_400_000,
    }
}

pub(crate) fn get_json(url: &str) -> Result<serde_json::Value, BinanceError> {
    call(url, 10)?.into_json().map_err(BinanceError::Io)
}

/// Until when (ms) this process asks Binance nothing: set by a 418 / 429
/// answer, or by our own pause near the minute's weight limit.
static PAUSED_UNTIL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Pause at this share of the IP's request weight per minute (6000 on Spot):
/// the header counts every window of every program on this computer.
const WEIGHT_PAUSE_AT: u64 = 4800;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// This process's requests of the last minute: (time ms, endpoint path).
fn recent_calls() -> &'static std::sync::Mutex<std::collections::VecDeque<(i64, String)>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<(i64, String)>>> =
        std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

fn note_call(url: &str) {
    let path = url
        .split("://")
        .nth(1)
        .and_then(|r| r.split_once('/'))
        .map_or(url, |(_, p)| p)
        .split('?')
        .next()
        .unwrap_or("")
        .to_string();
    // Candles: which interval, so a busy caller can be told apart.
    let interval = url
        .split("interval=")
        .nth(1)
        .and_then(|r| r.split('&').next())
        .map(|i| format!(" {i}"))
        .unwrap_or_default();
    let now = now_ms();
    if let Ok(mut q) = recent_calls().lock() {
        q.push_back((now, format!("/{path}{interval}")));
        while q.front().is_some_and(|(t, _)| now - t > 60_000) {
            q.pop_front();
        }
    }
}

/// This process's requests in the last minute, per endpoint, most first.
pub fn calls_last_minute() -> Vec<(String, usize)> {
    let now = now_ms();
    let mut by: std::collections::HashMap<String, usize> = Default::default();
    if let Ok(q) = recent_calls().lock() {
        for (_, p) in q.iter().filter(|(t, _)| now - t <= 60_000) {
            *by.entry(p.clone()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// The IP's used weight this minute as the last answer reported it.
static LAST_WEIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_WEIGHT_AT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The request weight this IP used in the current minute (every program on
/// this computer), from the newest answer; `None` when none came this minute.
pub fn used_weight() -> Option<u64> {
    use std::sync::atomic::Ordering;
    let at = LAST_WEIGHT_AT.load(Ordering::Relaxed);
    (at > 0 && at / 60_000 == now_ms() / 60_000).then(|| LAST_WEIGHT.load(Ordering::Relaxed))
}

/// Is Binance REST paused now, and until when?
pub fn paused_until() -> Option<i64> {
    let t = PAUSED_UNTIL.load(std::sync::atomic::Ordering::Relaxed);
    (t > now_ms()).then_some(t)
}

/// When a refusal says how long: `"... IP banned until 1791712736435 ..."`
/// (ms) or `Retry-After` (s); else a minute.
fn refusal_until(code: u16, body: &str, retry_after: Option<&str>, now: i64) -> i64 {
    let banned = body
        .split("until ")
        .nth(1)
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<i64>().ok())
        .filter(|t| *t > now);
    let retry = retry_after
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|s| now + s * 1000);
    banned
        .or(retry)
        .unwrap_or(now + if code == 418 { 120_000 } else { 60_000 })
}

/// GET from Binance REST, minding its limits: nothing is sent while paused;
/// a 418 / 429 pauses until the time it gives; a minute whose used weight
/// passes `WEIGHT_PAUSE_AT` pauses until the next minute.  Every request of
/// this module goes through here.
fn call(url: &str, read_secs: u64) -> Result<ureq::Response, BinanceError> {
    use std::sync::atomic::Ordering;
    if let Some(t) = paused_until() {
        return Err(BinanceError::Paused(t));
    }
    note_call(url);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(read_secs))
        .user_agent("dos/0.1.0")
        .build();
    match agent.get(url).call() {
        Ok(resp) => {
            let used = resp
                .header("x-mbx-used-weight-1m")
                .and_then(|v| v.trim().parse::<u64>().ok());
            if let Some(w) = used {
                LAST_WEIGHT.store(w, Ordering::Relaxed);
                LAST_WEIGHT_AT.store(now_ms(), Ordering::Relaxed);
            }
            if used.is_some_and(|w| w >= WEIGHT_PAUSE_AT) {
                let now = now_ms();
                PAUSED_UNTIL.fetch_max(now - now % 60_000 + 61_000, Ordering::Relaxed);
            }
            Ok(resp)
        }
        Err(ureq::Error::Status(code @ (418 | 429), resp)) => {
            let retry = resp.header("retry-after").map(str::to_string);
            let body = resp.into_string().unwrap_or_default();
            let until = refusal_until(code, &body, retry.as_deref(), now_ms());
            PAUSED_UNTIL.fetch_max(until, Ordering::Relaxed);
            Err(BinanceError::Paused(until))
        }
        Err(e) => Err(BinanceError::Http(Box::new(e))),
    }
}

fn fetch_klines_before(
    symbol: &str,
    interval: &str,
    limit: u32,
    venue: Venue,
    end_ms: Option<i64>,
) -> Result<Vec<Ohlc>, BinanceError> {
    fetch_klines_range(symbol, interval, limit, venue, None, end_ms)
}

/// Up to `limit` candles from `start_ms` on, or ending at `end_ms`, or the
/// newest; one request.
fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    limit: u32,
    venue: Venue,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<Vec<Ohlc>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::klines(symbol, interval, limit, demo::now_ms()));
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/klines",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/klines",
    };
    let mut url = format!(
        "{}?symbol={}&interval={}&limit={}",
        base, symbol, interval, limit
    );
    if let Some(start) = start_ms {
        url.push_str(&format!("&startTime={start}"));
    }
    if let Some(end) = end_ms {
        url.push_str(&format!("&endTime={end}"));
    }

    let response = call(&url, 10)?;
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
    fetch_agg_trades_from(symbol, limit, venue, None)
}

/// Like [`fetch_agg_trades`] but only trades at or after `start_ms` (the
/// exchange accepts start times up to an hour back; older ones are clamped).
pub fn fetch_agg_trades_from(
    symbol: &str,
    limit: u32,
    venue: Venue,
    start_ms: Option<i64>,
) -> Result<Vec<AggTrade>, BinanceError> {
    let start_ms = start_ms.map(|t| t.max(demo::now_ms() - 55 * 60_000));
    if demo::is_on() {
        let mut v = demo::agg_trades(symbol, limit.min(1000), demo::now_ms());
        if let Some(t) = start_ms {
            v.retain(|x| x.time_ms >= t);
        }
        return Ok(v);
    }
    let base = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/aggTrades",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/aggTrades",
    };
    let mut url = format!("{}?symbol={}&limit={}", base, symbol, limit);
    if let Some(t) = start_ms {
        url.push_str(&format!("&startTime={t}"));
    }
    let resp = call(&url, 15)?;
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
    let resp = call(&url, 10)?;
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
    let resp = call(&url, 10)?;
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

/// Every instrument a Binance venue lists (`exchangeInfo`, weight 20 on
/// Spot, ~6 MB; 1 on Futures): steps, minimum order, whether it trades,
/// what kind of asset it is.  Futures: perpetuals only (quarterlies left
/// out).  Nothing in demo mode.
pub fn fetch_listings(venue: Venue) -> Result<Vec<crate::data::marketdb::Listing>, BinanceError> {
    if demo::is_on() {
        return Ok(Vec::new());
    }
    let raw = get_json(match venue {
        Venue::Spot => "https://api.binance.com/api/v3/exchangeInfo",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/exchangeInfo",
    })?;
    let rows = parse_listings(&raw, venue);
    if rows.is_empty() {
        return Err(BinanceError::Parse("exchangeInfo: no symbols".into()));
    }
    Ok(rows)
}

/// Spot's trading group of tokenized stocks and ETFs (`TSLAB`, `SPYB`): all
/// 94 of them carry it and no coin does (seen live 20.10.2026).  Spot has
/// no asset type of its own; Futures has (`underlyingType`).
const SPOT_STOCK_GROUP: &str = "TRD_GRP_261";

/// [`fetch_listings`] over a parsed reply.
pub fn parse_listings(
    raw: &serde_json::Value,
    venue: Venue,
) -> Vec<crate::data::marketdb::Listing> {
    let Some(arr) = raw.get("symbols").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let symbol = text(x, "symbol");
        if symbol.is_empty() {
            continue;
        }
        let asset = match venue {
            Venue::Spot => {
                let groups = x.get("permissionSets").and_then(|v| v.as_array());
                let stock = groups.is_some_and(|sets| {
                    sets.iter()
                        .filter_map(|set| set.as_array())
                        .flatten()
                        .any(|g| g.as_str() == Some(SPOT_STOCK_GROUP))
                });
                if stock {
                    "stock"
                } else {
                    "crypto"
                }
            }
            Venue::Futures => {
                if !text(x, "contractType").contains("PERPETUAL") {
                    continue;
                }
                match text(x, "underlyingType").as_str() {
                    "COIN" => "crypto",
                    "INDEX" => "index",
                    "COMMODITY" => "commodity",
                    "FX" => "fx",
                    t if t.contains("EQUITY") || t == "PREMARKET" => "stock",
                    _ => "other",
                }
            }
        };
        let (mut tick, mut lot, mut min_notional) = (0.0, 0.0, 0.0);
        for f in x
            .get("filters")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let num = |k: &str| f.get(k).and_then(|v| parse_f64(v).ok()).unwrap_or(0.0);
            match f.get("filterType").and_then(|v| v.as_str()).unwrap_or("") {
                "PRICE_FILTER" => tick = num("tickSize"),
                "LOT_SIZE" => lot = num("stepSize"),
                "NOTIONAL" => min_notional = num("minNotional"),
                "MIN_NOTIONAL" => min_notional = num("notional").max(num("minNotional")),
                _ => {}
            }
        }
        out.push(crate::data::marketdb::Listing {
            base: text(x, "baseAsset"),
            quote: text(x, "quoteAsset"),
            active: text(x, "status") == "TRADING",
            symbol,
            tick,
            lot,
            min_notional,
            asset,
        });
    }
    out
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
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    let resp = call(url, 15)?;
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

/// Last price of every USDT pair: `/api/v3/ticker/price` (weight 4) or
/// `/fapi/v1/ticker/price` (weight 2) — what positions need, at a twentieth
/// of the 24h summary's weight.
pub fn fetch_prices(venue: Venue) -> Result<Vec<(String, f64)>, BinanceError> {
    if demo::is_on() {
        return Ok(demo::summaries(demo::now_ms())
            .into_iter()
            .map(|t| (t.symbol, t.last_price))
            .collect());
    }
    let url = match venue {
        Venue::Spot => "https://api.binance.com/api/v3/ticker/price",
        Venue::Futures => "https://fapi.binance.com/fapi/v1/ticker/price",
    };
    let raw: serde_json::Value = call(url, 10)?.into_json().map_err(BinanceError::Io)?;
    Ok(parse_prices(&raw))
}

fn parse_prices(raw: &serde_json::Value) -> Vec<(String, f64)> {
    raw.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let s = r.get("symbol")?.as_str()?;
                    let p = parse_f64(r.get("price")?).ok()?;
                    (s.ends_with("USDT") && p > 0.0).then(|| (s.to_string(), p))
                })
                .collect()
        })
        .unwrap_or_default()
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

pub(crate) fn parse_f64(v: &serde_json::Value) -> Result<f64, BinanceError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_tell_stocks_from_coins_and_keep_the_steps() {
        let spot = serde_json::json!({"symbols": [
            {"symbol": "BTCUSDT", "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "USDT",
             "permissionSets": [["SPOT", "MARGIN", "TRD_GRP_004"]],
             "filters": [
                {"filterType": "PRICE_FILTER", "tickSize": "0.01000000"},
                {"filterType": "LOT_SIZE", "stepSize": "0.00001000"},
                {"filterType": "NOTIONAL", "minNotional": "5.00000000"}]},
            {"symbol": "TSLABUSDT", "status": "TRADING", "baseAsset": "TSLAB", "quoteAsset": "USDT",
             "permissionSets": [["SPOT", "TRD_GRP_261"]], "filters": []},
            {"symbol": "OLDUSDT", "status": "BREAK", "baseAsset": "OLD", "quoteAsset": "USDT",
             "permissionSets": [["SPOT"]], "filters": []}
        ]});
        let s = parse_listings(&spot, Venue::Spot);
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].asset, s[0].tick, s[0].lot, s[0].min_notional),
            ("crypto", 0.01, 0.00001, 5.0)
        );
        assert_eq!(s[1].asset, "stock");
        assert!(s[0].active && !s[2].active);
        let fut = serde_json::json!({"symbols": [
            {"symbol": "BTCUSDT", "contractType": "PERPETUAL", "underlyingType": "COIN", "status": "TRADING",
             "filters": [{"filterType": "MIN_NOTIONAL", "notional": "100"}]},
            {"symbol": "TSLAUSDT", "contractType": "TRADIFI_PERPETUAL", "underlyingType": "EQUITY", "status": "TRADING"},
            {"symbol": "XAUUSDT", "contractType": "TRADIFI_PERPETUAL", "underlyingType": "COMMODITY", "status": "TRADING"},
            {"symbol": "BTCUSDT_261226", "contractType": "CURRENT_QUARTER", "underlyingType": "COIN", "status": "TRADING"}
        ]});
        let f = parse_listings(&fut, Venue::Futures);
        let kinds: Vec<(&str, &str)> = f.iter().map(|l| (l.symbol.as_str(), l.asset)).collect();
        assert_eq!(
            kinds,
            vec![
                ("BTCUSDT", "crypto"),
                ("TSLAUSDT", "stock"),
                ("XAUUSDT", "commodity")
            ]
        );
        assert_eq!(f[0].min_notional, 100.0);
        assert!(parse_listings(&serde_json::json!({}), Venue::Spot).is_empty());
    }

    #[test]
    fn pages_merge_oldest_first_without_repeats() {
        let k = |t: i64| Ohlc {
            time_ms: t,
            close: t as f64,
            ..Default::default()
        };
        // Newest page first, as the history walk collects them; one overlap.
        let merged = merge_pages(vec![vec![k(3), k(4), k(5)], vec![k(1), k(2), k(3)]]);
        assert_eq!(
            merged.iter().map(|o| o.time_ms).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert!(merge_pages(vec![]).is_empty());
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    #[test]
    fn a_refusal_pauses_until_the_time_it_gives() {
        let now = 1_791_712_000_000;
        let body = r#"{"code":-1003,"msg":"Way too much request weight used; IP banned until 1791712736435. Please use WebSocket Streams"}"#;
        assert_eq!(refusal_until(418, body, None, now), 1_791_712_736_435);
        assert_eq!(refusal_until(429, "{}", Some("30"), now), now + 30_000);
        assert_eq!(refusal_until(429, "{}", None, now), now + 60_000);
        assert_eq!(
            refusal_until(418, "banned until 5", None, now),
            now + 120_000,
            "a time in the past is ignored"
        );
        // While paused nothing is sent, and the error says how long.
        PAUSED_UNTIL.store(now_ms() + 30_000, std::sync::atomic::Ordering::Relaxed);
        match call("https://example.invalid/", 1) {
            Err(e @ BinanceError::Paused(_)) => assert!(e.to_string().contains("waiting"), "{e}"),
            other => panic!("{other:?}"),
        }
        PAUSED_UNTIL.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn the_cache_takes_new_candles_over_its_tail() {
        let k = |t: i64, c: f64| Ohlc {
            time_ms: t,
            close: c,
            ..Default::default()
        };
        let mut cached = vec![k(0, 1.0), k(60, 2.0), k(120, 3.0)];
        // The forming candle at 120 changed, 180 is new.
        merge_tail(&mut cached, vec![k(120, 3.5), k(180, 4.0)]);
        let got: Vec<(i64, f64)> = cached.iter().map(|c| (c.time_ms, c.close)).collect();
        assert_eq!(got, vec![(0, 1.0), (60, 2.0), (120, 3.5), (180, 4.0)]);
        merge_tail(&mut cached, vec![]);
        assert_eq!(cached.len(), 4);
        assert_eq!(interval_ms("15m"), 900_000);
        assert_eq!(interval_ms("4h"), 14_400_000);
        assert_eq!(interval_ms("1M"), 31 * 86_400_000);
        assert_eq!(interval_ms("1w"), 7 * 86_400_000);
    }

    #[test]
    fn prices_keep_usdt_pairs_only() {
        let raw = serde_json::json!([
            {"symbol":"BTCUSDT","price":"65000.10"},
            {"symbol":"ETHBTC","price":"0.05"},
            {"symbol":"XUSDT","price":"0"}
        ]);
        assert_eq!(parse_prices(&raw), vec![("BTCUSDT".to_string(), 65000.10)]);
    }
}
