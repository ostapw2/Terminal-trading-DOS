//! Statistical arbitrage on the most liquid perpetuals: a grid of limit
//! orders on the spread of a cointegrated pair.
//!
//! Every day the `top` coins by the previous week's dollar volume (point in
//! time, from the candles) are paired; a pair is kept when, over the
//! formation days, the log prices are cointegrated (Engle-Granger: OLS hedge
//! ratio, then the residual's pull-back t) and the residual returns to its
//! mean in 30 minutes .. 2 days.  It is then traded on the residual's z
//! against a rolling window: one unit at each of `levels` as the spread
//! widens (limit orders resting at those levels, filled a bar later if the
//! spread is still there), all units out together when z comes back to
//! `z_out`.  A position also leaves on a loss cap (at market), on a time cap
//! (a multiple of the pair's half-life), or when its coin's history ends.
//! Legs in dollars by the hedge ratio.  Costs: maker on every limit, taker
//! on the loss cap, and the funding of both perpetuals at every settlement
//! held through.  One coin sits in at most `max_per_coin` open pairs and at
//! most `max_open` pairs are open at once.  Pure: a [`Market`] in, trades
//! out; `examples/statarb_probe.rs` runs it on `market.db`.

use serde::{Deserialize, Serialize};

use crate::markets::Ohlc;

/// One coin's input: name, candles, settlements (ms, rate).
pub type Leg = (String, Vec<Ohlc>, Vec<(i64, f64)>);

/// Aligned closes of many coins on one clock.
pub struct Market {
    pub names: Vec<String>,
    /// Bar opens, ms, ascending.
    pub clock: Vec<i64>,
    /// `px[coin][bar]`: close, carried over a missing bar, NaN outside the
    /// coin's life.
    pub px: Vec<Vec<f64>>,
    /// `dv[coin][bar]`: dollars traded in the bar (0 when missing).
    pub dv: Vec<Vec<f64>>,
    /// Settlements per coin: (ms, rate), ascending.
    pub funding: Vec<Vec<(i64, f64)>>,
    /// Minutes a bar lasts.
    pub bar_min: f64,
}

impl Market {
    /// Put every coin's candles on one clock (the union of their opens).
    pub fn align(series: Vec<Leg>) -> Market {
        let mut clock: Vec<i64> = series
            .iter()
            .flat_map(|(_, c, _)| c.iter().filter(|k| !k.is_gap).map(|k| k.time_ms))
            .collect();
        clock.sort_unstable();
        clock.dedup();
        let bar_min = match clock.len() {
            0 | 1 => 1.0,
            _ => {
                let mut d: Vec<i64> = clock.windows(2).map(|w| w[1] - w[0]).collect();
                d.sort_unstable();
                d[d.len() / 2] as f64 / 60_000.0
            }
        };
        let mut names = Vec::new();
        let mut px = Vec::new();
        let mut dv = Vec::new();
        let mut funding = Vec::new();
        for (name, candles, mut f) in series {
            let mut p = vec![f64::NAN; clock.len()];
            let mut v = vec![0.0; clock.len()];
            let mut at = 0usize;
            for k in candles.iter().filter(|k| !k.is_gap) {
                while at < clock.len() && clock[at] < k.time_ms {
                    at += 1;
                }
                if at < clock.len() && clock[at] == k.time_ms && k.close > 0.0 {
                    p[at] = k.close;
                    v[at] = k.volume * k.close;
                }
            }
            // Carry the last close over a missing bar inside the coin's life.
            let first = p.iter().position(|x| x.is_finite());
            let last = p.iter().rposition(|x| x.is_finite());
            if let (Some(a), Some(b)) = (first, last) {
                for i in a + 1..=b {
                    if !p[i].is_finite() {
                        p[i] = p[i - 1];
                    }
                }
            }
            f.sort_by_key(|x| x.0);
            names.push(name);
            px.push(p);
            dv.push(v);
            funding.push(f);
        }
        Market {
            names,
            clock,
            px,
            dv,
            funding,
            bar_min,
        }
    }

    pub fn bars_per_day(&self) -> usize {
        ((24.0 * 60.0 / self.bar_min).round() as usize).max(1)
    }
}

/// The rule's settings.  Times in days / minutes so they mean the same on
/// 5- and 15-minute bars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Coins by last week's dollars that may form pairs.
    pub top: usize,
    pub form_days: usize,
    /// Engle-Granger pull-back t must be at or under this (negative).
    pub t_max: f64,
    /// Minimum correlation of the formation's returns.
    pub min_r: f64,
    /// Half-life of the residual, minutes.
    pub hl_min: f64,
    pub hl_max: f64,
    /// Rolling window of the z, days (0: the formation's mean and sd).
    pub roll_days: f64,
    /// Entry levels of |z|, one unit each.
    pub levels: Vec<f64>,
    /// All units leave when the spread has come back to this z (0.5:
    /// half a sd past the mean).
    pub z_out: f64,
    /// Leave at market when the position is down this much of its dollars (%).
    pub loss_cap_pct: Option<f64>,
    /// Leave when held this many half-lives (capped by `max_hold_min`).
    pub max_hold_hl: Option<f64>,
    pub max_hold_min: f64,
    /// A limit fills a bar later, and only if the spread is still at its level.
    pub delay_fill: bool,
    pub max_per_coin: usize,
    pub max_open: usize,
    /// No new entries while the market's daily move is over this times its
    /// 30-day median.
    pub regime: Option<f64>,
    pub maker_bps: f64,
    pub taker_bps: f64,
    pub funding: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            top: 20,
            form_days: 7,
            t_max: -3.4,
            min_r: 0.7,
            hl_min: 30.0,
            hl_max: 2880.0,
            roll_days: 1.0,
            levels: vec![2.0, 2.5, 3.0, 3.5],
            z_out: 0.5,
            loss_cap_pct: Some(4.0),
            max_hold_hl: Some(2.0),
            max_hold_min: 2880.0,
            delay_fill: true,
            max_per_coin: 2,
            max_open: 10,
            regime: None,
            maker_bps: 2.0,
            taker_bps: 5.5,
            funding: true,
        }
    }
}

