//! The search landscape: one condition, two parameters, every setting on a
//! grid, so you see *where* a pattern holds instead of one lucky number.
//!
//! The condition is written with ranges in braces: `Z(BTC,{10..200}) <
//! -{1.0..3.0}` asks the judge's question (is the target higher h candles
//! later?) for every n and every k.  Three maps come back, side by side:
//!
//! - **older** part (where a search would pick the best setting): odds after
//!   the event minus the usual odds, in percentage points;
//! - **newer** part, never used to pick: does the same shape come back?
//! - **chaos**: for each cell, what the same condition shifted in time (the
//!   link to the target broken) reaches 95 % of the time.
//!
//! A real pattern is a plateau that survives on the newer part and stands
//! above the chaos map; a lucky one is a lone peak.  The grid as a whole is
//! checked the way White's reality check does it (Econometrica, 2000): the
//! whole grid is shifted at once, and p is how often the best cell of a
//! meaningless grid is as good as the best real one, so picking the best of
//! many cells is paid for.  The best cell then goes to the full judge with
//! every cell counted as a question.  Pure: aligned data in, a map out.

use std::time::Instant;

use crate::formula::{self, Aligned, Field, Templates};
use crate::judge::{self, Verdict};

/// Values per axis.
pub const STEPS: usize = 9;
const OLDER: f64 = 0.7;

/// One parameter's values.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    /// As written, `10..200`.
    pub range: String,
    pub values: Vec<f64>,
}

/// A condition with one or two `{lo..hi}` ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    /// The text around the ranges: `parts.len() == axes.len() + 1`.
    parts: Vec<String>,
    /// x (the first range), then y when there is a second.
    pub axes: Vec<Axis>,
}

/// A number as a formula writes it: `200`, `1.25`, `0.5`.
pub fn num(v: f64) -> String {
    if v == v.round() && v.abs() < 1e12 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn axis(range: &str) -> Result<Axis, String> {
    let bad = || format!("'{{{range}}}': write a range as {{lo..hi}}, e.g. {{10..200}}");
    let (lo, hi) = range.split_once("..").ok_or_else(bad)?;
    let (lo, hi): (f64, f64) = (
        lo.trim().parse().map_err(|_| bad())?,
        hi.trim().parse().map_err(|_| bad())?,
    );
    if !(lo.is_finite() && hi.is_finite()) || lo >= hi {
        return Err(format!("'{{{range}}}': the range must go up (lo < hi)"));
    }
    // Whole numbers unless a decimal point is written: `{10..200}` is a
    // window, `{1.0..3.0}` a threshold.
    let ints = range.split("..").all(|p| !p.trim().contains('.'));
    // Wide positive ranges (a window of 10 to 200) step by ratio, so the
    // short end is not skipped; narrow ones step evenly.
    let geometric = lo > 0.0 && hi / lo >= 8.0;
    let mut values: Vec<f64> = (0..STEPS)
        .map(|i| {
            let f = i as f64 / (STEPS - 1) as f64;
            let v = if geometric {
                lo * (hi / lo).powf(f)
            } else {
                lo + (hi - lo) * f
            };
            if ints {
                v.round()
            } else {
                (v * 100.0).round() / 100.0
            }
        })
        .collect();
    values.dedup();
    Ok(Axis {
        range: range.replace(' ', ""),
        values,
    })
}

impl Grid {
    /// `Z(BTC,{10..200}) < -{1.0..3.0}`: one or two ranges (whole numbers
    /// unless a decimal point is written).
    pub fn parse(text: &str) -> Result<Grid, String> {
        let text = text.trim().trim_start_matches('=');
        let mut parts = Vec::new();
        let mut axes = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let close = rest[open..].find('}').ok_or("a '{' without its '}'")? + open;
            parts.push(rest[..open].to_string());
            axes.push(axis(&rest[open + 1..close])?);
            rest = &rest[close + 1..];
        }
        parts.push(rest.to_string());
        if rest.contains('}') {
            return Err("a '}' without its '{'".into());
        }
        match axes.len() {
            0 => Err("no range: write the parameters to vary as {lo..hi}, e.g. Z(BTC,{10..200}) < -{1.0..3.0}".into()),
            1 | 2 => Ok(Grid { parts, axes }),
            _ => Err("at most two ranges (a map has two axes)".into()),
        }
    }

    pub fn width(&self) -> usize {
        self.axes[0].values.len()
    }

    pub fn height(&self) -> usize {
        self.axes.get(1).map_or(1, |a| a.values.len())
    }

    /// The condition at column `x`, row `y`.
    pub fn at(&self, x: usize, y: usize) -> String {
        let mut s = self.parts[0].clone();
        for (i, a) in self.axes.iter().enumerate() {
            let v = if i == 0 { a.values[x] } else { a.values[y] };
            s.push_str(&num(v));
            s.push_str(&self.parts[i + 1]);
        }
        s
    }

    /// The condition as written, ranges included.
    pub fn text(&self) -> String {
        let mut s = self.parts[0].clone();
        for (i, a) in self.axes.iter().enumerate() {
            s.push_str(&format!("{{{}}}", a.range));
            s.push_str(&self.parts[i + 1]);
        }
        s
    }
}

