//! Linking between several `dos` windows on one machine.
//!
//! Every window owns a tiny Unix socket `<dir>/<id>.sock` (owner-only,
//! no network).  A window that is in a **link group** (1-8) sends the symbol
//! it shows to every other live socket; windows in the same group show it
//! too.  Only navigation travels this way: never orders, limits or mandates.
//!
//! - [`Node`] — the sockets: listen, publish, count live peers.
//! - [`Linker`] — the rules on top: group, debounce, echo guard.
//! - [`Lease`] — "which window trades": one holder at a time, taken with an
//!   atomic file create and checked for life through the holder's socket.
//!   Nothing enforces it yet: stage 4 of the linking plan makes the engine
//!   follow it.
//!
//! The network is never touched; a dead window leaves a socket file that the
//! next scan removes.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_GROUP: u8 = 8;
/// Longest line accepted from a socket (bytes).
pub const MAX_LINE: usize = 4096;
/// A change must stay this long (ms) before it is sent: scrubbing a list
/// with the arrow keys must not make ten windows download ten charts.
pub const DEBOUNCE_MS: i64 = 250;
/// Unix `sun_path` is ~104 bytes on macOS (108 on Linux).
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 100;

/// One message between windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    /// "The group's symbol is now `symbol`" (Binance form, e.g. `BTCUSDT`).
    Symbol { group: u8, symbol: String },
    /// The group's chart timeframe (`15s 1m 5m 1h 1d`).
    Timeframe { group: u8, tf: String },
    /// The group's active watchlist, by name.
    List { group: u8, name: String },
    /// Window `from` asks the trading window `to` to hand trading over.
    TradeHandoff { to: String, from: String },
    /// `to` may trade now; `from` has stopped (`agent`: it served agents).
    TradeGranted {
        to: String,
        from: String,
        agent: bool,
    },
    /// `from` cannot hand over now (live mode).
    TradeDenied { to: String, from: String },
    /// `from` saved the watchlists: reload them from the database.
    ListsChanged { from: String },
    /// `from` is saving a desk: every window answers with a [`Msg::DeskReport`].
    DeskQuery { from: String, req: u32 },
    /// `from` closes its desk: every window quits (each saves as on F10).
    DeskClose { from: String },
    /// `from` tiles the desk `desk`: each window finds its frame among the
    /// first of each pair and takes the second.
    DeskTile {
        from: String,
        desk: String,
        moves: Vec<(crate::desk::Frame, crate::desk::Frame)>,
    },
    /// A view-only window asks the trading window to trade (the chart's
    /// buttons and lines).  Only the window holding the lease acts on it, through the same
    /// risk gate as its own orders, and answers with [`Msg::OrderDone`].
    Order {
        from: String,
        req: u32,
        symbol: String,
        cmd: OrderCmd,
    },
    /// A view-only window asks the trading window to open a basket (Compare's
    /// "Start" and `b`): the same checks and the same entry as its own
    /// baskets.  Answered with [`Msg::OrderDone`].
    Basket {
        from: String,
        req: u32,
        basket: BasketReq,
    },
    /// A view-only window asks the trading window to keep an auto rule.
    Rule {
        from: String,
        req: u32,
        rule: crate::autobasket::Rule,
    },
    /// What became of `from`'s [`Msg::Order`] `req`, in words.
    OrderDone {
        to: String,
        from: String,
        req: u32,
        text: String,
    },
    /// A window's place in the desk `from` asked about (`req`).
    DeskReport {
        to: String,
        from: String,
        req: u32,
        win: crate::desk::DeskWindow,
    },
}

/// A basket as it travels (one line of the socket holds [`MAX_LINE`] bytes):
/// the legs in dollars, coins fixed in advance, how long it may live, and the
/// prices the sender knew, for a trading window that does not stream them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BasketReq {
    pub name: String,
    pub origin: String,
    pub legs: Vec<(String, bool, f64)>,
    pub fixed: Vec<(String, f64)>,
    pub max_minutes: Option<u32>,
    pub prices: Vec<(String, f64)>,
}

/// A leg's symbol: a coin pair or an option (`ETH-250625-3000-C`).
pub fn valid_leg_symbol(s: &str) -> bool {
    (2..=30).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'@'
        })
}

