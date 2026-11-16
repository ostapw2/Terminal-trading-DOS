//! Extreme funding as a signal: at a settlement whose rate is at least
//! `thr` (crowded longs) short the perpetual; at most -`thr` (crowded
//! shorts) buy it.  Held `h` hours (no stop), entered at the open of the
//! settlement's hour, taker fees in and out (5 bps each), the funding paid
//! or received while held counted.  One trade per coin at a time.  Older
//! 2/3 vs newest third, and quarters.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example funding_probe -- 150 200
#![allow(clippy::type_complexity)]

use dos::data::binance::{self, Venue};
use dos::data::marketdb;

const FEES: f64 = 0.10;

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(150);
    let days: i64 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(200);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let coins: Vec<String> = marketdb::with(|c| {
        marketdb::busiest(c, "BINANCEF", "perp", "crypto", n * 2)
            .into_iter()
            .filter(|s| s.ends_with("USDT") && dos::scanner::tradable(s))
            .take(n)
            .collect()
    })
    .unwrap_or_default();
    let hours = (days * 24) as u32;
    // Per coin: hourly (open time, open, close), and settlements.
    let mut data: Vec<(String, Vec<(i64, f64, f64)>, Vec<(i64, f64)>)> = Vec::new();
    for s in &coins {
        let Ok(c) = binance::fetch_klines_stored(s, "1h", hours, Venue::Futures) else {
            continue;
        };
        if c.len() < 24 * 30 {
            continue;
        }
        let f = dos::data::history::funding(s, days + 2, now);
        if f.is_empty() {
            continue;
        }
        data.push((
            s.clone(),
            c.iter().map(|k| (k.time_ms, k.open, k.close)).collect(),
            f,
        ));
    }
    let all_ts: Vec<i64> = data.iter().flat_map(|d| d.1.iter().map(|x| x.0)).collect();
    let (first, last) = (
        all_ts.iter().copied().min().unwrap_or(0),
        all_ts.iter().copied().max().unwrap_or(0),
    );
    let cut = first + (last - first) * 2 / 3;
    println!("{} perpetuals, {} days of hourly candles and settlements; % of the position after 0.10% fees, funding counted", data.len(), days);
    for side in [-1.0f64, 1.0] {
        for thr in [0.0003, 0.0005, 0.001] {
            for h in [8usize, 24, 72] {
                let mut res: Vec<(f64, i64)> = Vec::new();
                for (_, c, f) in &data {
                    let mut free = 0i64;
                    for &(ts, rate) in f {
                        let hit = if side < 0.0 {
                            rate >= thr
                        } else {
                            rate <= -thr
                        };
                        if !hit || ts < free {
                            continue;
                        }
                        let Some(i) = c.iter().position(|x| x.0 >= ts) else {
                            break;
                        };
                        if i + h >= c.len() {
                            break;
                        }
                        let (entry, exit) = (c[i].1, c[i + h - 1].2);
                        let t_end = c[i + h - 1].0 + 3_600_000;
                        let price = side * (exit / entry - 1.0) * 100.0;
                        // A long pays a positive rate, a short receives it.
                        let fund: f64 = f
                            .iter()
                            .filter(|(ft, _)| *ft > c[i].0 && *ft <= t_end)
                            .map(|x| -side * x.1 * 100.0)
                            .sum();
                        res.push((price + fund - FEES, ts));
                        free = t_end;
                    }
                }
                let nets: Vec<f64> = res.iter().map(|r| r.0).collect();
                let older: Vec<f64> = res.iter().filter(|r| r.1 < cut).map(|r| r.0).collect();
                let newer: Vec<f64> = res.iter().filter(|r| r.1 >= cut).map(|r| r.0).collect();
                let q: Vec<String> = (0..4)
                    .map(|qi| {
                        let (lo, hi) = (
                            first + (last - first) * qi / 4,
                            first + (last - first) * (qi + 1) / 4,
                        );
                        let x: Vec<f64> = res
                            .iter()
                            .filter(|r| r.1 >= lo && r.1 <= hi)
                            .map(|r| r.0)
                            .collect();
                        format!("{:+.2}({})", mean(&x), x.len())
                    })
                    .collect();
                let wins = nets.iter().filter(|x| **x > 0.0).count();
                let worst = nets.iter().copied().fold(f64::MAX, f64::min);
                println!("  {} funding {}{:.2}%  hold {:>2}h: {:>4} trades, won {:>3.0}%, mean {:+.2}%, worst {:+.1} | older {:+.2} ({}) newest {:+.2} ({}) | {}",
                    if side < 0.0 { "SHORT" } else { "LONG " }, if side < 0.0 { ">=" } else { "<=-" }, thr * 100.0, h,
                    nets.len(), wins as f64 / nets.len().max(1) as f64 * 100.0, mean(&nets), worst,
                    mean(&older), older.len(), mean(&newer), newer.len(), q.join(" "));
            }
        }
    }
}
