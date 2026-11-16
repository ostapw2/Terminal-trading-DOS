//! Binance WebSocket streams — aggTrade, depth diff, bookTicker (selected /
//! all symbols), forceOrder (liquidations).
//!
//! All streams run on ONE connection loop, [`ws_loop`], so liveness policy is
//! defined once (audit D2/R5):
//!   * a read timeout on the socket, so a stop request or a silently dead
//!     connection is noticed within about a second;
//!   * reconnect with exponential backoff (1 → 2 → 4 … 30 s);
//!   * a worker thread only ends when it is stopped or its receiver is gone,
//!     so `Handle::is_alive()` is a real liveness check.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WsError, Message, WebSocket};

use crate::data::binance::AggTrade;
use crate::data::orderbook::{parse_depth_diff, DepthDiffEvent};

/// How long a connection may stay completely silent before it is declared
/// dead.  Binance pings every 20 s (Spot) / 3 min (Futures) and every active
/// stream is far chattier than that.
const IDLE_LIMIT: Duration = Duration::from_secs(90);
const READ_TICK: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const BACKOFF_MAX_SECS: u64 = 30;

/// What [`ws_loop`] reports to its owner.
pub enum Signal {
    Connected,
    Disconnected(String),
    Text(String),
}

/// Whether the loop should keep going after a [`Signal`].
#[derive(PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

fn open(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, String> {
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| format!("bad url: {e}"))?;
    let host = uri.host().ok_or("url has no host")?;
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("ws") {
        80
    } else {
        443
    });
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("dns: {e}"))?;
    let mut last_err = String::from("no address");
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let stream = stream.ok_or(last_err)?;
    let _ = stream.set_nodelay(true);
    // Generous timeouts for the TLS + WS handshake…
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    // …then tighten the read timeout through a clone: socket options live on
    // the shared fd, so this applies to the connection `client_tls` owns.
    let ctl = stream.try_clone().map_err(|e| e.to_string())?;
    let (socket, _) = tungstenite::client_tls(url, stream).map_err(|e| e.to_string())?;
    let _ = ctl.set_read_timeout(Some(READ_TICK));
    Ok(socket)
}

fn stopped(stop_rx: &Receiver<()>) -> bool {
    !matches!(stop_rx.try_recv(), Err(mpsc::TryRecvError::Empty))
}

/// Sleep `secs`, waking early on stop.  Returns `false` if stopped.
fn sleep_or_stop(secs: u64, stop_rx: &Receiver<()>) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if stopped(stop_rx) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !stopped(stop_rx)
}

/// Run one stream until stopped: connect, read, reconnect with backoff.
pub fn ws_loop(url: &str, stop_rx: &Receiver<()>, on: &mut dyn FnMut(Signal) -> Flow) {
    if crate::data::demo::is_on() {
        return crate::data::demo::feed(url, stop_rx, on);
    }
    let mut backoff: u64 = 1;
    loop {
        if stopped(stop_rx) {
            return;
        }
        let mut socket = match open(url) {
            Ok(s) => s,
            Err(e) => {
                if on(Signal::Disconnected(format!("connect: {e}"))) == Flow::Stop {
                    return;
                }
                if !sleep_or_stop(backoff, stop_rx) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
                continue;
            }
        };
        let started = Instant::now();
        let mut last_rx = Instant::now();
        if on(Signal::Connected) == Flow::Stop {
            return;
        }
        let reason = loop {
            if stopped(stop_rx) {
                let _ = socket.close(None);
                return;
            }
            match socket.read() {
                Ok(Message::Text(s)) => {
                    last_rx = Instant::now();
                    if on(Signal::Text(s.to_string())) == Flow::Stop {
                        return;
                    }
                }
                Ok(Message::Binary(b)) => {
                    last_rx = Instant::now();
                    if let Ok(s) = String::from_utf8(b.to_vec()) {
                        if on(Signal::Text(s)) == Flow::Stop {
                            return;
                        }
                    }
                }
                Ok(Message::Ping(p)) => {
                    last_rx = Instant::now();
                    let _ = socket.send(Message::Pong(p));
                }
                Ok(Message::Close(_)) => break "closed by peer".to_string(),
                Ok(_) => last_rx = Instant::now(),
                Err(WsError::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if last_rx.elapsed() > IDLE_LIMIT {
                        break format!("silent for {}s", IDLE_LIMIT.as_secs());
                    }
                }
                Err(e) => break format!("read: {e}"),
            }
        };
        if on(Signal::Disconnected(reason)) == Flow::Stop {
            return;
        }
        // A connection that lived a while was healthy: start over fast.
        backoff = if started.elapsed() > Duration::from_secs(10) {
            1
        } else {
            (backoff * 2).min(BACKOFF_MAX_SECS)
        };
        if !sleep_or_stop(backoff, stop_rx) {
            return;
        }
    }
}

