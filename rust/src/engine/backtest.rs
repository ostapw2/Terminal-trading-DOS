//! Offline backtest: replay stored candles through the same [`Runtime`],
//! risk gate and paper broker that run live, with no network and no clock.
//!
//! ## Fill model (read before trusting a number)
//!
//! - Each candle becomes four ticks: open, then low and high, then close.  A
//!   bullish candle (close >= open) visits the low first, a bearish one the
//!   high first.  The real intra-candle path is unknown; this is the usual
//!   assumption and it decides which of two resting orders fills first.
//! - A market order fills at the last tick it saw, with **no slippage**.  An
//!   order emitted from `on_bar` therefore fills at that candle's close, which
//!   is optimistic against a real next-open fill.
//! - A limit order fills at its limit as soon as a tick touches it (broker rule); one that
//!   is already marketable when placed fills at the last price instead.
//! - Fees: the ledger charges `fee_bps` on every fill of a spot symbol.  A
//!   perpetual (`BTCUSDT_PERP`) pays the ledger's futures rates instead
//!   (maker on a limit that rested, taker on a market or marketable one) and
//!   `funding_rate` of its notional at every 00 / 08 / 16 UTC settlement it
//!   holds through (a long pays a positive rate): one flat rate stands in for
//!   the history, so the result is a typical cost, not the dated one.
//! - `on_timer` is dispatched once per candle (at its open), so a strategy
//!   with `timer_secs` below the candle length runs slower than it does live.
//!
//! Candles with `time_ms == 0` (pre-timestamp cache) cannot be replayed.

use crate::markets::Ohlc;
use crate::strategies::{Bar, Fill, Side, Strategy};

use super::broker::Broker;
use super::runtime::Runtime;

#[derive(Clone, Debug)]
pub struct BacktestConfig {
    pub starting_cash: f64,
    /// Fee per fill in basis points of notional (10 = 0.10 %), spot only.
    pub fee_bps: f64,
    /// Funding per settlement on a perpetual, a fraction (0.0001 = 1 bp).
    pub funding_rate: f64,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            starting_cash: 10_000.0,
            fee_bps: 10.0,
            funding_rate: 0.0001,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BacktestResult {
    pub strategy_id: String,
    pub symbol: String,
    pub bars: usize,
    pub first_ms: i64,
    pub last_ms: i64,
    pub starting_cash: f64,
    pub final_equity: f64,
    /// Final equity minus starting cash minus fees.
    pub net_pnl: f64,
    pub return_pct: f64,
    pub fees: f64,
    /// Net cash funding moved on a perpetual (negative: paid).
    pub funding: f64,
    /// Largest peak-to-trough fall of the equity curve, percent of the peak.
    pub max_drawdown_pct: f64,
    pub fills: Vec<Fill>,
    /// Round trips: from flat (or a flip) back to flat (or the next flip).
    pub trades: usize,
    pub wins: usize,
    /// Equity (net of fees) sampled at each candle close.
    pub equity_curve: Vec<f64>,
    /// Why the risk gate stopped the run early, if it did.
    pub halted: Option<String>,
    /// Position still open at the end, marked to the last close.
    pub open_qty: f64,
}

impl BacktestResult {
    pub fn win_rate_pct(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 * 100.0 / self.trades as f64
        }
    }
}

fn tick_path(b: &Ohlc) -> [f64; 4] {
    if b.close >= b.open {
        [b.open, b.low, b.high, b.close]
    } else {
        [b.open, b.high, b.low, b.close]
    }
}

