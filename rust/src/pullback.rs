//! Pullback: buy the break of a pullback after a fast rise.
//!
//! A coin up at least `min_rise_pct` over the last `window_min` minutes
//! (15 or 60, set by hand) is watched on `tf_min`-minute candles.  After
//! the top, the first closed red candle (close below open) arms a buy-stop
//! just above its high; every newer red candle moves the stop down to its
//! own high.  When a candle trades above the stop, the coin is bought: the
//! stop-loss sits under the pullback's low, the target `target_r` times that
//! risk above the entry, and the trade leaves after `MAX_HOLD_BARS` candles
//! at most.  The setup is dropped when the pullback gives back more than
//! `MAX_RETRACE` of the rise, or nothing breaks within `MAX_WAIT_BARS`.
//!
//! [`analyze`] reads one coin's candles (the last one may still be forming)
//! and says whether a setup is armed and whether it has triggered;
//! [`replay`] runs the same rule over history.  Long only, on spot or on
//! the coin's perpetual (`perp`): fees as the paper ledger charges them
//! ([`Params::fees_pct`]); the stop fills at its level and a candle that
//! reaches both the stop and the target counts as the stop.  Funding is not
//! counted (a trade lasts hours).  Pure: candles in, numbers out.

use serde::{Deserialize, Serialize};

use crate::i18n::pick;
use crate::markets::Ohlc;

pub const TEST_KV: &str = "pullback_test";
/// A pullback deeper than this share of the rise kills the setup.
pub const MAX_RETRACE: f64 = 0.5;
/// Candles after the top a break may take, and a trade may last.
pub const MAX_WAIT_BARS: usize = 6;
pub const MAX_HOLD_BARS: usize = 48;
/// The buy-stop sits this far above the red candle's high, the stop-loss
/// this far under the pullback's low (bps).
pub const BUFFER_BPS: f64 = 5.0;
/// A break already further than this above the trigger is not chased (%).
pub const MAX_CHASE_PCT: f64 = 0.5;
const SPOT_FEE_BPS: f64 = 10.0;

pub const RISERS_KV: &str = "risers";

/// A coin's move over the last 15 and 60 minutes, from the live all-market
/// stream (the scanning window writes these for the trading window).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Riser {
    pub symbol: String,
    pub pct_15: Option<f64>,
    pub pct_60: Option<f64>,
    pub price: f64,
}

/// The risers worth a look for `p`: up at least `min_rise_pct` over its window.
pub fn watch<'a>(risers: &'a [Riser], p: &Params) -> Vec<&'a Riser> {
    risers
        .iter()
        .filter(|r| {
            let pct = if p.window_min >= 60 {
                r.pct_60
            } else {
                r.pct_15
            };
            pct.is_some_and(|x| x >= p.min_rise_pct)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub window_min: u32,
    pub min_rise_pct: f64,
    pub tf_min: u32,
    pub target_r: f64,
    /// On the coin's perpetual instead of spot.
    #[serde(default)]
    pub perp: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            window_min: 15,
            tf_min: 5,
            // Chosen on the older two thirds of 13 days of 120 coins' 5-minute
            // perpetual candles, and paid on the newest third (20.10.2026).
            min_rise_pct: 5.0,
            target_r: 3.0,
            perp: true,
        }
    }
}

impl Params {
    /// Candles the rise window spans (one at least).
    pub fn window_bars(&self) -> usize {
        (self.window_min / self.tf_min.max(1)).max(1) as usize
    }
    /// The interval name Binance gives `tf_min` minutes.
    pub fn interval(&self) -> &'static str {
        match self.tf_min {
            1 => "1m",
            3 => "3m",
            15 => "15m",
            30 => "30m",
            60 => "1h",
            _ => "5m",
        }
    }
    pub fn words(&self) -> String {
        format!(
            "up {:.1}%+ in {} min, buy the break of a red {}m candle, stop under the pullback, target {:.1}R, {}",
            self.min_rise_pct,
            self.window_min,
            self.tf_min,
            self.target_r,
            if self.perp { "perpetual" } else { "spot" }
        )
    }

    /// Fees of a round trip, % of the dollars: in on the buy-stop (taker),
    /// out on the target's limit (maker) or at market (stop, clock).
    /// Spot: 10 bps either way; perpetuals: maker 2 / taker 5.
    pub fn fees_pct(&self, at_target: bool) -> f64 {
        let (maker, taker) = if self.perp {
            (crate::futures::FEE_MAKER_BPS, crate::futures::FEE_TAKER_BPS)
        } else {
            (SPOT_FEE_BPS, SPOT_FEE_BPS)
        };
        (taker + if at_target { maker } else { taker }) / 100.0
    }
}

