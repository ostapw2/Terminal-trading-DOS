//! Replay a strategy over Binance history and print the result.
//!
//! Run: `cargo run --release --example backtest -- <strategy> <SYMBOL> <interval> [candles] [key=value ...] [--fee=BPS] [--funding=RATE] [--cash=USD]`
//!
//! Example: `cargo run --release --example backtest -- dca_timer BTCUSDT 1h 1000 --fee=10`
//! A perpetual: `-- hedge_grid BTCUSDT_PERP 15m 19200 spacing_pct=1 range_pct=8` (futures fees,
//! funding `--funding` per settlement, 0.0001 = 1 bp).
//!
//! Candles come through `market.db` (`DOS_DB`), up to 20,000, fetched once.
//! `--list` prints the strategies and their parameters.  Fill model and its
//! limits: see the module docs of `engine::backtest`.

use dos::data::binance::{fetch_klines_stored, Venue, MAX_HISTORY};
use dos::engine::backtest::{run, BacktestConfig};
use dos::strategies::{registry, ParamKind, ParamValue};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        list();
        return;
    }
    let mut cfg = BacktestConfig::default();
    let mut pos: Vec<&str> = Vec::new();
    let mut kv: Vec<(&str, &str)> = Vec::new();
    for a in &args {
        if let Some(v) = a.strip_prefix("--fee=") {
            cfg.fee_bps = v.parse().unwrap_or_else(|_| die("bad --fee"));
        } else if let Some(v) = a.strip_prefix("--funding=") {
            cfg.funding_rate = v.parse().unwrap_or_else(|_| die("bad --funding"));
        } else if let Some(v) = a.strip_prefix("--cash=") {
            cfg.starting_cash = v.parse().unwrap_or_else(|_| die("bad --cash"));
        } else if let Some((k, v)) = a.split_once('=') {
            kv.push((k, v));
        } else {
            pos.push(a);
        }
    }
    if pos.len() < 3 {
        die("usage: backtest <strategy> <SYMBOL> <interval> [candles] [key=value ...] [--fee=BPS] [--cash=USD]  (--list for strategies)");
    }
    let (id, symbol, interval) = (pos[0], pos[1], pos[2]);
    let limit: u32 = pos
        .get(3)
        .map(|s| s.parse().unwrap_or_else(|_| die("bad candle count")))
        .unwrap_or(1000);

    let mut strategy =
        registry::instantiate(id).unwrap_or_else(|| die(&format!("unknown strategy '{id}'")));
    for (k, v) in kv {
        let spec = strategy
            .schema()
            .iter()
            .find(|s| s.key == k)
            .copied()
            .unwrap_or_else(|| die(&format!("'{id}' has no parameter '{k}'")));
        let value = match spec.kind {
            ParamKind::Int { .. } | ParamKind::Duration { .. } => ParamValue::Int(
                v.parse()
                    .unwrap_or_else(|_| die(&format!("{k}: need integer"))),
            ),
            ParamKind::Float { .. } => ParamValue::Float(
                v.parse()
                    .unwrap_or_else(|_| die(&format!("{k}: need number"))),
            ),
            ParamKind::Bool { .. } => ParamValue::Bool(
                v.parse()
                    .unwrap_or_else(|_| die(&format!("{k}: true/false"))),
            ),
            ParamKind::Choice { .. } => ParamValue::Text(v.to_string()),
        };
        if let Err(e) = strategy.set_param_checked(k, value) {
            die(&format!("{k}: {e}"));
        }
    }

    let limit = limit.min(MAX_HISTORY);
    let (venue, exchange_symbol) = if dos::futures::is_perp(symbol) {
        (Venue::Futures, dos::futures::exchange_symbol(symbol))
    } else {
        (Venue::Spot, symbol)
    };
    eprintln!("fetching {limit} x {interval} candles for {exchange_symbol} ({venue:?}) ...");
    let bars = fetch_klines_stored(exchange_symbol, interval, limit, venue)
        .unwrap_or_else(|e| die(&format!("fetch failed: {e}")));
    let r = run(strategy, symbol, &bars, &cfg).unwrap_or_else(|e| die(&e));

    println!("strategy      {}", r.strategy_id);
    println!(
        "symbol        {}  {} candles ({} .. {})",
        r.symbol,
        r.bars,
        dos::markets::unix_secs_to_iso_date(r.first_ms / 1000),
        dos::markets::unix_secs_to_iso_date(r.last_ms / 1000)
    );
    println!("start cash    {:.2}", r.starting_cash);
    println!("final equity  {:.2}", r.final_equity);
    println!("net pnl       {:+.2}  ({:+.2} %)", r.net_pnl, r.return_pct);
    println!(
        "fees          {:.2}  ({} bps per fill)",
        r.fees, cfg.fee_bps
    );
    if dos::futures::is_perp(symbol) {
        println!(
            "funding       {:+.2}  ({} per settlement, futures fees)",
            r.funding, cfg.funding_rate
        );
    }
    println!("max drawdown  {:.2} %", r.max_drawdown_pct);
    println!(
        "round trips   {}  (win rate {:.0} %)",
        r.trades,
        r.win_rate_pct()
    );
    println!("fills         {}", r.fills.len());
    println!("open position {}", r.open_qty);
    if let Some(why) = &r.halted {
        println!("HALTED by risk gate: {why}");
    }
}

fn list() {
    for m in registry::catalog() {
        println!("{:<14} {}", m.id, m.description);
        if let Some(s) = registry::instantiate(m.id) {
            for p in s.schema() {
                println!("    {:<14} {}", p.key, p.help);
            }
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}
