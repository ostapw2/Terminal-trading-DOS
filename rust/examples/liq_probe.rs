//! Live check of the liquidation stream: `cargo run --release --example
//! liq_probe -- 120` listens to Binance Futures `!forceOrder@arr` for that
//! many seconds and prints what it hears, then the totals.

use std::time::{Duration, Instant};

use dos::data::binance_ws::{connect_liquidations, LiqMsg};
use dos::data::liquidations::{fmt_usd, totals};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let h = connect_liquidations();
    let end = Instant::now() + Duration::from_secs(secs);
    let mut all = Vec::new();
    while Instant::now() < end {
        match h.rx.recv_timeout(Duration::from_millis(500)) {
            Ok(LiqMsg::Connected) => println!("connected"),
            Ok(LiqMsg::Disconnected(why)) => println!("disconnected: {why}"),
            Ok(LiqMsg::Liq(l)) => {
                println!(
                    "{} {:<14} {:<5} {:>12} {:>10}",
                    l.time_ms,
                    l.symbol,
                    if l.long { "LONG" } else { "SHORT" },
                    l.price,
                    fmt_usd(l.usd())
                );
                all.push(l);
            }
            Err(_) => {}
        }
    }
    let _ = h.stop_tx.send(());
    let t = totals(&all, 0);
    println!(
        "{} liquidations in {secs}s: longs {} shorts {}",
        t.count,
        fmt_usd(t.long_usd),
        fmt_usd(t.short_usd)
    );
}