impl BasketReq {
    pub fn is_valid(&self) -> bool {
        let text = |t: &str| {
            t.chars().count() <= 80 && !t.chars().any(crate::widgets::fmt::is_unsafe_char)
        };
        let num = |v: f64| v.is_finite() && v > 0.0 && v < 1e12;
        text(&self.name)
            && text(&self.origin)
            && (1..=crate::basket::MAX_LEGS).contains(&self.legs.len())
            && self.fixed.len() <= crate::basket::MAX_LEGS
            && self.prices.len() <= 2 * crate::basket::MAX_LEGS
            && self
                .legs
                .iter()
                .all(|(s, _, usd)| valid_leg_symbol(s) && num(*usd) && *usd <= 1e7)
            && self
                .fixed
                .iter()
                .all(|(s, q)| valid_leg_symbol(s) && num(*q))
            && self
                .prices
                .iter()
                .all(|(s, p)| valid_leg_symbol(s) && num(*p))
            && self
                .max_minutes
                .is_none_or(|m| (1..=1_000_000).contains(&m))
    }
}

/// What a [`Msg::Order`] asks for.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum OrderCmd {
    /// Buy or sell `qty` at market.
    Market { buy: bool, qty: f64 },
    /// Close the whole position at market.
    Close,
    /// The position's stop and take, both as they should be (`None`: none).
    Bracket { sl: Option<f64>, tp: Option<f64> },
    /// A resting limit for `qty` at `price` (from the order book's ladder).
    Limit { buy: bool, qty: f64, price: f64 },
    /// Cancel this window's resting orders at `price`, give or take
    /// `within` (half of the book's row at the clicking window's zoom).
    CancelAt { price: f64, within: f64 },
}

impl OrderCmd {
    pub fn is_valid(&self) -> bool {
        let price = |p: &Option<f64>| p.is_none_or(|v| v.is_finite() && v > 0.0 && v < 1e12);
        match self {
            OrderCmd::Market { qty, .. } => qty.is_finite() && *qty > 0.0 && *qty < 1e12,
            OrderCmd::Close => true,
            OrderCmd::Bracket { sl, tp } => price(sl) && price(tp),
            OrderCmd::Limit { qty, price: p, .. } => {
                qty.is_finite() && *qty > 0.0 && *qty < 1e12 && price(&Some(*p))
            }
            OrderCmd::CancelAt { price: p, within } => {
                price(&Some(*p)) && within.is_finite() && *within >= 0.0 && *within < *p
            }
        }
    }
}

/// Longest answer text a window shows from another (chars).
pub const MAX_ORDER_TEXT: usize = 200;

