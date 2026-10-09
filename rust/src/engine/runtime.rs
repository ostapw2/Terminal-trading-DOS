//! Runtime — owns the broker, the risk gate, and the live strategy slots.
//!
//! Driven from the main UI loop:
//!   * `on_price(symbol, price, now_ms)`  on every tick from the data feed
//!   * `dispatch_timers(now_ms)`          on each main-loop tick
//!   * `submit_user(action, now_ms)`      from the terminal command bar
//!
//! All strategy callbacks happen synchronously inside these methods.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::strategies::{
    actions::Action,
    context::{Bar, Ctx, Fill, Side, Tick},
    Strategy,
};

use super::binance::{
    spawn_user_data_ws, BalanceEntry, BinanceClient, FillSource, LiveEvent, UserDataWs,
};
use super::broker::{Broker, OpenOrder, PaperBroker};
use super::risk::{check_action, enforce_aggregate, RiskLimits, RiskState};

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
    paper_stash: Option<(PaperBroker, RiskState)>,
    /// Which slot placed an order (broker / exchange order id → slot symbol).
    /// A strategy only hears about fills of its OWN orders, and `on_stop`
    /// cancels only its own orders (audit D5).
    order_owner: HashMap<u64, String>,
    /// Live: client order id → slot symbol, until the exchange id is known.
    owner_by_cid: HashMap<String, String>,
    /// Live fill de-duplication (REST + user-data report the same fill).
    rest_filled: HashSet<u64>,
    seen_trades: HashSet<(u64, u64)>,
    /// Latest live-side problem the user must see (sync / cancel / unknown
    /// order).  `live_error_seq` bumps on every new one.
    pub live_error: Option<String>,
    pub live_error_seq: u64,
}

/// Bound on `Runtime::equity_history`.  Sized for ~5 minutes of 1.5s
/// samples — enough to read on a small terminal without quantizing.
pub const EQUITY_HISTORY_CAP: usize = 200;

/// Starting cash of a fresh paper ledger.
pub const PAPER_STARTING_CASH: f64 = 10_000.0;

impl Runtime {
    /// Wipe the paper ledger: broker (cash, positions, open orders), risk
    /// counters and the activity logs.  Slots and their strategies stay.
    /// Refuses in live mode, where the broker mirrors the real exchange.
    pub fn reset_paper(&mut self) -> Result<(), String> {
        if self.is_live() {
            return Err("live mode: the ledger mirrors the exchange".into());
        }
        self.broker = PaperBroker::new(PAPER_STARTING_CASH);
        self.risk_state = RiskState::default();
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
        }
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
        self.paper_stash = Some((paper, risk));
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
        if let Some((paper, risk)) = self.paper_stash.take() {
            self.broker = paper;
            self.risk_state = risk;
        }
        self.reset_activity();
        let _ = now;
    }

    /// Forget logs / ownership that belong to the ledger we just left.
    fn reset_activity(&mut self) {
        self.fill_log.clear();
        self.blocked_log.clear();
        self.equity_history.clear();
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
    pub fn paper_ledger(&self) -> (&PaperBroker, &RiskState) {
        match &self.paper_stash {
            Some((b, r)) => (b, r),
            None => (&self.broker, &self.risk_state),
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

    /// Equity delta vs. start-of-day snapshot.  Tracks daily P&L for the
    /// dashboard summary; rolls over via `RiskState::on_equity_update`.
    pub fn day_pnl(&self) -> f64 {
        self.equity() - self.risk_state.day_start_equity
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
            self.order_owner.retain(|_, owner| *owner != sym);
            self.slots.remove(idx);
        }
    }

    pub fn start(&mut self, idx: usize, now_ms: i64) -> Result<(), String> {
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

        let fills = self.broker.on_tick(symbol, price, now_ms);
        for f in fills {
            self.fill_log.push_back((now_ms, symbol.into(), f.clone()));
            while self.fill_log.len() > 128 {
                self.fill_log.pop_front();
            }
            self.notify_owner(&f, symbol, price, now_ms);
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

        let eq = self.broker.equity(&self.last_marks);
        self.risk_state.on_equity_update(eq, day_id(now_ms));
        enforce_aggregate(&mut self.limits, &mut self.risk_state, eq);
        self.push_equity(eq);
    }

    /// Append the latest equity sample to the rolling history buffer.
    /// Caps at `EQUITY_HISTORY_CAP`; drops the oldest sample when full.
    fn push_equity(&mut self, eq: f64) {
        // Skip identical consecutive samples to avoid plotting flatness
        // when no positions are open.
        if let Some(&last) = self.equity_history.back() {
            if (last - eq).abs() < 1e-6 {
                return;
            }
        }
        self.equity_history.push_back(eq);
        while self.equity_history.len() > EQUITY_HISTORY_CAP {
            self.equity_history.pop_front();
        }
    }

    /// Feed a closed bar into the runtime.  Updates the slot's bar history
    /// (capped at 1024 bars) and dispatches `on_bar`.
    pub fn on_bar(&mut self, symbol: &str, bar: Bar, now_ms: i64) {
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
                    },
                ));
                return Ok(vec![]);
            }
        };
        let last = self.last_marks.get(&sym).copied().unwrap_or(0.0);
        let pos = self.broker.position(&sym);
        let oo_count = self.broker.open_order_count();
        if let Err(e) = check_action(&action, &pos, &self.limits, oo_count) {
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
            return Ok(vec![]);
        }

        if let Action::CancelAll { symbol } = &action {
            self.order_owner.retain(|id, _| {
                self.broker
                    .all_open_orders()
                    .iter()
                    .any(|o| o.id == *id && o.symbol != *symbol)
            });
        }
        let fills = self.broker.submit(&action, last, now_ms);
        if let Some(i) = self.find_slot(&sym) {
            self.slots[i].push_log(format!("USER {}", action.label()));
        }
        for f in &fills {
            self.fill_log.push_back((now_ms, sym.clone(), f.clone()));
        }
        while self.fill_log.len() > 128 {
            self.fill_log.pop_front();
        }
        Ok(fills)
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
        if let Some(i) = self.slots.iter().position(|s| s.symbol == owner) {
            if self.slots[i].symbol == symbol && self.slots[i].status == SlotStatus::Running {
                self.dispatch_fill(i, fill, last, now_ms);
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
            self.order_owner.entry(ev.order_id).or_insert(owner);
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
                        self.order_owner.insert(order_id, owner);
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
            if let Err(e) =
                check_action(&action, &pos, &self.limits, self.broker.open_order_count())
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
                self.order_owner.insert(id, symbol.clone());
            }
            for f in fills {
                self.slots[slot_idx]
                    .push_log(format!("FILL {:?} {} @ {:.6}", f.side, f.qty, f.price));
            }
        }
    }

    /// Cancel every resting order the slot placed (paper: drop locally;
    /// live: ask the exchange, the cancel confirmation drops it from the mirror).
    fn cancel_owned(&mut self, slot_idx: usize) {
        let symbol = self.slots[slot_idx].symbol.clone();
        let ids: Vec<u64> = self
            .order_owner
            .iter()
            .filter(|(_, owner)| **owner == symbol)
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
}
