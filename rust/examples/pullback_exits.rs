#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
//! The same pullback entries, other ways out: fixed target, trailing stops,
//! breakeven + trail, a ladder of takes, a BTC hedge instead of the stop.
//! Perpetual fees (taker 5 / maker 2 bps); 5-minute futures candles.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example pullback_exits -- 120 13

use std::collections::HashMap;

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::markets::Ohlc;
use dos::pullback::{self, Params};

const TAKER: f64 = 0.05;
const MAKER: f64 = 0.02;

#[derive(Clone, Copy)]
enum Exit {
    Target(f64),
    Trail(f64),
    BreakevenTrail(f64),
    /// Thirds: at 3R, at +10 %, the rest trailing `x` % (stop to entry after the first).
    Ladder(f64),
    /// Half at 3R, half trailing `x` %.
    Half(f64),
    NoStopTrail(f64),
    HedgeBtc(f64),
}

fn name(e: Exit) -> String {
    match e {
        Exit::Target(r) => format!("target {r}R + stop"),
        Exit::Trail(x) => format!("trail {x}% + stop"),
        Exit::BreakevenTrail(x) => format!("+2R -> breakeven, trail {x}%"),
        Exit::Ladder(x) => format!("ladder 1/3 @3R, 1/3 @+10%, 1/3 trail {x}%"),
        Exit::Half(x) => format!("1/2 @3R, 1/2 trail {x}%"),
        Exit::NoStopTrail(x) => format!("NO stop, trail {x}%"),
        Exit::HedgeBtc(x) => format!("BTC hedge at stop, trail {x}%"),
    }
}

