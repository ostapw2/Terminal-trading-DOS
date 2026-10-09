//! `dos-commander` — terminal trading dashboard, Rust runtime.

#![allow(clippy::too_many_arguments)]

use dos::widgets::fmt::rect_contains;
use std::{
    collections::HashSet,
    io,
    io::Write,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    Terminal,
};
use rusqlite::Connection;

use dos::{
    data::{binance, db, yahoo},
    engine::{broker::Broker, current_now_ms, Runtime, SlotStatus},
    markets::{market_symbols, MarketType, Ohlc, Symbol, Timeframe},
    strategies::{self, registry as strategy_registry, Action as StrategyAction},
    tokens::CLASSIC,
    widgets::{
        chart::HistoryRange,
        dashboard,
        function_bar::{self, FKey},
        markets::{self, MarketsLayout},
        menu_bar::{self, MenuEntry},
        screener::{self, ScreenerLayout},
        settings::{self, SettingsLayout, SettingsState, MARKETS as SETTINGS_MARKETS},
        status_line::{self},
        strategy_panel::{self, StrategyPanelLayout},
        trade_ticket::{self, SizeMode, TicketAction, TradeTicketLayout},
    },
};

/// Longest an idle screen goes without being redrawn.
const IDLE_REDRAW: Duration = Duration::from_millis(50);

const F_KEYS: &[FKey] = &[
    FKey {
        number: 1,
        label: "Help",
    },
    FKey {
        number: 3,
        label: "Markets",
    },
    FKey {
        number: 4,
        label: "Screener",
    },
    FKey {
        number: 5,
        label: "Dash",
    },
    FKey {
        number: 6,
        label: "Book",
    },
    FKey {
        number: 9,
        label: "Setup",
    },
    FKey {
        number: 10,
        label: "Quit",
    },
];

const MENU_MARKETS: &[MenuEntry] = &[
    MenuEntry {
        title: "Symbol",
        hotkey: 'S',
    },
    MenuEntry {
        title: "Chart",
        hotkey: 'C',
    },
    MenuEntry {
        title: "Timeframe",
        hotkey: 'T',
    },
    MenuEntry {
        title: "History",
        hotkey: 'H',
    },
    MenuEntry {
        title: "Panes",
        hotkey: 'P',
    },
];

const MENU_SCREENER: &[MenuEntry] = &[
    MenuEntry {
        title: "Sort",
        hotkey: 'S',
    },
    MenuEntry {
        title: "Filter",
        hotkey: 'F',
    },
    MenuEntry {
        title: "Open",
        hotkey: 'O',
    },
];

const MENU_DASHBOARD: &[MenuEntry] = &[MenuEntry {
    title: "Refresh",
    hotkey: 'R',
}];

const MENU_SETTINGS: &[MenuEntry] = &[
    MenuEntry {
        title: "Market",
        hotkey: 'M',
    },
    MenuEntry {
        title: "API",
        hotkey: 'A',
    },
    MenuEntry {
        title: "Cache",
        hotkey: 'C',
    },
];

const MENU_ORDERBOOK: &[MenuEntry] = &[
    MenuEntry {
        title: "Symbol",
        hotkey: 'S',
    },
    MenuEntry {
        title: "Zoom",
        hotkey: 'Z',
    },
    MenuEntry {
        title: "Reconnect",
        hotkey: 'R',
    },
];

