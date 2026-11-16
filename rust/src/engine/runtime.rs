//! Runtime — owns the broker, the risk gate, and the live strategy slots.
//!
//! Driven from the main UI loop:
//!   * `on_price(symbol, price, now_ms)`  on every tick from the data feed
//!   * `dispatch_timers(now_ms)`          on each main-loop tick
//!   * `submit_user(action, now_ms)`      from the terminal command bar
//!
//! All strategy callbacks happen synchronously inside these methods.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::basket::{self, Basket, Book, Intent};
use crate::strategies::{
    actions::Action,
    context::{Bar, Ctx, Fill, Side, Tick},
    Strategy,
};

use super::binance::{
    spawn_user_data_ws, BalanceEntry, BinanceClient, FillSource, LiveEvent, UserDataWs,
};
use super::broker::{Bracket, Broker, OpenOrder, PaperBroker};
use super::risk::{check_action, enforce_aggregate, RiskLimits, RiskState};

/// Who placed an order: a strategy slot (by its symbol) or a basket (by id).
/// A strategy only hears about fills of its OWN orders and cancels only
/// those; a basket likewise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    Slot(String),
    Basket(u64),
}

/// What `submit_owned` did: the fills it got at once, and the ids of the
/// orders that rested.
#[derive(Debug, Default)]
pub struct Submitted {
    pub fills: Vec<Fill>,
    pub order_ids: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotStatus {
    Idle,
    Running,
    Error,
}

impl SlotStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running => "Run",
            Self::Error => "Err",
        }
    }
}

/// One symbol + strategy pair tracked by the runtime.
pub struct Slot {
    pub symbol: String,
    pub venue: String, // "BINANCE-SPOT", "BINANCE-FUT", "YAHOO", "MANUAL"
    pub strategy: Box<dyn Strategy>,
    pub strategy_id: String,
    pub status: SlotStatus,
    pub log: VecDeque<String>,
    pub bar_history: Vec<Bar>,
    pub last_timer_at: i64,
    pub error: Option<String>,
}

impl Slot {
    fn push_log(&mut self, msg: String) {
        self.log.push_back(msg);
        while self.log.len() > 64 {
            self.log.pop_front();
        }
    }
}

pub struct Runtime {
    pub slots: Vec<Slot>,
    pub broker: PaperBroker,
    pub limits: RiskLimits,
    pub risk_state: RiskState,
    pub last_marks: HashMap<String, f64>,
    /// Log of every fill (broker echo).  The UI tails this for the activity panel.
    pub fill_log: VecDeque<(i64, String, Fill)>,
    /// Log of every action that was rejected by the risk gate.
    pub blocked_log: VecDeque<(i64, String, String)>,
    /// Recent equity samples (mark-to-market).  Used by the F5 Dashboard
    /// equity curve.  Capped at `EQUITY_HISTORY_CAP`.
    pub equity_history: VecDeque<f64>,
    /// Time bucket (`now_ms / EQUITY_SAMPLE_MS`) of the newest sample in
    /// `equity_history`; within one bucket the sample is overwritten.
    equity_bucket: i64,
    /// Wall-clock millis when the last fresh tick was applied via
    /// `on_price`.  The UI uses this to show data-staleness ("marks
    /// 2.3s ago") so the user can tell live from frozen.
    pub last_mark_update_ms: i64,
    /// Whether a WS price feed is currently connected.  Updated by
    /// the commander's `drain_mini_ticker`.  Surfaced in panel titles.
    pub ws_connected: bool,
    /// Best-effort error/status message from the WS thread.  Empty
    /// when healthy.
    pub ws_status: String,
    /// Total Tick events processed since startup.  Surfaced in the
    /// Selection panel header next to `WS:Xms` so the user can see
    /// the stream is actually delivering updates ("WS:2ms (12.4K)").
    pub ws_tick_count: u64,
    /// Per-symbol `(last_price, last_tick_at_ms)`.  Used by the UI to
    /// flash the row briefly when a fresh quote arrives — gives the
    /// user a visual proof that the WS is alive even on thin alts
    /// where the displayed price barely moves.
    pub last_tick_per_symbol: HashMap<String, (f64, i64)>,
    /// Live Binance client.  When `Some`, every action submitted via
    /// `submit_user` is forwarded to Binance over signed REST; the
    /// PaperBroker becomes a UI mirror of executed fills.  When
    /// `None`, the runtime is in paper mode and PaperBroker simulates
    /// fills locally.
    pub binance: Option<BinanceClient>,
    /// Last Binance balance snapshot from `/api/v3/account`.  Drives
    /// the F5 "Live Balances" view.
    pub live_balances: Vec<BalanceEntry>,
    /// Wall-clock millis when `live_balances` was last updated.
    pub live_balances_at: i64,
    /// Active listen-key for the user-data WebSocket (if live mode).
    pub listen_key: Option<String>,
    /// Handle to the user-data WS thread (if live mode).
    pub user_data_ws: Option<UserDataWs>,
    /// The PAPER ledger parked while live mode is on.  Live and paper never
    /// share a ledger: `broker` is the exchange mirror during live, and the
    /// paper state is restored untouched on `disable_live` (audit A5).
    paper_stash: Option<(PaperBroker, RiskState, Book)>,
    /// Who placed an order (broker / exchange order id → [`Owner`]).
    /// A strategy only hears about fills of its OWN orders, and `on_stop`
    /// cancels only its own orders (audit D5); a basket the same.
    order_owner: HashMap<u64, Owner>,
    /// Live: client order id → slot symbol, until the exchange id is known.
    owner_by_cid: HashMap<String, String>,
    /// Live fill de-duplication (REST + user-data report the same fill).
    rest_filled: HashSet<u64>,
    seen_trades: HashSet<(u64, u64)>,
    /// Latest live-side problem the user must see (sync / cancel / unknown
    /// order).  `live_error_seq` bumps on every new one.
    pub live_error: Option<String>,
    pub live_error_seq: u64,
    /// This window may look but not trade (another window holds the trading
    /// lease, `dos::link`).  Marks still move so equity reads right, but
    /// nothing fills, no strategy runs and no order is accepted: the ledger
    /// shown is a copy of the trading window's.
    pub read_only: bool,
    /// The scalper's PAPER ledger (`agent::mandate`): its own cash, positions
    /// and orders, never mixed with `broker`.  A new mandate starts a new one;
    /// the last one stays here, marked to market, until then.  Risk limits
    /// apply to both ledgers together ([`risk_equity`](Self::risk_equity),
    /// `check_action`).
    pub scalper: Option<PaperBroker>,
    /// Fills of resting scalper orders, until the caller drains them.
    pub scalper_fills: Vec<(String, Fill)>,
    /// Stops and takes that fired (or failed to), until the UI drains them.
    pub exit_events: Vec<String>,
    /// Baskets on the paper ledger (`crate::basket`): synthetic instruments
    /// entered and left as one.  Parked with the ledger in live mode.
    pub baskets: Book,
    /// What the baskets did (opened, closed, blocked), until the UI drains them.
    pub basket_events: Vec<String>,
    /// Baskets that closed since the UI last drained them (the journal,
    /// `db::journal_add`), taken before the book retires old closed ones.
    pub closed_out: Vec<basket::Basket>,
    /// (id, created) of the closed baskets already handed out.
    closed_seen: HashSet<(u64, i64)>,
    /// Perpetuals (`crate::futures`): the latest funding rate per exchange
    /// symbol (per 8 h), when funding was last accounted, and what the
    /// forced closes and funding payments did, until the UI drains them.
    pub funding_rates: HashMap<String, f64>,
    last_funding_ms: i64,
    pub futures_events: Vec<String>,
    /// Leverage a basket's perpetual legs get when the basket itself has none
    /// (`crate::futures::DEFAULT_LEVERAGE`, set in F9).
    pub default_leverage: f64,
    /// A basket with a short coin leg is moved to perpetuals (on by default;
    /// off keeps the old spot-style paper shorts, for tests).
    pub perp_shorts: bool,
}

/// Why a view-only runtime refuses to act.
pub const VIEW_ONLY: &str = "view-only window: another window trades (:take to take over)";

/// One equity sample per this many millis (the newest one is live: it is
/// overwritten until its bucket ends).  One chart column = one bucket.
pub const EQUITY_SAMPLE_MS: i64 = 5_000;

/// Bound on `Runtime::equity_history`: 200 buckets ≈ 17 minutes.
pub const EQUITY_HISTORY_CAP: usize = 200;

/// Starting cash of a fresh paper ledger.
pub const PAPER_STARTING_CASH: f64 = 10_000.0;

/// Scalper order ids start here, so an id never names an order in both
/// ledgers (agents see the ids).
pub const SCALPER_FIRST_ORDER_ID: u64 = 1_000_000_000;

/// Bound on `Runtime::scalper_fills` when nobody drains it.
const SCALPER_FILLS_CAP: usize = 256;

