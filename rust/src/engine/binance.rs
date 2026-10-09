//! Authenticated Binance **Spot** REST client for live trading.
//!
//! Live mode is OFF by default and (until `TODO.md` stage 4 is signed off by
//! a testnet run) is not supported for real money.  Futures is NOT
//! implemented here: a Futures slot must never reach this client.
//!
//! Endpoints (Spot only):
//! - `POST   /api/v3/order` — market + limit orders (with `newClientOrderId`)
//! - `GET    /api/v3/order` — resolve an order whose outcome we did not see
//! - `DELETE /api/v3/order` — cancel by id
//! - `GET    /api/v3/account`, `/openOrders`, `/time`
//! - user-data stream (listenKey) for fills
//!
//! All signed requests use HMAC-SHA256 over the query string and a
//! server-time-corrected timestamp.  Network calls run on background threads;
//! results reach the main loop through an mpsc channel the runtime drains.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::data::binance_ws::{ws_loop, Flow, Signal};
use crate::strategies::{actions::Action, context::Side};

type HmacSha256 = Hmac<Sha256>;

pub const PROD_BASE: &str = "https://api.binance.com";
pub const TESTNET_BASE: &str = "https://testnet.binance.vision";

/// The only hosts that may ever receive an API key.
pub fn validate_base(base: &str) -> Result<String, String> {
    let b = base.trim().trim_end_matches('/');
    if b == PROD_BASE || b == TESTNET_BASE {
        Ok(b.to_string())
    } else {
        Err(format!(
            "DOS_BINANCE_BASE must be {PROD_BASE} or {TESTNET_BASE}; refusing to send an API key anywhere else"
        ))
    }
}

/// `DOS_BINANCE_BASE` if set (validated), else production.
pub fn api_base_from_env() -> Result<String, String> {
    match std::env::var("DOS_BINANCE_BASE") {
        Ok(v) => validate_base(&v),
        Err(_) => Ok(PROD_BASE.to_string()),
    }
}

/// Agent for every authenticated call: never follows redirects (ureq keeps
/// custom headers such as `X-MBX-APIKEY` across a redirect to another host).
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().redirects(0).build()
}

/// `server_time - local_time` in ms, measured once when live mode starts.
/// Signed requests are timestamped with it so a skewed local clock does not
/// get every order rejected (`-1021`).
static SERVER_OFFSET_MS: AtomicI64 = AtomicI64::new(0);
static CID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct BalanceEntry {
    pub asset: String,
    pub free: f64,
    pub locked: f64,
}

impl BalanceEntry {
    pub fn total(&self) -> f64 {
        self.free + self.locked
    }
}

/// Where a fill report came from.  The same fill can arrive twice — in the
/// REST response and on the user-data stream — and must be counted once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillSource {
    /// REST response of a order that is already fully filled (`FILLED`).
    Rest,
    /// One `executionReport` TRADE on the user-data stream.
    UserData,
}

/// Asynchronous result delivered through the client's channel.  Drain
/// in the main loop and apply to local state.
#[derive(Debug)]
pub enum LiveEvent {
    /// Executed quantity at `price`.
    OrderFilled {
        symbol: String,
        side: Side,
        qty: f64,
        price: f64,
        time_ms: i64,
        order_id: u64,
        /// Exchange trade id (0 when unknown, i.e. a REST aggregate).
        trade_id: u64,
        client_id: String,
        source: FillSource,
    },
    /// Order accepted and resting (remaining quantity `qty`).
    OrderQueued {
        symbol: String,
        side: Side,
        qty: f64,
        limit: f64,
        order_id: u64,
        created_ms: i64,
        client_id: String,
    },
    /// The exchange definitely refused the order (nothing was placed).
    OrderRejected {
        action: Action,
        client_id: String,
        reason: String,
    },
    /// We could not tell whether the order was placed (timeout, 5xx,
    /// unparsable reply).  A background lookup by client id follows and ends
    /// in `OrderFilled` / `OrderQueued` / `OrderRejected` / `SyncError`.
    OrderUnknown {
        action: Action,
        client_id: String,
        reason: String,
    },
    /// Periodic balance snapshot from `/api/v3/account`.
    AccountSync(Vec<BalanceEntry>),
    /// Periodic open-orders snapshot from `/api/v3/openOrders`.
    OpenOrdersSync(Vec<OpenOrderInfo>),
    /// Order no longer resting (cancelled / expired).
    OrderCancelled { symbol: String, order_id: u64 },
    /// Result of creating the user-data listen key (done off the UI thread).
    ListenKey(Result<String, String>),
    /// A background call failed in a way the user must see.
    SyncError(String),
}

