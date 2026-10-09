//! Live probe of the order-book data path.  Runs the same WS + REST seed +
//! `LocalBook::apply_checked` recovery the F6 view uses and prints
//! top-of-book once per second.  Exits non-zero if the book had to be
//! re-seeded more than once after convergence (i.e. sequencing is wrong).
//!
//! Run: `cargo run --release --example ob_probe -- BTCUSDT [spot|futures] [seconds]`

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use dos::data::binance::Venue;
use dos::data::orderbook::{DepthDiffEvent, LocalBook, OrderBookSnapshot};

type Seed = Receiver<Result<(OrderBookSnapshot, u64), String>>;

fn spawn_seed(symbol: String, venue: Venue) -> Seed {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = dos::data::binance::fetch_depth_with_id(&symbol, 1000, venue)
            .map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    rx
}

fn main() {
    let mut args = std::env::args().skip(1);
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let venue = match args.next().as_deref() {
        Some("futures") => Venue::Futures,
        _ => Venue::Spot,
    };
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    println!("probe: symbol={symbol} venue={venue:?} for {secs}s");

    let diff = dos::data::binance_ws::connect_depth_diff(&symbol, venue == Venue::Futures);
    // Binance's order: open the stream, wait for the first event, THEN fetch
    // the snapshot.
    let mut pending_seed: Option<Seed> = None;
    let mut seed_requested = false;
    let mut buffer: Vec<DepthDiffEvent> = Vec::new();
    let mut book: Option<LocalBook> = None;

    let start = Instant::now();
    let mut last_print = Instant::now();
    let (mut seen, mut applied, mut gaps_after_sync) = (0u64, 0u64, 0u64);
    let mut converged = false;

    while start.elapsed() < Duration::from_secs(secs) {
        let mut new_diffs: Vec<DepthDiffEvent> = Vec::new();
        while let Ok(d) = diff.rx.try_recv() {
            seen += 1;
            new_diffs.push(d);
        }

        if !seed_requested
            && pending_seed.is_none()
            && (!buffer.is_empty() || !new_diffs.is_empty())
        {
            buffer.append(&mut new_diffs);
            pending_seed = Some(spawn_seed(symbol.clone(), venue));
            seed_requested = true;
        }
        let seeded = pending_seed
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
            .and_then(|r| r.ok());
        if let Some((snap, last_id)) = seeded {
            pending_seed = None;
            let mut b = LocalBook::from_snapshot(&snap, last_id);
            let backlog: Vec<_> = std::mem::take(&mut buffer);
            match backlog
                .iter()
                .position(|d| b.apply_checked(d, venue).is_err())
            {
                Some(idx) => {
                    buffer.extend(backlog.into_iter().skip(idx));
                    buffer.append(&mut new_diffs);
                    std::thread::sleep(Duration::from_secs(1));
                    pending_seed = Some(spawn_seed(symbol.clone(), venue));
                    println!("probe: catch-up gap, re-seeding");
                }
                None => {
                    println!("probe: seed converged last_id={}", b.last_update_id);
                    converged = true;
                    book = Some(b);
                }
            }
        }

        if let Some(b) = book.as_mut() {
            let gap_at = new_diffs.iter().position(|d| {
                let ok = b.apply_checked(d, venue).is_ok();
                applied += ok as u64;
                !ok
            });
            if let Some(idx) = gap_at {
                buffer.extend(new_diffs.drain(idx..));
                gaps_after_sync += 1;
                pending_seed = Some(spawn_seed(symbol.clone(), venue));
                book = None;
                println!("probe: SYNC GAP after convergence");
            }
        } else {
            buffer.append(&mut new_diffs);
        }

        if last_print.elapsed() >= Duration::from_secs(1) {
            last_print = Instant::now();
            println!(
                "t+{:>3}s seen={seen} applied={applied} gaps={gaps_after_sync} buf={} last_id={:?} top_bid={:?}",
                start.elapsed().as_secs(),
                buffer.len(),
                book.as_ref().map(|b| b.last_update_id),
                book.as_ref().and_then(|b| b.snapshot_top(1).bids.first().copied()),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = diff.stop_tx.send(());
    println!("probe: done seen={seen} applied={applied} gaps_after_sync={gaps_after_sync} converged={converged}");
    if !converged || gaps_after_sync > 0 {
        std::process::exit(1);
    }
}