/// An armed setup.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// The buy-stop, the stop-loss, the target.
    pub trigger: f64,
    pub stop: f64,
    pub target: f64,
    /// The rise it follows, %; the open time of the red candle that armed it.
    pub rise_pct: f64,
    pub red_ms: i64,
    /// The forming candle has traded through the trigger (and not past the
    /// chase limit): buy now.
    pub triggered: bool,
}

impl Setup {
    /// The basket's exit for a buy at `entry`: stop and target as % of its
    /// dollars, the clock in minutes.
    pub fn exit(&self, entry: f64, p: &Params) -> crate::basket::ExitRule {
        let risk = ((entry - self.stop) / entry * 100.0).max(0.05);
        crate::basket::ExitRule {
            target_pct: Some(risk * p.target_r),
            stop_pct: Some(risk),
            max_minutes: Some(MAX_HOLD_BARS as u32 * p.tf_min),
        }
    }
}

fn red(c: &Ohlc) -> bool {
    c.close < c.open
}

/// The setup on `candles` (oldest first; the last one may be forming), if one
/// is armed now.  `closed` = how many of them have closed.
fn setup_at(candles: &[Ohlc], closed: usize, p: &Params) -> Option<Setup> {
    let w = p.window_bars();
    if closed < w + 2 {
        return None;
    }
    let done = &candles[..closed];
    // The top: the highest high of the last MAX_WAIT_BARS + 1 closed candles.
    let look = (MAX_WAIT_BARS + 1).min(done.len());
    let (top_i, top) = done[done.len() - look..]
        .iter()
        .enumerate()
        .map(|(k, c)| (done.len() - look + k, c.high))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    // The rise: from the lowest low of the window before the top.
    let from = top_i.saturating_sub(w);
    let base = done[from..=top_i]
        .iter()
        .map(|c| c.low)
        .fold(f64::INFINITY, f64::min);
    if base.is_nan() || base <= 0.0 {
        return None;
    }
    let rise_pct = (top / base - 1.0) * 100.0;
    if rise_pct < p.min_rise_pct {
        return None;
    }
    // The pullback: the candles closed after the top (the top's own candle
    // only when it is the red one).
    let after = if red(&done[top_i]) {
        &done[top_i..]
    } else {
        &done[top_i + 1..]
    };
    if after.is_empty() {
        return None;
    }
    let low = after.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    if low < top - MAX_RETRACE * (top - base) || after.len() > MAX_WAIT_BARS + 1 {
        return None;
    }
    let (k, last_red) = after.iter().enumerate().rev().find(|(_, c)| red(c))?;
    // A closed candle after the red one already broke it: that trade was then.
    if after[k + 1..].iter().any(|c| c.high > last_red.high) {
        return None;
    }
    let trigger = last_red.high * (1.0 + BUFFER_BPS / 10_000.0);
    let stop = low * (1.0 - BUFFER_BPS / 10_000.0);
    if !(stop > 0.0 && trigger > stop) {
        return None;
    }
    let target = trigger + p.target_r * (trigger - stop);
    let forming = candles.get(closed);
    let triggered = forming
        .is_some_and(|f| f.high >= trigger && f.close <= trigger * (1.0 + MAX_CHASE_PCT / 100.0));
    Some(Setup {
        trigger,
        stop,
        target,
        rise_pct,
        red_ms: last_red.time_ms,
        triggered,
    })
}