#[derive(Clone, Debug)]
pub struct OpenOrderInfo {
    pub order_id: u64,
    pub symbol: String,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub created_ms: i64,
}

pub struct BinanceClient {
    pub api_key: String,
    pub api_secret: String,
    pub base_url: String,
    pub recv_window: u32,
    tx: Sender<LiveEvent>,
    rx: Receiver<LiveEvent>,
}

/// Why a signed call failed.
#[derive(Debug)]
enum ApiError {
    /// The exchange answered with a 4xx: the request had no effect.
    Definite(String),
    /// Transport error, timeout, 5xx or an unreadable 2xx: the request MAY
    /// have taken effect.
    Unknown(String),
}

impl ApiError {
    fn text(&self) -> &str {
        match self {
            ApiError::Definite(s) | ApiError::Unknown(s) => s,
        }
    }
}

impl BinanceClient {
    pub fn new(api_key: String, api_secret: String, base_url: String) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            api_key,
            api_secret,
            base_url,
            recv_window: 5000,
            tx,
            rx,
        }
    }

    pub fn drain(&self) -> Vec<LiveEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Clone the event sender for the user-data WS thread.
    pub fn event_sender(&self) -> Sender<LiveEvent> {
        self.tx.clone()
    }

    /// A fresh client order id: unique per process run and traceable.
    pub fn new_client_id() -> String {
        let n = CID_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("dos{}x{}", now_ms(), n)
    }

    /// Everything that must not block the UI thread: server-time offset,
    /// listen key, first balance + open-orders sync.  Results arrive as events.
    pub fn bootstrap(&self) {
        let c = self.clone_creds();
        thread::spawn(move || {
            match fetch_server_offset(&c.base) {
                Ok(off) => SERVER_OFFSET_MS.store(off, Ordering::Relaxed),
                Err(e) => {
                    let _ = c.tx.send(LiveEvent::SyncError(format!(
                        "server time unavailable ({e}); using local clock"
                    )));
                }
            }
            let _ =
                c.tx.send(LiveEvent::ListenKey(create_listen_key(&c.base, &c.key)));
            match get_account(&c.key, &c.secret, &c.base, c.recv) {
                Ok(b) => {
                    let _ = c.tx.send(LiveEvent::AccountSync(b));
                }
                Err(e) => {
                    let _ =
                        c.tx.send(LiveEvent::SyncError(format!("account sync: {e}")));
                }
            }
            match get_open_orders(&c.key, &c.secret, &c.base, c.recv) {
                Ok(o) => {
                    let _ = c.tx.send(LiveEvent::OpenOrdersSync(o));
                }
                Err(e) => {
                    let _ =
                        c.tx.send(LiveEvent::SyncError(format!("open orders sync: {e}")));
                }
            }
        });
    }

    fn clone_creds(&self) -> Creds {
        Creds {
            key: self.api_key.clone(),
            secret: self.api_secret.clone(),
            base: self.base_url.clone(),
            recv: self.recv_window,
            tx: self.tx.clone(),
        }
    }

    /// Submit an order.  Returns immediately; the outcome arrives through
    /// [`drain`](Self::drain).  `client_id` becomes `newClientOrderId`, so an
    /// order whose reply we lose can still be found.
    pub fn submit(&self, action: Action, client_id: String) {
        let c = self.clone_creds();
        thread::spawn(move || {
            match post_order(&c.key, &c.secret, &c.base, c.recv, &action, &client_id) {
                Ok(events) => {
                    for ev in events {
                        let _ = c.tx.send(ev);
                    }
                }
                Err(ApiError::Definite(reason)) => {
                    let _ = c.tx.send(LiveEvent::OrderRejected {
                        action,
                        client_id,
                        reason,
                    });
                }
                Err(ApiError::Unknown(reason)) => {
                    let _ = c.tx.send(LiveEvent::OrderUnknown {
                        action: action.clone(),
                        client_id: client_id.clone(),
                        reason,
                    });
                    resolve_unknown(&c, &action, &client_id);
                }
            }
        });
    }

    /// Cancel an open order by id.
    pub fn cancel(&self, symbol: String, order_id: u64) {
        let c = self.clone_creds();
        thread::spawn(move || {
            match delete_order(&c.key, &c.secret, &c.base, c.recv, &symbol, order_id) {
                Ok(()) => {
                    let _ = c.tx.send(LiveEvent::OrderCancelled { symbol, order_id });
                }
                Err(e) => {
                    let _ = c.tx.send(LiveEvent::SyncError(format!(
                        "cancel #{order_id} {symbol}: {}",
                        e.text()
                    )));
                }
            }
        });
    }

    /// Pull the latest non-zero balances.
    pub fn sync_account(&self) {
        let c = self.clone_creds();
        thread::spawn(
            move || match get_account(&c.key, &c.secret, &c.base, c.recv) {
                Ok(b) => {
                    let _ = c.tx.send(LiveEvent::AccountSync(b));
                }
                Err(e) => {
                    let _ =
                        c.tx.send(LiveEvent::SyncError(format!("account sync: {e}")));
                }
            },
        );
    }

    /// Pull the latest open orders.
    pub fn sync_open_orders(&self) {
        let c = self.clone_creds();
        thread::spawn(
            move || match get_open_orders(&c.key, &c.secret, &c.base, c.recv) {
                Ok(o) => {
                    let _ = c.tx.send(LiveEvent::OpenOrdersSync(o));
                }
                Err(e) => {
                    let _ =
                        c.tx.send(LiveEvent::SyncError(format!("open orders sync: {e}")));
                }
            },
        );
    }

    /// Keep the listen key alive — off the UI thread.
    pub fn keepalive_listen_key_async(&self, listen_key: String) {
        let c = self.clone_creds();
        thread::spawn(move || {
            if let Err(e) = keepalive_listen_key(&c.base, &c.key, &listen_key) {
                let _ =
                    c.tx.send(LiveEvent::SyncError(format!("listen-key keepalive: {e}")));
            }
        });
    }

    /// Delete the listen key — off the UI thread, best effort.
    pub fn delete_listen_key_async(&self, listen_key: String) {
        let (key, base) = (self.api_key.clone(), self.base_url.clone());
        thread::spawn(move || {
            let _ = delete_listen_key(&base, &key, &listen_key);
        });
    }
}

