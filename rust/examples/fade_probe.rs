//! Short after a pump: the mirror of pullback.  A coin up `min_rise_pct`
//! in 15 min; after the top the first red candle closes; short its
//! perpetual at that close, or on the break of the red candle's low.  The
//! stop above the pump's top; out on a retrace of the rise, a 2R target or
//! a trailing stop; 3 days at most.  Perpetual fees, funding received while
//! short (paid when negative).  Older 2/3 vs newest third, and quarters.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example fade_probe -- 120 60 5
#![allow(
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::too_many_arguments
)]

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::markets::Ohlc;
use dos::pullback::{self, Params};

const TAKER: f64 = 0.05;
const MAKER: f64 = 0.02;
const BUF: f64 = 0.002;

#[derive(Clone, Copy)]
enum Exit {
    Retrace(f64),
    TwoR,
    Trail(f64),
}

fn exit_name(e: Exit) -> String {
    match e {
        Exit::Retrace(x) => format!("retrace {:.0}% of the rise", x * 100.0),
        Exit::TwoR => "target 2R".into(),
        Exit::Trail(x) => format!("trail {x}% from the low"),
    }
}

/// Net % of a short opened at bar t at `entry`, stop `stop` above, the rise's base `base`.
fn short(
    c: &[Ohlc],
    t: usize,
    entry: f64,
    stop: f64,
    base: f64,
    e: Exit,
    max: usize,
    funding: &[(i64, f64)],
) -> f64 {
    let end = (t + max).min(c.len() - 1);
    let risk = stop - entry;
    let target = match e {
        Exit::Retrace(x) => Some(entry - x * (entry - base).max(0.0)),
        Exit::TwoR => Some(entry - 2.0 * risk),
        Exit::Trail(_) => None,
    };
    let mut low = entry.min(c[t].low);
    let fund = |t0: i64, t1: i64| {
        funding
            .iter()
            .filter(|(ft, _)| *ft > t0 && *ft <= t1)
            .map(|x| x.1 * 100.0)
            .sum::<f64>()
    };
    for k in t..=end {
        let bar = &c[k];
        // The stop first (a squeeze).
        if bar.high >= stop {
            let px = stop.max(if k > t { bar.open } else { stop });
            return (1.0 - px / entry) * 100.0 - 2.0 * TAKER + fund(c[t].time_ms, bar.time_ms);
        }
        if k > t {
            if let Some(tg) = target {
                if tg > 0.0 && tg < entry && bar.low <= tg {
                    return (1.0 - tg / entry) * 100.0 - TAKER - MAKER
                        + fund(c[t].time_ms, bar.time_ms);
                }
            }
            if let Exit::Trail(x) = e {
                let level = low * (1.0 + x / 100.0);
                if level < entry * 0.999 && bar.high >= level {
                    return (1.0 - level.max(bar.open.min(level)) / entry) * 100.0 - 2.0 * TAKER
                        + fund(c[t].time_ms, bar.time_ms);
                }
            }
        }
        low = low.min(bar.low);
    }
    (1.0 - c[end].close / entry) * 100.0 - 2.0 * TAKER + fund(c[t].time_ms, c[end].time_ms)
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(120);
    let days: u32 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(60);
    let tf: u32 = a.get(2).and_then(|x| x.parse().ok()).unwrap_or(5);
    let rise: f64 = a.get(3).and_then(|x| x.parse().ok()).unwrap_or(5.0);
    let p = Params {
        tf_min: tf,
        min_rise_pct: rise,
        ..Params::default()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
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
    let bars = (days * 1440 / tf).min(binance::MAX_HISTORY);
    let day = (1440 / tf) as usize;
    let exits = [
        Exit::Retrace(0.5),
        Exit::Retrace(1.0),
        Exit::TwoR,
        Exit::Trail(5.0),
    ];
    // [entry kind][exit] -> (net, ms)
    let mut res: Vec<Vec<Vec<(f64, i64)>>> = vec![vec![Vec::new(); exits.len()]; 2];
    for s in &coins {
        let Ok(c) = binance::fetch_klines_stored(s, p.interval(), bars, Venue::Futures) else {
            continue;
        };
        if c.len() < 100 {
            continue;
        }
        let funding = dos::data::history::funding(s, days as i64 + 2, now);
        let mut seen_red = 0i64;
        let mut t = 31;
        while t + 1 < c.len() {
            let Some(st) = pullback::analyze(&c[t - 30..=t], &p) else {
                t += 1;
                continue;
            };
            if st.red_ms == seen_red {
                t += 1;
                continue;
            }
            seen_red = st.red_ms;
            let Some(ri) = c[..t].iter().rposition(|k| k.time_ms == st.red_ms) else {
                t += 1;
                continue;
            };
            let w = p.window_bars() + 8;
            let top = c[t.saturating_sub(w)..t]
                .iter()
                .map(|k| k.high)
                .fold(0.0, f64::max);
            let base = top / (1.0 + st.rise_pct / 100.0);
            let stop = top * (1.0 + BUF);
            // Entry 1: at the red candle's close (= this bar's open).
            let e1 = c[t].open;
            for (j, e) in exits.iter().enumerate() {
                if e1 < stop {
                    res[0][j].push((
                        short(&c, t, e1, stop, base, *e, 3 * day, &funding),
                        c[t].time_ms,
                    ));
                }
            }
            // Entry 2: the first break of the red candle's low within 6 candles.
            let red_low = c[ri].low;
            if let Some(k) =
                (t..(t + 6).min(c.len() - 1)).find(|&k| c[k].low < red_low && c[k].high < stop)
            {
                let e2 = red_low.min(c[k].open);
                for (j, e) in exits.iter().enumerate() {
                    res[1][j].push((
                        short(&c, k, e2, stop, base, *e, 3 * day, &funding),
                        c[k].time_ms,
                    ));
                }
            }
            t += 1;
        }
    }
    let all: Vec<i64> = res.iter().flatten().flatten().map(|r| r.1).collect();
    let (first, last) = (
        all.iter().copied().min().unwrap_or(0),
        all.iter().copied().max().unwrap_or(0),
    );
    let cut = first + (last - first) * 2 / 3;
    println!("short after a pump: {} coins, {days} days of {tf}m perpetual candles, rise {rise}%+ in 15 min; % of the position after fees and funding", coins.len());
    for (i, entry) in ["at the red candle's close", "on the break of its low"]
        .iter()
        .enumerate()
    {
        for (j, e) in exits.iter().enumerate() {
            let v = &res[i][j];
            let nets: Vec<f64> = v.iter().map(|r| r.0).collect();
            let older: Vec<f64> = v.iter().filter(|r| r.1 < cut).map(|r| r.0).collect();
            let newer: Vec<f64> = v.iter().filter(|r| r.1 >= cut).map(|r| r.0).collect();
            let q: Vec<String> = (0..4)
                .map(|qi| {
                    let (a, b) = (
                        first + (last - first) * qi / 4,
                        first + (last - first) * (qi + 1) / 4,
                    );
                    let x: Vec<f64> = v
                        .iter()
                        .filter(|r| r.1 >= a && r.1 <= b)
                        .map(|r| r.0)
                        .collect();
                    format!("{:+.2}", mean(&x))
                })
                .collect();
            let wins = nets.iter().filter(|x| **x > 0.0).count();
            let worst = nets.iter().copied().fold(f64::MAX, f64::min);
            println!("  {:<26} {:<24} {:>4} trades, won {:>3.0}%, mean {:+.2}%, worst {:+.1} | older {:+.2} ({}) newest {:+.2} ({}) | quarters {}",
                entry, exit_name(*e), nets.len(), wins as f64 / nets.len().max(1) as f64 * 100.0, mean(&nets), worst,
                mean(&older), older.len(), mean(&newer), newer.len(), q.join(" "));
        }
    }
}
