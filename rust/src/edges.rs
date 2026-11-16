//! Pairs at their edge: statistical arbitrage on the widest gap.
//!
//! A pair qualifies when its two coins moved together in every one of the
//! last `months` months (the weakest month's correlation of hourly changes
//! at least `min_r`: "most often correlated", not on average).  Its gap is
//! the price ratio's distance from its own `mean_h`-hour mean, in %
//! (100 x the log ratio).  The edge is the widest gap of the last `months`
//! months.  The rule waits until today's gap reaches `entry_frac` of that
//! edge, then sells the rich coin and buys the cheap one, equal dollars;
//! it leaves when the gap is back to `exit_frac` of the edge (0: the mean),
//! on a stop when the gap runs `stop_frac` times the old edge (the tie is
//! broken), or after `max_hold_h`.
//!
//! [`replay`] tests exactly that on hourly history with no look-ahead: the
//! pairs and their edges are chosen every week from the months before it.
//! Fees as the paper ledger charges a basket on perpetuals: a maker fill in
//! (2 bps a leg), maker out on the mean and the clock, taker on the stop.
//! Funding on the two perpetuals is not counted (they mostly offset).
//! Pure: series in, numbers out.

use serde::{Deserialize, Serialize};

pub const BOOK_KV: &str = "edge_book";
pub const TEST_KV: &str = "edge_test";
/// Hours a month counts, and the hours between two re-selections in the replay.
pub const MONTH_H: usize = 720;
pub const RESELECT_H: usize = 168;
/// Pairs kept in a book at most.
pub const MAX_PAIRS: usize = 60;
const MAKER_BPS: f64 = crate::futures::FEE_MAKER_BPS;
const TAKER_BPS: f64 = crate::futures::FEE_TAKER_BPS;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Months the tie and the edge are measured over.
    pub months: usize,
    /// The weakest month's correlation a pair needs.
    pub min_r: f64,
    /// Hours of the ratio's own mean.
    pub mean_h: usize,
    /// Enter at this share of the edge; leave at `exit_frac` of it; stop at
    /// `stop_frac` times it.
    pub entry_frac: f64,
    pub exit_frac: f64,
    pub stop_frac: f64,
    pub max_hold_h: usize,
    /// Baskets open at once.
    pub max_open: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            months: 3,
            min_r: 0.8,
            mean_h: 168,
            entry_frac: 0.9,
            exit_frac: 0.0,
            stop_frac: 1.5,
            max_hold_h: 336,
            max_open: 10,
        }
    }
}

/// One pair of the book: how tied, how wide its edge, where it is now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgePair {
    pub a: String,
    pub b: String,
    /// Correlation of hourly changes: the weakest month, and all of them.
    pub r_min: f64,
    pub r_all: f64,
    /// The widest gap of the months, % (absolute).
    pub edge_pct: f64,
    /// The gap now, % (signed: + means `a` rich against `b`).
    pub gap_pct: f64,
    /// Separate times the gap reached the entry level in the months.
    pub times: usize,
    /// The leg to buy and the one to sell if it trades now.
    pub long: String,
    pub short: String,
}

impl EdgePair {
    /// The gap now as a share of the edge.
    pub fn share(&self) -> f64 {
        if self.edge_pct > 0.0 {
            self.gap_pct.abs() / self.edge_pct
        } else {
            0.0
        }
    }
    /// Is it at the entry level of `p`?
    pub fn at_edge(&self, p: &Params) -> bool {
        self.edge_pct > 0.0 && self.share() >= p.entry_frac
    }
    /// The basket's exit for a trade opened now: the target when the gap is
    /// back to `exit_frac` of the edge, the stop at `stop_frac` times it,
    /// both as % of the basket's dollars (two legs: half the gap's move).
    pub fn exit(&self, p: &Params) -> crate::basket::ExitRule {
        let now = self.gap_pct.abs();
        crate::basket::ExitRule {
            target_pct: Some(((now - p.exit_frac * self.edge_pct) / 2.0).max(0.05)),
            stop_pct: Some(((p.stop_frac * self.edge_pct - now) / 2.0).max(0.05)),
            max_minutes: Some(p.max_hold_h as u32 * 60),
        }
    }
}

/// The book at the end of the series: the tied pairs, their edges, now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub params: Option<Params>,
    pub coins: usize,
    pub hours: usize,
    /// Pairs tried and pairs tied.
    pub tried: usize,
    pub pairs: Vec<EdgePair>,
    pub at_ms: i64,
}

/// Hourly closes per coin, every series the same hours (aligned at the end).
pub type Series = [(String, Vec<f64>)];

