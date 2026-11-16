//! The pullback rule replayed on live history: `n` busy coins, `days` of
//! `tf`-minute candles (disk store), the user's setting and the grid.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example pullback_probe -- 120 13 15 3 5

use dos::data::binance::{self, Venue};
use dos::data::marketdb;
use dos::pullback::{self, Params};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: f64| a.get(i).and_then(|x| x.parse().ok()).unwrap_or(d);
    let n = num(0, 120.0) as usize;
    let days = num(1, 13.0) as u32;
    let p = Params {
        window_min: num(2, 15.0) as u32,
        min_rise_pct: num(3, 3.0),
        tf_min: num(4, 5.0) as u32,
        target_r: 2.0,
        perp: a.get(5).is_some_and(|x| x == "perp"),
    };
    let venue = if p.perp { Venue::Futures } else { Venue::Spot };
    let coins: Vec<String> =
        marketdb::with(|c| marketdb::busiest(c, "BINANCE", "spot", "crypto", n * 3))
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.ends_with("USDT")
                    && dos::scanner::tradable(s)
                    && (!p.perp
                        || marketdb::with(|c| {
                            marketdb::find_instrument(c, "BINANCEF", "perp", s).is_some()
                        })
                        .unwrap_or(false))
            })
            .take(n)
            .collect();
    let bars = days * 1440 / p.tf_min;
    let t = std::time::Instant::now();
    let series: Vec<(String, Vec<dos::markets::Ohlc>)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = coins
            .chunks(coins.len().div_ceil(6).max(1))
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|s| {
                            let c =
                                binance::fetch_klines_stored(s, p.interval(), bars, venue).ok()?;
                            (c.len() > 100).then(|| (s.clone(), c))
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
        "{} coins x {bars} bars in {:.1} s; {}",
        series.len(),
        t.elapsed().as_secs_f64(),
        p.words()
    );
    let r = pullback::report(&series, p, 0);
    for x in std::iter::once(&r.own).chain(&r.tried) {
        let q = x.params.unwrap();
        println!(
            "  rise {:>4.1}% {:.1}R: {:>4} trades, {:>3.0}% won, gross {:+.3} net {:+.3} worst {:+.2} | target {} stop {} clock {} | older {:+.3} ({}) newest {:+.3} ({}) {}",
            q.min_rise_pct, q.target_r, x.trades, x.win_rate(), x.mean_gross_pct, x.mean_pct, x.worst_pct,
            x.by_target, x.by_stop, x.by_clock, x.older_pct, x.older_trades, x.newer_pct, x.newer_trades,
            if x.passes() { "PASSES" } else { "" }
        );
    }
}