fn menu_for(view: View) -> &'static [MenuEntry] {
    match view {
        View::Markets => MENU_MARKETS,
        View::Screener => MENU_SCREENER,
        View::Dashboard => MENU_DASHBOARD,
        View::OrderBook => MENU_ORDERBOOK,
        View::Settings => MENU_SETTINGS,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ModalKind {
    TickerInput,
    MenuDropdown, // top-menu dropdown popup
    DatePickerFrom,
    DatePickerTo,
    SessionTime,
    LiveConfirm,
    Help,
    CommandPalette,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Markets,
    Screener,
    Dashboard,
    OrderBook,
    Settings,
}

struct App {
    view: View,
    status: String,
    quit: bool,
    modal: Option<ModalKind>,
    symbols: Vec<Symbol>,
    sym_idx: usize,
    chart_zoom: u16,
    chart_aggregate: u16, // 1 = no aggregation; >1 means N candles → 1 bar
    chart_type: dos::widgets::chart::ChartType,
    pane_count: u8,
    pane_active: u8,
    pane_states: Vec<PaneState>, // exactly pane_count entries; each fully independent
    panes_linked: bool,
    menu_open: Option<usize>,
    menu_item_sel: usize,
    screener_sel: usize,
    screener_sort: screener::SortMode,
    screener_sort_asc: bool,
    screener_live: bool,
    screener_venue: dos::data::binance::Venue,
    screener_min_qv: f64,
    live_pairs: Vec<dos::data::binance::TickerSummary>,
    pending_live: Option<Receiver<Result<Vec<dos::data::binance::TickerSummary>, String>>>,
    last_live_refresh: Instant,
    last_footprint_refresh: Instant,
    /// F6 OrderBook view state.
    ob_ticker: String,
    ob_snapshot: Option<dos::data::orderbook::OrderBookSnapshot>,
    /// Local copy of the FULL book, maintained from the diff stream.
    /// `ob_snapshot` is regenerated from this each frame (top-N only).
    ob_local_book: Option<dos::data::orderbook::LocalBook>,
    /// Pending REST depth snapshot fetch (one-shot, used to seed
    /// `ob_local_book` so the diff stream can be applied).
    ob_pending_seed:
        Option<Receiver<Result<(dos::data::orderbook::OrderBookSnapshot, u64), String>>>,
    /// Buffer of diff events received before the REST seed lands.
    ob_diff_buffer: Vec<dos::data::orderbook::DepthDiffEvent>,
    /// A re-seed is wanted but waiting for its backoff slot (audit A11).
    ob_seed_wanted: bool,
    /// Background fetch of the startup universe: (index, ticker, result).
    startup_rx: Option<Receiver<StartupMsg>>,
    startup_market: MarketType,
    startup_total: usize,
    startup_done: usize,
    startup_ok: usize,
    /// Risk limits as last written to the DB (written only on change).
    last_limits_json: Option<String>,
    /// Focused button of the live-trading confirm modal (0 = Yes, 1 = No).
    live_confirm_focus: usize,
    /// `runtime.live_error_seq` already shown in the status line.
    live_error_shown: u64,
    /// When the diff stream was opened: the first seed waits for the first
    /// buffered event (Binance's recommended order) or a short timeout.
    ob_ws_opened_at: Instant,
    ob_next_seed_at: Instant,
    ob_seed_delay_secs: u64,
    /// When the current book last became contiguous (resets the backoff).
    ob_synced_at: Instant,
    /// Symbol for which the Spot↔Futures flip was already tried.
    ob_flipped_for: Option<String>,
    /// Live diff stream — fires every 100 ms.
    ob_diff_ws: Option<dos::data::binance_ws::DepthDiffHandle>,
    ob_trade_ws: Option<dos::data::binance_ws::WsHandle>,
    /// Combined-stream `<symbol>@bookTicker` WS handle.  Spun up on
    /// demand for every selected symbol.  Reconnects when the symbol
    /// set or venue changes.  None = no subscription.
    mini_ticker_ws: Option<dos::data::binance_ws::MiniTickerHandle>,
    /// Symbols the active WS is subscribed to (sorted, used for
    /// reconnect-on-change comparison).
    mini_ticker_symbols: Vec<String>,
    /// Venue the active WS is bound to.  Reconnect on Spot↔Futures.
    mini_ticker_futures: bool,
    /// Whether the WS thread reports an open connection.  Surfaced in
    /// the F4/F5 title strip via `[WS]` / `[ws...]` indicator.
    mini_ticker_connected: bool,
    /// Last "Tick" we received.  Used by the freshness indicator.
    last_mini_ticker_at: Instant,
    /// `!bookTicker` WS for the Screener — pushes bid/ask mid-prices for
    /// every USDT pair in real time (sub-second updates on all screener
    /// symbols, unlike !miniTicker@arr which only covers ~80/batch).
    mini_ticker_arr_ws: Option<dos::data::binance_ws::MiniTickerHandle>,
    /// Venue the screener WS is bound to (false=Spot, true=Futures).
    /// Used to reconnect when the user flips venue via `v`.
    mini_ticker_arr_futures: bool,
    /// Whether the bookTicker WS reports an open connection.
    mini_ticker_arr_connected: bool,
    /// Last Tick from the screener `!bookTicker` WS.  Used by the stale
    /// connection watchdog (distinct from `last_mini_ticker_at` which is
    /// updated by the per-symbol WS).
    last_screener_tick_at: Instant,
    ob_trades: std::collections::VecDeque<dos::data::binance::AggTrade>,
    ob_ws_symbol: String,
    ob_venue: dos::data::binance::Venue,
    ob_zoom: u32,
    ob_last_trade_price: Option<f64>,
    ob_last_trade_at: Instant,
    /// Last time we received a fresh depth snapshot from the polling
    /// thread.  Used for the "stalled" warning when no data arrives.
    ob_last_snapshot_at: Instant,
    /// Set true the moment a fresh `live_pairs` batch lands; cleared after
    /// the splice runs once.  Prevents the splice from running on every
    /// loop iteration.
    live_pairs_dirty: bool,
    /// Sticky drag state: while Some(pane_idx), every Drag(Left) event
    /// updates that pane's chart_offset based on x position (clamped to
    /// the scrollbar bounds).  Cleared on Up(Left).  Lets the user keep
    /// dragging even if cursor wanders off the thin scrollbar row.
    dragging_scrollbar: Option<usize>,
    ticker_input: dos::widgets::input::InputState,
    command_input: dos::widgets::input::InputState,
    help_page: usize,
    /// Calendar state used by both DatePickerFrom and DatePickerTo
    /// modals.  Re-initialised from the pane's current value each time a
    /// modal opens.
    calendar: dos::widgets::calendar::CalendarState,
    /// Session-window picker draft.  Edits live here until the user
    /// hits Apply (then committed back to the active pane).
    session_picker: dos::widgets::session_picker::SessionPickerState,
    market: MarketType,
    timeframe: Timeframe,
    binance_key: String,
    binance_secret: String,
    binance_futures_key: String,
    binance_futures_secret: String,
    /// Spot / Futures keys came from `DOS_BINANCE_*` env vars: never written
    /// to the database, never cleared by `:keys clear`.
    env_spot: Option<(String, String)>,
    env_futures: Option<(String, String)>,
    /// Result of the last `:keys check` for the CURRENT spot key.
    key_check: Option<dos::engine::KeyPermissions>,
    pending_key_check: Option<Receiver<Result<dos::engine::KeyPermissions, String>>>,
    /// `:live` was asked while the key was unchecked: open the confirm modal
    /// when the check comes back clean.
    live_after_check: bool,
    /// Show the live trades feed in the F6 Order Book view.
    ob_show_trades: bool,
    /// History range display filter — overrides any per-pane date_from/date_to
    /// range while set to Today or Last3_5Days, regardless of timeframe.
    chart_history_range: HistoryRange,
    /// Preferred terminal font size in points.  Sent as an OSC 50 hint on
    /// startup and after each Apply; not all terminals honor it (xterm/urxvt
    /// do, iTerm2/Terminal.app/kitty don't).
    font_size: i32,
    /// How many candles to request per fetch (200 / 500 / 1000).  Affects
    /// scroll-back history depth.
    chart_history: u32,
    settings: SettingsState,
    mouse: Option<(u16, u16)>,
    db: Option<Connection>,
    pending_fetches: Vec<PendingFetch>,
    pending_footprints: Vec<PendingFootprint>,
    last_persist: Instant,
    /// What the last paper snapshot wrote — a snapshot is only written when
    /// the serialized ledger actually changed.
    last_persisted: Option<(String, String)>,
    /// Why the database could not be opened (nothing is saved while set).
    db_error: Option<String>,
    /// Armed by the first "Reset paper ledger"; a second one before this
    /// instant confirms it.
    reset_confirm_until: Option<Instant>,
    /// Last `/api/v3/account` + `/api/v3/openOrders` reconciliation.
    /// Only meaningful in live mode; idle in paper mode.
    last_live_sync: Instant,
    /// Last listen-key keepalive (Binance expires after 60 min; refresh every 20).
    last_listen_key_keepalive: Instant,
    /// Press-flash for action buttons.  When a button is mouse-clicked we
    /// record (kind, idx, when); render functions check `at.elapsed() <
    /// PRESS_FLASH_MS` and paint `button::State::Pressed`.  Cleared on
    /// expiry — natural redraw cadence (16ms poll, 250ms tick) makes the
    /// flash visible without an explicit timer.
    last_press: Option<(PressedBtnKind, usize, Instant)>,
    /// Trading engine: paper broker + risk gate + live strategy slots.
    runtime: Runtime,
    /// Mirror of `runtime.slots[*].symbol` for fast O(1) screener checkbox
    /// rendering.  Kept in sync by `add_slot` / `remove_slot` helpers.
    selected_set: HashSet<String>,
    /// Highlight target in the selection list (for visual focus only —
    /// the trade ticket is basket-mode and ignores this).
    active_slot: Option<usize>,
    /// Per-pair order qty when `size_mode = Qty`.  Bumped via the
    /// `[-]`/`[+]` stepper or `+`/`-` hot-keys on F4.
    ticket_qty: f64,
    /// Per-pair USD notional when `size_mode = Usd`.  Engine divides
    /// this by each pair's last mark to get coins on submit.
    ticket_notional: f64,
    /// Whether the ticket value field reads as raw qty or as USD.
    size_mode: SizeMode,
    /// Position-size multiplier on submit.  Paper broker doesn't
    /// model margin / liquidation — leverage just scales effective qty.
    leverage: f64,
    /// Last ticket button press for the press-flash visual feedback.
    last_ticket_press: Option<(TicketAction, Instant)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PressedBtnKind {
    Settings,
}

const PRESS_FLASH_MS: u128 = 80;

fn pressed_for(app: &App, kind: PressedBtnKind) -> Option<usize> {
    match app.last_press {
        Some((k, idx, at)) if k == kind && at.elapsed().as_millis() < PRESS_FLASH_MS => Some(idx),
        _ => None,
    }
}

/// One chart pane.  When multiple panes are open they're each fully
/// independent: own ticker, own timeframe, own OHLC buffer.  In linked mode
/// the active pane's ticker is propagated to every pane on each ticker
/// change (and each pane refetches at its own TF).
struct PaneState {
    sym_idx: usize,
    ticker: String,
    name: String,
    data: Vec<Ohlc>,
    timeframe: Timeframe,
    chart_type: dos::widgets::chart::ChartType,
    chart_zoom: u16,
    chart_aggregate: u16,
    /// Number of candles scrolled into the past (0 = latest at the right).
    chart_offset: usize,
    /// Inclusive YYYY-MM-DD lower bound on visible candles.  None = no lower bound.
    date_from: Option<String>,
    /// Inclusive YYYY-MM-DD upper bound on visible candles.  None = no upper bound.
    date_to: Option<String>,
    /// Minute-of-day session window.  ALL = filter off.  Active filter
    /// drops candles outside the window AND inserts visible session
    /// gaps between days.
    session: dos::markets::SessionWindow,
    /// Last-rendered post-filter+aggregate length.  Updated each frame
    /// by the markets renderer so keyboard scroll handlers can clamp
    /// `chart_offset` against the true visible series rather than the
    /// raw `data` length.
    visible_total: usize,
    footprint: Vec<dos::data::footprint::FootprintCandle>,
    tick_size: f64,
    /// Rolling buffer of recent aggTrades from the WebSocket stream + the
    /// initial REST seed.  Capped at ~5000 entries; oldest dropped on
    /// overflow.  Footprint rebuilt from this buffer every ~250ms.
    ws_trades: Vec<dos::data::binance::AggTrade>,
    /// Active WS connection for this pane's ticker.  None when chart_type
    /// != DeltaCluster or when ticker is empty.
    ws: Option<dos::data::binance_ws::WsHandle>,
    /// Symbol the WS connection is currently subscribed to.
    ws_symbol: String,
}

impl Clone for PaneState {
    fn clone(&self) -> Self {
        // WS handle is NOT cloned — each pane gets its own connection
        // when needed via `ensure_ws_for_pane`.
        PaneState {
            sym_idx: self.sym_idx,
            ticker: self.ticker.clone(),
            name: self.name.clone(),
            data: self.data.clone(),
            timeframe: self.timeframe,
            chart_type: self.chart_type,
            chart_zoom: self.chart_zoom,
            chart_aggregate: self.chart_aggregate,
            chart_offset: self.chart_offset,
            date_from: self.date_from.clone(),
            date_to: self.date_to.clone(),
            session: self.session,
            visible_total: self.visible_total,
            footprint: self.footprint.clone(),
            tick_size: self.tick_size,
            ws_trades: self.ws_trades.clone(),
            ws: None,
            ws_symbol: String::new(),
        }
    }
}

/// A klines request, identified by WHAT it asked for (ticker + timeframe),
/// not just by pane index: the pane may show something else by the time the
/// reply lands (audit A4).
struct PendingFetch {
    pane_idx: usize,
    ticker: String,
    tf: Timeframe,
    venue: dos::data::binance::Venue,
    rx: Receiver<Result<Vec<Ohlc>, String>>,
}

struct PendingFootprint {
    pane_idx: usize,
    ticker: String,
    tf: Timeframe,
    rx: Receiver<Result<(Vec<dos::data::binance::AggTrade>, f64), String>>,
}

#[derive(Clone, Copy, Default)]
struct Layouts {
    menu: Rect,
    panels: Rect,
    sep: Rect,
    status: Rect,
    fbar: Rect,
}

/// `--demo` / `DOS_DEMO=1`: offline synthetic data, in-memory database.
fn demo_requested() -> bool {
    std::env::args().skip(1).any(|a| a == "--demo")
        || std::env::var("DOS_DEMO").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn maybe_print_help_and_exit() {
    for a in std::env::args().skip(1) {
        if a == "-V" || a == "--version" {
            println!("dos-commander {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        if a == "-h" || a == "--help" {
            eprintln!(
                "dos-commander — terminal trading dashboard\n\
                 \n\
                 Usage:\n\
                    dos-commander        fetch OHLCV from Binance / Yahoo at startup, then run\n\
                    dos-commander --demo offline demo: synthetic data, nothing saved, no keys\n\
                    dos-commander --version print the version\n\
                    dos-commander --help show this help\n\
                 \n\
                 Environment:\n\
                    DOS_DB=<path>        SQLite file (default ~/.dos/data.db)\n\
                 \n\
                 Inside the app:\n\
                   F1 Help  F3 Markets  F4 Screener  F5 Dashboard  F6 Order Book\n\
                   F9 Settings   F10 Quit   :command palette\n\
                   In Markets view: <-/-> switch symbol, +/- zoom, c chart type,\n\
                   f timeframe, r refresh, [ / ] remove / add pane"
            );
            std::process::exit(0);
        }
    }
}

fn main() -> io::Result<()> {
    maybe_print_help_and_exit();
    let demo = demo_requested();
    if demo {
        dos::data::demo::enable();
    }

    // Open SQLite first so we can hydrate user-saved market + timeframe.
    // Demo: a throw-away in-memory database, so no saved keys or ledger can
    // leak into it (or out of it).
    let (db, db_error) = match if demo { db::open_memory() } else { db::open() } {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let initial_market = db
        .as_ref()
        .and_then(|c| db::load_kv(c, "market"))
        .and_then(|s| match s.as_str() {
            "US Stocks" => Some(MarketType::UsStocks),
            "Crypto" => Some(MarketType::Crypto),
            "EU Stocks" => Some(MarketType::EuStocks),
            "Forex" => Some(MarketType::Forex),
            "Commodities" => Some(MarketType::Commodities),
            _ => None,
        })
        .unwrap_or(MarketType::Crypto);
    let initial_tf = db
        .as_ref()
        .and_then(|c| db::load_kv(c, "timeframe"))
        .and_then(|s| match s.as_str() {
            "15s" => Some(Timeframe::S15),
            "1m" => Some(Timeframe::M1),
            "5m" => Some(Timeframe::M5),
            "1h" => Some(Timeframe::H1),
            "1d" => Some(Timeframe::D1),
            _ => None,
        })
        .unwrap_or(Timeframe::D1);
    let symbols = market_symbols(initial_market);
    // The universe is fetched in the BACKGROUND once the UI is up (the first
    // pane comes from the OHLC cache / its own fetch); the user never stares at
    // a blank terminal while ten sequential HTTP calls finish (audit stage 7).
    let mut startup_status = format!(
        "Fetching 0/{} {} via {}...",
        symbols.len(),
        initial_market.name(),
        if initial_market == MarketType::Crypto {
            "Binance"
        } else {
            "Yahoo"
        },
    );
    let startup_total = symbols.len();
    let startup_rx = spawn_startup_fetch(
        symbols.iter().map(|s| s.ticker).collect(),
        initial_market,
        initial_tf,
    );

    // Keys: DOS_BINANCE_API_KEY / _SECRET (and DOS_BINANCE_FUTURES_API_KEY /
    // _SECRET) win over the database and are never written to it.  Demo mode
    // reads no key from anywhere.
    let env_pair = |k: &str, s: &str| -> Option<(String, String)> {
        if demo {
            return None;
        }
        let (k, s) = (std::env::var(k).ok()?, std::env::var(s).ok()?);
        let (k, s) = (k.trim().to_string(), s.trim().to_string());
        (!k.is_empty() && !s.is_empty()).then_some((k, s))
    };
    let db_kv = |name: &str| {
        db.as_ref()
            .and_then(|c| db::load_kv(c, name))
            .unwrap_or_default()
    };
    let env_spot = env_pair("DOS_BINANCE_API_KEY", "DOS_BINANCE_API_SECRET");
    let env_futures = env_pair(
        "DOS_BINANCE_FUTURES_API_KEY",
        "DOS_BINANCE_FUTURES_API_SECRET",
    );
    let (env_spot_keep, env_futures_keep) = (env_spot.clone(), env_futures.clone());
    let (binance_key, binance_secret) =
        env_spot.unwrap_or_else(|| (db_kv("binance_api_key"), db_kv("binance_api_secret")));
    let (binance_futures_key, binance_futures_secret) = env_futures.unwrap_or_else(|| {
        (
            db_kv("binance_futures_api_key"),
            db_kv("binance_futures_api_secret"),
        )
    });
    let ob_show_trades = db
        .as_ref()
        .and_then(|c| db::load_kv(c, "ob_show_trades"))
        .map(|s| s != "false")
        .unwrap_or(true);
    let chart_history_range = db
        .as_ref()
        .and_then(|c| db::load_kv(c, "chart_history_range"))
        .map(|s| match s.as_str() {
            "today" => HistoryRange::Today,
            "last3_5" => HistoryRange::Last3_5Days,
            _ => HistoryRange::All,
        })
        .unwrap_or(HistoryRange::All);
    let font_size = db
        .as_ref()
        .and_then(|c| db::load_kv(c, "font_size"))
        .and_then(|s| s.parse::<i32>().ok())
        .map(|n| n.clamp(settings::FONT_SIZE_MIN, settings::FONT_SIZE_MAX))
        .unwrap_or(settings::FONT_SIZE_DEFAULT);
    if demo {
        startup_status = "DEMO DATA: synthetic and offline, nothing is saved".into();
    } else if db.is_some() {
        let path = db::db_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?".into());
        startup_status = format!("{}  |  DB: {}", startup_status, path);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Add motion-no-button tracking (DECSET 1003) on top of crossterm's
    // 1000+1002+1006 — gives a hover cursor that follows the mouse without
    // requiring a click.  DECRST 1003 on shutdown.
    stdout.write_all(b"\x1B[?1003h")?;
    // Apply the saved font-size preference (best-effort OSC 50 hint —
    // xterm/urxvt honor it; most modern emulators ignore silently).
    if font_size != settings::FONT_SIZE_DEFAULT {
        let _ =
            stdout.write_all(format!("\x1B]50;xft:Monospace:size={}\x07", font_size).as_bytes());
    }
    stdout.flush()?;

    // Panic hook: if anything panics deep in the render path, restore the
    // terminal first so escape sequences don't leak into the user's shell.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1B[?1003l");
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        let _ = disable_raw_mode();
        let _ = out.flush();
        prev_hook(info);
    }));
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let initial_pane = PaneState {
        sym_idx: 0,
        ticker: symbols[0].ticker.to_string(),
        name: symbols[0].name.to_string(),
        data: symbols[0].data.clone(),
        timeframe: initial_tf,
        chart_type: dos::widgets::chart::ChartType::Candle,
        chart_zoom: 1,
        chart_aggregate: 1,
        chart_offset: 0,
        date_from: None,
        date_to: None,
        session: dos::markets::SessionWindow::ALL,
        visible_total: 0,
        footprint: Vec::new(),
        tick_size: 0.01,
        ws_trades: Vec::new(),
        ws: None,
        ws_symbol: String::new(),
    };
    let mut app = App {
        view: View::Screener,
        status: startup_status,
        quit: false,
        modal: None,
        symbols,
        sym_idx: 0,
        chart_zoom: 1,
        chart_aggregate: 1,
        chart_type: dos::widgets::chart::ChartType::Candle,
        pane_count: 1,
        pane_active: 0,
        pane_states: vec![initial_pane],
        panes_linked: true,
        menu_open: None,
        menu_item_sel: 0,
        screener_sel: 0,
        screener_live: true, // live Binance pairs by default — independent of F9 market
        market: initial_market,
        timeframe: initial_tf,
        binance_key: binance_key.clone(),
        binance_secret: binance_secret.clone(),
        binance_futures_key: binance_futures_key.clone(),
        binance_futures_secret: binance_futures_secret.clone(),
        env_spot: env_spot_keep,
        env_futures: env_futures_keep,
        key_check: None,
        pending_key_check: None,
        live_after_check: false,
        ob_show_trades,
        chart_history_range,
        font_size,
        chart_history: db
            .as_ref()
            .and_then(|c| db::load_kv(c, "chart_history"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(200),
        settings: SettingsState::new_full(
            initial_market,
            initial_tf,
            &binance_key,
            &binance_secret,
            &binance_futures_key,
            &binance_futures_secret,
            ob_show_trades,
            chart_history_range,
            font_size,
            false, // live_enabled starts OFF (env-gated at startup)
        ),
        screener_sort: screener::SortMode::Gap,
        screener_sort_asc: false,
        screener_venue: dos::data::binance::Venue::Spot,
        screener_min_qv: 50_000.0,
        live_pairs: Vec::new(),
        pending_live: None,
        last_live_refresh: Instant::now() - Duration::from_secs(60),
        last_footprint_refresh: Instant::now() - Duration::from_secs(60),
        live_pairs_dirty: false,
        dragging_scrollbar: None,
        ob_ticker: String::new(),
        ob_snapshot: None,
        ob_local_book: None,
        ob_pending_seed: None,
        ob_diff_buffer: Vec::new(),
        ob_seed_wanted: false,
        startup_total,
        startup_rx: Some(startup_rx),
        startup_market: initial_market,
        startup_done: 0,
        startup_ok: 0,
        last_limits_json: None,
        live_confirm_focus: 1,
        live_error_shown: 0,
        ob_ws_opened_at: Instant::now(),
        ob_next_seed_at: Instant::now(),
        ob_seed_delay_secs: 1,
        ob_synced_at: Instant::now(),
        ob_flipped_for: None,
        ob_diff_ws: None,
        ob_trade_ws: None,
        mini_ticker_ws: None,
        mini_ticker_symbols: Vec::new(),
        mini_ticker_futures: false,
        mini_ticker_connected: false,
        last_mini_ticker_at: Instant::now() - Duration::from_secs(60),
        mini_ticker_arr_ws: None,
        mini_ticker_arr_futures: false,
        mini_ticker_arr_connected: false,
        last_screener_tick_at: Instant::now() - Duration::from_secs(60),
        ob_trades: std::collections::VecDeque::with_capacity(50),
        ob_ws_symbol: String::new(),
        ob_venue: dos::data::binance::Venue::Spot,
        ob_zoom: 1,
        ob_last_trade_price: None,
        ob_last_trade_at: Instant::now() - Duration::from_secs(10),
        ob_last_snapshot_at: Instant::now() - Duration::from_secs(60),
        ticker_input: dos::widgets::input::InputState::new("BTC, AAPL, EURUSD..."),
        command_input: dos::widgets::input::InputState::new(""),
        help_page: 0,
        calendar: dos::widgets::calendar::CalendarState::from_date(None),
        session_picker: dos::widgets::session_picker::SessionPickerState::from_window(
            dos::markets::SessionWindow::ALL,
        ),
        mouse: None,
        db,
        pending_fetches: Vec::new(),
        pending_footprints: Vec::new(),
        last_persist: Instant::now(),
        last_persisted: None,
        db_error,
        reset_confirm_until: None,
        last_live_sync: Instant::now() - Duration::from_secs(60),
        last_listen_key_keepalive: Instant::now(),
        last_press: None,
        runtime: Runtime::new(dos::engine::PAPER_STARTING_CASH),
        selected_set: HashSet::new(),
        active_slot: None,
        ticket_qty: 0.1,
        ticket_notional: 5.0,
        size_mode: SizeMode::Usd,
        leverage: 1.0,
        last_ticket_press: None,
    };
    // Hydrate persisted screener selections + slot params from disk,
    // then restore the paper broker (positions + open limits) so the
    // user picks up exactly where they left off.
    if let Some(c) = app.db.as_ref() {
        hydrate_selection_from_db(&mut app.runtime, &mut app.selected_set, c);
        hydrate_paper_state(&mut app.runtime, c);
        if let Some(l) = db::load_kv(c, "risk_limits")
            .and_then(|j| serde_json::from_str::<dos::engine::RiskLimits>(&j).ok())
        {
            app.runtime.limits = l;
        }
    }
    if !app.runtime.slots.is_empty() {
        app.active_slot = Some(0);
    }

    // First paint: the cached series (if any) shows at once, the fetch follows.
    refresh_pane(&mut app, 0);

    // DOS_LIVE stays blocked until a testnet run has been done by hand
    // (TODO.md, stage 4); it is read only to tell the user why it is ignored.
    let live_requested = std::env::var("DOS_LIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if demo {
        app.status = "DEMO DATA: synthetic and offline | PAPER | nothing is saved".into();
    } else if live_requested {
        app.status = "live via DOS_LIVE is blocked until a testnet run (TODO.md) | PAPER".into();
    } else {
        let n_pos = app
            .runtime
            .broker
            .all_positions()
            .values()
            .filter(|p| !p.is_flat())
            .count();
        let n_ord = app.runtime.broker.open_order_count();
        if n_pos > 0 || n_ord > 0 {
            app.status = format!(
                "PAPER | restored {} pos / {} open orders from local DB",
                n_pos, n_ord
            );
        } else {
            app.status = "PAPER | orders simulated locally, no Binance traffic".into();
        }
    }

    if let Some(e) = &app.db_error {
        app.status = format!("DB UNAVAILABLE ({e}) - settings, ledger and slots are NOT saved");
    }

    let result = run(&mut terminal, &mut app);

    // Final snapshot before exit — save paper broker state so the next
    // launch reads the same positions back.
    if let Some(c) = app.db.as_ref() {
        snapshot_paper_state(&app.runtime, c, &mut app.last_persisted);
    }
    persist_risk_limits(&mut app);

    let _ = terminal.backend_mut().write_all(b"\x1B[?1003l");
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    let palette = &CLASSIC;
    let mut layouts = Layouts::default();
    let mut last_markets_layout: Option<MarketsLayout> = None;
    let mut last_screener_layout: Option<ScreenerLayout> = None;
    let mut last_strategy_panel: Option<StrategyPanelLayout> = None;
    let mut last_trade_ticket: Option<TradeTicketLayout> = None;
    let mut last_settings_layout: Option<SettingsLayout> = None;
    let mut last_menu_dropdown_layout: Option<MenuDropdownLayout> = None;
    let mut last_calendar_layout: Option<dos::widgets::calendar::CalendarLayout> = None;
    let mut last_session_layout: Option<dos::widgets::session_picker::SessionPickerLayout> = None;
    let mut last_ticker_layout: Option<TickerModalLayout> = None;
    let mut last_help_layout: Option<dos::widgets::help::HelpLayout> = None;
    let mut last_cmd_layout: Option<CmdPaletteLayout> = None;
    let mut last_live_confirm_layout: Option<dos::widgets::modal::ModalLayout> = None;

    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        // Redraw immediately after input, otherwise at most every
        // IDLE_REDRAW: clocks, flashes and streams still move, but an idle
        // terminal no longer rebuilds every pane 60 times a second.
        if dirty || last_draw.elapsed() >= IDLE_REDRAW {
            dirty = false;
            last_draw = Instant::now();
            terminal.draw(|frame| {
                let area = frame.area();
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1), // menu bar
                        Constraint::Min(1),    // panels
                        Constraint::Length(1), // separator
                        Constraint::Length(1), // status line
                        Constraint::Length(1), // F-key bar
                    ])
                    .split(area);
                layouts = Layouts {
                    menu: rows[0],
                    panels: rows[1],
                    sep: rows[2],
                    status: rows[3],
                    fbar: rows[4],
                };

                menu_bar::render_with_hover(
                    frame,
                    layouts.menu,
                    menu_for(app.view),
                    app.mouse,
                    palette,
                );

                last_markets_layout = None;
                last_screener_layout = None;
                last_strategy_panel = None;
                last_trade_ticket = None;
                // Mirror top-level edits (chart_zoom, chart_aggregate, chart_type,
                // timeframe, sym_idx) into the active pane BEFORE rendering, so
                // +/-/0/c/f keys take effect on the same frame.
                normalize_indices(app);
                sync_active_pane(app);
                match app.view {
                    View::Markets => {
                        if app.pane_count <= 1 {
                            let pane = &app.pane_states[app.pane_active as usize];
                            let filters = series_filters(app.chart_history_range, pane);
                            let layout = markets::render(
                                frame,
                                layouts.panels,
                                &pane.ticker,
                                &pane.name,
                                &pane.data,
                                &pane.footprint,
                                pane.chart_zoom,
                                pane.chart_aggregate,
                                pane.timeframe,
                                pane.chart_type,
                                pane.chart_offset,
                                &filters,
                                app.mouse,
                                palette,
                            );
                            // Stamp the post-filter scroll length back onto the
                            // pane so handle_key (which doesn't have access to
                            // the layout) can clamp scroll keys correctly.
                            let visible = layout.scroll_total;
                            if let Some(p) = app.pane_states.get_mut(app.pane_active as usize) {
                                p.visible_total = visible;
                            }
                            last_markets_layout = Some(layout);
                        } else {
                            let total = app.pane_count.max(1) as usize;
                            let constraints: Vec<ratatui::layout::Constraint> = (0..total)
                                .map(|_| ratatui::layout::Constraint::Ratio(1, total as u32))
                                .collect();
                            let pane_areas = ratatui::layout::Layout::default()
                                .direction(ratatui::layout::Direction::Vertical)
                                .constraints(constraints)
                                .split(layouts.panels);
                            let mut pane_rects: Vec<Rect> = Vec::with_capacity(total);
                            let mut visible_totals: Vec<(usize, usize)> = Vec::with_capacity(total);
                            for i in 0..total {
                                let pane_area = pane_areas[i];
                                pane_rects.push(pane_area);
                                let active = (app.pane_active as usize) == i;
                                let pane = &app.pane_states[i];
                                let filters = series_filters(app.chart_history_range, pane);
                                let full_len = markets::render_pane(
                                    frame,
                                    pane_area,
                                    &pane.ticker,
                                    &pane.data,
                                    &pane.footprint,
                                    &filters,
                                    pane.chart_offset,
                                    pane.chart_zoom,
                                    pane.chart_aggregate,
                                    pane.timeframe,
                                    pane.chart_type,
                                    active,
                                    palette,
                                );
                                visible_totals.push((i, full_len));
                            }
                            for (i, total) in visible_totals {
                                if let Some(p) = app.pane_states.get_mut(i) {
                                    p.visible_total = total;
                                }
                            }
                            last_markets_layout = Some(MarketsLayout {
                                area: layouts.panels,
                                pane_rects,
                                scrollbar_rect: Rect::default(),
                                scroll_total: 0,
                                from_date_rect: Rect::default(),
                                to_date_rect: Rect::default(),
                                session_rect: Rect::default(),
                            });
                        }
                    }
                    View::Screener => {
                        // Three-pane layout:
                        //   left  ≈ screener list
                        //   right top    = trade ticket (button grid + qty stepper)
                        //   right bottom = compact selection list
                        let h_split = Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                            .split(layouts.panels);
                        if app.screener_live {
                            let ws_status = if app.mini_ticker_arr_connected {
                                let secs = app.last_screener_tick_at.elapsed().as_secs();
                                if secs < 60 {
                                    format!("WS:{}s", secs)
                                } else {
                                    "WS:stale".to_string()
                                }
                            } else {
                                "WS:off".to_string()
                            };
                            last_screener_layout = Some(screener::render_live(
                                frame,
                                h_split[0],
                                &app.live_pairs,
                                app.screener_sel,
                                app.screener_sort,
                                app.screener_sort_asc,
                                app.screener_venue,
                                app.screener_min_qv,
                                &app.selected_set,
                                palette,
                                &ws_status,
                            ));
                        } else {
                            last_screener_layout = Some(screener::render(
                                frame,
                                h_split[0],
                                &app.symbols,
                                app.screener_sel,
                                app.screener_sort,
                                palette,
                                app.screener_sort_asc,
                            ));
                        }
                        // Trade ticket needs ~17 rows.  Selection list takes
                        // whatever remains.
                        let ticket_height = h_split[1].height.saturating_sub(4).min(17);
                        let v_split = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Length(ticket_height), Constraint::Min(3)])
                            .split(h_split[1]);
                        let pressed_action = app.last_ticket_press.and_then(|(a, at)| {
                            if at.elapsed().as_millis() < PRESS_FLASH_MS {
                                Some(a)
                            } else {
                                None
                            }
                        });
                        last_trade_ticket = Some(trade_ticket::render(
                            frame,
                            v_split[0],
                            &app.runtime,
                            app.ticket_qty,
                            app.ticket_notional,
                            app.size_mode,
                            app.leverage,
                            app.mouse,
                            pressed_action,
                            palette,
                        ));
                        last_strategy_panel = Some(strategy_panel::render(
                            frame,
                            v_split[1],
                            &app.runtime,
                            app.active_slot,
                            palette,
                        ));
                    }
                    View::Dashboard => {
                        dashboard::render_engine(frame, layouts.panels, &app.runtime, palette);
                    }
                    View::OrderBook => {
                        let flash = if app.ob_last_trade_at.elapsed() < Duration::from_millis(500) {
                            app.ob_last_trade_price
                        } else {
                            None
                        };
                        let stale_secs = if app.ob_snapshot.is_some() {
                            app.ob_last_snapshot_at.elapsed().as_secs()
                        } else {
                            0
                        };
                        let diag = dos::widgets::orderbook::LoadDiag {
                            buffer_len: app.ob_diff_buffer.len(),
                            seed_in_flight: app.ob_pending_seed.is_some() || app.ob_seed_wanted,
                            diff_ws_open: app.ob_diff_ws.as_ref().is_some_and(|h| h.is_alive()),
                            trade_ws_open: app.ob_trade_ws.as_ref().is_some_and(|h| h.is_alive()),
                            trades_received: app.ob_trades.len(),
                        };
                        dos::widgets::orderbook::render(
                            frame,
                            layouts.panels,
                            &app.ob_ticker,
                            app.ob_snapshot.as_ref(),
                            &app.ob_trades,
                            app.ob_zoom,
                            flash,
                            stale_secs,
                            app.ob_show_trades,
                            diag,
                            palette,
                        );
                    }
                    View::Settings => {
                        let cache = if app.db.is_some() {
                            db::db_file_size_bytes()
                                .map(db::human_bytes)
                                .unwrap_or_else(|| "n/a".into())
                        } else {
                            "DB OFF - nothing is saved".into()
                        };
                        let pressed = pressed_for(app, PressedBtnKind::Settings);
                        last_settings_layout = Some(settings::render(
                            frame,
                            layouts.panels,
                            &app.settings,
                            app.market,
                            app.timeframe,
                            &cache,
                            app.mouse,
                            pressed,
                            palette,
                        ));
                    }
                }

                // Separator line between panels and status
                let sep_filler: String = "─".repeat(layouts.sep.width as usize);
                let sep_style = ratatui::style::Style::default()
                    .fg(palette.accent_dim)
                    .bg(palette.bg)
                    .add_modifier(ratatui::style::Modifier::DIM);
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(ratatui::text::Span::styled(
                        sep_filler, sep_style,
                    )),
                    layouts.sep,
                );

                status_line::render_extended(
                    frame,
                    layouts.status,
                    &app.status,
                    app.runtime.is_live(),
                    app.mini_ticker_connected || app.mini_ticker_arr_connected,
                    palette,
                );
                function_bar::render_with_hover(frame, layouts.fbar, F_KEYS, app.mouse, palette);

                // Modal overlay.
                last_menu_dropdown_layout = None;
                last_calendar_layout = None;
                last_session_layout = None;
                last_ticker_layout = None;
                last_help_layout = None;
                last_cmd_layout = None;
                last_live_confirm_layout = None;
                match app.modal {
                    Some(ModalKind::TickerInput) => {
                        last_ticker_layout = Some(render_ticker_modal(
                            frame,
                            area,
                            &app.ticker_input,
                            app.mouse,
                            palette,
                        ));
                    }
                    Some(ModalKind::MenuDropdown) => {
                        last_menu_dropdown_layout =
                            Some(render_menu_dropdown(frame, &layouts, app, palette));
                    }
                    Some(ModalKind::DatePickerFrom) => {
                        let active = app.pane_active as usize;
                        let cur = app
                            .pane_states
                            .get(active)
                            .and_then(|p| p.date_from.clone());
                        last_calendar_layout = Some(dos::widgets::calendar::render(
                            frame,
                            area,
                            &app.calendar,
                            "Pick FROM date",
                            cur.as_deref(),
                            app.mouse,
                            None,
                            palette,
                        ));
                    }
                    Some(ModalKind::DatePickerTo) => {
                        let active = app.pane_active as usize;
                        let cur = app.pane_states.get(active).and_then(|p| p.date_to.clone());
                        last_calendar_layout = Some(dos::widgets::calendar::render(
                            frame,
                            area,
                            &app.calendar,
                            "Pick TO date",
                            cur.as_deref(),
                            app.mouse,
                            None,
                            palette,
                        ));
                    }
                    Some(ModalKind::SessionTime) => {
                        last_session_layout = Some(dos::widgets::session_picker::render(
                            frame,
                            area,
                            &app.session_picker,
                            app.mouse,
                            None,
                            palette,
                        ));
                    }
                    Some(ModalKind::LiveConfirm) => {
                        last_live_confirm_layout = Some(dos::widgets::modal::render(
                            frame,
                            area,
                            &dos::widgets::modal::Modal {
                                title: " Enable Live Trading ",
                                body: &[
                                    "Orders will be sent to the REAL Binance exchange.",
                                    "Make sure your API key has trading permissions.",
                                    "",
                                    "Continue?",
                                ],
                                buttons: &[
                                    dos::widgets::modal::Button {
                                        label: " Yes ",
                                        primary: false,
                                    },
                                    dos::widgets::modal::Button {
                                        label: " No ",
                                        primary: true,
                                    },
                                ],
                                focused_button: app.live_confirm_focus,
                                mouse: app.mouse,
                                pressed_button: None,
                            },
                            palette,
                        ));
                    }
                    Some(ModalKind::Help) => {
                        let sections = build_help_sections();
                        let page = dos::widgets::help::HelpPage {
                            page: app.help_page,
                            page_count: sections.len().max(1),
                            sections,
                        };
                        last_help_layout = Some(dos::widgets::help::render(
                            frame, area, &page, app.mouse, palette,
                        ));
                    }
                    Some(ModalKind::CommandPalette) => {
                        last_cmd_layout = Some(render_command_palette(
                            frame,
                            area,
                            &app.command_input,
                            app.mouse,
                            palette,
                        ));
                    }
                    None => {}
                }

                // Mouse cursor — bright-yellow 3-cell band at last reported
                // position.  Pure ASCII (just bg colour, no exotic glyphs).
                // Wider than 1 cell so it's visible on dense UI; centered on
                // the actual mouse position so the precise click target is
                // unambiguous.
                if let Some((mx, my)) = app.mouse {
                    if my < area.height {
                        let buf = frame.buffer_mut();
                        for dx in -1i32..=1 {
                            let cx = mx as i32 + dx;
                            if cx < 0 || cx as u16 >= area.width {
                                continue;
                            }
                            if let Some(cell) = buf.cell_mut((cx as u16, my)) {
                                // Strongest visibility: yellow bg + black fg, regardless of underlying.
                                let s = cell
                                    .style()
                                    .bg(ratatui::style::Color::LightYellow)
                                    .fg(ratatui::style::Color::Black);
                                cell.set_style(s);
                                // Keep the existing char visible.
                                if dx == 0 {
                                    cell.modifier.insert(Modifier::BOLD);
                                }
                            }
                        }
                    }
                }
            })?;
        }

        // Drain ALL queued events before redrawing — eliminates lag when the
        // user moves the mouse fast.  Without this, only one event per
        // 16ms-poll-tick is processed, the rest queue and the cursor lags.
        let mut handled = false;
        while event::poll(Duration::from_millis(if handled { 0 } else { 16 }))? {
            handled = true;
            dirty = true;
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_key(app, k.code);
                    // Handlers may mutate lists the layouts index: redraw first.
                    break;
                }
                Event::Mouse(m) => {
                    app.mouse = Some((m.column, m.row));
                    handle_mouse(
                        app,
                        m,
                        &layouts,
                        last_markets_layout.as_ref(),
                        last_screener_layout.as_ref(),
                        last_settings_layout.as_ref(),
                        last_menu_dropdown_layout.as_ref(),
                        last_calendar_layout.as_ref(),
                        last_session_layout.as_ref(),
                        last_strategy_panel.as_ref(),
                        last_trade_ticket.as_ref(),
                        last_help_layout.as_ref(),
                        last_cmd_layout.as_ref(),
                        last_ticker_layout.as_ref(),
                        last_live_confirm_layout.as_ref(),
                    );
                    if !matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
                        break;
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Synthetic tick — keeps the chart visibly alive even when WS
        // trades are absent (low-volume alts, stocks, forex).

        // Background fetch poller.
        poll_pending_fetch(app);
        poll_pending_live(app);
        poll_pending_footprints(app);
        drain_startup(app);
        poll_key_check(app);
        ensure_all_ws(app);
        drain_ws_trades(app);
        ensure_orderbook_ws(app);
        drain_orderbook_ws(app);
        // Sub-second price stream — drives marks for every selection
        // far faster than the 3-5s REST refresh.
        ensure_mini_ticker_ws(app);
        drain_mini_ticker(app);
        // All-symbols miniTicker@arr for the Screener LIVE view.
        // Connects when on Screener; updates `live_pairs` at ~1-2s cadence.
        ensure_mini_ticker_arr_ws(app);
        drain_mini_ticker_arr(app);

        // Candle rollover — close stale candles and open new ones so the
        // WS streams fill the correct timeframe bucket instead of extending
        // the same candle forever.  Only runs for WS-connected panes.
        rollover_candles(app);

        // Auto-refresh live pairs whenever there are engine slots OR open
        // positions — regardless of view.  Without this the Dashboard
        // marks would freeze on the entry price (and the F4 ticket
        // header would lie about open PnL).  Cadence:
        //   * 3s while the user is on Screener with engine slots
        //   * 5s on every other view (cheap background sync)
        //   * 15s on Screener live without slots (background pair refresh)
        //   * 1h otherwise (no trading activity)
        let has_engine_state =
            !app.runtime.slots.is_empty() || !app.runtime.broker.all_positions().is_empty();
        let refresh_interval = if has_engine_state {
            if app.view == View::Screener {
                Duration::from_secs(3)
            } else {
                Duration::from_secs(5)
            }
        } else if app.screener_live && app.view == View::Screener {
            Duration::from_secs(15)
        } else {
            Duration::from_secs(60 * 60)
        };
        if app.pending_live.is_none()
            && (has_engine_state || (app.screener_live && app.view == View::Screener))
            && app.last_live_refresh.elapsed() >= refresh_interval
        {
            request_live_pairs(app, false);
        }
        // After a live fetch lands, splice prices back into app.symbols + pane
        // data so dashboard portfolio + chart panes reflect real prices.
        // Runs only ONCE per fetch (then cleared) so it doesn't overwrite
        // live WS price updates on every render loop iteration.
        if app.live_pairs_dirty && !app.live_pairs.is_empty() {
            update_symbols_from_live(app);
            app.live_pairs_dirty = false;
            // Fresh prices just landed — push them through the runtime
            // immediately so the Dashboard / ticket reflect real marks
            // without waiting for the next 250ms tick gate.
            if !app.runtime.slots.is_empty() {
                push_marks_to_runtime(app);
            }
        }

        // Drive the trading engine on each main-loop iteration.  Cheap
        // when the slot list is small; runtime self-gates per-slot.
        // Always runs (not just on Dashboard) so limit fills + strategy
        // timers fire promptly regardless of which page the user is on.
        if !app.runtime.slots.is_empty() {
            push_marks_to_runtime(app);
        }

        // Auto-refresh footprint every 30s for any pane viewing DeltaCluster
        // on Markets view (and we're on the Markets view).  Real-time-ish
        // without paying the cost when user isn't looking at footprints.
        if app.view == View::Markets
            && app.market == MarketType::Crypto
            && app.last_footprint_refresh.elapsed() >= Duration::from_secs(30)
        {
            let mut any = false;
            for i in 0..app.pane_count as usize {
                if let Some(p) = app.pane_states.get(i) {
                    if p.chart_type == dos::widgets::chart::ChartType::DeltaCluster {
                        any = true;
                        refresh_footprint(app, i);
                    }
                }
            }
            if any {
                app.last_footprint_refresh = Instant::now();
            }
        }

        // Periodic snapshot to disk every 10s — survives crashes.
        if app.last_persist.elapsed() >= Duration::from_secs(10) {
            app.last_persist = Instant::now();
            if let Some(c) = app.db.as_ref() {
                snapshot_paper_state(&app.runtime, c, &mut app.last_persisted);
            }
            persist_risk_limits(app);
        }

        // Live-mode plumbing: drain Binance event channel every loop +
        // reconcile balances / open orders every 15s + listen-key keepalive
        // every 20 min (Binance expiry is 60 min).
        if app.runtime.is_live() {
            app.runtime.pump_binance(current_now_ms());
            if app.runtime.live_error_seq != app.live_error_shown {
                app.live_error_shown = app.runtime.live_error_seq;
                if let Some(e) = &app.runtime.live_error {
                    app.status = format!("LIVE: {e}");
                }
            }
            if app.last_live_sync.elapsed() >= Duration::from_secs(15) {
                app.last_live_sync = Instant::now();
                if let Some(b) = app.runtime.binance.as_ref() {
                    b.sync_account();
                    b.sync_open_orders();
                }
            }
            if app.last_listen_key_keepalive.elapsed() >= Duration::from_secs(1200) {
                app.last_listen_key_keepalive = Instant::now();
                app.runtime.keepalive_user_data_stream();
            }
        }
    }
    Ok(())
}