struct Creds {
    key: String,
    secret: String,
    base: String,
    recv: u32,
    tx: Sender<LiveEvent>,
}

// ─── HTTP helpers ────────────────────────────────────────────────────

fn local_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Exchange-corrected "now" in ms.
fn now_ms() -> i64 {
    local_ms() + SERVER_OFFSET_MS.load(Ordering::Relaxed)
}

fn sign(secret: &str, query: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key");
    mac.update(query.as_bytes());
    let bytes = mac.finalize().into_bytes();
    to_hex(&bytes)
}

fn to_hex(bytes: &[u8]) -> String {
    static HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

fn fmt_decimal(v: f64) -> String {
    // Binance accepts up to 8 decimals; trim trailing zeros for clean
    // signing payloads.
    let s = format!("{:.8}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() {
        "0".into()
    } else {
        s
    }
}

/// `(symbol, side, query)` for an order action; `None` for non-orders.
/// The query has no timestamp / signature yet.
fn order_request(action: &Action, client_id: &str) -> Option<(String, Side, String)> {
    let (symbol, side, kind, params): (&String, Side, &str, Vec<(&str, String)>) = match action {
        Action::BuyMarket { symbol, qty } => (
            symbol,
            Side::Buy,
            "MARKET",
            vec![("quantity", fmt_decimal(*qty))],
        ),
        Action::SellMarket { symbol, qty } => (
            symbol,
            Side::Sell,
            "MARKET",
            vec![("quantity", fmt_decimal(*qty))],
        ),
        Action::BuyLimit { symbol, qty, limit } => (
            symbol,
            Side::Buy,
            "LIMIT",
            vec![
                ("timeInForce", "GTC".into()),
                ("quantity", fmt_decimal(*qty)),
                ("price", fmt_decimal(*limit)),
            ],
        ),
        Action::SellLimit { symbol, qty, limit } => (
            symbol,
            Side::Sell,
            "LIMIT",
            vec![
                ("timeInForce", "GTC".into()),
                ("quantity", fmt_decimal(*qty)),
                ("price", fmt_decimal(*limit)),
            ],
        ),
        Action::CancelAll { .. } | Action::Note { .. } => return None,
    };
    let side_str = if side == Side::Buy { "BUY" } else { "SELL" };
    let mut q = format!("symbol={symbol}&side={side_str}&type={kind}&newClientOrderId={client_id}");
    for (k, v) in &params {
        q.push('&');
        q.push_str(k);
        q.push('=');
        q.push_str(v);
    }
    Some((symbol.clone(), side, q))
}

fn parse_num(v: &serde_json::Value, key: &str) -> f64 {
    v[key].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// Turn an order JSON (POST /order reply or GET /order) into events.
///
/// * `FILLED`            → one `Rest` fill for the whole executed quantity;
/// * `NEW` / `PARTIALLY_FILLED` → `OrderQueued` for the REMAINING quantity
///   (any partial execution is reported by the user-data stream, which is
///   authoritative for in-flight orders);
/// * `CANCELED` / `EXPIRED` / `REJECTED` → a fill if something executed
///   first, otherwise an `Err` (the order is not resting and did nothing).
fn parse_order_json(
    v: &serde_json::Value,
    symbol: &str,
    side: Side,
    client_id: &str,
) -> Result<Vec<LiveEvent>, String> {
    let order_id = v["orderId"].as_u64().unwrap_or(0);
    let status = v["status"].as_str().unwrap_or("");
    let executed = parse_num(v, "executedQty");
    let cum_quote = parse_num(v, "cummulativeQuoteQty");
    let orig = parse_num(v, "origQty");
    let fill = |executed: f64| LiveEvent::OrderFilled {
        symbol: symbol.to_string(),
        side,
        qty: executed,
        price: if executed > 0.0 {
            cum_quote / executed
        } else {
            0.0
        },
        time_ms: now_ms(),
        order_id,
        trade_id: 0,
        client_id: client_id.to_string(),
        source: FillSource::Rest,
    };
    match status {
        "FILLED" if executed > 0.0 => Ok(vec![fill(executed)]),
        "NEW" | "PARTIALLY_FILLED" => Ok(vec![LiveEvent::OrderQueued {
            symbol: symbol.to_string(),
            side,
            qty: (orig - executed).max(0.0),
            limit: parse_num(v, "price"),
            order_id,
            created_ms: now_ms(),
            client_id: client_id.to_string(),
        }]),
        "CANCELED" | "EXPIRED" | "EXPIRED_IN_MATCH" | "REJECTED" => {
            if executed > 0.0 {
                // Market remainder expired after a partial: the part that
                // executed is final.
                Ok(vec![fill(executed)])
            } else {
                Err(format!("order {status}"))
            }
        }
        other => Err(format!("unexpected status `{other}`: {v}")),
    }
}

fn post_order(
    key: &str,
    secret: &str,
    base: &str,
    recv: u32,
    action: &Action,
    client_id: &str,
) -> Result<Vec<LiveEvent>, ApiError> {
    let (symbol, side, mut q) = order_request(action, client_id)
        .ok_or_else(|| ApiError::Definite("not an order".into()))?;
    q.push_str(&format!("&recvWindow={recv}&timestamp={}", now_ms()));
    let sig = sign(secret, &q);
    let url = format!("{base}/api/v3/order?{q}&signature={sig}");
    let resp = agent()
        .post(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(classify_ureq_error)?;
    let body = resp
        .into_string()
        .map_err(|e| ApiError::Unknown(format!("read body: {e}")))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ApiError::Unknown(format!("json: {e}")))?;
    // A 2xx we cannot interpret is NOT a rejection: the order may exist.
    parse_order_json(&v, &symbol, side, client_id).map_err(|e| {
        if v["status"].as_str().is_some() {
            ApiError::Definite(e)
        } else {
            ApiError::Unknown(e)
        }
    })
}

/// After an `OrderUnknown`: ask the exchange what happened to `client_id`.
fn resolve_unknown(c: &Creds, action: &Action, client_id: &str) {
    let Some((symbol, side, _)) = order_request(action, client_id) else {
        return;
    };
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(1500));
        let q = format!(
            "symbol={symbol}&origClientOrderId={client_id}&recvWindow={}&timestamp={}",
            c.recv,
            now_ms()
        );
        let sig = sign(&c.secret, &q);
        let url = format!("{}/api/v3/order?{q}&signature={sig}", c.base);
        let res = agent()
            .get(&url)
            .set("X-MBX-APIKEY", &c.key)
            .timeout(Duration::from_secs(10))
            .call();
        match res {
            Ok(resp) => {
                let parsed = resp
                    .into_string()
                    .ok()
                    .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok());
                if let Some(v) = parsed {
                    match parse_order_json(&v, &symbol, side, client_id) {
                        Ok(events) => {
                            for ev in events {
                                let _ = c.tx.send(ev);
                            }
                        }
                        Err(reason) => {
                            let _ = c.tx.send(LiveEvent::OrderRejected {
                                action: action.clone(),
                                client_id: client_id.to_string(),
                                reason,
                            });
                        }
                    }
                    return;
                }
            }
            // -2013 "Order does not exist": it never reached the book.
            Err(ureq::Error::Status(400, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                if body.contains("-2013") {
                    let _ = c.tx.send(LiveEvent::OrderRejected {
                        action: action.clone(),
                        client_id: client_id.to_string(),
                        reason: "not found on the exchange after a timeout (never placed)".into(),
                    });
                    return;
                }
            }
            Err(_) => {}
        }
    }
    let _ = c.tx.send(LiveEvent::SyncError(format!(
        "order {client_id} ({}) outcome UNKNOWN — check the exchange before retrying",
        action.label()
    )));
}