/// `true` while the worker thread is still running.
fn thread_alive(t: &Option<JoinHandle<()>>) -> bool {
    t.as_ref().is_some_and(|t| !t.is_finished())
}

fn spawn_stream(
    name: String,
    url: String,
    mut on: impl FnMut(Signal) -> Flow + Send + 'static,
) -> (Sender<()>, Option<JoinHandle<()>>) {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name(name)
        .spawn(move || ws_loop(&url, &stop_rx, &mut on))
        .ok();
    (stop_tx, thread)
}

/// What a stream carries, for the Futures URL: since 2026 Binance serves
/// USDⓈ-M market streams from two paths.  Book data (`depth`, `bookTicker`)
/// is under `/public`; everything else (`aggTrade`, `miniTicker`,
/// `forceOrder`) under `/market`.  The old `/ws` path still accepts every
/// stream but sends book data only: the rest connects and stays silent
/// (checked live 2026-10-11).  Spot has one path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Feed {
    Book,
    Market,
}

fn host(venue_is_futures: bool, feed: Feed) -> &'static str {
    match (venue_is_futures, feed) {
        (false, _) => "wss://stream.binance.com",
        (true, Feed::Book) => "wss://fstream.binance.com/public",
        (true, Feed::Market) => "wss://fstream.binance.com/market",
    }
}

fn parse_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key)?.as_str()?.parse().ok()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── aggTrade ─────────────────────────────────────────────────────────────

