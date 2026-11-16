//! Long on negative funding, taken apart: the price part and the funding
//! part of each trade, the median, and stops of 10 / 20 % (hourly lows).
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example funding_long_probe -- 150 200
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
fn median(v: &[f64]) -> f64 {
    let mut x = v.to_vec();
    x.sort_by(f64::total_cmp);
    x.get(x.len() / 2).copied().unwrap_or(0.0)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(150);
    let days: i64 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(200);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // "all": every USDT perpetual the catalogue knows, delisted ones too (no
    // survivors' bias); else the `n` busiest trading now.
    let all = a.get(2).is_some_and(|x| x == "all");
    let coins: Vec<String> = marketdb::with(|c| {
        if all {
            c.prepare("SELECT symbol FROM instruments WHERE venue = 'BINANCEF' AND kind = 'perp' AND asset = 'crypto' AND symbol LIKE '%USDT'")
                .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>()))
                .unwrap_or_default()
                .into_iter()
                .filter(|s| dos::scanner::tradable(s))
                .collect()
        } else {
            marketdb::busiest(c, "BINANCEF", "perp", "crypto", n * 2)
                .into_iter()
                .filter(|s| s.ends_with("USDT") && dos::scanner::tradable(s))
                .take(n)
                .collect()
        }
    })
    .unwrap_or_default();
    let hours = (days * 24) as u32;
    let mut data = Vec::new();
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
        data.push((s.clone(), c, f));
    }
    let all_ts: Vec<i64> = data
        .iter()
        .flat_map(|d| d.1.iter().map(|x| x.time_ms))
        .collect();
    let (first, last) = (
        all_ts.iter().copied().min().unwrap_or(0),
        all_ts.iter().copied().max().unwrap_or(0),
    );
    println!(
        "LONG on negative funding, {} perpetuals{}, {days} days; % after 0.10% fees",
        data.len(),
        if all { " (delisted ones included)" } else { "" }
    );
    for thr in [0.0003, 0.0005] {
        for h in [8usize, 24] {
            for stop in [None, Some(20.0), Some(10.0)] {
                // (net, price part, funding part, ts, coin)
                let mut res: Vec<(f64, f64, f64, i64, String)> = Vec::new();
                for (s, c, f) in &data {
                    let mut free = 0i64;
                    for &(ts, rate) in f {
                        if rate > -thr || ts < free {
                            continue;
                        }
                        let Some(i) = c.iter().position(|x| x.time_ms >= ts) else {
                            break;
                        };
                        if i + h >= c.len() {
                            break;
                        }
                        // Not trading any more (a delisted contract's flat candles).
                        if c[i].volume <= 0.0 {
                            continue;
                        }
                        let entry = c[i].open;
                        let mut exit = c[i + h - 1].close;
                        let mut j_end = i + h - 1;
                        if let Some(sp) = stop {
                            let level = entry * (1.0 - sp / 100.0);
                            if let Some(j) = (i..i + h).find(|&j| c[j].low <= level) {
                                exit = level.min(c[j].open);
                                j_end = j;
                            }
                        }
                        let t_end = c[j_end].time_ms + 3_600_000;
                        let price = (exit / entry - 1.0) * 100.0;
                        let fund: f64 = f
                            .iter()
                            .filter(|(ft, _)| *ft > c[i].time_ms && *ft <= t_end)
                            .map(|x| -x.1 * 100.0)
                            .sum();
                        res.push((price + fund - FEES, price, fund, ts, s.clone()));
                        free = t_end;
                    }
                }
                let nets: Vec<f64> = res.iter().map(|r| r.0).collect();
                let q: Vec<String> = (0..4)
                    .map(|qi| {
                        let (lo, hi) = (
                            first + (last - first) * qi / 4,
                            first + (last - first) * (qi + 1) / 4,
                        );
                        let x: Vec<f64> = res
                            .iter()
                            .filter(|r| r.3 >= lo && r.3 <= hi)
                            .map(|r| r.0)
                            .collect();
                        format!("{:+.2}/{:+.2}", mean(&x), median(&x))
                    })
                    .collect();
                let mut coins_n: Vec<&String> = res.iter().map(|r| &r.4).collect();
                coins_n.sort();
                coins_n.dedup();
                let worst = nets.iter().copied().fold(f64::MAX, f64::min);
                println!("  funding <=-{:.2}% hold {:>2}h stop {:<5}: {:>4} trades on {:>3} coins, mean {:+.2}% (price {:+.2}, funding {:+.2}), median {:+.2}%, worst {:+.1} | quarters mean/median {}",
                    thr * 100.0, h, stop.map_or("none".into(), |x| format!("-{x}%")), nets.len(), coins_n.len(), mean(&nets),
                    mean(&res.iter().map(|r| r.1).collect::<Vec<_>>()), mean(&res.iter().map(|r| r.2).collect::<Vec<_>>()),
                    median(&nets), worst, q.join(" "));
            }
        }
    }
}