/// One setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// Older part: events and edge (odds after the event minus usual odds).
    pub older: (usize, f64),
    /// Newer part, the same.
    pub newer: (usize, f64),
    /// What 95 % of shifted conditions stay within, |edge| on the older part.
    pub noise: f64,
}

impl Cell {
    pub fn rankable(&self) -> bool {
        self.older.0 >= judge::MIN_EVENTS
    }

    /// Above its own chaos baseline (older part)?
    pub fn above_noise(&self) -> bool {
        self.rankable() && self.older.1.abs() > self.noise
    }
}

#[derive(Clone, Debug)]
pub struct Landscape {
    pub grid: Grid,
    pub target: String,
    pub horizon: usize,
    /// `cells[y][x]`; `None` where the condition could not be computed.
    pub cells: Vec<Vec<Option<Cell>>>,
    /// The cell a search would pick: the largest older |edge| with enough events.
    pub best: Option<(usize, usize)>,
    /// How often the best cell of the whole grid, shifted at once, is at
    /// least as good as the real best; and how many shifts.
    pub p_best: f64,
    pub shifts: usize,
    /// Of the best's 8 neighbours with enough events, how many keep its sign
    /// and at least half its edge.
    pub plateau: (usize, usize),
    /// Cells with enough events on both parts, and how many agree in sign.
    pub agree: (usize, usize),
    /// The full judge on the best cell (every cell counted as a question).
    pub verdict: Option<Verdict>,
    pub cores: usize,
    pub elapsed_ms: u128,
}

impl Landscape {
    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        self.cells.get(y)?.get(x)?.as_ref()
    }

    /// The shape of the best cell's neighbourhood, in words.
    pub fn shape(&self) -> String {
        use crate::i18n::pick;
        let (k, m) = self.plateau;
        if self.best.is_none() {
            pick(
                "no cell has enough events",
                "ні в якої клітинки немає досить подій",
            )
            .into()
        } else if m == 0 {
            pick("no neighbour to compare", "немає сусідів для порівняння").into()
        } else if k * 2 >= m {
            pick(
                format!("a plateau: {k} of {m} neighbours keep half the edge"),
                format!("плато: {k} з {m} сусідів тримають половину ефекту"),
            )
        } else {
            pick(
                format!("a lone peak: {k} of {m} neighbours keep half the edge"),
                format!("одинокий пік: {k} з {m} сусідів тримають половину ефекту"),
            )
        }
    }
}

fn shifts_for(n: usize, h: usize, count: usize) -> Vec<usize> {
    let span = n.saturating_sub(2 * h + 1).max(1);
    let mut seed = 0x2545_f491_4f6c_dd1du64 ^ n as u64;
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            h + 1 + (seed % span as u64) as usize
        })
        .collect()
}

