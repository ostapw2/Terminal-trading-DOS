//! Every minute-based rule replayed with the code that trades it, on live
//! history: `n` busy coins (`market.db`'s catalogue), `days` of one-minute
//! closes through the disk store.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example replay_probe -- 120 3

use dos::data::binance::{self, Venue};
use dos::data::marketdb;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(120);
    let days: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let coins: Vec<String> =
        marketdb::with(|c| marketdb::busiest(c, "BINANCE", "spot", "crypto", n * 3))
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.ends_with("USDT") && dos::scanner::tradable(s))
            .take(n)
            .collect();
    let minutes = days * 1440;
    let t = std::time::Instant::now();
    let series: Vec<(String, Vec<f64>)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = coins
            .chunks(coins.len().div_ceil(6).max(1))
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|s| {
                            let c =
                                binance::fetch_klines_stored(s, "1m", minutes, Venue::Spot).ok()?;
                            (c.len() + 5 >= minutes as usize)
                                .then(|| (s.clone(), c.iter().map(|k| k.close).collect()))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        jobs.into_iter()
            .flat_map(|j| j.join().unwrap_or_default())
            .collect()
    });
    println!(
        "{} of {} coins x {minutes} min gathered in {:.1} s",
        series.len(),
        coins.len(),
        t.elapsed().as_secs_f64()
    );
    let t = std::time::Instant::now();
    let r = dos::replay::full_report(&series, &[], 0);
    println!(
        "replayed {} settings in {:.1} s over {} minutes",
        r.rules.len() + 48,
        t.elapsed().as_secs_f64(),
        r.minutes
    );
    for x in r.rules.iter().chain(&r.tuned) {
        let (th, unit) = x.kind.map_or((0.0, ""), |k| k.threshold());
        println!(
            "{:<16} th {th:.1}{unit:<6} exit {:<28} {:>5} b, gross {:+.3} fees {:.3} net {:+.3} | older 2/3 {:+.3} ({}) newest 1/3 {:+.3} ({}) {}",
            x.short, x.exit.map_or(String::new(), |e| e.words()), x.trades, x.mean_gross_pct, x.mean_fees_pct, x.mean_pct,
            x.older_pct, x.older_trades, x.newer_pct, x.newer_trades, if x.passes() { "PASSES" } else { "" }
        );
    }
}
