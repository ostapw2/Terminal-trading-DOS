//! Global risk limits.  Evaluated *before* each action is forwarded to
//! the broker.  Aggregate limits (drawdown, daily P&L) are checked
//! against the broker's running equity in [`Runtime::tick_risk`].

use serde::{Deserialize, Serialize};

use crate::strategies::actions::Action;
use crate::strategies::context::Position;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Maximum |position qty| any single symbol may hold.
    pub max_abs_position: f64,
    /// Maximum simultaneous open limit orders across all symbols.
    pub max_open_orders: usize,
    /// Equity drop below `day_start_equity` that trips the kill switch.
    /// Expressed as a positive number (e.g. 100.0 = stop after losing $100 today).
    pub max_daily_loss: f64,
    /// Drawdown from `equity_peak` (percent) that trips the kill switch.
    pub max_drawdown_pct: f64,
    /// User-toggled global kill switch.  When true, every action is
    /// rejected and running strategies receive no further events.
    pub kill_switch: bool,
    /// Baskets (`crate::basket`): dollars one basket may hold, dollars all
    /// open baskets may hold together, legs per basket.
    #[serde(default = "d_basket_usd")]
    pub max_basket_usd: f64,
    #[serde(default = "d_basket_usd")]
    pub max_baskets_usd: f64,
    #[serde(default = "d_basket_legs")]
    pub max_basket_legs: usize,
}

