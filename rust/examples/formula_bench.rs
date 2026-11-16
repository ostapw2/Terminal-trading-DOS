//! How many formula evaluations per second (the search use case: align the
//! data once, then evaluate many formulas on it):
//! `cargo run --release --example formula_bench`.
use std::collections::HashMap;
use std::time::Instant;

use dos::formula::{align, parse, Field, Source};
use dos::markets::{Ohlc, Timeframe};

fn series(seed: f64, n: usize) -> Vec<Ohlc> {
    let mut p = 100.0 + seed;
    (0..n)
        .map(|i| {
            p *= 1.0 + ((i as f64 * 0.37 + seed).sin()) * 0.01;
            Ohlc {
                open: p,
                high: p * 1.01,
                low: p * 0.99,
                close: p * 1.002,
                volume: 1000.0,
                date: String::new(),
                time_ms: 1_699_999_200_000 + i as i64 * 3_600_000,
                is_gap: false,
            }
        })
        .collect()
}

fn per_second(mut f: impl FnMut()) -> u64 {
    let t = Instant::now();
    let mut runs = 0u64;
    while t.elapsed().as_millis() < 1000 {
        f();
        runs += 1;
    }
    runs
}

fn main() {
    let n = 1000;
    let mut data = HashMap::new();
    let names = ["AUSDT", "BUSDT", "CUSDT"];
    for (i, s) in names.iter().enumerate() {
        data.insert(Source::Spot(s.to_string()), series(i as f64 * 7.0, n));
    }
    let sources: Vec<Source> = names.iter().map(|s| Source::Spot(s.to_string())).collect();
    let al = align(&sources, &data, Timeframe::H1).unwrap();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!("{n} candles, {cores} cores");
    for text in ["=A/B", "=Z(A/B,50)", "=IDX(A)-IDX(B)+SMA(C,20)/EMA(A,10)"] {
        let f = parse(text).unwrap();
        let full = per_second(|| {
            f.evaluate(&data, Timeframe::H1).unwrap();
        });
        let one = per_second(|| {
            f.series(&al, Field::Close).unwrap();
        });
        let all: u64 = std::thread::scope(|s| {
            let hs: Vec<_> = (0..cores)
                .map(|_| {
                    s.spawn(|| {
                        per_second(|| {
                            f.series(&al, Field::Close).unwrap();
                        })
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).sum()
        });
        println!("{text:<38} candles {full:>7}/s   close series: {one:>8}/s one core, {all:>9}/s all cores");
    }
}