struct Prepared {
    names: Vec<String>,
    /// ln price, and its running sum (for the means).
    ln: Vec<Vec<f64>>,
    sum: Vec<Vec<f64>>,
    /// Hourly log changes (index t: from t-1 to t; 0 at t = 0).
    ch: Vec<Vec<f64>>,
}

fn prepare(series: &Series) -> (Prepared, usize) {
    let n = series.iter().map(|s| s.1.len()).min().unwrap_or(0);
    let mut p = Prepared {
        names: Vec::new(),
        ln: Vec::new(),
        sum: Vec::new(),
        ch: Vec::new(),
    };
    for (name, c) in series {
        let c = &c[c.len() - n..];
        if c.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
            continue;
        }
        let ln: Vec<f64> = c.iter().map(|x| x.ln()).collect();
        let mut sum = vec![0.0; n + 1];
        for t in 0..n {
            sum[t + 1] = sum[t] + ln[t];
        }
        let mut ch = vec![0.0; n];
        for t in 1..n {
            ch[t] = ln[t] - ln[t - 1];
        }
        p.names.push(name.clone());
        p.ln.push(ln);
        p.sum.push(sum);
        p.ch.push(ch);
    }
    (p, n)
}

/// The gap of pair (i, j) at hour t, % (needs t >= mean_h).
fn gap(p: &Prepared, i: usize, j: usize, t: usize, mean_h: usize) -> f64 {
    let s = p.ln[i][t] - p.ln[j][t];
    let m = ((p.sum[i][t] - p.sum[i][t - mean_h]) - (p.sum[j][t] - p.sum[j][t - mean_h]))
        / mean_h as f64;
    (s - m) * 100.0
}

fn corr(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    if n < 3.0 {
        return 0.0;
    }
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa <= 1e-18 || sbb <= 1e-18 {
        0.0
    } else {
        sab / (saa * sbb).sqrt()
    }
}

/// Hours of history a book at hour t needs before it.
pub fn warmup(p: &Params) -> usize {
    p.months * MONTH_H + p.mean_h
}

/// The book as it stood at hour `t` (exclusive: only hours before `t` are
/// read, plus `t` itself for the gap now).
fn book_at(pr: &Prepared, t: usize, p: &Params) -> (Vec<(usize, usize, EdgePair)>, usize) {
    let span = p.months * MONTH_H;
    if t < span + p.mean_h || pr.names.len() < 2 {
        return (Vec::new(), 0);
    }
    let from = t - span;
    let k = pr.names.len();
    let mut tried = 0;
    let mut out: Vec<(usize, usize, EdgePair)> = Vec::new();
    for i in 0..k {
        for j in i + 1..k {
            tried += 1;
            let mut r_min = f64::INFINITY;
            for m in 0..p.months {
                let (s, e) = (from + m * MONTH_H + 1, from + (m + 1) * MONTH_H);
                r_min = r_min.min(corr(&pr.ch[i][s..e], &pr.ch[j][s..e]));
                if r_min < p.min_r {
                    break;
                }
            }
            if r_min < p.min_r {
                continue;
            }
            let r_all = corr(&pr.ch[i][from + 1..t], &pr.ch[j][from + 1..t]);
            let gaps: Vec<f64> = (from.max(p.mean_h)..t)
                .map(|u| gap(pr, i, j, u, p.mean_h))
                .collect();
            let edge = gaps.iter().fold(0.0f64, |m, g| m.max(g.abs()));
            if edge <= 0.0 {
                continue;
            }
            // Separate times the gap reached the entry level.
            let level = p.entry_frac * edge;
            let mut times = 0;
            let mut inside = false;
            for g in &gaps {
                let at = g.abs() >= level;
                if at && !inside {
                    times += 1;
                }
                inside = at;
            }
            let now = gap(pr, i, j, t, p.mean_h);
            let (long, short) = if now > 0.0 {
                (pr.names[j].clone(), pr.names[i].clone())
            } else {
                (pr.names[i].clone(), pr.names[j].clone())
            };
            out.push((
                i,
                j,
                EdgePair {
                    a: pr.names[i].clone(),
                    b: pr.names[j].clone(),
                    r_min,
                    r_all,
                    edge_pct: edge,
                    gap_pct: now,
                    times,
                    long,
                    short,
                },
            ));
        }
    }
    out.sort_by(|x, y| y.2.r_min.total_cmp(&x.2.r_min));
    out.truncate(MAX_PAIRS);
    (out, tried)
}