/// Kept as a no-op placeholder.  The chart now ONLY shows real data from
/// Yahoo / Binance / WS tick streams.  No synthetic walk corrupts OHLC.
/// Roll over stale candles for WS-connected panes.  Whenever the wall-clock
/// timeframe bucket boundary passes, the current candle is finalised and a
/// new one is opened using the last trade price as the open.
fn rollover_candles(app: &mut App) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    for pane in app.pane_states.iter_mut() {
        if !pane.ws.as_ref().is_some_and(|h| h.is_alive()) {
            continue;
        }
        if pane.data.is_empty() {
            continue;
        }
        let tf_secs = pane.timeframe.duration_secs() as i64;
        if tf_secs == 0 {
            continue;
        }
        let bucket_ms = (now / (tf_secs * 1000)) * (tf_secs * 1000);
        let last = pane.data.last().unwrap();
        if last.time_ms >= bucket_ms {
            continue;
        }
        let prev_close = last.close;
        let date = dos::markets::unix_secs_to_iso_date(bucket_ms / 1000);
        pane.data.push(Ohlc {
            open: prev_close,
            high: prev_close,
            low: prev_close,
            close: prev_close,
            volume: 0.0,
            date,
            time_ms: bucket_ms,
            is_gap: false,
        });
    }
}

/// Debug-only guard against `handle_key` re-entering itself without bound
/// (audit A2).  The structural fix is in `execute_menu_action`; this makes a
/// regression fail loudly in tests / debug runs and costs nothing in release.
#[cfg(debug_assertions)]
struct KeyDepthGuard;

#[cfg(debug_assertions)]
thread_local! {
    static KEY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(debug_assertions)]
impl KeyDepthGuard {
    fn enter() -> Self {
        KEY_DEPTH.with(|d| {
            d.set(d.get() + 1);
            assert!(d.get() <= 4, "handle_key recursion depth {}", d.get());
        });
        KeyDepthGuard
    }
}

#[cfg(debug_assertions)]
impl Drop for KeyDepthGuard {
    fn drop(&mut self) {
        KEY_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// Re-establish list/index invariants before drawing so no render or hit-test
/// path can index past the end (audit A1/A3).
fn normalize_indices(app: &mut App) {
    if app.pane_states.is_empty() {
        return;
    }
    app.pane_count = app.pane_states.len().min(255) as u8;
    app.pane_active = app.pane_active.min(app.pane_count - 1);
    app.sym_idx = app.sym_idx.min(app.symbols.len().saturating_sub(1));
    if app.runtime.slots.is_empty() {
        app.active_slot = None;
    } else if let Some(a) = app.active_slot {
        app.active_slot = Some(a.min(app.runtime.slots.len() - 1));
    }
    let max_sel = if app.screener_live {
        app.live_pairs.len()
    } else {
        app.symbols.len()
    };
    app.screener_sel = app.screener_sel.min(max_sel.saturating_sub(1));
}

fn handle_key(app: &mut App, code: KeyCode) {
    #[cfg(debug_assertions)]
    let _depth = KeyDepthGuard::enter();
    // Global keybindings — F-keys switch between top-level pages from any
    // view (and dismiss any open modal first).
    if app.modal == Some(ModalKind::MenuDropdown) {
        // F-keys close the dropdown and fall through to the global page-
        // switch handler below.  Other keys are handled here.
        match code {
            KeyCode::F(_) => {
                app.modal = None;
                app.menu_open = None;
                // fall through
            }
            KeyCode::Esc => {
                app.modal = None;
                app.menu_open = None;
                return;
            }
            KeyCode::Up => {
                if app.menu_item_sel > 0 {
                    app.menu_item_sel -= 1;
                }
                return;
            }
            KeyCode::Down => {
                let n = current_dropdown_items(app).len();
                if n > 0 && app.menu_item_sel + 1 < n {
                    app.menu_item_sel += 1;
                }
                return;
            }
            KeyCode::Enter => {
                let item_idx = app.menu_item_sel;
                let menu_idx = app.menu_open.unwrap_or(0);
                execute_menu_action(app, menu_idx, item_idx);
                return;
            }
            KeyCode::Left | KeyCode::Right => {
                let entries = menu_for(app.view);
                if !entries.is_empty() {
                    let cur = app.menu_open.unwrap_or(0);
                    let next = match code {
                        KeyCode::Left => (cur + entries.len() - 1) % entries.len(),
                        _ => (cur + 1) % entries.len(),
                    };
                    app.menu_open = Some(next);
                    app.menu_item_sel = 0;
                }
                return;
            }
            _ => return,
        }
    }

    if app.modal.is_none() {
        match code {
            KeyCode::F(1) => {
                app.modal = Some(ModalKind::Help);
                app.help_page = 0;
                app.status = "Help: ←/→ page  Esc/F1/q close".into();
                return;
            }
            KeyCode::F(3) => {
                app.view = View::Markets;
                let needs_refresh = app
                    .pane_states
                    .get(app.pane_active as usize)
                    .map(|p| p.data.is_empty())
                    .unwrap_or(true);
                if needs_refresh {
                    refresh_pane(app, app.pane_active as usize);
                }
                app.status = "Markets: ←/→ symbol  +/- zoom  c chart  f tf  / ticker  Esc back  |  :cmd F1 help".into();
                return;
            }
            KeyCode::F(4) => {
                app.view = View::Screener;
                // Auto-trigger live fetch on first entry so user sees real
                // Binance pairs without manual `l` toggle.  Screener is
                // independent of the F9 "Market" setting (live mode is
                // always Binance USDT pairs).
                if app.screener_live && app.live_pairs.is_empty() && app.pending_live.is_none() {
                    request_live_pairs(app, true);
                }
                app.status =
                    "Screener: ↑/↓ nav  Enter chart  s sort  v venue  m vol  |  :cmd F1 help"
                        .into();
                return;
            }
            KeyCode::F(5) => {
                app.view = View::Dashboard;
                app.status = "Dashboard: portfolio & PnL  Esc back  |  :cmd F1 help".into();
                return;
            }
            KeyCode::F(6) => {
                app.view = View::OrderBook;
                // Default to active pane's ticker.  User can change via the
                // top "Symbol" menu.
                if app.ob_ticker.is_empty() {
                    if let Some(p) = app.pane_states.get(app.pane_active as usize) {
                        app.ob_ticker = p.ticker.clone();
                    }
                }
                app.status = format!(
                    "Order Book: {} ({})  |  v venue  +/- zoom  t ticker  :cmd F1 help",
                    app.ob_ticker,
                    app.ob_venue.name()
                );
                return;
            }
            KeyCode::F(9) => {
                app.view = View::Settings;
                app.settings = SettingsState::new_full(
                    app.market,
                    app.timeframe,
                    &app.binance_key,
                    &app.binance_secret,
                    &app.binance_futures_key,
                    &app.binance_futures_secret,
                    app.ob_show_trades,
                    app.chart_history_range,
                    app.font_size,
                    app.runtime.is_live(),
                );
                app.status = "Settings: ↑/↓ nav  Enter apply  Esc back  |  :cmd F1 help".into();
                return;
            }
            KeyCode::F(10) => {
                app.quit = true;
                return;
            }
            KeyCode::Char(':') => {
                // Command palette — like Vim/opencontrol command mode
                app.modal = Some(ModalKind::CommandPalette);
                app.command_input.text.clear();
                app.command_input.cursor = 0;
                app.status = "Command: :help  :live  :paper  :q  :screener  :markets  :dashboard  :book  :settings".into();
                return;
            }
            _ => {}
        }
    }

    // Modal: Help overlay — keyboard shortcut reference.
    if app.modal == Some(ModalKind::Help) {
        match code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('q') => {
                app.modal = None;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let total = help_page_count();
                app.help_page = if app.help_page > 0 {
                    app.help_page - 1
                } else {
                    total.saturating_sub(1)
                };
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let total = help_page_count();
                app.help_page = (app.help_page + 1) % total.max(1);
            }
            _ => {}
        }
        return;
    }

    // Modal: Command Palette — quick actions via `:command`
    if app.modal == Some(ModalKind::CommandPalette) {
        match code {
            KeyCode::Esc => {
                app.modal = None;
                app.command_input.text.clear();
                app.command_input.cursor = 0;
            }
            KeyCode::Enter => {
                execute_command(app);
            }
            KeyCode::Char(c) => app.command_input.push_char(c),
            KeyCode::Backspace => app.command_input.backspace(),
            KeyCode::Delete => app.command_input.delete(),
            KeyCode::Left => app.command_input.left(),
            KeyCode::Right => app.command_input.right(),
            KeyCode::Home => app.command_input.home(),
            KeyCode::End => app.command_input.end(),
            _ => {}
        }
        return;
    }

    // Modal: DatePicker (FROM or TO).
    if matches!(
        app.modal,
        Some(ModalKind::DatePickerFrom) | Some(ModalKind::DatePickerTo)
    ) {
        let is_from = app.modal == Some(ModalKind::DatePickerFrom);
        match code {
            KeyCode::Esc => {
                app.modal = None;
            }
            KeyCode::Left => app.calendar.move_focus(-1),
            KeyCode::Right => app.calendar.move_focus(1),
            KeyCode::Up => app.calendar.move_focus(-7),
            KeyCode::Down => app.calendar.move_focus(7),
            KeyCode::PageUp => app.calendar.shift_month(-1),
            KeyCode::PageDown => app.calendar.shift_month(1),
            KeyCode::Home => {
                app.calendar = dos::widgets::calendar::CalendarState::from_date(None);
            }
            KeyCode::Delete | KeyCode::Backspace => {
                commit_date(app, is_from, None);
                app.modal = None;
            }
            KeyCode::Enter => {
                let iso = app.calendar.current_iso();
                commit_date(app, is_from, Some(iso));
                app.modal = None;
            }
            _ => {}
        }
        return;
    }

    // Modal: SessionTime.
    if app.modal == Some(ModalKind::SessionTime) {
        match code {
            KeyCode::Esc => {
                app.modal = None;
            }
            KeyCode::Tab => {
                app.session_picker.focus = (app.session_picker.focus + 2) % 4; // hour ↔ hour
            }
            KeyCode::Left => {
                if app.session_picker.focus >= 2 {
                    app.session_picker.focus -= 2;
                }
            }
            KeyCode::Right => {
                if app.session_picker.focus < 2 {
                    app.session_picker.focus += 2;
                }
            }
            KeyCode::Up => app.session_picker.nudge(1),
            KeyCode::Down => app.session_picker.nudge(-1),
            KeyCode::Char('0') => {
                app.session_picker.start_min = 0;
                app.session_picker.end_min = 1440;
            }
            KeyCode::Delete | KeyCode::Backspace => {
                commit_session(app, dos::markets::SessionWindow::ALL);
                app.modal = None;
            }
            KeyCode::Enter => {
                let w = app.session_picker.to_window();
                commit_session(app, w);
                app.modal = None;
            }
            _ => {}
        }
        return;
    }

    // Modal: TickerInput — manual ticker entry from Markets view.
    if app.modal == Some(ModalKind::TickerInput) {
        match code {
            KeyCode::Esc => {
                app.modal = None;
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
            }
            KeyCode::Enter => submit_ticker(app),
            KeyCode::Char(c) => app.ticker_input.push_char(c.to_ascii_uppercase()),
            KeyCode::Backspace => app.ticker_input.backspace(),
            KeyCode::Delete => app.ticker_input.delete(),
            KeyCode::Left => app.ticker_input.left(),
            KeyCode::Right => app.ticker_input.right(),
            KeyCode::Home => app.ticker_input.home(),
            KeyCode::End => app.ticker_input.end(),
            _ => {}
        }
        return;
    }

    // Modal: LiveConfirm — Enter runs the FOCUSED button (default: No).
    if app.modal == Some(ModalKind::LiveConfirm) {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => cancel_live(app),
            KeyCode::Char('y') | KeyCode::Char('Y') => confirm_live(app),
            KeyCode::Enter => {
                if app.live_confirm_focus == 0 {
                    confirm_live(app);
                } else {
                    cancel_live(app);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                app.live_confirm_focus = 1 - app.live_confirm_focus.min(1);
            }
            _ => {}
        }
        return;
    }

    // Settings page — market type + API keys.
    if app.view == View::Settings {
        match settings::handle_key(&mut app.settings, code) {
            settings::SettingsAction::Cancel => {
                app.view = View::Screener;
                app.status = "Settings cancelled.".into();
            }
            settings::SettingsAction::Apply => {
                apply_settings(app);
                sync_live_with_settings(app);
            }
            settings::SettingsAction::ClearCache => clear_cache_action(app),
            settings::SettingsAction::None => {}
        }
        return;
    }

    // Dashboard view.
    if app.view == View::Dashboard {
        match code {
            KeyCode::Esc => {
                app.view = View::Screener;
                app.status = "Ready.".into();
            }
            KeyCode::Char('q') => app.quit = true,
            _ => {}
        }
        return;
    }

    // Order book view — F6.
    if app.view == View::OrderBook {
        match code {
            KeyCode::Esc => {
                app.view = View::Screener;
                app.status = "Screener.".into();
            }
            KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('/') => {
                app.modal = Some(ModalKind::TickerInput);
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                app.ob_zoom = next_ob_zoom_in(app.ob_zoom);
                app.status = format!("Order Book zoom: x{}", app.ob_zoom);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                app.ob_zoom = next_ob_zoom_out(app.ob_zoom);
                app.status = format!("Order Book zoom: x{}", app.ob_zoom);
            }
            KeyCode::Char('0') => {
                app.ob_zoom = 1;
                app.status = "Order Book zoom: x1 (native tick)".into();
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if let Some(h) = app.ob_diff_ws.take() {
                    let _ = h.stop_tx.send(());
                }
                if let Some(h) = app.ob_trade_ws.take() {
                    let _ = h.stop_tx.send(());
                }
                app.ob_ws_symbol.clear();
                app.ob_snapshot = None;
                app.ob_local_book = None;
                app.ob_diff_buffer.clear();
                app.ob_pending_seed = None;
                app.ob_trades.clear();
                app.status = "Order book: reconnecting…".into();
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                flip_ob_venue(app);
                app.status = format!("OrderBook venue: {}", app.ob_venue.name());
            }
            KeyCode::Char('q') => app.quit = true,
            _ => {}
        }
        return;
    }

    // Screener view.
    if app.view == View::Screener {
        match code {
            // Cycle through selected slots; the trade ticket follows.
            KeyCode::Tab => {
                cycle_active_slot(app, 1);
            }
            KeyCode::BackTab => {
                cycle_active_slot(app, -1);
            }
            // Qty stepper hot-keys (mirrors the [-]/[+] panel buttons).
            KeyCode::Char('+') | KeyCode::Char('=') => {
                bump_ticket_value(app, 1);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                bump_ticket_value(app, -1);
            }
            // Basket hot-keys (uppercase to avoid clashing with `s`/`v`/`m`).
            KeyCode::Char('B') => {
                submit_basket_action(app, TicketAction::BuyMarket);
            }
            KeyCode::Char('N') => {
                submit_basket_action(app, TicketAction::SellMarket);
            }
            KeyCode::Char('Z') => {
                submit_basket_action(app, TicketAction::Close);
            }
            KeyCode::Char('U') => {
                app.size_mode = app.size_mode.flip();
                app.status = format!("size mode = {}", app.size_mode.label());
            }
            KeyCode::Char('X') => {
                app.leverage = trade_ticket::cycle_leverage(app.leverage, 1);
                app.status = format!("leverage = x{}", app.leverage);
            }
            KeyCode::Char(' ') => {
                toggle_screener_selection_at(app, app.screener_sel);
            }
            KeyCode::Esc => {
                // Screener is the home page; nothing to back to.
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if app.screener_sel > 0 {
                    app.screener_sel -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = screener_total(app);
                if max > 0 && app.screener_sel + 1 < max {
                    app.screener_sel += 1;
                }
            }
            KeyCode::PageUp => {
                app.screener_sel = app.screener_sel.saturating_sub(10);
            }
            KeyCode::PageDown => {
                let max = screener_total(app);
                if max > 0 {
                    app.screener_sel = (app.screener_sel + 10).min(max - 1);
                }
            }
            KeyCode::Home => {
                app.screener_sel = 0;
            }
            KeyCode::End => {
                let max = screener_total(app);
                if max > 0 {
                    app.screener_sel = max - 1;
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                app.screener_sort = app.screener_sort.cycle();
                app.screener_sort_asc = false;
                app.screener_sel = 0;
                app.status = format!("Sort: {}", app.screener_sort.display_name(false));
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                app.screener_live = !app.screener_live;
                app.screener_sel = 0;
                if app.screener_live {
                    request_live_pairs(app, true);
                    app.status =
                        format!("Live screener: {} (loading...)", app.screener_venue.name());
                } else {
                    app.status = "Screener: synthetic samples".into();
                }
            }
            KeyCode::Char('v') | KeyCode::Char('V') if app.screener_live => {
                use dos::data::binance::Venue;
                app.screener_venue = match app.screener_venue {
                    Venue::Spot => Venue::Futures,
                    Venue::Futures => Venue::Spot,
                };
                app.screener_sel = 0;
                request_live_pairs(app, true);
                // Chart data and streams follow the venue too.
                for i in 0..app.pane_states.len() {
                    refresh_pane(app, i);
                }
                app.status = format!("Venue: {} (refetching)", app.screener_venue.name());
            }
            KeyCode::Char('m') | KeyCode::Char('M') if app.screener_live => {
                // Cycle through some sane volume thresholds.
                let next = match app.screener_min_qv as u64 {
                    0..=49_999 => 50_000.0,
                    50_000..=499_999 => 500_000.0,
                    500_000..=4_999_999 => 5_000_000.0,
                    5_000_000..=49_999_999 => 50_000_000.0,
                    _ => 0.0,
                };
                app.screener_min_qv = next;
                app.screener_sel = 0;
                app.status = format!(
                    "min volume: {} USDT/24h",
                    screener::fmt_usdt(app.screener_min_qv)
                );
            }
            KeyCode::Char('r') | KeyCode::Char('R') if app.screener_live => {
                request_live_pairs(app, true);
                app.status = "Refreshing live pairs...".into();
            }
            KeyCode::Enter => {
                if app.screener_live {
                    let ranked = screener::rank_live(
                        &app.live_pairs,
                        500,
                        app.screener_sort,
                        app.screener_min_qv,
                        app.screener_sort_asc,
                    );
                    if let Some((idx, _)) = ranked.get(app.screener_sel) {
                        let pair = app.live_pairs[*idx].clone();
                        let base = pair.symbol.trim_end_matches("USDT").to_string();
                        let name =
                            format!("{} (Binance {})", pair.symbol, app.screener_venue.name());
                        screener_open_markets(app, &base, &name);
                    }
                } else {
                    let gainers = screener::top_gainers_sorted(
                        &app.symbols,
                        10,
                        app.screener_sort,
                        app.screener_sort_asc,
                    );
                    if let Some((sym_idx, _)) = gainers.get(app.screener_sel) {
                        let Some(s) = app.symbols.get(*sym_idx).cloned() else {
                            return;
                        };
                        app.sym_idx = *sym_idx;
                        app.chart_history_range = HistoryRange::All;
                        app.view = View::Markets;
                        app.status = format!("Markets: {} - +/- zoom  Esc back", s.ticker);
                        set_active_ticker(app, s.ticker, s.name, Some(*sym_idx));
                    }
                }
            }
            KeyCode::Char('q') => app.quit = true,
            _ => {}
        }
        return;
    }

    // Markets view has its own keys.
    if app.view == View::Markets {
        match code {
            KeyCode::Esc => {
                app.view = View::Screener;
                app.status = "Screener.".into();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let n = app.symbols.len();
                if n > 0 {
                    let new_idx = if app.sym_idx > 0 {
                        app.sym_idx - 1
                    } else {
                        n - 1
                    };
                    if let Some(s) = app.symbols.get(new_idx).cloned() {
                        set_active_ticker(app, s.ticker, s.name, Some(new_idx));
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let n = app.symbols.len();
                if n > 0 {
                    let new_idx = (app.sym_idx + 1) % n;
                    if let Some(s) = app.symbols.get(new_idx).cloned() {
                        set_active_ticker(app, s.ticker, s.name, Some(new_idx));
                    }
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                // Zoom IN: shrink aggregation first, then widen candles.
                let zoom_cap = if app.chart_type == dos::widgets::chart::ChartType::DeltaCluster {
                    16 // need ≥9 to fit numbers; allow up to 16
                } else {
                    5
                };
                if app.chart_aggregate > 1 {
                    app.chart_aggregate -= 1;
                } else if app.chart_zoom < zoom_cap {
                    app.chart_zoom += 1;
                }
                app.status = format!("zoom: w x{} agg x{}", app.chart_zoom, app.chart_aggregate);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                // Zoom OUT: narrow candles first, then increase aggregation.
                if app.chart_zoom > 1 {
                    app.chart_zoom -= 1;
                } else if app.chart_aggregate < 16 {
                    app.chart_aggregate += 1;
                }
                app.status = format!("zoom: w x{} agg x{}", app.chart_zoom, app.chart_aggregate);
            }
            KeyCode::Char('0') => {
                app.chart_zoom = 1;
                app.chart_aggregate = 1;
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_offset = 0;
                }
                app.status = "zoom + scroll reset".into();
            }
            KeyCode::Char(',') | KeyCode::Char('<') => {
                // Single-candle scroll for max smoothness.
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    // visible_total is the post-filter aggregated length
                    // stamped by the renderer.  Falls back to raw `data`
                    // length on the very first frame before the renderer
                    // has run.
                    let max_offset = scroll_max_offset(p);
                    p.chart_offset = (p.chart_offset + 1).min(max_offset);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
            }
            KeyCode::Char('.') | KeyCode::Char('>') => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_offset = p.chart_offset.saturating_sub(1);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
            }
            KeyCode::PageUp => {
                // Half-page jump.
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    let max_offset = scroll_max_offset(p);
                    p.chart_offset = (p.chart_offset + 25).min(max_offset);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
            }
            KeyCode::PageDown => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_offset = p.chart_offset.saturating_sub(25);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
            }
            KeyCode::Home => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_offset = 0;
                    app.status = "scroll: LIVE".into();
                }
            }
            KeyCode::End => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    let total = if p.visible_total > 0 {
                        p.visible_total
                    } else {
                        p.data.len()
                    };
                    p.chart_offset = total.saturating_sub(20);
                    app.status = format!("scroll: oldest (-{})", p.chart_offset);
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                refresh_active_symbol(app);
            }
            KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('/') => {
                app.modal = Some(ModalKind::TickerInput);
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
                app.status = "Enter ticker, then Enter to fetch".into();
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                app.chart_type = app.chart_type.cycle();
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_type = app.chart_type;
                }
                if app.chart_type == dos::widgets::chart::ChartType::DeltaCluster {
                    app.chart_zoom = app.chart_zoom.max(9);
                    if let Some(p) = app.pane_states.get_mut(active) {
                        p.chart_zoom = app.chart_zoom;
                    }
                    refresh_footprint(app, active);
                }
                ensure_ws_for_pane(app, active);
                app.status = format!("chart: {}", app.chart_type.name());
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                app.timeframe = next_timeframe(app.timeframe);
                // Mirror to active pane only — other panes keep their TFs.
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.timeframe = app.timeframe;
                    p.data.clear(); // invalidate cached data; refresh fills it
                }
                app.settings = SettingsState::new(
                    app.market,
                    app.timeframe,
                    &app.binance_key,
                    &app.binance_secret,
                    app.runtime.is_live(),
                );
                if let Some(c) = app.db.as_ref() {
                    let _ = db::save_kv(c, "timeframe", app.timeframe.name());
                }
                app.status = format!(
                    "pane {} timeframe: {} (refetching)",
                    active + 1,
                    app.timeframe.name()
                );
                refresh_active_symbol(app);
            }
            KeyCode::Char(']') => {
                add_pane(app);
            }
            KeyCode::Char('[') => {
                remove_pane(app);
            }
            KeyCode::Tab if app.pane_count > 1 => {
                cycle_active_pane(app);
            }
            KeyCode::Char('L') => {
                toggle_link(app);
            }
            KeyCode::Char('q') => app.quit = true,
            _ => {}
        }
        return;
    }

    let _ = code;
}

