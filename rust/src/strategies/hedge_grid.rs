//! Grid-style hedge strategy on a single symbol.
//!
//! On `start`, places a ladder of orders around the entry price:
//!
//! ```text
//!   above last:  N short LIMIT orders at last × (1 + spacing × i)  (i = 1..=levels_above)
//!   below last:  M long  LIMIT orders at last × (1 − spacing × i)  (i = 1..=levels_below)
//! ```
//!
//! Each fill triggers an opposite take-profit limit order at
//! `fill_price × (1 ± take_profit_pct)`.  As the market oscillates the
//! grid harvests each round-trip as realized PnL.
//!
//! On `stop`, every open limit on this symbol is cancelled.  Open
//! positions are *not* flattened — the user can hit `CLOSE` on the F4
//! ticket if they want a clean exit.
//!
//! Parameters (all UI-editable via the strategy panel):
//! - `levels_above` — how many short ladder rungs to seed
//! - `levels_below` — how many long ladder rungs
//! - `qty_per_level` — order size per rung
//! - `spacing_pct` — distance between adjacent rungs (% of last)
//! - `take_profit_pct` — distance of the auto-TP from each fill (%)

use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "levels_below",
        label: "Long levels",
        kind: ParamKind::Int {
            min: 0,
            max: 50,
            default: 4,
            step: 1,
        },
        help: "Long limit orders placed below entry.",
    },
    ParamSpec {
        key: "levels_above",
        label: "Short levels",
        kind: ParamKind::Int {
            min: 0,
            max: 50,
            default: 4,
            step: 1,
        },
        help: "Short limit orders placed above entry.",
    },
    ParamSpec {
        key: "qty_per_level",
        label: "Qty per leg",
        kind: ParamKind::Float {
            min: 0.0,
            max: 1.0e9,
            default: 0.1,
            step: 0.01,
        },
        help: "Order size of every grid rung.",
    },
    ParamSpec {
        key: "spacing_pct",
        label: "Spacing %",
        kind: ParamKind::Float {
            min: 0.01,
            max: 50.0,
            default: 0.5,
            step: 0.05,
        },
        help: "Distance between consecutive rungs (% of last).",
    },
    ParamSpec {
        key: "take_profit_pct",
        label: "TP %",
        kind: ParamKind::Float {
            min: 0.0,
            max: 50.0,
            default: 0.5,
            step: 0.05,
        },
        help: "Auto-TP placed at this distance from each fill.",
    },
];

pub struct HedgeGrid {
    levels_below: i64,
    levels_above: i64,
    qty_per_level: f64,
    spacing_pct: f64,
    take_profit_pct: f64,
    seeded: bool,
}

impl Default for HedgeGrid {
    fn default() -> Self {
        Self {
            levels_below: 4,
            levels_above: 4,
            qty_per_level: 0.1,
            spacing_pct: 0.5,
            take_profit_pct: 0.5,
            seeded: false,
        }
    }
}

impl Strategy for HedgeGrid {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "hedge_grid",
            name: "Hedge Grid",
            description: "Ladder of long+short limits with auto-TP on each fill.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "levels_below" => ParamValue::Int(self.levels_below),
            "levels_above" => ParamValue::Int(self.levels_above),
            "qty_per_level" => ParamValue::Float(self.qty_per_level),
            "spacing_pct" => ParamValue::Float(self.spacing_pct),
            "take_profit_pct" => ParamValue::Float(self.take_profit_pct),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        match key {
            "levels_below" => self.levels_below = value.as_int().ok_or("int expected")?.max(0),
            "levels_above" => self.levels_above = value.as_int().ok_or("int expected")?.max(0),
            "qty_per_level" => {
                self.qty_per_level = value.as_float().ok_or("number expected")?.max(0.0)
            }
            "spacing_pct" => {
                self.spacing_pct = value.as_float().ok_or("number expected")?.max(0.01)
            }
            "take_profit_pct" => {
                self.take_profit_pct = value.as_float().ok_or("number expected")?.max(0.0)
            }
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }

