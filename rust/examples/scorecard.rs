//! One table for every automatic rule: what each made on history after
//! fees, on the newest third it was not tuned on, its worst position, and
//! whether the guard would let it trade.  The same code and the same data
//! the app uses (`replay`, `carry`, `pullback`, `statarb`, `backtest`), so
//! the table says what to keep working on and what to park.
//!
//!   DOS_DB=/Volumes/DEV/dos-data/data.db cargo run --release --example scorecard
//!   ... -- --skip minutes,carry,pullback,statarb,grid   (any of them)
//!
//! Minute rules: 13 days of 1-minute closes of the 120 busiest spot coins.
//! Carry: 90 days of hours of 24 perpetuals.  Pullback: 60 days of 5m.
//! Stat-arb: 60 days of 5m of the top-20 perpetuals (delisted included).
//! Grid: 200 days of 15m of BTC and ETH perpetuals, futures fees, 1 bp
//! funding a settlement.

use dos::data::binance::{self, Venue};
use dos::data::history;
use dos::engine::backtest::{self, BacktestConfig};
use dos::strategies::{registry, ParamValue};

const WORKERS: usize = 6;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Candles of many coins in parallel through the disk store.
fn candles(
    coins: &[String],
    tf: &str,
    bars: u32,
    venue: Venue,
) -> Vec<(String, Vec<dos::markets::Ohlc>)> {
    std::thread::scope(|scope| {
        let jobs: Vec<_> = coins
            .chunks(coins.len().div_ceil(WORKERS).max(1))
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|s| {
                            binance::fetch_klines_stored(s, tf, bars, venue)
                                .ok()
                                .filter(|c| c.len() > 100)
                                .map(|c| (s.clone(), c))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        jobs.into_iter()
            .flat_map(|j| j.join().unwrap_or_default())
            .collect()
    })
}

fn row(
    name: &str,
    trades: usize,
    mean: f64,
    newer: f64,
    newer_n: usize,
    worst: f64,
    verdict: Result<String, String>,
) {
    let (v, why) = match verdict {
        Ok(w) => ("TRADES", w),
        Err(w) => ("parked", w),
    };
    println!(
        "{name:<34} {trades:>5} {mean:>+8.3} {newer:>+8.3} ({newer_n:>4}) {worst:>+8.2}  {v:<7} {why}"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let skip: Vec<String> = args
        .iter()
        .position(|a| a == "--skip")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();
    let want = |k: &str| !skip.iter().any(|s| s == k);
    let now = now_ms();
    println!(
        "{:<34} {:>5} {:>8} {:>8} {:>6} {:>8}  {:<7} why",
        "rule", "n", "net %", "newest", "(n)", "worst", "guard"
    );
    println!("{}", "-".repeat(110));

    if want("minutes") {
        let t = std::time::Instant::now();
        let coins = history::spot_universe(120, now);
        let minutes = 13 * 1440;
        let series: Vec<(String, Vec<f64>)> = candles(&coins, "1m", minutes, Venue::Spot)
            .into_iter()
            .filter(|(_, c)| c.len() + 5 >= minutes as usize)
            .map(|(s, c)| (s, c.iter().map(|k| k.close).collect()))
            .collect();
        eprintln!(
            "minutes: {} coins x 13 days in {:.0} s",
            series.len(),
            t.elapsed().as_secs_f64()
        );
        let rules = dos::replay::default_rules();
        let r = dos::replay::full_report(&series, &rules, now);
        for d in &r.rules {
            let rule = rules.iter().find(|x| Some(&x.kind) == d.kind.as_ref());
            let v = match rule {
                Some(rule) => dos::replay::verdict(Some(&r), rule),
                None => Err("no rule".into()),
            };
            row(
                &format!("{} (default)", d.short),
                d.trades,
                d.mean_pct,
                d.newer_pct,
                d.newer_trades,
                d.worst_pct,
                v,
            );
            if let Some(tn) = r.tuned(&d.short) {
                let v = if tn.passes() {
                    Ok(format!("tuned on the older two thirds: {}", tn.label))
                } else {
                    Err(format!("tuned on the older two thirds: {}", tn.label))
                };
                row(
                    &format!("{} (tuned)", d.short),
                    tn.trades,
                    tn.mean_pct,
                    tn.newer_pct,
                    tn.newer_trades,
                    tn.worst_pct,
                    v,
                );
            }
        }
    }

    if want("carry") {
        let t = std::time::Instant::now();
        let coins = history::carry_universe(24);
        let series: Vec<dos::carry::Series> = coins
            .iter()
            .filter_map(|c| history::carry_series(c, 90, now))
            .collect();
        eprintln!(
            "carry: {} coins x 90 days in {:.0} s",
            series.len(),
            t.elapsed().as_secs_f64()
        );
        let p = dos::carry::Params::default();
        let r = dos::carry::report(&series, p, now);
        let a = &r.all;
        row(
            "funding carry (spot long / perp short)",
            a.trades,
            a.mean_pct,
            r.newer.mean_pct,
            r.newer.trades,
            a.worst_pct,
            dos::carry::verdict(Some(&r), p.min_apr),
        );
    }

    if want("pullback") {
        let t = std::time::Instant::now();
        let p = dos::pullback::Params::default();
        let coins: Vec<String> = history::spot_universe(120, now)
            .into_iter()
            .filter(|s| {
                !p.perp
                    || dos::data::marketdb::with(|c| {
                        dos::data::marketdb::find_instrument(c, "BINANCEF", "perp", s).is_some()
                    })
                    .unwrap_or(false)
            })
            .collect();
        let bars = (60 * 1440 / p.tf_min.max(1)).min(binance::MAX_HISTORY);
        let venue = if p.perp { Venue::Futures } else { Venue::Spot };
        let series = candles(&coins, p.interval(), bars, venue);
        eprintln!(
            "pullback: {} coins x 60 days in {:.0} s",
            series.len(),
            t.elapsed().as_secs_f64()
        );
        let r = dos::pullback::report(&series, p, now);
        let o = &r.own;
        row(
            "pullback (buy the break after a rise)",
            o.trades,
            o.mean_pct,
            o.newer_pct,
            o.newer_trades,
            o.worst_pct,
            dos::pullback::verdict(Some(&r), &p),
        );
    }

    if want("statarb") {
        let t = std::time::Instant::now();
        let pool = history::perp_universe(150, true, now);
        let bars = (60 * 288).min(binance::MAX_HISTORY);
        let legs: Vec<dos::statarb::Leg> = candles(&pool, "5m", bars, Venue::Futures)
            .into_iter()
            .map(|(s, c)| {
                let f = history::funding(&s, 61, now);
                (s, c, f)
            })
            .collect();
        let m = dos::statarb::Market::align(legs);
        eprintln!(
            "stat-arb: {} perpetuals x 60 days of 5m in {:.0} s",
            m.names.len(),
            t.elapsed().as_secs_f64()
        );
        let p = dos::statarb::Params::default();
        let r = dos::statarb::replay(&m, &p);
        let s = dos::statarb::summarize(&r, &p);
        row(
            "stat-arb grid (top-20 perps, limits)",
            s.trades,
            s.mean_pct,
            s.newer_pct,
            s.newer_trades,
            s.worst_pct,
            dos::statarb::passes(&s, &p),
        );
    }

    if want("grid") {
        // Spacing x range tried, the setting chosen on the older two thirds
        // of the equity curve, judged on the newest third (TP = spacing; a
        // rung = 2 % of the cash; futures fees; 1 bp funding a settlement).
        for sym in ["BTCUSDT_PERP", "ETHUSDT_PERP"] {
            let t = std::time::Instant::now();
            let bars = binance::fetch_klines_stored(
                dos::futures::exchange_symbol(sym),
                "15m",
                (200 * 96).min(binance::MAX_HISTORY),
                Venue::Futures,
            )
            .unwrap_or_default();
            if bars.len() < 1000 {
                eprintln!("grid: no candles for {sym}");
                continue;
            }
            let cfg = BacktestConfig::default();
            let qty = cfg.starting_cash * 0.02 / bars[0].close;
            let coin = sym.trim_end_matches("USDT_PERP");
            let mut runs: Vec<(f64, f64, backtest::BacktestResult, f64, f64)> = Vec::new();
            for spacing in [0.5, 1.0, 1.5, 2.0] {
                for range in [0.0, 5.0, 8.0, 12.0] {
                    let Some(mut s) = registry::instantiate("hedge_grid") else {
                        continue;
                    };
                    let _ = s.set_param_checked("spacing_pct", ParamValue::Float(spacing));
                    let _ = s.set_param_checked("take_profit_pct", ParamValue::Float(spacing));
                    let _ = s.set_param_checked("range_pct", ParamValue::Float(range));
                    let _ = s.set_param_checked("qty_per_level", ParamValue::Float(qty));
                    let Ok(r) = backtest::run(s, sym, &bars, &cfg) else {
                        continue;
                    };
                    let n = r.equity_curve.len();
                    let cut = n * 2 / 3;
                    let (older, newer) = if n > 2 {
                        (
                            (r.equity_curve[cut] - cfg.starting_cash) / cfg.starting_cash * 100.0,
                            (r.equity_curve[n - 1] - r.equity_curve[cut]) / cfg.starting_cash
                                * 100.0,
                        )
                    } else {
                        (0.0, 0.0)
                    };
                    runs.push((spacing, range, r, older, newer));
                }
            }
            // The plain default first, then the one the older two thirds pick.
            let show = |label: &str,
                        run: &(f64, f64, backtest::BacktestResult, f64, f64),
                        chosen: bool| {
                let (spacing, range, r, older, newer) = run;
                let why = format!(
                    "spacing {spacing}% range {}: older {older:+.2}%, newest {newer:+.2}% of the cash, drawdown {:.1}%, funding {:+.1}",
                    if *range > 0.0 { format!("{range}%") } else { "none".into() },
                    r.max_drawdown_pct,
                    r.funding
                );
                let v = if chosen && *newer > 0.0 && r.max_drawdown_pct < 10.0 {
                    Ok(why)
                } else {
                    Err(why)
                };
                row(
                    &format!("grid {coin} {label}"),
                    r.trades,
                    if r.trades > 0 {
                        r.net_pnl / r.trades as f64 / cfg.starting_cash * 100.0
                    } else {
                        0.0
                    },
                    *newer,
                    r.trades / 3,
                    -r.max_drawdown_pct,
                    v,
                );
            };
            if let Some(d) = runs.iter().find(|x| x.0 == 0.5 && x.1 == 0.0) {
                show("(default, no range)", d, false);
            }
            // Chosen on the older two thirds among settings that traded enough
            // there (30 round trips, as the rules' replay asks).
            match runs
                .iter()
                .filter(|x| x.2.trades * 2 / 3 >= 30)
                .max_by(|a, b| a.3.total_cmp(&b.3))
            {
                Some(best) => show("(chosen on the older 2/3)", best, true),
                None => println!(
                    "{:<34} no setting made 30 round trips on the older two thirds",
                    format!("grid {coin} (chosen)")
                ),
            }
            let up = runs.iter().filter(|x| x.4 > 0.0).count();
            eprintln!(
                "grid {sym}: {} candles, {} of {} settings up on the newest third, in {:.0} s",
                bars.len(),
                up,
                runs.len(),
                t.elapsed().as_secs_f64()
            );
        }
    }
}