fn delete_order(
    key: &str,
    secret: &str,
    base: &str,
    recv: u32,
    symbol: &str,
    order_id: u64,
) -> Result<(), ApiError> {
    let q = format!(
        "symbol={symbol}&orderId={order_id}&recvWindow={recv}&timestamp={}",
        now_ms()
    );
    let sig = sign(secret, &q);
    let url = format!("{base}/api/v3/order?{q}&signature={sig}");
    agent()
        .delete(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(classify_ureq_error)?;
    Ok(())
}

/// What a Binance API key is allowed to do (`GET /sapi/v1/account/apiRestrictions`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPermissions {
    pub reading: bool,
    pub trading: bool,
    pub withdrawals: bool,
    pub ip_restricted: bool,
}

/// `None` unless both `enableReading` and `enableWithdrawals` are present: a
/// reply we cannot read is never taken as "safe".
pub fn parse_key_permissions(v: &serde_json::Value) -> Option<KeyPermissions> {
    Some(KeyPermissions {
        reading: v.get("enableReading")?.as_bool()?,
        trading: v
            .get("enableSpotAndMarginTrading")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        withdrawals: v.get("enableWithdrawals")?.as_bool()?,
        ip_restricted: v
            .get("ipRestrict")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    })
}

impl KeyPermissions {
    /// Plain-words verdict for the status line.  Never contains key material.
    pub fn summary(&self) -> String {
        let ip = if self.ip_restricted {
            "IP-restricted"
        } else {
            "not IP-restricted"
        };
        if self.withdrawals {
            "WARNING: this key can WITHDRAW funds. Create a key without withdrawal permission."
                .into()
        } else if !self.reading {
            "Key is valid but has no read permission.".into()
        } else if self.trading {
            format!("OK: can read and trade, cannot withdraw ({ip}).")
        } else {
            format!("OK: read-only ({ip}).")
        }
    }
}