/// The book now (the last hour of the series).
pub fn book(series: &Series, p: Params, at_ms: i64) -> Book {
    let (pr, n) = prepare(series);
    let (pairs, tried) = if n == 0 {
        (Vec::new(), 0)
    } else {
        book_at(&pr, n - 1, &p)
    };
    let mut pairs: Vec<EdgePair> = pairs.into_iter().map(|x| x.2).collect();
    // Closest to the edge first: what may trade soon.
    pairs.sort_by(|a, b| b.share().total_cmp(&a.share()));
    Book {
        params: Some(p),
        coins: pr.names.len(),
        hours: n,
        tried,
        pairs,
        at_ms,
    }
}

/// The replay's result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeTest {
    pub params: Option<Params>,
    /// Hours tested (after the warm-up) and coins.
    pub hours: usize,
    pub coins: usize,
    pub trades: usize,
    pub wins: usize,
    pub by_target: usize,
    pub by_stop: usize,
    pub by_clock: usize,
    /// Mean and worst net of a basket, % of its dollars; the mean before fees.
    pub mean_pct: f64,
    pub worst_pct: f64,
    pub mean_gross_pct: f64,
    pub mean_hours: f64,
    /// Baskets opened in the older two thirds and the newest third.
    pub older_pct: f64,
    pub older_trades: usize,
    pub newer_pct: f64,
    pub newer_trades: usize,
}

impl EdgeTest {
    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }
    /// The same bar as the other rules' guard (`crate::replay`).
    pub fn passes(&self) -> bool {
        self.newer_trades >= crate::replay::MIN_VERDICT_TRADES
            && self.newer_pct >= crate::replay::MIN_EDGE_PCT
            && self.mean_pct > 0.0
    }
}

struct Open {
    i: usize,
    j: usize,
    /// +1: long i / short j; -1: the other way.
    dir: f64,
    t0: usize,
    edge: f64,
}

