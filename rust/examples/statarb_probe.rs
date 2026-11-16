//! Statistical arbitrage on the most liquid perpetuals, several settings at
//! once on the same data (`dos::statarb`, loaded once from `market.db`):
//! the 21.10 grid as it was, then funding counted, a loss cap and a time
//! cap, the concentration caps, the regime filter.  Delisted perpetuals are
//! in the pool (Binance still serves their candles).
//!
//!   DOS_DB=/Volumes/DEV/dos-data/data.db cargo run --release --example statarb_probe -- 20 200 15m
//!   DOS_DB=/Volumes/DEV/dos-data/data.db cargo run --release --example statarb_probe -- 20 60 5m

use dos::data::binance::{self, Venue};
use dos::data::history;
use dos::statarb::{self, Market, Params};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let top: usize = a.first().and_then(|x| x.parse().ok()).unwrap_or(20);
    let days: u32 = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(60);
    let tf: String = a.get(2).cloned().unwrap_or_else(|| "5m".into());
    let bar_min = match tf.as_str() {
        "1m" => 1,
        "5m" => 5,
        "15m" => 15,
        "1h" => 60,
        _ => {
            eprintln!("timeframe 1m / 5m / 15m / 1h");
            return;
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let pool = history::perp_universe(150, true, now);
    if pool.is_empty() {
        eprintln!("no perpetuals in the catalogue (set DOS_DB and be online)");
        return;
    }
    let bars = (days * 1440 / bar_min).min(binance::MAX_HISTORY);
    let t = std::time::Instant::now();
    let mut legs: Vec<statarb::Leg> = Vec::new();
    let (mut ok, mut dead) = (0, 0);
    for (k, s) in pool.iter().enumerate() {
        match binance::fetch_klines_stored(s, &tf, bars, Venue::Futures) {
            Ok(c) if c.len() >= 30 => {
                let f = history::funding(s, days as i64 + 1, now);
                legs.push((s.clone(), c, f));
                ok += 1;
            }
            _ => dead += 1,
        }
        if (k + 1) % 25 == 0 {
            eprintln!(
                "  {} of {} coins in {:.0} s",
                k + 1,
                pool.len(),
                t.elapsed().as_secs_f64()
            );
        }
    }
    let m = Market::align(legs);
    println!(
        "{ok} perpetuals ({dead} without candles), {} bars of {tf} ({:.0} days), gathered in {:.0} s; top {top} by last week's dollars",
        m.clock.len(),
        m.clock.len() as f64 / m.bars_per_day() as f64,
        t.elapsed().as_secs_f64()
    );
    let free = Params {
        top,
        loss_cap_pct: None,
        max_hold_hl: None,
        max_hold_min: 1440.0,
        max_per_coin: 1000,
        max_open: 1000,
        regime: None,
        funding: false,
        ..Params::default()
    };
    let caps = Params {
        loss_cap_pct: Some(4.0),
        max_hold_hl: Some(2.0),
        max_hold_min: 2880.0,
        funding: true,
        ..free.clone()
    };
    let focus = Params {
        max_per_coin: 2,
        max_open: 10,
        ..caps.clone()
    };
    let cfgs: Vec<(&str, Params)> = vec![
        (
            "A  the 21.10 grid: no caps, no funding, a day at most",
            free.clone(),
        ),
        (
            "B  A + funding of both legs counted",
            Params {
                funding: true,
                ..free.clone()
            },
        ),
        (
            "C  B + loss cap 4% + time cap 2 half-lives (2 days at most)",
            caps.clone(),
        ),
        (
            "C3 loss cap 3%",
            Params {
                loss_cap_pct: Some(3.0),
                ..caps.clone()
            },
        ),
        (
            "C5 loss cap 5%",
            Params {
                loss_cap_pct: Some(5.0),
                ..caps.clone()
            },
        ),
        (
            "C1 time cap 1 half-life",
            Params {
                max_hold_hl: Some(1.0),
                ..caps.clone()
            },
        ),
        (
            "D  C + a coin in 2 pairs at most, 10 pairs open at most",
            focus.clone(),
        ),
        (
            "E  D + no entries when the market's day move > 1.5x its month",
            Params {
                regime: Some(1.5),
                ..focus.clone()
            },
        ),
        (
            "F  D + out at the mean (z 0)",
            Params {
                z_out: 0.0,
                ..focus.clone()
            },
        ),
        (
            "G  D + fills at once (no bar of delay)",
            Params {
                delay_fill: false,
                ..focus.clone()
            },
        ),
        (
            "H  D with 20 pairs open at most",
            Params {
                max_open: 20,
                ..focus.clone()
            },
        ),
        (
            "I  H + out at the mean (z 0)",
            Params {
                max_open: 20,
                z_out: 0.0,
                ..focus.clone()
            },
        ),
        (
            "J  D with a single entry at 2 sd (no grid)",
            Params {
                levels: vec![2.0],
                ..focus.clone()
            },
        ),
        (
            "K  D on the top 50",
            Params {
                top: 50,
                ..focus.clone()
            },
        ),
    ];
    println!("% of the dollars deployed, per position; fees maker 2 bps a leg, taker 5.5 on the loss cap");
    for (name, p) in &cfgs {
        let t = std::time::Instant::now();
        let r = statarb::replay(&m, p);
        let s = statarb::summarize(&r, p);
        println!(
            "\n{name} — {} pair-days, {} positions ({} units; back {}, loss {}, time {}, end {}), {:.1} h held, {:.1} s",
            s.pair_days,
            s.trades,
            s.units,
            s.by_how[0],
            s.by_how[1],
            s.by_how[2],
            s.by_how[3],
            s.mean_hours,
            t.elapsed().as_secs_f64()
        );
        if s.trades == 0 {
            continue;
        }
        println!(
            "   net {:+.3}% (gross {:+.3}, fees -{:.3}, funding {:+.3}), won {:.0}%, worst {:+.2}% | quarters {} | older {:+.3}% ({}) newest third {:+.3}% ({})",
            s.mean_pct,
            s.mean_gross_pct,
            s.mean_fees_pct,
            s.mean_funding_pct,
            s.won_pct,
            s.worst_pct,
            s.quarters.iter().map(|q| format!("{q:+.3}")).collect::<Vec<_>>().join(" "),
            s.older_pct,
            s.older_trades,
            s.newer_pct,
            s.newer_trades
        );
        println!(
            "   capital of {} full grids: {:+.2}% over the run, deepest drawdown {:.2}%; skipped: regime {}, no room {}; 1% of the thinner leg's 5 min = ${:.0} median, ${:.0} thinnest tenth",
            p.max_open.min(1000),
            s.capital_pct,
            s.max_dd_pct,
            s.skipped_regime,
            s.skipped_room,
            s.thin_median,
            s.thin_tenth
        );
        match statarb::passes(&s, p) {
            Ok(why) => println!("   PASSES: {why}"),
            Err(why) => println!("   held back: {why}"),
        }
    }
}