/// Ask Binance what `key` may do.  Needs only a valid key (no trading
/// permission), sends nothing but the signed read request.
pub fn check_key(key: &str, secret: &str, base: &str) -> Result<KeyPermissions, String> {
    if crate::data::demo::is_on() {
        return Err("demo mode has no exchange connection".into());
    }
    let base = validate_base(base)?;
    let base = base.as_str();
    let v = signed_get(key, secret, base, 5000, "/sapi/v1/account/apiRestrictions")?;
    parse_key_permissions(&v).ok_or_else(|| "unexpected reply from apiRestrictions".to_string())
}

fn signed_get(
    key: &str,
    secret: &str,
    base: &str,
    recv: u32,
    path: &str,
) -> Result<serde_json::Value, String> {
    let q = format!("recvWindow={recv}&timestamp={}", now_ms());
    let sig = sign(secret, &q);
    let url = format!("{base}{path}?{q}&signature={sig}");
    let resp = agent()
        .get(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(describe_ureq_error)?;
    let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("json: {e}"))
}

fn get_account(
    key: &str,
    secret: &str,
    base: &str,
    recv: u32,
) -> Result<Vec<BalanceEntry>, String> {
    let v = signed_get(key, secret, base, recv, "/api/v3/account")?;
    let arr = v["balances"]
        .as_array()
        .ok_or_else(|| format!("no balances: {v}"))?;
    Ok(arr
        .iter()
        .filter_map(|b| {
            let free = parse_num(b, "free");
            let locked = parse_num(b, "locked");
            (free > 0.0 || locked > 0.0).then(|| BalanceEntry {
                asset: b["asset"].as_str().unwrap_or("").to_string(),
                free,
                locked,
            })
        })
        .collect())
}

