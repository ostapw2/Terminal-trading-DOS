//! Strategy registry — one place to list every available strategy.

use super::{
    breakout::Breakout, dca_timer::DcaTimer, hedge_grid::HedgeGrid, limit_entry::LimitEntry,
    manual_market::ManualMarket, rsi_reversion::RsiReversion, sma_cross::SmaCross, Strategy,
    StrategyMeta,
};

/// Build a fresh strategy instance by id.  Returns `None` if unknown.
pub fn instantiate(id: &str) -> Option<Box<dyn Strategy>> {
    match id {
        "manual_market" => Some(Box::new(ManualMarket)),
        "dca_timer" => Some(Box::new(DcaTimer::default())),
        "limit_entry" => Some(Box::new(LimitEntry::default())),
        "hedge_grid" => Some(Box::new(HedgeGrid::default())),
        "sma_cross" => Some(Box::new(SmaCross::default())),
        "rsi_reversion" => Some(Box::new(RsiReversion::default())),
        "breakout" => Some(Box::new(Breakout::default())),
        _ => None,
    }
}

/// Every registered strategy's metadata, in the order shown by the UI.
pub fn catalog() -> Vec<StrategyMeta> {
    vec![
        ManualMarket.meta(),
        DcaTimer::default().meta(),
        LimitEntry::default().meta(),
        HedgeGrid::default().meta(),
        SmaCross::default().meta(),
        RsiReversion::default().meta(),
        Breakout::default().meta(),
    ]
}

/// First registered id (used as a UI default).
pub fn default_id() -> &'static str {
    "manual_market"
}