fn d_basket_usd() -> f64 {
    1.0e9
}
fn d_basket_legs() -> usize {
    crate::basket::MAX_LEGS
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_abs_position: 1.0e6,
            // Three full baskets of resting legs fit under it.
            max_open_orders: 2_000,
            max_daily_loss: 1.0e9,
            max_drawdown_pct: 100.0,
            kill_switch: false,
            max_basket_usd: d_basket_usd(),
            max_baskets_usd: d_basket_usd(),
            max_basket_legs: d_basket_legs(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RiskState {
    pub equity_peak: f64,
    pub day_start_equity: f64,
    pub day_unix: i64, // YYYYMMDD style, used to detect day rollover
    pub last_violation: Option<String>,
}

impl RiskState {
    pub fn on_equity_update(&mut self, eq: f64, day_unix: i64) {
        if self.day_start_equity == 0.0 || day_unix != self.day_unix {
            self.day_start_equity = eq;
            self.day_unix = day_unix;
        }
        if eq > self.equity_peak {
            self.equity_peak = eq;
        }
    }
}

/// Actions that can only shrink exposure and so stay allowed under the kill
/// switch: cancelling orders, notes, and a market order that closes (part of)
/// the open position without flipping it.  Blocking those would trap the
/// user in the very position the switch tripped on.
fn reduces_risk(action: &Action, pos: &Position) -> bool {
    match action {
        Action::CancelAll { .. } | Action::Note { .. } => true,
        Action::SellMarket { qty, .. } => pos.qty > 0.0 && *qty <= pos.qty + 1e-12,
        Action::BuyMarket { qty, .. } => pos.qty < 0.0 && *qty <= -pos.qty + 1e-12,
        _ => false,
    }
}

/// Pre-trade check against a candidate action.  Returns `Err(reason)` if
/// the runtime should drop the action (and surface the reason in the UI).
///
/// `pos` is the position of the ledger the order goes to (the kill switch lets
/// it shrink); `other_qty` is what the other ledger holds in the same symbol
/// (the human's and the scalper's ledgers share one position limit), and
/// `open_order_count` counts both ledgers.
pub fn check_action(
    action: &Action,
    pos: &Position,
    other_qty: f64,
    limits: &RiskLimits,
    open_order_count: usize,
) -> Result<(), String> {
    if limits.kill_switch && !reduces_risk(action, pos) {
        return Err(crate::i18n::pick("kill switch active", "стоп-кран увімкнено").into());
    }
    match action {
        Action::BuyMarket { qty, .. } | Action::BuyLimit { qty, .. } => {
            let held = pos.qty + other_qty;
            let projected = (held + qty).abs();
            if projected > limits.max_abs_position {
                return Err(crate::i18n::pick(
                    format!(
                        "max position {} exceeded ({} → {})",
                        limits.max_abs_position, held, projected
                    ),
                    format!(
                        "перевищено макс. позицію {} ({} → {})",
                        limits.max_abs_position, held, projected
                    ),
                ));
            }
        }
        Action::SellMarket { qty, .. } | Action::SellLimit { qty, .. } => {
            let held = pos.qty + other_qty;
            let projected = (held - qty).abs();
            if projected > limits.max_abs_position {
                return Err(crate::i18n::pick(
                    format!(
                        "max position {} exceeded ({} → {})",
                        limits.max_abs_position, held, projected
                    ),
                    format!(
                        "перевищено макс. позицію {} ({} → {})",
                        limits.max_abs_position, held, projected
                    ),
                ));
            }
        }
        Action::CancelAll { .. } | Action::Note { .. } => return Ok(()),
    }
    if matches!(action, Action::BuyLimit { .. } | Action::SellLimit { .. })
        && open_order_count >= limits.max_open_orders
    {
        return Err(crate::i18n::pick(
            format!("max open orders {} reached", limits.max_open_orders),
            format!(
                "досягнуто макс. відкритих ордерів {}",
                limits.max_open_orders
            ),
        ));
    }
    Ok(())
}

/// Aggregate post-trade check.  Mutates `limits.kill_switch` if a hard
/// limit is breached; the runtime then drops every subsequent action.
pub fn enforce_aggregate(limits: &mut RiskLimits, state: &mut RiskState, eq: f64) {
    if limits.kill_switch {
        return;
    }
    let dd = if state.equity_peak > 0.0 {
        (state.equity_peak - eq) / state.equity_peak * 100.0
    } else {
        0.0
    };
    if dd > limits.max_drawdown_pct {
        limits.kill_switch = true;
        state.last_violation = Some(format!(
            "drawdown {:.2}% > {:.2}%",
            dd, limits.max_drawdown_pct
        ));
        return;
    }
    let day_pnl = eq - state.day_start_equity;
    if -day_pnl > limits.max_daily_loss {
        limits.kill_switch = true;
        state.last_violation = Some(format!(
            "daily loss {:.2} > {:.2}",
            -day_pnl, limits.max_daily_loss
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(qty: f64) -> Position {
        Position {
            qty,
            avg: 100.0,
            realized: 0.0,
        }
    }

    fn killed() -> RiskLimits {
        RiskLimits {
            kill_switch: true,
            ..RiskLimits::default()
        }
    }

    fn sell(qty: f64) -> Action {
        Action::SellMarket {
            symbol: "X".into(),
            qty,
        }
    }

    #[test]
    fn kill_switch_blocks_new_exposure_but_not_exits() {
        let k = killed();
        let long = pos(2.0);
        // Cancels and notes always pass.
        assert!(check_action(&Action::CancelAll { symbol: "X".into() }, &long, 0.0, &k, 0).is_ok());
        // Closing (part of) the long is allowed; flipping or adding is not.
        assert!(check_action(&sell(2.0), &long, 0.0, &k, 0).is_ok());
        assert!(check_action(&sell(1.0), &long, 0.0, &k, 0).is_ok());
        assert!(
            check_action(&sell(3.0), &long, 0.0, &k, 0).is_err(),
            "would flip short"
        );
        let buy = Action::BuyMarket {
            symbol: "X".into(),
            qty: 1.0,
        };
        assert!(
            check_action(&buy, &long, 0.0, &k, 0).is_err(),
            "adding to a long"
        );
        // Short side mirrors.
        assert!(check_action(&buy, &pos(-1.0), 0.0, &k, 0).is_ok());
        // A fresh limit order is new exposure.
        let lim = Action::BuyLimit {
            symbol: "X".into(),
            qty: 1.0,
            limit: 1.0,
        };
        assert!(check_action(&lim, &pos(-1.0), 0.0, &k, 0).is_err());
        // Flat: nothing to reduce.
        assert!(check_action(&sell(1.0), &pos(0.0), 0.0, &k, 0).is_err());
        // The other ledger's position is not ours to reduce.
        assert!(check_action(&sell(1.0), &pos(0.0), 5.0, &k, 0).is_err());
    }

    #[test]
    fn the_position_limit_counts_both_ledgers() {
        let lim = RiskLimits {
            max_abs_position: 3.0,
            ..RiskLimits::default()
        };
        let buy = |qty| Action::BuyMarket {
            symbol: "X".into(),
            qty,
        };
        assert!(check_action(&buy(1.0), &pos(1.0), 0.0, &lim, 0).is_ok());
        let e = check_action(&buy(1.0), &pos(1.0), 1.5, &lim, 0).unwrap_err();
        assert!(e.contains("max position"), "{e}");
        // Opposite positions net out.
        assert!(check_action(&buy(1.0), &pos(1.0), -2.0, &lim, 0).is_ok());
    }
}
