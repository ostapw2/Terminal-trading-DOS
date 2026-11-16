//! Search: ask thousands of questions of the data at once, then let the
//! judge (`crate::judge`) decide which answers are more than chance.
//!
//! 1. **Generate** conditions from families over the given symbols: falls
//!    and rises in a candle, distance from normal (z), volume spikes,
//!    distance from the average, pairs far from their normal ratio, and a
//!    condition on one symbol with another as the target (who drags whom).
//!    Each is asked at several horizons.
//! 2. **Screen** every question on the older part of the history only, on
//!    all cores (the newer part stays unseen).
//! 3. **Judge** the strongest few in full, with the threshold divided by
//!    the number of questions **screened**: picking the best of thousands is
//!    itself a way to find chance, and the count must say so.
//!
//! Pure: aligned data in, a report out.

use std::time::Instant;

use crate::formula::{self, Aligned, Field, Templates};
use crate::judge::{self, Verdict};

/// Symbols a search takes at most.
pub const MAX_SYMBOLS: usize = 10;
/// Horizons asked (candles).
pub const HORIZONS: [usize; 3] = [1, 4, 12];
/// How many of the screened go to the full judge.
pub const TOP: usize = 40;
const OLDER: f64 = 0.7;

/// One question: when `cond` turns true, is `target` higher `h` later?
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub cond: String,
    pub target: String,
    pub h: usize,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub q: Question,
    /// Older part: events and odds minus usual odds.
    pub older_events: usize,
    pub screen_edge: f64,
    pub verdict: Option<Verdict>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub symbols: Vec<String>,
    /// Questions screened (the judge's number of tries).
    pub tested: usize,
    /// Questions with enough older events to rank.
    pub rankable: usize,
    /// The strongest, judged; survivors first.
    pub hits: Vec<Hit>,
    pub survivors: usize,
    pub cores: usize,
    pub elapsed_ms: u128,
}

/// The conditions to ask about, each with the symbols it may target.
/// `funded`: symbols whose funding rate (`FR:`) is in the data.
pub fn questions(symbols: &[String], funded: &[String]) -> Vec<(String, Vec<String>)> {
    let mut v: Vec<(String, Vec<String>)> = Vec::new();
    let all: Vec<String> = symbols.to_vec();
    for s in symbols {
        // Own conditions, any symbol as the target (itself first).
        let mut targets = vec![s.clone()];
        targets.extend(all.iter().filter(|t| *t != s).cloned());
        for x in [0.5, 1.0, 2.0, 3.0] {
            v.push((format!("CHG({s},1) < -{x}"), targets.clone()));
            v.push((format!("CHG({s},1) > {x}"), targets.clone()));
        }
        for p in [90, 95, 99] {
            v.push((format!("RANK(VOL({s}),100) > {p}"), targets.clone()));
        }
        // Distance from normal / from the average: about itself.
        for n in [20, 50, 100] {
            for k in [1.5, 2.0, 2.5] {
                v.push((format!("Z({s},{n}) < -{k}"), vec![s.clone()]));
                v.push((format!("Z({s},{n}) > {k}"), vec![s.clone()]));
            }
        }
        for n in [20, 50] {
            for x in [2, 5] {
                v.push((format!("DEV({s},{n}) < -{x}"), vec![s.clone()]));
                v.push((format!("DEV({s},{n}) > {x}"), vec![s.clone()]));
            }
        }
    }
    // Funding: crowded longs (high), crowded shorts (below zero); any target.
    for s in funded {
        for (op, x) in [(">", "0.03"), (">", "0.05"), ("<", "0"), ("<", "-0.01")] {
            v.push((format!("FR:{s} {op} {x}"), all.clone()));
        }
    }
    // Pairs far from their normal ratio: both legs as targets.
    for (i, a) in symbols.iter().enumerate() {
        for b in &symbols[i + 1..] {
            for n in [50, 100] {
                for k in [2.0, 2.5] {
                    let t = vec![a.clone(), b.clone()];
                    v.push((format!("PAIRZ({a},{b},{n}) < -{k}"), t.clone()));
                    v.push((format!("PAIRZ({a},{b},{n}) > {k}"), t));
                }
            }
        }
    }
    v
}