pub struct WsHandle {
    pub rx: Receiver<AggTrade>,
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl WsHandle {
    pub fn is_alive(&self) -> bool {
        thread_alive(&self.thread)
    }
}

/// Parse one `aggTrade` payload.
fn parse_agg_trade(v: &serde_json::Value) -> Option<AggTrade> {
    let price = parse_f64(v, "p")?;
    let qty = parse_f64(v, "q")?;
    if price <= 0.0 || qty <= 0.0 {
        return None;
    }
    Some(AggTrade {
        price,
        qty,
        time_ms: v.get("T").and_then(|x| x.as_i64()).unwrap_or(0),
        is_buyer_maker: v.get("m").and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

/// Stream aggTrades for `symbol`.
pub fn connect_agg_trades(symbol: &str, venue_is_futures: bool) -> WsHandle {
    let url = format!(
        "{}/ws/{}@aggTrade",
        host(venue_is_futures, Feed::Market),
        symbol.to_lowercase()
    );
    let (tx, rx) = mpsc::channel::<AggTrade>();
    let (stop_tx, thread) = spawn_stream(format!("ws:{symbol}"), url, move |sig| {
        if let Signal::Text(text) = sig {
            let trade = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| parse_agg_trade(&v));
            if let Some(t) = trade {
                if tx.send(t).is_err() {
                    return Flow::Stop;
                }
            }
        }
        Flow::Continue
    });
    WsHandle {
        rx,
        stop_tx,
        thread,
    }
}

// ── depth diff ───────────────────────────────────────────────────────────

pub struct DepthDiffHandle {
    pub rx: Receiver<DepthDiffEvent>,
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl DepthDiffHandle {
    pub fn is_alive(&self) -> bool {
        thread_alive(&self.thread)
    }
}

/// Subscribe to `<symbol>@depth@100ms` — incremental diff stream.  The caller
/// keeps a local book seeded by a REST snapshot and applies each diff with
/// `LocalBook::apply_checked`; after a reconnect the first diff fails the
/// sequence check and the caller re-seeds.
pub fn connect_depth_diff(symbol: &str, venue_is_futures: bool) -> DepthDiffHandle {
    let url = format!(
        "{}/ws/{}@depth@100ms",
        host(venue_is_futures, Feed::Book),
        symbol.to_lowercase()
    );
    let (tx, rx) = mpsc::channel::<DepthDiffEvent>();
    let (stop_tx, thread) = spawn_stream(format!("ws-diff:{symbol}"), url, move |sig| {
        if let Signal::Text(text) = sig {
            let diff = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| parse_depth_diff(&v));
            if let Some(d) = diff {
                if tx.send(d).is_err() {
                    return Flow::Stop;
                }
            }
        }
        Flow::Continue
    });
    DepthDiffHandle {
        rx,
        stop_tx,
        thread,
    }
}

// ── bookTicker ───────────────────────────────────────────────────────────

/// Sub-second mark feed for the F4 selection / F5 dashboard and the Screener.
///
/// Subscribes to Binance `!bookTicker` (best bid/ask for every symbol).  We
/// forward `(symbol, mid_price, event_ms)` where `mid_price` is
/// `(best_bid + best_ask) / 2` — what trading UIs show as "Last"
/// between actual trades.
pub struct MiniTickerHandle {
    pub rx: Receiver<MiniTickerMsg>,
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl MiniTickerHandle {
    pub fn is_alive(&self) -> bool {
        thread_alive(&self.thread)
    }
}

#[derive(Debug)]
pub enum MiniTickerMsg {
    /// WS connection established.
    Connected,
    /// Connection broke; the worker is reconnecting with backoff.
    Disconnected(String),
    /// `(symbol, mid_price, event_time_ms)` for one bookTicker event.
    Tick(String, f64, i64),
}

/// `(symbol, mid, event_ms)` from one `!bookTicker` payload.  Futures events
/// carry the exchange event time in `E`; Spot's bookTicker has none, so the
/// local clock stands in for it there.
fn parse_book_ticker(v: &serde_json::Value) -> Option<(String, f64, i64)> {
    let sym = v.get("s")?.as_str()?;
    let bid = parse_f64(v, "b")?;
    let ask = parse_f64(v, "a")?;
    if bid <= 0.0 || ask <= 0.0 {
        return None;
    }
    let ts = v.get("E").and_then(|x| x.as_i64()).unwrap_or_else(now_ms);
    Some((sym.to_string(), (bid + ask) / 2.0, ts))
}

/// Most symbols one combined `bookTicker` connection carries; beyond that the
/// all-symbols 1 Hz `!miniTicker@arr` stream is filtered instead (a URL with
/// hundreds of streams gets unwieldy).
pub const MAX_COMBINED_STREAMS: usize = 150;

/// `(symbol, last_price, event_ms)` for every entry of an `!miniTicker@arr`
/// payload (an array of 24h mini tickers, once a second).
fn parse_mini_ticker_arr(v: &serde_json::Value) -> Vec<(String, f64, i64)> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let sym = t.get("s")?.as_str()?;
            let price = parse_f64(t, "c")?;
            if price <= 0.0 {
                return None;
            }
            let ts = t.get("E").and_then(|x| x.as_i64()).unwrap_or_else(now_ms);
            Some((sym.to_string(), price, ts))
        })
        .collect()
}

/// The stream URL for a set of symbols.  Spot's all-symbols `!bookTicker`
/// stream accepts the connection and then sends NOTHING (it was removed), so
/// prices come from per-symbol `<symbol>@bookTicker` streams combined in one
/// connection, or from `!miniTicker@arr` when there are too many symbols.
/// The bool says whether the combined `bookTicker` form is used.
pub fn price_stream_url(symbols: &[String], venue_is_futures: bool) -> (String, bool) {
    if symbols.len() <= MAX_COMBINED_STREAMS && !symbols.is_empty() {
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@bookTicker", s.to_ascii_lowercase()))
            .collect();
        let host = host(venue_is_futures, Feed::Book);
        (format!("{host}/stream?streams={}", streams.join("/")), true)
    } else {
        let host = host(venue_is_futures, Feed::Market);
        (format!("{host}/ws/!miniTicker@arr"), false)
    }
}

fn spawn_price_stream(
    name: &str,
    url: String,
    extract: impl Fn(&serde_json::Value) -> Vec<(String, f64, i64)> + Send + 'static,
) -> MiniTickerHandle {
    let (tx, rx) = mpsc::channel::<MiniTickerMsg>();
    let (stop_tx, thread) = spawn_stream(name.to_string(), url, move |sig| {
        match sig {
            Signal::Connected => {
                if tx.send(MiniTickerMsg::Connected).is_err() {
                    return Flow::Stop;
                }
            }
            Signal::Disconnected(r) => {
                if tx.send(MiniTickerMsg::Disconnected(r)).is_err() {
                    return Flow::Stop;
                }
            }
            Signal::Text(text) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                    return Flow::Continue;
                };
                for (sym, price, ts) in extract(&v) {
                    if tx.send(MiniTickerMsg::Tick(sym, price, ts)).is_err() {
                        return Flow::Stop;
                    }
                }
            }
        }
        Flow::Continue
    });
    MiniTickerHandle {
        rx,
        stop_tx,
        thread,
    }
}

