//! Does funding carry make money?  The rule: when a perpetual's funding,
//! annualized, is at least `min_apr` %, buy the coin on spot and sell its
//! perpetual for the same dollars (`crate::autobasket::Kind::Carry`).  The
//! price moves cancel; the shorts collect the funding the longs pay.  The
//! basket leaves on a stop of its dollars or after `hold_h` hours.
//!
//! This replays exactly that on hourly history of both legs and the
//! perpetual's settlements: a signal at a settlement (its rate is known
//! then), the entry at the close of that hour, every later settlement paid
//! to the short until the exit.  Fees: a taker fill on each leg in and out
//! (spot 10 bps, perpetual 5).  Liquidation: the short perpetual at
//! `leverage` on isolated margin is closed when the hour's high reaches its
//! liquidation price; it loses its margin, the spot leg is sold that hour.
//! Pure: series in, numbers out.

use serde::{Deserialize, Serialize};

use crate::i18n::pick;

pub const KV_KEY: &str = "carry_test";
/// At 30 % a year a week's funding is 0.58 % of a leg against 0.30 % of
/// fees in and out: carry pays only when held for days.
pub const MIN_APR: f64 = 30.0;
pub const HOLD_H: usize = 168;
pub const STOP_PCT: f64 = 3.0;
pub const LEVERAGE: f64 = 3.0;
pub const SPOT_TAKER_BPS: f64 = 10.0;
pub const PERP_TAKER_BPS: f64 = crate::futures::FEE_TAKER_BPS;

/// One hour of both legs: the open time, the closes, the perpetual's high.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hour {
    pub ts: i64,
    pub spot: f64,
    pub perp: f64,
    pub perp_high: f64,
}

/// A coin's history: hours oldest first, settlements (ms, rate) oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub symbol: String,
    pub hours: Vec<Hour>,
    pub funding: Vec<(i64, f64)>,
}

