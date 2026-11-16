//! Paper broker — simulates fills against the latest tick price.
//!
//! Market orders fill immediately at `last`.  Limit orders are queued
//! and filled when the next tick crosses the limit price.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::strategies::{
    actions::Action,
    context::{Fill, Position, Side},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenOrder {
    pub id: u64,
    pub symbol: String,
    pub side: Side,
    pub qty: f64,
    pub limit: f64,
    pub created_ms: i64,
}

/// Serializable snapshot of [`PaperBroker`] state.  Used by the SQLite
/// persistence layer so paper positions + open limits survive restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrokerSnapshot {
    pub starting_cash: f64,
    pub positions: Vec<(String, Position)>,
    pub open_orders: Vec<OpenOrder>,
    pub next_id: u64,
    /// Stop-loss / take-profit per symbol (older snapshots have none).
    #[serde(default)]
    pub brackets: Vec<(String, Bracket)>,
    /// Fee rates and what was paid so far (older snapshots: the defaults, 0).
    #[serde(default = "d_maker")]
    pub fee_maker_bps: f64,
    #[serde(default = "d_taker")]
    pub fee_taker_bps: f64,
    #[serde(default)]
    pub fees_paid: f64,
    /// Perpetuals: leverage per instrument, funding moved so far, futures fee rates.
    #[serde(default)]
    pub leverage: Vec<(String, f64)>,
    #[serde(default)]
    pub funding: f64,
    #[serde(default = "d_fut_maker")]
    pub fut_maker_bps: f64,
    #[serde(default = "d_fut_taker")]
    pub fut_taker_bps: f64,
}

fn d_fut_maker() -> f64 {
    crate::futures::FEE_MAKER_BPS
}
fn d_fut_taker() -> f64 {
    crate::futures::FEE_TAKER_BPS
}

/// Binance Spot, VIP 0, no BNB discount: 10 bps both sides.
pub const FEE_MAKER_BPS: f64 = 10.0;
pub const FEE_TAKER_BPS: f64 = 10.0;
fn d_maker() -> f64 {
    FEE_MAKER_BPS
}
fn d_taker() -> f64 {
    FEE_TAKER_BPS
}

/// A position's protective exits: the whole position is closed at market
/// when the price reaches `sl` (loss side) or `tp` (profit side).  They are
/// held by the trading window, not by an exchange: nothing fires while no
/// window trades.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bracket {
    pub sl: Option<f64>,
    pub tp: Option<f64>,
    /// The side it was set for: a position reversed since is not the one
    /// these levels protect.
    #[serde(default)]
    pub long: bool,
}

impl Bracket {
    pub fn is_empty(&self) -> bool {
        self.sl.is_none() && self.tp.is_none()
    }

    /// Which exit `price` reaches for a position of signed `qty`, if any.
    pub fn hit(&self, qty: f64, price: f64) -> Option<Exit> {
        let long = qty > 0.0;
        let crossed = |level: f64, below: bool| {
            if below {
                price <= level
            } else {
                price >= level
            }
        };
        if self.sl.is_some_and(|l| crossed(l, long)) {
            return Some(Exit::StopLoss);
        }
        if self.tp.is_some_and(|l| crossed(l, !long)) {
            return Some(Exit::TakeProfit);
        }
        None
    }

    /// Why these levels cannot protect a position of signed `qty` at `price`
    /// (a stop must be on the losing side of the price, a take on the
    /// winning side), if they cannot.
    pub fn check(&self, qty: f64, price: f64) -> Result<(), String> {
        let ok = |l: f64| l.is_finite() && l > 0.0;
        if self.sl.is_some_and(|l| !ok(l)) || self.tp.is_some_and(|l| !ok(l)) {
            return Err("a level must be a positive price".into());
        }
        if qty == 0.0 {
            return Err("no position".into());
        }
        let long = qty > 0.0;
        if let Some(l) = self.sl {
            if long && l >= price {
                return Err(format!("a long's stop goes below the price ({price})"));
            }
            if !long && l <= price {
                return Err(format!("a short's stop goes above the price ({price})"));
            }
        }
        if let Some(l) = self.tp {
            if long && l <= price {
                return Err(format!("a long's take goes above the price ({price})"));
            }
            if !long && l >= price {
                return Err(format!("a short's take goes below the price ({price})"));
            }
        }
        Ok(())
    }
}

