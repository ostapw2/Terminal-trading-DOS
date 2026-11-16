//! RSI mean reversion: buy when RSI falls below the oversold level, leave
//! when it recovers to the exit level.  Optionally the mirror image short.

use super::indicators::rsi;
use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "period",
        label: "RSI period",
        kind: ParamKind::Int {
            min: 2,
            max: 200,
            default: 14,
            step: 1,
        },
        help: "Bars in Wilder's RSI.",
    },
    ParamSpec {
        key: "oversold",
        label: "Oversold",
        kind: ParamKind::Float {
            min: 1.0,
            max: 49.0,
            default: 30.0,
            step: 1.0,
        },
        help: "Buy when RSI drops below this.",
    },
    ParamSpec {
        key: "overbought",
        label: "Overbought",
        kind: ParamKind::Float {
            min: 51.0,
            max: 99.0,
            default: 70.0,
            step: 1.0,
        },
        help: "Short when RSI rises above this (needs Allow short).",
    },
    ParamSpec {
        key: "exit_level",
        label: "Exit level",
        kind: ParamKind::Float {
            min: 40.0,
            max: 60.0,
            default: 50.0,
            step: 1.0,
        },
        help: "Close the position when RSI returns to this level.",
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
        help: "Position size when entering.",
    },
    ParamSpec {
        key: "allow_short",
        label: "Allow short",
        kind: ParamKind::Bool { default: false },
        help: "Off = long only.",
    },
];

pub struct RsiReversion {
    period: i64,
    oversold: f64,
    overbought: f64,
    exit_level: f64,
    qty: f64,
    allow_short: bool,
}

impl Default for RsiReversion {
    fn default() -> Self {
        Self {
            period: 14,
            oversold: 30.0,
            overbought: 70.0,
            exit_level: 50.0,
            qty: 1.0,
            allow_short: false,
        }
    }
}

impl Strategy for RsiReversion {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "rsi_reversion",
            name: "RSI Reversion",
            description: "Mean reversion: buy oversold RSI, exit when it recovers.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "period" => ParamValue::Int(self.period),
            "oversold" => ParamValue::Float(self.oversold),
            "overbought" => ParamValue::Float(self.overbought),
            "exit_level" => ParamValue::Float(self.exit_level),
            "qty" => ParamValue::Float(self.qty),
            "allow_short" => ParamValue::Bool(self.allow_short),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        let num = || value.as_float().ok_or("number expected");
        match key {
            "period" => self.period = value.as_int().ok_or("int expected")?.max(2),
            "oversold" => self.oversold = num()?,
            "overbought" => self.overbought = num()?,
            "exit_level" => self.exit_level = num()?,
            "qty" => self.qty = num()?.max(0.0),
            "allow_short" => self.allow_short = value.as_bool().ok_or("bool expected")?,
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }
    fn on_start(&mut self, ctx: &mut Ctx) {
        ctx.note(format!(
            "RSI({}) reversion armed: buy < {}, exit at {}",
            self.period, self.oversold, self.exit_level
        ));
    }
    fn on_bar(&mut self, ctx: &mut Ctx, _b: &Bar) {
        let Some(r) = rsi(ctx.bars, self.period as usize) else {
            return;
        };
        if self.qty <= 0.0 {
            return;
        }
        let pos = ctx.position.qty;
        if pos == 0.0 {
            if r < self.oversold {
                ctx.buy_market(self.qty);
            } else if self.allow_short && r > self.overbought {
                ctx.sell_market(self.qty);
            }
        } else if pos > 0.0 && r >= self.exit_level {
            ctx.sell_market(pos);
        } else if pos < 0.0 && r <= self.exit_level {
            ctx.buy_market(-pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::indicators::tests::bars;
    use super::*;

    fn act(s: &mut RsiReversion, closes: &[f64], pos: f64) -> Vec<Action> {
        let b = bars(closes);
        let mut actions = Vec::new();
        let mut ctx = Ctx {
            symbol: "T",
            now_ms: 0,
            last: *closes.last().unwrap(),
            position: Position {
                qty: pos,
                avg: 100.0,
                realized: 0.0,
            },
            equity: 10_000.0,
            actions: &mut actions,
            bars: &b,
        };
        let last = b.last().unwrap().clone();
        s.on_bar(&mut ctx, &last);
        actions
    }

    fn falling() -> Vec<f64> {
        (0..30).map(|i| 100.0 - i as f64).collect() // RSI ~ 0
    }
    fn rising() -> Vec<f64> {
        (0..30).map(|i| 100.0 + i as f64).collect() // RSI = 100
    }

    #[test]
    fn buys_oversold_exits_on_recovery_and_stays_out_in_between() {
        let mut s = RsiReversion::default();
        assert!(matches!(
            act(&mut s, &falling(), 0.0).as_slice(),
            [Action::BuyMarket { .. }]
        ));
        // Long and RSI recovered: closes exactly the position.
        let a = act(&mut s, &rising(), 2.0);
        assert!(
            matches!(a.as_slice(), [Action::SellMarket { qty, .. }] if *qty == 2.0),
            "{a:?}"
        );
        // Long but still oversold: hold.
        assert!(act(&mut s, &falling(), 2.0).is_empty());
        // Flat and RSI = 100 with long-only: nothing.
        assert!(act(&mut s, &rising(), 0.0).is_empty());
    }

    #[test]
    fn short_side_mirrors_it() {
        let mut s = RsiReversion {
            allow_short: true,
            ..RsiReversion::default()
        };
        assert!(matches!(
            act(&mut s, &rising(), 0.0).as_slice(),
            [Action::SellMarket { .. }]
        ));
        let a = act(&mut s, &falling(), -3.0);
        assert!(
            matches!(a.as_slice(), [Action::BuyMarket { qty, .. }] if *qty == 3.0),
            "{a:?}"
        );
        assert!(
            act(&mut s, &rising(), -3.0).is_empty(),
            "still overbought: hold the short"
        );
    }

    #[test]
    fn needs_history_and_clamps_parameters() {
        let mut s = RsiReversion::default();
        assert!(
            act(&mut s, &falling()[..10], 0.0).is_empty(),
            "fewer than period + 1 bars"
        );
        s.set_param_checked("oversold", ParamValue::Float(500.0))
            .unwrap();
        s.set_param_checked("exit_level", ParamValue::Float(0.0))
            .unwrap();
        assert_eq!(
            s.get_param("oversold").and_then(|v| v.as_float()),
            Some(49.0)
        );
        assert_eq!(
            s.get_param("exit_level").and_then(|v| v.as_float()),
            Some(40.0)
        );
        assert!(s.set_param("period", ParamValue::Bool(true)).is_err());
    }
}
