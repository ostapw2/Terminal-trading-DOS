//! `--demo`: offline, synthetic market data.
//!
//! Used for screenshots, the first run and people without keys.  It is a
//! deliberate, labelled replacement of the *network layer only*: REST calls
//! return generated data, `ws_loop` runs a generated feed that emits the same
//! JSON Binance does, so every parser, the order-book sequencing and the
//! chart pipeline run unchanged.  Nothing here is ever mixed with real data:
//! the switch is process-wide and set once at startup.
//!
//! The price of a symbol is a pure function of (symbol, time): a sum of sines
//! with symbol-derived phases.  Candles, trades, quotes and the book therefore
//! agree with each other and with every timeframe, with no stored state.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::data::binance::{heuristic_tick_size, AggTrade, TickerSummary};
use crate::data::binance_ws::{Flow, Signal};
use crate::data::orderbook::OrderBookSnapshot;
use crate::markets::Ohlc;

static ON: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── deterministic noise ──────────────────────────────────────────────────

fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hash_str(s: &str) -> u64 {
    s.bytes().fold(0xCBF2_9CE4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

/// Uniform in [0, 1) from (seed, n).
fn unit(seed: u64, n: i64) -> f64 {
    (splitmix(seed ^ splitmix(n as u64)) >> 11) as f64 / (1u64 << 53) as f64
}

// ── price model ──────────────────────────────────────────────────────────

/// "BTCUSDT" / "EURUSD=X" / "AAPL" → "BTC" / "EURUSD" / "AAPL".
fn core(symbol: &str) -> String {
    let s = symbol.to_ascii_uppercase();
    let s = s.strip_suffix("USDT").unwrap_or(&s);
    let s = s.strip_suffix("=X").unwrap_or(s);
    s.strip_suffix("=F").unwrap_or(s).to_string()
}

const KNOWN: &[(&str, f64)] = &[
    ("BTC", 64_000.0),
    ("ETH", 3_200.0),
    ("BNB", 580.0),
    ("SOL", 150.0),
    ("XRP", 0.55),
    ("ADA", 0.45),
    ("DOGE", 0.15),
    ("AVAX", 35.0),
    ("DOT", 7.0),
    ("LINK", 14.0),
    ("LTC", 80.0),
    ("TRX", 0.12),
    ("AAPL", 225.0),
    ("GOOGL", 175.0),
    ("MSFT", 420.0),
    ("NVDA", 130.0),
    ("TSLA", 250.0),
    ("AMZN", 185.0),
    ("META", 520.0),
    ("EURUSD", 1.08),
    ("GBPUSD", 1.27),
    ("USDJPY", 150.0),
];

/// Extra pairs so the Screener has a believable list.
const EXTRA: &[&str] = &[
    "ARB", "OP", "APT", "SUI", "NEAR", "INJ", "TIA", "SEI", "PEPE", "WIF", "ATOM", "FIL", "ETC",
    "UNI",
];

fn base_price(core: &str) -> f64 {
    if let Some((_, p)) = KNOWN.iter().find(|(c, _)| *c == core) {
        return *p;
    }
    // Log-uniform between 0.2 and 200.
    0.2 * (unit(hash_str(core), 77) * 1000f64.ln()).exp()
}

const MIN: f64 = 60_000.0;
const HOUR: f64 = 60.0 * MIN;
const DAY: f64 = 24.0 * HOUR;

/// Octave periods (ms) of the price path, slowest first.
const OCTAVES: [f64; 10] = [
    30.0 * DAY,
    10.0 * DAY,
    3.0 * DAY,
    DAY,
    8.0 * HOUR,
    2.0 * HOUR,
    30.0 * MIN,
    8.0 * MIN,
    2.0 * MIN,
    20_000.0,
];

/// Smooth value noise in [-1, 1]: hashed lattice values, smoothstep between.
fn value_noise(seed: u64, octave: i64, x: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    let at = |k: f64| unit(seed, (octave << 40) ^ (k as i64)) * 2.0 - 1.0;
    at(i) + (at(i + 1.0) - at(i)) * s
}

/// Stateless random-walk-like path: octaves of value noise whose amplitude
/// grows with the square root of the period (so volatility scales like a
/// real market).  A pure function of (symbol, time).
pub fn price_at(symbol: &str, t_ms: i64) -> f64 {
    let c = core(symbol);
    let seed = hash_str(&c);
    let vol = 0.012 + unit(seed, 9) * 0.012;
    let t = t_ms as f64;
    let mut log_f = 0.0;
    for (k, period) in OCTAVES.iter().enumerate() {
        let amp = vol * (period / DAY).sqrt();
        log_f += amp * value_noise(seed, k as i64, t / period);
    }
    base_price(&c) * log_f.exp()
}

/// Round to 8 decimals the way Binance prints prices, so the f64 that comes
/// back from parsing `{:.8}` is bit-identical to the one we started from
/// (order-book keys are price scaled to an integer).
fn snap(p: f64) -> f64 {
    (p * 1e8).round() / 1e8
}

pub fn tick_size(symbol: &str) -> f64 {
    heuristic_tick_size(base_price(&core(symbol)))
}

fn interval_ms(interval: &str) -> i64 {
    match interval {
        "15s" => 15_000,
        "1m" => 60_000,
        "5m" => 5 * 60_000,
        "15m" => 15 * 60_000,
        "30m" => 30 * 60_000,
        "1h" | "60m" => 3_600_000,
        "4h" => 4 * 3_600_000,
        "1d" => 86_400_000,
        _ => 60_000,
    }
}

// ── candles ──────────────────────────────────────────────────────────────

/// `limit` candles ending with the one that is open at `now_ms`.
pub fn klines(symbol: &str, interval: &str, limit: u32, now_ms: i64) -> Vec<Ohlc> {
    let step = interval_ms(interval);
    let seed = hash_str(&core(symbol));
    let usd_per_min = 20_000.0 + unit(seed, 5) * 400_000.0;
    let last_open = now_ms.div_euclid(step) * step;
    (0..limit as i64)
        .rev()
        .map(|i| {
            let s = last_open - i * step;
            let e = (s + step).min(now_ms);
            let open = price_at(symbol, s);
            let close = price_at(symbol, e);
            // The extremes come from the same path the trades follow, so a
            // footprint built from demo trades always fits its candle.
            let (mut hi, mut lo) = (open.max(close), open.min(close));
            for k in 1..24 {
                let p = price_at(symbol, s + step * k / 24);
                hi = hi.max(p);
                lo = lo.min(p);
            }
            // 0.00012 covers a trade sitting one tick off the path; the rest is wick.
            let wick = 0.0001 + 0.0004 * (step as f64 / MIN).sqrt().min(6.0) / 6.0;
            let high = hi * (1.0 + 0.00012 + wick * unit(seed, s ^ 1));
            let low = lo * (1.0 - 0.00012 - wick * unit(seed, s ^ 2));
            let mid = (open + close) / 2.0;
            let mv = ((close - open) / open).abs() * 400.0;
            let volume = usd_per_min * (step as f64 / MIN) / mid
                * (0.4 + 1.2 * unit(seed, s ^ 3))
                * (1.0 + mv.min(2.0));
            Ohlc {
                open,
                high,
                low,
                close,
                volume,
                date: crate::markets::unix_secs_to_iso_date(s / 1000),
                time_ms: s,
                is_gap: false,
            }
        })
        .collect()
}

/// Yahoo-shaped request (`range` + `interval`) for the non-crypto markets.
pub fn candles(ticker: &str, range: &str, interval: &str, now_ms: i64) -> Vec<Ohlc> {
    let span = match range {
        "1d" => DAY,
        "5d" => 5.0 * DAY,
        "1mo" => 30.0 * DAY,
        "3mo" => 90.0 * DAY,
        "6mo" => 180.0 * DAY,
        "1y" => 365.0 * DAY,
        _ => 90.0 * DAY,
    };
    let n = (span / interval_ms(interval) as f64).clamp(2.0, 2000.0) as u32;
    klines(ticker, interval, n, now_ms)
}

// ── trades ───────────────────────────────────────────────────────────────

const TRADE_MS: i64 = 250;

/// Trade number `k` of `symbol` (index = time / 250 ms; stateless).
fn trade_at(symbol: &str, k: i64) -> AggTrade {
    let seed = hash_str(&core(symbol));
    let t = k * TRADE_MS + (unit(seed, k ^ 11) * (TRADE_MS - 1) as f64) as i64;
    let tick = tick_size(symbol);
    // Aggressive sells hit the best bid, aggressive buys lift the best ask
    // (the same two prices the book and `!bookTicker` show).
    let bid = snap((price_at(symbol, t) / tick).floor() * tick);
    let is_buyer_maker = unit(seed, k ^ 14) < 0.5;
    let price = if is_buyer_maker {
        bid
    } else {
        snap(bid + tick)
    };
    let usd = 30.0 * (4.0 * unit(seed, k ^ 13)).exp();
    AggTrade {
        price,
        qty: ((usd / price) * 1e5).round() / 1e5,
        time_ms: t,
        is_buyer_maker,
    }
    .nonzero()
}

impl AggTrade {
    /// Quantity rounding must never produce a zero-size trade.
    fn nonzero(mut self) -> Self {
        if self.qty <= 0.0 {
            self.qty = 1e-5;
        }
        self
    }
}

/// The last `limit` trades up to `now_ms`, oldest first.
pub fn agg_trades(symbol: &str, limit: u32, now_ms: i64) -> Vec<AggTrade> {
    let k_now = now_ms.div_euclid(TRADE_MS);
    (k_now - limit as i64 + 1..=k_now)
        .map(|k| trade_at(symbol, k))
        .collect()
}

// ── 24h summary (Screener) ───────────────────────────────────────────────

pub fn universe() -> Vec<String> {
    let mut v: Vec<String> = KNOWN
        .iter()
        .take(12)
        .map(|(c, _)| format!("{c}USDT"))
        .collect();
    v.extend(EXTRA.iter().map(|c| format!("{c}USDT")));
    for s in crate::markets::market_symbols(crate::markets::MarketType::Crypto) {
        let sym = format!("{}USDT", s.ticker.trim_end_matches("USDT"));
        if !v.contains(&sym) {
            v.push(sym);
        }
    }
    v
}

pub fn summaries(now_ms: i64) -> Vec<TickerSummary> {
    universe()
        .into_iter()
        .map(|symbol| {
            let seed = hash_str(&core(&symbol));
            let last = price_at(&symbol, now_ms);
            let prev = price_at(&symbol, now_ms - DAY as i64);
            let (mut hi, mut lo) = (last, last);
            for k in 0..48 {
                let p = price_at(&symbol, now_ms - (DAY as i64) * k / 48);
                hi = hi.max(p);
                lo = lo.min(p);
            }
            let quote = 1e7 * (6.0 * unit(seed, 21)).exp();
            TickerSummary {
                change_pct: (last / prev - 1.0) * 100.0,
                high_24h: hi,
                low_24h: lo,
                volume_base: quote / last,
                volume_quote: quote,
                last_price: last,
                symbol,
            }
        })
        .collect()
}

// ── order book ───────────────────────────────────────────────────────────

const DEPTH: i64 = 50;
const BOOK_MS: i64 = 100;
const RING: usize = 300;

type Change = (bool, i64, f64); // (is_bid, level index, qty; 0 = removed)

struct Book {
    id: u64,
    tick: f64,
    seed: u64,
    symbol: String,
    levels: BTreeMap<i64, (bool, f64)>,
    ring: VecDeque<(u64, Vec<Change>)>,
}

fn id_at(ms: i64) -> u64 {
    (ms / BOOK_MS) as u64
}

impl Book {
    fn new(symbol: &str, now_ms: i64) -> Self {
        let mut b = Book {
            id: id_at(now_ms),
            tick: tick_size(symbol),
            seed: hash_str(&core(symbol)),
            symbol: symbol.to_string(),
            levels: BTreeMap::new(),
            ring: VecDeque::new(),
        };
        b.sync_window(false);
        b
    }

    fn mid_idx(&self) -> i64 {
        (price_at(&self.symbol, self.id as i64 * BOOK_MS) / self.tick).floor() as i64
    }

    fn qty(&self, idx: i64, dist: i64, salt: i64) -> f64 {
        let price = idx as f64 * self.tick;
        let usd = 4000.0 * (1.0 + dist as f64 * 0.15) * (0.2 + 1.8 * unit(self.seed, idx ^ salt));
        ((usd / price) * 1e5).round().max(1.0) / 1e5
    }

    /// Bring `levels` to the window around the current mid; returns the diff.
    fn sync_window(&mut self, record: bool) -> BTreeMap<(bool, i64), f64> {
        let mut out = BTreeMap::new();
        let mid = self.mid_idx();
        let salt = self.id as i64;
        let stale: Vec<(i64, bool)> = self
            .levels
            .iter()
            .filter(|(&i, &(bid, _))| !(mid - DEPTH < i && i <= mid + DEPTH) || (i <= mid) != bid)
            .map(|(&i, &(bid, _))| (i, bid))
            .collect();
        for (i, bid) in stale {
            self.levels.remove(&i);
            out.insert((bid, i), 0.0);
        }
        for i in mid - DEPTH + 1..=mid + DEPTH {
            if !self.levels.contains_key(&i) {
                let bid = i <= mid;
                let dist = if bid { mid - i } else { i - mid - 1 };
                let q = self.qty(i, dist, salt);
                self.levels.insert(i, (bid, q));
                out.insert((bid, i), q);
            }
        }
        if !record {
            out.clear();
        }
        out
    }

    fn step(&mut self) {
        self.id += 1;
        let mut changes = self.sync_window(true);
        let mid = self.mid_idx();
        for n in 0..6i64 {
            let off = (splitmix(self.seed ^ self.id.wrapping_mul(31) ^ n as u64)
                % (2 * DEPTH as u64)) as i64;
            let (idx, bid, dist) = if off < DEPTH {
                (mid - off, true, off)
            } else {
                (mid + 1 + (off - DEPTH), false, off - DEPTH)
            };
            let q = self.qty(idx, dist, self.id as i64 ^ (n << 40));
            self.levels.insert(idx, (bid, q));
            changes.insert((bid, idx), q);
        }
        let list: Vec<Change> = changes.into_iter().map(|((b, i), q)| (b, i, q)).collect();
        self.ring.push_back((self.id, list));
        while self.ring.len() > RING {
            self.ring.pop_front();
        }
    }
}

fn books() -> &'static Mutex<HashMap<String, Book>> {
    static B: OnceLock<Mutex<HashMap<String, Book>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Run `f` on the (shared) book of `symbol`, advanced to the wall clock.
fn with_book<R>(symbol: &str, f: impl FnOnce(&Book) -> R) -> R {
    let mut map = books().lock().unwrap_or_else(|e| e.into_inner());
    let now = now_ms();
    let target = id_at(now);
    let b = map
        .entry(symbol.to_ascii_uppercase())
        .or_insert_with(|| Book::new(symbol, now));
    if target.saturating_sub(b.id) > RING as u64 {
        *b = Book::new(symbol, now);
    }
    while b.id < target {
        b.step();
    }
    f(b)
}

/// REST depth snapshot + its `lastUpdateId`.
pub fn depth(symbol: &str, limit: u32) -> (OrderBookSnapshot, u64) {
    with_book(symbol, |b| {
        let n = limit.clamp(5, 5000) as usize;
        let mut bids: Vec<(f64, f64)> = b
            .levels
            .iter()
            .rev()
            .filter(|(_, (bid, _))| *bid)
            .map(|(&i, &(_, q))| (snap(i as f64 * b.tick), q))
            .collect();
        let mut asks: Vec<(f64, f64)> = b
            .levels
            .iter()
            .filter(|(_, (bid, _))| !*bid)
            .map(|(&i, &(_, q))| (snap(i as f64 * b.tick), q))
            .collect();
        bids.truncate(n);
        asks.truncate(n);
        (OrderBookSnapshot { bids, asks }, b.id)
    })
}

// ── websocket feeds ──────────────────────────────────────────────────────

fn sleep_or_stop(stop_rx: &Receiver<()>, ms: u64) -> bool {
    matches!(
        stop_rx.recv_timeout(Duration::from_millis(ms)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

fn symbol_in_url(url: &str) -> String {
    let tail = url.rsplit("/ws/").next().unwrap_or("");
    tail.split('@').next().unwrap_or("").to_ascii_uppercase()
}

fn level(p: f64, q: f64) -> String {
    format!("[\"{p:.8}\",\"{q:.8}\"]")
}

/// Stands in for `binance_ws::ws_loop` while demo mode is on.
pub fn feed(url: &str, stop_rx: &Receiver<()>, on: &mut dyn FnMut(Signal) -> Flow) {
    if on(Signal::Connected) == Flow::Stop {
        return;
    }
    let symbol = symbol_in_url(url);
    if url.contains("@aggTrade") {
        let mut last = now_ms().div_euclid(TRADE_MS);
        while sleep_or_stop(stop_rx, 100) {
            let k_now = now_ms().div_euclid(TRADE_MS);
            for k in last + 1..=k_now {
                let t = trade_at(&symbol, k);
                let text = format!(
                    "{{\"e\":\"aggTrade\",\"p\":\"{:.8}\",\"q\":\"{:.8}\",\"T\":{},\"m\":{}}}",
                    t.price, t.qty, t.time_ms, t.is_buyer_maker
                );
                if on(Signal::Text(text)) == Flow::Stop {
                    return;
                }
            }
            last = k_now;
        }
    } else if url.contains("@depth") {
        let futures = url.contains("fstream");
        // Start a little in the past, like a stream that connected before the
        // REST snapshot was asked for (the caller buffers, then seeds).
        let mut last = with_book(&symbol, |b| b.id).saturating_sub(5);
        while sleep_or_stop(stop_rx, BOOK_MS as u64) {
            let events: Vec<(u64, Vec<Change>)> = with_book(&symbol, |b| {
                b.ring.iter().filter(|e| e.0 > last).cloned().collect()
            });
            for (id, changes) in events {
                let side = |want: bool| {
                    changes
                        .iter()
                        .filter(|c| c.0 == want)
                        .map(|c| {
                            let tick = tick_size(&symbol);
                            level(snap(c.1 as f64 * tick), c.2)
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let pu = if futures {
                    format!(",\"pu\":{}", id - 1)
                } else {
                    String::new()
                };
                let text = format!(
                    "{{\"e\":\"depthUpdate\",\"E\":{},\"s\":\"{}\",\"U\":{id},\"u\":{id}{pu},\"b\":[{}],\"a\":[{}]}}",
                    now_ms(),
                    symbol,
                    side(true),
                    side(false)
                );
                if on(Signal::Text(text)) == Flow::Stop {
                    return;
                }
                last = id;
            }
        }
    } else if url.contains("!bookTicker") {
        let syms = universe();
        while sleep_or_stop(stop_rx, 250) {
            let now = now_ms();
            for s in &syms {
                let tick = tick_size(s);
                let bid = snap((price_at(s, now) / tick).floor() * tick);
                let text = format!(
                    "{{\"u\":{now},\"s\":\"{s}\",\"b\":\"{bid:.8}\",\"B\":\"1\",\"a\":\"{:.8}\",\"A\":\"1\",\"E\":{now}}}",
                    snap(bid + tick)
                );
                if on(Signal::Text(text)) == Flow::Stop {
                    return;
                }
            }
        }
    } else {
        // Unknown stream: stay connected and silent until asked to stop.
        while sleep_or_stop(stop_rx, 500) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::binance::Venue;
    use crate::data::orderbook::{parse_depth_diff, LocalBook};

    const NOW: i64 = 1_790_000_000_000;

    #[test]
    fn same_inputs_same_series() {
        let a = klines("BTCUSDT", "1m", 100, NOW);
        let b = klines("BTCUSDT", "1m", 100, NOW);
        assert_eq!(a.len(), 100);
        assert!(a.iter().zip(&b).all(|(x, y)| x.close == y.close));
        assert!(klines("ETHUSDT", "1m", 100, NOW)[0].close != a[0].close);
    }

    #[test]
    fn candles_are_sane_and_continuous() {
        let k = klines("SOLUSDT", "5m", 300, NOW);
        for c in &k {
            assert!(c.low <= c.open.min(c.close) && c.high >= c.open.max(c.close));
            assert!(c.low > 0.0 && c.volume > 0.0 && !c.is_gap);
        }
        for w in k.windows(2) {
            assert_eq!(w[1].time_ms - w[0].time_ms, 300_000);
            assert!((w[1].open - w[0].close).abs() < 1e-9 * w[0].close.max(1.0));
        }
    }

    #[test]
    fn trades_fit_inside_their_candle() {
        let sym = "BTCUSDT";
        let k = klines(sym, "1m", 10, NOW);
        for t in agg_trades(sym, 1000, NOW) {
            if let Some(c) = k
                .iter()
                .find(|c| t.time_ms >= c.time_ms && t.time_ms < c.time_ms + 60_000)
            {
                assert!(
                    t.price <= c.high && t.price >= c.low,
                    "trade {} outside candle {}..{}",
                    t.price,
                    c.low,
                    c.high
                );
            }
        }
    }

    #[test]
    fn book_snapshot_plus_diffs_stay_consistent() {
        for (venue, url) in [
            (Venue::Spot, "wss://stream/ws/btcusdt@depth@100ms"),
            (Venue::Futures, "wss://fstream/ws/btcusdt@depth@100ms"),
        ] {
            let (snap, last) = depth("BTCUSDT", 1000);
            assert!(snap.best_bid().unwrap() < snap.best_ask().unwrap());
            let mut book = LocalBook::from_snapshot(&snap, last);
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let mut applied = 0;
            let mut gaps = 0;
            let started = std::time::Instant::now();
            feed(url, &stop_rx, &mut |s| {
                if let Signal::Text(t) = s {
                    let d = parse_depth_diff(&serde_json::from_str(&t).unwrap()).unwrap();
                    if d.final_update_id >= last {
                        match book.apply_checked(&d, venue) {
                            Ok(()) => applied += 1,
                            Err(_) => gaps += 1,
                        }
                    }
                }
                if started.elapsed() > Duration::from_millis(1500) {
                    Flow::Stop
                } else {
                    Flow::Continue
                }
            });
            drop(stop_tx);
            assert!(applied >= 8, "{venue:?}: applied {applied}");
            assert_eq!(gaps, 0, "{venue:?}: sequencing gaps");
            let (b, a) = (
                book.bids.keys().next_back().unwrap(),
                book.asks.keys().next().unwrap(),
            );
            assert!(b < a, "{venue:?}: crossed book");
            // No two keys for "the same" price (float repr drift would show
            // up as duplicate rows in the book view).
            let min_gap = (tick_size("BTCUSDT") * book.price_scale * 0.5) as i64;
            for side in [&book.bids, &book.asks] {
                let keys: Vec<i64> = side.keys().copied().collect();
                assert!(
                    keys.windows(2).all(|w| w[1] - w[0] > min_gap),
                    "{venue:?}: duplicate price levels"
                );
            }
        }
    }
}