fn refresh_active_symbol(app: &mut App) {
    refresh_pane(app, app.pane_active as usize);
}

/// Schedule a background fetch of recent aggTrades + tickSize for pane `i`.
/// Runs only for Crypto market (Binance).  Result lands as a vec of
/// `FootprintCandle` in `pane.footprint`.
/// Ensure pane[i] has an active WebSocket subscribed to the right symbol.
/// Drops + reconnects when ticker changed.  Closes WS when leaving Markets
/// view or switching off Crypto.  ANY crypto chart type benefits — every
/// trade tick updates pane.data.last() in real time.
fn ensure_ws_for_pane(app: &mut App, i: usize) {
    let pane = match app.pane_states.get(i) {
        Some(p) => p,
        None => return,
    };
    let want =
        app.view == View::Markets && app.market == MarketType::Crypto && !pane.ticker.is_empty();
    let venue_is_futures = matches!(app.screener_venue, dos::data::binance::Venue::Futures);
    let target_symbol = if want {
        dos::data::binance::binance_symbol(&pane.ticker)
    } else {
        String::new()
    };
    // The connection is identified by symbol AND venue: switching Spot ↔
    // Futures must reconnect (audit A7).
    let target_key = format!(
        "{target_symbol}|{}",
        if venue_is_futures { 'F' } else { 'S' }
    );
    let current_symbol = pane.ws_symbol.clone();
    let mut needs_close = false;
    let mut needs_open = false;
    // A dead worker counts as "no connection" — it is replaced, not kept.
    let ws_alive = pane.ws.as_ref().is_some_and(|h| h.is_alive());
    if !want && pane.ws.is_some() {
        needs_close = true;
    } else if want && (!ws_alive || current_symbol != target_key) {
        needs_close = pane.ws.is_some();
        needs_open = true;
    }
    if needs_close {
        if let Some(p) = app.pane_states.get_mut(i) {
            if let Some(h) = p.ws.take() {
                let _ = h.stop_tx.send(());
                // Don't join the thread here; it may block on `socket.read()`.
                // It exits on next message or on TCP close.
                drop(h.thread);
            }
            p.ws_symbol.clear();
        }
    }
    if needs_open {
        let h = dos::data::binance_ws::connect_agg_trades(&target_symbol, venue_is_futures);
        if let Some(p) = app.pane_states.get_mut(i) {
            p.ws = Some(h);
            p.ws_symbol = target_key;
        }
    }
}

/// Drain incoming WS trades for every pane.  For each trade:
///   * Update `pane.data.last()` in place — close = trade.price, extend
///     high/low, accumulate volume.
///   * Append to `ws_trades` for the footprint chart.
///
/// Rebuild footprint when chart_type is DeltaCluster.
fn drain_ws_trades(app: &mut App) {
    for i in 0..app.pane_states.len() {
        let pane = match app.pane_states.get_mut(i) {
            Some(p) => p,
            None => continue,
        };
        let mut got_any = false;
        if let Some(h) = pane.ws.as_ref() {
            while let Ok(t) = h.rx.try_recv() {
                // Realtime price update — drive the last (in-progress)
                // candle from this trade.
                if let Some(last) = pane.data.last_mut() {
                    last.close = t.price;
                    if t.price > last.high {
                        last.high = t.price;
                    }
                    if t.price < last.low || last.low == 0.0 {
                        last.low = t.price;
                    }
                    last.volume += t.qty.max(0.0);
                }
                pane.ws_trades.push(t);
                got_any = true;
            }
            if pane.ws_trades.len() > 5000 {
                let drop_n = pane.ws_trades.len() - 5000;
                pane.ws_trades.drain(0..drop_n);
            }
        }
        if got_any
            && !pane.ws_trades.is_empty()
            && pane.chart_type == dos::widgets::chart::ChartType::DeltaCluster
        {
            let real = dos::data::footprint::build_from_trades(
                &pane.ws_trades,
                pane.timeframe,
                pane.tick_size,
            );
            pane.footprint =
                dos::data::footprint::align_to_klines(&pane.data, &real, pane.timeframe);
        }
    }
}

/// Walk all panes and ensure each has the right WS state for the current
/// view/market/ticker.  Called every loop iteration; cheap on no-op.
type StartupMsg = (usize, &'static str, Result<Vec<Ohlc>, String>);

/// Fetch the startup universe on a worker thread, one symbol after another
/// (polite to the providers), reporting each result as it lands.
fn spawn_startup_fetch(
    tickers: Vec<&'static str>,
    market: MarketType,
    tf: Timeframe,
) -> Receiver<StartupMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (i, t) in tickers.into_iter().enumerate() {
            let result = match market {
                MarketType::Crypto => binance::fetch_klines(
                    &binance::binance_symbol(t),
                    tf.binance_interval(),
                    200,
                    dos::data::binance::Venue::Spot,
                )
                .map_err(|e| e.to_string()),
                _ => yahoo::fetch(
                    &market.yahoo_ticker(t),
                    tf.yahoo_range(),
                    tf.yahoo_interval(),
                )
                .map_err(|e| e.to_string()),
            };
            if tx.send((i, t, result)).is_err() {
                return;
            }
        }
    });
    rx
}