/// Mid prices of `symbols` (sub-second): one combined connection of
/// `<symbol>@bookTicker` streams; see [`price_stream_url`].
pub fn connect_book_tickers(symbols: Vec<String>, venue_is_futures: bool) -> MiniTickerHandle {
    let mut symbols: Vec<String> = symbols
        .into_iter()
        .map(|s| s.to_ascii_uppercase())
        .collect();
    symbols.sort();
    symbols.dedup();
    let (url, combined) = price_stream_url(&symbols, venue_is_futures);
    let wanted: std::collections::HashSet<String> = symbols.into_iter().collect();
    spawn_price_stream("ws-booktickers", url, move |v| {
        if combined {
            // {"stream": "btcusdt@bookTicker", "data": {...}}
            let data = v.get("data").unwrap_or(v);
            parse_book_ticker(data).into_iter().collect()
        } else {
            parse_mini_ticker_arr(v)
                .into_iter()
                .filter(|(s, _, _)| wanted.contains(s))
                .collect()
        }
    })
}

/// Every USDT symbol for the Screener.  Futures still has the all-symbols
/// `!bookTicker`; Spot does not (see [`price_stream_url`]), so it uses
/// `!miniTicker@arr` (last price, once a second).
pub fn connect_all_book_tickers(venue_is_futures: bool) -> MiniTickerHandle {
    if venue_is_futures {
        let url = format!("{}/ws/!bookTicker", host(true, Feed::Book));
        spawn_price_stream("ws-screener-book", url, |v| {
            parse_book_ticker(v)
                .filter(|(s, _, _)| s.ends_with("USDT"))
                .into_iter()
                .collect()
        })
    } else {
        let url = format!("{}/ws/!miniTicker@arr", host(false, Feed::Market));
        spawn_price_stream("ws-screener-book", url, |v| {
            parse_mini_ticker_arr(v)
                .into_iter()
                .filter(|(s, _, _)| s.ends_with("USDT"))
                .collect()
        })
    }
}

// ── all-market tickers (movers) ──────────────────────────────────────────

pub struct MarketTickHandle {
    pub rx: Receiver<Vec<crate::movers::Tick>>,
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl MarketTickHandle {
    pub fn is_alive(&self) -> bool {
        thread_alive(&self.thread)
    }
}

/// Every symbol's last price and 24h quote volume from one
/// `!miniTicker@arr` payload.
fn parse_market_ticks(v: &serde_json::Value) -> Vec<crate::movers::Tick> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            Some(crate::movers::Tick {
                symbol: t.get("s")?.as_str()?.to_string(),
                price: parse_f64(t, "c")?,
                quote_volume: parse_f64(t, "q")?,
                time_ms: t.get("E").and_then(|x| x.as_i64()).unwrap_or_else(now_ms),
            })
        })
        .collect()
}

