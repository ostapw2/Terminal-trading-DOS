//! Manual strategy — no automation.  Used when the user wants to drive
//! the position by hand via the terminal command bar.  Exists as a slot
//! type so the symbol shows up in the selection panel with a position +
//! PnL view.

use super::*;

#[derive(Default)]
pub struct ManualMarket;

impl Strategy for ManualMarket {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "manual_market",
            name: "Manual",
            description: "No automation — trade via terminal commands.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        &[]
    }
    fn get_param(&self, _key: &str) -> Option<ParamValue> {
        None
    }
    fn set_param(&mut self, _key: &str, _value: ParamValue) -> Result<(), String> {
        Ok(())
    }
}
