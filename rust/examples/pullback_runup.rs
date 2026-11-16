//! After each pullback entry: how far did the price run before the stop
//! (max favourable excursion over the next 24 h)?  Would letting winners
//! run (no fixed target) catch the 20 % moves?
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example pullback_runup -- 120 13

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::pullback::{self, Params};

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
    let day = (1440 / p.tf_min) as usize;
    let mut runs: Vec<(f64, f64, String)> = Vec::new(); // (max run %, risk %, coin)
    for s in &coins {
        let Ok(c) = binance::fetch_klines_stored(s, p.interval(), bars, Venue::Futures) else {
            continue;
        };
        let mut t = 30;
        while t + 1 < c.len() {
            // Candle t forming: an armed setup that breaks in it.
            let Some(st) = pullback::analyze(&c[t - 30..=t], &p) else {
                t += 1;
                continue;
            };
            if !st.triggered {
                t += 1;
                continue;
            }
            let entry = st.trigger.max(c[t].open);
            let mut best = c[t].high;
            let mut k = t + 1;
            while k < c.len() && k <= t + day {
                if c[k].low <= st.stop {
                    break;
                }
                best = best.max(c[k].high);
                k += 1;
            }
            runs.push((
                (best / entry - 1.0) * 100.0,
                (entry / st.stop - 1.0) * 100.0,
                s.clone(),
            ));
            t += 6;
        }
    }
    runs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let cnt = |x: f64| runs.iter().filter(|r| r.0 >= x).count();
    println!(
        "{} entries on {} coins ({}), risk to the stop: median {:.2}%",
        runs.len(),
        coins.len(),
        p.words(),
        {
            let mut v: Vec<f64> = runs.iter().map(|r| r.1).collect();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied().unwrap_or(0.0)
        }
    );
    for x in [1.0, 3.0, 5.0, 10.0, 20.0, 30.0] {
        println!(
            "  ran >= {x:>4}% before the stop (24 h): {:>4} ({:.1}%)",
            cnt(x),
            cnt(x) as f64 / runs.len().max(1) as f64 * 100.0
        );
    }
    println!("  biggest runs:");
    for r in runs.iter().take(10) {
        println!("    {:<14} +{:.1}% (risk {:.2}%)", r.2, r.0, r.1);
    }
}