/// The whole market once a second (`!miniTicker@arr`): only the symbols that
/// changed are sent.  Feeds `crate::movers`.
pub fn connect_market_tickers(venue_is_futures: bool) -> MarketTickHandle {
    let url = format!(
        "{}/ws/!miniTicker@arr",
        host(venue_is_futures, Feed::Market)
    );
    let (tx, rx) = mpsc::channel::<Vec<crate::movers::Tick>>();
    let (stop_tx, thread) = spawn_stream("ws-movers".into(), url, move |sig| {
        if let Signal::Text(text) = sig {
            let ticks = serde_json::from_str::<serde_json::Value>(&text)
                .map(|v| parse_market_ticks(&v))
                .unwrap_or_default();
            if !ticks.is_empty() && tx.send(ticks).is_err() {
                return Flow::Stop;
            }
        }
        Flow::Continue
    });
    MarketTickHandle {
        rx,
        stop_tx,
        thread,
    }
}

// ── forceOrder (liquidations) ────────────────────────────────────────────

pub enum LiqMsg {
    Connected,
    Disconnected(String),
    Liq(crate::data::liquidations::Liquidation),
}

pub struct LiqHandle {
    pub rx: Receiver<LiqMsg>,
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl LiqHandle {
    pub fn is_alive(&self) -> bool {
        thread_alive(&self.thread)
    }
}

/// Every liquidation on Binance USDⓈ-M Futures (`!forceOrder@arr`; at most
/// the largest one per symbol per second).  Quiet markets may stay silent
/// longer than the idle limit: the loop then simply reconnects.
pub fn connect_liquidations() -> LiqHandle {
    let url = format!("{}/ws/!forceOrder@arr", host(true, Feed::Market));
    let (tx, rx) = mpsc::channel::<LiqMsg>();
    let (stop_tx, thread) = spawn_stream("ws-liq".into(), url, move |sig| {
        let msgs = match sig {
            Signal::Connected => vec![LiqMsg::Connected],
            Signal::Disconnected(why) => vec![LiqMsg::Disconnected(why)],
            Signal::Text(text) => crate::data::liquidations::parse_force_orders(&text)
                .into_iter()
                .map(LiqMsg::Liq)
                .collect(),
        };
        for m in msgs {
            if tx.send(m).is_err() {
                return Flow::Stop;
            }
        }
        Flow::Continue
    });
    LiqHandle {
        rx,
        stop_tx,
        thread,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agg_trade_parses_and_rejects_garbage() {
        let v = json!({"e":"aggTrade","p":"65000.10","q":"0.5","T":1700000000123_i64,"m":true});
        let t = parse_agg_trade(&v).unwrap();
        assert_eq!(t.price, 65000.10);
        assert_eq!(t.time_ms, 1_700_000_000_123);
        assert!(t.is_buyer_maker);
        assert!(parse_agg_trade(&json!({"p":"0","q":"1"})).is_none());
        assert!(parse_agg_trade(&json!({"p":"x","q":"1"})).is_none());
    }

    #[test]
    fn book_ticker_uses_event_time_when_present() {
        let fut = json!({"s":"BTCUSDT","b":"100.0","a":"101.0","E":1700000000999_i64,"T":1});
        let (sym, mid, ts) = parse_book_ticker(&fut).unwrap();
        assert_eq!(sym, "BTCUSDT");
        assert_eq!(mid, 100.5);
        assert_eq!(ts, 1_700_000_000_999, "exchange E, not the local clock");
        // Spot has no event time → local clock fallback.
        let spot = json!({"s":"BTCUSDT","b":"100.0","a":"101.0"});
        let (_, _, ts) = parse_book_ticker(&spot).unwrap();
        assert!(ts > 1_600_000_000_000);
        assert!(parse_book_ticker(&json!({"s":"X","b":"0","a":"1"})).is_none());
    }

    #[test]
    fn ws_loop_reconnects_with_backoff_and_stops_promptly() {
        // Nothing listens on port 1: every attempt fails, the loop must keep
        // reporting Disconnected and stop within ~a backoff step when asked.
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ev_tx, ev_rx) = mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            ws_loop("ws://127.0.0.1:1/ws/x", &stop_rx, &mut |s| {
                if let Signal::Disconnected(r) = s {
                    let _ = ev_tx.send(r);
                }
                Flow::Continue
            });
        });
        let first = ev_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first failure");
        assert!(first.starts_with("connect:"), "got {first}");
        let t0 = Instant::now();
        stop_tx.send(()).unwrap();
        handle.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "stop must interrupt backoff sleep"
        );
    }

    #[test]
    fn spot_prices_come_from_per_symbol_streams_never_the_dead_all_symbols_one() {
        let syms = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let (url, combined) = price_stream_url(&syms, false);
        assert!(combined);
        assert_eq!(
            url,
            "wss://stream.binance.com/stream?streams=btcusdt@bookTicker/ethusdt@bookTicker"
        );
        assert!(
            !url.contains("!bookTicker"),
            "removed from Spot: it connects and stays silent"
        );
        let (fut, _) = price_stream_url(&syms, true);
        assert!(fut.starts_with("wss://fstream.binance.com/public/stream?streams="));
        // Too many symbols (or none): the 1 Hz all-symbols stream, filtered.
        let many: Vec<String> = (0..MAX_COMBINED_STREAMS + 1)
            .map(|i| format!("S{i}USDT"))
            .collect();
        let (url, combined) = price_stream_url(&many, false);
        assert!(!combined && url.ends_with("/ws/!miniTicker@arr"));
        assert!(!price_stream_url(&[], false).1);
        let (fut, _) = price_stream_url(&many, true);
        assert_eq!(fut, "wss://fstream.binance.com/market/ws/!miniTicker@arr");
    }

    #[test]
    fn market_ticks_carry_price_and_quote_volume() {
        let v = json!([
            {"e":"24hrMiniTicker","E":1700000000000_i64,"s":"BTCUSDT","c":"65000.5","q":"123456.7"},
            {"e":"24hrMiniTicker","s":"BAD","c":"x","q":"1"}
        ]);
        let t = parse_market_ticks(&v);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].price, t[0].quote_volume), (65000.5, 123456.7));
        assert_eq!(t[0].time_ms, 1_700_000_000_000);
    }

    #[test]
    fn futures_streams_take_the_path_that_still_sends() {
        // Book data under /public, the rest under /market; Spot unchanged.
        assert_eq!(host(true, Feed::Book), "wss://fstream.binance.com/public");
        assert_eq!(host(true, Feed::Market), "wss://fstream.binance.com/market");
        assert_eq!(host(false, Feed::Book), host(false, Feed::Market));
    }

    #[test]
    fn combined_and_array_payloads_parse() {
        let wrapped =
            json!({"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"100.0","a":"102.0"}});
        let data = wrapped.get("data").unwrap();
        let (s, mid, _) = parse_book_ticker(data).unwrap();
        assert_eq!((s.as_str(), mid), ("BTCUSDT", 101.0));
        let arr = json!([
            {"e":"24hrMiniTicker","E":1700000000000_i64,"s":"BNBBTC","c":"0.009"},
            {"e":"24hrMiniTicker","E":1700000000001_i64,"s":"ETHUSDT","c":"2494.5"},
            {"s":"BADUSDT","c":"0"},
            {"s":"NOPRICE"}
        ]);
        let v = parse_mini_ticker_arr(&arr);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1], ("ETHUSDT".to_string(), 2494.5, 1_700_000_000_001));
        assert!(parse_mini_ticker_arr(&json!({"not": "an array"})).is_empty());
    }
}