/// Replay `bars` (oldest first) through `strategy`.  The strategy keeps the
/// parameters it was given; build it with `registry::instantiate` and
/// `set_param_checked` before calling.
pub fn run(
    strategy: Box<dyn Strategy>,
    symbol: &str,
    bars: &[Ohlc],
    cfg: &BacktestConfig,
) -> Result<BacktestResult, String> {
    let series: Vec<&Ohlc> = bars
        .iter()
        .filter(|b| !b.is_gap && b.close > 0.0 && b.open > 0.0)
        .collect();
    if series.len() < 2 {
        return Err("need at least 2 candles".into());
    }
    if series.iter().any(|b| b.time_ms <= 0) {
        return Err("candles without timestamps (old cache): reload the symbol".into());
    }
    if series.windows(2).any(|w| w[1].time_ms <= w[0].time_ms) {
        return Err("candles are not in strictly increasing time order".into());
    }

    let strategy_id = strategy.meta().id.to_string();
    let mut rt = Runtime::new(cfg.starting_cash);
    let perp = crate::futures::is_perp(symbol);
    if perp {
        rt.funding_rates.insert(
            crate::futures::exchange_symbol(symbol).to_string(),
            cfg.funding_rate,
        );
    } else {
        // The broker charges the fee on every fill; equity is net of it.
        rt.broker.set_fee_bps(cfg.fee_bps);
    }
    let idx = rt.add_slot(
        symbol.to_string(),
        "BACKTEST".into(),
        strategy_id.clone(),
        strategy,
    );

    let mut res = BacktestResult {
        strategy_id,
        symbol: symbol.to_string(),
        bars: series.len(),
        first_ms: series[0].time_ms,
        last_ms: series[series.len() - 1].time_ms,
        starting_cash: cfg.starting_cash,
        ..Default::default()
    };

    let mut fees = 0.0;
    let mut peak = cfg.starting_cash;
    let mut max_dd = 0.0_f64;

    // `drain` empties the runtime's capped fill log into our own record.
    let drain = |rt: &mut Runtime, fees: &mut f64, out: &mut Vec<Fill>| {
        for (_, _, f) in rt.fill_log.drain(..) {
            *fees += f.fee;
            out.push(f);
        }
    };

    let first = series[0];
    rt.on_price(symbol, first.open, first.time_ms);
    rt.start(idx, first.time_ms)?;
    drain(&mut rt, &mut fees, &mut res.fills);

    for b in &series {
        let t0 = b.time_ms;
        for (k, p) in tick_path(b).into_iter().enumerate() {
            rt.on_price(symbol, p, t0 + k as i64);
            if k == 0 {
                rt.dispatch_timers(t0);
            }
            drain(&mut rt, &mut fees, &mut res.fills);
        }
        let bar = Bar {
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
            time_ms: t0,
        };
        rt.on_bar(symbol, bar, t0 + 3);
        drain(&mut rt, &mut fees, &mut res.fills);

        let eq = rt.equity();
        peak = peak.max(eq);
        if peak > 0.0 {
            max_dd = max_dd.max((peak - eq) / peak * 100.0);
        }
        res.equity_curve.push(eq);

        if rt.limits.kill_switch {
            res.halted = Some(
                rt.risk_state
                    .last_violation
                    .clone()
                    .unwrap_or_else(|| "kill switch".into()),
            );
            break;
        }
    }

    let end_ms = res.last_ms + 4;
    rt.stop(idx, end_ms)?;
    drain(&mut rt, &mut fees, &mut res.fills);

    res.open_qty = rt.broker.position(symbol).qty;
    res.fees = fees;
    // Every price tick settles the funding that is due (`Runtime::on_price`).
    res.funding = rt.broker.funding_total();
    res.final_equity = rt.equity();
    res.net_pnl = res.final_equity - cfg.starting_cash;
    res.return_pct = if cfg.starting_cash > 0.0 {
        res.net_pnl / cfg.starting_cash * 100.0
    } else {
        0.0
    };
    res.max_drawdown_pct = max_dd;
    let (trades, wins) = count_round_trips(&res.fills);
    res.trades = trades;
    res.wins = wins;
    Ok(res)
}

