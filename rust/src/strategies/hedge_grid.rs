//! Grid on a single instrument.
//!
//! A ladder of resting limits around a centre price:
//!
//! ```text
//!   above:  N short LIMIT orders at centre × (1 + spacing × i)  (i = 1..=levels_above)
//!   below:  M long  LIMIT orders at centre × (1 − spacing × i)  (i = 1..=levels_below)
//! ```
//!
//! Every fill gets an opposite take-profit limit at `fill × (1 ± tp)`, and
//! when that take-profit fills the rung is put back at its price, so the
//! grid keeps harvesting while the price oscillates inside it.  The ladder
//! itself caps the inventory: at most `levels × qty` a side.
//!
//! A grid loses on a trend, so it says when it is beaten: when the price
//! leaves `range_pct` from the centre every order is cancelled and the
//! position closed at market; with `recenter` on, a new ladder is laid
//! around the new price (the loss is taken, the grid goes on), else the
//! strategy stays stopped.  `spacing_atr` > 0 sets the spacing to that many
//! ATR(14) of the closed bars instead of `spacing_pct` (the grid then waits
//! for 15 bars before it is laid).  Fees and funding are the ledger's.
//!
//! On `stop`, every open limit on this symbol is cancelled.  Open
//! positions are *not* flattened — the user can hit `CLOSE` on the ticket.

use super::*;

const ATR_PERIOD: usize = 14;

const SCHEMA: &[ParamSpec] = &[
    ParamSpec {
        key: "spacing_pct",
        label: "Spacing %",
        kind: ParamKind::Float {
            min: 0.01,
            max: 50.0,
            default: 0.5,
            step: 0.05,
        },
        help: "Distance between rungs (% of the centre).",
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
        help: "Take-profit placed this far from each fill (%).",
    },
    ParamSpec {
        key: "levels_below",
        label: "Long levels",
        kind: ParamKind::Int {
            min: 0,
            max: 50,
            default: 4,
            step: 1,
        },
        help: "Long limit orders below the centre.",
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
        help: "Short limit orders above the centre.",
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
        help: "Order size of every rung.",
    },
    ParamSpec {
        key: "range_pct",
        label: "Range %",
        kind: ParamKind::Float {
            min: 0.0,
            max: 100.0,
            default: 0.0,
            step: 0.5,
        },
        help: "Beyond this from the centre: cancel all, close at market (0: never).",
    },
    ParamSpec {
        key: "recenter",
        label: "Re-centre",
        kind: ParamKind::Bool { default: true },
        help: "After a range break, lay a new grid around the price.",
    },
    ParamSpec {
        key: "spacing_atr",
        label: "Spacing ATR",
        kind: ParamKind::Float {
            min: 0.0,
            max: 10.0,
            default: 0.0,
            step: 0.1,
        },
        help: "Spacing as a multiple of ATR(14) instead of a % (0: off).",
    },
];

/// One rung of the ladder.
#[derive(Clone, Debug)]
struct Rung {
    side: Side,
    price: f64,
    /// Resting in the book (else filled and waiting for its take-profit).
    open: bool,
    tp: Option<f64>,
}

pub struct HedgeGrid {
    levels_below: i64,
    levels_above: i64,
    qty_per_level: f64,
    spacing_pct: f64,
    take_profit_pct: f64,
    range_pct: f64,
    recenter: bool,
    spacing_atr: f64,
    /// The ladder's centre and spacing (a fraction) once laid.
    centre: f64,
    spacing: f64,
    rungs: Vec<Rung>,
    seeded: bool,
    /// Beaten by a range break and told not to re-centre.
    stopped: bool,
}

impl Default for HedgeGrid {
    fn default() -> Self {
        Self {
            levels_below: 4,
            levels_above: 4,
            qty_per_level: 0.1,
            spacing_pct: 0.5,
            take_profit_pct: 0.5,
            range_pct: 0.0,
            recenter: true,
            spacing_atr: 0.0,
            centre: 0.0,
            spacing: 0.0,
            rungs: Vec::new(),
            seeded: false,
            stopped: false,
        }
    }
}