/// The setup now on a coin's candles; the last candle is the forming one.
pub fn analyze(candles: &[Ohlc], p: &Params) -> Option<Setup> {
    let c: Vec<Ohlc> = candles.iter().filter(|c| !c.is_gap).cloned().collect();
    if c.len() < 2 {
        return None;
    }
    setup_at(&c, c.len() - 1, p)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PullbackTest {
    pub params: Option<Params>,
    pub coins: usize,
    pub bars: usize,
    pub trades: usize,
    pub wins: usize,
    pub by_target: usize,
    pub by_stop: usize,
    pub by_clock: usize,
    /// Mean and worst net of a trade, % of its dollars; the mean before fees.
    pub mean_pct: f64,
    pub worst_pct: f64,
    pub mean_gross_pct: f64,
    /// Trades opened in the older two thirds and the newest third.
    pub older_pct: f64,
    pub older_trades: usize,
    pub newer_pct: f64,
    pub newer_trades: usize,
}

impl PullbackTest {
    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }
    /// The guard's bar (`crate::replay`).
    pub fn passes(&self) -> bool {
        self.newer_trades >= crate::replay::MIN_VERDICT_TRADES
            && self.newer_pct >= crate::replay::MIN_EDGE_PCT
            && self.mean_pct > 0.0
    }
}

/// Replay the rule on each coin's candles (`tf_min` minutes, oldest first).
/// The split into older / newer is by time, across coins.
pub fn replay(series: &[(String, Vec<Ohlc>)], p: Params) -> PullbackTest {
    let mut out = PullbackTest {
        params: Some(p),
        coins: series.len(),
        bars: series.iter().map(|s| s.1.len()).max().unwrap_or(0),
        ..Default::default()
    };
    let first = series
        .iter()
        .filter_map(|s| s.1.first().map(|c| c.time_ms))
        .min()
        .unwrap_or(0);
    let last = series
        .iter()
        .filter_map(|s| s.1.last().map(|c| c.time_ms))
        .max()
        .unwrap_or(0);
    let cut = first + (last - first) * 2 / 3;
    // (net, gross, opened ms, how)
    let mut done: Vec<(f64, f64, i64, u8)> = Vec::new();
    for (_, raw) in series {
        let c: Vec<Ohlc> = raw.iter().filter(|c| !c.is_gap).cloned().collect();
        let mut t = 0;
        while t + 1 < c.len() {
            // Candle t is forming: does an armed setup break in it?
            let Some(s) = setup_at(&c, t, &p) else {
                t += 1;
                continue;
            };
            let bar = &c[t];
            if bar.high < s.trigger || bar.open > s.trigger * (1.0 + MAX_CHASE_PCT / 100.0) {
                t += 1;
                continue;
            }
            // Bought at the trigger (a gap over it: at the open).
            let entry = bar.open.max(s.trigger);
            let target = entry + p.target_r * (entry - s.stop);
            let mut exit = None;
            if bar.low <= s.stop {
                exit = Some((s.stop, 1u8, t));
            } else {
                for (k, x) in c.iter().enumerate().skip(t + 1) {
                    if x.low <= s.stop {
                        exit = Some((s.stop.min(x.open), 1, k));
                        break;
                    }
                    if x.high >= target {
                        exit = Some((target.max(x.open), 0, k));
                        break;
                    }
                    if k - t >= MAX_HOLD_BARS {
                        exit = Some((x.close, 2, k));
                        break;
                    }
                }
            }
            let Some((px, how, k)) = exit else {
                break; // still open when the history ends: not counted
            };
            let gross = (px / entry - 1.0) * 100.0;
            done.push((gross - p.fees_pct(how == 0), gross, bar.time_ms, how));
            t = k + 1;
        }
    }
    if done.is_empty() {
        return out;
    }
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let nets: Vec<f64> = done.iter().map(|d| d.0).collect();
    let older: Vec<f64> = done.iter().filter(|d| d.2 < cut).map(|d| d.0).collect();
    let newer: Vec<f64> = done.iter().filter(|d| d.2 >= cut).map(|d| d.0).collect();
    out.trades = done.len();
    out.wins = nets.iter().filter(|x| **x > 0.0).count();
    out.by_target = done.iter().filter(|d| d.3 == 0).count();
    out.by_stop = done.iter().filter(|d| d.3 == 1).count();
    out.by_clock = done.iter().filter(|d| d.3 == 2).count();
    out.mean_pct = mean(&nets);
    out.worst_pct = nets.iter().copied().fold(f64::INFINITY, f64::min);
    out.mean_gross_pct = mean(&done.iter().map(|d| d.1).collect::<Vec<_>>());
    out.older_pct = mean(&older);
    out.older_trades = older.len();
    out.newer_pct = mean(&newer);
    out.newer_trades = newer.len();
    out
}