impl Runtime {
    /// Wipe the paper ledger: broker (cash, positions, open orders), risk
    /// counters and the activity logs.  Slots and their strategies stay.
    /// Refuses in live mode, where the broker mirrors the real exchange.
    pub fn reset_paper(&mut self) -> Result<(), String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if self.is_live() {
            return Err("live mode: the ledger mirrors the exchange".into());
        }
        self.broker = PaperBroker::new(PAPER_STARTING_CASH);
        self.risk_state = RiskState::default();
        self.baskets = Book::default();
        self.reset_activity();
        Ok(())
    }

    pub fn new(starting_cash: f64) -> Self {
        Self {
            slots: Vec::new(),
            broker: PaperBroker::new(starting_cash),
            limits: RiskLimits::default(),
            risk_state: RiskState::default(),
            last_marks: HashMap::new(),
            fill_log: VecDeque::with_capacity(128),
            blocked_log: VecDeque::with_capacity(64),
            equity_history: VecDeque::with_capacity(EQUITY_HISTORY_CAP),
            equity_bucket: i64::MIN,
            last_mark_update_ms: 0,
            binance: None,
            live_balances: Vec::new(),
            live_balances_at: 0,
            ws_connected: false,
            ws_status: String::new(),
            ws_tick_count: 0,
            last_tick_per_symbol: HashMap::new(),
            listen_key: None,
            user_data_ws: None,
            paper_stash: None,
            order_owner: HashMap::new(),
            owner_by_cid: HashMap::new(),
            rest_filled: HashSet::new(),
            seen_trades: HashSet::new(),
            live_error: None,
            live_error_seq: 0,
            read_only: false,
            scalper: None,
            scalper_fills: Vec::new(),
            exit_events: Vec::new(),
            baskets: Book::default(),
            basket_events: Vec::new(),
            closed_out: Vec::new(),
            closed_seen: HashSet::new(),
            funding_rates: HashMap::new(),
            last_funding_ms: 0,
            futures_events: Vec::new(),
            default_leverage: crate::futures::DEFAULT_LEVERAGE,
            perp_shorts: true,
        }
    }

    // ── Perpetuals: margin, liquidation, funding (`crate::futures`) ──────

    /// Equity not tied up by open positions or resting orders.
    pub fn free_margin(&self) -> f64 {
        self.equity() - self.broker.margin_used() - self.broker.margin_reserved()
    }

    /// Dollars of margin an order of `qty` at `price` would tie up on top of
    /// what its position holds: only a perpetual's growth counts.
    pub fn margin_needed(&self, symbol: &str, buy: bool, qty: f64, price: f64) -> f64 {
        if !crate::futures::is_perp(symbol)
            || price.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        {
            return 0.0;
        }
        let pos = self.broker.position(symbol);
        let signed = if buy { qty } else { -qty };
        let grow = (pos.qty + signed).abs() - pos.qty.abs();
        if grow <= 0.0 {
            return 0.0;
        }
        crate::futures::initial_margin(grow * price, self.broker.leverage(symbol))
    }

    /// Is there margin for this order?  (Spot orders are not gated here.)
    pub fn margin_ok(&self, symbol: &str, buy: bool, qty: f64, price: f64) -> Result<(), String> {
        let need = self.margin_needed(symbol, buy, qty, price);
        let free = self.free_margin();
        if need > free + 1e-9 {
            return Err(format!(
                "not enough margin: needs ${need:.2}, free ${free:.2} (leverage x{})",
                self.broker.leverage(symbol)
            ));
        }
        Ok(())
    }

    /// The margin gate for an action (market: at `last`; limit: at its limit).
    fn margin_for(&self, action: &Action, last: f64) -> Result<(), String> {
        use crate::strategies::actions::Action as A;
        match action {
            A::BuyMarket { symbol, qty } => self.margin_ok(symbol, true, *qty, last),
            A::SellMarket { symbol, qty } => self.margin_ok(symbol, false, *qty, last),
            A::BuyLimit { symbol, qty, limit } => self.margin_ok(symbol, true, *qty, *limit),
            A::SellLimit { symbol, qty, limit } => self.margin_ok(symbol, false, *qty, *limit),
            _ => Ok(()),
        }
    }

    /// Close a perpetual by force when `price` reached its liquidation price:
    /// the fill goes to the log and to every basket leg holding it.
    fn check_liquidation(&mut self, symbol: &str, price: f64, now_ms: i64) {
        if !self.broker.is_liquidated_at(symbol, price) {
            return;
        }
        let before = self.broker.position(symbol);
        let Some(fill) = self.broker.liquidate(symbol, now_ms) else {
            return;
        };
        self.fill_log
            .push_back((now_ms, symbol.into(), fill.clone()));
        while self.fill_log.len() > 128 {
            self.fill_log.pop_front();
        }
        // The baskets holding it lose the leg: it is flat now.
        for b in self
            .baskets
            .baskets
            .iter_mut()
            .filter(|b| !b.state.is_closed())
        {
            for i in 0..b.legs.len() {
                let l = &b.legs[i];
                if l.symbol == symbol && l.held > 0.0 {
                    let closing = crate::strategies::context::Fill {
                        side: fill.side,
                        qty: l.held,
                        price: fill.price,
                        time_ms: now_ms,
                        order_id: 0,
                        fee: l.held * fill.price * crate::futures::LIQ_FEE_RATE,
                    };
                    b.on_fill_leg(i, &closing);
                    b.clear_order(i);
                }
            }
        }
        let text = format!(
            "LIQUIDATED {symbol}: {} {:.6} closed at {:.6} (x{}, entry {:.6}, loss ${:.2})",
            if before.qty > 0.0 { "long" } else { "short" },
            before.qty.abs(),
            fill.price,
            self.broker.leverage(symbol),
            before.avg,
            (before.qty * (fill.price - before.avg)).abs() + fill.fee,
        );
        self.futures_events.push(text.clone());
        self.push_basket_event(text);
        if self.futures_events.len() > 32 {
            self.futures_events.remove(0);
        }
    }

    /// Account the funding times that have passed since the last call, at the
    /// rates known (none known: nothing moves).  Returns the cash moved.
    pub fn tick_funding(&mut self, now_ms: i64) -> f64 {
        if self.last_funding_ms == 0 {
            self.last_funding_ms = now_ms;
            return 0.0;
        }
        let times = crate::futures::funding_times_between(self.last_funding_ms, now_ms);
        self.last_funding_ms = now_ms;
        if times <= 0 || self.read_only || self.is_live() {
            return 0.0;
        }
        let mut moved = 0.0;
        let perps: Vec<String> = self
            .broker
            .all_positions()
            .iter()
            .filter(|(s, p)| crate::futures::is_perp(s) && !p.is_flat())
            .map(|(s, _)| s.clone())
            .collect();
        for sym in perps {
            let rate = self
                .funding_rates
                .get(crate::futures::exchange_symbol(&sym))
                .copied()
                .unwrap_or(0.0);
            let mark = self.last_marks.get(&sym).copied().unwrap_or(0.0);
            if rate == 0.0 || mark <= 0.0 {
                continue;
            }
            for _ in 0..times {
                moved += self.broker.apply_funding(&sym, mark, rate);
            }
        }
        if moved != 0.0 {
            self.futures_events.push(format!(
                "funding: {}${:.4} on the perpetuals",
                if moved >= 0.0 { "+" } else { "-" },
                moved.abs()
            ));
        }
        moved
    }

    /// Switch to LIVE.  Strategies are stopped first (their paper state must
    /// not leak into the exchange), the paper ledger is parked, and `broker`
    /// becomes a fresh exchange mirror.  Everything slow (server time, listen
    /// key, first sync) happens on a background thread; its results arrive as
    /// events handled by [`pump_binance`](Self::pump_binance).
    pub fn enable_live(&mut self, api_key: String, api_secret: String, base_url: String) {
        if self.is_live() || crate::data::demo::is_on() {
            return;
        }
        let now = current_now_ms();
        for i in 0..self.slots.len() {
            if self.slots[i].status == SlotStatus::Running {
                let _ = self.stop(i, now);
                self.slots[i].push_log("stopped: switching to LIVE".into());
            }
        }
        let paper = std::mem::replace(&mut self.broker, PaperBroker::mirror());
        let risk = std::mem::take(&mut self.risk_state);
        let baskets = std::mem::take(&mut self.baskets);
        self.paper_stash = Some((paper, risk, baskets));
        self.reset_activity();
        let client = BinanceClient::new(api_key, api_secret, base_url);
        client.bootstrap();
        self.binance = Some(client);
    }

    pub fn disable_live(&mut self) {
        let now = current_now_ms();
        for i in 0..self.slots.len() {
            self.slots[i].status = SlotStatus::Idle;
            self.slots[i].push_log("stopped: leaving LIVE".into());
        }
        if let Some(ws) = self.user_data_ws.take() {
            let _ = ws.stop_tx.send(());
        }
        if let (Some(client), Some(key)) = (&self.binance, self.listen_key.take()) {
            client.delete_listen_key_async(key);
        }
        self.binance = None;
        if let Some((paper, risk, baskets)) = self.paper_stash.take() {
            self.broker = paper;
            self.risk_state = risk;
            self.baskets = baskets;
        }
        self.reset_activity();
        let _ = now;
    }

    /// Forget logs / ownership that belong to the ledger we just left.
    fn reset_activity(&mut self) {
        self.fill_log.clear();
        self.blocked_log.clear();
        self.equity_history.clear();
        self.equity_bucket = i64::MIN;
        self.order_owner.clear();
        self.owner_by_cid.clear();
        self.rest_filled.clear();
        self.seen_trades.clear();
        self.live_balances.clear();
        self.live_error = None;
    }

    pub fn is_live(&self) -> bool {
        self.binance.is_some()
    }

    /// The PAPER ledger and its risk state — the one to persist — even while
    /// live mode has parked it.
    pub fn paper_ledger(&self) -> (&PaperBroker, &RiskState, &Book) {
        match &self.paper_stash {
            Some((b, r, k)) => (b, r, k),
            None => (&self.broker, &self.risk_state, &self.baskets),
        }
    }

    /// Refresh the listen key (Binance expires after 60 min; keepalive every 30).
    pub fn keepalive_user_data_stream(&self) {
        let (Some(client), Some(key)) = (&self.binance, &self.listen_key) else {
            return;
        };
        client.keepalive_listen_key_async(key.clone());
    }

    fn note_live_error(&mut self, msg: String) {
        self.live_error = Some(msg);
        self.live_error_seq += 1;
    }

    pub fn equity(&self) -> f64 {
        self.broker.equity(&self.last_marks)
    }

    /// What the scalper's ledger made or lost so far (0 without one).
    pub fn scalper_pnl(&self) -> f64 {
        self.scalper
            .as_ref()
            .map_or(0.0, |b| b.equity(&self.last_marks) - b.starting_cash)
    }

    /// The equity the risk limits watch: this ledger plus the scalper's PnL
    /// (paper only; a live ledger is the exchange's and stands alone).
    pub fn risk_equity(&self) -> f64 {
        if self.is_live() {
            self.equity()
        } else {
            self.equity() + self.scalper_pnl()
        }
    }

    /// Start a fresh scalper ledger with `cash`.  The PnL of the one it
    /// replaces stays counted in today's loss and the drawdown peak: the
    /// risk baseline moves by that amount, so a new mandate never wipes a
    /// loss off the books.
    pub fn start_scalper_ledger(&mut self, cash: f64) -> Result<(), String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if self.is_live() {
            return Err("the scalper ledger is paper-only: leave live mode first".into());
        }
        if !(cash.is_finite() && cash > 0.0) {
            return Err("the scalper's balance must be positive".into());
        }
        let old = self.scalper_pnl();
        if self.risk_state.day_start_equity != 0.0 {
            self.risk_state.day_start_equity -= old;
        }
        if self.risk_state.equity_peak != 0.0 {
            self.risk_state.equity_peak -= old;
        }
        self.scalper = Some(PaperBroker::restore(super::broker::BrokerSnapshot {
            starting_cash: cash,
            positions: Vec::new(),
            open_orders: Vec::new(),
            next_id: SCALPER_FIRST_ORDER_ID,
            brackets: Vec::new(),
            fee_maker_bps: self.broker.fee_maker_bps,
            fee_taker_bps: self.broker.fee_taker_bps,
            fees_paid: 0.0,
            leverage: Vec::new(),
            funding: 0.0,
            fut_maker_bps: self.broker.fut_maker_bps,
            fut_taker_bps: self.broker.fut_taker_bps,
        }));
        self.scalper_fills.clear();
        Ok(())
    }

    /// An order of the scalper, to its own ledger, through the same risk
    /// gate as everyone else's (the position limit and the open-order count
    /// cover both ledgers; the kill switch lets it shrink only its own
    /// positions).  Paper only.
    pub fn submit_scalper(&mut self, action: Action, now_ms: i64) -> Result<Vec<Fill>, String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if self.is_live() {
            return Err("the scalper trades paper only: the terminal is in live mode".into());
        }
        let sym = action
            .symbol()
            .ok_or("scalper orders need a symbol")?
            .to_string();
        let Some(book) = self.scalper.as_ref() else {
            return Err("no scalper ledger: no mandate was issued".into());
        };
        let own = book.position(&sym);
        let other = self.broker.position(&sym).qty;
        let oo = self.broker.open_order_count() + book.open_order_count();
        if let Err(e) = check_action(&action, &own, other, &self.limits, oo) {
            self.blocked_log
                .push_back((now_ms, sym, format!("scalper: {e} ({})", action.label())));
            while self.blocked_log.len() > 64 {
                self.blocked_log.pop_front();
            }
            return Err(e);
        }
        let last = self.last_marks.get(&sym).copied().unwrap_or(0.0);
        let fills = match self.scalper.as_mut() {
            Some(b) => b.submit(&action, last, now_ms),
            None => Vec::new(),
        };
        Ok(fills)
    }

    /// Drop one resting scalper order.  `true` when it existed.
    pub fn cancel_scalper_order(&mut self, id: u64) -> bool {
        if self.read_only {
            return false;
        }
        self.scalper.as_mut().is_some_and(|b| b.drop_order(id))
    }

    /// Open-position unrealized PnL summed across symbols (broker marks
    /// against `last_marks`).  Used by the F5 Dashboard "Active P&L" line.
    pub fn unrealized_total(&self) -> f64 {
        self.broker
            .all_positions()
            .iter()
            .filter(|(_, p)| !p.is_flat())
            .map(|(s, p)| p.unrealized(self.last_marks.get(s).copied().unwrap_or(p.avg)))
            .sum()
    }

    /// Realized PnL accumulated across all closed legs (per symbol).
    pub fn realized_total(&self) -> f64 {
        self.broker
            .all_positions()
            .values()
            .map(|p| p.realized)
            .sum()
    }

    /// Equity delta vs. start-of-day snapshot, both ledgers together (what
    /// the daily loss limit watches); rolls over via
    /// `RiskState::on_equity_update`.
    pub fn day_pnl(&self) -> f64 {
        // No baseline yet (a fresh or view-only runtime that has not seen a
        // tick): nothing happened today, not "the whole equity".
        if self.risk_state.day_start_equity == 0.0 {
            return 0.0;
        }
        self.risk_equity() - self.risk_state.day_start_equity
    }

    pub fn last_mark(&self, symbol: &str) -> Option<f64> {
        self.last_marks.get(symbol).copied()
    }

    /// Append a line to a slot's log (no-op for a bad index).
    pub fn slot_log(&mut self, idx: usize, msg: &str) {
        if let Some(s) = self.slots.get_mut(idx) {
            s.push_log(msg.to_string());
        }
    }

    pub fn find_slot(&self, symbol: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.symbol == symbol)
    }

    pub fn add_slot(
        &mut self,
        symbol: String,
        venue: String,
        strategy_id: String,
        strategy: Box<dyn Strategy>,
    ) -> usize {
        if let Some(i) = self.find_slot(&symbol) {
            return i;
        }
        self.slots.push(Slot {
            symbol,
            venue,
            strategy_id,
            strategy,
            status: SlotStatus::Idle,
            log: VecDeque::with_capacity(32),
            bar_history: Vec::new(),
            last_timer_at: 0,
            error: None,
        });
        self.slots.len() - 1
    }

    pub fn replace_strategy(
        &mut self,
        idx: usize,
        strategy_id: String,
        strategy: Box<dyn Strategy>,
    ) {
        if idx >= self.slots.len() {
            return;
        }
        // Stopping ensures any open limit orders this strategy placed get cancelled.
        let _ = self.stop(idx, current_now_ms());
        let s = &mut self.slots[idx];
        s.strategy = strategy;
        s.strategy_id = strategy_id;
        s.status = SlotStatus::Idle;
        s.error = None;
    }

    pub fn remove_slot(&mut self, idx: usize) {
        if idx < self.slots.len() {
            let _ = self.stop(idx, current_now_ms());
            let sym = self.slots[idx].symbol.clone();
            self.order_owner
                .retain(|_, owner| !matches!(owner, Owner::Slot(s) if *s == sym));
            self.slots.remove(idx);
        }
    }

    pub fn start(&mut self, idx: usize, now_ms: i64) -> Result<(), String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if idx >= self.slots.len() {
            return Err("slot OOB".into());
        }
        let last = self
            .last_marks
            .get(&self.slots[idx].symbol)
            .copied()
            .unwrap_or(0.0);
        let mut actions: Vec<Action> = Vec::new();
        {
            let symbol = self.slots[idx].symbol.clone();
            let position = self.broker.position(&symbol);
            let equity = self.broker.equity(&self.last_marks);
            let bars: Vec<Bar> = self.slots[idx].bar_history.clone();
            let mut ctx = Ctx {
                symbol: &symbol,
                now_ms,
                last,
                position,
                equity,
                actions: &mut actions,
                bars: &bars,
            };
            self.slots[idx].strategy.on_start(&mut ctx);
        }
        self.slots[idx].status = SlotStatus::Running;
        self.slots[idx].error = None;
        self.execute_actions(idx, actions, last, now_ms);
        Ok(())
    }

    pub fn stop(&mut self, idx: usize, now_ms: i64) -> Result<(), String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if idx >= self.slots.len() {
            return Err("slot OOB".into());
        }
        let last = self
            .last_marks
            .get(&self.slots[idx].symbol)
            .copied()
            .unwrap_or(0.0);
        let mut actions: Vec<Action> = Vec::new();
        {
            let symbol = self.slots[idx].symbol.clone();
            let position = self.broker.position(&symbol);
            let equity = self.broker.equity(&self.last_marks);
            let bars: Vec<Bar> = self.slots[idx].bar_history.clone();
            let mut ctx = Ctx {
                symbol: &symbol,
                now_ms,
                last,
                position,
                equity,
                actions: &mut actions,
                bars: &bars,
            };
            self.slots[idx].strategy.on_stop(&mut ctx);
        }
        self.slots[idx].status = SlotStatus::Idle;
        self.execute_actions(idx, actions, last, now_ms);
        Ok(())
    }

    /// Feed a price tick into the runtime.  Updates marks, fills any
    /// crossed limit orders, then dispatches `on_tick` to every running
    /// strategy that watches this symbol.
    pub fn on_price(&mut self, symbol: &str, price: f64, now_ms: i64) {
        if price <= 0.0 {
            return;
        }
        self.last_marks.insert(symbol.into(), price);
        self.last_mark_update_ms = now_ms;
        self.ws_tick_count = self.ws_tick_count.saturating_add(1);
        self.last_tick_per_symbol
            .insert(symbol.into(), (price, now_ms));

        if self.read_only {
            // A copy of someone else's ledger: show it marked to market,
            // never fill, never run a strategy, never touch the risk state.
            let eq = self.broker.equity(&self.last_marks);
            self.push_equity(eq, now_ms);
            return;
        }

        let fills = self.broker.on_tick(symbol, price, now_ms);
        for f in fills {
            self.fill_log.push_back((now_ms, symbol.into(), f.clone()));
            while self.fill_log.len() > 128 {
                self.fill_log.pop_front();
            }
            self.notify_owner(&f, symbol, price, now_ms);
        }
        self.check_liquidation(symbol, price, now_ms);
        self.tick_funding(now_ms);
        self.check_bracket(symbol, price, now_ms);
        if let Some(book) = self.scalper.as_mut() {
            for f in book.on_tick(symbol, price, now_ms) {
                self.scalper_fills.push((symbol.into(), f));
            }
            if self.scalper_fills.len() > SCALPER_FILLS_CAP {
                let extra = self.scalper_fills.len() - SCALPER_FILLS_CAP;
                self.scalper_fills.drain(..extra);
            }
        }

        for i in 0..self.slots.len() {
            if self.slots[i].symbol != symbol || self.slots[i].status != SlotStatus::Running {
                continue;
            }
            let mut actions: Vec<Action> = Vec::new();
            {
                let symbol = self.slots[i].symbol.clone();
                let position = self.broker.position(&symbol);
                let equity = self.broker.equity(&self.last_marks);
                let bars: Vec<Bar> = self.slots[i].bar_history.clone();
                let mut ctx = Ctx {
                    symbol: &symbol,
                    now_ms,
                    last: price,
                    position,
                    equity,
                    actions: &mut actions,
                    bars: &bars,
                };
                self.slots[i].strategy.on_tick(
                    &mut ctx,
                    &Tick {
                        price,
                        time_ms: now_ms,
                    },
                );
            }
            self.execute_actions(i, actions, price, now_ms);
        }

        let risk_eq = self.risk_equity();
        self.risk_state.on_equity_update(risk_eq, day_id(now_ms));
        enforce_aggregate(&mut self.limits, &mut self.risk_state, risk_eq);
        let eq = self.broker.equity(&self.last_marks);
        self.push_equity(eq, now_ms);
    }

    /// Set the stop / take of `symbol`'s position (an empty bracket removes
    /// them).  Levels must sit on the right side of the current price.
    pub fn set_bracket(&mut self, symbol: &str, mut b: Bracket) -> Result<(), String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        if !b.is_empty() {
            let pos = self.broker.position(symbol);
            let price = self.last_marks.get(symbol).copied().unwrap_or(pos.avg);
            b.check(pos.qty, price)?;
            b.long = pos.qty > 0.0;
        }
        self.broker.set_bracket(symbol, b);
        Ok(())
    }

    /// Close `symbol`'s whole position at market when the price reaches its
    /// stop or take; a bracket outlives no position.  The exit goes through
    /// the risk gate like any user order (a close always passes it).
    fn check_bracket(&mut self, symbol: &str, price: f64, now_ms: i64) {
        let b = self.broker.bracket(symbol);
        if b.is_empty() {
            return;
        }
        let pos = self.broker.position(symbol);
        if pos.is_flat() || (pos.qty > 0.0) != b.long {
            // Closed or reversed: these levels protected another position.
            self.broker.set_bracket(symbol, Bracket::default());
            return;
        }
        let Some(exit) = b.hit(pos.qty, price) else {
            return;
        };
        // Removed first: one exit per crossing, never a loop of closes.
        self.broker.set_bracket(symbol, Bracket::default());
        let qty = pos.qty.abs();
        let action = if pos.qty > 0.0 {
            Action::SellMarket {
                symbol: symbol.to_string(),
                qty,
            }
        } else {
            Action::BuyMarket {
                symbol: symbol.to_string(),
                qty,
            }
        };
        let upnl = pos.unrealized(price);
        // Eight decimals at most: a qty from a dollar size is 0.0000742948...
        let shown = (qty * 1e8).round() / 1e8;
        let text = match self.submit_user(action, now_ms) {
            Ok(_) => format!(
                "{} {symbol}: closed {shown} at {price} ({}{:.2})",
                exit.label(),
                if upnl >= 0.0 { "+$" } else { "-$" },
                upnl.abs()
            ),
            Err(e) => format!(
                "{} {symbol} reached {price}, close refused: {e}",
                exit.label()
            ),
        };
        if let Some(i) = self.find_slot(symbol) {
            self.slots[i].push_log(text.clone());
        }
        self.exit_events.push(text);
        if self.exit_events.len() > 32 {
            self.exit_events.remove(0);
        }
    }

    /// Record the latest equity: a new sample every `EQUITY_SAMPLE_MS`,
    /// in between the newest sample is updated in place.  Capped at
    /// `EQUITY_HISTORY_CAP`; drops the oldest sample when full.
    fn push_equity(&mut self, eq: f64, now_ms: i64) {
        let bucket = now_ms.div_euclid(EQUITY_SAMPLE_MS);
        if bucket == self.equity_bucket {
            if let Some(last) = self.equity_history.back_mut() {
                *last = eq;
                return;
            }
        }
        self.equity_history.push_back(eq);
        self.equity_bucket = bucket;
        while self.equity_history.len() > EQUITY_HISTORY_CAP {
            self.equity_history.pop_front();
        }
    }

    /// Feed a closed bar into the runtime.  Updates the slot's bar history
    /// (capped at 1024 bars) and dispatches `on_bar`.
    pub fn on_bar(&mut self, symbol: &str, bar: Bar, now_ms: i64) {
        if self.read_only {
            return;
        }
        for i in 0..self.slots.len() {
            if self.slots[i].symbol != symbol {
                continue;
            }
            self.slots[i].bar_history.push(bar.clone());
            if self.slots[i].bar_history.len() > 1024 {
                let drop = self.slots[i].bar_history.len() - 1024;
                self.slots[i].bar_history.drain(..drop);
            }
            if self.slots[i].status != SlotStatus::Running {
                continue;
            }
            let mut actions: Vec<Action> = Vec::new();
            let last = self.last_marks.get(symbol).copied().unwrap_or(bar.close);
            {
                let symbol = self.slots[i].symbol.clone();
                let position = self.broker.position(&symbol);
                let equity = self.broker.equity(&self.last_marks);
                let bars: Vec<Bar> = self.slots[i].bar_history.clone();
                let mut ctx = Ctx {
                    symbol: &symbol,
                    now_ms,
                    last,
                    position,
                    equity,
                    actions: &mut actions,
                    bars: &bars,
                };
                self.slots[i].strategy.on_bar(&mut ctx, &bar);
            }
            self.execute_actions(i, actions, last, now_ms);
        }
    }

    /// Drive timers on every running slot.  Cheap: each strategy
    /// self-gates with `next_at_ms`, so calling per-frame is fine.
    pub fn dispatch_timers(&mut self, now_ms: i64) {
        if self.read_only {
            return;
        }
        for i in 0..self.slots.len() {
            if self.slots[i].status != SlotStatus::Running {
                continue;
            }
            let timer = self.slots[i].strategy.meta().timer_secs;
            if timer == 0 {
                continue;
            }
            let due = self.slots[i].last_timer_at + (timer as i64) * 1000;
            if now_ms < due {
                continue;
            }
            self.slots[i].last_timer_at = now_ms;
            let symbol = self.slots[i].symbol.clone();
            let last = self.last_marks.get(&symbol).copied().unwrap_or(0.0);
            let mut actions: Vec<Action> = Vec::new();
            {
                let position = self.broker.position(&symbol);
                let equity = self.broker.equity(&self.last_marks);
                let bars: Vec<Bar> = self.slots[i].bar_history.clone();
                let mut ctx = Ctx {
                    symbol: &symbol,
                    now_ms,
                    last,
                    position,
                    equity,
                    actions: &mut actions,
                    bars: &bars,
                };
                self.slots[i].strategy.on_timer(&mut ctx);
            }
            self.execute_actions(i, actions, last, now_ms);
        }
    }

    /// Forward a user-typed action (e.g. from the terminal command bar)
    /// through the risk gate to the broker.  Logged on the slot if one
    /// matches the action's symbol; otherwise into a dedicated channel.
    pub fn submit_user(&mut self, action: Action, now_ms: i64) -> Result<Vec<Fill>, String> {
        self.submit_owned(action, None, now_ms).map(|s| s.fills)
    }

    /// `submit_user` with an owner for the orders that rest (a basket):
    /// the same checks and logs, plus the ids of the resting orders.
    fn submit_owned(
        &mut self,
        action: Action,
        owner: Option<Owner>,
        now_ms: i64,
    ) -> Result<Submitted, String> {
        if self.read_only {
            return Err(VIEW_ONLY.into());
        }
        let sym = match action.symbol() {
            Some(s) => s.to_string(),
            None => {
                self.fill_log.push_back((
                    now_ms,
                    "USER".into(),
                    Fill {
                        side: crate::strategies::context::Side::Buy,
                        qty: 0.0,
                        price: 0.0,
                        time_ms: now_ms,
                        order_id: 0,
                        fee: 0.0,
                    },
                ));
                return Ok(Submitted::default());
            }
        };
        let last = self.last_marks.get(&sym).copied().unwrap_or(0.0);
        let pos = self.broker.position(&sym);
        let (other, oo_count) = self.other_ledger(&sym);
        if let Err(e) = check_action(&action, &pos, other, &self.limits, oo_count)
            .and_then(|()| self.margin_for(&action, last))
        {
            self.blocked_log
                .push_back((now_ms, sym.clone(), format!("{e} ({})", action.label())));
            while self.blocked_log.len() > 64 {
                self.blocked_log.pop_front();
            }
            if let Some(i) = self.find_slot(&sym) {
                self.slots[i].push_log(format!("BLOCKED: {} ({e})", action.label()));
            }
            return Err(e);
        }

        // Live mode: forward to Binance.  Fills come back asynchronously
        // through `pump_binance` and are recorded in the exchange mirror.
        if self.binance.is_some() {
            if let Some(why) = self.live_block_reason(&sym) {
                self.blocked_log.push_back((
                    now_ms,
                    sym.clone(),
                    format!("{why} ({})", action.label()),
                ));
                return Err(why);
            }
            match &action {
                // A user CancelAll cancels everything resting on the symbol.
                Action::CancelAll { symbol } => {
                    let ids: Vec<u64> = self
                        .broker
                        .all_open_orders()
                        .iter()
                        .filter(|o| o.symbol == *symbol)
                        .map(|o| o.id)
                        .collect();
                    if let Some(client) = &self.binance {
                        for id in ids {
                            client.cancel(symbol.clone(), id);
                        }
                    }
                }
                _ => {
                    let cid = BinanceClient::new_client_id();
                    if let Some(client) = &self.binance {
                        client.submit(action.clone(), cid);
                    }
                }
            }
            if let Some(i) = self.find_slot(&sym) {
                self.slots[i].push_log(format!("LIVE {} → Binance", action.label()));
            }
            return Ok(Submitted::default());
        }

        if let Action::CancelAll { symbol } = &action {
            self.order_owner.retain(|id, _| {
                self.broker
                    .all_open_orders()
                    .iter()
                    .any(|o| o.id == *id && o.symbol != *symbol)
            });
        }
        let first_new_id = self.broker.peek_next_order_id();
        let fills = self.broker.submit(&action, last, now_ms);
        let order_ids: Vec<u64> = (first_new_id..self.broker.peek_next_order_id()).collect();
        if let Some(owner) = &owner {
            for id in &order_ids {
                self.order_owner.insert(*id, owner.clone());
            }
        }
        if let Some(i) = self.find_slot(&sym) {
            self.slots[i].push_log(format!(
                "{} {}",
                match &owner {
                    Some(Owner::Basket(id)) => format!("BASKET #{id}"),
                    _ => "USER".to_string(),
                },
                action.label()
            ));
        }
        for f in &fills {
            self.fill_log.push_back((now_ms, sym.clone(), f.clone()));
        }
        while self.fill_log.len() > 128 {
            self.fill_log.pop_front();
        }
        Ok(Submitted { fills, order_ids })
    }

    // ── Baskets (`crate::basket`): one unit of several legs ──────────────

    /// Why baskets cannot be worked right now, if so.
    fn basket_block(&self) -> Option<String> {
        if self.read_only {
            Some(VIEW_ONLY.into())
        } else if self.is_live() {
            Some("baskets are paper only (:paper first)".into())
        } else {
            None
        }
    }

    /// A basket with a short coin leg can only be held on perpetuals: all its
    /// coin legs move there, at the basket's leverage (or the default one),
    /// and the perpetual's mark starts from the spot price until its own
    /// feed brings one.
    fn instrumentize(&self, b: &mut Basket) {
        if self.perp_shorts && b.needs_perps() {
            let lev = if b.leverage > 1.0 {
                b.leverage
            } else {
                self.default_leverage
            };
            b.move_to_perps(lev);
        }
    }

    /// Marks for the perpetuals a basket uses, from their spot prices when
    /// no futures price is known yet.
    fn seed_perp_marks(&mut self, b: &Basket, now_ms: i64) {
        for l in &b.legs {
            if crate::futures::is_perp(&l.symbol) && !self.last_marks.contains_key(&l.symbol) {
                if let Some(&spot) = self
                    .last_marks
                    .get(crate::futures::exchange_symbol(&l.symbol))
                {
                    self.last_marks.insert(l.symbol.clone(), spot);
                    self.last_tick_per_symbol
                        .insert(l.symbol.clone(), (spot, now_ms));
                }
            }
        }
    }

    /// Open a basket: every leg passes the risk gate before any order goes
    /// out (all or nothing at submission), then the entry orders are placed.
    /// Returns the basket's id.
    pub fn open_basket(&mut self, mut b: Basket, now_ms: i64) -> Result<u64, String> {
        if let Some(why) = self.basket_block() {
            return Err(why);
        }
        self.instrumentize(&mut b);
        self.seed_perp_marks(&b, now_ms);
        // The basket's own leverage wins; else what the instrument has; else
        // the default one.
        for l in &b.legs {
            if !crate::futures::is_perp(&l.symbol) {
                continue;
            }
            if b.leverage > 1.0 {
                self.broker.set_leverage(&l.symbol, b.leverage);
            } else if !self.broker.has_leverage(&l.symbol) {
                self.broker.set_leverage(&l.symbol, self.default_leverage);
            }
        }
        let l = &self.limits;
        if b.legs.len() > l.max_basket_legs {
            return Err(format!(
                "a basket has {} legs at most (:risk legs)",
                l.max_basket_legs
            ));
        }
        let usd = b.total_usd();
        if usd > l.max_basket_usd {
            return Err(format!(
                "basket ${usd:.2} over the limit ${:.2} (:risk basket)",
                l.max_basket_usd
            ));
        }
        let open = self.baskets.open_notional();
        if open + usd > l.max_baskets_usd {
            return Err(format!(
                "all baskets would hold ${:.2}, over the limit ${:.2} (:risk baskets)",
                open + usd,
                l.max_baskets_usd
            ));
        }
        b.size(&self.last_marks)?;
        // Perpetual legs are held on margin: all of it must be there at once.
        let need: f64 = b
            .legs
            .iter()
            .map(|l| {
                let mark = self.last_marks.get(&l.symbol).copied().unwrap_or(0.0);
                self.margin_needed(&l.symbol, l.long, l.target_qty, mark)
            })
            .sum();
        if need > self.free_margin() + 1e-9 {
            return Err(format!(
                "not enough margin: the basket needs ${need:.2}, free ${:.2}",
                self.free_margin()
            ));
        }
        for (i, leg) in b.legs.iter().enumerate() {
            let action = match (b.entry, leg.long) {
                (basket::Entry::Market, true) => Action::BuyMarket {
                    symbol: leg.symbol.clone(),
                    qty: leg.target_qty,
                },
                (basket::Entry::Market, false) => Action::SellMarket {
                    symbol: leg.symbol.clone(),
                    qty: leg.target_qty,
                },
                (basket::Entry::Limit { .. }, true) => Action::BuyLimit {
                    symbol: leg.symbol.clone(),
                    qty: leg.target_qty,
                    limit: 1.0,
                },
                (basket::Entry::Limit { .. }, false) => Action::SellLimit {
                    symbol: leg.symbol.clone(),
                    qty: leg.target_qty,
                    limit: 1.0,
                },
            };
            let pos = self.broker.position(&leg.symbol);
            let (other, oo) = self.other_ledger(&leg.symbol);
            check_action(&action, &pos, other, &self.limits, oo + i)
                .map_err(|e| format!("leg {}: {e}", leg.symbol))?;
        }
        let id = self.baskets.next_id();
        b.id = id;
        b.start(now_ms);
        let intents = b.step(now_ms, &self.last_marks);
        self.baskets.baskets.push(b);
        self.run_intents(id, intents, now_ms);
        Ok(id)
    }

    /// What opening `baskets` one after another would do, without touching
    /// anything: the same checks as [`Runtime::open_basket`], each basket
    /// counted on top of the ones before it (dollars, resting orders, the
    /// position its legs would add up to).  One result per basket.
    pub fn dry_run_baskets(&self, baskets: &[Basket]) -> Vec<Result<(), String>> {
        let l = &self.limits;
        let mut pending_usd = 0.0;
        let mut pending_orders = 0usize;
        let mut pending_margin = 0.0f64;
        let mut pending_qty: std::collections::HashMap<String, f64> = Default::default();
        let mut out = Vec::new();
        for b in baskets {
            let r = (|| -> Result<(), String> {
                if let Some(why) = self.basket_block() {
                    return Err(why);
                }
                if b.legs.len() > l.max_basket_legs {
                    return Err(format!(
                        "{} legs, over the limit {}",
                        b.legs.len(),
                        l.max_basket_legs
                    ));
                }
                let mut sized = b.clone();
                self.instrumentize(&mut sized);
                let mut marks = self.last_marks.clone();
                for l in &sized.legs {
                    if crate::futures::is_perp(&l.symbol) && !marks.contains_key(&l.symbol) {
                        if let Some(&spot) = marks.get(crate::futures::exchange_symbol(&l.symbol)) {
                            marks.insert(l.symbol.clone(), spot);
                        }
                    }
                }
                sized.size(&marks)?;
                let usd = sized.total_usd();
                if usd > l.max_basket_usd {
                    return Err(format!(
                        "${usd:.2} over the basket limit ${:.2}",
                        l.max_basket_usd
                    ));
                }
                let need: f64 = sized
                    .legs
                    .iter()
                    .map(|l| {
                        let mark = marks.get(&l.symbol).copied().unwrap_or(0.0);
                        self.margin_needed(&l.symbol, l.long, l.target_qty, mark)
                    })
                    .sum();
                if pending_margin + need > self.free_margin() + 1e-9 {
                    return Err(format!(
                        "not enough margin: ${:.2} needed, free ${:.2}",
                        pending_margin + need,
                        self.free_margin()
                    ));
                }
                pending_margin += need;
                let held = self.baskets.open_notional() + pending_usd;
                if held + usd > l.max_baskets_usd {
                    return Err(format!(
                        "all baskets would hold ${:.2}, over ${:.2}",
                        held + usd,
                        l.max_baskets_usd
                    ));
                }
                let mut add: std::collections::HashMap<String, f64> = Default::default();
                for (i, leg) in sized.legs.iter().enumerate() {
                    let signed = if leg.long {
                        leg.target_qty
                    } else {
                        -leg.target_qty
                    };
                    let action = if leg.long {
                        Action::BuyLimit {
                            symbol: leg.symbol.clone(),
                            qty: leg.target_qty,
                            limit: 1.0,
                        }
                    } else {
                        Action::SellLimit {
                            symbol: leg.symbol.clone(),
                            qty: leg.target_qty,
                            limit: 1.0,
                        }
                    };
                    let mut pos = self.broker.position(&leg.symbol);
                    pos.qty += pending_qty.get(&leg.symbol).copied().unwrap_or(0.0)
                        + add.get(&leg.symbol).copied().unwrap_or(0.0);
                    let (other, oo) = self.other_ledger(&leg.symbol);
                    check_action(&action, &pos, other, l, oo + pending_orders + i)
                        .map_err(|e| format!("{}: {e}", leg.symbol))?;
                    *add.entry(leg.symbol.clone()).or_default() += signed;
                }
                for (k, v) in add {
                    *pending_qty.entry(k).or_default() += v;
                }
                pending_usd += usd;
                pending_orders += sized.legs.len();
                Ok(())
            })();
            out.push(r);
        }
        out
    }

    /// What [`Runtime::automation_guard`] would set: (limit, value, words).
    fn guard_steps(&self) -> Vec<(u8, f64, String)> {
        use crate::i18n::pick;
        const OPEN: f64 = 1.0e8;
        let eq = self.equity().max(1.0);
        let mut v = Vec::new();
        if self.limits.max_baskets_usd >= OPEN {
            let x = (eq * 0.5).round();
            v.push((
                0,
                x,
                pick(
                    format!("all baskets together at most ${x:.0} (50% of equity)"),
                    format!("усі кошики разом не більше ${x:.0} (50% капіталу)"),
                ),
            ));
        }
        if self.limits.max_basket_usd >= OPEN {
            let x = (eq * 0.1).round();
            v.push((
                1,
                x,
                pick(
                    format!("one basket at most ${x:.0} (10% of equity)"),
                    format!("один кошик не більше ${x:.0} (10% капіталу)"),
                ),
            ));
        }
        if self.limits.max_daily_loss >= OPEN {
            let x = (eq * 0.05).round();
            v.push((
                2,
                x,
                pick(
                    format!("stop at a day's loss of ${x:.0} (5% of equity)"),
                    format!("стоп при збитку за день ${x:.0} (5% капіталу)"),
                ),
            ));
        }
        if self.limits.max_drawdown_pct >= 100.0 {
            v.push((
                3,
                10.0,
                pick("stop at a 10% drawdown", "стоп при просіданні 10%").to_string(),
            ));
        }
        v
    }

    /// What the guard would set, in words (nothing changes).
    pub fn automation_guard_preview(&self) -> Vec<String> {
        self.guard_steps().into_iter().map(|s| s.2).collect()
    }

    /// Before baskets run by themselves: every risk limit still at its
    /// "unlimited" default is set to a share of the equity.  Limits only
    /// tighten here, never loosen.  Returns what was set, in words.
    pub fn automation_guard(&mut self) -> Vec<String> {
        let steps = self.guard_steps();
        for (field, x, _) in &steps {
            match field {
                0 => self.limits.max_baskets_usd = *x,
                1 => self.limits.max_basket_usd = *x,
                2 => self.limits.max_daily_loss = *x,
                _ => self.limits.max_drawdown_pct = *x,
            }
        }
        steps.into_iter().map(|s| s.2).collect()
    }

    /// Close a basket now (an open one), or give one up while it enters.
    pub fn close_basket(&mut self, id: u64, now_ms: i64) -> Result<(), String> {
        self.exit_basket(id, basket::Why::User, now_ms)
    }

    /// Ask a basket to leave for `why` (a rule's own reason, such as a
    /// spread that came back): one still entering is abandoned instead.
    pub fn exit_basket(&mut self, id: u64, why: basket::Why, now_ms: i64) -> Result<(), String> {
        if let Some(why) = self.basket_block() {
            return Err(why);
        }
        let marks = self.last_marks.clone();
        let b = self
            .baskets
            .get_mut(id)
            .ok_or_else(|| format!("no basket #{id}"))?;
        if b.state.is_closed() {
            return Err(format!("basket #{id} is closed already"));
        }
        let why = match b.state {
            basket::State::Entering { .. } => basket::Why::Abandoned,
            _ => why,
        };
        let intents = b.request_exit(why, now_ms, &marks);
        self.run_intents(id, intents, now_ms);
        Ok(())
    }

    /// Let a blocked basket try its orders again.
    pub fn retry_basket(&mut self, id: u64, now_ms: i64) -> Result<(), String> {
        if let Some(why) = self.basket_block() {
            return Err(why);
        }
        let b = self
            .baskets
            .get_mut(id)
            .ok_or_else(|| format!("no basket #{id}"))?;
        b.blocked = None;
        self.tick_baskets(now_ms);
        Ok(())
    }

    pub fn set_basket_exit(&mut self, id: u64, exit: basket::ExitRule) -> Result<(), String> {
        if let Some(why) = self.basket_block() {
            return Err(why);
        }
        let b = self
            .baskets
            .get_mut(id)
            .ok_or_else(|| format!("no basket #{id}"))?;
        if b.state.is_closed() {
            return Err(format!("basket #{id} is closed already"));
        }
        b.exit = exit;
        Ok(())
    }

    /// Drive every basket: entry and exit orders, the wait, the exit rule.
    /// Called from the main loop after the marks are pushed; nothing happens
    /// in a view-only window or in live mode.
    pub fn tick_baskets(&mut self, now_ms: i64) {
        if self.basket_block().is_some() {
            return;
        }
        let marks = self.last_marks.clone();
        let ids: Vec<u64> = self
            .baskets
            .baskets
            .iter()
            .filter(|b| !b.state.is_closed())
            .map(|b| b.id)
            .collect();
        for id in ids {
            let Some(b) = self.baskets.get_mut(id) else {
                continue;
            };
            let intents = b.step(now_ms, &marks);
            self.run_intents(id, intents, now_ms);
        }
        for b in self.baskets.baskets.iter().filter(|b| b.state.is_closed()) {
            if self.closed_seen.insert((b.id, b.created_ms)) {
                self.closed_out.push(b.clone());
            }
        }
        self.baskets.retire();
    }

    /// Execute a basket's intents on the paper broker.  A refused order
    /// blocks the basket (the user drops or retries it) — except an exit
    /// limit refused under the kill switch, which goes to market instead.
    fn run_intents(&mut self, id: u64, intents: Vec<Intent>, now_ms: i64) {
        for intent in intents {
            let Some(b) = self.baskets.get(id) else {
                return;
            };
            // A fill steps the basket again inside this loop: what the
            // earlier step asked for a leg may be done by now.
            let stale = |leg: usize| {
                b.legs.get(leg).is_none_or(|l| {
                    l.order.is_some()
                        || match b.state {
                            basket::State::Entering { .. } => l.entered(),
                            basket::State::Exiting { .. } => l.flat(),
                            _ => true,
                        }
                })
            };
            match intent {
                Intent::Note(text) => self.push_basket_event(text),
                Intent::Limit { leg, .. } | Intent::Market { leg, .. } if stale(leg) => {}
                Intent::Cancel { leg, order }
                    if b.legs.get(leg).is_none_or(|l| l.order != Some(order)) => {}
                Intent::Cancel { leg, order } => {
                    self.cancel_paper_order(order);
                    if let Some(b) = self.baskets.get_mut(id) {
                        b.clear_order(leg);
                    }
                }
                Intent::Limit {
                    leg,
                    side,
                    qty,
                    limit,
                } => {
                    let Some(symbol) = b.legs.get(leg).map(|l| l.symbol.clone()) else {
                        continue;
                    };
                    let exiting = matches!(b.state, basket::State::Exiting { .. });
                    let action = match side {
                        Side::Buy => Action::BuyLimit {
                            symbol: symbol.clone(),
                            qty,
                            limit,
                        },
                        Side::Sell => Action::SellLimit {
                            symbol: symbol.clone(),
                            qty,
                            limit,
                        },
                    };
                    match self.submit_owned(action, Some(Owner::Basket(id)), now_ms) {
                        Ok(s) => self.take_submitted(id, leg, s, now_ms),
                        Err(e) if exiting && e.contains("kill switch") => {
                            // Shrinking is allowed: leave at market instead.
                            let action = match side {
                                Side::Buy => Action::BuyMarket {
                                    symbol: symbol.clone(),
                                    qty,
                                },
                                Side::Sell => Action::SellMarket {
                                    symbol: symbol.clone(),
                                    qty,
                                },
                            };
                            match self.submit_owned(action, Some(Owner::Basket(id)), now_ms) {
                                Ok(s) => self.take_submitted(id, leg, s, now_ms),
                                Err(e) => self.block_basket(id, &symbol, &e),
                            }
                        }
                        Err(e) => self.block_basket(id, &symbol, &e),
                    }
                }
                Intent::Market { leg, side, qty } => {
                    let Some(symbol) = b.legs.get(leg).map(|l| l.symbol.clone()) else {
                        continue;
                    };
                    let action = match side {
                        Side::Buy => Action::BuyMarket {
                            symbol: symbol.clone(),
                            qty,
                        },
                        Side::Sell => Action::SellMarket {
                            symbol: symbol.clone(),
                            qty,
                        },
                    };
                    match self.submit_owned(action, Some(Owner::Basket(id)), now_ms) {
                        Ok(s) => self.take_submitted(id, leg, s, now_ms),
                        Err(e) => self.block_basket(id, &symbol, &e),
                    }
                }
            }
        }
    }

    /// Give a basket what its order did: a resting id, or the fills (after
    /// which it may be complete: it steps again at once).
    fn take_submitted(&mut self, id: u64, leg: usize, s: Submitted, now_ms: i64) {
        let Some(b) = self.baskets.get_mut(id) else {
            return;
        };
        if let Some(&oid) = s.order_ids.first() {
            b.set_order(leg, oid);
        }
        for f in &s.fills {
            b.on_fill_leg(leg, f);
        }
        if !s.fills.is_empty() {
            let marks = self.last_marks.clone();
            let intents = self
                .baskets
                .get_mut(id)
                .map(|b| b.step(now_ms, &marks))
                .unwrap_or_default();
            self.run_intents(id, intents, now_ms);
        }
    }

    fn block_basket(&mut self, id: u64, symbol: &str, why: &str) {
        let text =
            format!("basket #{id}: {symbol} refused: {why} (:basket drop {id} / retry {id})");
        if let Some(b) = self.baskets.get_mut(id) {
            b.blocked = Some(why.to_string());
        }
        self.push_basket_event(text);
    }

    fn push_basket_event(&mut self, text: String) {
        self.basket_events.push(text);
        if self.basket_events.len() > 32 {
            self.basket_events.remove(0);
        }
    }

    /// For the risk gate of `broker`'s orders: what the scalper's ledger holds
    /// in `symbol`, and the open orders of both ledgers.  The scalper is paper:
    /// in live mode it does not count against the exchange ledger.
    fn other_ledger(&self, symbol: &str) -> (f64, usize) {
        let own = self.broker.open_order_count();
        match self.scalper.as_ref().filter(|_| !self.is_live()) {
            Some(b) => (b.position(symbol).qty, own + b.open_order_count()),
            None => (0.0, own),
        }
    }

    /// Why a LIVE order for `symbol` must not be sent, if any.  Futures is
    /// not implemented: sending a Futures slot's order to the Spot endpoint
    /// would trade the wrong instrument (audit A7).
    fn live_block_reason(&self, symbol: &str) -> Option<String> {
        let futures = self
            .slots
            .iter()
            .find(|s| s.symbol == symbol)
            .is_some_and(|s| s.venue.to_ascii_uppercase().contains("FUT"));
        futures.then(|| "live Futures trading is not implemented".to_string())
    }

    /// Tell the slot that placed the order behind `fill` (and only that slot).
    fn notify_owner(&mut self, fill: &Fill, symbol: &str, last: f64, now_ms: i64) {
        let owner = if fill.order_id != 0 {
            self.order_owner.get(&fill.order_id).cloned()
        } else {
            None
        };
        let Some(owner) = owner else { return };
        match owner {
            Owner::Slot(owner) => {
                if let Some(i) = self.slots.iter().position(|s| s.symbol == owner) {
                    if self.slots[i].symbol == symbol && self.slots[i].status == SlotStatus::Running
                    {
                        self.dispatch_fill(i, fill, last, now_ms);
                    }
                }
            }
            Owner::Basket(id) => {
                self.order_owner.remove(&fill.order_id);
                let leg = self
                    .baskets
                    .get_mut(id)
                    .and_then(|b| b.on_fill(fill.order_id, fill));
                if leg.is_some() {
                    self.push_basket_event(format!(
                        "basket #{id}: {symbol} {:?} {} filled at {} (maker)",
                        fill.side, fill.qty, fill.price
                    ));
                    // The basket may be complete now: let it move on at once.
                    let marks = self.last_marks.clone();
                    let intents = self
                        .baskets
                        .get_mut(id)
                        .map(|b| b.step(now_ms, &marks))
                        .unwrap_or_default();
                    self.run_intents(id, intents, now_ms);
                }
            }
        }
    }

    /// Account one exchange-reported fill exactly once.  The same fill can
    /// arrive in a REST reply (`FILLED`) and on the user-data stream; a trade
    /// id seen twice is also dropped.
    fn on_exchange_fill(&mut self, ev: ExchangeFill, now_ms: i64) {
        match ev.source {
            FillSource::Rest => {
                // Already reported by the stream (partial or whole)? Then the
                // stream owns this order.
                let by_stream = self.seen_trades.iter().any(|(oid, _)| *oid == ev.order_id);
                if by_stream || !self.rest_filled.insert(ev.order_id) {
                    return;
                }
            }
            FillSource::UserData => {
                if self.rest_filled.contains(&ev.order_id) {
                    return;
                }
                if ev.trade_id != 0 && !self.seen_trades.insert((ev.order_id, ev.trade_id)) {
                    return;
                }
                if ev.trade_id == 0 {
                    self.seen_trades.insert((ev.order_id, 0));
                }
            }
        }
        if self.seen_trades.len() > 5000 {
            self.seen_trades.clear();
        }
        if self.rest_filled.len() > 5000 {
            self.rest_filled.clear();
        }
        let fill = self.broker.exchange_fill(
            &ev.symbol,
            ev.side,
            ev.qty,
            ev.price,
            ev.time_ms,
            ev.order_id,
        );
        self.fill_log
            .push_back((ev.time_ms, ev.symbol.clone(), fill.clone()));
        while self.fill_log.len() > 128 {
            self.fill_log.pop_front();
        }
        if let Some(i) = self.find_slot(&ev.symbol) {
            self.slots[i].push_log(format!(
                "LIVE FILLED #{} {:?} {} @ {:.6}",
                ev.order_id, ev.side, ev.qty, ev.price
            ));
        }
        // Attribute the exchange order to its slot (by client id) and tell it.
        if let Some(owner) = self.owner_by_cid.get(&ev.client_id).cloned() {
            self.order_owner
                .entry(ev.order_id)
                .or_insert(Owner::Slot(owner));
        }
        let last = self.last_marks.get(&ev.symbol).copied().unwrap_or(ev.price);
        self.notify_owner(&fill, &ev.symbol, last, now_ms);
    }

    /// Drain pending Binance events and apply them to the exchange mirror.
    /// No-op when not in live mode.
    pub fn pump_binance(&mut self, now_ms: i64) {
        let Some(client) = &self.binance else {
            return;
        };
        let events = client.drain();
        for ev in events {
            match ev {
                LiveEvent::OrderFilled {
                    symbol,
                    side,
                    qty,
                    price,
                    time_ms,
                    order_id,
                    trade_id,
                    client_id,
                    source,
                } => self.on_exchange_fill(
                    ExchangeFill {
                        symbol,
                        side,
                        qty,
                        price,
                        time_ms,
                        order_id,
                        trade_id,
                        client_id,
                        source,
                    },
                    now_ms,
                ),
                LiveEvent::OrderQueued {
                    symbol,
                    side,
                    qty,
                    limit,
                    order_id,
                    created_ms,
                    client_id,
                } => {
                    self.broker.exchange_order(OpenOrder {
                        id: order_id,
                        symbol: symbol.clone(),
                        side,
                        qty,
                        limit,
                        created_ms,
                    });
                    if let Some(owner) = self.owner_by_cid.get(&client_id).cloned() {
                        self.order_owner.insert(order_id, Owner::Slot(owner));
                    }
                    if let Some(i) = self.find_slot(&symbol) {
                        self.slots[i].push_log(format!(
                            "LIVE QUEUED #{} {:?} {} @ {:.6}",
                            order_id, side, qty, limit
                        ));
                    }
                }
                LiveEvent::OrderRejected {
                    action,
                    client_id,
                    reason,
                } => {
                    self.owner_by_cid.remove(&client_id);
                    let sym = action.symbol().unwrap_or("").to_string();
                    self.blocked_log.push_back((
                        now_ms,
                        sym.clone(),
                        format!("LIVE REJECTED {}: {}", action.label(), reason),
                    ));
                    while self.blocked_log.len() > 64 {
                        self.blocked_log.pop_front();
                    }
                    if let Some(i) = self.find_slot(&sym) {
                        self.slots[i].push_log(format!(
                            "LIVE REJECTED {}: {}",
                            action.label(),
                            reason
                        ));
                    }
                }
                LiveEvent::OrderUnknown {
                    action,
                    client_id,
                    reason,
                } => {
                    let sym = action.symbol().unwrap_or("").to_string();
                    if let Some(i) = self.find_slot(&sym) {
                        self.slots[i].push_log(format!(
                            "LIVE UNKNOWN {} ({client_id}): {reason} — checking the exchange",
                            action.label()
                        ));
                    }
                    self.note_live_error(format!(
                        "{} {}: no reply ({reason}) — resolving by client id",
                        action.label(),
                        client_id
                    ));
                }
                LiveEvent::AccountSync(balances) => {
                    self.live_balances = balances;
                    self.live_balances_at = now_ms;
                }
                LiveEvent::OpenOrdersSync(orders) => {
                    // The exchange is the truth about what is resting.
                    let list = orders
                        .into_iter()
                        .map(|o| OpenOrder {
                            id: o.order_id,
                            symbol: o.symbol,
                            side: o.side,
                            qty: o.qty,
                            limit: o.price,
                            created_ms: o.created_ms,
                        })
                        .collect::<Vec<_>>();
                    let ids: HashSet<u64> = list.iter().map(|o| o.id).collect();
                    self.order_owner.retain(|id, _| ids.contains(id));
                    self.broker.replace_open_orders(list);
                }
                LiveEvent::OrderCancelled {
                    symbol: _,
                    order_id,
                } => {
                    self.broker.drop_order(order_id);
                    self.order_owner.remove(&order_id);
                }
                LiveEvent::ListenKey(Ok(key)) => {
                    if let Some(client) = &self.binance {
                        let tx = client.event_sender();
                        self.user_data_ws =
                            Some(spawn_user_data_ws(&client.base_url, key.clone(), tx));
                    }
                    self.listen_key = Some(key);
                }
                LiveEvent::ListenKey(Err(e)) => {
                    self.note_live_error(format!(
                        "user-data stream unavailable ({e}): only REST-confirmed full fills are seen"
                    ));
                }
                LiveEvent::SyncError(e) => self.note_live_error(e),
            }
        }
    }

    fn execute_actions(&mut self, slot_idx: usize, actions: Vec<Action>, last: f64, now_ms: i64) {
        if slot_idx >= self.slots.len() {
            return;
        }
        for action in actions {
            // Notes don't go to the broker — log + skip.
            if let Action::Note { text } = &action {
                self.slots[slot_idx].push_log(format!("NOTE: {text}"));
                continue;
            }
            let symbol = self.slots[slot_idx].symbol.clone();
            let pos = self.broker.position(&symbol);
            let (other, oo_count) = self.other_ledger(&symbol);
            if let Err(e) = check_action(&action, &pos, other, &self.limits, oo_count)
                .and_then(|()| self.margin_for(&action, last))
            {
                self.slots[slot_idx].push_log(format!("BLOCKED: {} ({e})", action.label()));
                self.blocked_log.push_back((
                    now_ms,
                    symbol.clone(),
                    format!("{e} ({})", action.label()),
                ));
                while self.blocked_log.len() > 64 {
                    self.blocked_log.pop_front();
                }
                continue;
            }

            // A slot's CancelAll cancels the slot's OWN orders only.
            if let Action::CancelAll { .. } = &action {
                self.slots[slot_idx].push_log(action.label());
                self.cancel_owned(slot_idx);
                continue;
            }

            // LIVE: the strategy trades the exchange, never the paper broker.
            if self.binance.is_some() {
                if let Some(why) = self.live_block_reason(&symbol) {
                    self.slots[slot_idx].push_log(format!("BLOCKED: {} ({why})", action.label()));
                    self.blocked_log.push_back((
                        now_ms,
                        symbol,
                        format!("{why} ({})", action.label()),
                    ));
                    continue;
                }
                let cid = BinanceClient::new_client_id();
                self.owner_by_cid.insert(cid.clone(), symbol);
                self.slots[slot_idx].push_log(format!("LIVE {} → Binance", action.label()));
                if let Some(client) = &self.binance {
                    client.submit(action, cid);
                }
                continue;
            }

            self.slots[slot_idx].push_log(action.label());
            let first_new_id = self.broker.peek_next_order_id();
            let fills = self.broker.submit(&action, last, now_ms);
            for id in first_new_id..self.broker.peek_next_order_id() {
                self.order_owner.insert(id, Owner::Slot(symbol.clone()));
            }
            for f in fills {
                self.slots[slot_idx]
                    .push_log(format!("FILL {:?} {} @ {:.6}", f.side, f.qty, f.price));
                // The global log feeds the Stats page; market fills a
                // strategy gets straight from `submit` belong there too.
                self.fill_log.push_back((now_ms, symbol.clone(), f));
                while self.fill_log.len() > 128 {
                    self.fill_log.pop_front();
                }
            }
        }
    }

    /// Drop one resting PAPER order by id.  Refused in live mode (the exchange
    /// owns the order there).  `true` when the order existed.
    pub fn cancel_paper_order(&mut self, id: u64) -> bool {
        if self.read_only || self.binance.is_some() {
            return false;
        }
        self.order_owner.remove(&id);
        self.broker.drop_order(id)
    }

    /// Cancel every resting order the slot placed (paper: drop locally;
    /// live: ask the exchange, the cancel confirmation drops it from the mirror).
    fn cancel_owned(&mut self, slot_idx: usize) {
        let symbol = self.slots[slot_idx].symbol.clone();
        let ids: Vec<u64> = self
            .order_owner
            .iter()
            .filter(|(_, owner)| matches!(owner, Owner::Slot(s) if *s == symbol))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(client) = &self.binance {
                client.cancel(symbol.clone(), id);
            } else {
                self.broker.drop_order(id);
                self.order_owner.remove(&id);
            }
        }
    }

    fn dispatch_fill(&mut self, slot_idx: usize, fill: &Fill, last: f64, now_ms: i64) {
        let mut actions: Vec<Action> = Vec::new();
        {
            let symbol = self.slots[slot_idx].symbol.clone();
            let position = self.broker.position(&symbol);
            let equity = self.broker.equity(&self.last_marks);
            let bars: Vec<Bar> = self.slots[slot_idx].bar_history.clone();
            let mut ctx = Ctx {
                symbol: &symbol,
                now_ms,
                last,
                position,
                equity,
                actions: &mut actions,
                bars: &bars,
            };
            self.slots[slot_idx].strategy.on_fill(&mut ctx, fill);
        }
        self.execute_actions(slot_idx, actions, last, now_ms);
    }
}

