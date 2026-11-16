//! Which pullback entries became runners?  Every entry of `days` of 5-minute
//! perpetual candles with what was known at that moment (features), its
//! result under "half at 3R, half trailing 15 %", and whether it ran +20 %
//! before the stop.  Then a search for a filter (one feature, then two) on
//! the older two thirds, judged on the newest third it never saw.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example pullback_filters -- 120 60
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

use std::collections::HashMap;

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::markets::Ohlc;
use dos::pullback::{self, Params};

const TAKER: f64 = 0.05;
const MAKER: f64 = 0.02;
const NAMES: [&str; 11] = [
    "rise %",
    "pullback depth (share of rise)",
    "risk to stop %",
    "volume x (rise vs 24h)",
    "coin 24h before %",
    "below 7d high %",
    "pumps in last 24h",
    "BTC 1h %",
    "BTC 24h %",
    "funding (bps/8h)",
    "hour UTC",
];

/// Half at 3R (stop to entry after), half trailing `trail` % from the peak; 3 days at most.
fn half_runner(c: &[Ohlc], t: usize, entry: f64, stop: f64, trail: f64, max: usize) -> (f64, f64) {
    let risk = entry - stop;
    let end = (t + max).min(c.len() - 1);
    let t1 = entry + 3.0 * risk;
    let mut peak = entry.max(c[t].high);
    let mut mfe = peak;
    let (mut pnl, mut left, mut sl, mut took) = (0.0, 2.0, stop, false);
    for k in t..=end {
        if k > t {
            let trail_l = if took {
                (peak * (1.0 - trail / 100.0)).max(entry)
            } else {
                0.0
            };
            let level = sl.max(trail_l);
            if c[k].low <= level {
                pnl += left / 2.0 * ((level.min(c[k].open) / entry - 1.0) * 100.0 - TAKER);
                return (pnl - TAKER, (mfe / entry - 1.0) * 100.0);
            }
            sl = level;
            if !took && c[k].high >= t1 {
                took = true;
                left -= 1.0;
                pnl += ((t1 / entry - 1.0) * 100.0 - MAKER) / 2.0;
                sl = sl.max(entry);
            }
        }
        peak = peak.max(c[k].high);
        mfe = mfe.max(c[k].high);
    }
    (
        pnl + left / 2.0 * ((c[end].close / entry - 1.0) * 100.0 - TAKER) - TAKER,
        (mfe / entry - 1.0) * 100.0,
    )
}

