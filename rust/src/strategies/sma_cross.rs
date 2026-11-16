//! SMA crossover: long when the fast average crosses above the slow one, flat
//! (or short, if enabled) when it crosses back below.  Acts once per closed
//! bar, so it needs the bar feed (backtest, forward test, or live bars).

use super::indicators::sma;
use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "fast",
        label: "Fast SMA",
        kind: ParamKind::Int {
            min: 2,
            max: 500,
            default: 10,
            step: 1,
        },
        help: "Bars in the fast average (must be below Slow).",
    },
    ParamSpec {
        key: "slow",
        label: "Slow SMA",
        kind: ParamKind::Int {
            min: 3,
            max: 1000,
            default: 30,
            step: 1,
        },
        help: "Bars in the slow average.",
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
        help: "Off = long only: a down-cross just closes the long.",
    },
];

pub struct SmaCross {
    fast: i64,
    slow: i64,
    qty: f64,
    allow_short: bool,
}

impl Default for SmaCross {
    fn default() -> Self {
        Self {
            fast: 10,
            slow: 30,
            qty: 1.0,
            allow_short: false,
        }
    }
}

impl Strategy for SmaCross {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "sma_cross",
            name: "SMA Cross",
            description:
                "Trend: buy when the fast SMA crosses above the slow one, exit on the reverse.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "fast" => ParamValue::Int(self.fast),
            "slow" => ParamValue::Int(self.slow),
            "qty" => ParamValue::Float(self.qty),
            "allow_short" => ParamValue::Bool(self.allow_short),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        match key {
            "fast" => self.fast = value.as_int().ok_or("int expected")?.max(2),
            "slow" => self.slow = value.as_int().ok_or("int expected")?.max(3),
            "qty" => self.qty = value.as_float().ok_or("number expected")?.max(0.0),
            "allow_short" => self.allow_short = value.as_bool().ok_or("bool expected")?,
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }
    fn on_start(&mut self, ctx: &mut Ctx) {
        if self.fast >= self.slow {
            ctx.note(format!(
                "SMA Cross idle: fast {} must be below slow {}",
                self.fast, self.slow
            ));
        } else {
            ctx.note(format!(
                "SMA Cross armed: {}/{} bars, qty {}",
                self.fast, self.slow, self.qty
            ));
        }
    }
    fn on_bar(&mut self, ctx: &mut Ctx, _b: &Bar) {
        let (fast, slow) = (self.fast as usize, self.slow as usize);
        if fast >= slow || ctx.bars.len() < slow + 1 || self.qty <= 0.0 {
            return;
        }
        let prev = &ctx.bars[..ctx.bars.len() - 1];
        let (Some(f1), Some(s1), Some(f0), Some(s0)) = (
            sma(ctx.bars, fast),
            sma(ctx.bars, slow),
            sma(prev, fast),
            sma(prev, slow),
        ) else {
            return;
        };
        let pos = ctx.position.qty;
        if f0 <= s0 && f1 > s1 {
            // Up-cross: be long.  Cover a short on the way.
            if pos < 0.0 {
                ctx.buy_market(-pos + self.qty);
            } else if pos == 0.0 {
                ctx.buy_market(self.qty);
            }
        } else if f0 >= s0 && f1 < s1 {
            // Down-cross: leave the long; optionally go short.
            if pos > 0.0 {
                ctx.sell_market(if self.allow_short {
                    pos + self.qty
                } else {
                    pos
                });
            } else if pos == 0.0 && self.allow_short {
                ctx.sell_market(self.qty);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::indicators::tests::bars;
    use super::*;

    fn run_bar(s: &mut SmaCross, closes: &[f64], pos: f64) -> Vec<Action> {
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

    fn small() -> SmaCross {
        SmaCross {
            fast: 2,
            slow: 4,
            qty: 1.0,
            allow_short: false,
        }
    }

    #[test]
    fn up_cross_buys_down_cross_closes_and_nothing_else_happens() {
        let mut s = small();
        // fast(2) vs slow(4): before 9.5 <= 9.75, after 11 > 10.5 -> up-cross.
        let a = run_bar(&mut s, &[10.0, 10.0, 10.0, 9.0, 13.0], 0.0);
        assert!(
            matches!(a.as_slice(), [Action::BuyMarket { qty, .. }] if *qty == 1.0),
            "{a:?}"
        );
        // Already long: an up-cross does nothing more.
        assert!(run_bar(&mut s, &[10.0, 10.0, 10.0, 9.0, 13.0], 1.0).is_empty());
        // Down-cross while long closes exactly the position.
        let a = run_bar(&mut s, &[10.0, 10.0, 12.0, 14.0, 5.0], 1.0);
        assert!(
            matches!(a.as_slice(), [Action::SellMarket { qty, .. }] if *qty == 1.0),
            "{a:?}"
        );
        // No cross, no action.
        assert!(run_bar(&mut s, &[10.0, 11.0, 12.0, 13.0, 14.0], 0.0).is_empty());
        // Down-cross while flat and long-only: nothing.
        assert!(run_bar(&mut s, &[10.0, 10.0, 12.0, 14.0, 5.0], 0.0).is_empty());
    }

    #[test]
    fn allow_short_flips_both_ways() {
        let mut s = SmaCross {
            allow_short: true,
            ..small()
        };
        let a = run_bar(&mut s, &[10.0, 10.0, 12.0, 14.0, 5.0], 1.0);
        assert!(
            matches!(a.as_slice(), [Action::SellMarket { qty, .. }] if *qty == 2.0),
            "close 1 + open 1: {a:?}"
        );
        let a = run_bar(&mut s, &[10.0, 10.0, 12.0, 14.0, 5.0], 0.0);
        assert!(matches!(a.as_slice(), [Action::SellMarket { qty, .. }] if *qty == 1.0));
        let a = run_bar(&mut s, &[10.0, 10.0, 10.0, 9.0, 13.0], -1.0);
        assert!(
            matches!(a.as_slice(), [Action::BuyMarket { qty, .. }] if *qty == 2.0),
            "cover 1 + open 1: {a:?}"
        );
    }

    #[test]
    fn it_waits_for_history_and_ignores_bad_settings() {
        let mut s = small();
        assert!(
            run_bar(&mut s, &[10.0, 9.0, 13.0], 0.0).is_empty(),
            "fewer than slow+1 bars"
        );
        let mut bad = SmaCross {
            fast: 5,
            slow: 5,
            ..small()
        };
        assert!(
            run_bar(&mut bad, &[10.0, 10.0, 10.0, 9.0, 13.0, 14.0], 0.0).is_empty(),
            "fast >= slow is idle"
        );
        let mut zero = SmaCross {
            qty: 0.0,
            ..small()
        };
        assert!(run_bar(&mut zero, &[10.0, 10.0, 10.0, 9.0, 13.0], 0.0).is_empty());
    }

    #[test]
    fn params_clamp_and_report() {
        let mut s = SmaCross::default();
        s.set_param_checked("fast", ParamValue::Int(-5)).unwrap();
        s.set_param_checked("slow", ParamValue::Int(99_999))
            .unwrap();
        assert_eq!(s.get_param("fast").and_then(|v| v.as_int()), Some(2));
        assert_eq!(s.get_param("slow").and_then(|v| v.as_int()), Some(1000));
        assert!(s.set_param("nope", ParamValue::Int(1)).is_err());
        assert!(s.set_param("allow_short", ParamValue::Int(1)).is_err());
    }
}
