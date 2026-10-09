//! Manual smoke test for the Yahoo Finance client.  Not run by `cargo test`
//! (network-dependent).
//!
//!   cargo run --release --example yahoo_smoke -- AAPL

fn main() {
    let ticker = std::env::args().nth(1).unwrap_or_else(|| "AAPL".into());
    println!("Fetching {} from Yahoo Finance...", ticker);
    match dos::data::yahoo::fetch(&ticker, "1mo", "1d") {
        Ok(data) => {
            println!("OK: {} candles", data.len());
            for c in data.iter().rev().take(5).rev() {
                println!(
                    "  {}  O {:>9.2}  H {:>9.2}  L {:>9.2}  C {:>9.2}  V {}",
                    c.date, c.open, c.high, c.low, c.close, c.volume
                );
            }
        }
        Err(e) => {
            eprintln!("ERR: {}", e);
            std::process::exit(1);
        }
    }
}
