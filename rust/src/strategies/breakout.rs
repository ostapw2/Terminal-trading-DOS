//! Channel breakout (Donchian): buy a close above the highest high of the
//! last N bars, exit on a close below the lowest low of the last M bars.

use super::indicators::{prior_high, prior_low};
use super::*;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "entry_bars",
        label: "Entry channel",
        kind: ParamKind::Int {
            min: 2,
            max: 500,
            default: 20,
            step: 1,
        },
        help: "Enter on a close beyond the high/low of this many previous bars.",
    },
    ParamSpec {
        key: "exit_bars",
        label: "Exit channel",
        kind: ParamKind::Int {
            min: 1,
            max: 500,
            default: 10,
            step: 1,
        },
        help: "Leave on a close beyond the opposite extreme of this many bars.",
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

pub struct Breakout {
    entry_bars: i64,
    exit_bars: i64,
    qty: f64,
    allow_short: bool,
}

impl Default for Breakout {
    fn default() -> Self {
        Self {
            entry_bars: 20,
            exit_bars: 10,
            qty: 1.0,
            allow_short: false,
        }
    }
}

impl Strategy for Breakout {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "breakout",
            name: "Breakout",
            description: "Trend: buy a close above the N-bar high, exit below the M-bar low.",
            timer_secs: 0,
        }
    }
    fn schema(&self) -> &[ParamSpec] {
        SCHEMA
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        Some(match key {
            "entry_bars" => ParamValue::Int(self.entry_bars),
            "exit_bars" => ParamValue::Int(self.exit_bars),
            "qty" => ParamValue::Float(self.qty),
            "allow_short" => ParamValue::Bool(self.allow_short),
            _ => return None,
        })
    }
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        match key {
            "entry_bars" => self.entry_bars = value.as_int().ok_or("int expected")?.max(2),
            "exit_bars" => self.exit_bars = value.as_int().ok_or("int expected")?.max(1),
            "qty" => self.qty = value.as_float().ok_or("number expected")?.max(0.0),
            "allow_short" => self.allow_short = value.as_bool().ok_or("bool expected")?,
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }
    fn on_start(&mut self, ctx: &mut Ctx) {
        ctx.note(format!(
            "Breakout armed: enter {}-bar channel, exit {}-bar channel",
            self.entry_bars, self.exit_bars
        ));
    }
    fn on_bar(&mut self, ctx: &mut Ctx, b: &Bar) {
        if self.qty <= 0.0 {
            return;
        }
        let (n, m) = (self.entry_bars as usize, self.exit_bars as usize);
        let pos = ctx.position.qty;
        let close = b.close;
        if pos == 0.0 {
            if prior_high(ctx.bars, n).is_some_and(|h| close > h) {
                ctx.buy_market(self.qty);
            } else if self.allow_short && prior_low(ctx.bars, n).is_some_and(|l| close < l) {
                ctx.sell_market(self.qty);
            }
        } else if pos > 0.0 {
            if prior_low(ctx.bars, m).is_some_and(|l| close < l) {
                ctx.sell_market(pos);
            }
        } else if prior_high(ctx.bars, m).is_some_and(|h| close > h) {
            ctx.buy_market(-pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::indicators::tests::bars;
    use super::*;

    fn act(s: &mut Breakout, closes: &[f64], pos: f64) -> Vec<Action> {
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

    fn small() -> Breakout {
        Breakout {
            entry_bars: 4,
            exit_bars: 2,
            qty: 1.0,
            allow_short: false,
        }
    }

    #[test]
    fn buys_a_close_above_the_channel_and_exits_below_the_short_one() {
        let mut s = small();
        // Prior 4 highs max = 12+1 = 13 -> close 15 breaks out.
        assert!(matches!(
            act(&mut s, &[10.0, 11.0, 12.0, 11.0, 15.0], 0.0).as_slice(),
            [Action::BuyMarket { .. }]
        ));
        // Inside the channel: nothing.
        assert!(act(&mut s, &[10.0, 11.0, 12.0, 11.0, 12.5], 0.0).is_empty());
        // Long; prior 2 lows = min(11,12)-1 = 10 -> close 9 exits.
        let a = act(&mut s, &[10.0, 11.0, 12.0, 11.0, 9.0], 1.0);
        assert!(
            matches!(a.as_slice(), [Action::SellMarket { qty, .. }] if *qty == 1.0),
            "{a:?}"
        );
        // Long, close still above the exit channel: hold.
        assert!(act(&mut s, &[10.0, 11.0, 12.0, 11.0, 11.5], 1.0).is_empty());
    }

    #[test]
    fn a_spike_bar_does_not_widen_its_own_channel() {
        let mut s = small();
        // The last bar's own high (51) must not count, or it could never break out.
        assert!(matches!(
            act(&mut s, &[10.0, 10.0, 10.0, 10.0, 50.0], 0.0).as_slice(),
            [Action::BuyMarket { .. }]
        ));
    }

    #[test]
    fn short_side_and_waiting_for_history() {
        let mut s = Breakout {
            allow_short: true,
            ..small()
        };
        assert!(matches!(
            act(&mut s, &[10.0, 11.0, 12.0, 11.0, 5.0], 0.0).as_slice(),
            [Action::SellMarket { .. }]
        ));
        let a = act(&mut s, &[10.0, 9.0, 8.0, 9.0, 12.0], -2.0);
        assert!(
            matches!(a.as_slice(), [Action::BuyMarket { qty, .. }] if *qty == 2.0),
            "{a:?}"
        );
        let mut s = small();
        assert!(
            act(&mut s, &[10.0, 11.0, 15.0], 0.0).is_empty(),
            "needs entry_bars + 1 bars"
        );
        let mut s = Breakout {
            qty: 0.0,
            ..small()
        };
        assert!(act(&mut s, &[10.0, 11.0, 12.0, 11.0, 15.0], 0.0).is_empty());
    }
}
