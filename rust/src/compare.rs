//! Comparing series: who leads whom (lead-lag), and how a group moves
//! together (correlation matrix).
//!
//! Both work on candle-to-candle **changes** by default: two rising prices
//! correlate whatever their link, changes do not (levels are offered, and
//! labelled as such).  A correlation of `n` pairs of independent series is
//! within ±1.96/√n 95 % of the time, so that band is drawn and a peak inside
//! it is not called a lead.  Pure: candles in, numbers out.

use std::collections::HashMap;

use crate::markets::Ohlc;

/// Fewest pairs a correlation is computed from.
pub const MIN_PAIRS: usize = 10;
pub const MAX_LAG: usize = 100;

/// What is correlated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// Percent change candle to candle (the default).
    Changes,
    /// The values themselves (trends make this high for unrelated series).
    Levels,
}

impl Basis {
    pub fn label(self) -> &'static str {
        match self {
            Basis::Changes => crate::i18n::pick("changes", "зміни"),
            Basis::Levels => crate::i18n::pick("levels", "рівні"),
        }
    }
}

/// Percent change of each value from the previous one; NaN where there is
/// none (the first value, next to a gap, from zero).
pub fn changes(x: &[f64]) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for i in 1..x.len() {
        let (a, b) = (x[i - 1], x[i]);
        if a.is_finite() && b.is_finite() && a != 0.0 {
            out[i] = (b / a - 1.0) * 100.0;
        }
    }
    out
}

/// Pearson correlation of the pairs where both are numbers; `None` below
/// [`MIN_PAIRS`] or when one side does not move.
pub fn pearson(x: &[f64], y: &[f64]) -> Option<(f64, usize)> {
    let (mut n, mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0usize, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (&a, &b) in x.iter().zip(y) {
        if a.is_finite() && b.is_finite() {
            n += 1;
            sx += a;
            sy += b;
            sxx += a * a;
            syy += b * b;
            sxy += a * b;
        }
    }
    if n < MIN_PAIRS {
        return None;
    }
    let nf = n as f64;
    let cov = sxy / nf - sx / nf * sy / nf;
    let vx = sxx / nf - (sx / nf).powi(2);
    let vy = syy / nf - (sy / nf).powi(2);
    if vx <= 1e-18 || vy <= 1e-18 {
        return None;
    }
    Some(((cov / (vx * vy).sqrt()).clamp(-1.0, 1.0), n))
}

/// The straight line y = intercept + slope * x through the pairs where both
/// are numbers (least squares), with r and the pair count; `None` below
/// [`MIN_PAIRS`] or when x does not move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regression {
    pub slope: f64,
    pub intercept: f64,
    pub r: f64,
    pub n: usize,
}

pub fn regress(x: &[f64], y: &[f64]) -> Option<Regression> {
    let (r, n) = pearson(x, y)?;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (&a, &b) in x.iter().zip(y) {
        if a.is_finite() && b.is_finite() {
            sx += a;
            sy += b;
            sxx += a * a;
            sxy += a * b;
        }
    }
    let nf = n as f64;
    let var = sxx - sx * sx / nf;
    if var <= 1e-18 {
        return None;
    }
    let slope = (sxy - sx * sy / nf) / var;
    Some(Regression {
        slope,
        intercept: (sy - slope * sx) / nf,
        r,
        n,
    })
}

/// Closes of two candle series at the times both have, oldest first.
pub fn align_closes(a: &[Ohlc], b: &[Ohlc]) -> (Vec<f64>, Vec<f64>) {
    let bm: HashMap<i64, f64> = b
        .iter()
        .filter(|k| !k.is_gap && k.time_ms > 0)
        .map(|k| (k.time_ms, k.close))
        .collect();
    let mut pairs: Vec<(i64, f64, f64)> = a
        .iter()
        .filter(|k| !k.is_gap && k.time_ms > 0)
        .filter_map(|k| bm.get(&k.time_ms).map(|&c| (k.time_ms, k.close, c)))
        .collect();
    pairs.sort_by_key(|p| p.0);
    pairs.dedup_by_key(|p| p.0);
    pairs.into_iter().map(|(_, x, y)| (x, y)).unzip()
}