/// A symbol a window may be told to show: 2-20 chars of `A-Z0-9`.
pub fn valid_symbol(s: &str) -> bool {
    (2..=20).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A timeframe a window may send or be started with: its exact name.
pub fn valid_timeframe(s: &str) -> bool {
    crate::markets::Timeframe::ALL.iter().any(|t| t.name() == s)
}

/// A list name another window may ask for: 1-24 printable chars.
pub fn valid_list_name(s: &str) -> bool {
    let n = s.chars().count();
    (1..=crate::watchlists::MAX_NAME_CHARS).contains(&n)
        && !s.chars().any(crate::widgets::fmt::is_unsafe_char)
}

/// A window id as written by [`Node::start`]: `<pid>-<n>`.
pub fn valid_id(s: &str) -> bool {
    (1..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Msg {
    /// Incoming data is untrusted: anything off is dropped.
    pub fn is_valid(&self) -> bool {
        let group_ok = |g: &u8| (1..=MAX_GROUP).contains(g);
        match self {
            Msg::Symbol { group, symbol } => group_ok(group) && valid_symbol(symbol),
            Msg::Timeframe { group, tf } => group_ok(group) && valid_timeframe(tf),
            Msg::List { group, name } => group_ok(group) && valid_list_name(name),
            Msg::TradeHandoff { to, from }
            | Msg::TradeGranted { to, from, .. }
            | Msg::TradeDenied { to, from } => valid_id(to) && valid_id(from),
            Msg::ListsChanged { from } => valid_id(from),
            Msg::Order {
                from, symbol, cmd, ..
            } => valid_id(from) && valid_symbol(symbol) && cmd.is_valid(),
            Msg::Basket { from, basket, .. } => valid_id(from) && basket.is_valid(),
            Msg::Rule { from, rule, .. } => valid_id(from) && rule.validate().is_ok(),
            Msg::OrderDone { to, from, text, .. } => {
                valid_id(to)
                    && valid_id(from)
                    && text.chars().count() <= MAX_ORDER_TEXT
                    && !text.chars().any(crate::widgets::fmt::is_unsafe_char)
            }
            Msg::DeskQuery { from, .. } | Msg::DeskClose { from } => valid_id(from),
            Msg::DeskReport { to, from, win, .. } => {
                valid_id(to) && valid_id(from) && win.validate().is_ok()
            }
            Msg::DeskTile { from, desk, moves } => {
                valid_id(from)
                    && crate::desk::valid_name(desk)
                    && moves.len() <= crate::desk::MAX_WINDOWS
                    && moves
                        .iter()
                        .all(|(a, b)| crate::desk::valid_frame(a) && crate::desk::valid_frame(b))
            }
        }
    }
}

/// Where the sockets live: `DOS_LINK_DIR`, else `link/` inside the agent
/// directory (a temp dir in demo mode).
pub fn link_dir(demo: bool) -> Option<PathBuf> {
    if let Ok(d) = std::env::var("DOS_LINK_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    crate::agent::agent_dir(demo).map(|d| d.join("link"))
}

/// Linking is on unless `DOS_LINK=0`.
pub fn enabled() -> bool {
    !matches!(std::env::var("DOS_LINK").as_deref(), Ok("0") | Ok("off"))
}

// ─── Linker: group, debounce, echo guard ───────────────────────────────

/// What a link group shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Symbol = 0,
    Timeframe = 1,
    List = 2,
}

const KINDS: [Kind; 3] = [Kind::Symbol, Kind::Timeframe, Kind::List];

#[derive(Default)]
struct Chan {
    /// The value as last observed here or applied from a peer.  A change of
    /// the local value against it is a local edit worth publishing.
    seen: Option<String>,
    pending: Option<(String, i64)>,
    /// When the local value last changed (ms); `None`: never.
    changed_at: Option<i64>,
}

/// What arrived since the last [`Linker::poll`]: the newest value per kind
/// for our group, and every trade-handoff message.
#[derive(Debug, Default, PartialEq)]
pub struct Inbox {
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub list: Option<String>,
    pub trade: Vec<Msg>,
    /// Another window saved the watchlists.
    pub lists_changed: bool,
    /// Desk queries and reports (whatever the group).
    pub desk: Vec<Msg>,
}

/// The rules on top of [`Node`].  Time is passed in, never read.
pub struct Linker {
    node: Node,
    /// 0 = not linked.
    group: u8,
    chans: [Chan; 3],
}

impl Linker {
    pub fn new(node: Node) -> Self {
        Self {
            node,
            group: 0,
            chans: Default::default(),
        }
    }

    pub fn node(&self) -> &Node {
        &self.node
    }

    pub fn group(&self) -> u8 {
        self.group
    }

    /// Join group `group` (0 = leave).  Joining adopts nothing and sends
    /// nothing: the window keeps what it shows until someone changes it.
    /// `locals` are the current values in [`Kind`] order.
    pub fn set_group(&mut self, group: u8, locals: [Option<&str>; 3]) {
        self.group = group.min(MAX_GROUP);
        for (c, l) in self.chans.iter_mut().zip(locals) {
            c.seen = l.map(str::to_string);
            c.pending = None;
        }
    }

    /// Drain the inbox.  Group messages for other groups are dropped.
    pub fn poll(&mut self) -> Inbox {
        let mut inbox = Inbox::default();
        for m in self.node.poll() {
            let mine = |g: u8| self.group != 0 && g == self.group;
            match m {
                Msg::Symbol { group, symbol } if mine(group) => inbox.symbol = Some(symbol),
                Msg::Timeframe { group, tf } if mine(group) => inbox.timeframe = Some(tf),
                Msg::List { group, name } if mine(group) => inbox.list = Some(name),
                Msg::TradeHandoff { .. }
                | Msg::TradeGranted { .. }
                | Msg::TradeDenied { .. }
                | Msg::Order { .. }
                | Msg::Basket { .. }
                | Msg::Rule { .. }
                | Msg::OrderDone { .. } => inbox.trade.push(m),
                Msg::ListsChanged { .. } => inbox.lists_changed = true,
                Msg::DeskQuery { .. }
                | Msg::DeskReport { .. }
                | Msg::DeskClose { .. }
                | Msg::DeskTile { .. } => inbox.desk.push(m),
                _ => {}
            }
        }
        inbox
    }

    /// A peer's value was applied; `local_after` is what this window shows
    /// now (it may not have changed, e.g. the symbol is not in this list).
    pub fn adopted(&mut self, kind: Kind, local_after: Option<&str>) {
        let c = &mut self.chans[kind as usize];
        c.seen = local_after.map(str::to_string);
        c.pending = None;
    }

    /// Feed what this window shows now.  A local change that stayed for
    /// [`DEBOUNCE_MS`] is published, and so is the first change after a
    /// quiet [`DEBOUNCE_MS`]; returns whether one was sent.
    pub fn observe(&mut self, kind: Kind, local: Option<&str>, now_ms: i64) -> bool {
        let group = self.group;
        let c = &mut self.chans[kind as usize];
        let Some(local) = local else {
            c.pending = None;
            return false;
        };
        if group == 0 || c.seen.as_deref() == Some(local) {
            if group == 0 {
                c.seen = Some(local.to_string());
            }
            c.pending = None;
            return false;
        }
        // A change on a quiet channel goes out at once (a click is seen by
        // the other windows in the same frame); within a burst (arrow keys
        // scrubbing a list) only a value that stood for DEBOUNCE_MS does.
        let quiet = c.changed_at.is_none_or(|t| now_ms - t >= DEBOUNCE_MS);
        let since = match &c.pending {
            Some((s, t)) if s == local => *t,
            _ => {
                c.changed_at = Some(now_ms);
                now_ms
            }
        };
        c.pending = Some((local.to_string(), since));
        if !quiet && now_ms - since < DEBOUNCE_MS {
            return false;
        }
        c.seen = Some(local.to_string());
        c.pending = None;
        let value = local.to_string();
        self.node.publish(match kind {
            Kind::Symbol => Msg::Symbol {
                group,
                symbol: value,
            },
            Kind::Timeframe => Msg::Timeframe { group, tf: value },
            Kind::List => Msg::List { group, name: value },
        });
        true
    }

    /// Send a window-to-window message (trade handoff).
    pub fn send(&self, msg: Msg) {
        self.node.publish(msg);
    }
}

/// All kinds, in the order [`Linker::set_group`] expects.
pub fn kinds() -> [Kind; 3] {
    KINDS
}

// ─── Lease: which window trades ────────────────────────────────────────

pub const LEASE_FILE: &str = "trade.lease";

#[derive(Debug)]
pub enum LeaseError {
    /// Another live window holds it (its id).
    Held(String),
    Io(io::Error),
}

/// The right to trade.  Dropping it (window closed) releases it.
pub struct Lease {
    path: PathBuf,
    id: String,
}

impl Lease {
    /// Take the lease for window `id`, unless a live window holds it.
    pub fn acquire(dir: &Path, id: &str) -> Result<Lease, LeaseError> {
        use std::fs::OpenOptions;
        use std::io::Write;
        let path = dir.join(LEASE_FILE);
        let me = |path: &PathBuf| Lease {
            path: path.clone(),
            id: id.to_string(),
        };
        let create = |path: &Path| -> io::Result<()> {
            let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
            f.write_all(id.as_bytes())
        };
        match create(&path) {
            Ok(()) => return Ok(me(&path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(LeaseError::Io(e)),
        }
        let holder = std::fs::read_to_string(&path).unwrap_or_default();
        let holder = holder.trim();
        if holder == id {
            return Ok(me(&path));
        }
        if !holder.is_empty() && imp::is_alive(dir, holder) {
            return Err(LeaseError::Held(holder.to_string()));
        }
        // Stale (or unreadable): reclaim.  Two windows can do this at the
        // same instant, so after creating it we wait a moment and check
        // that the file still names us.
        let _ = std::fs::remove_file(&path);
        match create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let other = std::fs::read_to_string(&path).unwrap_or_default();
                return Err(LeaseError::Held(other.trim().to_string()));
            }
            Err(e) => return Err(LeaseError::Io(e)),
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        let now = std::fs::read_to_string(&path).unwrap_or_default();
        if now.trim() == id {
            Ok(me(&path))
        } else {
            Err(LeaseError::Held(now.trim().to_string()))
        }
    }

    /// The live holder of the lease in `dir`, if any.
    pub fn holder(dir: &Path) -> Option<String> {
        let holder = std::fs::read_to_string(dir.join(LEASE_FILE)).ok()?;
        let holder = holder.trim();
        (!holder.is_empty() && imp::is_alive(dir, holder)).then(|| holder.to_string())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mine = std::fs::read_to_string(&self.path).is_ok_and(|s| s.trim() == self.id);
        if mine {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ─── Node: the sockets ─────────────────────────────────────────────────

#[cfg(unix)]
pub use imp::Node;

#[cfg(not(unix))]
pub struct Node;

#[cfg(not(unix))]
impl Node {
    pub fn start(_dir: &Path) -> io::Result<Node> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "window linking needs Unix sockets",
        ))
    }
    pub fn id(&self) -> &str {
        ""
    }
    pub fn dir(&self) -> &Path {
        Path::new("")
    }
    pub fn poll(&self) -> Vec<Msg> {
        Vec::new()
    }
    pub fn publish(&self, _msg: Msg) {}
    pub fn peers(&self) -> usize {
        0
    }
    pub fn scan(&self) -> usize {
        0
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn is_alive(_dir: &std::path::Path, _id: &str) -> bool {
        false
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::time::Duration;

    /// Distinguishes several windows of one process (tests).
    static NEXT: AtomicU32 = AtomicU32::new(0);

    fn sock_path(dir: &Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.sock"))
    }

    /// Does a window with this id still answer on its socket?
    pub fn is_alive(dir: &Path, id: &str) -> bool {
        // Ids come from a file: refuse anything that could leave `dir`.
        id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && UnixStream::connect(sock_path(dir, id)).is_ok()
    }

    enum Out {
        /// Boxed: a `Msg` carries a rule or a basket, `Quit` nothing.
        Msg(Box<Msg>),
        Quit,
    }

    pub struct Node {
        dir: PathBuf,
        id: String,
        sock: PathBuf,
        inbox: Receiver<Msg>,
        out: Sender<Out>,
        peers: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl Node {
        /// Open this window's socket in `dir` and start the helper threads.
        pub fn start(dir: &Path) -> io::Result<Node> {
            std::fs::create_dir_all(dir)?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            let id = format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let sock = sock_path(dir, &id);
            let len = sock.as_os_str().len();
            if len > MAX_SOCKET_PATH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "link socket path is {len} bytes (limit {MAX_SOCKET_PATH}); \
                         set DOS_LINK_DIR to a shorter directory"
                    ),
                ));
            }
            let _ = std::fs::remove_file(&sock);
            let listener = UnixListener::bind(&sock)?;
            std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;

            let (in_tx, inbox) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let peers = Arc::new(AtomicUsize::new(0));

            let stop_a = Arc::clone(&stop);
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop_a.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let tx = in_tx.clone();
                    std::thread::spawn(move || read_conn(conn, tx));
                }
            });

            let (out, out_rx) = mpsc::channel();
            let (dir_w, sock_w, peers_w) = (dir.to_path_buf(), sock.clone(), Arc::clone(&peers));
            std::thread::spawn(move || loop {
                match out_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(Out::Msg(m)) => {
                        if let Ok(mut line) = serde_json::to_vec(&m) {
                            line.push(b'\n');
                            deliver(&dir_w, &sock_w, Some(&line), &peers_w);
                        }
                    }
                    Ok(Out::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => deliver(&dir_w, &sock_w, None, &peers_w),
                }
            });

            Ok(Node {
                dir: dir.to_path_buf(),
                id,
                sock,
                inbox,
                out,
                peers,
                stop,
            })
        }

        pub fn id(&self) -> &str {
            &self.id
        }

        pub fn dir(&self) -> &Path {
            &self.dir
        }

        /// Messages that arrived since the last call.
        pub fn poll(&self) -> Vec<Msg> {
            self.inbox.try_iter().collect()
        }

        /// Queue `msg` for every other window (sent by a worker thread, the
        /// caller never blocks).
        pub fn publish(&self, msg: Msg) {
            let _ = self.out.send(Out::Msg(Box::new(msg)));
        }

        /// Other windows that answered at the last scan (every 2 s, and
        /// after each publish).
        pub fn peers(&self) -> usize {
            self.peers.load(Ordering::Relaxed)
        }

        /// Scan now (on the calling thread) and return how many other windows
        /// are alive: for the moment a window closes and must know whether it
        /// is the last one.
        pub fn scan(&self) -> usize {
            deliver(&self.dir, &self.sock, None, &self.peers);
            self.peers()
        }
    }

    impl Drop for Node {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.out.send(Out::Quit);
            // Wake the accept loop so its thread ends.
            let _ = UnixStream::connect(&self.sock);
            let _ = std::fs::remove_file(&self.sock);
        }
    }

    /// Read lines from one connection; valid messages go to the inbox.
    fn read_conn(conn: UnixStream, tx: Sender<Msg>) {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(conn);
        // At most a few messages per connection; each line capped.
        for _ in 0..16 {
            let mut buf = Vec::new();
            let n = (&mut reader)
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut buf);
            match n {
                Ok(0) | Err(_) => return,
                Ok(_) if buf.len() > MAX_LINE => return,
                Ok(_) => {}
            }
            if let Ok(m) = serde_json::from_slice::<Msg>(&buf) {
                if m.is_valid() {
                    let _ = tx.send(m);
                }
            }
        }
    }

    /// Visit every other window's socket: send `line` if given, otherwise
    /// just ping; refresh the live count; delete sockets nobody answers.
    fn deliver(dir: &Path, own: &Path, line: Option<&[u8]>, peers: &AtomicUsize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut alive = 0;
        for e in entries.flatten() {
            let p = e.path();
            if p == own || p.extension().and_then(|x| x.to_str()) != Some("sock") {
                continue;
            }
            match UnixStream::connect(&p) {
                Ok(mut s) => {
                    alive += 1;
                    if let Some(line) = line {
                        let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
                        let _ = s.write_all(line);
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    let _ = std::fs::remove_file(&p);
                }
                Err(_) => {}
            }
        }
        peers.store(alive, Ordering::Relaxed);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A short, unique directory (socket paths are limited to ~100 bytes).
    fn tmp(tag: &str) -> PathBuf {
        let d = PathBuf::from(format!("/tmp/dl-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn sym(group: u8, s: &str) -> Msg {
        Msg::Symbol {
            group,
            symbol: s.into(),
        }
    }

    #[test]
    fn validation_drops_junk() {
        assert!(valid_symbol("BTCUSDT") && valid_symbol("1000PEPEUSDT"));
        for bad in [
            "",
            "A",
            "btcusdt",
            "BTC/USDT",
            "BTC USDT",
            "../../etc",
            &"A".repeat(21),
        ] {
            assert!(!valid_symbol(bad), "{bad}");
        }
        assert!(sym(1, "BTCUSDT").is_valid());
        assert!(Msg::ListsChanged { from: "1-0".into() }.is_valid());
        assert!(!Msg::ListsChanged {
            from: "../x".into()
        }
        .is_valid());
        assert!(!sym(0, "BTCUSDT").is_valid() && !sym(9, "BTCUSDT").is_valid());
        assert!(valid_timeframe("15s") && !valid_timeframe("2h") && !valid_timeframe(""));
        assert!(
            valid_list_name("Pump 15m") && !valid_list_name("") && !valid_list_name("a\u{1b}b")
        );
        assert!(!valid_list_name(&"x".repeat(25)));
        assert!(valid_id("123-4") && !valid_id("../etc") && !valid_id(""));
        let win = crate::desk::DeskWindow {
            view: "chart".into(),
            link: 1,
            symbol: None,
            tf: None,
            bare: true,
            own_tf: false,
            font: None,
            x: 0,
            y: 0,
            cols: 80,
            rows: 24,
        };
        let report = |win| Msg::DeskReport {
            to: "1-0".into(),
            from: "2-0".into(),
            req: 7,
            win,
        };
        assert!(report(win.clone()).is_valid());
        assert!(!report(crate::desk::DeskWindow {
            view: "x".into(),
            ..win
        })
        .is_valid());
        assert!(Msg::DeskQuery {
            from: "1-0".into(),
            req: 1
        }
        .is_valid());
        assert!(Msg::DeskClose { from: "1-0".into() }.is_valid());
        let fr = |x| crate::desk::Frame {
            x,
            y: 0,
            w: 100,
            h: 100,
        };
        let tile = |desk: &str, moves| Msg::DeskTile {
            from: "1-0".into(),
            desk: desk.into(),
            moves,
        };
        assert!(tile("Day", vec![(fr(0), fr(10))]).is_valid());
        assert!(!tile("../x", vec![]).is_valid());
        assert!(!tile("Day", vec![(fr(0), fr(99_999))]).is_valid());
        assert!(!tile("Day", vec![(fr(0), fr(1)); 17]).is_valid());
        assert!(!Msg::DeskClose {
            from: "../x".into()
        }
        .is_valid());
    }

    #[test]
    fn orders_between_windows_are_checked() {
        let order = |symbol: &str, cmd| Msg::Order {
            from: "1-0".into(),
            req: 1,
            symbol: symbol.into(),
            cmd,
        };
        let market = |qty| OrderCmd::Market { buy: true, qty };
        assert!(order("BTCUSDT", market(0.01)).is_valid());
        assert!(order("BTCUSDT", OrderCmd::Close).is_valid());
        for qty in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e13] {
            assert!(!order("BTCUSDT", market(qty)).is_valid(), "{qty}");
        }
        assert!(!order("btc", market(1.0)).is_valid());
        let bracket = |sl, tp| OrderCmd::Bracket { sl, tp };
        assert!(order("BTCUSDT", bracket(Some(1.0), None)).is_valid());
        assert!(order("BTCUSDT", bracket(None, None)).is_valid());
        assert!(!order("BTCUSDT", bracket(Some(-1.0), None)).is_valid());
        assert!(!order("BTCUSDT", bracket(None, Some(f64::NAN))).is_valid());
        let limit = |qty, price| OrderCmd::Limit {
            buy: false,
            qty,
            price,
        };
        assert!(order("BTCUSDT", limit(0.5, 100.0)).is_valid());
        for (q, p) in [
            (0.0, 100.0),
            (1.0, 0.0),
            (1.0, -5.0),
            (f64::NAN, 1.0),
            (1.0, 1e13),
        ] {
            assert!(!order("BTCUSDT", limit(q, p)).is_valid(), "{q} {p}");
        }
        let cancel = |price, within| OrderCmd::CancelAt { price, within };
        assert!(order("BTCUSDT", cancel(100.0, 0.05)).is_valid());
        assert!(order("BTCUSDT", cancel(100.0, 0.0)).is_valid());
        assert!(!order("BTCUSDT", cancel(0.0, 0.05)).is_valid());
        assert!(!order("BTCUSDT", cancel(100.0, -1.0)).is_valid());
        assert!(!order("BTCUSDT", cancel(100.0, 100.0)).is_valid());
        let done = |text: &str| Msg::OrderDone {
            to: "1-0".into(),
            from: "2-0".into(),
            req: 1,
            text: text.into(),
        };
        assert!(done("BUY filled").is_valid());
        assert!(!done("a\u{1b}[2J").is_valid());
        assert!(!done(&"x".repeat(MAX_ORDER_TEXT + 1)).is_valid());
        // The wire form round-trips.
        let m = order("ETHUSDT", bracket(Some(2.5), None));
        let line = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Msg>(&line).unwrap(), m);
    }

    #[test]
    fn a_message_reaches_the_other_window_and_not_the_sender() {
        let dir = tmp("msg");
        let a = Node::start(&dir).unwrap();
        let b = Node::start(&dir).unwrap();
        a.publish(sym(2, "ETHUSDT"));
        let mut got = Vec::new();
        assert!(wait_for(|| {
            got.extend(b.poll());
            !got.is_empty()
        }));
        assert_eq!(got, vec![sym(2, "ETHUSDT")]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(a.poll().is_empty(), "no echo to the sender");
        assert!(wait_for(|| a.peers() == 1), "the sender counted its peer");
        drop((a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_and_oversized_lines_are_ignored() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let dir = tmp("junk");
        let a = Node::start(&dir).unwrap();
        let path = dir.join(format!("{}.sock", a.id()));
        for payload in [
            b"not json\n".to_vec(),
            b"{\"t\":\"symbol\",\"group\":1,\"symbol\":\"../x\"}\n".to_vec(),
            vec![b'x'; MAX_LINE + 10],
        ] {
            let mut s = UnixStream::connect(&path).unwrap();
            let _ = s.write_all(&payload);
        }
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"{\"t\":\"symbol\",\"group\":1,\"symbol\":\"SOLUSDT\"}\n")
            .unwrap();
        let mut got = Vec::new();
        assert!(wait_for(|| {
            got.extend(a.poll());
            !got.is_empty()
        }));
        assert_eq!(
            got,
            vec![sym(1, "SOLUSDT")],
            "only the valid line gets through"
        );
        drop(a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_window_is_cleaned_up_and_the_socket_removed_on_drop() {
        let dir = tmp("dead");
        let a = Node::start(&dir).unwrap();
        let ghost = dir.join("99999-0.sock");
        drop(std::os::unix::net::UnixListener::bind(&ghost).unwrap());
        assert!(ghost.exists(), "a bound socket file outlives its listener");
        a.publish(sym(1, "BTCUSDT"));
        assert!(
            wait_for(|| !ghost.exists()),
            "stale socket removed by a scan"
        );
        let own = dir.join(format!("{}.sock", a.id()));
        assert!(own.exists());
        drop(a);
        assert!(!own.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linker_debounces_guards_echo_and_filters_groups() {
        use Kind::*;
        let dir = tmp("lnk");
        let mut a = Linker::new(Node::start(&dir).unwrap());
        let mut b = Linker::new(Node::start(&dir).unwrap());
        let eth = [Some("ETHUSDT"), Some("1m"), Some("Main")];
        // Not linked: nothing is sent, however the symbol changes.
        assert!(!a.observe(Symbol, Some("BTCUSDT"), 0));
        assert!(!a.observe(Symbol, Some("ETHUSDT"), 10_000));
        a.set_group(3, eth);
        b.set_group(3, eth);
        assert!(
            !a.observe(Symbol, Some("ETHUSDT"), 20_000),
            "joining sends nothing"
        );
        // The first change on a quiet channel goes out at once.
        assert!(a.observe(Symbol, Some("SOLUSDT"), 30_000));
        // Each message has its own connection: in real use the next one is
        // at least DEBOUNCE_MS later, so let this one land first.
        std::thread::sleep(Duration::from_millis(DEBOUNCE_MS as u64));
        // Within a burst a change must stay for the debounce time.
        let t = 30_000 + 10;
        assert!(!a.observe(Symbol, Some("ADAUSDT"), t));
        assert!(!a.observe(Symbol, Some("ADAUSDT"), t + DEBOUNCE_MS - 11));
        assert!(
            !a.observe(Symbol, Some("XRPUSDT"), t + DEBOUNCE_MS - 1),
            "a new value restarts the clock"
        );
        assert!(!a.observe(Symbol, Some("XRPUSDT"), t + 2 * DEBOUNCE_MS - 2));
        assert!(a.observe(Symbol, Some("XRPUSDT"), t + 2 * DEBOUNCE_MS - 1));
        // Timeframe and list are separate channels: quiet, so at once.
        assert!(a.observe(Timeframe, Some("5m"), 40_000));
        assert!(a.observe(List, Some("Pump 15m"), 40_000));
        let mut got = Inbox::default();
        // SOLUSDT went out at once, XRPUSDT later: keep the newest of each.
        assert!(wait_for(|| {
            let i = b.poll();
            got.symbol = i.symbol.or(got.symbol.take());
            got.timeframe = i.timeframe.or(got.timeframe.take());
            got.list = i.list.or(got.list.take());
            got.symbol.as_deref() == Some("XRPUSDT")
                && got.timeframe.is_some()
                && got.list.is_some()
        }));
        assert_eq!(got.symbol.as_deref(), Some("XRPUSDT"));
        assert_eq!(got.timeframe.as_deref(), Some("5m"));
        assert_eq!(got.list.as_deref(), Some("Pump 15m"));
        // The receiver applies it; a view that cannot show it must not echo.
        b.adopted(Symbol, Some("ETHUSDT"));
        assert!(!b.observe(Symbol, Some("ETHUSDT"), 50_000));
        assert!(!b.observe(Symbol, Some("ETHUSDT"), 60_000));
        // Other groups are not heard.
        b.set_group(4, eth);
        a.node().publish(sym(3, "ADAUSDT"));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(b.poll(), Inbox::default());
        drop((a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trade_messages_pass_without_a_group_and_junk_ids_are_dropped() {
        let dir = tmp("trade");
        let a = Linker::new(Node::start(&dir).unwrap());
        let mut b = Linker::new(Node::start(&dir).unwrap());
        let ask = Msg::TradeHandoff {
            to: "1-0".into(),
            from: b.node().id().into(),
        };
        assert!(ask.is_valid());
        a.send(ask.clone());
        a.send(Msg::TradeDenied {
            to: "../x".into(),
            from: "1-1".into(),
        });
        let mut got = Vec::new();
        assert!(wait_for(|| {
            got.extend(b.poll().trade);
            !got.is_empty()
        }));
        std::thread::sleep(Duration::from_millis(100));
        got.extend(b.poll().trade);
        assert_eq!(
            got,
            vec![ask],
            "the message with a path-like id never arrives"
        );
        drop((a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lease_has_one_live_holder_and_stale_ones_are_reclaimed() {
        let dir = tmp("lease");
        let a = Node::start(&dir).unwrap();
        let b = Node::start(&dir).unwrap();
        let lease = Lease::acquire(&dir, a.id()).unwrap();
        assert_eq!(Lease::holder(&dir).as_deref(), Some(a.id()));
        match Lease::acquire(&dir, b.id()) {
            Err(LeaseError::Held(h)) => assert_eq!(h, a.id()),
            other => panic!("second window must be refused: {:?}", other.map(|_| ())),
        }
        // Asking again as the holder is fine.
        let again = Lease::acquire(&dir, a.id()).unwrap();
        drop(again);
        drop(lease);
        assert_eq!(Lease::holder(&dir), None, "released on drop");
        let lease_b = Lease::acquire(&dir, b.id()).unwrap();
        drop(lease_b);
        // A holder whose window is gone: stale, taken over.
        std::fs::write(dir.join(LEASE_FILE), "424242-7").unwrap();
        assert_eq!(Lease::holder(&dir), None);
        let l = Lease::acquire(&dir, a.id()).unwrap();
        assert_eq!(Lease::holder(&dir).as_deref(), Some(a.id()));
        drop((l, a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_forty_leg_basket_fits_one_socket_line_and_bad_ones_are_dropped() {
        let legs: Vec<(String, bool, f64)> = (0..40)
            .map(|i| (format!("S{i}LONGNAMEUSDT"), i % 2 == 0, 5.25))
            .collect();
        let req = BasketReq {
            name: "group of 40 / BTC".into(),
            origin: "GROUP 8 coins (1h)".into(),
            fixed: legs.iter().take(4).map(|l| (l.0.clone(), 1.5)).collect(),
            prices: legs.iter().map(|l| (l.0.clone(), 12345.6789)).collect(),
            legs,
            max_minutes: Some(240),
        };
        let m = Msg::Basket {
            from: "1-1".into(),
            req: 7,
            basket: req.clone(),
        };
        assert!(m.is_valid());
        let line = serde_json::to_string(&m).unwrap();
        assert!(line.len() < MAX_LINE, "{} bytes", line.len());
        let back: Msg = serde_json::from_str(&line).unwrap();
        assert_eq!(back, m);
        let mut bad = req.clone();
        bad.legs[3].2 = f64::NAN;
        assert!(!bad.is_valid());
        bad = req.clone();
        bad.legs[0].0 = "btc usdt".into();
        assert!(!bad.is_valid());
        bad = req.clone();
        bad.legs.extend(bad.legs.clone());
        assert!(!bad.is_valid(), "81 legs");
        bad = req;
        bad.name = "x\u{1b}[31m".into();
        assert!(!bad.is_valid());
    }
}