fn get_open_orders(
    key: &str,
    secret: &str,
    base: &str,
    recv: u32,
) -> Result<Vec<OpenOrderInfo>, String> {
    let v = signed_get(key, secret, base, recv, "/api/v3/openOrders")?;
    let list = v.as_array().ok_or_else(|| format!("not array: {v}"))?;
    Ok(list
        .iter()
        .map(|o| OpenOrderInfo {
            order_id: o["orderId"].as_u64().unwrap_or(0),
            symbol: o["symbol"].as_str().unwrap_or("").to_string(),
            side: if o["side"].as_str() == Some("BUY") {
                Side::Buy
            } else {
                Side::Sell
            },
            // Remaining quantity, not the original one.
            qty: (parse_num(o, "origQty") - parse_num(o, "executedQty")).max(0.0),
            price: parse_num(o, "price"),
            created_ms: o["time"].as_i64().unwrap_or(0),
        })
        .collect())
}

/// `serverTime - local midpoint of the request`.
fn fetch_server_offset(base: &str) -> Result<i64, String> {
    let t0 = local_ms();
    let resp = agent()
        .get(&format!("{base}/api/v3/time"))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(describe_ureq_error)?;
    let t1 = local_ms();
    let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("json: {e}"))?;
    let server = v["serverTime"].as_i64().ok_or("no serverTime")?;
    Ok(server - (t0 + t1) / 2)
}

fn describe_ureq_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!(
                "HTTP {code}: {}",
                body.trim().chars().take(200).collect::<String>()
            )
        }
        // `Display` would print the request URL, signature included: keep it out
        // of status lines and logs.
        ureq::Error::Transport(t) => format!(
            "transport: {:?} {}",
            t.kind(),
            t.message().unwrap_or_default()
        ),
    }
}

/// 4xx → the exchange refused (definite); everything else → unknown.
fn classify_ureq_error(e: ureq::Error) -> ApiError {
    let definite = matches!(&e, ureq::Error::Status(code, _) if (400..500).contains(code));
    let text = describe_ureq_error(e);
    if definite {
        ApiError::Definite(text)
    } else {
        ApiError::Unknown(text)
    }
}

// ─── User Data Stream (listenKey) ────────────────────────────────────

/// Create a listen key for the user data stream.
/// `POST /api/v3/userDataStream` (unsigned, just API key header).
pub fn create_listen_key(base: &str, key: &str) -> Result<String, String> {
    let url = format!("{}/api/v3/userDataStream", base);
    let resp = agent()
        .post(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(describe_ureq_error)?;
    let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("json: {e}"))?;
    v["listenKey"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("no listenKey in: {body}"))
}