/// Which side of a [`Bracket`] closed a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    StopLoss,
    TakeProfit,
}

impl Exit {
    pub fn label(self) -> &'static str {
        match self {
            Exit::StopLoss => "SL",
            Exit::TakeProfit => "TP",
        }
    }
}

pub trait Broker {
    fn submit(&mut self, action: &Action, last: f64, now_ms: i64) -> Vec<Fill>;
    fn on_tick(&mut self, symbol: &str, price: f64, now_ms: i64) -> Vec<Fill>;
    fn position(&self, symbol: &str) -> Position;
    fn open_orders(&self, symbol: &str) -> Vec<OpenOrder>;
    fn equity(&self, marks: &HashMap<String, f64>) -> f64;
}

pub struct PaperBroker {
    pub starting_cash: f64,
    positions: HashMap<String, Position>,
    open_orders: Vec<OpenOrder>,
    next_id: u64,
    brackets: HashMap<String, Bracket>,
    /// A mirror of the REAL exchange (live mode): it never simulates fills,
    /// it only records what the exchange reports (audit A5).
    mirror: bool,
    /// Fee per fill, bps of notional: maker for a limit that rested and was
    /// crossed by a tick, taker for a market or a marketable limit.
    pub fee_maker_bps: f64,
    pub fee_taker_bps: f64,
    /// Every fee charged on this ledger so far; equity is net of it.
    fees_paid: f64,
    /// Perpetuals (`crate::futures`): the leverage of each instrument, the
    /// net cash funding has moved (negative: paid), and the futures fee rates.
    leverage: HashMap<String, f64>,
    funding: f64,
    pub fut_maker_bps: f64,
    pub fut_taker_bps: f64,
}

impl PaperBroker {
    pub fn new(starting_cash: f64) -> Self {
        Self {
            starting_cash,
            positions: HashMap::new(),
            open_orders: Vec::new(),
            next_id: 1,
            brackets: HashMap::new(),
            mirror: false,
            fee_maker_bps: FEE_MAKER_BPS,
            fee_taker_bps: FEE_TAKER_BPS,
            fees_paid: 0.0,
            leverage: HashMap::new(),
            funding: 0.0,
            fut_maker_bps: crate::futures::FEE_MAKER_BPS,
            fut_taker_bps: crate::futures::FEE_TAKER_BPS,
        }
    }

    /// Taker / maker fee rate of an instrument: futures rates for a perpetual.
    fn taker_bps(&self, symbol: &str) -> f64 {
        // A coin on another exchange (`BTCUSDT@BYBIT`): that exchange's taker.
        if let Some((_, ex)) = crate::exchanges::split_instrument(symbol) {
            return ex.taker_bps();
        }
        if crate::futures::is_perp(symbol) {
            self.fut_taker_bps
        } else {
            self.fee_taker_bps
        }
    }
    fn maker_bps(&self, symbol: &str) -> f64 {
        if let Some((_, ex)) = crate::exchanges::split_instrument(symbol) {
            return ex.taker_bps();
        }
        if crate::futures::is_perp(symbol) {
            self.fut_maker_bps
        } else {
            self.fee_maker_bps
        }
    }

    /// One rate for both sides (backtests take a single `fee_bps`).
    pub fn set_fee_bps(&mut self, bps: f64) {
        self.fee_maker_bps = bps;
        self.fee_taker_bps = bps;
    }

    /// Every fee charged on this ledger so far.
    pub fn fees_paid(&self) -> f64 {
        self.fees_paid
    }

    /// A ledger that mirrors the exchange: `on_tick` never fills, and state
    /// changes only through the `exchange_*` methods.
    pub fn mirror() -> Self {
        Self {
            mirror: true,
            ..Self::new(0.0)
        }
    }

    pub fn is_mirror(&self) -> bool {
        self.mirror
    }