impl HedgeGrid {
    /// Lay the ladder around `centre` with `spacing` (a fraction).
    fn seed(&mut self, ctx: &mut Ctx, centre: f64, spacing: f64) {
        let qty = self.qty_per_level;
        self.centre = centre;
        self.spacing = spacing;
        self.rungs.clear();
        for i in 1..=self.levels_below {
            let price = centre * (1.0 - spacing * i as f64);
            if price > 0.0 {
                ctx.buy_limit(qty, price);
                self.rungs.push(Rung {
                    side: Side::Buy,
                    price,
                    open: true,
                    tp: None,
                });
            }
        }
        for i in 1..=self.levels_above {
            let price = centre * (1.0 + spacing * i as f64);
            ctx.sell_limit(qty, price);
            self.rungs.push(Rung {
                side: Side::Sell,
                price,
                open: true,
                tp: None,
            });
        }
        ctx.note(format!(
            "hedge_grid laid at {:.6}: {}L + {}S × {} qty, spacing {:.2}%, TP {:.2}%{}",
            centre,
            self.levels_below,
            self.levels_above,
            qty,
            spacing * 100.0,
            self.take_profit_pct,
            if self.range_pct > 0.0 {
                format!(", range ±{:.1}%", self.range_pct)
            } else {
                String::new()
            }
        ));
        self.seeded = true;
    }

    /// The spacing the settings ask for at `last`, if it can be known yet.
    fn spacing_at(&self, ctx: &Ctx, last: f64) -> Option<f64> {
        if self.spacing_atr > 0.0 {
            let atr = indicators::atr(ctx.bars, ATR_PERIOD)?;
            if atr <= 0.0 || last <= 0.0 {
                return None;
            }
            Some((self.spacing_atr * atr / last).max(0.0001))
        } else {
            Some(self.spacing_pct / 100.0)
        }
    }

    fn place_tp(&mut self, ctx: &mut Ctx, fill: &Fill) -> Option<f64> {
        if self.take_profit_pct <= 0.0 || fill.qty <= 0.0 {
            return None;
        }
        let tp = self.take_profit_pct / 100.0;
        let tp_price = match fill.side {
            Side::Buy => fill.price * (1.0 + tp),
            Side::Sell => fill.price * (1.0 - tp),
        };
        match fill.side {
            Side::Buy => ctx.sell_limit(fill.qty, tp_price),
            Side::Sell => ctx.buy_limit(fill.qty, tp_price),
        }
        ctx.note(format!(
            "hedge_grid: TP {} {} @ {:.6} (fill {:.6})",
            match fill.side {
                Side::Buy => "sell",
                Side::Sell => "buy",
            },
            fill.qty,
            tp_price,
            fill.price
        ));
        Some(tp_price)
    }

    /// Cancel everything and close the position at market.
    fn flatten(&mut self, ctx: &mut Ctx) {
        ctx.cancel_all();
        let qty = ctx.position.qty;
        if qty > 1e-12 {
            ctx.sell_market(qty);
        } else if qty < -1e-12 {
            ctx.buy_market(-qty);
        }
        self.rungs.clear();
    }
}

impl Strategy for HedgeGrid {
    fn meta(&self) -> StrategyMeta {
        StrategyMeta {
            id: "hedge_grid",
            name: "Hedge Grid",
            description: "Ladder of long+short limits, TP on each fill, rungs put back; out on a range break.",
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
            "range_pct" => ParamValue::Float(self.range_pct),
            "recenter" => ParamValue::Bool(self.recenter),
            "spacing_atr" => ParamValue::Float(self.spacing_atr),
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
            "range_pct" => self.range_pct = value.as_float().ok_or("number expected")?.max(0.0),
            "recenter" => self.recenter = value.as_bool().ok_or("true/false expected")?,
            "spacing_atr" => self.spacing_atr = value.as_float().ok_or("number expected")?.max(0.0),
            other => return Err(format!("unknown param {other}")),
        }
        Ok(())
    }