/// Count closed round trips and winners from the fill sequence (single symbol).
/// A trade ends when the position returns to flat or flips; its PnL is the sum
/// of realised PnL since it opened (fees excluded).
pub fn count_round_trips(fills: &[Fill]) -> (usize, usize) {
    let (mut qty, mut avg, mut pnl) = (0.0_f64, 0.0_f64, 0.0_f64);
    let (mut trades, mut wins) = (0, 0);
    for f in fills {
        let signed = match f.side {
            Side::Buy => f.qty,
            Side::Sell => -f.qty,
        };
        if qty.abs() < 1e-12 {
            qty = signed;
            avg = f.price;
            pnl = 0.0;
            continue;
        }
        if qty.signum() == signed.signum() {
            avg = (avg * qty.abs() + f.price * f.qty) / (qty.abs() + f.qty);
            qty += signed;
            continue;
        }
        let close = f.qty.min(qty.abs());
        pnl += (f.price - avg) * close * qty.signum();
        let new_qty = qty + signed;
        if new_qty.abs() < 1e-12 {
            trades += 1;
            if pnl > 0.0 {
                wins += 1;
            }
            qty = 0.0;
            avg = 0.0;
            pnl = 0.0;
        } else if new_qty.signum() != qty.signum() {
            trades += 1;
            if pnl > 0.0 {
                wins += 1;
            }
            qty = new_qty;
            avg = f.price;
            pnl = 0.0;
        } else {
            qty = new_qty;
        }
    }
    (trades, wins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::{Ctx, ParamSpec, ParamValue, StrategyMeta};

    /// Buys 1 unit on start and holds.
    struct BuyHold;
    impl Strategy for BuyHold {
        fn meta(&self) -> StrategyMeta {
            StrategyMeta {
                id: "buy_hold_test",
                name: "Buy&Hold",
                description: "test",
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
        fn on_start(&mut self, ctx: &mut Ctx) {
            ctx.buy_market(1.0);
        }
    }

    fn candle(i: i64, o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            date: String::new(),
            time_ms: 1_700_000_000_000 + i * 60_000,
            is_gap: false,
        }
    }

    fn cfg(fee_bps: f64) -> BacktestConfig {
        BacktestConfig {
            starting_cash: 1_000.0,
            fee_bps,
            ..Default::default()
        }
    }

    #[test]
    fn buy_and_hold_matches_price_change() {
        let bars = [
            candle(0, 100.0, 101.0, 99.0, 100.0),
            candle(1, 100.0, 111.0, 99.0, 110.0),
            candle(2, 110.0, 121.0, 109.0, 120.0),
        ];
        let r = run(Box::new(BuyHold), "T", &bars, &cfg(0.0)).unwrap();
        // Bought at the first open (100), marked at the last close (120).
        assert!((r.net_pnl - 20.0).abs() < 1e-9, "pnl {}", r.net_pnl);
        assert_eq!(r.fills.len(), 1);
        assert!((r.open_qty - 1.0).abs() < 1e-12);
        assert_eq!(r.equity_curve.len(), 3);
        assert!(r.halted.is_none());
    }

    #[test]
    fn a_perpetual_pays_funding_at_every_settlement() {
        // Three days of hourly candles from a settlement: the first tick only
        // starts the clock, then 8 settlements are held long.
        let bars: Vec<Ohlc> = (0..72)
            .map(|i| {
                let mut c = candle(0, 100.0, 100.0, 100.0, 100.0);
                c.time_ms = 1_700_006_400_000 + i * 3_600_000;
                c
            })
            .collect();
        let cfg = BacktestConfig {
            starting_cash: 1_000.0,
            fee_bps: 0.0,
            funding_rate: 0.001,
        };
        let r = run(Box::new(BuyHold), "T_PERP", &bars, &cfg).unwrap();
        // 1 unit at 100, 10 bps a settlement: -0.1 each.
        assert!((r.funding - (-0.8)).abs() < 1e-9, "funding {}", r.funding);
        // The ledger's futures taker fee on the one market fill, nothing else.
        let fee = 100.0 * crate::futures::FEE_TAKER_BPS / 10_000.0;
        assert!(
            (r.net_pnl - (r.funding - fee)).abs() < 1e-6,
            "pnl {} funding {}",
            r.net_pnl,
            r.funding
        );
        let spot = run(Box::new(BuyHold), "T", &bars, &cfg).unwrap();
        assert_eq!(spot.funding, 0.0);
    }

    #[test]
    fn fees_are_charged_per_fill() {
        let bars = [
            candle(0, 100.0, 100.0, 100.0, 100.0),
            candle(1, 100.0, 100.0, 100.0, 100.0),
        ];
        // 100 bps of a 100 notional = 1.0.
        let r = run(Box::new(BuyHold), "T", &bars, &cfg(100.0)).unwrap();
        assert!((r.fees - 1.0).abs() < 1e-9);
        assert!((r.net_pnl + 1.0).abs() < 1e-9);
    }

    #[test]
    fn drawdown_follows_the_curve() {
        let bars = [
            candle(0, 100.0, 100.0, 100.0, 100.0),
            candle(1, 100.0, 150.0, 100.0, 150.0),
            candle(2, 150.0, 150.0, 75.0, 75.0),
        ];
        let r = run(Box::new(BuyHold), "T", &bars, &cfg(0.0)).unwrap();
        // Equity 1000 -> 1050 (peak) -> 975: (1050 - 975) / 1050.
        assert!((r.max_drawdown_pct - 75.0 / 1050.0 * 100.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_unreplayable_input() {
        let one = [candle(0, 1.0, 1.0, 1.0, 1.0)];
        assert!(run(Box::new(BuyHold), "T", &one, &cfg(0.0)).is_err());
        let mut no_time = [candle(0, 1.0, 1.0, 1.0, 1.0), candle(1, 1.0, 1.0, 1.0, 1.0)];
        no_time[0].time_ms = 0;
        assert!(run(Box::new(BuyHold), "T", &no_time, &cfg(0.0)).is_err());
        let backwards = [candle(1, 1.0, 1.0, 1.0, 1.0), candle(0, 1.0, 1.0, 1.0, 1.0)];
        assert!(run(Box::new(BuyHold), "T", &backwards, &cfg(0.0)).is_err());
    }

    #[test]
    fn round_trips_count_wins_and_flips() {
        let f = |side, qty, price| Fill {
            side,
            qty,
            price,
            time_ms: 0,
            order_id: 0,
            fee: 0.0,
        };
        // Long 1 @100 -> sell 1 @110 (win); short 1 @110 -> buy 1 @120 (loss).
        let fills = [
            f(Side::Buy, 1.0, 100.0),
            f(Side::Sell, 1.0, 110.0),
            f(Side::Sell, 1.0, 110.0),
            f(Side::Buy, 1.0, 120.0),
        ];
        assert_eq!(count_round_trips(&fills), (2, 1));
        // A flip closes one trade and opens the next.
        let flip = [f(Side::Buy, 1.0, 100.0), f(Side::Sell, 2.0, 110.0)];
        assert_eq!(count_round_trips(&flip), (1, 1));
    }
}