struct Row {
    f: [f64; 11],
    net: f64,
    runner: bool,
    ms: i64,
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
    let p = Params {
        tf_min: tf,
        ..Params::default()
    };
    println!("{}", p.words());
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
    let bars = days * 1440 / p.tf_min;
    let day = (1440 / p.tf_min) as usize;
    let btc: HashMap<i64, f64> =
        binance::fetch_klines_stored("BTCUSDT", p.interval(), bars, Venue::Futures)
            .unwrap_or_default()
            .iter()
            .map(|k| (k.time_ms, k.close))
            .collect();
    let btc_at = |ms: i64| btc.get(&ms).copied();
    let mut rows: Vec<Row> = Vec::new();
    for s in &coins {
        let Ok(c) = binance::fetch_klines_stored(s, p.interval(), bars, Venue::Futures) else {
            continue;
        };
        if c.len() < day * 8 {
            continue;
        }
        let funding = dos::data::history::funding(s, days as i64 + 2, now);
        let mut pumps: Vec<usize> = Vec::new();
        let mut t = day * 7;
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
            let (net, mfe) = half_runner(&c, t, entry, st.stop, 15.0, 3 * day);
            let w = p.window_bars() + 7;
            let vol_rise: f64 =
                c[t - w..t].iter().map(|k| k.volume * k.close).sum::<f64>() / w as f64;
            let vol_day: f64 = c[t - day..t - w]
                .iter()
                .map(|k| k.volume * k.close)
                .sum::<f64>()
                / (day - w) as f64;
            let top = c[t - w..t].iter().map(|k| k.high).fold(0.0, f64::max);
            let low = c[t - 3..t].iter().map(|k| k.low).fold(f64::MAX, f64::min);
            let base = top / (1.0 + st.rise_pct / 100.0);
            let hi7 = c[t - 7 * day..t].iter().map(|k| k.high).fold(0.0, f64::max);
            let ms = c[t].time_ms;
            let btc1 = match (btc_at(ms - 3_600_000), btc_at(ms - 300_000)) {
                (Some(a), Some(b)) => (b / a - 1.0) * 100.0,
                _ => f64::NAN,
            };
            let btc24 = match (btc_at(ms - 86_400_000), btc_at(ms - 300_000)) {
                (Some(a), Some(b)) => (b / a - 1.0) * 100.0,
                _ => f64::NAN,
            };
            let fr = funding
                .iter()
                .rev()
                .find(|(ft, _)| *ft <= ms)
                .map_or(f64::NAN, |x| x.1 * 10_000.0);
            pumps.retain(|&x| t - x < day);
            let f = [
                st.rise_pct,
                if top > base {
                    (top - low) / (top - base)
                } else {
                    f64::NAN
                },
                (entry / st.stop - 1.0) * 100.0,
                if vol_day > 0.0 {
                    vol_rise / vol_day
                } else {
                    f64::NAN
                },
                (c[t - w].close / c[t - day].close - 1.0) * 100.0,
                (1.0 - entry / hi7) * 100.0,
                pumps.len() as f64,
                btc1,
                btc24,
                fr,
                ((ms / 3_600_000) % 24) as f64,
            ];
            pumps.push(t);
            rows.push(Row {
                f,
                net,
                runner: mfe >= 20.0,
                ms,
            });
            t += 6;
        }
    }
    let (first, last) = (
        rows.iter().map(|r| r.ms).min().unwrap_or(0),
        rows.iter().map(|r| r.ms).max().unwrap_or(0),
    );
    let cut = first + (last - first) * 2 / 3;
    let (old, new): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| r.ms < cut);
    let runners = rows.iter().filter(|r| r.runner).count();
    println!("{} entries ({} older, {} newest third), {} runners (+20% before the stop); all: mean {:+.2}%",
        rows.len(), old.len(), new.len(), runners, mean(&rows.iter().map(|r| r.net).collect::<Vec<_>>()));
    // Hypotheses named before this run (from the 60-day 5-minute search):
    // judged on every quarter of this history, nothing fitted here.
    let quarters = |f: &dyn Fn(&Row) -> bool| -> String {
        (0..4)
            .map(|q| {
                let (a, b) = (
                    first + (last - first) * q / 4,
                    first + (last - first) * (q + 1) / 4,
                );
                let v: Vec<f64> = rows
                    .iter()
                    .filter(|r| r.ms >= a && r.ms <= b && f(r))
                    .map(|r| r.net)
                    .collect();
                format!("{:+.2}% ({})", mean(&v), v.len())
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("\nnamed in advance, per quarter of the history:");
    println!(
        "  all entries                       {}",
        quarters(&|_| true)
    );
    println!(
        "  hour UTC <= 7                     {}",
        quarters(&|r| r.f[10] <= 7.0)
    );
    println!(
        "  coin 24h before <= +2%            {}",
        quarters(&|r| r.f[4] <= 2.0)
    );
    println!(
        "  both                              {}",
        quarters(&|r| r.f[10] <= 7.0 && r.f[4] <= 2.0)
    );
    println!(
        "  near 7d high (<= 4% below)        {}",
        quarters(&|r| r.f[5] <= 4.0)
    );
    // Runners vs the rest, per feature (medians), on all rows.
    println!("\nfeature medians: runners | the rest");
    for i in 0..NAMES.len() {
        let med = |pick: bool| {
            let mut v: Vec<f64> = rows
                .iter()
                .filter(|r| r.runner == pick && r.f[i].is_finite())
                .map(|r| r.f[i])
                .collect();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
        };
        println!(
            "  {:<32} {:>8.2} | {:>8.2}",
            NAMES[i],
            med(true),
            med(false)
        );
    }
    // Conditions: feature >= q or <= q at deciles of the older part.
    let mut conds: Vec<(usize, bool, f64)> = Vec::new();
    for i in 0..NAMES.len() {
        let mut v: Vec<f64> = old
            .iter()
            .map(|r| r.f[i])
            .filter(|x| x.is_finite())
            .collect();
        v.sort_by(f64::total_cmp);
        v.dedup();
        for d in 1..10 {
            if let Some(&q) = v.get(v.len() * d / 10) {
                conds.push((i, true, q));
                conds.push((i, false, q));
            }
        }
    }
    let ok = |r: &Row, c: &(usize, bool, f64)| {
        let x = r.f[c.0];
        x.is_finite() && if c.1 { x >= c.2 } else { x <= c.2 }
    };
    let words = |c: &(usize, bool, f64)| {
        format!(
            "{} {} {:.2}",
            NAMES[c.0],
            if c.1 { ">=" } else { "<=" },
            c.2
        )
    };
    let stats = |set: &[&Row]| {
        (
            mean(&set.iter().map(|r| r.net).collect::<Vec<_>>()),
            set.len(),
            set.iter().filter(|r| r.runner).count(),
        )
    };
    let mut tried = 0;
    let mut found: Vec<(f64, String, (f64, usize, usize), (f64, usize, usize))> = Vec::new();
    for c in &conds {
        tried += 1;
        let o: Vec<&Row> = old.iter().copied().filter(|r| ok(r, c)).collect();
        if o.len() < 40 {
            continue;
        }
        let nn: Vec<&Row> = new.iter().copied().filter(|r| ok(r, c)).collect();
        found.push((stats(&o).0, words(c), stats(&o), stats(&nn)));
    }
    // One feature alone: few tries, hard to fit by chance.
    let mut singles = found.clone();
    singles.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("\n{} one-feature filters (40+ older entries); the best 12 on the older part, then the newest third:", singles.len());
    for f in singles.iter().take(12) {
        println!(
            "  older {:+.2}% ({}, {} runners) | newest {:+.2}% ({}, {} runners)  {}",
            f.2 .0, f.2 .1, f.2 .2, f.3 .0, f.3 .1, f.3 .2, f.1
        );
    }
    for (x, cx) in conds.iter().enumerate() {
        for cy in conds.iter().skip(x + 1) {
            if cx.0 == cy.0 {
                continue;
            }
            tried += 1;
            let o: Vec<&Row> = old
                .iter()
                .copied()
                .filter(|r| ok(r, cx) && ok(r, cy))
                .collect();
            if o.len() < 40 {
                continue;
            }
            let nn: Vec<&Row> = new
                .iter()
                .copied()
                .filter(|r| ok(r, cx) && ok(r, cy))
                .collect();
            found.push((
                stats(&o).0,
                format!("{} AND {}", words(cx), words(cy)),
                stats(&o),
                stats(&nn),
            ));
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("\n{tried} filters tried (40+ older entries each); the best 15 on the older part, then the newest third:");
    for f in found.iter().take(15) {
        println!(
            "  older {:+.2}% ({}, {} runners) | newest {:+.2}% ({}, {} runners)  {}",
            f.2 .0, f.2 .1, f.2 .2, f.3 .0, f.3 .1, f.3 .2, f.1
        );
    }
    let held = found
        .iter()
        .take(15)
        .filter(|f| f.3 .1 >= 10 && f.3 .0 > 0.05)
        .count();
    println!("\nof the best 15, {held} kept +0.05%+ on the newest third with 10+ entries");
}