/// The two legs' hours matched by open time (an hour one of them lacks is
/// left out).
pub fn align(spot: &[crate::markets::Ohlc], perp: &[crate::markets::Ohlc]) -> Vec<Hour> {
    let by_ts: std::collections::HashMap<i64, &crate::markets::Ohlc> = perp
        .iter()
        .filter(|k| !k.is_gap)
        .map(|k| (k.time_ms, k))
        .collect();
    spot.iter()
        .filter(|k| !k.is_gap && k.close > 0.0)
        .filter_map(|s| {
            let p = by_ts.get(&s.time_ms)?;
            (p.close > 0.0).then_some(Hour {
                ts: s.time_ms,
                spot: s.close,
                perp: p.close,
                perp_high: p.high.max(p.close),
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Enter at this annualized funding, %.
    pub min_apr: f64,
    pub hold_h: usize,
    /// Stop: the basket's price part down this many % of its dollars.
    pub stop_pct: f64,
    pub leverage: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            min_apr: MIN_APR,
            hold_h: HOLD_H,
            stop_pct: STOP_PCT,
            leverage: LEVERAGE,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CarryTest {
    /// Coins and hours of history used.
    pub coins: usize,
    pub hours: usize,
    pub trades: usize,
    pub wins: usize,
    pub by_stop: usize,
    pub by_clock: usize,
    pub liquidations: usize,
    /// Mean and worst net of a trade, % of the basket's dollars (both legs).
    pub mean_pct: f64,
    pub worst_pct: f64,
    /// The mean's parts: funding collected, the legs' prices, fees.
    pub mean_funding_pct: f64,
    pub mean_price_pct: f64,
    pub mean_fees_pct: f64,
    /// Mean hours held.
    pub mean_hours: f64,
}

impl CarryTest {
    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }
}

/// The rule's replay on all of the history, and on its older and newer
/// halves (does it hold in both?).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CarryReport {
    pub params: Option<Params>,
    pub all: CarryTest,
    pub older: CarryTest,
    pub newer: CarryTest,
    /// Unix ms the test ran.
    pub at_ms: i64,
}

/// Settlements a year at the series' own interval (8 h when unknown).
fn per_year(funding: &[(i64, f64)]) -> f64 {
    let mut gaps: Vec<i64> = funding
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|g| *g > 0)
        .collect();
    gaps.sort_unstable();
    let gap = gaps.get(gaps.len() / 2).copied().unwrap_or(8 * 3_600_000);
    365.0 * 86_400_000.0 / gap.max(3_600_000) as f64
}

/// A rate as a yearly %.
pub fn apr(rate: f64, per_year: f64) -> f64 {
    rate * per_year * 100.0
}

/// One trade's result, % of the basket's dollars.
struct Trade {
    net: f64,
    funding: f64,
    price: f64,
    fees: f64,
    hours: usize,
    stop: bool,
    liquidated: bool,
}

fn replay_coin(s: &Series, p: Params, from_ms: i64, to_ms: i64) -> Vec<Trade> {
    let mut out = Vec::new();
    let py = per_year(&s.funding);
    let fee_leg = (SPOT_TAKER_BPS + PERP_TAKER_BPS) / 100.0 / 2.0; // % of the basket, one way
    let mut free_from = from_ms;
    for &(t, rate) in &s.funding {
        if t < free_from || t >= to_ms || apr(rate, py) < p.min_apr {
            continue;
        }
        // Enter at the close of the settlement's hour.
        let Some(h0) = s.hours.iter().position(|h| h.ts >= t) else {
            break;
        };
        let e = s.hours[h0];
        let entered = e.ts + 3_600_000;
        let liq = crate::futures::liquidation_price(e.perp, false, p.leverage);
        let mut exit = None;
        for (k, h) in s.hours.iter().enumerate().skip(h0 + 1) {
            let held = k - h0;
            if h.ts >= to_ms {
                exit = Some((k - 1, false, false));
                break;
            }
            if liq.is_some_and(|l| h.perp_high >= l) {
                exit = Some((k, false, true));
                break;
            }
            let price = ((h.spot / e.spot - 1.0) - (h.perp / e.perp - 1.0)) / 2.0 * 100.0;
            if price - fee_leg <= -p.stop_pct {
                exit = Some((k, true, false));
                break;
            }
            if held >= p.hold_h {
                exit = Some((k, false, false));
                break;
            }
        }
        let Some((k, stop, liquidated)) = exit else {
            break; // the history ends while it is held: not counted
        };
        if k <= h0 {
            continue;
        }
        let x = s.hours[k];
        let left = x.ts + 3_600_000;
        let spot_ret = x.spot / e.spot - 1.0;
        // A liquidated short loses its margin; else the price's move.
        let perp_ret = if liquidated {
            -1.0 / crate::futures::clamp_leverage(p.leverage)
        } else {
            -(x.perp / e.perp - 1.0)
        };
        let price = (spot_ret + perp_ret) / 2.0 * 100.0;
        let funding: f64 = if liquidated {
            0.0
        } else {
            s.funding
                .iter()
                .filter(|(ft, _)| *ft > entered && *ft <= left)
                .map(|(_, r)| r)
                .sum::<f64>()
                / 2.0
                * 100.0
        };
        let fees = if liquidated {
            fee_leg + SPOT_TAKER_BPS / 100.0 / 2.0
        } else {
            2.0 * fee_leg
        };
        out.push(Trade {
            net: price + funding - fees,
            funding,
            price,
            fees,
            hours: k - h0,
            stop,
            liquidated,
        });
        free_from = left;
    }
    out
}

/// Replay the rule on `series` between `from_ms` and `to_ms`.
pub fn run(series: &[Series], p: Params, from_ms: i64, to_ms: i64) -> CarryTest {
    let mut t = CarryTest {
        coins: series.iter().filter(|s| !s.hours.is_empty()).count(),
        hours: series
            .iter()
            .map(|s| {
                s.hours
                    .iter()
                    .filter(|h| h.ts >= from_ms && h.ts < to_ms)
                    .count()
            })
            .max()
            .unwrap_or(0),
        ..Default::default()
    };
    let trades: Vec<Trade> = series
        .iter()
        .flat_map(|s| replay_coin(s, p, from_ms, to_ms))
        .collect();
    if trades.is_empty() {
        return t;
    }
    let n = trades.len() as f64;
    t.trades = trades.len();
    t.wins = trades.iter().filter(|x| x.net > 0.0).count();
    t.by_stop = trades.iter().filter(|x| x.stop).count();
    t.liquidations = trades.iter().filter(|x| x.liquidated).count();
    t.by_clock = t.trades - t.by_stop - t.liquidations;
    t.mean_pct = trades.iter().map(|x| x.net).sum::<f64>() / n;
    t.worst_pct = trades.iter().map(|x| x.net).fold(f64::INFINITY, f64::min);
    t.mean_funding_pct = trades.iter().map(|x| x.funding).sum::<f64>() / n;
    t.mean_price_pct = trades.iter().map(|x| x.price).sum::<f64>() / n;
    t.mean_fees_pct = trades.iter().map(|x| x.fees).sum::<f64>() / n;
    t.mean_hours = trades.iter().map(|x| x.hours as f64).sum::<f64>() / n;
    t
}

/// The report: all of the history, then its halves by time.
pub fn report(series: &[Series], p: Params, at_ms: i64) -> CarryReport {
    let first = series
        .iter()
        .filter_map(|s| s.hours.first().map(|h| h.ts))
        .min()
        .unwrap_or(0);
    let last = series
        .iter()
        .filter_map(|s| s.hours.last().map(|h| h.ts))
        .max()
        .unwrap_or(0)
        + 3_600_000;
    let mid = first + (last - first) / 2;
    CarryReport {
        params: Some(p),
        all: run(series, p, first, last),
        older: run(series, p, first, mid),
        newer: run(series, p, mid, last),
        at_ms,
    }
}

/// May a carry rule asking for `min_apr` open baskets now?  Only when the
/// replay was made at that threshold and made money after fees overall and
/// on the newer half.  `Ok(why)` or `Err(why)`.
pub fn verdict(report: Option<&CarryReport>, min_apr: f64) -> Result<String, String> {
    let Some(r) = report else {
        return Err(pick(
            "waiting for the replay on history",
            "чекаю прогону на історії",
        )
        .into());
    };
    if r.params.is_none_or(|p| (p.min_apr - min_apr).abs() > 1e-9) {
        return Err(pick(
            "its threshold is not replayed yet",
            "цей поріг ще не прогнано",
        )
        .into());
    }
    let (a, n) = (&r.all, &r.newer);
    let edge = crate::replay::MIN_EDGE_PCT;
    if a.trades >= 5 && a.mean_pct >= edge && n.trades >= 2 && n.mean_pct >= edge {
        Ok(pick(
            format!("{:+.2}% a basket on {} after fees", a.mean_pct, a.trades),
            format!("{:+.2}% на кошик на {} після комісій", a.mean_pct, a.trades),
        ))
    } else if a.trades < 5 {
        Err(pick(
            format!("too few baskets on history ({}) to know", a.trades),
            format!("замало кошиків в історії ({}), щоб знати", a.trades),
        ))
    } else {
        Err(pick(
            format!("loses after fees on history: {:+.2}% a basket", a.mean_pct),
            format!(
                "на історії збиток після комісій: {:+.2}% на кошик",
                a.mean_pct
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;

    /// `n` hours at a flat price with the perpetual `basis` above spot, and
    /// a settlement every 8 h at `rate`.
    fn flat(n: usize, basis: f64, rate: f64) -> Series {
        Series {
            symbol: "X".into(),
            hours: (0..n)
                .map(|i| Hour {
                    ts: i as i64 * H,
                    spot: 100.0,
                    perp: 100.0 * (1.0 + basis),
                    perp_high: 100.0 * (1.0 + basis),
                })
                .collect(),
            funding: (0..n as i64 / 8).map(|i| (i * 8 * H, rate)).collect(),
        }
    }

    #[test]
    fn steady_funding_pays_after_fees_and_low_funding_is_not_traded() {
        // 0.03 % every 8 h = 33 % a year: above the threshold.
        let s = flat(24 * 30, 0.0, 0.0003);
        let t = run(std::slice::from_ref(&s), Params::default(), 0, 24 * 30 * H);
        assert!(t.trades >= 4, "{t:?}");
        assert_eq!(t.liquidations, 0);
        // 168 h = 21 settlements of 0.03 % on half the dollars, less 0.15 % of fees.
        assert!(
            (t.mean_funding_pct - 21.0 * 0.03 / 2.0).abs() < 0.02,
            "{t:?}"
        );
        assert!((t.mean_fees_pct - 0.15).abs() < 1e-9);
        assert!(t.mean_pct > 0.0 && t.wins == t.trades, "{t:?}");
        // Held three days the same funding does not cover the fees.
        let short = Params {
            hold_h: 72,
            ..Params::default()
        };
        assert!(run(&[s], short, 0, 24 * 30 * H).mean_pct < 0.0);
        // 0.005 % = 5 % a year: no trade.
        let low = flat(24 * 30, 0.0, 0.00005);
        assert_eq!(run(&[low], Params::default(), 0, 24 * 30 * H).trades, 0);
    }

    #[test]
    fn a_squeeze_liquidates_the_short_and_the_halves_split_by_time() {
        let mut s = flat(24 * 20, 0.0, 0.001);
        // Ten hours after the first entry the coin doubles.
        for h in s.hours.iter_mut().skip(10) {
            h.spot = 200.0;
            h.perp = 200.0;
            h.perp_high = 200.0;
        }
        let t = run(std::slice::from_ref(&s), Params::default(), 0, 24 * 20 * H);
        assert!(t.liquidations >= 1, "{t:?}");
        // Spot +100 %, the short loses its margin (1/3): about +33 % of a leg.
        assert!(
            t.worst_pct > 0.0,
            "the spot leg pays for the lost margin: {t:?}"
        );
        let r = report(&[s], Params::default(), 7);
        assert!(r.older.trades + r.newer.trades <= r.all.trades + 1);
        assert_eq!(r.at_ms, 7);
    }

    #[test]
    fn the_guard_needs_a_paying_replay_at_the_rule_s_threshold() {
        let s = flat(24 * 60, 0.0, 0.0003);
        let r = report(&[s], Params::default(), 1);
        assert!(verdict(Some(&r), MIN_APR).is_ok(), "{r:?}");
        assert!(verdict(Some(&r), 50.0).is_err(), "another threshold");
        assert!(verdict(None, MIN_APR).is_err());
        let losing = report(
            &[flat(24 * 60, 0.0, 0.0003)],
            Params {
                hold_h: 24,
                ..Params::default()
            },
            1,
        );
        assert!(verdict(Some(&losing), MIN_APR).is_err());
    }

    #[test]
    fn a_widening_basis_stops_the_basket() {
        let mut s = flat(24 * 10, 0.0, 0.001);
        // The perpetual runs 8 % above spot after the first entry.
        for h in s.hours.iter_mut().skip(5) {
            h.perp = 108.0;
            h.perp_high = 108.0;
        }
        let t = run(&[s], Params::default(), 0, 24 * 10 * H);
        assert!(t.by_stop >= 1, "{t:?}");
        assert!(t.worst_pct < -STOP_PCT, "{t:?}");
    }

    #[test]
    fn legs_align_by_hour_and_the_interval_sets_the_year() {
        let k = |t: i64, c: f64| crate::markets::Ohlc {
            open: c,
            high: c + 1.0,
            low: c,
            close: c,
            volume: 1.0,
            date: String::new(),
            time_ms: t,
            is_gap: false,
        };
        let a = align(
            &[k(0, 1.0), k(H, 2.0), k(2 * H, 3.0)],
            &[k(H, 2.5), k(2 * H, 3.5)],
        );
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].spot, a[0].perp, a[0].perp_high), (2.0, 2.5, 3.5));
        let hourly: Vec<(i64, f64)> = (0..10).map(|i| (i * H, 0.0)).collect();
        assert!((per_year(&hourly) - 8760.0).abs() < 1e-6);
        assert!((per_year(&[]) - 1095.0).abs() < 1e-6);
    }
}
