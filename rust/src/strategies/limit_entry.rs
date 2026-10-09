//! One-shot limit-entry strategy.  On `start`, places a single limit
//! order at `last_price ± dist_pct`.  On `stop`, cancels open orders.

use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "side_buy",
        label: "Buy side",
        kind: ParamKind::Bool { default: true },
        help: "On = buy limit (below price).  Off = sell limit (above).",
    },
    ParamSpec {
        key: "qty",
        label: "Qty",
        kind: ParamKind::Float {
            min: 0.0,
            max: 1.0e9,
            default: 1.0,
            step: 0.1,
        },
        help: "Order size.",
    },
    ParamSpec {
        key: "dist_pct",
        label: "Distance %",
        kind: ParamKind::Float {
            min: 0.0,
            max: 100.0,
            default: 0.5,
            step: 0.1,
        },
        help: "Place limit this far from last price (better side).",
    },
];

pub struct LimitEntry {
    side_buy: bool,
    qty: f64,
    dist_pct: f64,
    placed: bool,
}

impl Default for LimitEntry {
    fn default() -> Self {
        Self {
            side_buy: true,
            qty: 1.0,
            dist_pct: 0.5,
            placed: false,
        }
    }
}

impl Strategy for LimitEntry {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "limit_entry",
            name: "Limit Entry",
            description: "Place a single limit order on start.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "side_buy" => ParamValue::Bool(self.side_buy),
            "qty" => ParamValue::Float(self.qty),
            "dist_pct" => ParamValue::Float(self.dist_pct),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        match key {
            "side_buy" => self.side_buy = value.as_bool().ok_or("bool expected")?,
            "qty" => self.qty = value.as_float().ok_or("number expected")?.max(0.0),
            "dist_pct" => self.dist_pct = value.as_float().ok_or("number expected")?.max(0.0),
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }
    fn on_start(&mut self, ctx: &mut Ctx) {
        self.try_place(ctx);
    }
    /// No mark price yet at start (feed still connecting): keep trying on
    /// ticks instead of sitting "Running" forever with no order.
    fn on_tick(&mut self, ctx: &mut Ctx, _t: &Tick) {
        self.try_place(ctx);
    }
    fn on_stop(&mut self, ctx: &mut Ctx) {
        if self.placed {
            ctx.cancel_all();
            ctx.note("LMT cancelled");
            self.placed = false;
        }
    }
}

impl LimitEntry {
    fn try_place(&mut self, ctx: &mut Ctx) {
        if self.placed || ctx.last <= 0.0 {
            return;
        }
        let limit = if self.side_buy {
            ctx.last * (1.0 - self.dist_pct / 100.0)
        } else {
            ctx.last * (1.0 + self.dist_pct / 100.0)
        };
        if self.side_buy {
            ctx.buy_limit(self.qty, limit);
        } else {
            ctx.sell_limit(self.qty, limit);
        }
        ctx.note(format!(
            "{} LMT {} @ {:.6}",
            if self.side_buy { "BUY" } else { "SELL" },
            self.qty,
            limit
        ));
        self.placed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(last: f64, actions: &'a mut Vec<Action>) -> Ctx<'a> {
        Ctx {
            symbol: "X",
            now_ms: 0,
            last,
            position: Position::default(),
            equity: 0.0,
            actions,
            bars: &[],
        }
    }

    #[test]
    fn places_on_the_first_tick_with_a_price_when_start_had_none() {
        let mut s = LimitEntry::default();
        let mut a = Vec::new();
        s.on_start(&mut ctx(0.0, &mut a));
        assert!(a.is_empty(), "no price yet, no order");
        s.on_tick(
            &mut ctx(100.0, &mut a),
            &Tick {
                price: 100.0,
                time_ms: 1,
            },
        );
        assert!(a.iter().any(|x| matches!(x, Action::BuyLimit { .. })));
        // Exactly once.
        let n = a.len();
        s.on_tick(
            &mut ctx(101.0, &mut a),
            &Tick {
                price: 101.0,
                time_ms: 2,
            },
        );
        assert_eq!(a.len(), n);
    }

    #[test]
    fn out_of_range_params_are_clamped_by_the_schema() {
        let mut s = LimitEntry::default();
        s.set_param_checked("qty", ParamValue::Float(1.0e12))
            .unwrap();
        s.set_param_checked("dist_pct", ParamValue::Float(f64::NAN))
            .unwrap();
        assert_eq!(s.get_param("qty").unwrap().as_float(), Some(1.0e9));
        assert_eq!(s.get_param("dist_pct").unwrap().as_float(), Some(0.5));
        let mut g = hedge_grid::HedgeGrid::default();
        g.set_param_checked("levels_below", ParamValue::Int(1_000_000_000))
            .unwrap();
        let spec_max = g
            .schema()
            .iter()
            .find(|p| p.key == "levels_below")
            .map(|p| match p.kind {
                ParamKind::Int { max, .. } => max,
                _ => unreachable!(),
            });
        assert_eq!(g.get_param("levels_below").unwrap().as_int(), spec_max);
    }
}