    fn on_start(&mut self, ctx: &mut Ctx) {
        self.stopped = false;
        if self.seeded {
            return;
        }
        let last = ctx.last;
        if last <= 0.0 || self.qty_per_level <= 0.0 {
            ctx.note("hedge_grid: no last price yet, retry on next start");
            return;
        }
        match self.spacing_at(ctx, last) {
            Some(spacing) => self.seed(ctx, last, spacing),
            None => ctx.note(format!(
                "hedge_grid: waiting for {} closed bars to size the spacing by ATR",
                ATR_PERIOD + 1
            )),
        }
    }

    fn on_bar(&mut self, ctx: &mut Ctx, b: &Bar) {
        if self.stopped || self.qty_per_level <= 0.0 {
            return;
        }
        let last = if ctx.last > 0.0 { ctx.last } else { b.close };
        if !self.seeded {
            if let Some(spacing) = self.spacing_at(ctx, last) {
                self.seed(ctx, last, spacing);
            }
            return;
        }
        if self.range_pct <= 0.0 || self.centre <= 0.0 {
            return;
        }
        let range = self.range_pct / 100.0;
        if last > self.centre * (1.0 + range) || last < self.centre * (1.0 - range) {
            let qty = ctx.position.qty;
            self.flatten(ctx);
            ctx.note(format!(
                "hedge_grid: price {:.6} left ±{:.1}% of {:.6}; closed {:+} at market",
                last, self.range_pct, self.centre, qty
            ));
            if self.recenter {
                match self.spacing_at(ctx, last) {
                    Some(spacing) => self.seed(ctx, last, spacing),
                    None => self.seeded = false,
                }
            } else {
                self.stopped = true;
                self.seeded = false;
            }
        }
    }

    fn on_fill(&mut self, ctx: &mut Ctx, fill: &Fill) {
        if self.qty_per_level <= 0.0 || fill.price <= 0.0 {
            return;
        }
        let near = |a: f64, b: f64, tol: f64| (a - b).abs() <= b.abs() * tol;
        let tol = (self.spacing / 4.0).max(1e-6);
        // Its take-profit: the rung goes back into the book.
        if let Some(k) = self.rungs.iter().position(|r| {
            !r.open && r.side != fill.side && r.tp.is_some_and(|t| near(fill.price, t, tol))
        }) {
            let r = &mut self.rungs[k];
            r.open = true;
            r.tp = None;
            let (side, price) = (r.side, r.price);
            match side {
                Side::Buy => ctx.buy_limit(self.qty_per_level, price),
                Side::Sell => ctx.sell_limit(self.qty_per_level, price),
            }
            ctx.note(format!("hedge_grid: rung {side:?} @ {price:.6} put back"));
            return;
        }
        // A rung filled: its take-profit.
        let rung = self
            .rungs
            .iter()
            .position(|r| r.open && r.side == fill.side && near(fill.price, r.price, tol));
        let tp = self.place_tp(ctx, fill);
        if let Some(k) = rung {
            self.rungs[k].open = false;
            self.rungs[k].tp = tp;
        }
    }