/// Net % of one trade from bar t (entry at `entry`), at most `max` bars.
fn simulate(
    c: &[Ohlc],
    t: usize,
    entry: f64,
    stop: f64,
    e: Exit,
    max: usize,
    btc: &HashMap<i64, f64>,
) -> f64 {
    let risk = entry - stop;
    let end = (t + max).min(c.len() - 1);
    let mut peak = entry.max(c[t].high);
    match e {
        Exit::Target(r) => {
            let tg = entry + r * risk;
            for k in t..=end {
                if c[k].low <= stop {
                    return (stop.min(c[k].open) / entry - 1.0) * 100.0 - 2.0 * TAKER;
                }
                if k > t && c[k].high >= tg {
                    return (tg / entry - 1.0) * 100.0 - TAKER - MAKER;
                }
            }
            (c[end].close / entry - 1.0) * 100.0 - 2.0 * TAKER
        }
        Exit::Trail(x) | Exit::BreakevenTrail(x) | Exit::NoStopTrail(x) => {
            let mut sl = if matches!(e, Exit::NoStopTrail(_)) {
                0.0
            } else {
                stop
            };
            for k in t..=end {
                if k > t {
                    let trail = peak * (1.0 - x / 100.0);
                    let armed = match e {
                        Exit::BreakevenTrail(_) => peak >= entry + 2.0 * risk,
                        _ => true,
                    };
                    let level = if armed {
                        sl.max(trail).max(if matches!(e, Exit::BreakevenTrail(_)) {
                            entry
                        } else {
                            0.0
                        })
                    } else {
                        sl
                    };
                    if c[k].low <= level {
                        return (level.min(c[k].open) / entry - 1.0) * 100.0 - 2.0 * TAKER;
                    }
                    sl = level;
                }
                peak = peak.max(c[k].high);
            }
            (c[end].close / entry - 1.0) * 100.0 - 2.0 * TAKER
        }
        Exit::Ladder(x) | Exit::Half(x) => {
            let half = matches!(e, Exit::Half(_));
            let parts = if half { 2.0 } else { 3.0 };
            let (t1, t2) = (
                entry + 3.0 * risk,
                if half { f64::INFINITY } else { entry * 1.10 },
            );
            let mut left = parts as i32; // parts held
            let mut pnl = 0.0;
            let mut sl = stop;
            let mut took1 = false;
            let mut took2 = false;
            for k in t..=end {
                if k > t {
                    let trail = if took1 {
                        (peak * (1.0 - x / 100.0)).max(entry)
                    } else {
                        0.0
                    };
                    let level = sl.max(trail);
                    if c[k].low <= level {
                        pnl += left as f64 / parts
                            * ((level.min(c[k].open) / entry - 1.0) * 100.0 - TAKER);
                        return pnl - TAKER;
                    }
                    sl = level;
                    if !took1 && c[k].high >= t1 {
                        took1 = true;
                        left -= 1;
                        pnl += ((t1 / entry - 1.0) * 100.0 - MAKER) / parts;
                        sl = sl.max(entry);
                    }
                    if !took2 && c[k].high >= t2 {
                        took2 = true;
                        left -= 1;
                        pnl += ((t2 / entry - 1.0) * 100.0 - MAKER) / parts;
                    }
                }
                peak = peak.max(c[k].high);
            }
            pnl + left as f64 / parts * ((c[end].close / entry - 1.0) * 100.0 - TAKER) - TAKER
        }
        Exit::HedgeBtc(x) => {
            // At the stop a BTC short of the same dollars; both closed by the trail on the coin or the clock.
            let mut hedge: Option<f64> = None;
            for k in t..=end {
                if k > t {
                    if hedge.is_none() && c[k].low <= stop {
                        if let Some(b) = btc.get(&c[k].time_ms) {
                            hedge = Some(*b);
                        }
                    }
                    let trail = peak * (1.0 - x / 100.0);
                    if c[k].low <= trail && trail > entry {
                        let h = hedge
                            .and_then(|b0| btc.get(&c[k].time_ms).map(|b| -(b / b0 - 1.0) * 100.0))
                            .unwrap_or(0.0);
                        return (trail.min(c[k].open) / entry - 1.0) * 100.0 + h
                            - 2.0 * TAKER
                            - if hedge.is_some() { 2.0 * TAKER } else { 0.0 };
                    }
                }
                peak = peak.max(c[k].high);
            }
            let h = hedge
                .and_then(|b0| btc.get(&c[end].time_ms).map(|b| -(b / b0 - 1.0) * 100.0))
                .unwrap_or(0.0);
            (c[end].close / entry - 1.0) * 100.0 + h
                - 2.0 * TAKER
                - if hedge.is_some() { 2.0 * TAKER } else { 0.0 }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(120);
    let days: u32 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(13);
    let p = Params::default();
    let coins: Vec<String> = marketdb::with(|c| {
        marketdb::busiest(c, "BINANCE", "spot", "crypto", n * 3)
            .into_iter()
            .filter(|s| {
                s.ends_with("USDT")
                    && dos::scanner::tradable(s)
                    && marketdb::find_instrument(c, "BINANCEF", "perp", s).is_some()
            })
            .take(n)
            .collect()
    })
    .unwrap_or_default();
    let bars = days * 1440 / p.tf_min;
    let btc: HashMap<i64, f64> =
        binance::fetch_klines_stored("BTCUSDT", p.interval(), bars, Venue::Futures)
            .unwrap_or_default()
            .iter()
            .map(|k| (k.time_ms, k.close))
            .collect();
    let max = 3 * (1440 / p.tf_min) as usize;
    let exits = [
        Exit::Target(3.0),
        Exit::Trail(3.0),
        Exit::Trail(5.0),
        Exit::Trail(8.0),
        Exit::Trail(12.0),
        Exit::BreakevenTrail(5.0),
        Exit::BreakevenTrail(8.0),
        Exit::BreakevenTrail(12.0),
        Exit::Ladder(8.0),
        Exit::Ladder(12.0),
        Exit::Ladder(15.0),
        Exit::Half(12.0),
        Exit::Half(15.0),
        Exit::NoStopTrail(8.0),
        Exit::HedgeBtc(8.0),
    ];
    // Per exit: (net, entry ms)
    let mut res: Vec<Vec<(f64, i64)>> = vec![Vec::new(); exits.len()];
    let (mut first, mut last) = (i64::MAX, 0i64);
    for s in &coins {
        let Ok(c) = binance::fetch_klines_stored(s, p.interval(), bars, Venue::Futures) else {
            continue;
        };
        if let (Some(f), Some(l)) = (c.first(), c.last()) {
            first = first.min(f.time_ms);
            last = last.max(l.time_ms);
        }
        let mut t = 30;
        while t + 1 < c.len() {
            let Some(st) = pullback::analyze(&c[t - 30..=t], &p) else {
                t += 1;
                continue;
            };
            if !st.triggered {
                t += 1;
                continue;
            }
            let entry = st.trigger.max(c[t].open);
            for (i, e) in exits.iter().enumerate() {
                res[i].push((simulate(&c, t, entry, st.stop, *e, max, &btc), c[t].time_ms));
            }
            t += 6;
        }
    }
    let cut = first + (last - first) * 2 / 3;
    println!(
        "{} entries, {} coins, {} days; hold up to 3 days; % of the position after perp fees",
        res[0].len(),
        coins.len(),
        days
    );
    for (i, e) in exits.iter().enumerate() {
        let v = &res[i];
        let m = |f: &dyn Fn(&(f64, i64)) -> bool| {
            let x: Vec<f64> = v.iter().filter(|r| f(r)).map(|r| r.0).collect();
            (x.iter().sum::<f64>() / x.len().max(1) as f64, x.len())
        };
        let (all, _) = m(&|_| true);
        let (o, on) = m(&|r| r.1 < cut);
        let (nw, nn) = m(&|r| r.1 >= cut);
        let wins = v.iter().filter(|r| r.0 > 0.0).count();
        let big = v.iter().filter(|r| r.0 >= 20.0).count();
        let best = v.iter().map(|r| r.0).fold(f64::MIN, f64::max);
        let worst = v.iter().map(|r| r.0).fold(f64::MAX, f64::min);
        let sum: f64 = v.iter().map(|r| r.0).sum();
        let pf = {
            let g: f64 = v.iter().filter(|r| r.0 > 0.0).map(|r| r.0).sum();
            let l: f64 = -v.iter().filter(|r| r.0 < 0.0).map(|r| r.0).sum::<f64>();
            if l > 0.0 {
                g / l
            } else {
                0.0
            }
        };
        let top5: f64 = {
            let mut x: Vec<f64> = v.iter().map(|r| r.0).collect();
            x.sort_by(|a, b| b.total_cmp(a));
            x.iter().take(5).sum()
        };
        println!(
            "  {:<40} PF {pf:.2} sum-without-top5 {:+.0}%",
            "",
            sum - top5
        );
        println!("  {:<40} mean {:+.2}%  sum {:+.0}%  won {:>2.0}%  >=20%: {:>2}  best {:+.1}  worst {:+.1} | older {:+.2} ({on}) newest {:+.2} ({nn})",
            name(*e), all, sum, wins as f64 / v.len().max(1) as f64 * 100.0, big, best, worst, o, nw);
    }
}