    /// Id the next locally created order will get (to attribute new orders).
    pub fn peek_next_order_id(&self) -> u64 {
        self.next_id
    }

    /// Record a fill the exchange reported.  Reduces (or removes) the resting
    /// order `order_id` if we track it.
    pub fn exchange_fill(
        &mut self,
        symbol: &str,
        side: Side,
        qty: f64,
        price: f64,
        now_ms: i64,
        order_id: u64,
    ) -> Fill {
        if let Some(o) = self.open_orders.iter_mut().find(|o| o.id == order_id) {
            o.qty -= qty;
        }
        self.open_orders.retain(|o| o.qty > 1e-12);
        // The exchange charges its own fee; the mirror does not model it.
        self.fill_market(symbol, side, qty, price, now_ms, order_id, 0.0)
    }

    /// Start tracking an order the exchange accepted (replaces same id).
    pub fn exchange_order(&mut self, order: OpenOrder) {
        self.open_orders.retain(|o| o.id != order.id);
        self.open_orders.push(order);
    }

    /// Stop tracking one order.  Returns whether it was tracked.
    pub fn drop_order(&mut self, id: u64) -> bool {
        let before = self.open_orders.len();
        self.open_orders.retain(|o| o.id != id);
        self.open_orders.len() != before
    }

    /// Replace the tracked open orders with the exchange's list (reconcile).
    pub fn replace_open_orders(&mut self, orders: Vec<OpenOrder>) {
        self.open_orders = orders;
    }

    fn next_order_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_market(
        &mut self,
        symbol: &str,
        side: Side,
        qty: f64,
        price: f64,
        now_ms: i64,
        order_id: u64,
        fee_bps: f64,
    ) -> Fill {
        let fee = qty * price * fee_bps / 10_000.0;
        self.fees_paid += fee;
        let pos = self.positions.entry(symbol.into()).or_default();
        let signed = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };

        if pos.qty == 0.0 {
            // Opening from flat.
            pos.qty = signed;
            pos.avg = price;
        } else if pos.qty.signum() == signed.signum() {
            // Adding to existing direction → weighted avg.
            let abs_old = pos.qty.abs();
            let abs_add = qty;
            pos.avg = (pos.avg * abs_old + price * abs_add) / (abs_old + abs_add);
            pos.qty += signed;
        } else {
            // Reducing or flipping.
            let close_qty = qty.min(pos.qty.abs());
            let pnl_per = match side {
                Side::Sell => price - pos.avg, // closing long
                Side::Buy => pos.avg - price,  // closing short
            };
            pos.realized += pnl_per * close_qty;
            let new_qty = pos.qty + signed;
            if new_qty.abs() < 1e-12 {
                pos.qty = 0.0;
                pos.avg = 0.0;
            } else if new_qty.signum() != pos.qty.signum() {
                // Flipped — remainder opens at fill price.
                pos.qty = new_qty;
                pos.avg = price;
            } else {
                pos.qty = new_qty;
            }
        }

        Fill {
            side,
            qty,
            price,
            time_ms: now_ms,
            order_id,
            fee,
        }
    }
}