#[derive(Clone, Debug, PartialEq)]
pub struct LeadLag {
    /// `(k, r, pairs)` for k = -max..=max: r = corr(a[t], b[t+k]).  k > 0:
    /// what a does now, b does k candles later (a leads).
    pub points: Vec<(i32, Option<f64>, usize)>,
    /// Pairs at k = 0.
    pub n: usize,
    /// ±1.96/√n: independent series stay inside it 95 % of the time.
    pub band: f64,
    /// The strongest |r| outside the band, if any.
    pub peak: Option<(i32, f64)>,
}

impl LeadLag {
    /// One line: what the curve says, in words.
    pub fn verdict(&self, a: &str, b: &str) -> String {
        match self.peak {
            None => format!(
                "no lead: every |r| is inside ±{:.2} (what unrelated series give, n={})",
                self.band, self.n
            ),
            Some((0, r)) => format!("together: r={r:+.2} at 0, no lead (n={})", self.n),
            Some((k, r)) if k > 0 => {
                format!("{a} leads {b} by {k} candle(s): r={r:+.2} (n={})", self.n)
            }
            Some((k, r)) => format!(
                "{b} leads {a} by {} candle(s): r={r:+.2} (n={})",
                -k, self.n
            ),
        }
    }
}

/// Correlation of `a` with `b` shifted by -max..=max (aligned, same length).
pub fn lead_lag(a: &[f64], b: &[f64], basis: Basis, max: usize) -> LeadLag {
    let (x, y) = match basis {
        Basis::Changes => (changes(a), changes(b)),
        Basis::Levels => (a.to_vec(), b.to_vec()),
    };
    let max = max.min(MAX_LAG) as i32;
    let len = x.len().min(y.len()) as i32;
    let points: Vec<(i32, Option<f64>, usize)> = (-max..=max)
        .map(|k| {
            // a[t] against b[t + k] over every t where both exist.
            let (from, to) = (0.max(-k), len.min(len - k));
            if to - from < MIN_PAIRS as i32 {
                return (k, None, 0);
            }
            let xs = &x[from as usize..to as usize];
            let ys = &y[(from + k) as usize..(to + k) as usize];
            match pearson(xs, ys) {
                Some((r, n)) => (k, Some(r), n),
                None => (k, None, 0),
            }
        })
        .collect();
    let n = points.iter().find(|p| p.0 == 0).map_or(0, |p| p.2);
    let band = if n > 0 { 1.96 / (n as f64).sqrt() } else { 1.0 };
    let peak = points
        .iter()
        .filter_map(|&(k, r, _)| r.map(|r| (k, r)))
        .filter(|(_, r)| r.abs() > band)
        .max_by(|p, q| p.1.abs().total_cmp(&q.1.abs()));
    LeadLag {
        points,
        n,
        band,
        peak,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    pub names: Vec<String>,
    /// `r[i][j]`; `None` where too few common candles.
    pub r: Vec<Vec<Option<f64>>>,
    /// Fewest pairs behind any cell (the matrix's weakest evidence).
    pub min_pairs: usize,
}

/// Correlation of every pair, on candles both have.
pub fn corr_matrix(series: &[(String, Vec<Ohlc>)], basis: Basis) -> Matrix {
    let n = series.len();
    let mut r = vec![vec![None; n]; n];
    let mut min_pairs = usize::MAX;
    for i in 0..n {
        r[i][i] = Some(1.0);
        for j in i + 1..n {
            let (a, b) = align_closes(&series[i].1, &series[j].1);
            let (x, y) = match basis {
                Basis::Changes => (changes(&a), changes(&b)),
                Basis::Levels => (a, b),
            };
            if let Some((v, pairs)) = pearson(&x, &y) {
                r[i][j] = Some(v);
                r[j][i] = Some(v);
                min_pairs = min_pairs.min(pairs);
            }
        }
    }
    Matrix {
        names: series.iter().map(|s| s.0.clone()).collect(),
        r,
        min_pairs: if min_pairs == usize::MAX {
            0
        } else {
            min_pairs
        },
    }
}

/// Average r between two groups of symbols (pairs with no value skipped).
fn link(m: &Matrix, a: &[usize], b: &[usize]) -> Option<f64> {
    let v: Vec<f64> = a
        .iter()
        .flat_map(|&i| b.iter().filter_map(move |&j| m.r[i][j]))
        .collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

impl Matrix {
    /// Groups that move as one: clusters joined while their average r is at
    /// least `min_r` (average linkage), biggest first, only groups of two or
    /// more.  The order lists each group's members together (most alike
    /// neighbours side by side), then the loners, so the heat map shows the
    /// groups as blocks on its diagonal.
    pub fn groups(&self, min_r: f64) -> (Vec<Vec<usize>>, Vec<usize>) {
        let n = self.names.len();
        let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
        loop {
            let mut best: Option<(f64, usize, usize)> = None;
            for i in 0..clusters.len() {
                for j in i + 1..clusters.len() {
                    if let Some(r) = link(self, &clusters[i], &clusters[j]) {
                        if r >= min_r && best.is_none_or(|b| r > b.0) {
                            best = Some((r, i, j));
                        }
                    }
                }
            }
            let Some((_, i, j)) = best else {
                break;
            };
            let moved = clusters.remove(j);
            clusters[i].extend(moved);
        }
        // Inside a group: a chain of nearest neighbours.
        for c in clusters.iter_mut() {
            if c.len() > 2 {
                let mut rest = c.clone();
                let start = (0..rest.len())
                    .max_by(|&a, &b| {
                        let s = |k: usize| link(self, &[rest[k]], &rest).unwrap_or(0.0);
                        s(a).total_cmp(&s(b))
                    })
                    .unwrap_or(0);
                let mut chain = vec![rest.remove(start)];
                while !rest.is_empty() {
                    let last = *chain.last().unwrap();
                    let k = (0..rest.len())
                        .max_by(|&a, &b| {
                            let r = |k: usize| self.r[last][rest[k]].unwrap_or(-2.0);
                            r(a).total_cmp(&r(b))
                        })
                        .unwrap_or(0);
                    chain.push(rest.remove(k));
                }
                *c = chain;
            }
        }
        clusters.sort_by_key(|c| std::cmp::Reverse(c.len()));
        let order: Vec<usize> = clusters.iter().flatten().copied().collect();
        let groups = clusters.into_iter().filter(|c| c.len() >= 2).collect();
        (groups, order)
    }

    /// The same matrix with its symbols in `order`.
    pub fn reordered(&self, order: &[usize]) -> Matrix {
        Matrix {
            names: order.iter().map(|&i| self.names[i].clone()).collect(),
            r: order
                .iter()
                .map(|&i| order.iter().map(|&j| self.r[i][j]).collect())
                .collect(),
            min_pairs: self.min_pairs,
        }
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn symbols_that_move_together_form_groups_side_by_side() {
        // A, C, E move together; B, D together; F alone.
        let names = ["A", "B", "C", "D", "E", "F"];
        let together = |i: usize, j: usize| {
            let g = |k: usize| match k {
                0 | 2 | 4 => 0,
                1 | 3 => 1,
                _ => 2,
            };
            if i == j {
                1.0
            } else if g(i) == g(j) && g(i) < 2 {
                0.8
            } else {
                0.1
            }
        };
        let m = Matrix {
            names: names.iter().map(|s| s.to_string()).collect(),
            r: (0..6)
                .map(|i| (0..6).map(|j| Some(together(i, j))).collect())
                .collect(),
            min_pairs: 100,
        };
        let (groups, order) = m.groups(0.6);
        let mut g: Vec<Vec<usize>> = groups
            .iter()
            .map(|g| {
                let mut v = g.clone();
                v.sort();
                v
            })
            .collect();
        g.sort();
        assert_eq!(g, vec![vec![0, 2, 4], vec![1, 3]]);
        assert_eq!(order.len(), 6);
        assert_eq!(
            &order[..3]
                .iter()
                .map(|&i| names[i])
                .collect::<String>()
                .len(),
            &3
        );
        assert_eq!(*order.last().unwrap(), 5, "the loner last");
        let re = m.reordered(&order);
        assert_eq!(re.names[5], "F");
        assert_eq!(
            re.r[0][1],
            Some(0.8),
            "neighbours on the diagonal are alike"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(seed: u64, n: usize) -> Vec<f64> {
        // A deterministic random walk (xorshift).
        let mut s = seed.max(1);
        let mut p = 100.0;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                p *= 1.0 + ((s % 2001) as f64 / 1000.0 - 1.0) * 0.01;
                p
            })
            .collect()
    }

    fn candles(v: &[f64], t0: i64) -> Vec<Ohlc> {
        v.iter()
            .enumerate()
            .map(|(i, &c)| Ohlc {
                close: c,
                open: c,
                high: c,
                low: c,
                time_ms: t0 + i as i64 * 60_000,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn a_copy_shifted_by_three_is_found_leading_by_three() {
        let a = walk(7, 500);
        // b repeats a's moves 3 candles later.
        let mut b = vec![100.0; 3];
        b.extend_from_slice(&a[..497]);
        let ll = lead_lag(&a, &b, Basis::Changes, 10);
        assert_eq!(ll.points.len(), 21);
        let (k, r) = ll.peak.unwrap();
        assert_eq!(k, 3);
        assert!(r > 0.99, "{r}");
        assert!(ll.verdict("A", "B").starts_with("A leads B by 3"));
        // The other way round: b lags, so B leads A by -3 -> "A leads" flips.
        let back = lead_lag(&b, &a, Basis::Changes, 10);
        assert_eq!(back.peak.unwrap().0, -3);
        assert!(back.verdict("B", "A").starts_with("A leads B by 3"));
    }

    #[test]
    fn unrelated_walks_stay_inside_the_band_and_levels_can_lie() {
        let (a, b) = (walk(1, 600), walk(99, 600));
        let ll = lead_lag(&a, &b, Basis::Changes, 5);
        assert!(ll.band > 0.05 && ll.band < 0.1, "{}", ll.band);
        let outside = ll
            .points
            .iter()
            .filter(|p| p.1.is_some_and(|r| r.abs() > ll.band))
            .count();
        assert!(outside <= 1, "about 5% may stray: {outside} of 11");
        // Two trends correlate in levels whatever their link.
        let up: Vec<f64> = (0..200).map(|i| 100.0 + i as f64).collect();
        let up2: Vec<f64> = (0..200)
            .map(|i| 50.0 + 0.5 * i as f64 + (i % 3) as f64)
            .collect();
        let lv = lead_lag(&up, &up2, Basis::Levels, 0);
        assert!(lv.points[0].1.unwrap() > 0.9);
    }

    #[test]
    fn pearson_and_changes_skip_what_is_not_a_number() {
        assert!((changes(&[100.0, 110.0, f64::NAN, 50.0])[1] - 10.0).abs() < 1e-9);
        assert!(changes(&[100.0, 110.0, f64::NAN, 50.0])[3].is_nan());
        let x: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let mut y = x.clone();
        y[5] = f64::NAN;
        let (r, n) = pearson(&x, &y).unwrap();
        assert!((r - 1.0).abs() < 1e-12 && n == 19);
        assert!(pearson(&x[..5], &y[..5]).is_none(), "too few");
        assert!(pearson(&x, &[3.0; 20]).is_none(), "a flat side");
    }

    #[test]
    fn the_matrix_is_symmetric_and_aligns_by_time() {
        let a = walk(3, 300);
        let b: Vec<f64> = a.iter().map(|v| v * 2.0).collect(); // same changes
        let c = walk(42, 300);
        let series = vec![
            ("A".to_string(), candles(&a, 0)),
            // B starts later: only the common times count.
            ("B".to_string(), candles(&b, 0)[50..].to_vec()),
            ("C".to_string(), candles(&c, 0)),
        ];
        let m = corr_matrix(&series, Basis::Changes);
        assert_eq!(m.r[0][1], m.r[1][0]);
        assert!((m.r[0][1].unwrap() - 1.0).abs() < 1e-9);
        assert!(m.r[0][2].unwrap().abs() < 0.2);
        assert_eq!(m.r[2][2], Some(1.0));
        assert_eq!(m.min_pairs, 249);
        let (x, y) = align_closes(&series[0].1, &series[1].1);
        assert_eq!((x.len(), y.len()), (250, 250));
    }

    #[test]
    fn regression_finds_the_slope_and_refuses_a_flat_x() {
        let x: Vec<f64> = (0..30).map(|i| i as f64 * 0.1 - 1.5).collect();
        let y: Vec<f64> = x.iter().map(|v| 2.0 * v + 0.5).collect();
        let g = regress(&x, &y).unwrap();
        assert!((g.slope - 2.0).abs() < 1e-9 && (g.intercept - 0.5).abs() < 1e-9);
        assert!((g.r - 1.0).abs() < 1e-9 && g.n == 30);
        // NaN pairs are skipped; too few pairs or a flat x gives nothing.
        let mut xn = x.clone();
        xn[3] = f64::NAN;
        assert_eq!(regress(&xn, &y).unwrap().n, 29);
        assert!(regress(&x[..5], &y[..5]).is_none());
        assert!(regress(&vec![1.0; 30], &y).is_none());
    }
}
