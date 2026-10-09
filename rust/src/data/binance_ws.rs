//! Binance WebSocket streams — aggTrade, depth diff, bookTicker (selected /
//! all symbols).
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

fn host(venue_is_futures: bool) -> &'static str {
    if venue_is_futures {
        "wss://fstream.binance.com"
    } else {
        "wss://stream.binance.com"
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
        host(venue_is_futures),
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
        host(venue_is_futures),
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

fn spawn_book_ticker(
    name: &str,
    venue_is_futures: bool,
    keep: impl Fn(&str) -> bool + Send + 'static,
) -> MiniTickerHandle {
    let url = format!("{}/ws/!bookTicker", host(venue_is_futures));
    let (tx, rx) = mpsc::channel::<MiniTickerMsg>();
    let (stop_tx, thread) = spawn_stream(name.to_string(), url, move |sig| {
        let msg = match sig {
            Signal::Connected => MiniTickerMsg::Connected,
            Signal::Disconnected(r) => MiniTickerMsg::Disconnected(r),
            Signal::Text(text) => {
                // ~5000 events/s arrive; cheap-reject on the symbol first.
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                    return Flow::Continue;
                };
                match v.get("s").and_then(|s| s.as_str()) {
                    Some(s) if keep(s) => {}
                    _ => return Flow::Continue,
                }
                let Some((sym, mid, ts)) = parse_book_ticker(&v) else {
                    return Flow::Continue;
                };
                MiniTickerMsg::Tick(sym, mid, ts)
            }
        };
        if tx.send(msg).is_err() {
            Flow::Stop
        } else {
            Flow::Continue
        }
    });
    MiniTickerHandle {
        rx,
        stop_tx,
        thread,
    }
}

/// `!bookTicker` filtered to `symbols` in the worker thread, so the
/// main-loop drain stays cheap.
pub fn connect_book_tickers(symbols: Vec<String>, venue_is_futures: bool) -> MiniTickerHandle {
    let wanted: std::collections::HashSet<String> = symbols.into_iter().collect();
    spawn_book_ticker("ws-booktickers", venue_is_futures, move |s| {
        wanted.contains(s)
    })
}

/// `!bookTicker` for the Screener — every USDT symbol, unfiltered otherwise.
pub fn connect_all_book_tickers(venue_is_futures: bool) -> MiniTickerHandle {
    spawn_book_ticker("ws-screener-book", venue_is_futures, |s| {
        s.ends_with("USDT")
    })
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
}