impl Broker for PaperBroker {
    fn submit(&mut self, action: &Action, last: f64, now_ms: i64) -> Vec<Fill> {
        match action {
            Action::BuyMarket { symbol, qty } if last > 0.0 && *qty > 0.0 => {
                let taker = self.taker_bps(symbol);
                vec![self.fill_market(symbol, Side::Buy, *qty, last, now_ms, 0, taker)]
            }
            Action::SellMarket { symbol, qty } if last > 0.0 && *qty > 0.0 => {
                let taker = self.taker_bps(symbol);
                vec![self.fill_market(symbol, Side::Sell, *qty, last, now_ms, 0, taker)]
            }
            // A limit that is already marketable (a buy at or above the last
            // price, a sell at or below it) trades at once at the market
            // price, as on a real exchange; it does not wait for a tick and
            // does not pay its own, worse, limit.
            Action::BuyLimit { symbol, qty, limit }
                if *qty > 0.0 && last > 0.0 && *limit >= last =>
            {
                let taker = self.taker_bps(symbol);
                vec![self.fill_market(symbol, Side::Buy, *qty, last, now_ms, 0, taker)]
            }
            Action::SellLimit { symbol, qty, limit }
                if *qty > 0.0 && last > 0.0 && *limit > 0.0 && *limit <= last =>
            {
                let taker = self.taker_bps(symbol);
                vec![self.fill_market(symbol, Side::Sell, *qty, last, now_ms, 0, taker)]
            }
            Action::BuyLimit { symbol, qty, limit } if *qty > 0.0 && *limit > 0.0 => {
                let id = self.next_order_id();
                self.open_orders.push(OpenOrder {
                    id,
                    symbol: symbol.clone(),
                    side: Side::Buy,
                    qty: *qty,
                    limit: *limit,
                    created_ms: now_ms,
                });
                vec![]
            }
            Action::SellLimit { symbol, qty, limit } if *qty > 0.0 && *limit > 0.0 => {
                let id = self.next_order_id();
                self.open_orders.push(OpenOrder {
                    id,
                    symbol: symbol.clone(),
                    side: Side::Sell,
                    qty: *qty,
                    limit: *limit,
                    created_ms: now_ms,
                });
                vec![]
            }
            Action::CancelAll { symbol } => {
                self.open_orders.retain(|o| o.symbol != *symbol);
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_tick(&mut self, symbol: &str, price: f64, now_ms: i64) -> Vec<Fill> {
        if price <= 0.0 || self.mirror {
            return vec![];
        }
        let mut fills = Vec::new();
        let drained: Vec<OpenOrder> = std::mem::take(&mut self.open_orders);
        let mut still_open = Vec::with_capacity(drained.len());
        for o in drained {
            if o.symbol != symbol {
                still_open.push(o);
                continue;
            }
            let crossed = match o.side {
                Side::Buy => price <= o.limit,
                Side::Sell => price >= o.limit,
            };
            if crossed {
                // Fill at the user's limit (better-or-equal price), as a maker.
                let maker = self.maker_bps(symbol);
                fills.push(self.fill_market(symbol, o.side, o.qty, o.limit, now_ms, o.id, maker));
            } else {
                still_open.push(o);
            }
        }
        self.open_orders = still_open;
        fills
    }

    fn position(&self, symbol: &str) -> Position {
        self.positions.get(symbol).cloned().unwrap_or_default()
    }

    fn open_orders(&self, symbol: &str) -> Vec<OpenOrder> {
        self.open_orders
            .iter()
            .filter(|o| o.symbol == symbol)
            .cloned()
            .collect()
    }

    fn equity(&self, marks: &HashMap<String, f64>) -> f64 {
        let mut eq = self.starting_cash - self.fees_paid + self.funding;
        for (sym, pos) in &self.positions {
            eq += pos.realized;
            if !pos.is_flat() {
                let mark = marks.get(sym).copied().unwrap_or(pos.avg);
                eq += pos.unrealized(mark);
            }
        }
        eq
    }
}

impl PaperBroker {
    pub fn all_positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }
    pub fn all_open_orders(&self) -> &[OpenOrder] {
        &self.open_orders
    }
    pub fn open_order_count(&self) -> usize {
        self.open_orders.len()
    }

    /// The stop / take of `symbol` (empty when none).
    pub fn bracket(&self, symbol: &str) -> Bracket {
        self.brackets.get(symbol).copied().unwrap_or_default()
    }

    /// Set (or, when empty, remove) the stop / take of `symbol`.
    pub fn set_bracket(&mut self, symbol: &str, b: Bracket) {
        if b.is_empty() {
            self.brackets.remove(symbol);
        } else {
            self.brackets.insert(symbol.to_string(), b);
        }
    }

    /// Snapshot every internal piece of state worth persisting.
    /// Round-trip via [`PaperBroker::restore`].
    pub fn snapshot(&self) -> BrokerSnapshot {
        BrokerSnapshot {
            starting_cash: self.starting_cash,
            positions: self
                .positions
                .iter()
                .map(|(s, p)| (s.clone(), p.clone()))
                .collect(),
            open_orders: self.open_orders.clone(),
            next_id: self.next_id,
            brackets: {
                let mut b: Vec<(String, Bracket)> =
                    self.brackets.iter().map(|(s, b)| (s.clone(), *b)).collect();
                b.sort_by(|x, y| x.0.cmp(&y.0));
                b
            },
            fee_maker_bps: self.fee_maker_bps,
            fee_taker_bps: self.fee_taker_bps,
            fees_paid: self.fees_paid,
            leverage: {
                let mut l: Vec<(String, f64)> =
                    self.leverage.iter().map(|(s, v)| (s.clone(), *v)).collect();
                l.sort_by(|x, y| x.0.cmp(&y.0));
                l
            },
            funding: self.funding,
            fut_maker_bps: self.fut_maker_bps,
            fut_taker_bps: self.fut_taker_bps,
        }
    }

    pub fn restore(snap: BrokerSnapshot) -> Self {
        let positions: HashMap<String, Position> = snap.positions.into_iter().collect();
        Self {
            starting_cash: snap.starting_cash,
            positions,
            open_orders: snap.open_orders,
            next_id: snap.next_id.max(1),
            brackets: snap.brackets.into_iter().collect(),
            mirror: false,
            fee_maker_bps: snap.fee_maker_bps,
            fee_taker_bps: snap.fee_taker_bps,
            fees_paid: snap.fees_paid,
            leverage: snap.leverage.into_iter().collect(),
            funding: snap.funding,
            fut_maker_bps: snap.fut_maker_bps,
            fut_taker_bps: snap.fut_taker_bps,
        }
    }

    // ── Perpetuals (`crate::futures`) ───────────────────────────────────

    /// Set the leverage of a perpetual (a spot instrument stays at 1x).
    pub fn set_leverage(&mut self, symbol: &str, lev: f64) {
        if crate::futures::is_perp(symbol) {
            self.leverage
                .insert(symbol.to_string(), crate::futures::clamp_leverage(lev));
        }
    }

    /// Was a leverage ever set for this perpetual?
    pub fn has_leverage(&self, symbol: &str) -> bool {
        self.leverage.contains_key(symbol)
    }

    pub fn leverage(&self, symbol: &str) -> f64 {
        if crate::futures::is_perp(symbol) {
            self.leverage
                .get(symbol)
                .copied()
                .unwrap_or(crate::futures::MIN_LEVERAGE)
        } else {
            1.0
        }
    }

    /// Dollars the open positions tie up: a perpetual's notional at entry
    /// over its leverage; a spot position's whole cost.
    pub fn margin_used(&self) -> f64 {
        self.positions
            .iter()
            .filter(|(_, p)| !p.is_flat())
            .map(|(s, p)| crate::futures::initial_margin(p.qty * p.avg, self.leverage(s)))
            .sum()
    }

    /// Dollars the resting orders would tie up if they filled.
    pub fn margin_reserved(&self) -> f64 {
        self.open_orders
            .iter()
            .map(|o| crate::futures::initial_margin(o.qty * o.limit, self.leverage(&o.symbol)))
            .sum()
    }

    /// The price a perpetual position is closed by force at, if it has one.
    pub fn liquidation_price(&self, symbol: &str) -> Option<f64> {
        let p = self.positions.get(symbol).filter(|p| !p.is_flat())?;
        if !crate::futures::is_perp(symbol) {
            return None;
        }
        crate::futures::liquidation_price(p.avg, p.qty > 0.0, self.leverage(symbol))
    }

    /// Does `price` liquidate the perpetual position of `symbol`?
    pub fn is_liquidated_at(&self, symbol: &str, price: f64) -> bool {
        match self.positions.get(symbol).filter(|p| !p.is_flat()) {
            Some(p) if crate::futures::is_perp(symbol) => {
                crate::futures::is_liquidated(p.avg, p.qty > 0.0, self.leverage(symbol), price)
            }
            _ => false,
        }
    }

    /// Close the whole position by force at its liquidation price, with the
    /// clearance fee; its resting orders go too.  The fill carries order id 0.
    pub fn liquidate(&mut self, symbol: &str, now_ms: i64) -> Option<Fill> {
        let liq = self.liquidation_price(symbol)?;
        let pos = self.positions.get(symbol)?.clone();
        let side = if pos.qty > 0.0 { Side::Sell } else { Side::Buy };
        self.open_orders.retain(|o| o.symbol != symbol);
        self.brackets.remove(symbol);
        let fee_bps = crate::futures::LIQ_FEE_RATE * 10_000.0;
        Some(self.fill_market(symbol, side, pos.qty.abs(), liq, now_ms, 0, fee_bps))
    }

    /// One funding time for `symbol` at `rate` (per 8 h): the cash it moves
    /// (negative: this ledger pays).
    pub fn apply_funding(&mut self, symbol: &str, mark: f64, rate: f64) -> f64 {
        let Some(p) = self.positions.get(symbol).filter(|p| !p.is_flat()) else {
            return 0.0;
        };
        if !crate::futures::is_perp(symbol) || self.mirror {
            return 0.0;
        }
        let cash = crate::futures::funding_payment(p.qty, mark, rate);
        self.funding += cash;
        cash
    }

    /// Net funding moved so far.
    pub fn funding_total(&self) -> f64 {
        self.funding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buy(sym: &str, qty: f64) -> Action {
        Action::BuyMarket {
            symbol: sym.into(),
            qty,
        }
    }
    fn sell(sym: &str, qty: f64) -> Action {
        Action::SellMarket {
            symbol: sym.into(),
            qty,
        }
    }

    #[test]
    fn market_buy_opens_long_position() {
        let mut b = PaperBroker::new(10_000.0);
        b.submit(&buy("BTC", 1.0), 100.0, 1);
        let pos = b.position("BTC");
        assert!((pos.qty - 1.0).abs() < 1e-9);
        assert!((pos.avg - 100.0).abs() < 1e-9);
        assert_eq!(pos.realized, 0.0);
    }

    #[test]
    fn second_buy_averages_in() {
        let mut b = PaperBroker::new(10_000.0);
        b.submit(&buy("BTC", 1.0), 100.0, 1);
        b.submit(&buy("BTC", 1.0), 200.0, 2);
        let pos = b.position("BTC");
        assert!((pos.qty - 2.0).abs() < 1e-9);
        assert!((pos.avg - 150.0).abs() < 1e-9);
    }

    #[test]
    fn closing_realizes_pnl() {
        let mut b = PaperBroker::new(10_000.0);
        b.submit(&buy("BTC", 1.0), 100.0, 1);
        b.submit(&sell("BTC", 1.0), 150.0, 2);
        let pos = b.position("BTC");
        assert!(pos.is_flat());
        assert!((pos.realized - 50.0).abs() < 1e-9);
    }

    #[test]
    fn limit_buy_fills_when_price_crosses() {
        let mut b = PaperBroker::new(10_000.0);
        b.submit(
            &Action::BuyLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 90.0,
            },
            100.0,
            1,
        );
        // Price stays above — no fill.
        let f1 = b.on_tick("BTC", 95.0, 2);
        assert!(f1.is_empty());
        // Price drops to 90 — order fills at limit.
        let f2 = b.on_tick("BTC", 89.0, 3);
        assert_eq!(f2.len(), 1);
        let pos = b.position("BTC");
        assert!((pos.qty - 1.0).abs() < 1e-9);
        assert!((pos.avg - 90.0).abs() < 1e-9);
    }

    #[test]
    fn a_marketable_limit_fills_at_the_market_not_at_its_own_price() {
        let mut b = PaperBroker::new(10_000.0);
        // Buy limit 5% above the market: trades now, at 100.
        let f = b.submit(
            &Action::BuyLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 105.0,
            },
            100.0,
            1,
        );
        assert_eq!(f.len(), 1);
        assert!(
            (f[0].price - 100.0).abs() < 1e-9,
            "no 5% overpay: {}",
            f[0].price
        );
        assert_eq!(b.open_order_count(), 0, "nothing rests");
        // Sell limit below the market: sells now, at 100.
        let f = b.submit(
            &Action::SellLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 95.0,
            },
            100.0,
            2,
        );
        assert!((f[0].price - 100.0).abs() < 1e-9);
        // A limit exactly at the market is marketable too.
        let f = b.submit(
            &Action::BuyLimit {
                symbol: "ETH".into(),
                qty: 1.0,
                limit: 50.0,
            },
            50.0,
            3,
        );
        assert_eq!(f.len(), 1);
        // Non-marketable limits still rest.
        let f = b.submit(
            &Action::BuyLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 90.0,
            },
            100.0,
            4,
        );
        assert!(f.is_empty());
        let f = b.submit(
            &Action::SellLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 110.0,
            },
            100.0,
            5,
        );
        assert!(f.is_empty());
        assert_eq!(b.open_order_count(), 2);
        // Without a price (last = 0) nothing can be marketable: it rests.
        let f = b.submit(
            &Action::BuyLimit {
                symbol: "XRP".into(),
                qty: 1.0,
                limit: 1.0,
            },
            0.0,
            6,
        );
        assert!(f.is_empty());
        assert_eq!(b.open_order_count(), 3);
    }

    #[test]
    fn makers_pay_less_than_takers_and_equity_is_net_of_fees() {
        let mut b = PaperBroker::new(10_000.0);
        b.fee_maker_bps = 2.0;
        b.fee_taker_bps = 10.0;
        // A market buy of 1 @ 100: taker, 0.10.
        let f = b.submit(&buy("BTC", 1.0), 100.0, 1);
        assert!((f[0].fee - 0.10).abs() < 1e-12, "{:?}", f[0]);
        // A marketable limit is a taker too.
        let f = b.submit(
            &Action::SellLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 90.0,
            },
            100.0,
            2,
        );
        assert!((f[0].fee - 0.10).abs() < 1e-12);
        // A resting limit crossed by a tick is a maker: 1 @ 95 -> 0.019.
        assert!(b
            .submit(
                &Action::BuyLimit {
                    symbol: "BTC".into(),
                    qty: 1.0,
                    limit: 95.0,
                },
                100.0,
                3
            )
            .is_empty());
        let f = b.on_tick("BTC", 94.0, 4);
        assert_eq!(f.len(), 1);
        assert!((f[0].fee - 0.019).abs() < 1e-12, "{:?}", f[0]);
        assert!((b.fees_paid() - 0.219).abs() < 1e-12);
        // Equity: cash less fees, plus the position marked.
        let marks = HashMap::from([("BTC".to_string(), 95.0)]);
        assert!((b.equity(&marks) - (10_000.0 - 0.219)).abs() < 1e-9);
        // Realized stays gross.
        assert_eq!(b.position("BTC").realized, 0.0);
        // The snapshot keeps the rates and the fees; an old one restores the defaults.
        let back = PaperBroker::restore(b.snapshot());
        assert!((back.fees_paid() - 0.219).abs() < 1e-12 && back.fee_maker_bps == 2.0);
        let old: BrokerSnapshot = serde_json::from_str(
            r#"{"starting_cash":5.0,"positions":[],"open_orders":[],"next_id":3}"#,
        )
        .unwrap();
        let old = PaperBroker::restore(old);
        assert_eq!(old.fees_paid(), 0.0);
        assert_eq!(
            (old.fee_maker_bps, old.fee_taker_bps),
            (FEE_MAKER_BPS, FEE_TAKER_BPS)
        );
        // The exchange mirror charges nothing.
        let mut m = PaperBroker::mirror();
        let f = m.exchange_fill("BTC", Side::Buy, 1.0, 100.0, 1, 7);
        assert_eq!(f.fee, 0.0);
        assert_eq!(m.fees_paid(), 0.0);
    }

    #[test]
    fn cancel_all_clears_open_orders() {
        let mut b = PaperBroker::new(10_000.0);
        b.submit(
            &Action::BuyLimit {
                symbol: "BTC".into(),
                qty: 1.0,
                limit: 90.0,
            },
            100.0,
            1,
        );
        assert_eq!(b.open_order_count(), 1);
        b.submit(
            &Action::CancelAll {
                symbol: "BTC".into(),
            },
            100.0,
            2,
        );
        assert_eq!(b.open_order_count(), 0);
    }
}
