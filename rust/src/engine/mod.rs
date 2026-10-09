//! Trading engine: paper broker + risk gate + strategy runtime.
//!
//! The engine is the single owner of mutable trading state.  Strategies
//! emit [`crate::strategies::Action`]s, the runtime forwards them through
//! the [`risk`] gate to the [`broker`], which simulates fills.

pub mod binance;
pub mod broker;
pub mod risk;
pub mod runtime;

pub use binance::{
    BalanceEntry, BinanceClient, FillSource, KeyPermissions, LiveEvent, OpenOrderInfo, PROD_BASE,
    TESTNET_BASE,
};
pub use broker::{Broker, OpenOrder, PaperBroker};
pub use risk::{enforce_aggregate, RiskLimits, RiskState};
pub use runtime::{current_now_ms, Runtime, Slot, SlotStatus, PAPER_STARTING_CASH};