/// The settings tried around the user's: rise x0.67 / x1 / x1.67, target
/// 1.5 / 2 / 3 R (window and candle size stay the user's).
pub fn grid(base: Params) -> Vec<Params> {
    let mut v = Vec::new();
    for k in [0.67, 1.0, 1.67] {
        for target_r in [1.5, 2.0, 3.0] {
            v.push(Params {
                min_rise_pct: (base.min_rise_pct * k * 10.0).round() / 10.0,
                target_r,
                ..base
            });
        }
    }
    v
}

/// The user's setting, the settings tried, the best on the older two thirds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PullbackReport {
    pub own: PullbackTest,
    pub tried: Vec<PullbackTest>,
    pub best: Option<PullbackTest>,
    pub at_ms: i64,
}

pub fn report(series: &[(String, Vec<Ohlc>)], own: Params, at_ms: i64) -> PullbackReport {
    let tried: Vec<PullbackTest> = grid(own).into_iter().map(|p| replay(series, p)).collect();
    let best = tried
        .iter()
        .filter(|x| x.older_trades >= crate::replay::MIN_TUNE_TRADES)
        .max_by(|a, b| a.older_pct.total_cmp(&b.older_pct))
        .cloned();
    PullbackReport {
        own: replay(series, own),
        tried,
        best,
        at_ms,
    }
}