/// Run a search over `symbols` (already in `aligned`).
pub fn run(
    aligned: &Aligned,
    symbols: &[String],
    templates: &Templates,
    shifts: usize,
) -> Result<Report, String> {
    let started = Instant::now();
    let symbols: Vec<String> = symbols.iter().take(MAX_SYMBOLS).cloned().collect();
    if symbols.is_empty() {
        return Err("no symbols to search".into());
    }
    // Target series once per symbol.
    let mut closes: Vec<(String, Vec<f64>)> = Vec::new();
    for s in &symbols {
        let f = formula::parse_with(&format!("={s}"), templates)?;
        closes.push((s.clone(), f.series(aligned, Field::Close)?));
    }
    let funded: Vec<String> = symbols
        .iter()
        .filter(|s| aligned.has(&formula::Source::Funding((*s).clone())))
        .cloned()
        .collect();
    let qs = questions(&symbols, &funded);
    let cores = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .max(1);
    let chunk = qs.len().div_ceil(cores).max(1);
    // Screen on all cores: (question, older events, edge).
    let screened: Vec<(Question, usize, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = qs
            .chunks(chunk)
            .map(|part| {
                let closes = &closes;
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for (cond, targets) in part {
                        let Ok(f) = formula::parse_with(&format!("={cond}"), templates) else {
                            continue;
                        };
                        let Ok(c) = f.series(aligned, Field::Close) else {
                            continue;
                        };
                        for t in targets {
                            let Some((_, tc)) = closes.iter().find(|(s, _)| s == t) else {
                                continue;
                            };
                            for h in HORIZONS {
                                let (events, edge) = judge::screen(&c, tc, h, OLDER);
                                out.push((
                                    Question {
                                        cond: cond.clone(),
                                        target: t.clone(),
                                        h,
                                    },
                                    events,
                                    edge,
                                ));
                            }
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let tested = screened.len();
    let mut ranked: Vec<&(Question, usize, f64)> = screened
        .iter()
        .filter(|(_, ev, e)| *ev >= judge::MIN_EVENTS && e.is_finite())
        .collect();
    let rankable = ranked.len();
    ranked.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()));
    ranked.truncate(TOP);
    // Judge the strongest in full, on all cores; tries = every question screened.
    let hits: Vec<Hit> = std::thread::scope(|scope| {
        let handles: Vec<_> = ranked
            .chunks(TOP.div_ceil(cores).max(1))
            .map(|part| {
                let closes = &closes;
                scope.spawn(move || {
                    part.iter()
                        .map(|(q, ev, e)| {
                            let verdict = formula::parse_with(&format!("={}", q.cond), templates)
                                .and_then(|f| f.series(aligned, Field::Close))
                                .ok()
                                .and_then(|c| {
                                    let tc = &closes.iter().find(|(s, _)| *s == q.target)?.1;
                                    judge::judge(&c, tc, q.h, OLDER, shifts, tested.max(1)).ok()
                                });
                            Hit {
                                q: q.clone(),
                                older_events: *ev,
                                screen_edge: *e,
                                verdict,
                            }
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
    let mut hits = hits;
    hits.sort_by(|a, b| {
        let s = |h: &Hit| h.verdict.as_ref().is_some_and(|v| v.survives);
        s(b).cmp(&s(a))
            .then(b.screen_edge.abs().total_cmp(&a.screen_edge.abs()))
    });
    let survivors = hits
        .iter()
        .filter(|h| h.verdict.as_ref().is_some_and(|v| v.survives))
        .count();
    Ok(Report {
        symbols,
        tested,
        rankable,
        hits,
        survivors,
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

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 1_000_000) as f64 / 1_000_000.0
        }
    }

    /// Random walks for `syms`; when `plant`, AUSDT's big up-candles are
    /// followed by a rise of BUSDT one candle later.
    fn market(syms: &[&str], n: usize, plant: bool) -> HashMap<Source, Vec<Ohlc>> {
        let mut r = Rng(12345);
        let mut prices: Vec<Vec<f64>> = vec![Vec::with_capacity(n); syms.len()];
        let mut p = vec![100.0; syms.len()];
        for t in 0..n {
            for (i, px) in p.iter_mut().enumerate() {
                *px *= 1.0 + (r.next() - 0.5) * 0.03;
                prices[i].push(*px);
            }
            if plant && t >= 2 {
                let a = &prices[0];
                if a[t - 1] / a[t - 2] - 1.0 > 0.01 {
                    p[1] *= 1.04;
                    *prices[1].last_mut().unwrap() = p[1];
                }
            }
        }
        syms.iter()
            .zip(prices)
            .map(|(s, v)| {
                let candles = v
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| Ohlc {
                        open: c,
                        high: c,
                        low: c,
                        close: c,
                        volume: 100.0 + (i % 7) as f64,
                        time_ms: 1_699_999_200_000 + i as i64 * 3_600_000,
                        ..Default::default()
                    })
                    .collect();
                (Source::Spot(s.to_string()), candles)
            })
            .collect()
    }

    fn search(plant: bool) -> Report {
        let syms = ["AUSDT", "BUSDT", "CUSDT"];
        let data = market(&syms, 3000, plant);
        let sources: Vec<Source> = syms.iter().map(|s| Source::Spot(s.to_string())).collect();
        let al = align(&sources, &data, Timeframe::H1).unwrap();
        let names: Vec<String> = syms.iter().map(|s| s.to_string()).collect();
        run(&al, &names, &builtin_templates(), 200).unwrap()
    }

    #[test]
    fn the_planted_link_is_found_and_noise_gives_nothing() {
        let noise = search(false);
        assert!(noise.tested > 300, "{}", noise.tested);
        assert_eq!(
            noise.survivors,
            0,
            "{:?}",
            noise
                .hits
                .iter()
                .find(|h| h.verdict.as_ref().is_some_and(|v| v.survives))
                .map(|h| &h.q)
        );
        let real = search(true);
        assert!(real.survivors >= 1, "nothing survived");
        let best = &real.hits[0];
        assert!(
            best.q.cond.contains("AUSDT") && best.q.cond.contains(">"),
            "{:?}",
            best.q
        );
        assert_eq!(best.q.target, "BUSDT");
        assert!(best.verdict.as_ref().unwrap().tries == real.tested);
    }

    #[test]
    fn questions_cover_every_family_and_pair() {
        let syms: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        let q = questions(&syms, &["B".to_string()]);
        assert!(q.iter().any(|(c, t)| c == "FR:B < 0" && t.len() == 3));
        assert!(
            !q.iter().any(|(c, _)| c.starts_with("FR:A")),
            "only funded symbols"
        );
        assert!(q
            .iter()
            .any(|(c, t)| c.starts_with("CHG(A,1) > ") && t.contains(&"C".to_string())));
        assert!(q.iter().any(|(c, _)| c.starts_with("PAIRZ(B,C,")));
        assert!(
            !q.iter().any(|(c, _)| c.starts_with("PAIRZ(B,A,")),
            "each pair once"
        );
        assert!(
            q.iter()
                .all(|(c, _)| formula::parse(&format!("={c}")).is_ok()),
            "all parse"
        );
    }
}