/// Where the replay's report and the live pairs are kept (`kv`).
pub const TEST_KV: &str = "statarb_test";
pub const PAIRS_KV: &str = "statarb_pairs";
/// The replay the guard judges on: 15-minute bars, 200 days, the busiest
/// perpetuals plus the delisted ones.
pub const TEST_DAYS: u32 = 200;
pub const TEST_TF_MIN: u32 = 15;
pub const TEST_POOL: usize = 100;
/// Days of 5-minute bars the live picture is formed on (plus the rolling day).
pub const LIVE_TF_MIN: u32 = 5;
/// A limit of the rule rests this long before what is left goes to market.
pub const ENTRY_WAIT_SECS: u32 = 600;

/// A rule's settings: what `Kind::StatArb` carries (a plain copy of the
/// [`Params`] that matter to a trader).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub top: usize,
    /// First grid level of |z|, the step to the next, how many.
    pub z_in: f64,
    pub z_step: f64,
    pub levels: usize,
    pub z_out: f64,
    pub loss_cap_pct: f64,
    pub max_hold_hl: f64,
    pub max_pairs: usize,
    pub max_per_coin: usize,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            top: 20,
            z_in: 2.0,
            z_step: 0.5,
            levels: 4,
            z_out: 0.5,
            loss_cap_pct: 4.0,
            max_hold_hl: 2.0,
            max_pairs: 10,
            max_per_coin: 2,
        }
    }
}