/// Apply whatever the startup fetch has delivered.  Results for a market the
/// user has since left are dropped.  The status line is only touched while it
/// still shows startup text, so errors and user actions are never clobbered.
fn drain_startup(app: &mut App) {
    let Some(rx) = app.startup_rx.as_ref() else {
        return;
    };
    let mut changed = false;
    let mut finished = false;
    loop {
        match rx.try_recv() {
            Ok((i, ticker, result)) => {
                app.startup_done += 1;
                changed = true;
                if let Ok(data) = result {
                    if app.market == app.startup_market
                        && app.symbols.get(i).is_some_and(|s| s.ticker == ticker)
                    {
                        app.symbols[i].data = data;
                        app.startup_ok += 1;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = true;
                break;
            }
        }
    }
    if finished || app.startup_done >= app.startup_total {
        app.startup_rx = None;
    }
    let untouched = app.status.starts_with("Fetching") || app.status.starts_with("PAPER");
    if changed && untouched {
        app.status = if app.startup_rx.is_none() {
            format!(
                "Ready.  {}/{} {} loaded  |  Mkt F3  Scr F4  Dsh F5  :cmd",
                app.startup_ok,
                app.startup_total,
                app.startup_market.name(),
            )
        } else {
            format!(
                "Fetching {}/{} {}...",
                app.startup_done,
                app.startup_total,
                app.startup_market.name()
            )
        };
    }
}

fn ensure_all_ws(app: &mut App) {
    let n = app.pane_states.len();
    for i in 0..n {
        ensure_ws_for_pane(app, i);
    }
}

/// Spawn / tear down the combined `<symbol>@bookTicker` subscription
/// depending on the current selection.  Cheap idempotent — call every
/// loop iteration.  Reconnects when symbol set or venue changes.
fn ensure_mini_ticker_ws(app: &mut App) {
    use dos::data::binance::Venue;
    // We subscribe to slot symbols only.  Open-position symbols without
    // an active slot are rare (would only happen if user removed a slot
    // mid-position) — those won't get live marks until reselected.
    let mut wanted_syms: Vec<String> = app.runtime.slots.iter().map(|s| s.symbol.clone()).collect();
    // Also subscribe the active chart pane so WS prices flow to the
    // Markets view chart in real time, even without an engine slot.
    if (app.pane_active as usize) < app.pane_states.len() {
        wanted_syms.push(dos::data::binance::binance_symbol(
            &app.pane_states[app.pane_active as usize].ticker,
        ));
    }
    wanted_syms.sort();
    wanted_syms.dedup();
    let want_futures = matches!(app.screener_venue, Venue::Futures);

    let venue_changed = app.mini_ticker_ws.is_some() && app.mini_ticker_futures != want_futures;
    let symbols_changed = app.mini_ticker_ws.is_some() && app.mini_ticker_symbols != wanted_syms;
    let dead = app.mini_ticker_ws.as_ref().is_some_and(|h| !h.is_alive());

    if (wanted_syms.is_empty() && app.mini_ticker_ws.is_some())
        || venue_changed
        || symbols_changed
        || dead
    {
        if let Some(h) = app.mini_ticker_ws.take() {
            let _ = h.stop_tx.send(());
            drop(h.thread);
        }
        app.mini_ticker_symbols.clear();
        app.mini_ticker_connected = false;
    }
    if !wanted_syms.is_empty() && app.mini_ticker_ws.is_none() {
        app.mini_ticker_ws = Some(dos::data::binance_ws::connect_book_tickers(
            wanted_syms.clone(),
            want_futures,
        ));
        app.mini_ticker_symbols = wanted_syms;
        app.mini_ticker_futures = want_futures;
    }
}

/// Drain the bookTicker WS channel and apply every Tick to the
/// runtime.  Also tracks Connected/Disconnected so the user can see
/// stream health.
fn drain_mini_ticker(app: &mut App) {
    use dos::data::binance_ws::MiniTickerMsg;
    let Some(handle) = app.mini_ticker_ws.as_ref() else {
        return;
    };
    let mut got_any = false;
    while let Ok(msg) = handle.rx.try_recv() {
        match msg {
            MiniTickerMsg::Connected => {
                app.mini_ticker_connected = true;
                app.runtime.ws_connected = true;
                app.runtime.ws_status = format!(
                    "connected ({} pair{})",
                    app.mini_ticker_symbols.len(),
                    if app.mini_ticker_symbols.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                );
                app.status = format!("WS connected: {} pair(s)", app.mini_ticker_symbols.len());
            }
            MiniTickerMsg::Disconnected(reason) => {
                app.mini_ticker_connected = false;
                app.runtime.ws_connected = false;
                app.runtime.ws_status = format!("disconnected: {reason}");
                app.status = format!("WS disconnected: {reason}");
            }
            MiniTickerMsg::Tick(sym, price, ts) => {
                app.runtime.on_price(&sym, price, ts);
                // Flow WS price to chart pane data in real time so the
                // Markets view shows live movement between REST refreshes.
                for pane in app.pane_states.iter_mut() {
                    let binance_sym = dos::data::binance::binance_symbol(&pane.ticker);
                    if binance_sym == sym {
                        if let Some(last) = pane.data.last_mut() {
                            last.close = price;
                            if price > last.high {
                                last.high = price;
                            }
                            if price < last.low || last.low == 0.0 {
                                last.low = price;
                            }
                        }
                    }
                }
                got_any = true;
            }
        }
    }
    if got_any {
        app.last_mini_ticker_at = Instant::now();
    }
}

/// Connect or disconnect the `!bookTicker` WS based on whether the
/// Screener LIVE view is active.  When active, a single connection delivers
/// all-symbols book-ticker data for real-time mid-price updates.
fn ensure_mini_ticker_arr_ws(app: &mut App) {
    use dos::data::binance::Venue;
    let on_screener = app.view == View::Screener && app.screener_live;
    if on_screener {
        let want_futures = matches!(app.screener_venue, Venue::Futures);
        // Reconnect when thread died, venue changed, or connection went stale.
        let thread_dead = app
            .mini_ticker_arr_ws
            .as_ref()
            .is_none_or(|h| !h.is_alive());
        let venue_changed =
            app.mini_ticker_arr_ws.is_some() && app.mini_ticker_arr_futures != want_futures;
        let stale = !thread_dead
            && !venue_changed
            && app.mini_ticker_arr_connected
            && app.last_screener_tick_at.elapsed() >= Duration::from_secs(30);
        if app.mini_ticker_arr_ws.is_none() || thread_dead || venue_changed || stale {
            if let Some(ws) = app.mini_ticker_arr_ws.take() {
                let _ = ws.stop_tx.send(());
            }
            app.mini_ticker_arr_connected = false;
            app.mini_ticker_arr_futures = want_futures;
            let reason = if stale {
                "stale".to_string()
            } else if venue_changed {
                app.screener_venue.name().to_string()
            } else {
                "connecting".to_string()
            };
            app.status = format!("WS screener {reason}...");
            app.last_screener_tick_at = Instant::now();
            app.mini_ticker_arr_ws = Some(dos::data::binance_ws::connect_all_book_tickers(
                want_futures,
            ));
        }
    } else if let Some(ws) = app.mini_ticker_arr_ws.take() {
        let _ = ws.stop_tx.send(());
        app.mini_ticker_arr_connected = false;
    }
}

/// Drain the Screener's `!bookTicker` WS and update `live_pairs` mid-prices
/// in real time.  Each Tick carries the latest mid-price for one USDT pair
/// (pushed as Binance's order books update, often sub-second).  Only the
/// `last_price` field is touched — change/high/low/volume come from REST.
fn drain_mini_ticker_arr(app: &mut App) {
    use dos::data::binance_ws::MiniTickerMsg;
    let Some(handle) = app.mini_ticker_arr_ws.as_ref() else {
        return;
    };
    let mut got_any = false;
    let mut tick_count = 0u32;
    while let Ok(msg) = handle.rx.try_recv() {
        match msg {
            MiniTickerMsg::Connected => {
                app.mini_ticker_arr_connected = true;
                app.status = "WS screener connected".into();
            }
            MiniTickerMsg::Disconnected(reason) => {
                app.mini_ticker_arr_connected = false;
                app.status = format!("WS screener: {reason}");
            }
            MiniTickerMsg::Tick(sym, price, _ts) => {
                // Only update `last_price` — REST provides high/low/volume/change.
                if let Some(pair) = app.live_pairs.iter_mut().find(|p| p.symbol == sym) {
                    pair.last_price = price;
                    got_any = true;
                    tick_count += 1;
                }
            }
        }
    }
    if got_any {
        app.live_pairs_dirty = true;
        // Periodic status update so user can verify the WS is alive
        let now = Instant::now();
        app.last_screener_tick_at = now;
        if app.last_mini_ticker_at.elapsed() >= Duration::from_secs(3) {
            app.status = format!(
                "WS ticks: {tick_count} this frame, pairs: {}",
                app.live_pairs.len()
            );
            app.last_mini_ticker_at = now;
        }
    }
}

/// Ensure OrderBook view's WS connections (depth + aggTrade) are open and
/// subscribed to the current `ob_ticker`.  Closes them when leaving the view.
fn ensure_orderbook_ws(app: &mut App) {
    let want = app.view == View::OrderBook
        && app.market == MarketType::Crypto
        && !app.ob_ticker.is_empty();
    let target = if want {
        dos::data::binance::binance_symbol(&app.ob_ticker)
    } else {
        String::new()
    };
    let symbol_match = app.ob_ws_symbol == target;
    let has_any = app.ob_diff_ws.is_some() || app.ob_trade_ws.is_some();
    // A dead worker (not merely a reconnecting one) must be replaced, not kept.
    let alive = app.ob_diff_ws.as_ref().is_some_and(|h| h.is_alive())
        && app.ob_trade_ws.as_ref().is_some_and(|h| h.is_alive());
    let needs_close = has_any && (!want || !symbol_match || !alive);
    let needs_open = want && (!has_any || !symbol_match || !alive);

    if needs_close {
        if let Some(h) = app.ob_diff_ws.take() {
            let _ = h.stop_tx.send(());
            drop(h.thread);
        }
        if let Some(h) = app.ob_trade_ws.take() {
            let _ = h.stop_tx.send(());
            drop(h.thread);
        }
        app.ob_ws_symbol.clear();
        app.ob_local_book = None;
        app.ob_diff_buffer.clear();
        app.ob_pending_seed = None;
        app.ob_seed_wanted = false;
        if !want {
            app.ob_snapshot = None;
            app.ob_trades.clear();
        }
    }
    if needs_open {
        let venue_is_futures = matches!(app.ob_venue, dos::data::binance::Venue::Futures);
        // Open WS diff stream FIRST so we don't miss events while REST
        // seed is in flight.  Apply buffered events after seed lands.
        app.ob_diff_ws = Some(dos::data::binance_ws::connect_depth_diff(
            &target,
            venue_is_futures,
        ));
        app.ob_trade_ws = Some(dos::data::binance_ws::connect_agg_trades(
            &target,
            venue_is_futures,
        ));
        // Seed after the first diff is buffered (see `ob_ws_opened_at`).
        app.ob_pending_seed = None;
        app.ob_seed_wanted = true;
        app.ob_ws_opened_at = Instant::now();
        app.ob_seed_delay_secs = 1;
        app.ob_next_seed_at = Instant::now();
        app.ob_synced_at = Instant::now();
        if app.ob_flipped_for.as_deref() != Some(target.as_str()) {
            app.ob_flipped_for = None;
        }
        app.ob_ws_symbol = target;
        app.ob_diff_buffer.clear();
    }
}

/// Flip OrderBook venue (Spot ↔ Futures), clearing book state so
/// `ensure_orderbook_ws` reconnects WS + REST seed on the next cycle.
fn flip_ob_venue(app: &mut App) {
    app.ob_flipped_for = Some(app.ob_ws_symbol.clone());
    app.ob_seed_wanted = false;
    app.ob_venue = match app.ob_venue {
        dos::data::binance::Venue::Spot => dos::data::binance::Venue::Futures,
        dos::data::binance::Venue::Futures => dos::data::binance::Venue::Spot,
    };
    app.ob_snapshot = None;
    app.ob_local_book = None;
    app.ob_diff_buffer.clear();
    app.ob_pending_seed = None;
    if let Some(h) = app.ob_diff_ws.take() {
        let _ = h.stop_tx.send(());
    }
    if let Some(h) = app.ob_trade_ws.take() {
        let _ = h.stop_tx.send(());
    }
    app.ob_ws_symbol.clear();
}

/// Spawn a one-shot REST snapshot fetch (1000 levels) for the order-book
/// seed.  Returns the receiver the caller stores in `app.ob_pending_seed`.
fn spawn_ob_seed(
    symbol: &str,
    venue: dos::data::binance::Venue,
) -> Receiver<Result<(dos::data::orderbook::OrderBookSnapshot, u64), String>> {
    let (tx, rx) = mpsc::channel();
    let sym = symbol.to_string();
    std::thread::spawn(move || {
        let r =
            dos::data::binance::fetch_depth_with_id(&sym, 1000, venue).map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    rx
}

/// The book lost sequence: schedule a re-seed, backing off exponentially
/// (1 → 2 → 4 … 30 s) so a flapping stream cannot hammer the REST API
/// (weight 10 per snapshot).  A book that stayed contiguous for 10 s resets
/// the backoff.
fn request_ob_reseed(app: &mut App) {
    app.ob_seed_delay_secs = if app.ob_synced_at.elapsed() > Duration::from_secs(10) {
        1
    } else {
        (app.ob_seed_delay_secs * 2).min(30)
    };
    app.ob_next_seed_at = Instant::now() + Duration::from_secs(app.ob_seed_delay_secs);
    app.ob_seed_wanted = true;
    app.ob_local_book = None;
    app.status = format!("OB sync gap — re-seeding in {}s", app.ob_seed_delay_secs);
}

/// Cap the seed-catch-up buffer so a stuck connection can't grow it without
/// bound.  Keeps the most recent 5000 events.
fn cap_ob_buffer(buf: &mut Vec<dos::data::orderbook::DepthDiffEvent>) {
    if buf.len() > 5000 {
        let drop_n = buf.len() - 5000;
        buf.drain(0..drop_n);
    }
}

/// Drain depth + trade channels for the OrderBook view.  Cheap when idle.
fn drain_orderbook_ws(app: &mut App) {
    // 1. Drain incoming diff events.
    let mut diffs: Vec<dos::data::orderbook::DepthDiffEvent> = Vec::new();
    if let Some(h) = app.ob_diff_ws.as_ref() {
        while let Ok(d) = h.rx.try_recv() {
            diffs.push(d);
        }
    }

    // 2. Check if REST seed has landed.
    let mut seed_arrived: Option<(dos::data::orderbook::OrderBookSnapshot, u64)> = None;
    if let Some(rx) = app.ob_pending_seed.as_ref() {
        match rx.try_recv() {
            Ok(Ok((snap, last_id))) => seed_arrived = Some((snap, last_id)),
            Ok(Err(e)) => {
                if e.contains("Invalid symbol")
                    && app.ob_flipped_for.as_deref() != Some(app.ob_ws_symbol.as_str())
                {
                    // Flip Spot↔Futures once per symbol, never in a loop.
                    flip_ob_venue(app);
                    app.status = format!(
                        "OB symbol not on the previous venue — trying {}",
                        app.ob_venue.name()
                    );
                } else {
                    app.status = format!("OB seed failed: {}", e);
                    app.ob_pending_seed = None;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(_) => app.ob_pending_seed = None,
        }
    }

    // 3. If seed landed, build LocalBook + apply buffered diffs to catch up.
    //    On gap, RETAIN buffered events for the next re-seed attempt — clearing
    //    them caused the loop to never converge when the WS handshake finished
    //    after the REST snapshot was taken.
    if let Some((snap, last_id)) = seed_arrived {
        app.ob_pending_seed = None;
        let mut book = dos::data::orderbook::LocalBook::from_snapshot(&snap, last_id);
        let backlog: Vec<dos::data::orderbook::DepthDiffEvent> =
            std::mem::take(&mut app.ob_diff_buffer);
        let venue = app.ob_venue;
        let gap_at = backlog
            .iter()
            .position(|d| book.apply_checked(d, venue).is_err());
        if let Some(idx) = gap_at {
            // Keep the un-applied events (and this iteration's `diffs`) for
            // the next seed: as the buffer grows, a later snapshot's
            // lastUpdateId will fall inside one event's [U..u] and converge.
            app.ob_diff_buffer.extend(backlog.into_iter().skip(idx));
            app.ob_diff_buffer.append(&mut diffs);
            cap_ob_buffer(&mut app.ob_diff_buffer);
            request_ob_reseed(app);
            return;
        }
        app.ob_local_book = Some(book);
        app.ob_synced_at = Instant::now();
        app.ob_last_snapshot_at = Instant::now();
    }

    // 4. Apply / buffer live diffs.
    let venue = app.ob_venue;
    if let Some(book) = app.ob_local_book.as_mut() {
        let gap_at = diffs
            .iter()
            .position(|d| book.apply_checked(d, venue).is_err());
        if let Some(idx) = gap_at {
            // Save the gap-causing event + everything after it for the next
            // seed catch-up, instead of dropping them.
            let kept: Vec<_> = diffs.drain(idx..).collect();
            app.ob_diff_buffer.extend(kept);
            cap_ob_buffer(&mut app.ob_diff_buffer);
            request_ob_reseed(app);
        } else {
            app.ob_last_snapshot_at = Instant::now();
        }
    } else {
        for d in diffs {
            app.ob_diff_buffer.push(d);
        }
        cap_ob_buffer(&mut app.ob_diff_buffer);
    }

    // 4b. A re-seed waiting for its backoff slot.
    let stream_has_data =
        !app.ob_diff_buffer.is_empty() || app.ob_ws_opened_at.elapsed() > Duration::from_secs(3);
    if app.ob_seed_wanted
        && app.ob_pending_seed.is_none()
        && Instant::now() >= app.ob_next_seed_at
        && !app.ob_ws_symbol.is_empty()
        && stream_has_data
    {
        app.ob_pending_seed = Some(spawn_ob_seed(&app.ob_ws_symbol, app.ob_venue));
        app.ob_seed_wanted = false;
    }

    // 5. Materialise top-200 (each side) snapshot for rendering.
    if let Some(book) = app.ob_local_book.as_ref() {
        app.ob_snapshot = Some(book.snapshot_top(200));
    }
    if let Some(h) = app.ob_trade_ws.as_ref() {
        let mut last_price: Option<f64> = None;
        while let Ok(t) = h.rx.try_recv() {
            last_price = Some(t.price);
            app.ob_trades.push_back(t);
            while app.ob_trades.len() > 50 {
                app.ob_trades.pop_front();
            }
        }
        if let Some(p) = last_price {
            app.ob_last_trade_price = Some(p);
            app.ob_last_trade_at = Instant::now();
        }
    }
}

fn refresh_footprint(app: &mut App, i: usize) {
    if app.market != MarketType::Crypto {
        return;
    }
    let pane = match app.pane_states.get(i) {
        Some(p) => p.clone(),
        None => return,
    };
    if pane.ticker.is_empty() {
        return;
    }
    if app
        .pending_footprints
        .iter()
        .any(|f| f.pane_idx == i && f.ticker == pane.ticker && f.tf == pane.timeframe)
    {
        return;
    }
    let venue = app.screener_venue;
    let symbol = dos::data::binance::binance_symbol(&pane.ticker);
    let (tx, rx) = mpsc::channel();
    let symbol_thread = symbol.clone();
    std::thread::spawn(move || {
        let trades = match dos::data::binance::fetch_agg_trades(&symbol_thread, 1000, venue) {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };
        let tick_size =
            dos::data::binance::fetch_tick_size(&symbol_thread, venue).unwrap_or_else(|_| {
                let last_price = trades.last().map(|t| t.price).unwrap_or(0.0);
                dos::data::binance::heuristic_tick_size(last_price)
            });
        let _ = tx.send(Ok((trades, tick_size)));
    });
    app.pending_footprints.push(PendingFootprint {
        pane_idx: i,
        ticker: pane.ticker.clone(),
        tf: pane.timeframe,
        rx,
    });
}

fn poll_pending_footprints(app: &mut App) {
    let mut i = app.pending_footprints.len();
    while i > 0 {
        i -= 1;
        let drain = match app.pending_footprints[i].rx.try_recv() {
            Ok(Ok((trades, tick_size))) => {
                let pane_idx = app.pending_footprints[i].pane_idx;
                let ticker = app.pending_footprints[i].ticker.clone();
                let req_tf = app.pending_footprints[i].tf;
                // Drop trades fetched for a ticker / timeframe the pane no
                // longer shows (audit A4).
                let still_wanted = app
                    .pane_states
                    .get(pane_idx)
                    .is_some_and(|p| p.ticker == ticker && p.timeframe == req_tf);
                if !still_wanted {
                    app.pending_footprints.swap_remove(i);
                    continue;
                }
                if let Some(p) = app.pane_states.get_mut(pane_idx) {
                    // Seed the rolling WS buffer with REST history so the
                    // footprint chart has data immediately, before any WS
                    // ticks land.  WS will append fresh trades on top.
                    p.ws_trades = trades.clone();
                    let real =
                        dos::data::footprint::build_from_trades(&trades, p.timeframe, tick_size);
                    let aligned =
                        dos::data::footprint::align_to_klines(&p.data, &real, p.timeframe);
                    p.footprint = aligned;
                    p.tick_size = tick_size;
                    app.status = format!(
                        "{}: footprint seeded ({} trades, tick {}) — live via WS",
                        ticker,
                        trades.len(),
                        tick_size
                    );
                }
                // Open the WS connection now that we have tick_size.
                ensure_ws_for_pane(app, pane_idx);
                true
            }
            Ok(Err(e)) => {
                let ticker = app.pending_footprints[i].ticker.clone();
                app.status = format!("{}: footprint fetch failed ({})", ticker, e);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => true,
        };
        if drain {
            app.pending_footprints.swap_remove(i);
        }
    }
}

/// Schedule a background fetch for pane `i` using the pane's own ticker
/// and timeframe.  Result lands in pane_states[i].data via poll_pending.
fn refresh_pane(app: &mut App, i: usize) {
    let pane = match app.pane_states.get(i) {
        Some(p) => p.clone(),
        None => return,
    };
    let ticker = pane.ticker.clone();
    // Don't double-queue the SAME request for a pane; an in-flight request
    // for another ticker / timeframe must not block the one we need now.
    if app.pending_fetches.iter().any(|f| {
        f.pane_idx == i
            && f.ticker == ticker
            && f.tf == pane.timeframe
            && f.venue == app.screener_venue
    }) {
        return;
    }
    if ticker.is_empty() {
        return;
    }
    let market = app.market;
    let venue = app.screener_venue;
    let tf = pane.timeframe;
    // Instant first paint: show the cached series while the fetch is in flight.
    if pane.data.is_empty() {
        if let Some((cached, _)) = app
            .db
            .as_ref()
            .and_then(|c| db::cache_load(c, &ticker, tf.name()))
        {
            if let Some(p) = app.pane_states.get_mut(i) {
                p.data = cached;
            }
        }
    }
    let limit = app.chart_history.clamp(50, 1000);
    let (tx, rx) = mpsc::channel();
    let ticker_thread = ticker.clone();
    let provider: &'static str = match market {
        MarketType::Crypto => "Binance",
        _ => "Yahoo",
    };

    std::thread::spawn(move || {
        let result = match market {
            MarketType::Crypto => {
                let symbol = binance::binance_symbol(&ticker_thread);
                let interval = tf.binance_interval();
                binance::fetch_klines(&symbol, interval, limit, venue).map_err(|e| e.to_string())
            }
            _ => {
                let yt = market.yahoo_ticker(&ticker_thread);
                yahoo::fetch(&yt, tf.yahoo_range(), tf.yahoo_interval()).map_err(|e| e.to_string())
            }
        };
        let _ = tx.send(result);
    });
    app.pending_fetches.push(PendingFetch {
        pane_idx: i,
        ticker: ticker.clone(),
        tf,
        venue,
        rx,
    });
    app.status = format!(
        "Fetching {} pane {} from {} ({})...",
        ticker,
        i + 1,
        provider,
        tf.name()
    );
}

/// Set the active pane's ticker (and clear data so we re-fetch).  When in
/// linked mode, propagate to every pane and schedule per-pane fetches.
/// Open a ticker/name in the Markets view, triggered from the Screener.
/// Unlike `set_active_ticker`, this resets the global history range and
/// does NOT touch `app.symbols` — screener pairs don't belong in the
/// startup-symbol list.
fn screener_open_markets(app: &mut App, ticker: &str, name: &str) {
    if app.market != MarketType::Crypto {
        // Arrow-key cycling walks `app.symbols`; it must be the crypto list,
        // not the stock list the user came from (it would fetch `AAPLUSDT`).
        app.symbols = market_symbols(MarketType::Crypto);
        app.sym_idx = 0;
    }
    app.market = MarketType::Crypto;
    app.chart_history_range = HistoryRange::All;
    app.view = View::Markets;
    app.status = format!("Open chart: {}", ticker);
    set_active_ticker(app, ticker, name, None);
}

fn set_active_ticker(app: &mut App, ticker: &str, name: &str, sym_idx: Option<usize>) {
    sync_active_pane(app);
    let active = app.pane_active as usize;
    let panes_to_refresh: Vec<usize> = if app.panes_linked {
        (0..app.pane_count as usize).collect()
    } else {
        vec![active]
    };
    for i in &panes_to_refresh {
        if let Some(p) = app.pane_states.get_mut(*i) {
            p.ticker = ticker.to_string();
            p.name = name.to_string();
            p.data = Vec::new();
            // Reset trade buffer + footprint on ticker change.
            p.ws_trades.clear();
            p.footprint.clear();
            p.date_from = None;
            p.date_to = None;
            if let Some(s) = sym_idx {
                p.sym_idx = s;
            }
        }
    }
    if let Some(s) = sym_idx {
        app.sym_idx = s;
    }
    for i in panes_to_refresh.clone() {
        refresh_pane(app, i);
    }
    // Reconnect WS to the new ticker (only does something when chart_type
    // is DeltaCluster).
    for i in panes_to_refresh {
        ensure_ws_for_pane(app, i);
    }
    if app.market == MarketType::Crypto {
        let full = dos::data::binance::binance_symbol(ticker);
        app.ob_ticker = full;
        app.ob_venue = app.screener_venue;
        app.ob_snapshot = None;
        app.ob_local_book = None;
        app.ob_diff_buffer.clear();
        app.ob_pending_seed = None;
        if let Some(h) = app.ob_diff_ws.take() {
            let _ = h.stop_tx.send(());
        }
        if let Some(h) = app.ob_trade_ws.take() {
            let _ = h.stop_tx.send(());
        }
        app.ob_ws_symbol.clear();
    }
}

/// Splice live 24h Binance prices into pane data + market sample list so
/// dashboard portfolio + chart panes reflect real-time market state.
fn update_symbols_from_live(app: &mut App) {
    use std::collections::HashMap;
    let map: HashMap<&str, &dos::data::binance::TickerSummary> = app
        .live_pairs
        .iter()
        .map(|p| (p.symbol.as_str(), p))
        .collect();
    for sym in app.symbols.iter_mut() {
        let key = format!("{}USDT", sym.ticker);
        if let Some(p) = map.get(key.as_str()) {
            if let Some(last) = sym.data.last_mut() {
                last.close = p.last_price;
                last.high = last.high.max(p.last_price);
                last.low = last.low.min(p.last_price);
            }
        }
    }
    for pane in app.pane_states.iter_mut() {
        let key = if pane.ticker.ends_with("USDT") {
            pane.ticker.clone()
        } else {
            format!("{}USDT", pane.ticker)
        };
        if let Some(p) = map.get(key.as_str()) {
            if let Some(last) = pane.data.last_mut() {
                last.close = p.last_price;
                last.high = last.high.max(p.last_price);
                last.low = last.low.min(p.last_price);
            }
        }
    }
}

fn request_live_pairs(app: &mut App, force: bool) {
    if app.pending_live.is_some() && !force {
        return;
    }
    let venue = app.screener_venue;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = dos::data::binance::fetch_24h_summary(venue).map_err(|e| e.to_string());
        let _ = tx.send(res);
    });
    app.pending_live = Some(rx);
    app.last_live_refresh = Instant::now();
}

fn poll_pending_live(app: &mut App) {
    let Some(rx) = app.pending_live.as_ref() else {
        return;
    };
    match rx.try_recv() {
        Ok(Ok(pairs)) => {
            let ws_alive = app.mini_ticker_arr_ws.is_some()
                && app.mini_ticker_arr_connected
                && app.last_screener_tick_at.elapsed() < Duration::from_secs(30);
            if ws_alive {
                // WS provides real-time prices; REST only syncs pair list.
                // Keep WS-updated fields intact, add new pairs, remove delisted.
                let incoming_syms: std::collections::HashSet<&str> =
                    pairs.iter().map(|p| p.symbol.as_str()).collect();
                app.live_pairs
                    .retain(|p| incoming_syms.contains(p.symbol.as_str()));
                let existing_syms: std::collections::HashSet<String> =
                    app.live_pairs.iter().map(|p| p.symbol.clone()).collect();
                for pair in pairs {
                    if !existing_syms.contains(&pair.symbol) {
                        app.live_pairs.push(pair);
                    }
                }
            } else {
                // WS is down or stale — fall back to REST-only so prices
                // keep updating until the stream recovers.
                app.live_pairs = pairs;
            }
            app.live_pairs_dirty = true;
            app.pending_live = None;
            app.status = format!(
                "Live pairs: {} ({})",
                app.live_pairs.len(),
                app.screener_venue.name()
            );
        }
        Ok(Err(e)) => {
            app.pending_live = None;
            app.status = format!("Live fetch failed: {}", e);
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => {
            app.pending_live = None;
        }
    }
}

fn poll_pending_fetch(app: &mut App) {
    let mut i = app.pending_fetches.len();
    let mut needs_footprint: Vec<usize> = Vec::new();
    let mut refetch: Vec<usize> = Vec::new();
    while i > 0 {
        i -= 1;
        let drain = match app.pending_fetches[i].rx.try_recv() {
            Ok(Ok(data)) => {
                let pane_idx = app.pending_fetches[i].pane_idx;
                let ticker = app.pending_fetches[i].ticker.clone();
                let pane_tf = app.pending_fetches[i].tf;
                let n = data.len();
                // The data is correct for (ticker, tf) as REQUESTED, so it is
                // always cached under that key — never under whatever the pane
                // shows now (audit A4).
                if let Some(c) = app.db.as_ref() {
                    let _ = db::cache_save(c, &ticker, pane_tf.name(), &data);
                }
                let still_wanted = app
                    .pane_states
                    .get(pane_idx)
                    .is_some_and(|p| p.ticker == ticker && p.timeframe == pane_tf)
                    && (app.market != MarketType::Crypto
                        || app.pending_fetches[i].venue == app.screener_venue);
                if !still_wanted {
                    // Pane moved on while we were fetching: discard, and fetch
                    // what it shows now.
                    app.pending_fetches.swap_remove(i);
                    refetch.push(pane_idx);
                    continue;
                }
                let mut needs_fp = false;
                if let Some(p) = app.pane_states.get_mut(pane_idx) {
                    p.data = data.clone();
                    // Clamp chart_offset after data reload so scroll doesn't
                    // overflow into empty territory when the new batch is
                    // shorter than the old one.
                    p.chart_offset = p.chart_offset.min(p.data.len().saturating_sub(1));
                    if p.chart_type == dos::widgets::chart::ChartType::DeltaCluster {
                        needs_fp = true;
                    }
                }
                if needs_fp {
                    needs_footprint.push(pane_idx);
                }
                app.status = format!(
                    "{}: refreshed {} candles (pane {} @ {})",
                    ticker,
                    n,
                    pane_idx + 1,
                    pane_tf.name()
                );
                true
            }
            Ok(Err(e)) => {
                let ticker = app.pending_fetches[i].ticker.clone();
                app.status = format!("{}: fetch failed ({}) — keeping data", ticker, e);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => true,
        };
        if drain {
            app.pending_fetches.swap_remove(i);
        }
    }
    for idx in refetch {
        refresh_pane(app, idx);
    }
    for idx in needs_footprint {
        refresh_footprint(app, idx);
    }
}

/// Mirror top-level fields (timeframe / chart_type / zoom / agg / sym_idx)
/// down into the active pane.  Ticker / data live ON the pane and aren't
/// in top-level — they don't need syncing here.
fn sync_active_pane(app: &mut App) {
    let i = app.pane_active as usize;
    if let Some(p) = app.pane_states.get_mut(i) {
        p.timeframe = app.timeframe;
        p.chart_type = app.chart_type;
        p.chart_zoom = app.chart_zoom;
        p.chart_aggregate = app.chart_aggregate;
        p.sym_idx = app.sym_idx;
    }
}

/// Load pane[i]'s scalar settings into top-level so global key handlers
/// edit it.  Ticker / data remain in pane_states[i].
fn apply_pane_state_from(app: &mut App, i: usize) {
    if let Some(p) = app.pane_states.get(i) {
        app.timeframe = p.timeframe;
        app.chart_type = p.chart_type;
        app.chart_zoom = p.chart_zoom;
        app.chart_aggregate = p.chart_aggregate;
        app.sym_idx = p.sym_idx.min(app.symbols.len().saturating_sub(1));
    }
}

fn toggle_link(app: &mut App) {
    app.panes_linked = !app.panes_linked;
    if app.panes_linked && app.pane_count > 1 {
        // Snap all panes to the active pane's ticker; each refetches at
        // its own TF.
        sync_active_pane(app);
        let active = app.pane_active as usize;
        let (ticker, name, sym_idx) = {
            let Some(p) = app.pane_states.get(active) else {
                return;
            };
            (p.ticker.clone(), p.name.clone(), p.sym_idx)
        };
        for (i, p) in app.pane_states.iter_mut().enumerate() {
            if i != active {
                p.ticker = ticker.clone();
                p.name = name.clone();
                p.sym_idx = sym_idx;
                p.data.clear();
            }
        }
        for i in 0..app.pane_count as usize {
            if i != active {
                refresh_pane(app, i);
            }
        }
        app.status = format!("Panes LINKED to {} (refetching at each TF)", ticker);
    } else if app.panes_linked {
        app.status = "Panes LINKED (shared ticker)".into();
    } else {
        app.status = "Panes UNLINKED (each pane independent)".into();
    }
}

fn set_pane_count(app: &mut App, target: u8) {
    let target = target.clamp(1, 3);
    while app.pane_count < target {
        add_pane(app);
    }
    while app.pane_count > target {
        remove_pane(app);
    }
    app.status = format!(
        "panes: {} ({})",
        app.pane_count,
        if app.panes_linked {
            "linked"
        } else {
            "unlinked"
        }
    );
}

fn add_pane(app: &mut App) {
    if app.pane_count >= 3 {
        app.status = "panes: max 3 (use [ to remove)".into();
        return;
    }
    sync_active_pane(app);
    let Some(clone) = app.pane_states.get(app.pane_active as usize).cloned() else {
        return;
    };
    app.pane_states.push(clone);
    app.pane_count += 1;
    app.status = format!(
        "panes: {} ({}, Tab cycles, L toggles link)",
        app.pane_count,
        if app.panes_linked {
            "linked"
        } else {
            "unlinked"
        }
    );
}

fn remove_pane(app: &mut App) {
    if app.pane_count <= 1 {
        app.status = "panes: min 1".into();
        return;
    }
    app.pane_states.pop();
    app.pane_count -= 1;
    if app.pane_active as usize >= app.pane_count as usize {
        app.pane_active = 0;
        apply_pane_state_from(app, 0);
    }
    app.status = format!("panes: {}", app.pane_count);
}

fn cycle_active_pane(app: &mut App) {
    let total = app.pane_count.max(1);
    let next = (app.pane_active + 1) % total;
    if next == app.pane_active {
        return;
    }
    sync_active_pane(app);
    app.pane_active = next;
    apply_pane_state_from(app, next as usize);
    app.status = format!("active pane: {}/{}", app.pane_active + 1, app.pane_count);
}

fn screener_total(app: &App) -> usize {
    if app.screener_live {
        screener::rank_live(
            &app.live_pairs,
            1000,
            app.screener_sort,
            app.screener_min_qv,
            app.screener_sort_asc,
        )
        .len()
    } else {
        screener::top_gainers_sorted(&app.symbols, 10, app.screener_sort, app.screener_sort_asc)
            .len()
    }
}

fn next_ob_zoom_in(z: u32) -> u32 {
    match z {
        0 | 1 => 5,
        5 => 10,
        10 => 50,
        50 => 100,
        _ => 100,
    }
}

fn next_ob_zoom_out(z: u32) -> u32 {
    match z {
        100 => 50,
        50 => 10,
        10 => 5,
        5 => 1,
        _ => 1,
    }
}

fn next_timeframe(tf: Timeframe) -> Timeframe {
    match tf {
        Timeframe::S15 => Timeframe::M1,
        Timeframe::M1 => Timeframe::M5,
        Timeframe::M5 => Timeframe::H1,
        Timeframe::H1 => Timeframe::D1,
        Timeframe::D1 => Timeframe::S15,
    }
}

// ───────────────────────── top-menu dropdowns ─────────────────────────

struct MenuDropdownLayout {
    area: Rect,
    item_rects: Vec<Rect>,
}

fn current_dropdown_items(app: &App) -> Vec<&'static str> {
    let menu_idx = match app.menu_open {
        Some(i) => i,
        None => return Vec::new(),
    };
    let entry = match menu_for(app.view).get(menu_idx) {
        Some(e) => e,
        None => return Vec::new(),
    };
    match (app.view, entry.title) {
        (View::Markets, "Symbol") => vec!["Prev (<-)", "Next (->)", "Manual ticker (t)"],
        (View::Markets, "Chart") => {
            vec![
                "Candles",
                "Line",
                "Bars (OHLC)",
                "Footprint (delta cluster)",
                "Refresh trades (footprint)",
            ]
        }
        (View::Markets, "Timeframe") => vec!["15s", "1m", "5m", "1h", "1d"],
        (View::Markets, "History") => {
            let mut items = vec!["100 candles", "200 candles", "500 candles", "1000 candles"];
            // Show a marker on the active range filter, so the user can both
            // see the current state AND toggle back to "All history".
            match app.chart_history_range {
                HistoryRange::All => {
                    items.push("  All history  ◄");
                    items.push("  Today only");
                    items.push("  Last 3.5 days");
                }
                HistoryRange::Today => {
                    items.push("  All history");
                    items.push("  Today only  ◄");
                    items.push("  Last 3.5 days");
                }
                HistoryRange::Last3_5Days => {
                    items.push("  All history");
                    items.push("  Today only");
                    items.push("  Last 3.5 days  ◄");
                }
            }
            items
        }
        (View::Markets, "Panes") => {
            let link_label = if app.panes_linked {
                "Unlink panes"
            } else {
                "Link panes (share symbol)"
            };
            vec![
                "1 pane",
                "2 panes",
                "3 panes",
                "Cycle active (Tab)",
                link_label,
            ]
        }
        (View::Screener, "Sort") => vec!["24h Range %", "24h Change %", "24h Volume"],
        (View::Screener, "Filter") => vec![
            "All",
            "Min 50K USDT",
            "Min 500K USDT",
            "Min 5M USDT",
            "Min 50M USDT",
        ],
        (View::Screener, "Open") => vec!["Open selected (Enter)"],
        (View::Dashboard, "Refresh") => vec!["Refresh prices"],
        (View::OrderBook, "Symbol") => vec!["Manual ticker (t)"],
        (View::OrderBook, "Zoom") => vec!["x1 native tick", "x5", "x10", "x50", "x100"],
        (View::OrderBook, "Reconnect") => vec!["Reconnect WS streams"],
        (View::Settings, "Market") => {
            vec!["US Stocks", "Crypto", "EU Stocks", "Forex", "Commodities"]
        }
        (View::Settings, "API") => vec!["Edit Key", "Edit Secret"],
        (View::Settings, "Cache") => vec!["Clear OHLC cache", "Reset paper ledger"],
        _ => vec!["(no actions)"],
    }
}

fn execute_menu_action(app: &mut App, menu_idx: usize, item_idx: usize) {
    // Close the dropdown BEFORE dispatching: actions may re-enter
    // `handle_key` (Screener → Open sends Enter), which must not see the
    // dropdown still open or it recurses forever (audit A2).  It also lets
    // an action open its own modal without it being wiped afterwards.
    app.modal = None;
    app.menu_open = None;
    let entry_title: &'static str = match menu_for(app.view).get(menu_idx) {
        Some(e) => e.title,
        None => return,
    };
    match (app.view, entry_title) {
        (View::Markets, "Symbol") => match item_idx {
            0 => {
                if app.sym_idx > 0 {
                    app.sym_idx -= 1;
                } else {
                    app.sym_idx = app.symbols.len() - 1;
                }
            }
            1 => {
                app.sym_idx = (app.sym_idx + 1) % app.symbols.len();
            }
            2 => {
                app.modal = Some(ModalKind::TickerInput);
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
            }
            _ => {}
        },
        (View::Markets, "Chart") => {
            use dos::widgets::chart::ChartType;
            match item_idx {
                0 => {
                    app.chart_type = ChartType::Candle;
                    app.status = "chart: Candles".into();
                }
                1 => {
                    app.chart_type = ChartType::Line;
                    app.status = "chart: Line".into();
                }
                2 => {
                    app.chart_type = ChartType::Bar;
                    app.status = "chart: Bars".into();
                }
                3 => {
                    app.chart_type = ChartType::DeltaCluster;
                    app.chart_zoom = app.chart_zoom.max(9);
                    let active = app.pane_active as usize;
                    if let Some(p) = app.pane_states.get_mut(active) {
                        p.chart_type = ChartType::DeltaCluster;
                        p.chart_zoom = app.chart_zoom;
                    }
                    refresh_footprint(app, active);
                    ensure_ws_for_pane(app, active);
                    app.status = "chart: Footprint (live via WS)".into();
                }
                4 => {
                    let active = app.pane_active as usize;
                    refresh_footprint(app, active);
                    app.status = "Refreshing footprint trades...".into();
                }
                _ => {}
            }
        }
        (View::Markets, "Timeframe") => {
            let tf = match item_idx {
                0 => Timeframe::S15,
                1 => Timeframe::M1,
                2 => Timeframe::M5,
                3 => Timeframe::H1,
                _ => Timeframe::D1,
            };
            app.timeframe = tf;
            let active = app.pane_active as usize;
            if let Some(p) = app.pane_states.get_mut(active) {
                p.timeframe = tf;
                p.data.clear();
            }
            app.settings = SettingsState::new(
                app.market,
                app.timeframe,
                &app.binance_key,
                &app.binance_secret,
                app.runtime.is_live(),
            );
            if let Some(c) = app.db.as_ref() {
                let _ = db::save_kv(c, "timeframe", app.timeframe.name());
            }
            app.status = format!("pane {} timeframe: {} (refetching)", active + 1, tf.name());
            refresh_active_symbol(app);
        }
        (View::Markets, "Panes") => match item_idx {
            0 => set_pane_count(app, 1),
            1 => set_pane_count(app, 2),
            2 => set_pane_count(app, 3),
            3 => cycle_active_pane(app),
            4 => toggle_link(app),
            _ => {}
        },
        (View::Markets, "History") => {
            match item_idx {
                0 => app.chart_history = 100,
                1 => app.chart_history = 200,
                2 => app.chart_history = 500,
                3 => app.chart_history = 1000,
                4 => app.chart_history_range = HistoryRange::All,
                5 => app.chart_history_range = HistoryRange::Today,
                6 => app.chart_history_range = HistoryRange::Last3_5Days,
                _ => {}
            }
            // Sync back to settings state so F9 Apply doesn't revert it.
            app.settings.chart_history_range = app.chart_history_range;
            if let Some(c) = app.db.as_ref() {
                let _ = db::save_kv(c, "chart_history", &app.chart_history.to_string());
                let range_key = match app.chart_history_range {
                    HistoryRange::All => "all",
                    HistoryRange::Today => "today",
                    HistoryRange::Last3_5Days => "last3_5",
                };
                let _ = db::save_kv(c, "chart_history_range", range_key);
            }
            let msg = match item_idx {
                0..=3 => format!("history: {} candles (re-fetching)", app.chart_history),
                4 => "chart range: all history (refetching)".into(),
                5 => "chart range: today only (refetching)".into(),
                6 => "chart range: last 3.5 days (refetching)".into(),
                _ => unreachable!(),
            };
            app.status = msg;
            // Re-fetch all panes so the filter finds matching data.
            for i in 0..app.pane_count as usize {
                refresh_pane(app, i);
            }
        }
        (View::Screener, "Sort") => {
            app.screener_sort = match item_idx {
                0 => screener::SortMode::Gap,
                1 => screener::SortMode::Change,
                2 => screener::SortMode::Volume,
                _ => screener::SortMode::Last,
            };
            app.screener_sort_asc = false;
            app.screener_sel = 0;
            app.status = format!("sort: {}", app.screener_sort.display_name(false));
        }
        (View::Screener, "Filter") => {
            app.screener_min_qv = match item_idx {
                0 => 0.0,
                1 => 50_000.0,
                2 => 500_000.0,
                3 => 5_000_000.0,
                _ => 50_000_000.0,
            };
            if !app.screener_live {
                app.screener_live = true;
                request_live_pairs(app, true);
            }
            app.screener_sel = 0;
            app.status = format!(
                "min volume: {} USDT/24h",
                screener::fmt_usdt(app.screener_min_qv)
            );
        }
        (View::Screener, "Open") => {
            handle_key(app, KeyCode::Enter);
        }
        (View::Dashboard, "Refresh") => {
            request_live_pairs(app, true);
            app.status = "Refreshing live data...".into();
        }
        (View::Settings, "API") => {
            app.settings.focused = match item_idx {
                0 => settings::MARKETS.len(),
                _ => settings::MARKETS.len() + 1,
            };
            app.status = if item_idx == 0 {
                "Type your Binance API key (then Tab to Secret, Apply when done)".into()
            } else {
                "Type your Binance API secret (then Apply)".into()
            };
        }
        (View::Settings, "Market") => {
            let m = match item_idx {
                0 => MarketType::UsStocks,
                1 => MarketType::Crypto,
                2 => MarketType::EuStocks,
                3 => MarketType::Forex,
                _ => MarketType::Commodities,
            };
            app.settings.selected_market = SETTINGS_MARKETS
                .iter()
                .position(|(_, mt)| *mt == m)
                .unwrap_or(0);
            apply_settings(app);
        }
        (View::Settings, "Cache") => match item_idx {
            0 => clear_cache_action(app),
            _ => reset_paper_ledger_action(app),
        },
        (View::OrderBook, "Symbol") => {
            app.modal = Some(ModalKind::TickerInput);
            app.ticker_input.text.clear();
            app.ticker_input.cursor = 0;
        }
        (View::OrderBook, "Zoom") => {
            app.ob_zoom = match item_idx {
                0 => 1,
                1 => 5,
                2 => 10,
                3 => 50,
                _ => 100,
            };
            app.status = format!("Order Book zoom: x{}", app.ob_zoom);
        }
        (View::OrderBook, "Reconnect") => {
            if let Some(h) = app.ob_diff_ws.take() {
                let _ = h.stop_tx.send(());
            }
            if let Some(h) = app.ob_trade_ws.take() {
                let _ = h.stop_tx.send(());
            }
            app.ob_ws_symbol.clear();
            app.ob_snapshot = None;
            app.ob_local_book = None;
            app.ob_diff_buffer.clear();
            app.ob_pending_seed = None;
            app.ob_trades.clear();
            app.status = "Order book: reconnecting…".into();
        }
        _ => {}
    }
}

fn render_menu_dropdown(
    frame: &mut ratatui::Frame,
    layouts: &Layouts,
    app: &App,
    palette: &dos::tokens::Palette,
) -> MenuDropdownLayout {
    use ratatui::style::{Modifier, Style};
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    let menu_idx = app.menu_open.unwrap_or(0);
    let entries = menu_for(app.view);
    // Compute x position of the open menu item (mirrors menu_bar layout).
    let mut x = layouts.menu.x.saturating_add(2);
    for (i, e) in entries.iter().enumerate() {
        if i == menu_idx {
            break;
        }
        x = x.saturating_add(e.title.chars().count() as u16 + 3);
    }
    let items = current_dropdown_items(app);
    let max_w = items.iter().map(|s| s.chars().count()).max().unwrap_or(8) as u16 + 4;
    let h = items.len() as u16 + 2;
    let area = Rect {
        x,
        y: layouts.menu.y + 1,
        width: max_w.max(12),
        height: h,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.accent).bg(palette.bg))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut item_rects = Vec::with_capacity(items.len());
    for (i, label) in items.iter().enumerate() {
        let yy = inner.y + i as u16;
        if yy >= inner.y + inner.height {
            break;
        }
        let r = Rect {
            x: inner.x,
            y: yy,
            width: inner.width,
            height: 1,
        };
        let selected = i == app.menu_item_sel;
        let style = if selected {
            Style::default()
                .fg(palette.cursor_fg)
                .bg(palette.cursor_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        let prefix = if selected { "> " } else { "  " };
        frame.render_widget(
            Paragraph::new(format!("{}{}", prefix, label)).style(style),
            r,
        );
        item_rects.push(r);
    }
    MenuDropdownLayout { area, item_rects }
}

struct TickerModalLayout {
    area: ratatui::layout::Rect,
}

fn render_ticker_modal(
    frame: &mut ratatui::Frame,
    screen: ratatui::layout::Rect,
    input: &dos::widgets::input::InputState,
    mouse: Option<(u16, u16)>,
    palette: &dos::tokens::Palette,
) -> TickerModalLayout {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    let total_w: u16 = 50;
    let total_h: u16 = 5;
    let x = screen.x + (screen.width.saturating_sub(total_w)) / 2;
    let y = screen.y + (screen.height.saturating_sub(total_h)) / 2;
    let area = ratatui::layout::Rect {
        x,
        y,
        width: total_w,
        height: total_h,
    };
    frame.render_widget(Clear, area);

    let close_btn_x = area.x + area.width.saturating_sub(5);
    let close_hovered =
        mouse.is_some_and(|(mx, my)| my == area.y && mx >= close_btn_x && mx < close_btn_x + 4);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            " Enter ticker ",
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Close [X] button in title bar
    let close_style = if close_hovered {
        Style::default()
            .fg(palette.error)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    frame.render_widget(
        Paragraph::new(Span::styled("[X]", close_style)),
        Rect {
            x: close_btn_x,
            y: area.y,
            width: 4,
            height: 1,
        },
    );

    let pad = ratatui::layout::Rect {
        x: inner.x + 2,
        y: inner.y + 1,
        width: inner.width.saturating_sub(4),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new("Ticker:").style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: pad.y,
            width: 8,
            height: 1,
        },
    );
    let input_rect = Rect {
        x: pad.x + 8,
        y: pad.y,
        width: pad.width.saturating_sub(8),
        height: 1,
    };
    dos::widgets::input::render(frame, input_rect, input, true, palette);

    TickerModalLayout { area }
}

struct CmdPaletteLayout {
    area: ratatui::layout::Rect,
}

fn render_command_palette(
    frame: &mut ratatui::Frame,
    screen: ratatui::layout::Rect,
    input: &dos::widgets::input::InputState,
    mouse: Option<(u16, u16)>,
    palette: &dos::tokens::Palette,
) -> CmdPaletteLayout {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    let total_w: u16 = 54;
    let total_h: u16 = 7;
    let x = screen.x + (screen.width.saturating_sub(total_w)) / 2;
    let y = screen.y + (screen.height.saturating_sub(total_h)) / 2;
    let area = ratatui::layout::Rect {
        x,
        y,
        width: total_w,
        height: total_h,
    };
    frame.render_widget(Clear, area);

    let close_btn_x = area.x + area.width.saturating_sub(5);
    let close_hovered =
        mouse.is_some_and(|(mx, my)| my == area.y && mx >= close_btn_x && mx < close_btn_x + 4);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.accent).bg(palette.bg))
        .title(Span::styled(
            " Command Palette ",
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Close [X] button in title bar
    let close_style = if close_hovered {
        Style::default()
            .fg(palette.error)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    frame.render_widget(
        Paragraph::new(Span::styled("[X]", close_style)),
        Rect {
            x: close_btn_x,
            y: area.y,
            width: 4,
            height: 1,
        },
    );

    let pad = ratatui::layout::Rect {
        x: inner.x + 2,
        y: inner.y + 1,
        width: inner.width.saturating_sub(4),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(">").style(Style::default().fg(palette.accent).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: pad.y,
            width: 2,
            height: 1,
        },
    );
    let input_rect = Rect {
        x: pad.x + 2,
        y: pad.y,
        width: pad.width.saturating_sub(2),
        height: 1,
    };
    dos::widgets::input::render(frame, input_rect, input, true, palette);

    // Hint line
    let hint_y = pad.y + 2;
    frame.render_widget(
        Paragraph::new(
            " Try: :help  :live  :paper  :quit  :screener  :markets  :dashboard  :book  :settings",
        )
        .style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: hint_y,
            width: pad.width,
            height: 1,
        },
    );

    CmdPaletteLayout { area }
}

fn submit_ticker(app: &mut App) {
    let raw = app.ticker_input.text.trim().to_uppercase();
    if raw.is_empty() {
        app.modal = None;
        return;
    }
    let name = format!("{} (manual)", raw);
    app.modal = None;
    app.ticker_input.text.clear();
    app.ticker_input.cursor = 0;

    // OrderBook view: change the depth+trades subscription target.
    if app.view == View::OrderBook {
        app.ob_ticker = raw.clone();
        app.ob_snapshot = None;
        app.ob_local_book = None;
        app.ob_diff_buffer.clear();
        app.ob_pending_seed = None;
        app.ob_trades.clear();
        if let Some(h) = app.ob_diff_ws.take() {
            let _ = h.stop_tx.send(());
        }
        if let Some(h) = app.ob_trade_ws.take() {
            let _ = h.stop_tx.send(());
        }
        app.ob_ws_symbol.clear();
        app.status = format!("Order Book: {} (subscribing…)", raw);
        return;
    }

    // Markets view: update active pane's ticker.
    let ticker_static: &'static str = Box::leak(raw.clone().into_boxed_str());
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    if let Some(slot) = app.symbols.get_mut(app.sym_idx) {
        *slot = Symbol {
            ticker: ticker_static,
            name: name_static,
            data: vec![],
        };
    }
    set_active_ticker(app, &raw, &name, Some(app.sym_idx));
}

/// Send an OSC 50 font hint with the requested point size.  xterm/urxvt
/// honor `xft:Monospace:size=N` and rebuild the cell grid; iTerm2,
/// Terminal.app, kitty and most modern emulators silently ignore it (the
/// user must zoom with Cmd/Ctrl ± there — the saved value is still useful
/// as a recorded preference).
fn send_font_size_hint(size: i32) {
    let mut out = std::io::stdout();
    let _ = std::io::Write::write_all(
        &mut out,
        format!("\x1B]50;xft:Monospace:size={}\x07", size).as_bytes(),
    );
    let _ = std::io::Write::flush(&mut out);
}

/// Current Unix time in milliseconds.  Used for relative-time filtering
/// ("Today only" → last 24h) so the chart always has data to show even
/// when today's daily candle hasn't closed yet.
/// Filters for one pane.  The global history range uses RELATIVE timestamps
/// (last 24h / 84h) so it always finds data even when today's candle has not
/// closed yet; "All" falls back to the pane's own date / session pills.
fn series_filters(range: HistoryRange, pane: &PaneState) -> markets::SeriesFilters<'_> {
    // Rounded to the minute so the cut-off is stable between frames (a moving
    // cut-off would defeat the view cache and gain nothing visible).
    let now = unix_ms_now() / 60_000 * 60_000;
    let (time_from_ms, date_from, date_to) = match range {
        HistoryRange::Today => (now - 24 * 60 * 60 * 1000, None, None),
        HistoryRange::Last3_5Days => (now - 84 * 60 * 60 * 1000, None, None),
        HistoryRange::All => (0, pane.date_from.as_deref(), pane.date_to.as_deref()),
    };
    markets::SeriesFilters {
        date_from,
        date_to,
        session: pane.session,
        time_from_ms,
    }
}

fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clear_cache_action(app: &mut App) {
    if let Some(c) = app.db.as_ref() {
        match db::clear_cache(c) {
            Ok(()) => {
                app.status = "OHLC cache cleared (settings, keys and ledger untouched).".into();
            }
            Err(e) => app.status = format!("clear cache failed: {}", e),
        }
    } else {
        app.status = "No DB available — nothing to clear.".into();
    }
    app.modal = None;
}

/// Erase the paper ledger.  Destructive, so it takes two activations within
/// 8 seconds: the first only arms and explains, the second executes.
fn reset_paper_ledger_action(app: &mut App) {
    app.modal = None;
    if app.runtime.is_live() {
        app.status = "Reset paper ledger is unavailable in LIVE mode.".into();
        return;
    }
    let armed = app.reset_confirm_until.is_some_and(|t| Instant::now() < t);
    if !armed {
        app.reset_confirm_until = Some(Instant::now() + Duration::from_secs(8));
        app.status = "Reset paper ledger? Choose it again within 8s to ERASE it.".into();
        return;
    }
    app.reset_confirm_until = None;
    match app.runtime.reset_paper() {
        Ok(()) => {
            app.last_persisted = None;
            if let Some(c) = app.db.as_ref() {
                let _ = db::reset_paper_ledger(c);
                snapshot_paper_state(&app.runtime, c, &mut app.last_persisted);
            }
            app.status = format!(
                "Paper ledger reset to ${:.0}.",
                dos::engine::PAPER_STARTING_CASH
            );
        }
        Err(e) => app.status = format!("Reset failed: {e}"),
    }
}

fn apply_settings(app: &mut App) {
    let new_market = SETTINGS_MARKETS[app.settings.selected_market].1;
    let new_tf = settings::TIMEFRAMES[app.settings.selected_tf].1;
    let new_key = app.settings.api_key.text.clone();
    let new_secret = app.settings.api_secret.text.clone();
    let new_fkey = app.settings.futures_api_key.text.clone();
    let new_fsecret = app.settings.futures_api_secret.text.clone();
    let new_ob_show_trades = app.settings.ob_show_trades;
    let new_chart_history_range = app.settings.chart_history_range;
    let new_font_size = app.settings.font_size;

    let market_changed = new_market != app.market;

    let mut messages: Vec<String> = vec![];

    // Step 1: Update all app-level fields FIRST so sync_active_pane and
    // any subsequent refresh_pane see the final consistent state.
    if market_changed {
        app.market = new_market;
        messages.push(format!("market={}", new_market.name()));
    }
    if new_tf != app.timeframe {
        app.timeframe = new_tf;
        messages.push(format!("tf={}", new_tf.name()));
    }
    if new_key != app.binance_key {
        app.binance_key = new_key.clone();
        app.key_check = None;
        messages.push("binance_key updated".into());
    }
    if new_secret != app.binance_secret {
        app.binance_secret = new_secret.clone();
        app.key_check = None;
        messages.push("binance_secret updated".into());
    }
    if new_fkey != app.binance_futures_key {
        app.binance_futures_key = new_fkey.clone();
        messages.push("binance_futures_key updated".into());
    }
    if new_fsecret != app.binance_futures_secret {
        app.binance_futures_secret = new_fsecret.clone();
        messages.push("binance_futures_secret updated".into());
    }
    if new_ob_show_trades != app.ob_show_trades {
        app.ob_show_trades = new_ob_show_trades;
        messages.push(format!(
            "ob_trades_feed={}",
            if new_ob_show_trades { "on" } else { "off" }
        ));
    }
    if new_chart_history_range != app.chart_history_range {
        app.chart_history_range = new_chart_history_range;
        messages.push(format!("history_range={}", app.chart_history_range.name()));
    }
    if new_font_size != app.font_size {
        app.font_size = new_font_size;
        send_font_size_hint(new_font_size);
        messages.push(format!("font_size={}", new_font_size));
    }

    // Step 2: Sync app-level scalars down to the active pane so that
    // refresh_pane (which reads pane.timeframe) uses the new values.
    sync_active_pane(app);

    // Step 3: Handle market change — reset pane symbol/data and trigger
    // a background fetch that now sees the correct market + timeframe.
    if market_changed {
        app.symbols = market_symbols(new_market);
        app.sym_idx = 0;
        app.chart_zoom = 1;
        if let Some(p) = app.pane_states.get_mut(app.pane_active as usize) {
            if let Some(sym) = app.symbols.first() {
                p.ticker = sym.ticker.to_string();
                p.name = sym.name.to_string();
            }
            p.data.clear();
            p.footprint.clear();
            p.chart_offset = 0;
        }
        refresh_pane(app, app.pane_active as usize);
        // Screener state is intentionally NOT reset here: the live screener
        // pulls Binance USDT pairs and is independent of the F9 "Market"
        // setting (which only affects what charts/tabs in Markets view
        // display).
    }

    if let Some(c) = app.db.as_ref() {
        let _ = db::save_kv(c, "market", new_market.name());
        let _ = db::save_kv(c, "timeframe", new_tf.name());
        // A value is stored only if it is NOT what the environment supplied:
        // env-provided keys never reach the database, even when the user edits
        // the other half of the pair in Settings.
        let env = |p: &Option<(String, String)>| {
            (
                p.as_ref().map(|x| x.0.clone()),
                p.as_ref().map(|x| x.1.clone()),
            )
        };
        let (ek, es) = env(&app.env_spot);
        let (efk, efs) = env(&app.env_futures);
        let keep = |name: &str, val: &str, from_env: &Option<String>| {
            if from_env.as_deref() != Some(val) {
                let _ = db::save_kv(c, name, val);
            }
        };
        keep("binance_api_key", &app.binance_key, &ek);
        keep("binance_api_secret", &app.binance_secret, &es);
        keep("binance_futures_api_key", &app.binance_futures_key, &efk);
        keep(
            "binance_futures_api_secret",
            &app.binance_futures_secret,
            &efs,
        );
        let _ = db::save_kv(
            c,
            "ob_show_trades",
            if app.ob_show_trades { "true" } else { "false" },
        );
        let _ = db::save_kv(
            c,
            "chart_history_range",
            match app.chart_history_range {
                HistoryRange::All => "all",
                HistoryRange::Today => "today",
                HistoryRange::Last3_5Days => "last3_5",
            },
        );
        let _ = db::save_kv(c, "font_size", &app.font_size.to_string());
    }

    app.status = if messages.is_empty() {
        "Settings unchanged.".into()
    } else {
        format!("Settings applied: {}", messages.join(", "))
    };
    app.modal = None;

    // Auto-refresh the active symbol for non-market changes (timeframe,
    // history range, etc.).  Market change already triggered a refresh
    // above, so pending_fetches is non-empty and this is skipped.
    if app.pending_fetches.is_empty() {
        refresh_active_symbol(app);
    }
}

/// Map mouse-x on the scrollbar track to a chart_offset for the active
/// pane.  The scrollbar represents the FULL series; click x → position
/// of the visible window's right edge; offset = total - end.
fn seek_scrollbar(app: &mut App, ml: &MarketsLayout, x: u16) {
    if ml.scrollbar_rect.width == 0 || ml.scroll_total <= 1 {
        return;
    }
    let active = app.pane_active as usize;
    let bar_x = ml.scrollbar_rect.x;
    let bar_w = ml.scrollbar_rect.width as f64;
    let rel = (x as i32 - bar_x as i32).max(0) as f64;
    let frac = (rel / bar_w).clamp(0.0, 1.0);
    let end_idx = (frac * ml.scroll_total as f64).round() as usize;
    let end_idx = end_idx.clamp(1, ml.scroll_total);
    if let Some(p) = app.pane_states.get_mut(active) {
        p.chart_offset = ml.scroll_total.saturating_sub(end_idx);
        app.status = format!("scroll: -{} / {}", p.chart_offset, ml.scroll_total);
    }
}

fn handle_mouse(
    app: &mut App,
    m: crossterm::event::MouseEvent,
    layouts: &Layouts,
    markets_layout: Option<&MarketsLayout>,
    screener_layout: Option<&ScreenerLayout>,
    settings_layout: Option<&SettingsLayout>,
    menu_dropdown_layout: Option<&MenuDropdownLayout>,
    calendar_layout: Option<&dos::widgets::calendar::CalendarLayout>,
    session_layout: Option<&dos::widgets::session_picker::SessionPickerLayout>,
    strategy_panel_layout: Option<&StrategyPanelLayout>,
    trade_ticket_layout: Option<&TradeTicketLayout>,
    last_help_layout: Option<&dos::widgets::help::HelpLayout>,
    last_cmd_layout: Option<&CmdPaletteLayout>,
    last_ticker_layout: Option<&TickerModalLayout>,
    last_live_confirm_layout: Option<&dos::widgets::modal::ModalLayout>,
) {
    let (x, y) = (m.column, m.row);

    // ── Mouse wheel on chart (Markets) — smooth horizontal scroll ──
    // Wheel up = view shifts LEFT (toward older candles).
    // Wheel down = view shifts RIGHT (toward present).
    // Step = 8 candles per notch — feels like normal "page-ish" scroll.
    //
    // Skip entirely while a filter / ticker / dropdown modal is up so
    // wheel events directed at the modal don't sneak through to the
    // chart underneath.
    let modal_blocks_chart = matches!(
        app.modal,
        Some(ModalKind::DatePickerFrom)
            | Some(ModalKind::DatePickerTo)
            | Some(ModalKind::SessionTime)
            | Some(ModalKind::TickerInput)
            | Some(ModalKind::MenuDropdown)
    );
    if app.view == View::Markets && !modal_blocks_chart {
        match m.kind {
            MouseEventKind::ScrollUp => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    // Clamp against the post-filter aggregated length so a
                    // tight date / session filter doesn't let the offset
                    // run past the visible end (which would render an
                    // empty chart and feel like "scroll broken").
                    let max_offset = markets_layout
                        .map(|ml| ml.scroll_total.saturating_sub(1))
                        .unwrap_or_else(|| p.data.len().saturating_sub(1));
                    p.chart_offset = (p.chart_offset + 8).min(max_offset);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
                return;
            }
            MouseEventKind::ScrollDown => {
                let active = app.pane_active as usize;
                if let Some(p) = app.pane_states.get_mut(active) {
                    p.chart_offset = p.chart_offset.saturating_sub(8);
                    app.status = format!("scroll: -{} candles", p.chart_offset);
                }
                return;
            }
            _ => {}
        }
    }

    // ── Sticky drag for the scrollbar ──
    // Down(Left) on scrollbar  → start drag, immediately seek.
    // Drag(Left) while dragging → keep seeking based on x (clamped to bar).
    // Up(Left)                  → end drag.
    //
    // Skip entirely while a filter modal is up — on small terminals the
    // calendar / session picker overlaps the scrollbar row, and we don't
    // want a click on a day cell to be eaten by the scrollbar handler.
    if app.view == View::Markets && !modal_blocks_chart {
        if let Some(ml) = markets_layout {
            // Start drag on click inside scrollbar.
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && rect_contains(ml.scrollbar_rect, x, y)
                && ml.scrollbar_rect.width > 0
                && ml.scroll_total > 1
            {
                let active = app.pane_active as usize;
                app.dragging_scrollbar = Some(active);
                seek_scrollbar(app, ml, x);
                return;
            }
        }
        // Continue drag if active.
        if matches!(m.kind, MouseEventKind::Drag(MouseButton::Left))
            && app.dragging_scrollbar.is_some()
        {
            if let Some(ml) = markets_layout {
                seek_scrollbar(app, ml, x);
            }
            return;
        }
        // End drag.
        if matches!(m.kind, MouseEventKind::Up(MouseButton::Left))
            && app.dragging_scrollbar.take().is_some()
        {
            return;
        }
    }

    if m.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }

    // Calendar modal — handle click before any view-level routing.
    if let (Some(cl), true) = (
        calendar_layout,
        matches!(
            app.modal,
            Some(ModalKind::DatePickerFrom) | Some(ModalKind::DatePickerTo)
        ),
    ) {
        let is_from = app.modal == Some(ModalKind::DatePickerFrom);
        if rect_contains(cl.prev_rect, x, y) {
            app.calendar.shift_month(-1);
            return;
        }
        if rect_contains(cl.next_rect, x, y) {
            app.calendar.shift_month(1);
            return;
        }
        if rect_contains(cl.today_rect, x, y) {
            app.calendar = dos::widgets::calendar::CalendarState::from_date(None);
            return;
        }
        if rect_contains(cl.clear_rect, x, y) {
            commit_date(app, is_from, None);
            app.modal = None;
            return;
        }
        if rect_contains(cl.cancel_rect, x, y) {
            app.modal = None;
            return;
        }
        if rect_contains(cl.apply_rect, x, y) {
            let iso = app.calendar.current_iso();
            commit_date(app, is_from, Some(iso));
            app.modal = None;
            return;
        }
        for (rect, day) in cl.day_rects.iter() {
            if rect_contains(*rect, x, y) {
                app.calendar.focus_day = *day;
                let iso = app.calendar.current_iso();
                commit_date(app, is_from, Some(iso));
                app.modal = None;
                return;
            }
        }
        // Click outside the modal area closes it.
        if !rect_contains(cl.area, x, y) {
            app.modal = None;
        }
        return;
    }

    // Session-time modal.
    if let (Some(sl), Some(ModalKind::SessionTime)) = (session_layout, app.modal) {
        if rect_contains(sl.start_minus, x, y) {
            app.session_picker.focus = 0;
            app.session_picker.nudge(-1);
            return;
        }
        if rect_contains(sl.start_plus, x, y) {
            app.session_picker.focus = 0;
            app.session_picker.nudge(1);
            return;
        }
        if rect_contains(sl.start_field, x, y) {
            app.session_picker.focus = 0;
            return;
        }
        if rect_contains(sl.end_minus, x, y) {
            app.session_picker.focus = 2;
            app.session_picker.nudge(-1);
            return;
        }
        if rect_contains(sl.end_plus, x, y) {
            app.session_picker.focus = 2;
            app.session_picker.nudge(1);
            return;
        }
        if rect_contains(sl.end_field, x, y) {
            app.session_picker.focus = 2;
            return;
        }
        if rect_contains(sl.preset_24, x, y) {
            app.session_picker.start_min = 0;
            app.session_picker.end_min = 1440;
            return;
        }
        if rect_contains(sl.preset_ny, x, y) {
            app.session_picker.start_min = 13 * 60 + 30;
            app.session_picker.end_min = 20 * 60;
            return;
        }
        if rect_contains(sl.preset_lon, x, y) {
            app.session_picker.start_min = 8 * 60;
            app.session_picker.end_min = 16 * 60 + 30;
            return;
        }
        if rect_contains(sl.off_rect, x, y) {
            commit_session(app, dos::markets::SessionWindow::ALL);
            app.modal = None;
            return;
        }
        if rect_contains(sl.cancel_rect, x, y) {
            app.modal = None;
            return;
        }
        if rect_contains(sl.apply_rect, x, y) {
            let w = app.session_picker.to_window();
            commit_session(app, w);
            app.modal = None;
            return;
        }
        if !rect_contains(sl.area, x, y) {
            app.modal = None;
        }
        return;
    }

    // Top menu dropdown — clicks on items execute, click outside closes.
    if app.modal == Some(ModalKind::MenuDropdown) {
        if let Some(dl) = menu_dropdown_layout {
            for (i, r) in dl.item_rects.iter().enumerate() {
                if rect_contains(*r, x, y) {
                    let menu_idx = app.menu_open.unwrap_or(0);
                    execute_menu_action(app, menu_idx, i);
                    return;
                }
            }
            if !rect_contains(dl.area, x, y) {
                app.modal = None;
                app.menu_open = None;
            }
        }
        return;
    }

    // ── Help modal: prev / next / close ──
    if app.modal == Some(ModalKind::Help) {
        if let Some(hl) = last_help_layout {
            if rect_contains(hl.prev_rect, x, y) {
                let total = help_page_count();
                app.help_page = if app.help_page > 0 {
                    app.help_page - 1
                } else {
                    total.saturating_sub(1)
                };
                return;
            }
            if rect_contains(hl.next_rect, x, y) {
                let total = help_page_count();
                app.help_page = (app.help_page + 1) % total.max(1);
                return;
            }
            if rect_contains(hl.close_rect, x, y) || !rect_contains(hl.area, x, y) {
                app.modal = None;
                return;
            }
        }
        return;
    }

    // ── Command palette: close [X] or click outside ──
    if app.modal == Some(ModalKind::CommandPalette) {
        if let Some(cl) = last_cmd_layout {
            // Check for close button at title right
            let close_x = cl.area.x + cl.area.width.saturating_sub(5);
            if x >= close_x && x < close_x + 4 && y == cl.area.y {
                app.modal = None;
                app.command_input.text.clear();
                app.command_input.cursor = 0;
                return;
            }
            if !rect_contains(cl.area, x, y) {
                app.modal = None;
                app.command_input.text.clear();
                app.command_input.cursor = 0;
                return;
            }
        }
        return;
    }

    // ── Ticker input: close [X] or click outside ──
    if app.modal == Some(ModalKind::TickerInput) {
        if let Some(tl) = last_ticker_layout {
            let close_x = tl.area.x + tl.area.width.saturating_sub(5);
            if x >= close_x && x < close_x + 4 && y == tl.area.y {
                app.modal = None;
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
                return;
            }
            if !rect_contains(tl.area, x, y) {
                app.modal = None;
                app.ticker_input.text.clear();
                app.ticker_input.cursor = 0;
                return;
            }
        }
        return;
    }

    // ── LiveConfirm: Yes / No buttons ──
    if app.modal == Some(ModalKind::LiveConfirm) {
        if let Some(ll) = last_live_confirm_layout {
            if ll
                .button_rects
                .first()
                .is_some_and(|r| rect_contains(*r, x, y))
            {
                confirm_live(app);
            } else if ll
                .button_rects
                .get(1)
                .is_some_and(|r| rect_contains(*r, x, y))
                || !rect_contains(ll.area, x, y)
            {
                cancel_live(app);
            }
        }
        return;
    }

    // Top menu bar — click on a menu title opens its dropdown.
    // Checked BEFORE the per-view click handlers so that clicking on the
    // top menu while on the F9 Settings page doesn't get treated as a
    // "click outside the page" → page-cancel by mistake.
    if rect_contains(layouts.menu, x, y) {
        let entries = menu_for(app.view);
        // Same x layout as menu_bar::render_with_hover: 2px left pad,
        // 3-space gutter between titles.
        let mut cur_x = layouts.menu.x.saturating_add(2);
        for (i, e) in entries.iter().enumerate() {
            let w = e.title.chars().count() as u16;
            let cell = Rect {
                x: cur_x,
                y: layouts.menu.y,
                width: w,
                height: 1,
            };
            if rect_contains(cell, x, y) {
                app.menu_open = Some(i);
                app.menu_item_sel = 0;
                app.modal = Some(ModalKind::MenuDropdown);
                return;
            }
            cur_x = cur_x.saturating_add(w + 3);
        }
    }

    // Settings page (View::Settings) — full-page form clicks.
    // Runs AFTER the menu-bar handler above, so menu-title clicks are
    // already handled and don't fall through into settings.
    if app.view == View::Settings {
        if let Some(sl) = settings_layout {
            // Ignore clicks that aren't inside the settings page rect —
            // those belong to the surrounding chrome (status line, F-bar
            // hover) and must not affect the page state.
            if !rect_contains(sl.area, x, y) {
                return;
            }
            // Record press-flash for the footer buttons before delegating.
            // Index order matches BUTTONS in settings.rs.
            let footer_btns = [sl.apply_rect, sl.clear_rect, sl.cancel_rect];
            for (i, r) in footer_btns.iter().enumerate() {
                if rect_contains(*r, x, y) {
                    app.last_press = Some((PressedBtnKind::Settings, i, Instant::now()));
                    break;
                }
            }
            match settings::handle_click(&mut app.settings, sl, x, y) {
                settings::SettingsAction::Cancel => {
                    app.view = View::Screener;
                }
                settings::SettingsAction::Apply => {
                    apply_settings(app);
                    sync_live_with_settings(app);
                }
                settings::SettingsAction::ClearCache => clear_cache_action(app),
                settings::SettingsAction::None => {}
            }
        }
        return;
    }

    // Screener view: click handling.  Order matters:
    //   1. checkbox cell    → toggle selection (do not open chart)
    //   2. trade ticket     → buy/sell buttons, qty stepper, strategy cycle
    //   3. selection list   → start/stop/remove + click body to set active
    //   4. screener row     → open Markets for that symbol
    if app.view == View::Screener {
        if let Some(sl) = screener_layout {
            for (rect, sym_idx) in sl.checkbox_rects.iter() {
                if rect_contains(*rect, x, y) {
                    toggle_screener_selection_by_sym_idx(app, *sym_idx);
                    return;
                }
            }
        }
        if let Some(tt) = trade_ticket_layout {
            for (action, rect) in tt.buttons.iter() {
                if rect_contains(*rect, x, y) {
                    submit_basket_action(app, *action);
                    return;
                }
            }
            if rect_contains(tt.qty_minus, x, y) {
                bump_ticket_value(app, -1);
                return;
            }
            if rect_contains(tt.qty_plus, x, y) {
                bump_ticket_value(app, 1);
                return;
            }
            if rect_contains(tt.size_mode_toggle, x, y) {
                app.size_mode = app.size_mode.flip();
                app.status = format!("size mode = {}", app.size_mode.label());
                return;
            }
            if rect_contains(tt.leverage_cycle, x, y) {
                app.leverage = trade_ticket::cycle_leverage(app.leverage, 1);
                app.status = format!("leverage = x{}", app.leverage);
                return;
            }
        }
        if let Some(sp) = strategy_panel_layout {
            for sr in sp.slots.iter() {
                // Layout is one frame old: the slot may be gone (audit A1).
                let Some(slot_idx) = app.runtime.slots.iter().position(|s| s.symbol == sr.symbol)
                else {
                    continue;
                };
                if rect_contains(sr.start, x, y) {
                    let now = current_now_ms();
                    let _ = app.runtime.start(slot_idx, now);
                    persist_slot(app, slot_idx);
                    app.status = format!("[{}] started", sr.symbol);
                    return;
                }
                if rect_contains(sr.stop, x, y) {
                    let now = current_now_ms();
                    let _ = app.runtime.stop(slot_idx, now);
                    persist_slot(app, slot_idx);
                    app.status = format!("[{}] stopped", sr.symbol);
                    return;
                }
                if rect_contains(sr.remove, x, y) {
                    remove_screener_selection(app, &sr.symbol);
                    app.status = format!("- {} removed", sr.symbol);
                    return;
                }
                if rect_contains(sr.edit, x, y) {
                    cycle_slot_strategy(app, slot_idx);
                    return;
                }
                if rect_contains(sr.body, x, y) {
                    app.active_slot = Some(slot_idx);
                    app.status = format!("active: {}", sr.symbol);
                    return;
                }
            }
        }
        if let Some(sl) = screener_layout {
            for (rect, sort_mode) in sl.header_rects.iter() {
                if rect_contains(*rect, x, y) {
                    if app.screener_sort == *sort_mode {
                        app.screener_sort_asc = !app.screener_sort_asc;
                    } else {
                        app.screener_sort = *sort_mode;
                        app.screener_sort_asc = false;
                    }
                    app.screener_sel = 0;
                    app.status = format!("Sort: {}", sort_mode.display_name(app.screener_sort_asc));
                    return;
                }
            }
        }
        if let Some(sl) = screener_layout {
            for (rect, idx) in sl.row_rects.iter() {
                if rect_contains(*rect, x, y) {
                    if app.screener_live {
                        if let Some(pair) = app.live_pairs.get(*idx).cloned() {
                            let base = pair.symbol.trim_end_matches("USDT").to_string();
                            let name =
                                format!("{} (Binance {})", pair.symbol, app.screener_venue.name());
                            screener_open_markets(app, &base, &name);
                        }
                    } else {
                        let Some(s) = app.symbols.get(*idx).cloned() else {
                            return;
                        };
                        app.chart_history_range = HistoryRange::All;
                        app.view = View::Markets;
                        app.status = format!("Markets: {} - Esc back", s.ticker);
                        set_active_ticker(app, s.ticker, s.name, Some(*idx));
                    }
                    return;
                }
            }
        }
    }

    // Markets view: click-on-pane to focus, click-on-tab to switch symbol.
    // (Scrollbar handled above in the wheel/drag branch.)
    if app.view == View::Markets {
        if let Some(ml) = markets_layout {
            // Date / session pills in the header take priority over pane clicks.
            if rect_contains(ml.from_date_rect, x, y) {
                let active = app.pane_active as usize;
                // Default the calendar to the current FROM filter when
                // set; otherwise to the first candle in the loaded
                // history so the user lands inside the data range
                // instead of on "today" (often outside the fetched
                // window, leaving an empty chart on click).
                let default = app.pane_states.get(active).and_then(|p| {
                    p.date_from
                        .clone()
                        .or_else(|| p.data.first().map(|c| c.date.clone()))
                });
                app.calendar = dos::widgets::calendar::CalendarState::from_date(default.as_deref());
                app.modal = Some(ModalKind::DatePickerFrom);
                app.status = "Pick FROM date — click a day, or [Clear] to reset".into();
                return;
            }
            if rect_contains(ml.to_date_rect, x, y) {
                let active = app.pane_active as usize;
                let default = app.pane_states.get(active).and_then(|p| {
                    p.date_to
                        .clone()
                        .or_else(|| p.data.last().map(|c| c.date.clone()))
                });
                app.calendar = dos::widgets::calendar::CalendarState::from_date(default.as_deref());
                app.modal = Some(ModalKind::DatePickerTo);
                app.status = "Pick TO date — click a day, or [Clear] to reset".into();
                return;
            }
            if rect_contains(ml.session_rect, x, y) {
                let active = app.pane_active as usize;
                let cur = app
                    .pane_states
                    .get(active)
                    .map(|p| p.session)
                    .unwrap_or(dos::markets::SessionWindow::ALL);
                app.session_picker =
                    dos::widgets::session_picker::SessionPickerState::from_window(cur);
                app.modal = Some(ModalKind::SessionTime);
                app.status = "Session window — adjust HH:MM, or [Off] to disable".into();
                return;
            }
            for (i, r) in ml.pane_rects.iter().enumerate() {
                if rect_contains(*r, x, y)
                    && (app.pane_active as usize) != i
                    && i < app.pane_states.len()
                {
                    sync_active_pane(app);
                    app.pane_active = i as u8;
                    apply_pane_state_from(app, i);
                    app.status = format!("active pane: {}/{}", app.pane_active + 1, app.pane_count);
                    return;
                }
            }
        }
    }

    if rect_contains(layouts.fbar, x, y) {
        let n_keys = F_KEYS.len() as u16;
        if let Some(cell_w) = layouts.fbar.width.checked_div(n_keys) {
            let rel = x.saturating_sub(layouts.fbar.x);
            if let Some(cell) = rel.checked_div(cell_w) {
                let idx = cell.min(n_keys - 1) as usize;
                let key = F_KEYS[idx].number;
                handle_key(app, KeyCode::F(key));
                app.status = format!("clicked F{key}");
            }
        }
    }
}

/// Max valid `chart_offset` for keyboard scroll, taking the active
/// filter into account.  Falls back to the raw `data` length until the
/// renderer has stamped a post-filter `visible_total`.
fn scroll_max_offset(p: &PaneState) -> usize {
    let total = if p.visible_total > 0 {
        p.visible_total
    } else {
        p.data.len()
    };
    // Leave at least 1 candle visible after right margin.
    total.saturating_sub(1)
}

/// Commit a chosen date (or `None` for "clear filter") to every pane in
/// linked mode, otherwise only the active pane.  Reset chart_offset so
/// the user immediately sees the filtered range from the right edge.
fn commit_date(app: &mut App, is_from: bool, value: Option<String>) {
    let active = app.pane_active as usize;
    let panes: Vec<usize> = if app.panes_linked {
        (0..app.pane_states.len()).collect()
    } else {
        vec![active]
    };
    for i in panes {
        if let Some(p) = app.pane_states.get_mut(i) {
            if is_from {
                p.date_from = value.clone();
            } else {
                p.date_to = value.clone();
            }
            p.chart_offset = 0;
        }
    }
    app.status = match (is_from, value) {
        (true, Some(d)) => format!("from = {}", d),
        (true, None) => "from cleared".into(),
        (false, Some(d)) => format!("to = {}", d),
        (false, None) => "to cleared".into(),
    };
}

/// Commit a session-window selection across linked panes.
fn commit_session(app: &mut App, w: dos::markets::SessionWindow) {
    let active = app.pane_active as usize;
    let panes: Vec<usize> = if app.panes_linked {
        (0..app.pane_states.len()).collect()
    } else {
        vec![active]
    };
    for i in panes {
        if let Some(p) = app.pane_states.get_mut(i) {
            p.session = w;
            p.chart_offset = 0;
        }
    }
    app.status = format!("session = {}", w.label());
}

// ─── Strategy slot helpers ──────────────────────────────────────────

/// Build a runtime slot from a stored DB row.  Falls back to manual
/// strategy if the persisted id is no longer registered.
/// Restore paper broker state (positions + open limits + risk
/// counters) from disk.  Called once at startup, after slot hydration.
fn hydrate_paper_state(rt: &mut Runtime, conn: &rusqlite::Connection) {
    let (broker_json, risk_json) = db::load_paper_snapshot(conn);
    if let Some(j) = broker_json {
        if let Ok(snap) = serde_json::from_str::<dos::engine::broker::BrokerSnapshot>(&j) {
            rt.broker = dos::engine::PaperBroker::restore(snap);
        }
    }
    if let Some(j) = risk_json {
        if let Ok(rs) = serde_json::from_str::<dos::engine::RiskState>(&j) {
            rt.risk_state = rs;
        }
    }
}

/// Snapshot the paper broker + risk counters to disk.  Called every 10s in
/// the main loop, after basket orders and once on shutdown.  Writes only when
/// the serialized ledger differs from what was last written, so an idle
/// session does not touch the disk.  Best-effort (DB may be locked).
fn snapshot_paper_state(
    rt: &Runtime,
    conn: &rusqlite::Connection,
    last: &mut Option<(String, String)>,
) {
    // The PAPER ledger, even while live mode has it parked.
    let (paper, risk) = rt.paper_ledger();
    let broker_json = serde_json::to_string(&paper.snapshot()).unwrap_or_else(|_| "{}".into());
    let risk_json = serde_json::to_string(risk).unwrap_or_else(|_| "{}".into());
    let now = (broker_json, risk_json);
    if last.as_ref() == Some(&now) {
        return;
    }
    if db::save_paper_snapshot(conn, &now.0, &now.1).is_ok() {
        *last = Some(now);
    }
}

fn hydrate_selection_from_db(
    rt: &mut Runtime,
    selected_set: &mut HashSet<String>,
    conn: &rusqlite::Connection,
) {
    let stored = match db::load_slots(conn) {
        Ok(s) => s,
        Err(_) => return,
    };
    for s in stored {
        let mut strat = strategy_registry::instantiate(&s.strategy_id)
            .or_else(|| strategy_registry::instantiate(strategy_registry::default_id()))
            .expect("default strategy must exist");
        // Re-apply persisted params.
        if let Ok(rows) = db::load_params(conn, &s.symbol) {
            for (k, v_json) in rows {
                if let Ok(v) = serde_json::from_str::<strategies::ParamValue>(&v_json) {
                    let _ = strat.set_param_checked(&k, v);
                }
            }
        }
        let idx = rt.add_slot(
            s.symbol.clone(),
            s.venue.clone(),
            s.strategy_id.clone(),
            strat,
        );
        // A slot that was running when the app closed is NOT restarted: its
        // runtime state (ladder, DCA counter, ...) is gone, and a fresh start
        // would stack a second ladder on the orders restored from the ledger
        // (audit D4).  It comes back idle and the user presses Start.
        if s.running {
            rt.slot_log(idx, "was running at exit - paused, press Start to resume");
            let _ = db::save_slot(
                conn,
                &db::StoredSlot {
                    symbol: s.symbol.clone(),
                    venue: s.venue.clone(),
                    strategy_id: s.strategy_id.clone(),
                    running: false,
                },
                current_now_ms(),
            );
        }
        selected_set.insert(s.symbol);
    }
}

fn persist_slot(app: &App, slot_idx: usize) {
    let Some(slot) = app.runtime.slots.get(slot_idx) else {
        return;
    };
    let Some(conn) = app.db.as_ref() else { return };
    let stored = db::StoredSlot {
        symbol: slot.symbol.clone(),
        venue: slot.venue.clone(),
        strategy_id: slot.strategy_id.clone(),
        running: slot.status == SlotStatus::Running,
    };
    let now = current_now_ms();
    let _ = db::save_slot(conn, &stored, now);
    for spec in slot.strategy.schema() {
        if let Some(v) = slot.strategy.get_param(spec.key) {
            let json = serde_json::to_string(&v).unwrap_or_else(|_| "null".into());
            let _ = db::save_param(conn, &slot.symbol, spec.key, &json);
        }
    }
}

/// Add a symbol to the selection panel.  Idempotent; returns the slot idx.
/// First add also becomes the active trade-ticket slot.
fn add_screener_selection(app: &mut App, symbol: String, venue: String) -> usize {
    if let Some(i) = app.runtime.find_slot(&symbol) {
        return i;
    }
    let strat_id = strategy_registry::default_id().to_string();
    let strat = strategy_registry::instantiate(&strat_id).expect("default strategy");
    let idx = app.runtime.add_slot(symbol.clone(), venue, strat_id, strat);
    app.selected_set.insert(symbol);
    persist_slot(app, idx);
    if app.active_slot.is_none() {
        app.active_slot = Some(idx);
    }
    idx
}

fn remove_screener_selection(app: &mut App, symbol: &str) {
    let Some(i) = app.runtime.find_slot(symbol) else {
        return;
    };
    app.runtime.remove_slot(i);
    app.selected_set.remove(symbol);
    if let Some(c) = app.db.as_ref() {
        let _ = db::delete_slot(c, symbol);
    }
    // Repair active_slot after removal: if we removed the active or one
    // before it, shift; if the list is now empty, clear.
    let n = app.runtime.slots.len();
    app.active_slot = match app.active_slot {
        None => None,
        Some(_) if n == 0 => None,
        Some(a) if a == i => Some(a.min(n.saturating_sub(1))),
        Some(a) if a > i => Some(a - 1),
        Some(a) => Some(a.min(n.saturating_sub(1))),
    };
}

fn assign_strategy(app: &mut App, slot_idx: usize, strategy_id: &str) {
    let Some(strat) = strategy_registry::instantiate(strategy_id) else {
        app.status = format!("unknown strategy: {strategy_id}");
        return;
    };
    app.runtime
        .replace_strategy(slot_idx, strategy_id.to_string(), strat);
    persist_slot(app, slot_idx);
}

fn cycle_slot_strategy(app: &mut App, slot_idx: usize) {
    let cat = strategy_registry::catalog();
    if cat.is_empty() {
        return;
    }
    let cur_id = app
        .runtime
        .slots
        .get(slot_idx)
        .map(|s| s.strategy_id.clone())
        .unwrap_or_default();
    let i = cat.iter().position(|m| m.id == cur_id).unwrap_or(0);
    let next = &cat[(i + 1) % cat.len()];
    assign_strategy(app, slot_idx, next.id);
    if let Some(slot) = app.runtime.slots.get(slot_idx) {
        app.status = format!("[{}] strategy → {}", slot.symbol, next.name);
    }
}

/// Push the most recent live-pair prices into the runtime so the broker
/// can mark positions and fire any pending limit fills.
fn push_marks_to_runtime(app: &mut App) {
    if app.runtime.slots.is_empty() {
        return;
    }
    let now_ms = current_now_ms();
    // Build a small map of {symbol → last_price} from the live snapshot.
    // O(n_pairs) but n is bounded by the rendered list (≤1000).
    for slot_i in 0..app.runtime.slots.len() {
        let sym = app.runtime.slots[slot_i].symbol.clone();
        let price = app
            .live_pairs
            .iter()
            .find(|p| p.symbol == sym)
            .map(|p| p.last_price)
            .unwrap_or(0.0);
        if price > 0.0 {
            app.runtime.on_price(&sym, price, now_ms);
        }
    }
    app.runtime.dispatch_timers(now_ms);
}

/// Effective per-pair qty for slot `idx`, given the active size mode +
/// leverage.  Uses the symbol's last mark to convert USD → coins.
/// Returns 0.0 when there's no last price (don't submit a zero order).
fn effective_qty(app: &App, slot_idx: usize) -> f64 {
    let Some(slot) = app.runtime.slots.get(slot_idx) else {
        return 0.0;
    };
    let last = app.runtime.last_mark(&slot.symbol).unwrap_or(0.0);
    let base = match app.size_mode {
        SizeMode::Qty => app.ticket_qty,
        SizeMode::Usd => {
            if last > 0.0 {
                app.ticket_notional / last
            } else {
                0.0
            }
        }
    };
    // Leverage is a Futures concept; on Spot it must not scale the order.
    let lev = if slot.venue.to_ascii_uppercase().contains("FUT") {
        app.leverage
    } else {
        1.0
    };
    (base * lev).max(0.0)
}

/// Resolve a [`TicketAction`] for slot `idx` into a list of broker
/// actions, sized via [`effective_qty`].  Returns empty for no-op
/// (e.g. REV/CLOSE on a flat slot).
fn ticket_to_actions_for_slot(
    app: &App,
    slot_idx: usize,
    action: TicketAction,
) -> Vec<StrategyAction> {
    let Some(slot) = app.runtime.slots.get(slot_idx) else {
        return vec![];
    };
    let symbol = slot.symbol.clone();
    let last = app.runtime.last_mark(&symbol).unwrap_or(0.0);
    let pos = app.runtime.broker.position(&symbol);
    let qty = effective_qty(app, slot_idx);
    if qty <= 0.0 && !matches!(action, TicketAction::Reverse | TicketAction::Close) {
        return vec![];
    }
    match action {
        TicketAction::BuyMarket | TicketAction::BuyAsk => {
            vec![StrategyAction::BuyMarket { symbol, qty }]
        }
        TicketAction::SellMarket | TicketAction::SellBid => {
            vec![StrategyAction::SellMarket { symbol, qty }]
        }
        TicketAction::BuyBid => vec![StrategyAction::BuyLimit {
            symbol,
            qty,
            limit: last,
        }],
        TicketAction::SellAsk => vec![StrategyAction::SellLimit {
            symbol,
            qty,
            limit: last,
        }],
        TicketAction::Reverse => {
            if pos.is_flat() {
                return vec![];
            }
            let total = pos.qty.abs() + qty;
            if pos.qty > 0.0 {
                vec![StrategyAction::SellMarket { symbol, qty: total }]
            } else {
                vec![StrategyAction::BuyMarket { symbol, qty: total }]
            }
        }
        TicketAction::Close => {
            if pos.is_flat() {
                return vec![];
            }
            if pos.qty > 0.0 {
                vec![StrategyAction::SellMarket {
                    symbol,
                    qty: pos.qty.abs(),
                }]
            } else {
                vec![StrategyAction::BuyMarket {
                    symbol,
                    qty: pos.qty.abs(),
                }]
            }
        }
    }
}

/// Apply a ticket button to EVERY slot in the basket.  Reports an
/// aggregate status: "BUY MKT: 17 sent (12 filled, 5 blocked)".
fn submit_basket_action(app: &mut App, action: TicketAction) {
    let n_slots = app.runtime.slots.len();
    if n_slots == 0 {
        app.status = format!("{}: no pairs in basket", action.label());
        app.last_ticket_press = Some((action, Instant::now()));
        return;
    }
    let now_ms = current_now_ms();
    let mut sent = 0usize;
    let mut filled = 0usize;
    let mut blocked = 0usize;
    let mut last_err: Option<String> = None;
    for slot_idx in 0..n_slots {
        let actions = ticket_to_actions_for_slot(app, slot_idx, action);
        if actions.is_empty() {
            continue;
        }
        sent += 1;
        for a in actions {
            match app.runtime.submit_user(a, now_ms) {
                Ok(fills) => filled += fills.len(),
                Err(e) => {
                    blocked += 1;
                    last_err = Some(e);
                }
            }
        }
    }
    app.status = if sent == 0 {
        format!("{}: no-op (no qty / flat?)", action.label())
    } else if blocked == 0 {
        format!("{}: {} sent, {} filled", action.label(), sent, filled)
    } else {
        format!(
            "{}: {} sent, {} filled, {} blocked ({})",
            action.label(),
            sent,
            filled,
            blocked,
            last_err.unwrap_or_default()
        )
    };
    app.last_ticket_press = Some((action, Instant::now()));
    // Snapshot paper state immediately after a basket order so the
    // user's positions survive a crash / kill before the next 10s
    // periodic snapshot.  Cheap (one kv write).
    if let Some(c) = app.db.as_ref() {
        snapshot_paper_state(&app.runtime, c, &mut app.last_persisted);
    }
}

/// Adjust the active value field (qty when `size_mode = Qty`, notional
/// when `size_mode = Usd`).  Step is magnitude-aware via `step_qty`,
/// scaled by 10× when stepping USD so $5 → $6 → $7… feels natural.
fn bump_ticket_value(app: &mut App, dir: i32) {
    match app.size_mode {
        SizeMode::Qty => {
            app.ticket_qty = trade_ticket::step_qty(app.ticket_qty, dir);
            app.status = format!("qty = {}", app.ticket_qty);
        }
        SizeMode::Usd => {
            // For USD use a friendlier 1/5/10/50/100 ramp.
            let cur = app.ticket_notional;
            let step = if cur >= 100.0 {
                10.0
            } else if cur >= 10.0 {
                1.0
            } else if cur >= 1.0 {
                0.5
            } else {
                0.1
            };
            let next = (cur + step * dir.signum() as f64).max(0.1);
            app.ticket_notional = (next * 100.0).round() / 100.0;
            app.status = format!("notional = ${:.2}", app.ticket_notional);
        }
    }
}

/// Tab through the selection list, advancing the trade ticket.  Wraps.
fn cycle_active_slot(app: &mut App, dir: i32) {
    let n = app.runtime.slots.len();
    if n == 0 {
        app.active_slot = None;
        return;
    }
    let cur = app.active_slot.unwrap_or(0) as i32;
    let nx = ((cur + dir).rem_euclid(n as i32)) as usize;
    app.active_slot = Some(nx);
    app.status = format!("active: {}", app.runtime.slots[nx].symbol);
}

/// Toggle the symbol at the currently-selected screener row in/out of
/// the selection panel.  Used by Space + checkbox click.
fn toggle_screener_selection_at(app: &mut App, ranked_idx: usize) {
    if !app.screener_live {
        return; // synthetic mode has no checkboxes for now
    }
    let Some((sym_idx, _)) = screener::rank_live(
        &app.live_pairs,
        1000,
        app.screener_sort,
        app.screener_min_qv,
        app.screener_sort_asc,
    )
    .into_iter()
    .nth(ranked_idx) else {
        return;
    };
    let Some(pair) = app.live_pairs.get(sym_idx).cloned() else {
        return;
    };
    let venue = format!("BINANCE-{}", app.screener_venue.name().to_ascii_uppercase());
    if app.selected_set.contains(&pair.symbol) {
        remove_screener_selection(app, &pair.symbol);
        app.status = format!("- {} unselected", pair.symbol);
    } else {
        add_screener_selection(app, pair.symbol.clone(), venue);
        app.status = format!("+ {} added to selection", pair.symbol);
    }
}

fn toggle_screener_selection_by_sym_idx(app: &mut App, sym_idx: usize) {
    let Some(pair) = app.live_pairs.get(sym_idx).cloned() else {
        return;
    };
    let venue = format!("BINANCE-{}", app.screener_venue.name().to_ascii_uppercase());
    if app.selected_set.contains(&pair.symbol) {
        remove_screener_selection(app, &pair.symbol);
        app.status = format!("- {} unselected", pair.symbol);
    } else {
        add_screener_selection(app, pair.symbol.clone(), venue);
        app.status = format!("+ {} added to selection", pair.symbol);
    }
}

/// How many help pages to show.  Each page = one section.
fn help_page_count() -> usize {
    build_help_sections().len()
}

/// Build categorized help pages for the F1 overlay.
fn build_help_sections() -> Vec<dos::widgets::help::HelpSection> {
    use dos::widgets::help::HelpSection;
    vec![
        HelpSection {
            title: Some(" Global ".into()),
            items: vec![
                ("F1".into(), "Help — this screen".into(), "".into()),
                (
                    "F3".into(),
                    "Markets — chart view".into(),
                    "candles, line, bars, footprint".into(),
                ),
                (
                    "F4".into(),
                    "Screener — symbol list".into(),
                    "rank by gap/change/volume".into(),
                ),
                (
                    "F5".into(),
                    "Dashboard — paper/live ledger".into(),
                    "equity, positions, orders, fills".into(),
                ),
                (
                    "F6".into(),
                    "Order Book — depth".into(),
                    "real-time WebSocket book".into(),
                ),
                (
                    "F9".into(),
                    "Settings — configuration".into(),
                    "market, TF, API keys".into(),
                ),
                ("F10 / q".into(), "Quit".into(), "".into()),
                ("Esc".into(), "Back / close modal".into(), "".into()),
                ("Tab".into(), "Cycle focus / selections".into(), "".into()),
                (
                    ":command".into(),
                    "Command palette".into(),
                    "quick actions".into(),
                ),
                (
                    ":risk".into(),
                    "Show / set risk limits".into(),
                    "maxpos orders loss dd kill".into(),
                ),
                (
                    ":live / :paper".into(),
                    "Go live (confirm) / back to paper".into(),
                    "default answer is No".into(),
                ),
            ],
        },
        HelpSection {
            title: Some(" Markets (F3) ".into()),
            items: vec![
                (
                    "← → / h l".into(),
                    "Previous / next symbol".into(),
                    "".into(),
                ),
                (
                    "+ / -".into(),
                    "Zoom in / out".into(),
                    "widen candles or aggregate".into(),
                ),
                (
                    "0".into(),
                    "Reset zoom + scroll".into(),
                    "back to live".into(),
                ),
                (
                    ", / .  or  < / >".into(),
                    "Scroll chart left / right".into(),
                    "1 candle".into(),
                ),
                (
                    "PgUp / PgDn".into(),
                    "Scroll by 25 candles".into(),
                    "".into(),
                ),
                (
                    "Home / End".into(),
                    "Jump to live / oldest".into(),
                    "".into(),
                ),
                (
                    "t or /".into(),
                    "Enter ticker symbol".into(),
                    "modal input".into(),
                ),
                (
                    "c".into(),
                    "Cycle chart type".into(),
                    "Candle → Line → Bar → ΔCluster".into(),
                ),
                (
                    "f".into(),
                    "Cycle timeframe".into(),
                    "15s → 1m → 5m → 1h → 1d".into(),
                ),
                (
                    "r".into(),
                    "Refresh OHLCV data".into(),
                    "background fetch".into(),
                ),
                (
                    "[ / ]".into(),
                    "Remove / add pane".into(),
                    "multi-pane charts".into(),
                ),
                (
                    "L".into(),
                    "Toggle link panes".into(),
                    "sync ticker across panes".into(),
                ),
            ],
        },
        HelpSection {
            title: Some(" Screener (F4) ".into()),
            items: vec![
                ("↑ ↓ / j k".into(), "Navigate symbol list".into(), "".into()),
                ("PgUp / PgDn".into(), "Scroll by 10".into(), "".into()),
                (
                    "Home / End".into(),
                    "Jump to top / bottom".into(),
                    "".into(),
                ),
                (
                    "Enter".into(),
                    "Open chart for symbol".into(),
                    "switches to Markets".into(),
                ),
                (
                    "Space".into(),
                    "Toggle selection checkbox".into(),
                    "add/remove from portfolio".into(),
                ),
                (
                    "s".into(),
                    "Cycle sort mode".into(),
                    "Gap% → Change% → Volume".into(),
                ),
                ("l".into(), "Toggle live / synthetic".into(), "".into()),
                ("v".into(), "Toggle venue".into(), "Spot ↔ Futures".into()),
                ("m".into(), "Cycle min volume filter".into(), "".into()),
                (
                    "r".into(),
                    "Refresh live pairs".into(),
                    "Binance REST".into(),
                ),
                (
                    "B / N / Z".into(),
                    "Basket buy / sell / close".into(),
                    "all selected symbols".into(),
                ),
                ("U".into(), "Flip size mode".into(), "Qty ↔ USD".into()),
                ("X".into(), "Cycle leverage".into(), "".into()),
                (
                    "+ / -".into(),
                    "Adjust ticket qty/notional".into(),
                    "".into(),
                ),
            ],
        },
        HelpSection {
            title: Some(" Dashboard (F5) ".into()),
            items: vec![
                ("Esc".into(), "Back to Screener".into(), "".into()),
                ("q".into(), "Quit".into(), "".into()),
                (
                    "—".into(),
                    "Portfolio equity curve".into(),
                    "auto-updates".into(),
                ),
                (
                    "—".into(),
                    "Positions table".into(),
                    "PnL per symbol".into(),
                ),
                ("—".into(), "Recent fills".into(), "engine fill log".into()),
            ],
        },
        HelpSection {
            title: Some(" Order Book (F6) ".into()),
            items: vec![
                (
                    "t or /".into(),
                    "Enter ticker".into(),
                    "manual symbol".into(),
                ),
                ("+ / -".into(), "Zoom depth levels".into(), "".into()),
                ("0".into(), "Reset zoom".into(), "native tick".into()),
                ("r".into(), "Reconnect WS".into(), "restart streams".into()),
                ("Esc".into(), "Back to Screener".into(), "".into()),
            ],
        },
        HelpSection {
            title: Some(" Settings (F9) ".into()),
            items: vec![
                ("↑ ↓ / Tab".into(), "Navigate fields".into(), "".into()),
                ("Enter".into(), "Activate / apply".into(), "".into()),
                ("Esc".into(), "Cancel / back".into(), "".into()),
                (
                    "—".into(),
                    "Market universe".into(),
                    "US Stocks, Crypto, FX…".into(),
                ),
                ("—".into(), "Timeframe presets".into(), "15s to 1d".into()),
                (
                    "—".into(),
                    "API key management".into(),
                    "Binance keys".into(),
                ),
                ("—".into(), "Clear OHLC cache".into(), "SQLite DB".into()),
                (
                    "—".into(),
                    "Reset paper ledger".into(),
                    "menu Cache; confirm twice".into(),
                ),
            ],
        },
    ]
}

// ── Risk limits (audit A9) ───────────────────────────────────────────────

/// Write the risk limits to the DB when they changed — including a kill
/// switch the engine tripped by itself, so a restart does not silently
/// re-arm trading.
fn persist_risk_limits(app: &mut App) {
    let Some(conn) = app.db.as_ref() else { return };
    let json = serde_json::to_string(&app.runtime.limits).unwrap_or_default();
    if app.last_limits_json.as_deref() != Some(json.as_str())
        && db::save_kv(conn, "risk_limits", &json).is_ok()
    {
        app.last_limits_json = Some(json);
    }
}

fn risk_summary(l: &dos::engine::RiskLimits) -> String {
    format!(
        "risk: maxpos {}  orders {}  loss {}  dd {}%  kill {}",
        l.max_abs_position,
        l.max_open_orders,
        l.max_daily_loss,
        l.max_drawdown_pct,
        if l.kill_switch { "ON" } else { "off" }
    )
}

/// `:risk` shows the limits; `:risk maxpos|orders|loss|dd <n>` sets one;
/// `:risk kill on|off` flips the kill switch (off also clears the violation).
fn risk_command(app: &mut App, args: &str) {
    let mut it = args.split_whitespace();
    let (Some(key), Some(val)) = (it.next(), it.next()) else {
        app.status = format!(
            "{}   (set: :risk maxpos|orders|loss|dd <n>, :risk kill on|off)",
            risk_summary(&app.runtime.limits)
        );
        return;
    };
    let num = val
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0);
    let l = &mut app.runtime.limits;
    let ok = match (key, num) {
        ("maxpos", Some(n)) => {
            l.max_abs_position = n;
            true
        }
        ("orders", Some(n)) => {
            l.max_open_orders = n as usize;
            true
        }
        ("loss", Some(n)) => {
            l.max_daily_loss = n;
            true
        }
        ("dd", Some(n)) if n <= 100.0 => {
            l.max_drawdown_pct = n;
            true
        }
        ("kill", _) if val == "on" || val == "off" => {
            l.kill_switch = val == "on";
            if val == "off" {
                app.runtime.risk_state.last_violation = None;
            }
            true
        }
        _ => false,
    };
    app.status = if ok {
        risk_summary(&app.runtime.limits)
    } else {
        "usage: :risk maxpos|orders|loss|dd <non-negative number>, :risk kill on|off".into()
    };
    persist_risk_limits(app);
}

// ── Live trading: ONE owner for entering and leaving (audit A6) ──────────

/// Ask to go live.  Never switches by itself: it validates the keys and opens
/// the confirm modal, whose default button is "No".
fn request_live(app: &mut App) {
    if dos::data::demo::is_on() {
        app.status = "Demo mode has no exchange connection; restart without --demo".into();
    } else if app.runtime.is_live() {
        app.status = "Already live".into();
    } else if app.binance_key.is_empty() || app.binance_secret.is_empty() {
        app.status = "Set Binance API keys in Settings (F9) first".into();
    } else if app.key_check.as_ref().is_some_and(|k| k.withdrawals) {
        app.status = "Refusing live: this key can WITHDRAW funds. Use a key without withdrawal permission, then :keys check".into();
    } else if app.key_check.is_none() {
        app.live_after_check = true;
        start_key_check(app);
    } else {
        app.live_confirm_focus = 1;
        app.modal = Some(ModalKind::LiveConfirm);
        app.status = "Confirm live trading to send orders to Binance.".into();
    }
}

/// Where the spot key came from, for `:keys` (never prints the secret).
fn key_source(app: &App) -> String {
    if app.binance_key.is_empty() {
        return "no Binance key set (paper trading and market data need none)".into();
    }
    let tail: String = app
        .binance_key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let src = if app
        .env_spot
        .as_ref()
        .is_some_and(|(k, _)| *k == app.binance_key)
    {
        "environment DOS_BINANCE_API_KEY"
    } else {
        "local database"
    };
    format!("Binance key ...{tail} from {src}")
}

/// Check what the current spot key may do, on a worker thread.
fn start_key_check(app: &mut App) {
    if app.pending_key_check.is_some() {
        return;
    }
    let (key, secret) = (app.binance_key.clone(), app.binance_secret.clone());
    let base = match dos::engine::binance::api_base_from_env() {
        Ok(b) => b,
        Err(e) => {
            app.status = e;
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(dos::engine::binance::check_key(&key, &secret, &base));
    });
    app.pending_key_check = Some(rx);
    app.status = "Checking key permissions with Binance...".into();
}

fn poll_key_check(app: &mut App) {
    let Some(rx) = app.pending_key_check.as_ref() else {
        return;
    };
    let result = match rx.try_recv() {
        Ok(r) => r,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => Err("key check stopped unexpectedly".into()),
    };
    app.pending_key_check = None;
    let ask_live = std::mem::take(&mut app.live_after_check);
    match result {
        Ok(p) => {
            app.status = format!("{}  ({})", p.summary(), key_source(app));
            let ok_for_live = !p.withdrawals && p.trading;
            app.key_check = Some(p);
            if ask_live {
                if ok_for_live {
                    request_live(app);
                } else {
                    app.status.push_str("  Live needs a key that can trade.");
                }
            }
        }
        Err(e) => {
            app.status = format!("Key check failed: {e}");
        }
    }
}

/// `:keys` — where the key lives; `:keys check` — what it may do;
/// `:keys clear` — forget the stored key.
fn keys_command(app: &mut App, args: &str) {
    match args {
        "" => {
            app.status = format!(
                "{}.  :keys check | :keys clear   (env DOS_BINANCE_API_KEY / _SECRET skips the database)",
                key_source(app)
            );
        }
        "check" => {
            if dos::data::demo::is_on() {
                app.status = "Demo mode has no exchange connection".into();
            } else if app.binance_key.is_empty() || app.binance_secret.is_empty() {
                app.status = "No Binance key set. Add one in Settings (F9) or via DOS_BINANCE_API_KEY / DOS_BINANCE_API_SECRET".into();
            } else {
                start_key_check(app);
            }
        }
        "clear" => {
            if app.runtime.is_live() {
                app.status = "Leave live mode first (:paper)".into();
            } else if app
                .env_spot
                .as_ref()
                .is_some_and(|(k, s)| *k == app.binance_key && *s == app.binance_secret)
            {
                app.status =
                    "Key comes from the environment: unset DOS_BINANCE_API_KEY / _SECRET instead"
                        .into();
            } else {
                app.binance_key.clear();
                app.binance_secret.clear();
                app.key_check = None;
                if let Some(c) = app.db.as_ref() {
                    let _ = db::save_kv(c, "binance_api_key", "");
                    let _ = db::save_kv(c, "binance_api_secret", "");
                }
                app.status = "Binance spot key removed from the local database".into();
            }
        }
        _ => app.status = "Usage: :keys | :keys check | :keys clear".into(),
    }
}

/// The user said Yes.  (Slow startup work runs on a background thread.)
fn confirm_live(app: &mut App) {
    app.modal = None;
    if app.runtime.is_live() {
        return;
    }
    let base = match dos::engine::binance::api_base_from_env() {
        Ok(b) => b,
        Err(e) => {
            app.status = e;
            return;
        }
    };
    app.runtime.enable_live(
        app.binance_key.clone(),
        app.binance_secret.clone(),
        base.clone(),
    );
    app.settings.live_enabled = true;
    app.status =
        format!("LIVE BINANCE Spot | {base} | strategies stopped, ledger is the exchange mirror");
}

fn cancel_live(app: &mut App) {
    app.modal = None;
    app.settings.live_enabled = false;
    app.status = "Live trading cancelled.".into();
}

fn leave_live(app: &mut App) {
    if app.runtime.is_live() {
        app.runtime.disable_live();
    }
    app.settings.live_enabled = false;
}

/// After Settings → Apply: reconcile the toggle with the runtime.
fn sync_live_with_settings(app: &mut App) {
    if app.settings.live_enabled && !app.runtime.is_live() {
        request_live(app);
    } else if !app.settings.live_enabled && app.runtime.is_live() {
        leave_live(app);
        app.status = "Live trading disabled. Back to paper mode.".into();
    }
}

/// Execute a command from the command palette (`:command` input).
fn execute_command(app: &mut App) {
    let cmd = app.command_input.text.trim().to_lowercase();
    let trimmed = cmd.trim_start_matches(':');
    app.command_input.text.clear();
    app.command_input.cursor = 0;
    app.modal = None;

    if trimmed == "keys" || trimmed.starts_with("keys ") {
        keys_command(app, trimmed.trim_start_matches("keys").trim());
        return;
    }
    if trimmed == "risk" || trimmed.starts_with("risk ") {
        risk_command(app, trimmed.trim_start_matches("risk").trim());
        return;
    }

    match trimmed {
        "help" | "h" | "?" => {
            app.modal = Some(ModalKind::Help);
            app.help_page = 0;
            app.status = "Help: ←/→ page  Esc/F1/q close".into();
        }
        "live" | "l" => request_live(app),
        "paper" | "p" => {
            leave_live(app);
            app.status = "Paper mode (local simulation)".into();
        }
        "quit" | "q" | "exit" => {
            app.quit = true;
        }
        "screener" | "s" | "f4" => {
            app.view = View::Screener;
            app.status = "Screener.  :help for commands".into();
        }
        "markets" | "m" | "f3" => {
            app.view = View::Markets;
            app.status = "Markets.  :help for commands".into();
        }
        "dashboard" | "d" | "f5" => {
            app.view = View::Dashboard;
            app.status = "Dashboard.  :help for commands".into();
        }
        "book" | "orderbook" | "ob" | "f6" => {
            app.view = View::OrderBook;
            app.status = "Order Book.  :help for commands".into();
        }
        "settings" | "setup" | "config" | "f9" => {
            app.view = View::Settings;
            app.settings = SettingsState::new_full(
                app.market,
                app.timeframe,
                &app.binance_key,
                &app.binance_secret,
                &app.binance_futures_key,
                &app.binance_futures_secret,
                app.ob_show_trades,
                app.chart_history_range,
                app.font_size,
                app.runtime.is_live(),
            );
            app.status = "Settings.  :help for commands".into();
        }
        "" => {
            app.status =
                "Commands: :help :live :paper :quit :screener :markets :dashboard :book :settings"
                    .into();
        }
        other => {
            app.status = format!("Unknown command: `:{}`. Try :help", other);
        }
    }
}
