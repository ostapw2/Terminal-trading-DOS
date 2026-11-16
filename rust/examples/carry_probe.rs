//! The funding-carry rule replayed on live history: `n` busy perpetuals
//! (from `market.db`'s catalogue, else a fixed list), `days` of hourly
//! candles of both legs and the settlements, default and other thresholds.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example carry_probe -- 30 90

use dos::carry::{self, Params};
use dos::data::history;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(30);
    let days: i64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(90);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut coins = history::carry_universe(n);
    if coins.is_empty() {
        coins = [
            "BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "ADA", "LINK", "AVAX", "SUI",
        ]
        .iter()
        .map(|c| format!("{c}USDT"))
        .collect();
    }
    let t = std::time::Instant::now();
    let series: Vec<carry::Series> = coins
        .iter()
        .filter_map(|c| history::carry_series(c, days, now))
        .collect();
    println!(
        "{} of {} coins, {} days, gathered in {:.1} s",
        series.len(),
        coins.len(),
        days,
        t.elapsed().as_secs_f64()
    );
    for (apr, hold) in [
        (30.0, 168),
        (20.0, 168),
        (50.0, 168),
        (30.0, 72),
        (30.0, 336),
    ] {
        let p = Params {
            min_apr: apr,
            hold_h: hold,
            ..Params::default()
        };
        let r = carry::report(&series, p, now);
        let a = &r.all;
        println!(
            "apr>={apr:>3} hold {hold:>3}h: {:>4} trades, {:>3.0}% won, mean {:+.3}% (funding {:+.3}, price {:+.3}, fees -{:.3}), worst {:+.2}%, {} liq, {} stop | older {:+.3}% ({}) newer {:+.3}% ({})",
            a.trades, a.win_rate(), a.mean_pct, a.mean_funding_pct, a.mean_price_pct, a.mean_fees_pct,
            a.worst_pct, a.liquidations, a.by_stop, r.older.mean_pct, r.older.trades, r.newer.mean_pct, r.newer.trades
        );
    }
}