/// One fill as reported by the exchange (either channel).
struct ExchangeFill {
    symbol: String,
    side: Side,
    qty: f64,
    price: f64,
    time_ms: i64,
    order_id: u64,
    trade_id: u64,
    client_id: String,
    source: FillSource,
}

pub fn current_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Coarse day bucket used by risk-state to detect a UTC rollover.
fn day_id(now_ms: i64) -> i64 {
    now_ms / 86_400_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::params::{ParamSpec, ParamValue};
    use crate::strategies::StrategyMeta;
    use std::sync::{Arc, Mutex};

    /// Places one resting buy limit on start, records the fills it is told about.
    struct Probe {
        fills: Arc<Mutex<Vec<f64>>>,
        place_on_start: bool,
    }

    impl Strategy for Probe {
        fn meta(&self) -> StrategyMeta {
            StrategyMeta {
                id: "probe",
                name: "probe",
                description: "",
                timer_secs: 0,
            }
        }
        fn schema(&self) -> &[ParamSpec] {
            &[]
        }
        fn get_param(&self, _: &str) -> Option<ParamValue> {
            None
        }
        fn set_param(&mut self, _: &str, _: ParamValue) -> Result<(), String> {
            Ok(())
        }
        fn on_start(&mut self, ctx: &mut Ctx) {
            if self.place_on_start {
                ctx.buy_limit(1.0, 90.0);
            }
        }
        fn on_stop(&mut self, ctx: &mut Ctx) {
            ctx.cancel_all();
        }
        fn on_fill(&mut self, _ctx: &mut Ctx, f: &Fill) {
            self.fills.lock().unwrap().push(f.qty);
        }
    }

    fn rt_with_probe(venue: &str) -> (Runtime, Arc<Mutex<Vec<f64>>>) {
        let mut rt = Runtime::new(10_000.0);
        let fills = Arc::new(Mutex::new(Vec::new()));
        rt.add_slot(
            "BTCUSDT".into(),
            venue.into(),
            "probe".into(),
            Box::new(Probe {
                fills: fills.clone(),
                place_on_start: true,
            }),
        );
        (rt, fills)
    }

    fn buy_limit(qty: f64, limit: f64) -> Action {
        Action::BuyLimit {
            symbol: "BTCUSDT".into(),
            qty,
            limit,
        }
    }

    /// A client pointed at a closed port: bootstrap fails fast, nothing is sent.
    fn go_live(rt: &mut Runtime) {
        rt.enable_live("k".into(), "s".into(), "http://127.0.0.1:1".into());
    }

    fn inject(rt: &Runtime, ev: LiveEvent) {
        rt.binance
            .as_ref()
            .unwrap()
            .event_sender()
            .send(ev)
            .unwrap();
    }

    fn exchange_fill(order_id: u64, trade_id: u64, source: FillSource) -> LiveEvent {
        LiveEvent::OrderFilled {
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            qty: 1.0,
            price: 100.0,
            time_ms: 5,
            order_id,
            trade_id,
            client_id: String::new(),
            source,
        }
    }

    fn live_qty(rt: &Runtime) -> f64 {
        rt.broker.position("BTCUSDT").qty
    }

    #[test]
    fn live_and_paper_never_share_a_ledger() {
        let mut rt = Runtime::new(10_000.0);
        rt.broker.submit(
            &Action::BuyMarket {
                symbol: "BTCUSDT".into(),
                qty: 2.0,
            },
            100.0,
            1,
        );
        go_live(&mut rt);
        // The live ledger starts empty; the paper position is parked, not lost.
        assert_eq!(live_qty(&rt), 0.0);
        assert!(rt.broker.is_mirror());
        assert_eq!(rt.paper_ledger().0.position("BTCUSDT").qty, 2.0);
        assert!(
            rt.baskets.baskets.is_empty(),
            "the book is parked with the ledger"
        );
        // A mirror never simulates fills from price ticks.
        rt.broker.exchange_order(OpenOrder {
            id: 9,
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            qty: 1.0,
            limit: 90.0,
            created_ms: 1,
        });
        rt.on_price("BTCUSDT", 80.0, 2);
        assert_eq!(
            live_qty(&rt),
            0.0,
            "limit crossed locally must NOT fill in live"
        );
        // Leaving live restores the untouched paper ledger.
        rt.disable_live();
        assert!(!rt.broker.is_mirror());
        assert_eq!(rt.broker.position("BTCUSDT").qty, 2.0);
    }

    #[test]
    fn one_exchange_fill_is_counted_once_whichever_channel_reports_it() {
        // REST first, then the stream.
        let mut rt = Runtime::new(0.0);
        go_live(&mut rt);
        inject(&rt, exchange_fill(1, 0, FillSource::Rest));
        inject(&rt, exchange_fill(1, 77, FillSource::UserData));
        rt.pump_binance(10);
        assert_eq!(live_qty(&rt), 1.0, "REST then stream");

        // Stream first, then REST.
        let mut rt = Runtime::new(0.0);
        go_live(&mut rt);
        inject(&rt, exchange_fill(2, 78, FillSource::UserData));
        inject(&rt, exchange_fill(2, 0, FillSource::Rest));
        rt.pump_binance(10);
        assert_eq!(live_qty(&rt), 1.0, "stream then REST");

        // Same trade id twice (stream replay) and two distinct partials.
        let mut rt = Runtime::new(0.0);
        go_live(&mut rt);
        inject(&rt, exchange_fill(3, 5, FillSource::UserData));
        inject(&rt, exchange_fill(3, 5, FillSource::UserData));
        inject(&rt, exchange_fill(3, 6, FillSource::UserData));
        rt.pump_binance(10);
        assert_eq!(
            live_qty(&rt),
            2.0,
            "duplicate trade dropped, partials summed"
        );
        assert_eq!(rt.fill_log.len(), 2);
    }

    #[test]
    fn exchange_fill_reduces_the_resting_order_and_cancel_drops_it() {
        let mut rt = Runtime::new(0.0);
        go_live(&mut rt);
        inject(
            &rt,
            LiveEvent::OrderQueued {
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                qty: 3.0,
                limit: 90.0,
                order_id: 4,
                created_ms: 1,
                client_id: String::new(),
            },
        );
        inject(&rt, exchange_fill(4, 1, FillSource::UserData));
        rt.pump_binance(10);
        assert_eq!(rt.broker.all_open_orders()[0].qty, 2.0, "3 - 1 filled");
        inject(
            &rt,
            LiveEvent::OrderCancelled {
                symbol: "BTCUSDT".into(),
                order_id: 4,
            },
        );
        rt.pump_binance(11);
        assert!(rt.broker.all_open_orders().is_empty());
    }

    #[test]
    fn open_orders_sync_replaces_the_mirror() {
        let mut rt = Runtime::new(0.0);
        go_live(&mut rt);
        rt.broker.exchange_order(OpenOrder {
            id: 1,
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            qty: 1.0,
            limit: 1.0,
            created_ms: 0,
        });
        inject(
            &rt,
            LiveEvent::OpenOrdersSync(vec![crate::engine::OpenOrderInfo {
                order_id: 2,
                symbol: "BTCUSDT".into(),
                side: Side::Sell,
                qty: 0.5,
                price: 200.0,
                created_ms: 0,
            }]),
        );
        rt.pump_binance(1);
        let o = rt.broker.all_open_orders();
        assert_eq!((o.len(), o[0].id), (1, 2), "the exchange list wins");
    }

    #[test]
    fn a_strategy_only_hears_about_its_own_fills() {
        let (mut rt, fills) = rt_with_probe("BINANCE-SPOT");
        rt.start(0, 1).unwrap(); // probe rests buy 1 @ 90
                                 // The user rests another order on the same symbol.
        rt.submit_user(buy_limit(5.0, 95.0), 2).unwrap();
        assert_eq!(rt.broker.open_order_count(), 2);
        rt.on_price("BTCUSDT", 94.0, 3); // crosses only the USER's 95 limit
        assert!(
            fills.lock().unwrap().is_empty(),
            "user's fill is not the strategy's"
        );
        rt.on_price("BTCUSDT", 89.0, 4); // crosses the strategy's 90 limit
        assert_eq!(*fills.lock().unwrap(), vec![1.0]);
    }

    #[test]
    fn stopping_a_slot_cancels_only_its_own_orders() {
        let (mut rt, _fills) = rt_with_probe("BINANCE-SPOT");
        rt.start(0, 1).unwrap();
        rt.submit_user(buy_limit(5.0, 50.0), 2).unwrap(); // the user's order
        assert_eq!(rt.broker.open_order_count(), 2);
        rt.stop(0, 3).unwrap();
        let left = rt.broker.all_open_orders();
        assert_eq!(left.len(), 1, "the user's limit must survive on_stop");
        assert_eq!(left[0].limit, 50.0);
    }

    #[test]
    fn live_futures_slot_is_blocked_not_sent_to_spot() {
        let (mut rt, _f) = rt_with_probe("BINANCE-FUTURES");
        go_live(&mut rt);
        let err = rt.submit_user(buy_limit(1.0, 1.0), 1).unwrap_err();
        assert!(err.contains("Futures"), "{err}");
        // Started inside live: the probe's order is blocked, not submitted.
        rt.start(0, 2).unwrap();
        assert!(rt.slots[0].log.iter().any(|l| l.contains("BLOCKED")));
    }

    #[test]
    fn enable_live_stops_running_strategies() {
        let (mut rt, _f) = rt_with_probe("BINANCE-SPOT");
        rt.start(0, 1).unwrap();
        assert_eq!(rt.slots[0].status, SlotStatus::Running);
        go_live(&mut rt);
        assert_eq!(rt.slots[0].status, SlotStatus::Idle);
    }

    #[test]
    fn view_only_runtime_refuses_orders_and_never_fills() {
        let (mut rt, _f) = rt_with_probe("BINANCE-SPOT");
        rt.start(0, 1).unwrap();
        rt.submit_user(buy_limit(5.0, 95.0), 2).unwrap();
        assert_eq!(rt.broker.open_order_count(), 2);
        rt.read_only = true;
        assert_eq!(
            rt.submit_user(buy_limit(1.0, 50.0), 3).unwrap_err(),
            VIEW_ONLY
        );
        assert!(rt.start(0, 3).is_err() && rt.stop(0, 3).is_err());
        assert!(rt.reset_paper().is_err());
        let id = rt.broker.all_open_orders()[0].id;
        assert!(!rt.cancel_paper_order(id), "cancel refused");
        // A price through the resting limits: marks move, nothing fills.
        rt.on_price("BTCUSDT", 80.0, 4);
        assert_eq!(
            rt.broker.open_order_count(),
            2,
            "no fill in a view-only runtime"
        );
        assert_eq!(rt.last_mark("BTCUSDT"), Some(80.0));
        rt.read_only = false;
        assert!(rt.cancel_paper_order(id));
    }

    fn buy(sym: &str, qty: f64) -> Action {
        Action::BuyMarket {
            symbol: sym.into(),
            qty,
        }
    }

    fn pair_basket(usd: f64) -> Basket {
        Basket::new(
            0,
            "buy A / sell B",
            "test",
            &[
                ("AUSDT".to_string(), true, usd),
                ("BUSDT".to_string(), false, usd),
            ],
            basket::Entry::default(),
            basket::DEFAULT_EXIT,
            0,
        )
        .unwrap()
    }

    #[test]
    fn a_basket_enters_on_resting_limits_and_leaves_on_its_target() {
        let mut rt = Runtime::new(10_000.0);
        rt.perp_shorts = false;
        rt.broker.fee_maker_bps = 2.0;
        rt.broker.fee_taker_bps = 10.0;
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        let id = rt.open_basket(pair_basket(100.0), 1).unwrap();
        assert_eq!(id, 1);
        // Two limits rest inside the price; nothing filled yet.
        assert_eq!(rt.broker.open_order_count(), 2);
        assert!(rt.broker.position("AUSDT").is_flat());
        assert!(matches!(
            rt.baskets.get(1).unwrap().state,
            basket::State::Entering { .. }
        ));
        // Ticks cross both limits: maker fills, the basket opens.
        rt.on_price("AUSDT", 9.99, 2);
        rt.on_price("BUSDT", 20.02, 3);
        assert_eq!(rt.broker.open_order_count(), 0);
        let b = rt.baskets.get(1).unwrap();
        assert!(
            matches!(b.state, basket::State::Open { .. }),
            "{:?}",
            b.state
        );
        assert!((b.legs[0].held - 10.0).abs() < 1e-9 && (b.legs[1].held - 5.0).abs() < 1e-9);
        assert!((rt.broker.position("AUSDT").qty - 10.0).abs() < 1e-9);
        assert!((rt.broker.position("BUSDT").qty + 5.0).abs() < 1e-9);
        assert!(
            rt.basket_events.iter().any(|e| e.contains("open")),
            "{:?}",
            rt.basket_events
        );
        // A rises 5 %: +5 on 200 = +2.5 % >= the 2 % target: exit limits rest.
        rt.on_price("AUSDT", 10.5, 4);
        rt.tick_baskets(4);
        let b = rt.baskets.get(1).unwrap();
        assert!(
            matches!(
                b.state,
                basket::State::Exiting {
                    why: basket::Why::Target,
                    ..
                }
            ),
            "{:?}",
            b.state
        );
        assert_eq!(rt.broker.open_order_count(), 2);
        rt.on_price("AUSDT", 10.6, 5);
        rt.on_price("BUSDT", 19.9, 6);
        let b = rt.baskets.get(1).unwrap();
        assert!(
            matches!(
                b.state,
                basket::State::Closed {
                    why: basket::Why::Target,
                    ..
                }
            ),
            "{:?}",
            b.state
        );
        assert!(rt.broker.position("AUSDT").is_flat() && rt.broker.position("BUSDT").is_flat());
        // Four maker fills: in at 9.995 / 20.01, out at 10.50525 (5 bps over
        // 10.5) and 20.00999 (5 bps under B's last mark, 20.02).
        let b_out = 20.02 * (1.0 - 5.0 / 10_000.0);
        let p = b.pnl(&rt.last_marks);
        let fees = 2.0 / 10_000.0 * (10.0 * 9.995 + 5.0 * 20.01 + 10.0 * 10.50525 + 5.0 * b_out);
        assert!((p.fees - fees).abs() < 1e-9, "{p:?}");
        let gross = 10.0 * (10.50525 - 9.995) + 5.0 * (20.01 - b_out);
        assert!((p.realized - gross).abs() < 1e-9, "{p:?}");
        assert!((rt.broker.fees_paid() - fees).abs() < 1e-9);
        assert!(rt.basket_events.iter().any(|e| e.contains("target")));
    }

    #[test]
    fn a_basket_markets_what_did_not_fill_after_the_wait() {
        let mut rt = Runtime::new(10_000.0);
        rt.perp_shorts = false;
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        rt.open_basket(pair_basket(100.0), 1_000).unwrap();
        rt.on_price("AUSDT", 9.99, 2_000);
        assert_eq!(rt.baskets.get(1).unwrap().entered_legs(), 1);
        // 89 s: the B leg still rests.  91 s: cancelled and done at market.
        rt.tick_baskets(90_000);
        assert_eq!(rt.broker.open_order_count(), 1);
        rt.tick_baskets(92_000);
        assert_eq!(rt.broker.open_order_count(), 0);
        let b = rt.baskets.get(1).unwrap();
        assert!(
            matches!(b.state, basket::State::Open { .. }),
            "{:?}",
            b.state
        );
        assert!((b.legs[1].held - 5.0).abs() < 1e-9);
        assert!((b.legs[1].avg - 20.0).abs() < 1e-9, "at the market price");
        assert!((rt.broker.position("BUSDT").qty + 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_closed_basket_is_handed_to_the_journal_once() {
        let mut rt = Runtime::new(10_000.0);
        rt.perp_shorts = false;
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        rt.open_basket(pair_basket(100.0), 1_000).unwrap();
        rt.tick_baskets(92_000);
        assert!(rt.closed_out.is_empty(), "still open");
        rt.close_basket(1, 100_000).unwrap();
        for t in [101_000, 200_000, 300_000] {
            rt.tick_baskets(t);
        }
        assert!(rt.baskets.get(1).unwrap().state.is_closed());
        assert_eq!(rt.closed_out.len(), 1, "once, however many ticks follow");
        assert_eq!(rt.closed_out[0].id, 1);
    }

    #[test]
    fn a_basket_is_all_or_nothing_at_the_risk_gate() {
        let mut rt = Runtime::new(10_000.0);
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        // The second leg (5 coins short) is over the position cap: nothing rests.
        rt.limits.max_abs_position = 8.0;
        let e = rt.open_basket(pair_basket(100.0), 1).unwrap_err();
        assert!(e.contains("BUSDT") || e.contains("AUSDT"), "{e}");
        assert_eq!(rt.broker.open_order_count(), 0);
        assert!(rt.baskets.baskets.is_empty());
        rt.limits.max_abs_position = 1e6;
        // Too many dollars for one basket / for all baskets; too many legs.
        rt.limits.max_basket_usd = 150.0;
        assert!(rt.open_basket(pair_basket(100.0), 1).is_err());
        rt.limits.max_basket_usd = 1e9;
        rt.limits.max_baskets_usd = 300.0;
        rt.open_basket(pair_basket(100.0), 1).unwrap();
        assert!(
            rt.open_basket(pair_basket(100.0), 1).is_err(),
            "400 over 300"
        );
        rt.limits.max_basket_legs = 1;
        rt.limits.max_baskets_usd = 1e9;
        assert!(rt.open_basket(pair_basket(1.0), 1).is_err());
        // No price for a leg: refused before anything is sent.
        rt.limits.max_basket_legs = 20;
        let no_price = Basket::new(
            0,
            "x",
            "o",
            &[("ZZZUSDT".to_string(), true, 10.0)],
            basket::Entry::Market,
            basket::DEFAULT_EXIT,
            0,
        )
        .unwrap();
        assert!(rt.open_basket(no_price, 1).unwrap_err().contains("ZZZUSDT"));
    }

    #[test]
    fn the_kill_switch_blocks_a_basket_entry_but_not_its_exit() {
        let mut rt = Runtime::new(10_000.0);
        rt.perp_shorts = false;
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        rt.limits.kill_switch = true;
        assert!(rt.open_basket(pair_basket(100.0), 1).is_err());
        rt.limits.kill_switch = false;
        rt.open_basket(pair_basket(100.0), 1).unwrap();
        rt.on_price("AUSDT", 9.99, 2);
        rt.on_price("BUSDT", 20.02, 3);
        assert!(matches!(
            rt.baskets.get(1).unwrap().state,
            basket::State::Open { .. }
        ));
        // The switch trips; the user closes the basket: exit limits are
        // refused, so it leaves at market (shrinking is allowed).
        rt.limits.kill_switch = true;
        rt.close_basket(1, 4).unwrap();
        let b = rt.baskets.get(1).unwrap();
        assert!(
            matches!(
                b.state,
                basket::State::Closed {
                    why: basket::Why::User,
                    ..
                }
            ),
            "{:?}",
            b.state
        );
        assert!(b.blocked.is_none());
        assert!(rt.broker.position("AUSDT").is_flat() && rt.broker.position("BUSDT").is_flat());
        assert_eq!(rt.broker.open_order_count(), 0);
    }

    #[test]
    fn a_blocked_basket_waits_for_the_user_and_a_dropped_one_closes() {
        let mut rt = Runtime::new(10_000.0);
        rt.perp_shorts = false;
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        rt.open_basket(pair_basket(100.0), 1).unwrap();
        rt.on_price("AUSDT", 9.99, 2);
        // The position cap drops before the wait runs out: the market order
        // for B is refused, the basket is blocked, not retried every tick.
        rt.limits.max_abs_position = 1.0;
        rt.tick_baskets(100_000);
        let b = rt.baskets.get(1).unwrap();
        assert!(b.blocked.is_some(), "{:?}", b.state);
        let n = rt.basket_events.len();
        rt.tick_baskets(101_000);
        rt.tick_baskets(102_000);
        assert_eq!(rt.basket_events.len(), n, "quiet while blocked");
        // Retry with the cap back: it completes.
        rt.limits.max_abs_position = 1e6;
        rt.retry_basket(1, 103_000).unwrap();
        assert!(matches!(
            rt.baskets.get(1).unwrap().state,
            basket::State::Open { .. }
        ));
        // A second basket dropped while entering: its resting legs are
        // cancelled, what it held is sold.
        rt.open_basket(pair_basket(50.0), 200_000).unwrap();
        rt.on_price("AUSDT", 9.99, 200_001);
        rt.close_basket(2, 200_002).unwrap();
        rt.on_price("AUSDT", 10.01, 200_003);
        let b = rt.baskets.get(2).unwrap();
        assert!(
            matches!(
                b.state,
                basket::State::Closed {
                    why: basket::Why::Abandoned,
                    ..
                }
            ),
            "{:?}",
            b.state
        );
        assert!(rt.close_basket(2, 1).is_err(), "closed already");
        assert!(rt.close_basket(9, 1).is_err(), "no such basket");
        // The first basket still holds its legs.
        assert!((rt.broker.position("AUSDT").qty - 10.0).abs() < 1e-9);
    }

    #[test]
    fn baskets_are_paper_only_and_refused_in_a_view_only_window() {
        let mut rt = Runtime::new(10_000.0);
        rt.on_price("AUSDT", 10.0, 1);
        rt.on_price("BUSDT", 20.0, 1);
        rt.read_only = true;
        assert_eq!(rt.open_basket(pair_basket(1.0), 1).unwrap_err(), VIEW_ONLY);
        rt.read_only = false;
        rt.open_basket(pair_basket(100.0), 1).unwrap();
        go_live(&mut rt);
        assert!(
            rt.baskets.baskets.is_empty(),
            "parked with the paper ledger"
        );
        assert!(rt.open_basket(pair_basket(1.0), 2).is_err());
        rt.tick_baskets(3);
        rt.disable_live();
        assert_eq!(rt.baskets.baskets.len(), 1, "back with the ledger");
        assert_eq!(rt.paper_ledger().2.baskets.len(), 1);
        // The user's exit rule can be changed while it is open.
        rt.set_basket_exit(
            1,
            basket::ExitRule {
                target_pct: Some(5.0),
                stop_pct: None,
                max_minutes: None,
            },
        )
        .unwrap();
        assert_eq!(rt.baskets.get(1).unwrap().exit.target_pct, Some(5.0));
        rt.reset_paper().unwrap();
        assert!(rt.baskets.baskets.is_empty());
    }

    #[test]
    fn the_scalper_ledger_is_separate_from_the_human_one() {
        let mut rt = Runtime::new(10_000.0);
        // Exact PnL below: no fees in this test.
        rt.broker.set_fee_bps(0.0);
        rt.on_price("AUSDT", 100.0, 1);
        assert!(
            rt.submit_scalper(buy("AUSDT", 1.0), 2).is_err(),
            "no ledger yet"
        );
        rt.start_scalper_ledger(1_000.0).unwrap();
        let fills = rt.submit_scalper(buy("AUSDT", 2.0), 3).unwrap();
        assert_eq!(fills.len(), 1);
        assert!(
            rt.broker.position("AUSDT").is_flat(),
            "human ledger untouched"
        );
        let book = rt.scalper.as_ref().unwrap();
        assert_eq!(book.position("AUSDT").qty, 2.0);
        rt.on_price("AUSDT", 90.0, 4);
        assert!((rt.scalper_pnl() + 20.0).abs() < 1e-9);
        assert_eq!(rt.equity(), 10_000.0, "human equity untouched");
        assert!((rt.risk_equity() - 9_980.0).abs() < 1e-9);
        assert!((rt.day_pnl() + 20.0).abs() < 1e-9, "day PnL counts both");
    }

    #[test]
    fn scalper_limits_fill_on_ticks_and_ids_never_collide() {
        let mut rt = Runtime::new(10_000.0);
        rt.on_price("AUSDT", 100.0, 1);
        rt.start_scalper_ledger(1_000.0).unwrap();
        let lim = Action::BuyLimit {
            symbol: "AUSDT".into(),
            qty: 1.0,
            limit: 95.0,
        };
        assert!(rt.submit_scalper(lim, 2).unwrap().is_empty());
        let id = rt.scalper.as_ref().unwrap().all_open_orders()[0].id;
        assert!(id >= SCALPER_FIRST_ORDER_ID);
        rt.on_price("AUSDT", 94.0, 3);
        assert_eq!(rt.scalper_fills.len(), 1);
        assert_eq!(rt.scalper_fills[0].1.order_id, id);
        assert!(rt.fill_log.is_empty(), "not in the human fill log");
        assert!(!rt.cancel_scalper_order(id), "already filled");
    }

    #[test]
    fn risk_limits_cover_both_ledgers() {
        let mut rt = Runtime::new(10_000.0);
        rt.limits.max_abs_position = 3.0;
        rt.on_price("AUSDT", 100.0, 1);
        rt.start_scalper_ledger(1_000.0).unwrap();
        rt.submit_user(buy("AUSDT", 2.0), 2).unwrap();
        let e = rt.submit_scalper(buy("AUSDT", 2.0), 3).unwrap_err();
        assert!(e.contains("max position"), "{e}");
        rt.submit_scalper(buy("AUSDT", 1.0), 4).unwrap();
        assert!(rt.submit_user(buy("AUSDT", 0.5), 5).is_err());
        // The kill switch lets the scalper close its own position only.
        rt.limits.kill_switch = true;
        let sell = |q| Action::SellMarket {
            symbol: "AUSDT".into(),
            qty: q,
        };
        assert!(
            rt.submit_scalper(sell(2.0), 6).is_err(),
            "more than it holds"
        );
        assert!(rt.submit_scalper(sell(1.0), 7).is_ok());
    }

    #[test]
    fn the_daily_loss_limit_trips_on_the_scalper_loss() {
        let mut rt = Runtime::new(10_000.0);
        rt.limits.max_daily_loss = 50.0;
        rt.on_price("AUSDT", 100.0, 1);
        rt.start_scalper_ledger(1_000.0).unwrap();
        rt.submit_scalper(buy("AUSDT", 10.0), 2).unwrap();
        rt.on_price("AUSDT", 96.0, 3);
        assert!(!rt.limits.kill_switch, "-40 is inside the limit");
        rt.on_price("AUSDT", 94.0, 4);
        assert!(rt.limits.kill_switch, "-60 across both ledgers trips it");
    }

    #[test]
    fn a_new_scalper_ledger_does_not_wipe_todays_loss() {
        let mut rt = Runtime::new(10_000.0);
        // Exact PnL below: no fees in this test.
        rt.broker.set_fee_bps(0.0);
        rt.on_price("AUSDT", 100.0, 1);
        rt.start_scalper_ledger(1_000.0).unwrap();
        rt.submit_scalper(buy("AUSDT", 10.0), 2).unwrap();
        rt.on_price("AUSDT", 97.0, 3);
        assert!((rt.day_pnl() + 30.0).abs() < 1e-9);
        rt.start_scalper_ledger(1_000.0).unwrap();
        assert!((rt.day_pnl() + 30.0).abs() < 1e-9, "{}", rt.day_pnl());
        rt.on_price("AUSDT", 97.0, 4);
        assert!((rt.day_pnl() + 30.0).abs() < 1e-9, "{}", rt.day_pnl());
    }

    fn user_buy(rt: &mut Runtime, sym: &str, qty: f64, price: f64) {
        rt.on_price(sym, price, 1);
        rt.submit_user(
            Action::BuyMarket {
                symbol: sym.into(),
                qty,
            },
            1,
        )
        .expect("buy");
    }

    #[test]
    fn a_stop_closes_the_whole_position_once() {
        let mut rt = Runtime::new(10_000.0);
        user_buy(&mut rt, "BTCUSDT", 2.0, 100.0);
        let b = Bracket {
            sl: Some(95.0),
            tp: Some(120.0),
            long: false,
        };
        rt.set_bracket("BTCUSDT", b).expect("valid bracket");
        rt.on_price("BTCUSDT", 96.0, 2);
        assert_eq!(rt.broker.position("BTCUSDT").qty, 2.0, "above the stop");
        rt.on_price("BTCUSDT", 94.0, 3);
        assert!(rt.broker.position("BTCUSDT").is_flat(), "stop hit");
        assert!(rt.broker.bracket("BTCUSDT").is_empty(), "spent");
        assert_eq!(rt.exit_events.len(), 1);
        assert!(
            rt.exit_events[0].starts_with("SL BTCUSDT"),
            "{:?}",
            rt.exit_events
        );
        rt.on_price("BTCUSDT", 90.0, 4);
        assert_eq!(rt.exit_events.len(), 1, "no second exit");
    }

    #[test]
    fn a_take_closes_a_short() {
        let mut rt = Runtime::new(10_000.0);
        rt.on_price("ETHUSDT", 50.0, 1);
        rt.submit_user(
            Action::SellMarket {
                symbol: "ETHUSDT".into(),
                qty: 1.0,
            },
            1,
        )
        .expect("sell");
        let b = Bracket {
            sl: Some(55.0),
            tp: Some(40.0),
            long: true, // the runtime sets the side itself
        };
        rt.set_bracket("ETHUSDT", b).expect("valid bracket");
        rt.on_price("ETHUSDT", 39.0, 2);
        assert!(rt.broker.position("ETHUSDT").is_flat());
        assert!(rt.exit_events[0].starts_with("TP ETHUSDT"));
    }

    #[test]
    fn levels_on_the_wrong_side_or_without_a_position_are_refused() {
        let mut rt = Runtime::new(10_000.0);
        let stop = |sl| Bracket {
            sl: Some(sl),
            ..Bracket::default()
        };
        rt.on_price("BTCUSDT", 100.0, 1);
        assert!(rt.set_bracket("BTCUSDT", stop(90.0)).is_err(), "flat");
        user_buy(&mut rt, "BTCUSDT", 1.0, 100.0);
        assert!(
            rt.set_bracket("BTCUSDT", stop(105.0)).is_err(),
            "long stop above"
        );
        assert!(rt.set_bracket("BTCUSDT", stop(f64::NAN)).is_err());
        let take = Bracket {
            tp: Some(99.0),
            ..Bracket::default()
        };
        assert!(rt.set_bracket("BTCUSDT", take).is_err(), "long take below");
        assert!(
            rt.set_bracket("BTCUSDT", Bracket::default()).is_ok(),
            "clearing"
        );
        rt.read_only = true;
        assert!(rt.set_bracket("BTCUSDT", stop(90.0)).is_err(), "view-only");
    }

    #[test]
    fn a_reversed_position_drops_the_old_levels() {
        let mut rt = Runtime::new(10_000.0);
        user_buy(&mut rt, "BTCUSDT", 1.0, 100.0);
        let b = Bracket {
            sl: Some(90.0),
            ..Bracket::default()
        };
        rt.set_bracket("BTCUSDT", b).expect("valid");
        rt.submit_user(
            Action::SellMarket {
                symbol: "BTCUSDT".into(),
                qty: 2.0,
            },
            2,
        )
        .expect("reverse");
        // Short now; the long's stop below must not close it.
        rt.on_price("BTCUSDT", 101.0, 3);
        assert_eq!(rt.broker.position("BTCUSDT").qty, -1.0);
        assert!(rt.broker.bracket("BTCUSDT").is_empty());
        assert!(rt.exit_events.is_empty());
    }

    #[test]
    fn brackets_survive_a_snapshot() {
        let mut rt = Runtime::new(10_000.0);
        user_buy(&mut rt, "BTCUSDT", 1.0, 100.0);
        let b = Bracket {
            sl: Some(90.0),
            tp: Some(130.0),
            long: false,
        };
        rt.set_bracket("BTCUSDT", b).expect("valid");
        let json = serde_json::to_string(&rt.broker.snapshot()).expect("json");
        let back = PaperBroker::restore(serde_json::from_str(&json).expect("parse"));
        assert_eq!(back.bracket("BTCUSDT").sl, Some(90.0));
        assert!(back.bracket("BTCUSDT").long);
        // A snapshot written before brackets existed still loads.
        let old = r#"{"starting_cash":1.0,"positions":[],"open_orders":[],"next_id":1}"#;
        assert!(serde_json::from_str::<super::super::broker::BrokerSnapshot>(old).is_ok());
    }

    /// Ten generated portfolios (pairs, groups against BTC, 40-leg synthetics,
    /// a synthetic pair) opened together, driven through a rough market until
    /// every exit has fired: the limits hold, nothing leaks, the ledger ends
    /// flat and its equity is exactly what the baskets report.
    #[test]
    fn ten_generated_portfolios_open_and_close_safely_together() {
        let mut rt = Runtime::new(10_000.0);
        rt.broker.fee_maker_bps = 2.0;
        rt.broker.fee_taker_bps = 10.0;
        let said = rt.automation_guard();
        assert_eq!(said.len(), 4, "{said:?}");
        assert!(rt.limits.max_baskets_usd <= 5_000.0 && rt.limits.max_basket_usd <= 1_000.0);
        // A second call changes nothing: limits only tighten.
        assert!(rt.automation_guard().is_empty());

        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f64 / 10_000.0
        };
        let syms: Vec<String> = (0..120).map(|i| format!("S{i}USDT")).collect();
        let mut px: std::collections::HashMap<String, f64> = syms
            .iter()
            .map(|s| (s.clone(), 1.0 + rnd() * 500.0))
            .collect();
        px.insert("BTCUSDT".into(), 60_000.0);
        for (s, p) in &px {
            rt.on_price(s, *p, 1);
        }
        let usd = 5.0;
        let legs = |list: &[&String], long: bool| -> Vec<(String, bool, f64)> {
            list.iter().map(|s| ((*s).clone(), long, usd)).collect()
        };
        let mk = |name: &str, legs: Vec<(String, bool, f64)>| {
            Basket::new(
                0,
                name,
                "test",
                &legs,
                basket::Entry::default(),
                basket::DEFAULT_EXIT,
                1,
            )
            .unwrap()
        };
        let mut batch = Vec::new();
        for k in 0..4 {
            batch.push(mk(
                "pair",
                vec![
                    (syms[k * 2].clone(), true, usd),
                    (syms[k * 2 + 1].clone(), false, usd),
                ],
            ));
        }
        for k in 0..3 {
            let members: Vec<&String> = syms[20 + k * 20..20 + k * 20 + 19].iter().collect();
            let mut l = legs(&members, k % 2 == 0);
            l.push(("BTCUSDT".into(), k % 2 != 0, usd * 19.0));
            batch.push(mk("group vs BTC", l));
        }
        for k in 0..2 {
            let members: Vec<&String> = syms[k * 40..k * 40 + 40].iter().collect();
            batch.push(mk("synthetic 40", legs(&members, true)));
        }
        let (a, b): (Vec<&String>, Vec<&String>) = (
            syms[80..90].iter().collect(),
            syms[90..100].iter().collect(),
        );
        let mut sp = legs(&a, true);
        sp.extend(legs(&b, false));
        batch.push(mk("synthetic pair", sp));
        assert_eq!(batch.len(), 10);

        let dry = rt.dry_run_baskets(&batch);
        assert!(dry.iter().all(|r| r.is_ok()), "{dry:?}");
        let mut resting = 0;
        for b in &batch {
            resting += b.legs.len();
            rt.open_basket(b.clone(), 1_000).unwrap();
        }
        assert_eq!(rt.broker.open_order_count(), resting);
        assert!(rt.baskets.open_notional() <= rt.limits.max_baskets_usd);

        // A rough market: every price jumps a little each 10 s step, for 5 h.
        let mut now = 1_000i64;
        for _ in 0..1_800 {
            now += 10_000;
            for s in px.clone().keys() {
                let p = px.get_mut(s).unwrap();
                *p *= 1.0 + (rnd() - 0.5) * 0.006;
                rt.on_price(s, *p, now);
            }
            rt.tick_baskets(now);
            assert!(rt.baskets.open_notional() <= rt.limits.max_baskets_usd + 1e-6);
        }
        for b in rt.baskets.baskets.iter() {
            assert!(
                matches!(b.state, basket::State::Closed { .. }),
                "#{} {} {:?}",
                b.id,
                b.name,
                b.state
            );
        }
        assert_eq!(rt.broker.open_order_count(), 0);
        for s in px.keys() {
            assert!(rt.broker.position(s).is_flat(), "{s} not flat");
        }
        let net: f64 = rt
            .baskets
            .baskets
            .iter()
            .map(|b| b.pnl(&rt.last_marks).net())
            .sum();
        assert!(
            (rt.equity() - 10_000.0 - net).abs() < 1e-6,
            "equity {} vs baskets {net}",
            rt.equity()
        );
        assert!(rt.broker.fees_paid() > 0.0);
    }

    // ── Perpetuals: margin, liquidation, funding ────────────────────────

    #[test]
    fn a_perpetual_can_be_shorted_on_margin_and_costs_futures_fees() {
        let mut rt = Runtime::new(1_000.0);
        let sym = crate::futures::perp("ETHUSDT");
        rt.broker.set_leverage(&sym, 5.0);
        rt.on_price(&sym, 100.0, 1);
        // $2,000 of short on 5x ties up $400 of the $1,000.
        let fills = rt
            .submit_user(
                Action::SellMarket {
                    symbol: sym.clone(),
                    qty: 20.0,
                },
                2,
            )
            .unwrap();
        assert_eq!(fills.len(), 1);
        assert!((rt.broker.position(&sym).qty + 20.0).abs() < 1e-9);
        assert!((rt.broker.margin_used() - 400.0).abs() < 1e-6);
        // Taker fee of a perpetual is 5 bps, not the spot 10.
        assert!(
            (fills[0].fee - 2_000.0 * 5.0 / 10_000.0).abs() < 1e-9,
            "{}",
            fills[0].fee
        );
        assert!((rt.free_margin() - (1_000.0 - 1.0 - 400.0)).abs() < 1e-6);
        // Another $4,000 would need $800 against $599 free: refused.
        let e = rt
            .submit_user(
                Action::SellMarket {
                    symbol: sym.clone(),
                    qty: 40.0,
                },
                3,
            )
            .unwrap_err();
        assert!(e.contains("not enough margin"), "{e}");
        // Reducing is always allowed.
        rt.submit_user(
            Action::BuyMarket {
                symbol: sym.clone(),
                qty: 10.0,
            },
            4,
        )
        .unwrap();
    }

    #[test]
    fn a_perpetual_is_closed_by_force_at_its_liquidation_price() {
        let mut rt = Runtime::new(1_000.0);
        let sym = crate::futures::perp("SOLUSDT");
        rt.broker.set_leverage(&sym, 10.0);
        rt.on_price(&sym, 100.0, 1);
        rt.submit_user(
            Action::BuyMarket {
                symbol: sym.clone(),
                qty: 10.0,
            },
            2,
        )
        .unwrap();
        let liq = rt.broker.liquidation_price(&sym).unwrap();
        assert!((liq - 90.5).abs() < 1e-9, "{liq}");
        // 91: still alive.  90: gone, at 90.5, not at 90.
        rt.on_price(&sym, 91.0, 3);
        assert!(!rt.broker.position(&sym).is_flat());
        rt.on_price(&sym, 90.0, 4);
        assert!(rt.broker.position(&sym).is_flat());
        assert!(rt.futures_events.iter().any(|e| e.contains("LIQUIDATED")));
        // Lost 9.5 a coin (the margin less the maintenance) plus the fees.
        let eq = rt.equity();
        assert!(eq < 1_000.0 - 95.0 && eq > 1_000.0 - 110.0, "{eq}");
    }

    #[test]
    fn funding_moves_cash_at_the_eight_hour_marks() {
        const H: i64 = 3_600_000;
        let mut rt = Runtime::new(10_000.0);
        let sym = crate::futures::perp("BTCUSDT");
        rt.broker.set_leverage(&sym, 2.0);
        rt.on_price(&sym, 10_000.0, 7 * H);
        rt.submit_user(
            Action::BuyMarket {
                symbol: sym.clone(),
                qty: 1.0,
            },
            7 * H,
        )
        .unwrap();
        rt.funding_rates.insert("BTCUSDT".into(), 0.0001);
        let before = rt.equity();
        // No funding time passed yet.
        rt.on_price(&sym, 10_000.0, 7 * H + 1);
        assert_eq!(rt.broker.funding_total(), 0.0);
        // 08:00 UTC passes: the long pays $1.
        rt.on_price(&sym, 10_000.0, 8 * H + 1);
        assert!(
            (rt.broker.funding_total() + 1.0).abs() < 1e-9,
            "{}",
            rt.broker.funding_total()
        );
        assert!((rt.equity() - (before - 1.0)).abs() < 1e-9);
        // A short would have earned it.
        assert!(crate::futures::funding_payment(-1.0, 10_000.0, 0.0001) > 0.0);
    }

    #[test]
    fn a_liquidated_leg_leaves_its_basket_consistent() {
        let mut rt = Runtime::new(10_000.0);
        let (a, b) = (
            crate::futures::perp("AAAUSDT"),
            crate::futures::perp("BBBUSDT"),
        );
        rt.broker.set_leverage(&a, 20.0);
        rt.broker.set_leverage(&b, 20.0);
        rt.on_price(&a, 10.0, 1);
        rt.on_price(&b, 20.0, 1);
        let bk = Basket::new(
            0,
            "pair",
            "t",
            &[(a.clone(), true, 100.0), (b.clone(), false, 100.0)],
            basket::Entry::Market,
            basket::DEFAULT_EXIT,
            0,
        )
        .unwrap();
        rt.open_basket(bk, 1).unwrap();
        assert!(matches!(
            rt.baskets.get(1).unwrap().state,
            basket::State::Open { .. } | basket::State::Entering { .. }
        ));
        // The long falls far enough at 20x: forced out; the basket knows.
        rt.on_price(&a, 9.4, 2);
        assert!(rt.broker.position(&a).is_flat());
        let leg = &rt.baskets.get(1).unwrap().legs[0];
        assert!(leg.flat(), "{leg:?}");
        // The basket then leaves by itself and never sells what is gone.
        rt.tick_baskets(3);
        rt.on_price(&b, 20.0, 4_000_000);
        rt.on_price(&a, 9.4, 4_000_001);
        rt.tick_baskets(4_000_002);
        rt.on_price(&a, 9.4, 4_000_003);
        rt.on_price(&b, 20.0, 4_000_004);
        assert!(rt.broker.position(&a).is_flat());
    }
}