    fn on_start(&mut self, ctx: &mut Ctx) {
        if self.seeded {
            return;
        }
        let last = ctx.last;
        if last <= 0.0 || self.qty_per_level <= 0.0 {
            ctx.note("hedge_grid: no last price yet, retry on next start");
            return;
        }
        let qty = self.qty_per_level;
        let spacing = self.spacing_pct / 100.0;
        // Long ladder below.
        for i in 1..=self.levels_below {
            let price = last * (1.0 - spacing * i as f64);
            if price > 0.0 {
                ctx.buy_limit(qty, price);
            }
        }
        // Short ladder above.
        for i in 1..=self.levels_above {
            let price = last * (1.0 + spacing * i as f64);
            ctx.sell_limit(qty, price);
        }
        ctx.note(format!(
            "hedge_grid armed: {}L + {}S × {} qty, spacing {:.2}%, TP {:.2}%",
            self.levels_below, self.levels_above, qty, self.spacing_pct, self.take_profit_pct
        ));
        self.seeded = true;
    }

    fn on_fill(&mut self, ctx: &mut Ctx, fill: &Fill) {
        if self.take_profit_pct <= 0.0 || self.qty_per_level <= 0.0 {
            return;
        }
        let tp = self.take_profit_pct / 100.0;
        match fill.side {
            Side::Buy => {
                let tp_price = fill.price * (1.0 + tp);
                ctx.sell_limit(fill.qty, tp_price);
                ctx.note(format!(
                    "hedge_grid: TP sell {} @ {:.6} (entry {:.6})",
                    fill.qty, tp_price, fill.price
                ));
            }
            Side::Sell => {
                let tp_price = fill.price * (1.0 - tp);
                ctx.buy_limit(fill.qty, tp_price);
                ctx.note(format!(
                    "hedge_grid: TP buy {} @ {:.6} (entry {:.6})",
                    fill.qty, tp_price, fill.price
                ));
            }
        }
    }

    fn on_stop(&mut self, ctx: &mut Ctx) {
        ctx.cancel_all();
        ctx.note("hedge_grid: cancelled all open orders");
        self.seeded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at<'a>(now_ms: i64, last: f64, actions: &'a mut Vec<Action>) -> Ctx<'a> {
        Ctx {
            symbol: "BTCUSDT",
            now_ms,
            last,
            position: Position::default(),
            equity: 10_000.0,
            actions,
            bars: &[],
        }
    }

    #[test]
    fn on_start_seeds_long_and_short_ladders() {
        let mut s = HedgeGrid::default();
        s.set_param("levels_below", ParamValue::Int(3)).unwrap();
        s.set_param("levels_above", ParamValue::Int(2)).unwrap();
        s.set_param("qty_per_level", ParamValue::Float(0.5))
            .unwrap();
        s.set_param("spacing_pct", ParamValue::Float(1.0)).unwrap();

        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));

        let buys = actions
            .iter()
            .filter(|a| matches!(a, Action::BuyLimit { .. }))
            .count();
        let sells = actions
            .iter()
            .filter(|a| matches!(a, Action::SellLimit { .. }))
            .count();
        assert_eq!(buys, 3);
        assert_eq!(sells, 2);

        // First long should be 1% below last.
        let first_long = actions
            .iter()
            .find_map(|a| match a {
                Action::BuyLimit { limit, .. } => Some(*limit),
                _ => None,
            })
            .unwrap();
        assert!((first_long - 99.0).abs() < 1e-9);
    }

    #[test]
    fn on_fill_emits_opposite_tp_limit() {
        let mut s = HedgeGrid::default();
        s.set_param("take_profit_pct", ParamValue::Float(2.0))
            .unwrap();
        let mut actions = Vec::new();
        s.on_fill(
            &mut ctx_at(0, 100.0, &mut actions),
            &Fill {
                side: Side::Buy,
                qty: 0.1,
                price: 99.0,
                time_ms: 0,
                order_id: 0,
            },
        );
        let mut found = false;
        for a in &actions {
            if let Action::SellLimit { limit, qty, .. } = a {
                assert!((limit - 99.0 * 1.02).abs() < 1e-9);
                assert!((qty - 0.1).abs() < 1e-9);
                found = true;
            }
        }
        assert!(found, "expected a sell-limit TP");
    }

    #[test]
    fn on_stop_cancels_all() {
        let mut s = HedgeGrid::default();
        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        actions.clear();
        s.on_stop(&mut ctx_at(0, 100.0, &mut actions));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CancelAll { .. })));
    }
}
