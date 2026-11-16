//! Live check of the movers feed: `cargo run --release --example
//! movers_probe -- 90` listens to Spot `!miniTicker@arr` for that many
//! seconds, feeds `dos::movers`, and prints how many coins it heard, their
//! history span, the largest moves meanwhile and any pump / dump found.

use std::collections::HashMap;
use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let h = dos::data::binance_ws::connect_market_tickers(false);
    let mut movers = dos::movers::Movers::default();
    let mut first: HashMap<String, (f64, f64)> = HashMap::new();
    let mut last: HashMap<String, (f64, f64)> = HashMap::new();
    let (mut batches, mut ticks) = (0usize, 0usize);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(batch) = h.rx.recv_timeout(Duration::from_millis(500)) {
            batches += 1;
            ticks += batch.len();
            for t in &batch {
                first
                    .entry(t.symbol.clone())
                    .or_insert((t.price, t.quote_volume));
                last.insert(t.symbol.clone(), (t.price, t.quote_volume));
            }
            movers.feed(&batch);
        }
    }
    let _ = h.stop_tx.send(());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    movers.judge(now);
    let (coins, span) = movers.coverage();
    println!(
        "{batches} messages, {ticks} ticks, {coins} tradable coins, history {} s",
        span / 1000
    );
    let mut moves: Vec<(String, f64, f64)> = last
        .iter()
        .filter_map(|(s, (p, q))| {
            let (p0, q0) = first.get(s)?;
            Some((s.clone(), (p / p0 - 1.0) * 100.0, q - q0))
        })
        .collect();
    moves.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    println!("largest moves while listening (symbol, %, change of 24h quote volume):");
    for (s, pct, dq) in moves.iter().take(8) {
        println!("  {s:<14} {pct:+.2}%  {dq:+.0}");
    }
    for up in [true, false] {
        for m in movers.list(up) {
            println!("{} {m:?}", if up { "PUMP" } else { "DUMP" });
        }
    }
}
