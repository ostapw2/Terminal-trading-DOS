//! Cold and warm start of the candle store: fetch `n` busy coins' hourly
//! candles twice in one process (memory) and report the time.  Run it twice
//! with the same `DOS_DB` folder: the second run starts from `market.db`.
//!
//!   DOS_DB=/tmp/x/data.db cargo run --release --example candles_probe -- 40 1000

use std::time::Instant;

use dos::data::binance::{self, Venue};

const COINS: &[&str] = &[
    "BTC", "ETH", "BNB", "SOL", "XRP", "DOGE", "ADA", "TRX", "AVAX", "LINK", "DOT", "LTC", "BCH",
    "NEAR", "UNI", "APT", "ARB", "OP", "ATOM", "FIL", "ETC", "XLM", "HBAR", "INJ", "SUI", "AAVE",
    "TIA", "SEI", "RUNE", "ALGO", "PEPE", "SHIB", "WIF", "FET", "RENDER", "GRT", "LDO", "STX",
    "IMX", "MKR",
];

/// One pass over the first `n` coins on 8 threads (as Compare fetches).
fn pass(n: usize, want: u32) -> (f64, usize) {
    let t = Instant::now();
    let coins: Vec<&str> = COINS.iter().take(n).copied().collect();
    let ok = std::thread::scope(|scope| {
        let jobs: Vec<_> = coins
            .chunks(n.div_ceil(8).max(1))
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter(|c| {
                            binance::fetch_klines(&format!("{c}USDT"), "1h", want, Venue::Spot)
                                .is_ok()
                        })
                        .count()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap_or(0)).sum()
    });
    (t.elapsed().as_secs_f64(), ok)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(40);
    let want: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1000);
    let before = dos::data::marketdb::with(dos::data::marketdb::candle_rows).unwrap_or(0);
    let (first, ok) = pass(n, want);
    let (again, _) = pass(n, want);
    let after = dos::data::marketdb::with(dos::data::marketdb::candle_rows).unwrap_or(0);
    println!(
        "{ok}/{n} coins x {want} candles: first {first:.2} s, again {again:.2} s; candles on disk {before} -> {after}"
    );
}
