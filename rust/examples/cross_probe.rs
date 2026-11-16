//! Live probe: every exchange's quotes, then the widest cross-exchange rows.
//!   cargo run --release --example cross_probe
use std::collections::HashMap;

fn main() {
    let mut quotes = HashMap::new();
    for ex in dos::exchanges::ALL {
        let t = std::time::Instant::now();
        match dos::exchanges::fetch(ex) {
            Ok(q) => {
                println!(
                    "{:<9} {:>5} quotes in {:?}",
                    ex.code(),
                    q.len(),
                    t.elapsed()
                );
                quotes.insert(ex, q);
            }
            Err(e) => println!("{:<9} FAILED: {e}", ex.code()),
        }
    }
    for (label, kinds) in [
        ("spot vs spot", dos::crossx::Kind::Cross),
        ("with perps", dos::crossx::Kind::Basis),
    ] {
        let rows = dos::crossx::find(&quotes, 1_000_000.0, 0.0, |k| k == kinds);
        println!(
            "\n{label}: {} rows with net > 0 (>= $1M a day each side)",
            rows.len()
        );
        for o in rows.iter().take(8) {
            println!(
                "  {:<14} buy {:<9} {:<12} sell {:<9} {:<12} gap {:>6.1} bps net {:>6.1} bps vol {:.1}M",
                o.symbol, o.buy.code(), o.ask, o.sell.code(), o.bid, o.gap_bps, o.net_bps, o.vol_min / 1e6
            );
        }
    }
}
