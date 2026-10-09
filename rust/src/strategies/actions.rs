//! Side-effect actions a strategy emits.  The runtime forwards these to
//! the broker (paper or live) and to the UI log.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Action {
    BuyMarket {
        symbol: String,
        qty: f64,
    },
    SellMarket {
        symbol: String,
        qty: f64,
    },
    BuyLimit {
        symbol: String,
        qty: f64,
        limit: f64,
    },
    SellLimit {
        symbol: String,
        qty: f64,
        limit: f64,
    },
    CancelAll {
        symbol: String,
    },
    /// Free-form note rendered in the UI log; no broker side-effect.
    Note {
        text: String,
    },
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::BuyMarket { qty, .. } => format!("BUY MKT {qty}"),
            Self::SellMarket { qty, .. } => format!("SELL MKT {qty}"),
            Self::BuyLimit { qty, limit, .. } => format!("BUY LMT {qty} @{limit}"),
            Self::SellLimit { qty, limit, .. } => format!("SELL LMT {qty} @{limit}"),
            Self::CancelAll { .. } => "CANCEL ALL".into(),
            Self::Note { text } => format!("NOTE: {text}"),
        }
    }

    pub fn symbol(&self) -> Option<&str> {
        match self {
            Self::BuyMarket { symbol, .. }
            | Self::SellMarket { symbol, .. }
            | Self::BuyLimit { symbol, .. }
            | Self::SellLimit { symbol, .. }
            | Self::CancelAll { symbol } => Some(symbol),
            Self::Note { .. } => None,
        }
    }
}