    fn on_stop(&mut self, ctx: &mut Ctx) {
        ctx.cancel_all();
        ctx.note("hedge_grid: cancelled all open orders");
        self.rungs.clear();
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
                fee: 0.0,
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

    fn fill(side: Side, qty: f64, price: f64) -> Fill {
        Fill {
            side,
            qty,
            price,
            time_ms: 0,
            order_id: 0,
            fee: 0.0,
        }
    }

    fn limits(actions: &[Action]) -> Vec<(Side, f64)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::BuyLimit { limit, .. } => Some((Side::Buy, *limit)),
                Action::SellLimit { limit, .. } => Some((Side::Sell, *limit)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_take_profit_fill_puts_the_rung_back() {
        let mut s = HedgeGrid::default();
        s.set_param("spacing_pct", ParamValue::Float(1.0)).unwrap();
        s.set_param("take_profit_pct", ParamValue::Float(1.0))
            .unwrap();
        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        // The first long rung fills at 99: a TP sell at 99.99.
        actions.clear();
        s.on_fill(
            &mut ctx_at(1, 99.0, &mut actions),
            &fill(Side::Buy, 0.1, 99.0),
        );
        assert_eq!(limits(&actions), vec![(Side::Sell, 99.0 * 1.01)]);
        assert!(!s.rungs.iter().find(|r| r.price == 99.0).unwrap().open);
        // The TP fills: the rung is resting at 99 again, no second TP.
        actions.clear();
        s.on_fill(
            &mut ctx_at(2, 100.0, &mut actions),
            &fill(Side::Sell, 0.1, 99.0 * 1.01),
        );
        assert_eq!(limits(&actions), vec![(Side::Buy, 99.0)]);
        assert!(s.rungs.iter().find(|r| r.price == 99.0).unwrap().open);
    }

    #[test]
    fn a_range_break_closes_at_market_and_recentres() {
        let mut s = HedgeGrid::default();
        s.set_param("spacing_pct", ParamValue::Float(1.0)).unwrap();
        s.set_param("range_pct", ParamValue::Float(5.0)).unwrap();
        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        actions.clear();
        let bar = |c: f64| Bar {
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 1.0,
            time_ms: 0,
        };
        // Inside the range: nothing happens.
        s.on_bar(&mut ctx_at(1, 104.0, &mut actions), &bar(104.0));
        assert!(actions.is_empty());
        // Beyond it, short 0.3 from the upper rungs: cancel, buy back, new ladder at 106.
        let mut ctx = ctx_at(2, 106.0, &mut actions);
        ctx.position.qty = -0.3;
        s.on_bar(&mut ctx, &bar(106.0));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CancelAll { .. })));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::BuyMarket { qty, .. } if (qty - 0.3).abs() < 1e-12)));
        let l = limits(&actions);
        assert_eq!(l.len(), 8);
        assert!((l[0].1 - 106.0 * 0.99).abs() < 1e-9);
        assert!((s.centre - 106.0).abs() < 1e-12);
        // Without re-centre the grid stays stopped.
        let mut s = HedgeGrid::default();
        s.set_param("range_pct", ParamValue::Float(5.0)).unwrap();
        s.set_param("recenter", ParamValue::Bool(false)).unwrap();
        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        actions.clear();
        s.on_bar(&mut ctx_at(1, 90.0, &mut actions), &bar(90.0));
        assert!(limits(&actions).is_empty());
        assert!(s.stopped);
        actions.clear();
        s.on_bar(&mut ctx_at(2, 90.0, &mut actions), &bar(90.0));
        assert!(actions.is_empty());
    }

    #[test]
    fn atr_spacing_waits_for_bars() {
        let mut s = HedgeGrid::default();
        s.set_param("spacing_atr", ParamValue::Float(2.0)).unwrap();
        let mut actions = Vec::new();
        s.on_start(&mut ctx_at(0, 100.0, &mut actions));
        assert!(limits(&actions).is_empty());
        assert!(!s.seeded);
        let bars: Vec<Bar> = (0..20)
            .map(|i| Bar {
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
                time_ms: i,
            })
            .collect();
        actions.clear();
        let mut ctx = ctx_at(20, 100.0, &mut actions);
        ctx.bars = &bars;
        s.on_bar(&mut ctx, &bars[19]);
        // ATR 2 x 2 = 4 % spacing.
        let l = limits(&actions);
        assert_eq!(l.len(), 8);
        assert!((l[0].1 - 96.0).abs() < 1e-9, "{l:?}");
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
