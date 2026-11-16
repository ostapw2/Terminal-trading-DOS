//! Pairs at their edge on live history: `n` busy coins with a perpetual,
//! `days` of hourly closes (disk store), the book now and the replay.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example edges_probe -- 150 180

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::edges;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(150);
    let days: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(180);
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
    let hours = days * 24;
    let t = std::time::Instant::now();
    let series: Vec<(String, Vec<f64>)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = coins
            .chunks(coins.len().div_ceil(6).max(1))
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|s| {
                            let c =
                                binance::fetch_klines_stored(s, "1h", hours, Venue::Spot).ok()?;
                            (c.len() + 2 >= hours as usize)
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
        "{} of {} coins x {hours} h in {:.1} s",
        series.len(),
        coins.len(),
        t.elapsed().as_secs_f64()
    );
    let t = std::time::Instant::now();
    let b = edges::book(
        &series,
        edges::Params {
            min_r: 0.6,
            ..Default::default()
        },
        0,
    );
    println!(
        "book: {} tied of {} pairs in {:.1} s",
        b.pairs.len(),
        b.tried,
        t.elapsed().as_secs_f64()
    );
    for e in b.pairs.iter().take(15) {
        println!(
            "  {:>10}/{:<10} r_min {:.2} edge {:>5.2}% now {:+6.2}% ({:>3.0}% of edge) times {}",
            e.a,
            e.b,
            e.r_min,
            e.edge_pct,
            e.gap_pct,
            e.share() * 100.0,
            e.times
        );
    }
    let t = std::time::Instant::now();
    let r = edges::report(&series, 0);
    println!(
        "replayed {} settings in {:.1} s",
        r.tried.len(),
        t.elapsed().as_secs_f64()
    );
    for x in &r.tried {
        let p = x.params.unwrap();
        println!(
            "  r>={:.1} entry {:>3.0}% exit {:>3.0}% stop x{:.1}: {:>4} b, {:>3.0}% won, gross {:+.2} net {:+.2} worst {:+.2} | target {} stop {} clock {} | {:.0} h | older {:+.2} ({}) newest {:+.2} ({}) {}",
            p.min_r, p.entry_frac * 100.0, p.exit_frac * 100.0, p.stop_frac, x.trades, x.win_rate(), x.mean_gross_pct, x.mean_pct,
            x.worst_pct, x.by_target, x.by_stop, x.by_clock, x.mean_hours, x.older_pct, x.older_trades,
            x.newer_pct, x.newer_trades, if x.passes() { "PASSES" } else { "" }
        );
    }
    if let Some(b) = &r.best {
        let p = b.params.unwrap();
        println!(
            "best on older 2/3: r>={} entry {}% exit {}% -> newest {:+.2}% ({}) {}",
            p.min_r,
            p.entry_frac * 100.0,
            p.exit_frac * 100.0,
            b.newer_pct,
            b.newer_trades,
            if b.passes() { "PASSES" } else { "fails" }
        );
    }
}