impl Spec {
    pub fn words(&self) -> String {
        format!(
            "top {}, grid {} x {:.1} sd from {:.1}, out at {:.1}, loss cap {:.0}%, {:.0} half-lives, {} pairs, a coin in {}",
            self.top, self.levels, self.z_step, self.z_in, self.z_out, self.loss_cap_pct,
            self.max_hold_hl, self.max_pairs, self.max_per_coin
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(2..=100).contains(&self.top) {
            return Err("top: 2-100 coins".into());
        }
        if !(self.z_in.is_finite()
            && self.z_in > 0.0
            && self.z_step >= 0.0
            && self.z_step.is_finite())
        {
            return Err("z in must be positive, the step not negative".into());
        }
        if self.levels == 0 || self.levels > 8 {
            return Err("levels: 1-8".into());
        }
        if !(self.z_out.is_finite() && self.z_out >= 0.0 && self.z_out < self.z_in) {
            return Err("z out must be under z in".into());
        }
        if !(self.loss_cap_pct.is_finite() && self.loss_cap_pct > 0.0) {
            return Err("loss cap must be positive".into());
        }
        if !(self.max_hold_hl.is_finite() && self.max_hold_hl > 0.0) {
            return Err("half-lives must be positive".into());
        }
        if self.max_pairs == 0 || self.max_per_coin == 0 {
            return Err("pairs and a coin's pairs must be at least 1".into());
        }
        Ok(())
    }

    /// The grid's levels of |z|.
    pub fn grid(&self) -> Vec<f64> {
        (0..self.levels)
            .map(|k| self.z_in + k as f64 * self.z_step)
            .collect()
    }
}

impl From<Spec> for Params {
    fn from(s: Spec) -> Params {
        Params {
            top: s.top,
            levels: s.grid(),
            z_out: s.z_out,
            loss_cap_pct: Some(s.loss_cap_pct),
            max_hold_hl: Some(s.max_hold_hl),
            max_per_coin: s.max_per_coin,
            max_open: s.max_pairs,
            ..Params::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum How {
    /// The spread came back.
    Back,
    LossCap,
    TimeCap,
    /// The history (or the coin's) ended with the position open.
    End,
}

/// A closed position.  Percentages are of the dollars deployed (every
/// unit's dollars count once).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub x: String,
    pub y: String,
    pub opened_ms: i64,
    pub closed_ms: i64,
    pub units: usize,
    pub gross_pct: f64,
    pub fees_pct: f64,
    pub funding_pct: f64,
    pub how: How,
    /// Dollars the thinner leg trades in 5 minutes, on average (formation).
    pub thin_5m: f64,
    pub bars: usize,
}

impl Trade {
    pub fn net_pct(&self) -> f64 {
        self.gross_pct - self.fees_pct + self.funding_pct
    }
}

pub struct Report {
    pub trades: Vec<Trade>,
    pub pair_days: usize,
    pub skipped_regime: usize,
    pub skipped_room: usize,
    pub first_ms: i64,
    pub last_ms: i64,
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn sd(v: &[f64]) -> f64 {
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// OLS y = a + b x: (a, b).
pub fn ols(y: &[f64], x: &[f64]) -> (f64, f64) {
    let (mx, my) = (mean(x), mean(y));
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for i in 0..x.len().min(y.len()) {
        sxy += (x[i] - mx) * (y[i] - my);
        sxx += (x[i] - mx) * (x[i] - mx);
    }
    let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (my - b * mx, b)
}

/// Pull-back of a series: regress ds on s(t-1); (t of the slope, half-life
/// in bars; infinite when it does not pull back).
pub fn pull_back(s: &[f64]) -> (f64, f64) {
    let n = s.len();
    if n < 30 {
        return (0.0, f64::INFINITY);
    }
    let x = &s[..n - 1];
    let y: Vec<f64> = (1..n).map(|i| s[i] - s[i - 1]).collect();
    let (a, b) = ols(&y, x);
    let mx = mean(x);
    let res: f64 = (0..x.len()).map(|i| (y[i] - a - b * x[i]).powi(2)).sum();
    let sxx: f64 = x.iter().map(|v| (v - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return (0.0, f64::INFINITY);
    }
    let se = (res / (x.len() as f64 - 2.0) / sxx).sqrt();
    let t = if se > 0.0 { b / se } else { 0.0 };
    let hl = if b < 0.0 && b > -1.0 {
        -(2f64.ln()) / (1.0 + b).ln()
    } else {
        f64::INFINITY
    };
    (t, hl)
}

/// A pair formed for one day: the residual's parameters.
#[derive(Clone, Copy)]
struct Pair {
    x: usize,
    y: usize,
    alpha: f64,
    beta: f64,
    /// Formation mean / sd of the residual (used when `roll` is 0).
    m: f64,
    s: f64,
    hl_bars: f64,
    thin_5m: f64,
    /// A level's limit placed last bar, waiting for this bar to confirm.
    armed: Option<f64>,
}

struct Open {
    pair: Pair,
    side: f64,
    /// Entry bar of every unit.
    units: Vec<usize>,
    opened: usize,
    max_bars: usize,
    /// The next grid level's limit placed last bar.
    armed_add: bool,
}

/// Pairs of the day among `uni`, formed on the bars `[f0, f1)`.
fn form(m: &Market, lp: &[Vec<f64>], uni: &[usize], f0: usize, f1: usize, p: &Params) -> Vec<Pair> {
    let nf = f1 - f0;
    let per_5m = 5.0 / m.bar_min;
    let thin = |i: usize| mean(&m.dv[i][f0..f1]) * per_5m;
    let rets: Vec<Vec<f64>> = uni
        .iter()
        .map(|&i| (f0 + 1..f1).map(|t| lp[i][t] - lp[i][t - 1]).collect())
        .collect();
    let mut out = Vec::new();
    for a in 0..uni.len() {
        for b in a + 1..uni.len() {
            let (ra, rb) = (&rets[a], &rets[b]);
            let (ma, mb) = (mean(ra), mean(rb));
            let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
            for i in 0..ra.len() {
                sab += (ra[i] - ma) * (rb[i] - mb);
                saa += (ra[i] - ma).powi(2);
                sbb += (rb[i] - mb).powi(2);
            }
            if saa <= 0.0 || sbb <= 0.0 || sab / (saa * sbb).sqrt() < p.min_r {
                continue;
            }
            let (x, y) = (uni[a], uni[b]);
            let (alpha, beta) = ols(&lp[x][f0..f1], &lp[y][f0..f1]);
            if !(0.2..=5.0).contains(&beta) {
                continue;
            }
            let resid: Vec<f64> = (f0..f1)
                .map(|i| lp[x][i] - alpha - beta * lp[y][i])
                .collect();
            let (t, hl) = pull_back(&resid);
            let hl_min = hl * m.bar_min;
            if t > p.t_max || !(p.hl_min..=p.hl_max).contains(&hl_min) {
                continue;
            }
            let (rm, rs) = (mean(&resid), sd(&resid));
            if rs <= 0.0 || nf < 30 {
                continue;
            }
            out.push(Pair {
                x,
                y,
                alpha,
                beta,
                m: rm,
                s: rs,
                hl_bars: hl,
                thin_5m: thin(x).min(thin(y)),
                armed: None,
            });
        }
    }
    out
}

/// The residual's z at bar `i`.
fn z_at(lp: &[Vec<f64>], pr: &Pair, i: usize, roll: usize) -> f64 {
    let r = |t: usize| lp[pr.x][t] - pr.alpha - pr.beta * lp[pr.y][t];
    let now = r(i);
    if !now.is_finite() {
        return f64::NAN;
    }
    if roll > 0 && i >= roll {
        let w: Vec<f64> = (i - roll..i).map(r).collect();
        if w.iter().any(|v| !v.is_finite()) {
            return f64::NAN;
        }
        let ws = sd(&w);
        if ws > 0.0 {
            (now - mean(&w)) / ws
        } else {
            0.0
        }
    } else {
        (now - pr.m) / pr.s
    }
}

/// One unit's gross % (of its dollars) from bar `i0` to `i`.
fn unit_gross(lp: &[Vec<f64>], pr: &Pair, side: f64, i0: usize, i: usize) -> f64 {
    let ra = (lp[pr.x][i] - lp[pr.x][i0]).exp() - 1.0;
    let rb = (lp[pr.y][i] - lp[pr.y][i0]).exp() - 1.0;
    side * (ra - pr.beta * rb) / (1.0 + pr.beta) * 100.0
}

/// One unit's funding % (of its dollars) over the settlements in
/// `(t0, t1]`: a long pays the rate, a short receives it.
fn unit_funding(m: &Market, pr: &Pair, side: f64, t0: i64, t1: i64) -> f64 {
    let sum = |coin: usize| -> f64 {
        m.funding[coin]
            .iter()
            .filter(|(t, _)| *t > t0 && *t <= t1)
            .map(|(_, r)| r)
            .sum()
    };
    let (wx, wy) = (1.0 / (1.0 + pr.beta), pr.beta / (1.0 + pr.beta));
    -side * (wx * sum(pr.x) - wy * sum(pr.y)) * 100.0
}

/// Replay the rule over the whole market.
pub fn replay(m: &Market, p: &Params) -> Report {
    let n = m.clock.len();
    let bpd = m.bars_per_day();
    let roll = (p.roll_days * bpd as f64).round() as usize;
    let lp: Vec<Vec<f64>> =
        m.px.iter()
            .map(|s| s.iter().map(|v| v.ln()).collect())
            .collect();
    let mut report = Report {
        trades: Vec::new(),
        pair_days: 0,
        skipped_regime: 0,
        skipped_room: 0,
        first_ms: m.clock.first().copied().unwrap_or(0),
        last_ms: m.clock.last().copied().unwrap_or(0),
    };
    let start = p.form_days * bpd;
    if n <= start || p.levels.is_empty() {
        return report;
    }
    let mut today: Vec<Pair> = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    let mut day_vol: Vec<f64> = Vec::new();
    let mut quiet = true;
    let max_hold_bars = (p.max_hold_min / m.bar_min).round().max(1.0) as usize;
    let close = |o: &Open, i: usize, how: How, trades: &mut Vec<Trade>| {
        let pr = &o.pair;
        let exit_bps = if how == How::LossCap {
            p.taker_bps
        } else {
            p.maker_bps
        };
        let k = o.units.len() as f64;
        let gross: f64 = o
            .units
            .iter()
            .map(|&i0| unit_gross(&lp, pr, o.side, i0, i))
            .sum::<f64>()
            / k;
        let funding = if p.funding {
            o.units
                .iter()
                .map(|&i0| unit_funding(m, pr, o.side, m.clock[i0], m.clock[i]))
                .sum::<f64>()
                / k
        } else {
            0.0
        };
        trades.push(Trade {
            x: m.names[pr.x].clone(),
            y: m.names[pr.y].clone(),
            opened_ms: m.clock[o.opened],
            closed_ms: m.clock[i],
            units: o.units.len(),
            gross_pct: gross,
            fees_pct: (p.maker_bps + exit_bps) / 100.0,
            funding_pct: funding,
            how,
            thin_5m: pr.thin_5m,
            bars: i - o.opened,
        });
    };
    for i in start..n {
        let last = i + 1 == n;
        if i % bpd == 0 {
            // A new day: the universe and its pairs.
            let (f0, f1) = (i - start, i);
            let mut by_vol: Vec<(usize, f64)> = (0..m.names.len())
                .filter(|&c| lp[c][f0..f1].iter().all(|v| v.is_finite()))
                .map(|c| {
                    (
                        c,
                        m.dv[c][f1.saturating_sub(7 * bpd)..f1].iter().sum::<f64>(),
                    )
                })
                .filter(|(_, v)| *v > 0.0)
                .collect();
            by_vol.sort_by(|a, b| b.1.total_cmp(&a.1));
            let uni: Vec<usize> = by_vol.into_iter().take(p.top).map(|x| x.0).collect();
            // The market's move over the last day, against its 30-day median.
            if i > bpd {
                let mut moves: Vec<f64> = uni
                    .iter()
                    .map(|&c| (lp[c][i - 1] - lp[c][i - 1 - bpd]).abs())
                    .filter(|v| v.is_finite())
                    .collect();
                let today_vol = median(&mut moves);
                let mut past: Vec<f64> = day_vol.iter().rev().take(30).copied().collect();
                quiet = match p.regime {
                    Some(x) if past.len() >= 10 => today_vol <= x * median(&mut past),
                    _ => true,
                };
                day_vol.push(today_vol);
            }
            today = form(m, &lp, &uni, f0, f1, p);
            report.pair_days += today.len();
        }
        // Open positions: exits first, then the next grid level.
        let mut k = 0;
        while k < open.len() {
            let (z, alive, pnl, max_bars, opened, filled) = {
                let o = &open[k];
                let alive = lp[o.pair.x][i].is_finite() && lp[o.pair.y][i].is_finite();
                let pnl = if alive {
                    o.units
                        .iter()
                        .map(|&i0| unit_gross(&lp, &o.pair, o.side, i0, i))
                        .sum::<f64>()
                        / o.units.len() as f64
                } else {
                    0.0
                };
                let z = if alive {
                    z_at(&lp, &o.pair, i, roll)
                } else {
                    f64::NAN
                };
                (z, alive, pnl, o.max_bars, o.opened, o.units.len())
            };
            if !alive {
                // The coin's history ended: leave at its last price.
                let o = open.remove(k);
                let mut j = i;
                while j > o.opened && !(lp[o.pair.x][j].is_finite() && lp[o.pair.y][j].is_finite())
                {
                    j -= 1;
                }
                close(&o, j, How::End, &mut report.trades);
                continue;
            }
            let side = open[k].side;
            let back = z.is_finite() && side * z >= -p.z_out;
            let loss = p.loss_cap_pct.is_some_and(|cap| pnl <= -cap);
            let time = i - opened >= max_bars;
            let how = if back {
                Some(How::Back)
            } else if loss {
                Some(How::LossCap)
            } else if time {
                Some(How::TimeCap)
            } else if last {
                Some(How::End)
            } else {
                None
            };
            if let Some(how) = how {
                let o = open.remove(k);
                close(&o, i, how, &mut report.trades);
                continue;
            }
            if z.is_finite() && filled < p.levels.len() {
                let o = &mut open[k];
                let there = -side * z >= p.levels[filled];
                if p.delay_fill {
                    if o.armed_add && there {
                        o.units.push(i);
                        o.armed_add = false;
                    } else {
                        o.armed_add = there;
                    }
                } else if there {
                    o.units.push(i);
                }
            }
            k += 1;
        }
        if last {
            break;
        }
        // Entries of the day's pairs.
        for pr in today.iter_mut() {
            let z = z_at(&lp, pr, i, roll);
            if !z.is_finite() {
                pr.armed = None;
                continue;
            }
            let beyond = z.abs() >= p.levels[0];
            let fill = if p.delay_fill {
                let ok = beyond && pr.armed == Some(z.signum());
                pr.armed = if beyond { Some(z.signum()) } else { None };
                ok
            } else {
                beyond
            };
            if !fill {
                continue;
            }
            if open.iter().any(|o| o.pair.x == pr.x && o.pair.y == pr.y) {
                continue;
            }
            if !quiet {
                report.skipped_regime += 1;
                continue;
            }
            let busy = |c: usize| {
                open.iter()
                    .filter(|o| o.pair.x == c || o.pair.y == c)
                    .count()
            };
            if open.len() >= p.max_open
                || busy(pr.x) >= p.max_per_coin
                || busy(pr.y) >= p.max_per_coin
            {
                report.skipped_room += 1;
                continue;
            }
            let side = if z > 0.0 { -1.0 } else { 1.0 };
            let units = p.levels.iter().filter(|l| -side * z >= **l).count().max(1);
            let hl_cap = p.max_hold_hl.map_or(max_hold_bars, |h| {
                ((h * pr.hl_bars).round() as usize).clamp(1, max_hold_bars)
            });
            open.push(Open {
                pair: Pair { armed: None, ..*pr },
                side,
                units: vec![i; units],
                opened: i,
                max_bars: hl_cap,
                armed_add: false,
            });
            pr.armed = None;
        }
    }
    report
}

/// The numbers a report boils down to.  Percentages per dollar deployed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub trades: usize,
    pub units: usize,
    pub won_pct: f64,
    pub mean_pct: f64,
    pub mean_gross_pct: f64,
    pub mean_fees_pct: f64,
    pub mean_funding_pct: f64,
    pub worst_pct: f64,
    pub by_how: [usize; 4],
    pub quarters: [f64; 4],
    pub older_pct: f64,
    pub older_trades: usize,
    pub newer_pct: f64,
    pub newer_trades: usize,
    /// Return on the capital that `max_open` full grids need, and its
    /// deepest drawdown, both % of that capital.
    pub capital_pct: f64,
    pub max_dd_pct: f64,
    /// 1 % of the thinner leg's 5-minute dollars: the median grid, the
    /// thinnest tenth.
    pub thin_median: f64,
    pub thin_tenth: f64,
    pub pair_days: usize,
    pub skipped_regime: usize,
    pub skipped_room: usize,
    pub mean_hours: f64,
}

pub fn summarize(r: &Report, p: &Params) -> Summary {
    let t = &r.trades;
    let mut s = Summary {
        trades: t.len(),
        units: t.iter().map(|x| x.units).sum(),
        pair_days: r.pair_days,
        skipped_regime: r.skipped_regime,
        skipped_room: r.skipped_room,
        ..Default::default()
    };
    if t.is_empty() {
        return s;
    }
    let nets: Vec<f64> = t.iter().map(Trade::net_pct).collect();
    s.won_pct = nets.iter().filter(|x| **x > 0.0).count() as f64 / nets.len() as f64 * 100.0;
    s.mean_pct = mean(&nets);
    s.mean_gross_pct = mean(&t.iter().map(|x| x.gross_pct).collect::<Vec<_>>());
    s.mean_fees_pct = mean(&t.iter().map(|x| x.fees_pct).collect::<Vec<_>>());
    s.mean_funding_pct = mean(&t.iter().map(|x| x.funding_pct).collect::<Vec<_>>());
    s.worst_pct = nets.iter().copied().fold(f64::MAX, f64::min);
    for x in t {
        s.by_how[match x.how {
            How::Back => 0,
            How::LossCap => 1,
            How::TimeCap => 2,
            How::End => 3,
        }] += 1;
    }
    let (first, last) = (
        t.iter().map(|x| x.opened_ms).min().unwrap_or(0),
        t.iter().map(|x| x.opened_ms).max().unwrap_or(0),
    );
    let span = (last - first).max(1);
    for (qi, q) in s.quarters.iter_mut().enumerate() {
        let (lo, hi) = (
            first + span * qi as i64 / 4,
            first + span * (qi as i64 + 1) / 4,
        );
        let v: Vec<f64> = t
            .iter()
            .filter(|x| x.opened_ms >= lo && (x.opened_ms < hi || (qi == 3 && x.opened_ms <= hi)))
            .map(Trade::net_pct)
            .collect();
        *q = mean(&v);
    }
    let mid = first + span * 2 / 3;
    let older: Vec<f64> = t
        .iter()
        .filter(|x| x.opened_ms < mid)
        .map(Trade::net_pct)
        .collect();
    let newer: Vec<f64> = t
        .iter()
        .filter(|x| x.opened_ms >= mid)
        .map(Trade::net_pct)
        .collect();
    s.older_pct = mean(&older);
    s.older_trades = older.len();
    s.newer_pct = mean(&newer);
    s.newer_trades = newer.len();
    // Capital: every unit is a dollar; a full book is max_open grids.
    let capital = (p.max_open * p.levels.len()).max(1) as f64;
    let mut by_close: Vec<(i64, f64)> = t
        .iter()
        .map(|x| (x.closed_ms, x.net_pct() / 100.0 * x.units as f64))
        .collect();
    by_close.sort_by_key(|x| x.0);
    let (mut eq, mut peak, mut dd) = (0.0, 0.0f64, 0.0f64);
    for (_, pnl) in by_close {
        eq += pnl;
        peak = peak.max(eq);
        dd = dd.max(peak - eq);
    }
    s.capital_pct = eq / capital * 100.0;
    s.max_dd_pct = dd / capital * 100.0;
    let mut thin: Vec<f64> = t.iter().map(|x| x.thin_5m * 0.01).collect();
    thin.sort_by(f64::total_cmp);
    s.thin_median = thin[thin.len() / 2];
    s.thin_tenth = thin[thin.len() / 10];
    s.mean_hours = mean(
        &t.iter()
            .map(|x| (x.closed_ms - x.opened_ms) as f64 / 3_600_000.0)
            .collect::<Vec<_>>(),
    );
    s
}

/// May the rule trade?  The same bar every rule has to clear
/// (`crate::replay::MIN_EDGE_PCT` on the newest third, enough trades,
/// positive overall) plus: no losing quarter, and the worst position held
/// by the loss cap.
pub fn passes(s: &Summary, p: &Params) -> Result<String, String> {
    let edge = crate::replay::MIN_EDGE_PCT;
    if s.newer_trades < crate::replay::MIN_TUNE_TRADES {
        return Err(format!(
            "only {} positions on the newest third",
            s.newer_trades
        ));
    }
    if s.newer_pct < edge {
        return Err(format!(
            "newest third {:+.2}% a position, under {edge:+.2}%",
            s.newer_pct
        ));
    }
    if s.mean_pct <= 0.0 {
        return Err(format!("{:+.2}% a position over all", s.mean_pct));
    }
    if let Some(q) = s.quarters.iter().position(|q| *q < 0.0) {
        return Err(format!(
            "quarter {} lost {:+.2}% a position",
            q + 1,
            s.quarters[q]
        ));
    }
    if let Some(cap) = p.loss_cap_pct {
        if s.worst_pct < -cap * 1.5 {
            return Err(format!("worst {:+.1}% past the {cap}% cap", s.worst_pct));
        }
    }
    Ok(format!(
        "newest third {:+.2}% on {} positions, every quarter up",
        s.newer_pct, s.newer_trades
    ))
}

/// A pair of the day as the live picture sees it now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairNow {
    pub x: String,
    pub y: String,
    pub beta: f64,
    /// The residual's z against its rolling day at the last bar.
    pub z: f64,
    pub hl_min: f64,
    /// Dollars the thinner leg trades in 5 minutes.
    pub thin_5m: f64,
    /// Correlation of the formation's returns, for the row.
    pub bars: usize,
}

impl PairNow {
    /// Minutes a position of this pair may be held: `hl` half-lives, half
    /// an hour at least, two days at most.
    pub fn hold_minutes(&self, hl: f64) -> u32 {
        (self.hl_min * hl).round().clamp(30.0, 2880.0) as u32
    }
}

/// The pairs formed on the last `form_days` of `m` (the universe by the
/// last week's dollars) and their z at the last bar: what the rule trades
/// on.  `m` should hold `form_days + roll_days` days.
pub fn picture(m: &Market, p: &Params) -> Vec<PairNow> {
    let n = m.clock.len();
    let bpd = m.bars_per_day();
    let roll = (p.roll_days * bpd as f64).round() as usize;
    let span = p.form_days * bpd;
    if n < span + 2 {
        return Vec::new();
    }
    let lp: Vec<Vec<f64>> =
        m.px.iter()
            .map(|s| s.iter().map(|v| v.ln()).collect())
            .collect();
    let (f0, f1) = (n - 1 - span, n - 1);
    let mut by_vol: Vec<(usize, f64)> = (0..m.names.len())
        .filter(|&c| lp[c][f0..=f1].iter().all(|v| v.is_finite()))
        .map(|c| {
            (
                c,
                m.dv[c][f1.saturating_sub(7 * bpd)..f1].iter().sum::<f64>(),
            )
        })
        .filter(|(_, v)| *v > 0.0)
        .collect();
    by_vol.sort_by(|a, b| b.1.total_cmp(&a.1));
    let uni: Vec<usize> = by_vol.into_iter().take(p.top).map(|x| x.0).collect();
    let mut out: Vec<PairNow> = form(m, &lp, &uni, f0, f1, p)
        .into_iter()
        .map(|pr| PairNow {
            x: m.names[pr.x].clone(),
            y: m.names[pr.y].clone(),
            beta: pr.beta,
            z: z_at(&lp, &pr, n - 1, roll.min(n - 1)),
            hl_min: pr.hl_bars * m.bar_min,
            thin_5m: pr.thin_5m,
            bars: span,
        })
        .filter(|pr| pr.z.is_finite())
        .collect();
    out.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
    out
}

/// The replay's report the guard judges on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatArbReport {
    pub spec: Spec,
    pub summary: Summary,
    pub coins: usize,
    pub bars: usize,
    pub tf_min: u32,
    pub at_ms: i64,
}

/// Replay `spec` on `m` for the guard.
pub fn test(m: &Market, spec: Spec, at_ms: i64) -> StatArbReport {
    let p = Params::from(spec);
    let r = replay(m, &p);
    StatArbReport {
        spec,
        summary: summarize(&r, &p),
        coins: m.names.len(),
        bars: m.clock.len(),
        tf_min: m.bar_min.round() as u32,
        at_ms,
    }
}

/// May a stat-arb rule with `spec` trade?  Only when the replay was made at
/// these settings and passed ([`passes`]).
pub fn verdict(report: Option<&StatArbReport>, spec: &Spec) -> Result<String, String> {
    use crate::i18n::pick;
    let Some(r) = report else {
        return Err(pick(
            "waiting for the replay on 200 days of history",
            "чекаю прогону на 200 днях історії",
        )
        .into());
    };
    if r.spec != *spec {
        return Err(pick(
            "its settings are not replayed yet",
            "ці налаштування ще не прогнано",
        )
        .into());
    }
    let s = &r.summary;
    match passes(s, &Params::from(*spec)) {
        Ok(_) => Ok(pick(
            format!(
                "newest third {:+.2}% a position on {}, every quarter up",
                s.newer_pct, s.newer_trades
            ),
            format!(
                "нова третина {:+.2}% на позицію на {}, усі чверті в плюсі",
                s.newer_pct, s.newer_trades
            ),
        )),
        Err(why) => Err(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(closes: &[f64], step_ms: i64) -> Vec<Ohlc> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Ohlc {
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1_000_000.0 / c,
                date: String::new(),
                time_ms: i as i64 * step_ms,
                is_gap: false,
            })
            .collect()
    }

    /// Deterministic white noise in [-amp, amp] (a linear congruential
    /// generator seeded by the bar).
    fn noise(i: usize, amp: f64) -> f64 {
        let mut x = (i as u64)
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        amp * ((x % 2_000_001) as f64 / 1_000_000.0 - 1.0)
    }

    /// Two coins tied by y = x with a residual that pulls back fast.
    fn tied_market(days: usize) -> Market {
        let bpd = 288;
        let n = days * bpd;
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut level = 100.0f64;
        let mut resid = 0.0f64;
        for i in 0..n {
            level *= 1.0 + 0.004 * ((i as f64 / 5.0).sin()) + 0.002 * ((i as f64 / 41.0).cos());
            // AR(1) residual with a half-life of about 10 bars.
            resid = 0.93 * resid + noise(i, 0.0015);
            x.push(level * (1.0 + resid));
            y.push(level);
        }
        Market::align(vec![
            ("AAA".into(), candles(&x, 300_000), Vec::new()),
            ("BBB".into(), candles(&y, 300_000), Vec::new()),
        ])
    }

    #[test]
    fn pull_back_sees_mean_reversion() {
        let mut s = Vec::with_capacity(600);
        let mut v = 0.0;
        for i in 0..600 {
            v = 0.9 * v + noise(i, 1.0);
            s.push(v);
        }
        let (t, hl) = pull_back(&s);
        assert!(t < -3.0, "t {t}");
        assert!(hl.is_finite() && hl < 50.0, "hl {hl}");
        let walk: Vec<f64> = (0..600).map(|i| i as f64 * 0.1).collect();
        assert!(pull_back(&walk).1.is_infinite());
    }

    #[test]
    fn tied_pair_earns_on_the_grid() {
        let m = tied_market(20);
        assert_eq!(m.bars_per_day(), 288);
        let p = Params {
            top: 2,
            funding: false,
            ..Params::default()
        };
        let r = replay(&m, &p);
        assert!(r.pair_days > 0, "no pair formed");
        assert!(r.trades.len() >= 5, "{} trades", r.trades.len());
        let s = summarize(&r, &p);
        assert!(s.mean_pct > 0.0, "mean {:+.3}", s.mean_pct);
        assert!(s.by_how[0] > 0, "nothing came back: {:?}", s.by_how);
        assert!(s.units >= s.trades);
    }

    #[test]
    fn loss_cap_and_time_cap_close_a_diverging_pair() {
        let bpd = 288;
        let n = 14 * bpd;
        // Tied for 10 days, then x walks away from y for good.
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut resid = 0.0f64;
        for i in 0..n {
            let level = 100.0 * (1.0 + 0.3 * ((i as f64 / 5.0).sin()));
            resid = if i < 10 * bpd {
                0.93 * resid + noise(i, 0.0015)
            } else {
                resid + 0.0006
            };
            x.push(level * (1.0 + resid));
            y.push(level);
        }
        let m = Market::align(vec![
            ("AAA".into(), candles(&x, 300_000), Vec::new()),
            ("BBB".into(), candles(&y, 300_000), Vec::new()),
        ]);
        let p = Params {
            top: 2,
            funding: false,
            loss_cap_pct: Some(3.0),
            max_hold_hl: None,
            ..Params::default()
        };
        let r = replay(&m, &p);
        let capped = r.trades.iter().filter(|t| t.how == How::LossCap).count();
        assert!(
            capped > 0,
            "no loss cap fired: {:?}",
            r.trades.iter().map(|t| t.how).collect::<Vec<_>>()
        );
        for t in r.trades.iter().filter(|t| t.how == How::LossCap) {
            assert!(
                t.gross_pct <= -3.0 && t.gross_pct > -6.0,
                "capped at {:+.2}",
                t.gross_pct
            );
            assert!(t.fees_pct > p.maker_bps * 2.0 / 100.0, "taker on the cap");
        }
        let timed = Params {
            loss_cap_pct: None,
            max_hold_hl: Some(1.0),
            ..p.clone()
        };
        let r = replay(&m, &timed);
        assert!(r.trades.iter().any(|t| t.how == How::TimeCap));
        let s = summarize(&r, &timed);
        assert!(passes(&s, &timed).is_err());
    }

    #[test]
    fn funding_is_paid_by_the_long_leg() {
        let m = tied_market(10);
        let pr = Pair {
            x: 0,
            y: 1,
            alpha: 0.0,
            beta: 1.0,
            m: 0.0,
            s: 1.0,
            hl_bars: 10.0,
            thin_5m: 0.0,
            armed: None,
        };
        let mut m = m;
        m.funding[0] = vec![(1_000, 0.0010)];
        m.funding[1] = vec![(1_000, 0.0002)];
        // Long x (pays 10 bps on half the dollars), short y (receives 2 bps on half).
        let f = unit_funding(&m, &pr, 1.0, 0, 2_000);
        assert!((f - (-0.05 + 0.01)).abs() < 1e-9, "{f}");
        assert!((unit_funding(&m, &pr, -1.0, 0, 2_000) + f).abs() < 1e-12);
        assert_eq!(unit_funding(&m, &pr, 1.0, 1_000, 2_000), 0.0);
    }

    #[test]
    fn the_picture_names_the_tied_pair_with_its_z() {
        let m = tied_market(9);
        let p = Params {
            top: 2,
            ..Params::default()
        };
        let pic = picture(&m, &p);
        assert_eq!(pic.len(), 1, "{pic:?}");
        let pr = &pic[0];
        assert!((pr.x == "AAA" && pr.y == "BBB") || (pr.x == "BBB" && pr.y == "AAA"));
        assert!(pr.z.is_finite() && pr.z.abs() < 6.0);
        assert!(pr.hl_min > 0.0 && pr.hold_minutes(2.0) >= 30);
        assert!(picture(&tied_market(3), &p).is_empty(), "too short to form");
    }

    #[test]
    fn a_spec_becomes_params_and_a_verdict() {
        let spec = Spec::default();
        assert!(spec.validate().is_ok());
        assert_eq!(spec.grid(), vec![2.0, 2.5, 3.0, 3.5]);
        let p = Params::from(spec);
        assert_eq!(p.levels, spec.grid());
        assert_eq!(p.max_open, 10);
        assert!(Spec { z_out: 3.0, ..spec }.validate().is_err());
        assert!(verdict(None, &spec).is_err());
        let m = tied_market(20);
        let r = test(&m, Spec { top: 2, ..spec }, 1);
        assert_eq!(r.spec.top, 2);
        assert!(verdict(Some(&r), &spec).is_err(), "other settings");
        let v = verdict(Some(&r), &r.spec);
        assert_eq!(v.is_ok(), passes(&r.summary, &Params::from(r.spec)).is_ok());
    }

    #[test]
    fn concentration_caps_hold() {
        let m = tied_market(20);
        let p = Params {
            top: 2,
            max_open: 0,
            funding: false,
            ..Params::default()
        };
        let r = replay(&m, &p);
        assert!(r.trades.is_empty());
        assert!(r.skipped_room > 0);
    }
}