/// Replay the rule with `p` on hourly `series`.
pub fn replay(series: &Series, p: Params) -> EdgeTest {
    let (pr, n) = prepare(series);
    let warm = warmup(&p);
    let mut out = EdgeTest {
        params: Some(p),
        coins: pr.names.len(),
        hours: n.saturating_sub(warm),
        ..Default::default()
    };
    if n <= warm + 1 || pr.names.len() < 2 {
        return out;
    }
    let cut = warm + (n - warm) * 2 / 3;
    let mut open: Vec<Open> = Vec::new();
    let mut book: Vec<(usize, usize, EdgePair)> = Vec::new();
    // (net, gross, opened, how, hours)
    let mut done: Vec<(f64, f64, usize, u8, usize)> = Vec::new();
    for t in warm..n {
        // Follow what is open.
        open.retain(|o| {
            let g = gap(&pr, o.i, o.j, t, p.mean_h);
            // Long i / short j gains when the gap (i over j) rises.
            let ri = pr.ln[o.i][t] - pr.ln[o.i][o.t0];
            let rj = pr.ln[o.j][t] - pr.ln[o.j][o.t0];
            let gross = o.dir * ((ri.exp() - 1.0) - (rj.exp() - 1.0)) / 2.0 * 100.0;
            // The gap seen from the side traded: positive while still open.
            let side = -o.dir * g;
            let how = if side <= p.exit_frac * o.edge {
                0
            } else if side >= p.stop_frac * o.edge {
                1
            } else if t - o.t0 >= p.max_hold_h {
                2
            } else {
                return true;
            };
            let out_bps = if how == 1 { TAKER_BPS } else { MAKER_BPS };
            let net = gross - (MAKER_BPS + out_bps) / 100.0;
            done.push((net, gross, o.t0, how, t - o.t0));
            false
        });
        if (t - warm).is_multiple_of(RESELECT_H) {
            book = book_at(&pr, t, &p).0;
        }
        if t + 1 >= n {
            break;
        }
        for (i, j, e) in &book {
            if open.len() >= p.max_open {
                break;
            }
            if open
                .iter()
                .any(|o| [o.i, o.j].iter().any(|c| c == i || c == j))
            {
                continue;
            }
            let g = gap(&pr, *i, *j, t, p.mean_h);
            if g.abs() < p.entry_frac * e.edge_pct || g.abs() >= p.stop_frac * e.edge_pct {
                continue;
            }
            open.push(Open {
                i: *i,
                j: *j,
                dir: if g > 0.0 { -1.0 } else { 1.0 },
                t0: t,
                edge: e.edge_pct,
            });
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
    out.mean_hours = mean(&done.iter().map(|d| d.4 as f64).collect::<Vec<_>>());
    out.older_pct = mean(&older);
    out.older_trades = older.len();
    out.newer_pct = mean(&newer);
    out.newer_trades = newer.len();
    out
}

/// The settings tried: ties of 0.8 / 0.7 / 0.6, entry at 100 / 90 / 80 % of
/// the edge, leaving at the mean or at a quarter of the edge, the stop at
/// 1.5 or 2 times the edge.
pub fn grid() -> Vec<Params> {
    let mut v = Vec::new();
    for min_r in [0.8, 0.7, 0.6] {
        for entry_frac in [1.0, 0.9, 0.8] {
            for exit_frac in [0.0, 0.25] {
                for stop_frac in [1.5, 2.0] {
                    v.push(Params {
                        min_r,
                        entry_frac,
                        exit_frac,
                        stop_frac,
                        ..Params::default()
                    });
                }
            }
        }
    }
    v
}

/// The report: the default, every setting of [`grid`], the best chosen on
/// its older two thirds (its newest third is the honest result).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeReport {
    pub default: EdgeTest,
    pub tried: Vec<EdgeTest>,
    pub best: Option<EdgeTest>,
    pub at_ms: i64,
}

impl EdgeReport {
    /// The setting to trade with: the best, when it passed on unseen hours.
    pub fn chosen(&self) -> Option<&EdgeTest> {
        self.best.as_ref().filter(|b| b.passes())
    }
}

pub fn report(series: &Series, at_ms: i64) -> EdgeReport {
    let tried: Vec<EdgeTest> = grid().into_iter().map(|p| replay(series, p)).collect();
    let best = tried
        .iter()
        .filter(|x| x.older_trades >= crate::replay::MIN_TUNE_TRADES)
        .max_by(|a, b| a.older_pct.total_cmp(&b.older_pct))
        .cloned();
    let default = tried
        .iter()
        .find(|x| x.params == Some(Params::default()))
        .cloned()
        .unwrap_or_else(|| replay(series, Params::default()));
    EdgeReport {
        default,
        tried,
        best,
        at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 100_000) as f64 / 100_000.0 - 0.5
        }
    }

    /// Two coins on one walk with a slow mean-reverting gap (tied), and a
    /// third on its own walk.
    fn market(n: usize, seed: u64) -> Vec<(String, Vec<f64>)> {
        let mut r = Rng(seed);
        let (mut common, mut free, mut gap) = (100.0f64, 50.0f64, 0.0f64);
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..n {
            common *= 1.0 + r.next() * 0.02;
            free *= 1.0 + r.next() * 0.02;
            gap = gap * 0.98 + r.next() * 0.01;
            a.push(common * (1.0 + gap));
            b.push(common * 0.5);
            c.push(free);
        }
        vec![
            ("AUSDT".into(), a),
            ("BUSDT".into(), b),
            ("CUSDT".into(), c),
        ]
    }

    #[test]
    fn the_book_keeps_only_tied_pairs_and_knows_their_edge() {
        let s = market(3000, 5);
        let b = book(&s, Params::default(), 1);
        assert_eq!(b.pairs.len(), 1, "{b:?}");
        let e = &b.pairs[0];
        assert_eq!((e.a.as_str(), e.b.as_str()), ("AUSDT", "BUSDT"));
        assert!(e.r_min >= 0.8 && e.edge_pct > 0.0 && e.times >= 1, "{e:?}");
        assert!(e.share() <= 1.5);
        // The rich leg is sold.
        if e.gap_pct > 0.0 {
            assert_eq!((e.short.as_str(), e.long.as_str()), ("AUSDT", "BUSDT"));
        }
        let x = e.exit(&Params::default());
        assert!(x.target_pct.unwrap() > 0.0 && x.stop_pct.unwrap() > 0.0);
        assert_eq!(x.max_minutes, Some(336 * 60));
    }

    #[test]
    fn a_tied_pair_pays_at_its_edge_and_the_replay_splits_by_time() {
        let s = market(5000, 9);
        // A three months' widest gap rarely comes back: test the mechanics at 60 %.
        let p = Params {
            entry_frac: 0.6,
            ..Params::default()
        };
        let t = replay(&s, p);
        assert!(t.trades > 0, "{t:?}");
        assert_eq!(t.trades, t.by_target + t.by_stop + t.by_clock);
        assert_eq!(t.trades, t.older_trades + t.newer_trades);
        assert!(
            t.mean_gross_pct > 0.0,
            "a mean-reverting gap pays before fees: {t:?}"
        );
        let r = report(&s, 3);
        assert_eq!(r.tried.len(), grid().len());
        assert_eq!(r.default.params, Some(Params::default()));
        if let Some(b) = &r.best {
            assert!(b.older_trades >= crate::replay::MIN_TUNE_TRADES);
        }
    }

    #[test]
    fn too_little_history_finds_nothing() {
        let s = market(500, 1);
        assert!(book(&s, Params::default(), 0).pairs.is_empty());
        assert_eq!(replay(&s, Params::default()).trades, 0);
    }
}
