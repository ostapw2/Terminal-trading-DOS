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
    /// A mirror of the REAL exchange (live mode): it never simulates fills,
    /// it only records what the exchange reports (audit A5).
    mirror: bool,
}

impl PaperBroker {
    pub fn new(starting_cash: f64) -> Self {
        Self {
            starting_cash,
            positions: HashMap::new(),
            open_orders: Vec::new(),
            next_id: 1,
            mirror: false,
        }
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
        self.fill_market(symbol, side, qty, price, now_ms, order_id)
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

    fn fill_market(
        &mut self,
        symbol: &str,
        side: Side,
        qty: f64,
        price: f64,
        now_ms: i64,
        order_id: u64,
    ) -> Fill {
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
        }
    }
}

impl Broker for PaperBroker {
    fn submit(&mut self, action: &Action, last: f64, now_ms: i64) -> Vec<Fill> {
        match action {
            Action::BuyMarket { symbol, qty } if last > 0.0 && *qty > 0.0 => {
                vec![self.fill_market(symbol, Side::Buy, *qty, last, now_ms, 0)]
            }
            Action::SellMarket { symbol, qty } if last > 0.0 && *qty > 0.0 => {
                vec![self.fill_market(symbol, Side::Sell, *qty, last, now_ms, 0)]
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
                // Fill at the user's limit (better-or-equal price).
                fills.push(self.fill_market(symbol, o.side, o.qty, o.limit, now_ms, o.id));
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
        let mut eq = self.starting_cash;
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
        }
    }

    pub fn restore(snap: BrokerSnapshot) -> Self {
        let positions: HashMap<String, Position> = snap.positions.into_iter().collect();
        Self {
            starting_cash: snap.starting_cash,
            positions,
            open_orders: snap.open_orders,
            next_id: snap.next_id.max(1),
            mirror: false,
        }
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