/// Keep a listen key alive (must be called every 30 min).
/// `PUT /api/v3/userDataStream` (unsigned).
pub fn keepalive_listen_key(base: &str, key: &str, listen_key: &str) -> Result<(), String> {
    let url = format!("{base}/api/v3/userDataStream?listenKey={listen_key}");
    agent()
        .put(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(describe_ureq_error)?;
    Ok(())
}

/// Delete a listen key (clean up on disconnect).
/// `DELETE /api/v3/userDataStream` (unsigned).
pub fn delete_listen_key(base: &str, key: &str, listen_key: &str) -> Result<(), String> {
    let url = format!("{base}/api/v3/userDataStream?listenKey={listen_key}");
    agent()
        .delete(&url)
        .set("X-MBX-APIKEY", key)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(describe_ureq_error)?;
    Ok(())
}

/// Handle for the user-data WebSocket thread.  Lives on `Runtime` and
/// is torn down during `disable_live()`.
pub struct UserDataWs {
    pub stop_tx: Sender<()>,
    pub thread: Option<JoinHandle<()>>,
}

impl UserDataWs {
    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

/// Spawn a user-data WS connection that streams fills (and cancels) into the
/// same event channel used by REST.  Runs on the shared reconnecting
/// `ws_loop`.
pub fn spawn_user_data_ws(base: &str, listen_key: String, tx: Sender<LiveEvent>) -> UserDataWs {
    let url = if base.contains("testnet") {
        format!("wss://testnet.binance.vision/ws/{listen_key}")
    } else {
        format!("wss://stream.binance.com:9443/ws/{listen_key}")
    };
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("ws-userdata".into())
        .spawn(move || {
            ws_loop(&url, &stop_rx, &mut |sig| {
                if let Signal::Text(text) = sig {
                    let ev = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| parse_execution_report(&v));
                    if let Some(ev) = ev {
                        if tx.send(ev).is_err() {
                            return Flow::Stop;
                        }
                    }
                }
                Flow::Continue
            })
        })
        .ok();
    UserDataWs { stop_tx, thread }
}

/// `executionReport`: `TRADE` → a fill; `CANCELED` / `EXPIRED` → the order is
/// gone; everything else → `None`.
fn parse_execution_report(v: &serde_json::Value) -> Option<LiveEvent> {
    if v.get("e")?.as_str()? != "executionReport" {
        return None;
    }
    let symbol = v.get("s")?.as_str()?.to_string();
    let order_id = v.get("i")?.as_u64()?;
    match v.get("x")?.as_str()? {
        "TRADE" => {
            let side = match v.get("S")?.as_str()? {
                "BUY" => Side::Buy,
                _ => Side::Sell,
            };
            let qty: f64 = v.get("l")?.as_str()?.parse().ok()?;
            let price: f64 = v.get("L")?.as_str()?.parse().ok()?;
            let time_ms = v.get("T")?.as_i64()?;
            if qty <= 0.0 || price <= 0.0 {
                return None;
            }
            Some(LiveEvent::OrderFilled {
                symbol,
                side,
                qty,
                price,
                time_ms,
                order_id,
                trade_id: v.get("t").and_then(|t| t.as_u64()).unwrap_or(0),
                client_id: v
                    .get("c")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: FillSource::UserData,
            })
        }
        "CANCELED" | "EXPIRED" | "REJECTED" => Some(LiveEvent::OrderCancelled { symbol, order_id }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hmac_signature_matches_official_test_vector() {
        // From Binance API docs:
        //   secret  = "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j"
        //   query   = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559"
        //   sig     = "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        let secret = "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j";
        let query = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";
        let expected = "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71";
        assert_eq!(sign(secret, query), expected);
    }

    /// A 302 from the API host must not carry `X-MBX-APIKEY` anywhere else.
    #[test]
    fn signed_calls_do_not_follow_redirects() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let target_port = target.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let end = std::time::Instant::now() + Duration::from_millis(1500);
            while std::time::Instant::now() < end {
                if target.accept().is_ok() {
                    hits2.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let api = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_port = api.local_addr().unwrap().port();
        let a = std::thread::spawn(move || {
            if let Ok((mut c, _)) = api.accept() {
                let mut buf = [0u8; 2048];
                let _ = c.read(&mut buf);
                let _ = c.write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target_port}/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
            }
        });
        let _ = signed_get(
            "KEY",
            "SECRET",
            &format!("http://127.0.0.1:{api_port}"),
            5000,
            "/p",
        );
        a.join().unwrap();
        t.join().unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "redirect target was contacted"
        );
    }

    #[test]
    fn keys_only_go_to_binance_hosts_over_https() {
        assert_eq!(
            validate_base("https://api.binance.com/").unwrap(),
            PROD_BASE
        );
        assert_eq!(
            validate_base(" https://testnet.binance.vision ").unwrap(),
            TESTNET_BASE
        );
        for bad in [
            "http://api.binance.com",
            "https://api.binance.com.evil.com",
            "https://evil.com",
            "http://127.0.0.1:9",
            "https://api.binance.com:444",
            "",
        ] {
            assert!(validate_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn userinfo_trick_is_rejected() {
        // User-info before the at-sign: the real host is what follows it.
        let tricky = format!("https://api.binance.com{}evil.host", '@');
        assert!(validate_base(&tricky).is_err());
    }

    #[test]
    fn key_permissions_are_read_fail_safe() {
        let ro = json!({"enableReading":true,"enableSpotAndMarginTrading":false,"enableWithdrawals":false,"ipRestrict":true});
        let p = parse_key_permissions(&ro).unwrap();
        assert!(p.summary().starts_with("OK: read-only"));
        let trade = json!({"enableReading":true,"enableSpotAndMarginTrading":true,"enableWithdrawals":false});
        assert!(parse_key_permissions(&trade)
            .unwrap()
            .summary()
            .starts_with("OK: can read and trade"));
        let wd = json!({"enableReading":true,"enableSpotAndMarginTrading":true,"enableWithdrawals":true});
        assert!(parse_key_permissions(&wd)
            .unwrap()
            .summary()
            .contains("WITHDRAW"));
        // Missing / malformed fields are not "safe".
        assert!(parse_key_permissions(&json!({"enableReading":true})).is_none());
        assert!(parse_key_permissions(&json!({"enableWithdrawals":false})).is_none());
        assert!(parse_key_permissions(&json!({"code":-2015,"msg":"Invalid API-key"})).is_none());
    }

    #[test]
    fn fmt_decimal_strips_trailing_zeros() {
        assert_eq!(fmt_decimal(1.0), "1");
        assert_eq!(fmt_decimal(1.5), "1.5");
        assert_eq!(fmt_decimal(0.10000000), "0.1");
        assert_eq!(fmt_decimal(0.00000001), "0.00000001");
    }

    #[test]
    fn order_request_carries_the_client_order_id() {
        let a = Action::BuyLimit {
            symbol: "BTCUSDT".into(),
            qty: 0.5,
            limit: 60000.0,
        };
        let (sym, side, q) = order_request(&a, "dosabc").unwrap();
        assert_eq!(sym, "BTCUSDT");
        assert_eq!(side, Side::Buy);
        assert_eq!(
            q,
            "symbol=BTCUSDT&side=BUY&type=LIMIT&newClientOrderId=dosabc&timeInForce=GTC&quantity=0.5&price=60000"
        );
        assert!(order_request(&Action::CancelAll { symbol: "X".into() }, "c").is_none());
        assert_ne!(
            BinanceClient::new_client_id(),
            BinanceClient::new_client_id()
        );
    }

    fn first_fill(evs: Vec<LiveEvent>) -> (f64, f64, FillSource) {
        match evs.into_iter().next() {
            Some(LiveEvent::OrderFilled {
                qty, price, source, ..
            }) => (qty, price, source),
            other => panic!("expected a fill, got {other:?}"),
        }
    }

    #[test]
    fn filled_market_reply_is_one_rest_fill_at_the_average_price() {
        let v = json!({"orderId":7,"status":"FILLED","executedQty":"2","cummulativeQuoteQty":"200","origQty":"2"});
        let (qty, price, src) = first_fill(parse_order_json(&v, "X", Side::Buy, "c").unwrap());
        assert_eq!((qty, price, src), (2.0, 100.0, FillSource::Rest));
    }

    #[test]
    fn partially_filled_reply_queues_only_the_remainder() {
        // The executed part is the user-data stream's to report (no double count).
        let v = json!({"orderId":7,"status":"PARTIALLY_FILLED","executedQty":"1","origQty":"3","price":"50"});
        match parse_order_json(&v, "X", Side::Sell, "c")
            .unwrap()
            .remove(0)
        {
            LiveEvent::OrderQueued { qty, limit, .. } => assert_eq!((qty, limit), (2.0, 50.0)),
            other => panic!("expected queued, got {other:?}"),
        }
    }

    #[test]
    fn expired_market_remainder_keeps_the_executed_part_and_empty_expiry_is_an_error() {
        let part = json!({"orderId":7,"status":"EXPIRED","executedQty":"1","cummulativeQuoteQty":"10","origQty":"3"});
        assert_eq!(
            first_fill(parse_order_json(&part, "X", Side::Buy, "c").unwrap()).0,
            1.0
        );
        let none = json!({"orderId":7,"status":"EXPIRED","executedQty":"0","origQty":"3"});
        assert!(parse_order_json(&none, "X", Side::Buy, "c").is_err());
        assert!(parse_order_json(&json!({"status":"WAT"}), "X", Side::Buy, "c").is_err());
    }
}

#[cfg(test)]
mod ws_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn execution_report_trade_becomes_fill_with_ids() {
        let v = json!({"e":"executionReport","x":"TRADE","s":"BTCUSDT","S":"BUY",
            "l":"0.5","L":"65000.0","i":42,"t":9001,"c":"dos1x1","T":1700000000000_i64});
        match parse_execution_report(&v) {
            Some(LiveEvent::OrderFilled {
                symbol,
                qty,
                order_id,
                trade_id,
                client_id,
                source,
                ..
            }) => {
                assert_eq!((symbol.as_str(), qty, order_id), ("BTCUSDT", 0.5, 42));
                assert_eq!((trade_id, client_id.as_str()), (9001, "dos1x1"));
                assert_eq!(source, FillSource::UserData);
            }
            _ => panic!("expected OrderFilled"),
        }
        let new = json!({"e":"executionReport","x":"NEW","s":"BTCUSDT","i":1});
        assert!(parse_execution_report(&new).is_none());
        assert!(parse_execution_report(&json!({"e":"outboundAccountPosition"})).is_none());
    }

    #[test]
    fn execution_report_cancel_and_expiry_drop_the_order() {
        for x in ["CANCELED", "EXPIRED", "REJECTED"] {
            let v = json!({"e":"executionReport","x":x,"s":"BTCUSDT","i":42});
            assert!(matches!(
                parse_execution_report(&v),
                Some(LiveEvent::OrderCancelled { order_id: 42, .. })
            ));
        }
    }
}
