//! What a strategy sees on each callback.
//!
//! `Ctx` is built fresh by the runtime for each event.  It exposes the
//! current symbol, latest tick, the strategy's own position with the
//! broker, account equity, and a writable `actions` sink.  Convenience
//! methods (`buy_market`, etc.) push typed `Action`s so strategy code
//! reads naturally:
//!
//! ```ignore
//! ctx.buy_market(0.1);
//! ctx.note("Long opened");
//! ```

use serde::{Deserialize, Serialize};

use super::actions::Action;

#[derive(Clone, Debug)]
pub struct Tick {
    pub price: f64,
    pub time_ms: i64,
}

#[derive(Clone, Debug)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub time_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fill {
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub time_ms: i64,
    /// Order that produced this fill (broker / exchange id); 0 for an
    /// immediate market fill that never rested in the book.
    #[serde(default)]
    pub order_id: u64,
}

/// Strategy-visible position snapshot.  `qty` is signed: long > 0, short < 0.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Position {
    pub qty: f64,
    pub avg: f64,
    pub realized: f64,
}

impl Position {
    pub fn is_flat(&self) -> bool {
        self.qty.abs() < 1e-12
    }
    pub fn unrealized(&self, last: f64) -> f64 {
        if self.is_flat() {
            return 0.0;
        }
        (last - self.avg) * self.qty
    }
}

pub struct Ctx<'a> {
    pub symbol: &'a str,
    pub now_ms: i64,
    pub last: f64,
    pub position: Position,
    pub equity: f64,
    pub actions: &'a mut Vec<Action>,
    /// Recent bar history (oldest first).  Empty until `on_bar` has fired.
    pub bars: &'a [Bar],
}

impl<'a> Ctx<'a> {
    pub fn buy_market(&mut self, qty: f64) {
        self.actions.push(Action::BuyMarket {
            symbol: self.symbol.into(),
            qty,
        });
    }
    pub fn sell_market(&mut self, qty: f64) {
        self.actions.push(Action::SellMarket {
            symbol: self.symbol.into(),
            qty,
        });
    }
    pub fn buy_limit(&mut self, qty: f64, limit: f64) {
        self.actions.push(Action::BuyLimit {
            symbol: self.symbol.into(),
            qty,
            limit,
        });
    }
    pub fn sell_limit(&mut self, qty: f64, limit: f64) {
        self.actions.push(Action::SellLimit {
            symbol: self.symbol.into(),
            qty,
            limit,
        });
    }
    pub fn cancel_all(&mut self) {
        self.actions.push(Action::CancelAll {
            symbol: self.symbol.into(),
        });
    }
    pub fn note(&mut self, msg: impl Into<String>) {
        self.actions.push(Action::Note { text: msg.into() });
    }
}