/// Map `grid` against `target` (a symbol or formula) at horizon `h`.
/// `tries`: questions asked before this map (its cells are added).
pub fn run(
    aligned: &Aligned,
    grid: &Grid,
    target: &str,
    h: usize,
    templates: &Templates,
    shifts: usize,
    tries: usize,
) -> Result<Landscape, String> {
    let started = Instant::now();
    let tf = formula::parse_with(&format!("={}", target.trim_start_matches('=')), templates)?;
    let tc = tf.series(aligned, Field::Close)?;
    let n = tc.len();
    if h == 0 || h * 4 >= n {
        return Err(format!(
            "horizon {h} needs more than {} candles (have {n})",
            h * 4
        ));
    }
    let (w, ht) = (grid.width(), grid.height());
    // Every cell's condition series; a cell that fails stays None.
    let mut series: Vec<Option<Vec<f64>>> = Vec::with_capacity(w * ht);
    let mut first_err = None;
    for y in 0..ht {
        for x in 0..w {
            let s = formula::parse_with(&format!("={}", grid.at(x, y)), templates)
                .and_then(|f| f.series(aligned, Field::Close));
            match s {
                Ok(v) if v.len() == n => series.push(Some(v)),
                Ok(_) => series.push(None),
                Err(e) => {
                    first_err.get_or_insert(e);
                    series.push(None)
                }
            }
        }
    }
    if series.iter().all(Option::is_none) {
        return Err(first_err.unwrap_or_else(|| "no cell could be computed".into()));
    }
    let out = judge::outcomes(&tc, h);
    let cut = ((n as f64) * OLDER) as usize;
    // Per cell: (older, newer) events and edge.
    type Parts = ((usize, f64), (usize, f64));
    let real: Vec<Option<Parts>> = series
        .iter()
        .map(|s| {
            s.as_ref().map(|c| {
                (
                    judge::edge_in(c, &out, h, 0, cut),
                    judge::edge_in(c, &out, h, cut, n),
                )
            })
        })
        .collect();
    // The best a search would pick (older part only).
    let best_i = real
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.map(|(o, _)| (i, o)))
        .filter(|(_, o)| o.0 >= judge::MIN_EVENTS)
        .max_by(|a, b| a.1 .1.abs().total_cmp(&b.1 .1.abs()))
        .map(|(i, _)| i);
    let best_edge = best_i.and_then(|i| real[i]).map_or(0.0, |r| r.0 .1.abs());
    // Chaos: the whole grid shifted by the same amount, on all cores.
    let cores = std::thread::available_parallelism()
        .map_or(1, |c| c.get())
        .max(1);
    let offsets = shifts_for(n, h, shifts.max(20));
    let chunk = offsets.len().div_ceil(cores).max(1);
    // Per shift: |edge| per cell (NaN where it has too few events), the max.
    let noise_rows: Vec<(Vec<f64>, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = offsets
            .chunks(chunk)
            .map(|part| {
                let (series, out) = (&series, &out);
                scope.spawn(move || {
                    let mut shifted = vec![0.0; n];
                    part.iter()
                        .map(|&s| {
                            let mut best = 0.0f64;
                            let row: Vec<f64> = series
                                .iter()
                                .map(|c| {
                                    let Some(c) = c else {
                                        return f64::NAN;
                                    };
                                    for (t, v) in shifted.iter_mut().enumerate() {
                                        *v = c[(t + s) % n];
                                    }
                                    let (ev, e) = judge::edge_in(&shifted, out, h, 0, cut);
                                    if ev >= judge::MIN_EVENTS {
                                        best = best.max(e.abs());
                                    }
                                    e.abs()
                                })
                                .collect();
                            (row, best)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let done = noise_rows.len();
    let as_good = noise_rows
        .iter()
        .filter(|(_, m)| best_i.is_some() && *m >= best_edge)
        .count();
    let p_best = if best_i.is_some() {
        (as_good + 1) as f64 / (done + 1) as f64
    } else {
        1.0
    };
    let noise_of = |i: usize| -> f64 {
        let mut v: Vec<f64> = noise_rows
            .iter()
            .map(|(r, _)| r[i])
            .filter(|x| x.is_finite())
            .collect();
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(f64::total_cmp);
        v[((v.len() as f64 * 0.95) as usize).min(v.len() - 1)]
    };
    let cells: Vec<Vec<Option<Cell>>> = (0..ht)
        .map(|y| {
            (0..w)
                .map(|x| {
                    let i = y * w + x;
                    real[i].map(|(older, newer)| Cell {
                        older,
                        newer,
                        noise: noise_of(i),
                    })
                })
                .collect()
        })
        .collect();
    let best = best_i.map(|i| (i % w, i / w));
    let plateau = match best {
        Some((bx, by)) => {
            let e = cells[by][bx].as_ref().map_or(0.0, |c| c.older.1);
            let mut kept = 0;
            let mut seen = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (x, y) = (bx as i32 + dx, by as i32 + dy);
                    if (dx, dy) == (0, 0) || x < 0 || y < 0 || x >= w as i32 || y >= ht as i32 {
                        continue;
                    }
                    if let Some(c) = cells[y as usize][x as usize]
                        .as_ref()
                        .filter(|c| c.rankable())
                    {
                        seen += 1;
                        if c.older.1.signum() == e.signum() && c.older.1.abs() * 2.0 >= e.abs() {
                            kept += 1;
                        }
                    }
                }
            }
            (kept, seen)
        }
        None => (0, 0),
    };
    let both: Vec<&Cell> = cells
        .iter()
        .flatten()
        .flatten()
        .filter(|c| c.older.0 >= judge::MIN_EVENTS && c.newer.0 >= judge::MIN_EVENTS)
        .collect();
    let agree = (
        both.iter()
            .filter(|c| c.older.1.signum() == c.newer.1.signum())
            .count(),
        both.len(),
    );
    let cell_count = series.iter().filter(|s| s.is_some()).count();
    let verdict = best_i.and_then(|i| {
        let c = series[i].as_ref()?;
        judge::judge(c, &tc, h, OLDER, shifts, tries + cell_count).ok()
    });
    Ok(Landscape {
        grid: grid.clone(),
        target: target.to_string(),
        horizon: h,
        cells,
        best,
        p_best,
        shifts: done,
        plateau,
        agree,
        verdict,
        cores,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::{align, builtin_templates, Source};
    use crate::markets::{Ohlc, Timeframe};
    use std::collections::HashMap;

    #[test]
    fn grids_parse_step_and_fill_in() {
        let g = Grid::parse("=Z(BTC,{10..200}) < -{1.0..3.0}").unwrap();
        assert_eq!((g.width(), g.height()), (STEPS, STEPS));
        let n = &g.axes[0].values;
        assert_eq!((n[0], n[STEPS - 1]), (10.0, 200.0));
        // 10..200 is wide: steps by ratio, the short end is dense.
        assert!(n[1] < 20.0, "{n:?}");
        let k = &g.axes[1].values;
        assert_eq!(k[1], 1.25, "narrow steps evenly: {k:?}");
        assert_eq!(g.at(0, 0), "Z(BTC,10) < -1");
        assert_eq!(g.at(STEPS - 1, 1), "Z(BTC,200) < -1.25");
        assert_eq!(g.text(), "Z(BTC,{10..200}) < -{1.0..3.0}");
        // Whole numbers unless a point is written; no repeats.
        assert_eq!(Grid::parse("Z(A,20) < -{1..3}").unwrap().width(), 3);
        // One range: a single row.
        let one = Grid::parse("CHG(ETH,1) > {0.5..3}").unwrap();
        assert_eq!(
            (one.height(), one.at(0, 0).as_str()),
            (1, "CHG(ETH,1) > 0.5")
        );
        // Small integer ranges do not repeat values.
        assert_eq!(Grid::parse("Z(A,{1..4})").unwrap().width(), 4);
        for bad in [
            "Z(BTC,20)",
            "Z(BTC,{20})",
            "Z(B,{5..1})",
            "{1..2}{1..2}{1..2}",
            "Z({1..2",
        ] {
            assert!(Grid::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(2.0), "2");
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 1_000_000) as f64 / 1_000_000.0
        }
    }

    /// A random walk for A; when `plant`, every candle that falls more than
    /// 1 % is followed by a 3 % rise.
    fn aligned(n: usize, plant: bool) -> Aligned {
        let mut r = Rng(777);
        let mut p = 100.0;
        let mut v = Vec::with_capacity(n);
        let mut prev = p;
        for _ in 0..n {
            let fell = p / prev - 1.0 < -0.01;
            prev = p;
            p *= 1.0 + (r.next() - 0.5) * 0.03;
            if plant && fell {
                p *= 1.03;
            }
            v.push(p);
        }
        let candles = v
            .iter()
            .enumerate()
            .map(|(i, &c)| Ohlc {
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 100.0,
                time_ms: 1_699_999_200_000 + i as i64 * 3_600_000,
                ..Default::default()
            })
            .collect();
        let s = Source::Spot("AUSDT".into());
        let data = HashMap::from([(s.clone(), candles)]);
        align(&[s], &data, Timeframe::H1).unwrap()
    }

    #[test]
    fn a_planted_rule_is_a_plateau_and_noise_is_not() {
        let t = builtin_templates();
        let g = Grid::parse("CHG(AUSDT,1) < -{0.4..1.4}").unwrap();
        let real = run(&aligned(2500, true), &g, "AUSDT", 1, &t, 100, 0).unwrap();
        let v = real.verdict.as_ref().expect("judged");
        assert!(v.survives, "{}", v.reason);
        assert_eq!(v.tries, g.width(), "every cell is a question");
        assert!(real.p_best < 0.05, "{}", real.p_best);
        let best = real.cell(real.best.unwrap().0, 0).unwrap();
        assert!(best.above_noise() && best.newer.1 > 0.0, "{best:?}");
        assert!(real.agree.0 * 10 >= real.agree.1 * 8, "{:?}", real.agree);
        // Pure noise: the best cell is no better than the best of chaos.
        let noise = run(&aligned(2500, false), &g, "AUSDT", 1, &t, 100, 0).unwrap();
        assert!(!noise.verdict.as_ref().is_some_and(|v| v.survives));
        assert!(noise.p_best > 0.05, "{}", noise.p_best);
        // A formula the data can not give is an error, not an empty map.
        assert!(run(
            &aligned(300, false),
            &Grid::parse("CHG(ZZZ,1) < -{1..2}").unwrap(),
            "AUSDT",
            1,
            &t,
            20,
            0
        )
        .is_err());
    }
}