/// May a pullback rule with `p` trade?  Only when the replay was made at
/// these settings and passed on the newest third.
pub fn verdict(report: Option<&PullbackReport>, p: &Params) -> Result<String, String> {
    let Some(r) = report else {
        return Err(pick(
            "waiting for the replay on history",
            "чекаю прогону на історії",
        )
        .into());
    };
    if r.own.params.as_ref() != Some(p) {
        return Err(pick(
            "its settings are not replayed yet",
            "ці налаштування ще не прогнано",
        )
        .into());
    }
    let t = &r.own;
    let edge = crate::replay::MIN_EDGE_PCT;
    if t.passes() {
        Ok(pick(
            format!(
                "newest third {:+.2}% a trade on {} after fees",
                t.newer_pct, t.newer_trades
            ),
            format!(
                "нова третина {:+.2}% на угоду на {} після комісій",
                t.newer_pct, t.newer_trades
            ),
        ))
    } else if t.newer_trades < crate::replay::MIN_VERDICT_TRADES {
        Err(pick(
            format!(
                "too few trades on the newest third ({}) to know",
                t.newer_trades
            ),
            format!(
                "замало угод на новій третині ({}), щоб знати",
                t.newer_trades
            ),
        ))
    } else {
        Err(pick(
            format!("under +{edge:.2}% a trade after fees on unseen history: {:+.2}% (newest third {:+.2}%)", t.mean_pct, t.newer_pct),
            format!("менше +{edge:.2}% на угоду після комісій на непобаченій історії: {:+.2}% (нова третина {:+.2}%)", t.mean_pct, t.newer_pct),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(t: i64, o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            date: String::new(),
            time_ms: t * 300_000,
            is_gap: false,
        }
    }

    /// Flat at 100, a rise to 106 over three candles, a red candle, then
    /// whatever `tail` says.
    fn rise_then(tail: &[Ohlc]) -> Vec<Ohlc> {
        let mut v: Vec<Ohlc> = (0..5).map(|i| k(i, 100.0, 100.2, 99.8, 100.0)).collect();
        v.push(k(5, 100.0, 102.0, 99.9, 102.0));
        v.push(k(6, 102.0, 104.0, 101.8, 104.0));
        v.push(k(7, 104.0, 106.0, 103.8, 105.8));
        v.push(k(8, 105.8, 105.9, 104.0, 104.5)); // red: high 105.9, low 104
        v.extend_from_slice(tail);
        v
    }

    #[test]
    fn a_red_candle_after_a_rise_arms_a_buy_stop_above_it() {
        let p = Params {
            min_rise_pct: 3.0,
            target_r: 2.0,
            perp: false,
            ..Params::default()
        };
        // The forming candle has not reached the trigger.
        let c = rise_then(&[k(9, 104.5, 105.0, 104.3, 104.8)]);
        let s = analyze(&c, &p).expect("armed");
        assert!((s.trigger - 105.9 * 1.0005).abs() < 1e-9);
        assert!((s.stop - 104.0 * 0.9995).abs() < 1e-9);
        assert!(!s.triggered && s.rise_pct > 5.0);
        assert!((s.target - (s.trigger + 2.0 * (s.trigger - s.stop))).abs() < 1e-9);
        // Now it trades through: buy.
        let c = rise_then(&[k(9, 104.5, 106.1, 104.3, 106.0)]);
        assert!(analyze(&c, &p).unwrap().triggered);
        // Too far through: not chased.
        let c = rise_then(&[k(9, 104.5, 108.0, 104.3, 107.9)]);
        assert!(!analyze(&c, &p).unwrap().triggered);
        let x = s.exit(s.trigger, &p);
        assert!((x.target_pct.unwrap() - 2.0 * x.stop_pct.unwrap()).abs() < 1e-9);
    }

    #[test]
    fn a_newer_red_candle_lowers_the_trigger_and_a_deep_pullback_kills_it() {
        let p = Params {
            min_rise_pct: 3.0,
            target_r: 2.0,
            perp: false,
            ..Params::default()
        };
        let c = rise_then(&[
            k(9, 104.5, 104.9, 104.1, 104.2), // red, lower high
            k(10, 104.2, 104.4, 104.1, 104.3),
        ]);
        let s = analyze(&c, &p).unwrap();
        assert!((s.trigger - 104.9 * 1.0005).abs() < 1e-9, "{s:?}");
        // Gives back more than half of the rise (low under 103).
        let deep = rise_then(&[
            k(9, 104.5, 104.6, 102.5, 102.8),
            k(10, 102.8, 103.0, 102.7, 102.9),
        ]);
        assert!(analyze(&deep, &p).is_none());
        // No rise: nothing.
        let flat: Vec<Ohlc> = (0..12).map(|i| k(i, 100.0, 100.2, 99.8, 99.9)).collect();
        assert!(analyze(&flat, &p).is_none());
    }

    #[test]
    fn the_replay_takes_the_break_and_follows_it_to_the_target_or_the_stop() {
        let p = Params {
            min_rise_pct: 3.0,
            target_r: 2.0,
            perp: false,
            ..Params::default()
        };
        let mut win = rise_then(&[k(9, 104.5, 106.0, 104.4, 105.95)]);
        for i in 10..20 {
            win.push(k(i, 106.0, 110.0, 105.9, 109.0)); // runs to the target
        }
        let mut lose = rise_then(&[k(9, 104.5, 106.0, 104.4, 105.95)]);
        lose.push(k(10, 105.9, 106.0, 103.0, 103.5)); // through the stop
        lose.push(k(11, 103.5, 103.6, 103.0, 103.2));
        let t = replay(&[("W".into(), win), ("L".into(), lose)], p);
        assert_eq!((t.trades, t.by_target, t.by_stop), (2, 1, 1), "{t:?}");
        assert_eq!(t.wins, 1);
        assert!(t.mean_gross_pct - t.mean_pct > 0.19, "fees in and out");
        let perp = Params { perp: true, ..p };
        assert!(
            (perp.fees_pct(true) - 0.07).abs() < 1e-9 && (perp.fees_pct(false) - 0.10).abs() < 1e-9
        );
        assert!((p.fees_pct(true) - 0.20).abs() < 1e-9);
        assert_eq!(grid(p).len(), 9);
        let r = report(&[], p, 4);
        assert!(verdict(Some(&r), &p).is_err() && verdict(None, &p).is_err());
        let other = Params { target_r: 3.0, ..p };
        assert!(!verdict(Some(&r), &other).unwrap_err().is_empty());
    }
}
