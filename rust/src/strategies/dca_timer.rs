//! Dollar-cost-average timer: market-buy a fixed qty every N seconds.
//!
//! Scripting analogue: a strategy with `input.int("Interval (s)")`,
//! `input.float("Qty")`, and a `time` check that fires `strategy.entry`
//! at regular intervals.

use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "interval_secs",
        label: "Interval",
        kind: ParamKind::Duration { default_secs: 60 },
        help: "Wait this many seconds between orders.",
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
        help: "Order size for each fill.",
    },
    ParamSpec {
        key: "max_orders",
        label: "Max orders",
        kind: ParamKind::Int {
            min: 1,
            max: 10_000,
            default: 10,
            step: 1,
        },
        help: "Stop after this many fills.",
    },
    ParamSpec {
        key: "side_buy",
        label: "Buy side",
        kind: ParamKind::Bool { default: true },
        help: "On = DCA buy.  Off = DCA sell.",
    },
];

pub struct DcaTimer {
    interval_secs: i64,
    qty: f64,
    max_orders: i64,
    side_buy: bool,
    n_done: i64,
    next_at_ms: i64,
}

impl Default for DcaTimer {
    fn default() -> Self {
        Self {
            interval_secs: 60,
            qty: 1.0,
            max_orders: 10,
            side_buy: true,
            n_done: 0,
            next_at_ms: 0,
        }
    }
}

impl Strategy for DcaTimer {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "dca_timer",
            name: "DCA Timer",
            description: "Market order a fixed qty on a fixed interval.",
            timer_secs: 1, // self-gates via next_at_ms
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "interval_secs" => ParamValue::Int(self.interval_secs),
            "qty" => ParamValue::Float(self.qty),
            "max_orders" => ParamValue::Int(self.max_orders),
            "side_buy" => ParamValue::Bool(self.side_buy),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        match key {
            "interval_secs" => {
                self.interval_secs = value.as_int().ok_or("int expected")?.max(1);
            }
            "qty" => {
                self.qty = value.as_float().ok_or("number expected")?.max(0.0);
            }
            "max_orders" => {
                self.max_orders = value.as_int().ok_or("int expected")?.max(1);
            }
            "side_buy" => {
                self.side_buy = value.as_bool().ok_or("bool expected")?;
            }
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }
    fn on_start(&mut self, ctx: &mut Ctx) {
        self.n_done = 0;
        self.next_at_ms = ctx.now_ms; // first order fires immediately
        ctx.note(format!(
            "DCA armed: every {}s × {} qty × {} ({})",
            self.interval_secs,
            self.qty,
            self.max_orders,
            if self.side_buy { "BUY" } else { "SELL" }
        ));
    }
    fn on_timer(&mut self, ctx: &mut Ctx) {
        if self.n_done >= self.max_orders {
            return;
        }
        if ctx.now_ms < self.next_at_ms {
            return;
        }
        if self.side_buy {
            ctx.buy_market(self.qty);
        } else {
            ctx.sell_market(self.qty);
        }
        self.n_done += 1;
        self.next_at_ms = ctx.now_ms + self.interval_secs.saturating_mul(1000);
        if self.n_done >= self.max_orders {
            ctx.note("DCA finished");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::Position;

    fn ctx_at<'a>(now_ms: i64, last: f64, actions: &'a mut Vec<Action>) -> Ctx<'a> {
        Ctx {
            symbol: "BTC",
            now_ms,
            last,
            position: Position::default(),
            equity: 10_000.0,
            actions,
            bars: &[],
        }
    }

    #[test]
    fn dca_fires_at_each_interval_until_max() {
        let mut s = DcaTimer::default();
        s.set_param("interval_secs", ParamValue::Int(10)).unwrap();
        s.set_param("qty", ParamValue::Float(0.5)).unwrap();
        s.set_param("max_orders", ParamValue::Int(3)).unwrap();
        let mut actions = Vec::new();
        // on_start primes timer.
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        actions.clear();

        // First fire at t=0.
        s.on_timer(&mut ctx_at(0, 100.0, &mut actions));
        // Second fire requires +10s.
        s.on_timer(&mut ctx_at(5_000, 100.0, &mut actions));
        s.on_timer(&mut ctx_at(10_000, 100.0, &mut actions));
        s.on_timer(&mut ctx_at(20_000, 100.0, &mut actions));
        // Cap reached, no fourth order.
        s.on_timer(&mut ctx_at(30_000, 100.0, &mut actions));
        s.on_timer(&mut ctx_at(40_000, 100.0, &mut actions));

        let buys = actions
            .iter()
            .filter(|a| matches!(a, Action::BuyMarket { .. }))
            .count();
        assert_eq!(buys, 3);
    }
}
