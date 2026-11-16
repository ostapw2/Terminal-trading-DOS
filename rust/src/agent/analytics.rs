//! Candle-based analytics for agents: pure functions from downloaded series
//! to compact JSON.  The caller does the (slow) downloading off the main
//! loop; nothing here touches the network or the app state.

use serde_json::{json, Value};

use crate::data::binance::AggTrade;
use crate::data::orderbook::OrderBookSnapshot;
use crate::markets::{Ohlc, Timeframe};
use crate::pairs::{self, SpreadMode};
use crate::synthetic;

/// Longest series sent back to an agent (points).
pub const MAX_POINTS: usize = 120;
/// Most candles `get_klines` returns.
pub const MAX_KLINES: usize = 500;

/// A timeframe an agent may ask for: any but 15s (no exchange candles).
pub fn parse_timeframe(s: &str) -> Option<Timeframe> {
    Timeframe::parse(s).filter(|t| *t != Timeframe::S15)
}

/// At most `n` points, evenly spread, always keeping the last one.
pub fn downsample(v: &[f64], n: usize) -> Vec<f64> {
    if n == 0 || v.is_empty() {
        return Vec::new();
    }
    if v.len() <= n {
        return v.to_vec();
    }
    (0..n)
        .map(|i| v[((i + 1) * v.len() / n).clamp(1, v.len()) - 1])
        .collect()
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

pub fn parse_spread_mode(s: &str) -> Option<SpreadMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ratio" => Some(SpreadMode::Ratio),
        "beta" | "hedged" => Some(SpreadMode::Hedged),
        _ => None,
    }
}

pub fn parse_group_mode(s: &str) -> Option<synthetic::Mode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "index" => Some(synthetic::Mode::Index),
        "sum" => Some(synthetic::Mode::Sum),
        _ => None,
    }
}

/// Raw candles as `[time_ms, open, high, low, close, volume]`, newest last.
pub fn klines_json(symbol: &str, tf: Timeframe, candles: &[Ohlc], limit: usize) -> Value {
    let limit = limit.clamp(1, MAX_KLINES);
    let from = candles.len().saturating_sub(limit);
    let rows: Vec<Value> = candles[from..]
        .iter()
        .filter(|k| !k.is_gap)
        .map(|k| json!([k.time_ms, k.open, k.high, k.low, k.close, k.volume]))
        .collect();
    json!({
        "symbol": symbol,
        "timeframe": tf.name(),
        "columns": ["time_ms", "open", "high", "low", "close", "volume"],
        "count": rows.len(),
        "candles": rows,
    })
}

pub fn group_chart_json(
    list: &str,
    mode: synthetic::Mode,
    tf: Timeframe,
    series: &[(String, Vec<Ohlc>)],
    failed: usize,
) -> Result<Value, String> {
    let comp = synthetic::combine(series, mode);
    if comp.candles.is_empty() {
        return Err(format!(
            "no usable history for list '{list}' on {} ({} symbols failed to load, {} excluded)",
            tf.name(),
            failed,
            comp.excluded.len()
        ));
    }
    let closes: Vec<f64> = comp.candles.iter().map(|c| c.close).collect();
    let first = comp.candles[0].open;
    let last = *closes.last().unwrap_or(&first);
    let hi = comp.candles.iter().map(|c| c.high).fold(f64::MIN, f64::max);
    let lo = comp.candles.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    Ok(json!({
        "list": list,
        "mode": mode.short(),
        "timeframe": tf.name(),
        "symbols_used": comp.used,
        "excluded": comp.excluded,
        "failed_to_load": failed,
        "candles": comp.candles.len(),
        "first": round(first, 6),
        "last": round(last, 6),
        "change_pct": round((last / first - 1.0) * 100.0, 3),
        "high_bound": round(hi, 6),
        "low_bound": round(lo, 6),
        "note": "high/low are bounds (components do not peak together); open/close are exact",
        "closes": downsample(&closes, MAX_POINTS).into_iter().map(|x| round(x, 6)).collect::<Vec<_>>(),
    }))
}

pub fn spread_json(
    a: &str,
    b: &str,
    mode: SpreadMode,
    tf: Timeframe,
    ca: &[Ohlc],
    cb: &[Ohlc],
) -> Result<Value, String> {
    let sp = pairs::spread(ca, cb, mode).ok_or_else(|| {
        format!(
            "fewer than {} overlapping candles for {a}/{b} on {}",
            pairs::MIN_POINTS,
            tf.name()
        )
    })?;
    let s = &sp.stats;
    Ok(json!({
        "a": a,
        "b": b,
        "mode": mode.short(),
        "timeframe": tf.name(),
        "meaning": "deviation in percent of A vs B from the window mean; + = A rich, - = A cheap",
        "stats": {
            "n": s.n,
            "dev_pct": round(s.last, 3),
            "z": round(s.z, 3),
            "sigma_pct": round(s.sigma, 3),
            "corr_returns": round(s.corr, 3),
            "beta": round(s.beta, 3),
            "min_pct": round(s.min, 3),
            "max_pct": round(s.max, 3),
        },
        "last_time_ms": sp.time_ms.last(),
        "dev_pct_series": downsample(&sp.dev, MAX_POINTS).into_iter().map(|x| round(x, 3)).collect::<Vec<_>>(),
    }))
}

/// Pairwise scan: every pair of `series`, beta-hedged, ranked by |z| among
/// pairs whose return correlation is at least `min_corr`.
pub fn scan_pairs_json(
    series: &[(String, Vec<Ohlc>)],
    tf: Timeframe,
    min_corr: f64,
    limit: usize,
) -> Value {
    let mut found: Vec<(f64, Value)> = Vec::new();
    let mut tested = 0usize;
    for i in 0..series.len() {
        for j in i + 1..series.len() {
            let Some(sp) = pairs::spread(&series[i].1, &series[j].1, SpreadMode::Hedged) else {
                continue;
            };
            tested += 1;
            if sp.stats.corr < min_corr {
                continue;
            }
            let s = &sp.stats;
            found.push((
                s.z.abs(),
                json!({
                    "a": series[i].0, "b": series[j].0,
                    "z": round(s.z, 3), "dev_pct": round(s.last, 3),
                    "sigma_pct": round(s.sigma, 3), "corr": round(s.corr, 3),
                    "beta": round(s.beta, 3), "n": s.n,
                }),
            ));
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let matching = found.len();
    json!({
        "timeframe": tf.name(),
        "mode": "beta",
        "symbols": series.len(),
        "pairs_tested": tested,
        "pairs_matching_min_corr": matching,
        "min_corr": min_corr,
        "top": found.into_iter().take(limit.clamp(1, 50)).map(|(_, v)| v).collect::<Vec<_>>(),
        "caveat": "z is measured inside the loaded window; this does not test cointegration",
    })
}

/// Time-ordered prints summarised for an agent: who is hitting (aggressor
/// flow), size and speed, the biggest prints and how the flow evolved.
/// `trades` is oldest -> newest; `recent` raw rows are included (max 100).
pub fn tape_json(symbol: &str, trades: &[AggTrade], recent: usize) -> Result<Value, String> {
    if trades.is_empty() {
        return Err(format!("no recent trades for {symbol}"));
    }
    let recent = recent.min(100);
    let n = trades.len();
    let (t0, t1) = (trades[0].time_ms, trades[n - 1].time_ms);
    let span_ms = (t1 - t0).max(1);
    // `is_buyer_maker` = the aggressor sold.
    let side = |t: &AggTrade| if t.is_buyer_maker { "sell" } else { "buy" };
    let notional = |t: &AggTrade| t.price * t.qty;
    let (mut buy_q, mut sell_q, mut buy_n, mut sell_n) = (0.0, 0.0, 0.0, 0.0);
    let (mut hi, mut lo) = (f64::NEG_INFINITY, f64::INFINITY);
    for t in trades {
        if t.is_buyer_maker {
            sell_q += t.qty;
            sell_n += notional(t);
        } else {
            buy_q += t.qty;
            buy_n += notional(t);
        }
        hi = hi.max(t.price);
        lo = lo.min(t.price);
    }
    let total_n = buy_n + sell_n;
    let total_q = buy_q + sell_q;
    let (first, last) = (trades[0].price, trades[n - 1].price);
    let change_pct = (last - first) / first * 100.0;
    let delta_q = buy_q - sell_q;

    // Biggest prints: the top 10 by notional, but only ones clearly larger
    // than the typical print (>= 5x the median).
    let mut notionals: Vec<f64> = trades.iter().map(notional).collect();
    notionals.sort_by(|a, b| a.total_cmp(b));
    let median = notionals[n / 2];
    let mut big: Vec<&AggTrade> = trades
        .iter()
        .filter(|t| notional(t) >= 5.0 * median)
        .collect();
    big.sort_by(|a, b| notional(b).total_cmp(&notional(a)));
    big.truncate(10);

    // Ten equal time slices: the flow's shape and where price went.
    let buckets = 10usize;
    let mut flow: Vec<(f64, f64, f64, usize)> = vec![(0.0, 0.0, 0.0, 0); buckets]; // buy, sell, close, count
    for t in trades {
        let b = (((t.time_ms - t0) * buckets as i64) / (span_ms + 1)).clamp(0, buckets as i64 - 1)
            as usize;
        let f = &mut flow[b];
        if t.is_buyer_maker {
            f.1 += t.qty;
        } else {
            f.0 += t.qty;
        }
        f.2 = t.price;
        f.3 += 1;
    }
    // Speed: the last tenth of the window against the whole.
    let tail_from = t1 - span_ms / 10;
    let tail = trades.iter().filter(|t| t.time_ms >= tail_from).count();
    let rate = n as f64 / (span_ms as f64 / 1000.0);
    let tail_rate = tail as f64 / ((span_ms / 10).max(1) as f64 / 1000.0);

    let mut notes: Vec<String> = Vec::new();
    let share = if total_q > 0.0 { buy_q / total_q } else { 0.5 };
    if share >= 0.6 {
        notes.push(format!(
            "buyers hit the ask: {:.0}% of volume is aggressive buying",
            share * 100.0
        ));
    } else if share <= 0.4 {
        notes.push(format!(
            "sellers hit the bid: {:.0}% of volume is aggressive selling",
            (1.0 - share) * 100.0
        ));
    } else {
        notes.push("balanced flow".to_string());
    }
    if delta_q > 0.0 && change_pct < -0.02 {
        notes.push("price fell despite net buying: sellers (passive) are absorbing".to_string());
    } else if delta_q < 0.0 && change_pct > 0.02 {
        notes.push("price rose despite net selling: buyers (passive) are absorbing".to_string());
    }
    if tail_rate > 2.0 * rate && tail >= 5 {
        notes.push("trading is accelerating in the last tenth of the window".to_string());
    }
    if !big.is_empty() {
        let (bb, bs) = big.iter().fold((0, 0), |(b, s), t| {
            if t.is_buyer_maker {
                (b, s + 1)
            } else {
                (b + 1, s)
            }
        });
        notes.push(format!(
            "{} large print(s): {bb} buy / {bs} sell",
            big.len()
        ));
    }

    Ok(json!({
        "symbol": symbol,
        "trades": n,
        "from_ms": t0, "to_ms": t1,
        "next_since_ms": t1 + 1,
        "window_seconds": round(span_ms as f64 / 1000.0, 1),
        "trades_per_sec": round(rate, 2),
        "trades_per_sec_last_tenth": round(tail_rate, 2),
        "price": {"first": first, "last": last, "high": hi, "low": lo, "change_pct": round(change_pct, 3)},
        "volume": {
            "total_qty": round(total_q, 6),
            "total_notional": round(total_n, 2),
            "aggressive_buy_qty": round(buy_q, 6),
            "aggressive_sell_qty": round(sell_q, 6),
            "delta_qty": round(delta_q, 6),
            "aggressive_buy_share": round(share, 3),
            "vwap": if total_q > 0.0 { json!(round(total_n / total_q, 8)) } else { Value::Null },
            "median_print_notional": round(median, 2),
        },
        "flow_by_time": flow.iter().enumerate().map(|(i, f)| json!({
            "slice": i, "buy_qty": round(f.0, 6), "sell_qty": round(f.1, 6),
            "delta_qty": round(f.0 - f.1, 6), "last_price": f.2, "trades": f.3,
        })).collect::<Vec<_>>(),
        "large_prints": big.iter().map(|t| json!({
            "time_ms": t.time_ms, "price": t.price, "qty": t.qty,
            "notional": round(notional(t), 2), "aggressor": side(t),
        })).collect::<Vec<_>>(),
        "recent": {
            "columns": ["time_ms", "price", "qty", "aggressor"],
            "rows": trades[n.saturating_sub(recent)..].iter().map(|t| json!([t.time_ms, t.price, t.qty, side(t)])).collect::<Vec<_>>(),
        },
        "notes": notes,
        "note": "aggregated trades from the exchange (one row per taker order slice), not individual fills",
    }))
}

/// Market-wide picture from the screener's live pairs: breadth, volume,
/// leaders and laggards.  Pure (no network).
pub fn overview_json(
    pairs: &[crate::data::binance::TickerSummary],
    top: usize,
    min_qv: f64,
) -> Result<Value, String> {
    let live: Vec<&crate::data::binance::TickerSummary> = pairs
        .iter()
        .filter(|p| p.volume_quote >= min_qv && p.last_price > 0.0)
        .collect();
    if live.is_empty() {
        return Err("no live pairs yet: the screener has not loaded (wait a few seconds)".into());
    }
    let top = top.clamp(1, 20);
    let up = live.iter().filter(|p| p.change_pct > 0.0).count();
    let down = live.iter().filter(|p| p.change_pct < 0.0).count();
    let mut ch: Vec<f64> = live.iter().map(|p| p.change_pct).collect();
    ch.sort_by(|a, b| a.total_cmp(b));
    let median = ch[ch.len() / 2];
    let total_qv: f64 = live.iter().map(|p| p.volume_quote).sum();
    let row = |p: &&crate::data::binance::TickerSummary| {
        json!({"symbol": p.symbol, "last": p.last_price, "change_24h_pct": round(p.change_pct, 2),
               "quote_volume": round(p.volume_quote, 0),
               "range_position": if p.high_24h > p.low_24h { json!(round((p.last_price - p.low_24h) / (p.high_24h - p.low_24h), 2)) } else { Value::Null }})
    };
    let mut by_change = live.clone();
    by_change.sort_by(|a, b| b.change_pct.total_cmp(&a.change_pct));
    let mut by_vol = live.clone();
    by_vol.sort_by(|a, b| b.volume_quote.total_cmp(&a.volume_quote));
    let pick = |sym: &str| live.iter().find(|p| p.symbol == sym).map(row);
    let mood = if up as f64 / live.len() as f64 >= 0.65 {
        "broad strength"
    } else if (down as f64 / live.len() as f64) >= 0.65 {
        "broad weakness"
    } else {
        "mixed"
    };
    Ok(json!({
        "pairs": live.len(),
        "advancing": up, "declining": down,
        "advancing_share": round(up as f64 / live.len() as f64, 3),
        "median_change_24h_pct": round(median, 2),
        "total_quote_volume": round(total_qv, 0),
        "mood": mood,
        "majors": {"BTCUSDT": pick("BTCUSDT"), "ETHUSDT": pick("ETHUSDT")},
        "top_gainers": by_change.iter().take(top).map(row).collect::<Vec<_>>(),
        "top_losers": by_change.iter().rev().take(top).map(row).collect::<Vec<_>>(),
        "top_volume": by_vol.iter().take(top).map(row).collect::<Vec<_>>(),
        "note": "24h figures from the screener's live feed; range_position 0 = at the 24h low, 1 = at the high",
    }))
}

/// Book snapshot + trade-flow summary for the scalper.  `depth` levels per
/// side; imbalance is bid volume over total in the shown levels (0.5 = even).
pub fn orderbook_json(
    symbol: &str,
    depth: usize,
    book: &OrderBookSnapshot,
    trades: &[AggTrade],
) -> Result<Value, String> {
    let depth = depth.clamp(1, 20);
    let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else {
        return Err(format!("empty order book for {symbol}"));
    };
    let levels = |side: &[(f64, f64)]| -> Vec<Value> {
        let mut cum = 0.0;
        side.iter()
            .take(depth)
            .map(|(p, q)| {
                cum += q;
                json!({"price": p, "qty": q, "cum_qty": round(cum, 6)})
            })
            .collect()
    };
    let bid_vol: f64 = book.bids.iter().take(depth).map(|l| l.1).sum();
    let ask_vol: f64 = book.asks.iter().take(depth).map(|l| l.1).sum();
    let mid = (bid + ask) / 2.0;

    // `is_buyer_maker` = the aggressor sold.
    let (mut buy_vol, mut sell_vol, mut notional, mut qty_sum) = (0.0, 0.0, 0.0, 0.0);
    for t in trades {
        if t.is_buyer_maker {
            sell_vol += t.qty;
        } else {
            buy_vol += t.qty;
        }
        notional += t.price * t.qty;
        qty_sum += t.qty;
    }
    let flow = if buy_vol + sell_vol > 0.0 {
        json!(round(buy_vol / (buy_vol + sell_vol), 3))
    } else {
        Value::Null
    };
    Ok(json!({
        "symbol": symbol,
        "best_bid": bid, "best_ask": ask, "mid": round(mid, 8),
        "spread_bps": round((ask - bid) / mid * 10_000.0, 2),
        "bids": levels(&book.bids), "asks": levels(&book.asks),
        "imbalance_bid_share": if bid_vol + ask_vol > 0.0 { json!(round(bid_vol / (bid_vol + ask_vol), 3)) } else { Value::Null },
        "recent_trades": {
            "count": trades.len(),
            "aggressor_buy_volume": round(buy_vol, 6),
            "aggressor_sell_volume": round(sell_vol, 6),
            "aggressor_buy_share": flow,
            "vwap": if qty_sum > 0.0 { json!(round(notional / qty_sum, 8)) } else { Value::Null },
            "last_price": trades.last().map(|t| t.price),
        },
        "note": "a snapshot, not a stream: it is already a moment old when you read it",
    }))
}

/// Liquidations heard in the last `minutes` (all symbols, or `symbol`),
/// at least `min_usd` each: totals by side, per minute, the largest, the
/// most liquidated coins (whole market) and the newest `recent` rows.
pub fn liquidations_json<'a>(
    feed: impl DoubleEndedIterator<Item = &'a crate::data::liquidations::Liquidation>,
    symbol: Option<&str>,
    now_ms: i64,
    minutes: u32,
    min_usd: f64,
    recent: usize,
) -> Value {
    use crate::data::liquidations::{fmt_usd, totals};
    let since = now_ms - minutes as i64 * 60_000;
    let rows: Vec<&crate::data::liquidations::Liquidation> = feed
        .rev()
        .take_while(|l| l.time_ms >= since)
        .filter(|l| symbol.is_none_or(|s| l.symbol == s) && l.usd() >= min_usd)
        .collect();
    let t = totals(rows.iter().copied(), since);
    let row = |l: &crate::data::liquidations::Liquidation| {
        json!({
            "time_ms": l.time_ms,
            "symbol": l.symbol,
            "side": if l.long { "long" } else { "short" },
            "price": l.price,
            "usd": (l.usd() * 100.0).round() / 100.0,
        })
    };
    let mut largest: Vec<_> = rows.clone();
    largest.sort_by(|a, b| b.usd().total_cmp(&a.usd()));
    let mut by: std::collections::HashMap<&str, (f64, f64, usize)> = Default::default();
    for l in &rows {
        let e = by.entry(l.symbol.as_str()).or_default();
        if l.long {
            e.0 += l.usd();
        } else {
            e.1 += l.usd();
        }
        e.2 += 1;
    }
    let mut coins: Vec<_> = by.into_iter().collect();
    coins.sort_by(|a, b| {
        (b.1 .0 + b.1 .1)
            .total_cmp(&(a.1 .0 + a.1 .1))
            .then(a.0.cmp(b.0))
    });
    let total = t.long_usd + t.short_usd;
    let note = if t.count == 0 {
        "none heard in the window (the terminal hears them only while it runs)".to_string()
    } else if t.long_usd > 2.0 * t.short_usd {
        format!(
            "longs are being flushed: {} of longs vs {} of shorts",
            fmt_usd(t.long_usd),
            fmt_usd(t.short_usd)
        )
    } else if t.short_usd > 2.0 * t.long_usd {
        format!(
            "shorts are being squeezed: {} of shorts vs {} of longs",
            fmt_usd(t.short_usd),
            fmt_usd(t.long_usd)
        )
    } else {
        format!("both sides: {} in all", fmt_usd(total))
    };
    let mut out = json!({
        "scope": symbol.unwrap_or("all"),
        "minutes": minutes,
        "min_usd": min_usd,
        "count": t.count,
        "long_usd": t.long_usd.round(),
        "short_usd": t.short_usd.round(),
        "usd_per_minute": (total / minutes.max(1) as f64).round(),
        "note": note,
        "largest": largest.iter().take(5).map(|l| row(l)).collect::<Vec<_>>(),
        "recent": rows.iter().take(recent).map(|l| row(l)).collect::<Vec<_>>(),
        "caveat": "Binance sends only the largest liquidation per symbol per second: every total is a lower bound",
    });
    if symbol.is_none() {
        out["most_liquidated"] = coins
            .iter()
            .take(10)
            .map(|(s, (l, sh, n))| json!({"symbol": s, "long_usd": l.round(), "short_usd": sh.round(), "count": n}))
            .collect();
    }
    out
}

/// Estimated liquidation levels around the price: how much would be
/// liquidated if the price moved 1 / 2 / 5 / 10 % either way, and the
/// largest clusters with their distance.
pub fn liq_levels_json(map: &crate::liqmap::LiqMap, top: usize) -> Value {
    let p = map.price;
    let within = |pct: f64| {
        let (lo, hi) = (p * (1.0 - pct / 100.0), p * (1.0 + pct / 100.0));
        let longs: f64 = map
            .levels
            .iter()
            .filter(|l| l.price >= lo && l.price < p)
            .map(|l| l.long_usd)
            .sum();
        let shorts: f64 = map
            .levels
            .iter()
            .filter(|l| l.price <= hi && l.price > p)
            .map(|l| l.short_usd)
            .sum();
        json!({"pct": pct, "longs_below_usd": longs.round(), "shorts_above_usd": shorts.round()})
    };
    let cluster = |l: &crate::liqmap::Level, long: bool| {
        json!({
            "price": (l.price * 1e8).round() / 1e8,
            "usd": (if long { l.long_usd } else { l.short_usd }).round(),
            "distance_pct": ((l.price / p - 1.0) * 10_000.0).round() / 100.0,
        })
    };
    let mut longs: Vec<_> = map.levels.iter().filter(|l| l.long_usd > 0.0).collect();
    longs.sort_by(|a, b| b.long_usd.total_cmp(&a.long_usd));
    let mut shorts: Vec<_> = map.levels.iter().filter(|l| l.short_usd > 0.0).collect();
    shorts.sort_by(|a, b| b.short_usd.total_cmp(&a.short_usd));
    let (lt, st) = map.totals();
    let moves: Vec<Value> = [1.0, 2.0, 5.0, 10.0].into_iter().map(within).collect();
    json!({
        "price": p,
        "data_from_ms": map.from_ms,
        "data_to_ms": map.to_ms,
        "long_usd": lt.round(),
        "short_usd": st.round(),
        "if_price_moves": moves,
        "long_clusters": longs.iter().take(top).map(|l| cluster(l, true)).collect::<Vec<_>>(),
        "short_clusters": shorts.iter().take(top).map(|l| cluster(l, false)).collect::<Vec<_>>(),
        "model": "an ESTIMATE: open-interest rises are new longs and shorts at that period's price, \
                  spread over 5-100x leverage; levels the price already traded through are gone. \
                  Not the exchange's positions. Price tends to be drawn to large clusters.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(t: i64, c: f64) -> Ohlc {
        Ohlc {
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 1.0,
            date: String::new(),
            time_ms: t * 1000,
            is_gap: false,
        }
    }

    fn series(f: impl Fn(usize) -> f64, n: usize) -> Vec<Ohlc> {
        (0..n).map(|i| k(i as i64 + 1, f(i))).collect()
    }

    #[test]
    fn orderbook_summary_computes_spread_imbalance_and_aggressor_flow() {
        let book = OrderBookSnapshot {
            bids: vec![(99.0, 3.0), (98.0, 1.0)],
            asks: vec![(101.0, 1.0), (102.0, 1.0)],
        };
        let t = |p: f64, q: f64, maker: bool| AggTrade {
            price: p,
            qty: q,
            time_ms: 0,
            is_buyer_maker: maker,
        };
        let trades = [t(100.0, 3.0, false), t(100.0, 1.0, true)];
        let j = orderbook_json("X", 5, &book, &trades).unwrap();
        assert_eq!(j["mid"], 100.0);
        assert_eq!(j["spread_bps"], 200.0);
        assert_eq!(
            j["imbalance_bid_share"], 0.667,
            "4 of 6 shown units are bids"
        );
        assert_eq!(j["bids"][1]["cum_qty"], 4.0);
        assert_eq!(j["recent_trades"]["aggressor_buy_share"], 0.75);
        assert_eq!(j["recent_trades"]["vwap"], 100.0);
        // Depth is clamped and an empty book is an error, not a guess.
        assert_eq!(
            orderbook_json("X", 1, &book, &[]).unwrap()["bids"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            orderbook_json("X", 5, &book, &[]).unwrap()["recent_trades"]["vwap"],
            Value::Null
        );
        let empty = OrderBookSnapshot {
            bids: vec![],
            asks: vec![],
        };
        assert!(orderbook_json("X", 5, &empty, &[]).is_err());
    }

    #[test]
    fn timeframes_and_modes_parse() {
        assert_eq!(parse_timeframe("1H"), Some(Timeframe::H1));
        assert_eq!(parse_timeframe("15s"), None, "no 15s on Binance");
        assert!(parse_spread_mode("beta").is_some() && parse_spread_mode("x").is_none());
        assert!(parse_group_mode("Index").is_some() && parse_group_mode("avg").is_none());
    }

    #[test]
    fn downsample_keeps_the_last_point_and_the_bound() {
        let v: Vec<f64> = (0..1000).map(|i| i as f64).collect();
        let d = downsample(&v, 120);
        assert_eq!(d.len(), 120);
        assert_eq!(*d.last().unwrap(), 999.0);
        assert_eq!(downsample(&v[..5], 120), v[..5].to_vec());
        assert!(downsample(&v, 0).is_empty() && downsample(&[], 5).is_empty());
    }

    #[test]
    fn klines_are_capped_and_newest_last() {
        let c = series(|i| i as f64 + 1.0, 800);
        let j = klines_json("BTCUSDT", Timeframe::H1, &c, 10_000);
        assert_eq!(j["count"], MAX_KLINES);
        let rows = j["candles"].as_array().unwrap();
        assert_eq!(rows.last().unwrap()[4], 800.0);
        assert_eq!(klines_json("X", Timeframe::D1, &c, 3)["count"], 3);
    }

    #[test]
    fn group_chart_reports_change_and_refuses_empty_input() {
        let a = series(|i| 100.0 + i as f64, 40);
        let b = series(|i| 50.0 + i as f64 * 0.5, 40);
        let j = group_chart_json(
            "Main",
            synthetic::Mode::Index,
            Timeframe::H1,
            &[("A".into(), a), ("B".into(), b)],
            0,
        )
        .unwrap();
        assert_eq!(j["symbols_used"], 2);
        assert!(j["change_pct"].as_f64().unwrap() > 0.0);
        assert!(j["closes"].as_array().unwrap().len() <= MAX_POINTS);
        let e = group_chart_json("Empty", synthetic::Mode::Sum, Timeframe::H1, &[], 2).unwrap_err();
        assert!(e.contains("Empty") && e.contains("2 symbols failed"), "{e}");
    }

    #[test]
    fn spread_json_has_stats_and_errors_on_short_history() {
        let b = series(|i| 100.0 + i as f64, 60);
        let a = series(
            |i| (100.0 + i as f64) * 2.0 + if i == 59 { 10.0 } else { 0.0 },
            60,
        );
        let j = spread_json("A", "B", SpreadMode::Ratio, Timeframe::H1, &a, &b).unwrap();
        assert!(
            j["stats"]["dev_pct"].as_f64().unwrap() > 0.0,
            "A jumped above B"
        );
        assert!(j["stats"]["z"].as_f64().unwrap() > 3.0);
        assert_eq!(j["mode"], "ratio");
        let short = series(|i| i as f64 + 1.0, 5);
        assert!(spread_json("A", "B", SpreadMode::Ratio, Timeframe::H1, &short, &short).is_err());
    }

    #[test]
    fn scan_ranks_by_abs_z_and_filters_by_correlation() {
        // Wavy common driver so the correlation of returns is real.
        let wave = |i: usize| 100.0 + 10.0 * (i as f64 / 5.0).sin() + i as f64 * 0.1;
        let a = series(wave, 80);
        let b = series(|i| wave(i) * 2.0, 80);
        let mut c = series(|i| wave(i) * 3.0, 80);
        c.last_mut().unwrap().close *= 1.01; // C breaks away on the last candle
        let flat = series(|_| 7.0, 80);
        let j = scan_pairs_json(
            &[
                ("A".into(), a),
                ("B".into(), b),
                ("C".into(), c),
                ("FLAT".into(), flat),
            ],
            Timeframe::H1,
            0.5,
            10,
        );
        let top = j["top"].as_array().unwrap();
        assert!(!top.is_empty());
        assert!(top.iter().all(|p| p["corr"].as_f64().unwrap() >= 0.5));
        assert!(
            top[0]["a"] == "C" || top[0]["b"] == "C",
            "the pair with the broken-away leg ranks first: {top:?}"
        );
        assert!(
            top.iter().all(|p| p["a"] != "FLAT" && p["b"] != "FLAT"),
            "flat series has no correlation"
        );
        let z0 = top[0]["z"].as_f64().unwrap().abs();
        assert!(top
            .windows(2)
            .all(|w| w[0]["z"].as_f64().unwrap().abs() >= w[1]["z"].as_f64().unwrap().abs()));
        assert!(z0 > 2.0);
    }

    fn tr(t: i64, price: f64, qty: f64, sell: bool) -> AggTrade {
        AggTrade {
            price,
            qty,
            time_ms: t,
            is_buyer_maker: sell,
        }
    }

    #[test]
    fn tape_summarises_flow_size_and_speed() {
        // 100 prints over 10 s: buyers lift the price from 100 to 101, one big sell.
        let mut v: Vec<AggTrade> = (0..100)
            .map(|i| tr(i * 100, 100.0 + i as f64 * 0.01, 1.0, i % 5 == 0))
            .collect();
        v.push(tr(10_000, 100.9, 80.0, true)); // a large aggressive sell
        let r = tape_json("TESTUSDT", &v, 5).unwrap();
        assert_eq!(r["trades"], 101);
        assert!(r["trades_per_sec"].as_f64().unwrap() > 9.0);
        let share = r["volume"]["aggressive_buy_share"].as_f64().unwrap();
        assert!(share < 0.5, "the big sell outweighs the buys: {share}");
        let big = r["large_prints"].as_array().unwrap();
        assert_eq!(big.len(), 1);
        assert_eq!(big[0]["aggressor"], "sell");
        assert_eq!(r["recent"]["rows"].as_array().unwrap().len(), 5);
        assert_eq!(r["flow_by_time"].as_array().unwrap().len(), 10);
        let slices: usize = r["flow_by_time"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["trades"].as_u64().unwrap() as usize)
            .sum();
        assert_eq!(slices, 101, "every print lands in a slice");
        assert!(!r["notes"].as_array().unwrap().is_empty());
        assert!(tape_json("X", &[], 5).is_err());
        // Price up while net selling reads as absorption.
        let mut w: Vec<AggTrade> = (0..50)
            .map(|i| tr(i * 100, 100.0 + i as f64 * 0.05, 1.0, true))
            .collect();
        w.push(tr(5_000, 103.0, 1.0, false));
        let r = tape_json("Y", &w, 0).unwrap();
        assert!(
            r["notes"].to_string().contains("absorbing"),
            "{}",
            r["notes"]
        );
    }

    #[test]
    fn overview_counts_breadth_and_ranks_movers() {
        use crate::data::binance::TickerSummary;
        let p = |s: &str, ch: f64, qv: f64| TickerSummary {
            symbol: s.into(),
            last_price: 10.0,
            change_pct: ch,
            high_24h: 12.0,
            low_24h: 8.0,
            volume_base: 1.0,
            volume_quote: qv,
        };
        let v = vec![
            p("BTCUSDT", 2.0, 9e9),
            p("ETHUSDT", 1.0, 5e9),
            p("AAAUSDT", 30.0, 2e6),
            p("BBBUSDT", -20.0, 3e6),
            p("DUSTUSDT", 50.0, 10.0),
        ];
        let r = overview_json(&v, 2, 1_000.0).unwrap();
        assert_eq!(r["pairs"], 4, "illiquid dust is filtered out");
        assert_eq!(r["advancing"], 3);
        assert_eq!(r["declining"], 1);
        assert_eq!(r["top_gainers"][0]["symbol"], "AAAUSDT");
        assert_eq!(r["top_losers"][0]["symbol"], "BBBUSDT");
        assert_eq!(r["top_volume"][0]["symbol"], "BTCUSDT");
        assert_eq!(r["majors"]["BTCUSDT"]["symbol"], "BTCUSDT");
        assert_eq!(r["top_gainers"][0]["range_position"], 0.5);
        assert!(overview_json(&[], 5, 0.0).is_err());
    }

    #[test]
    fn liquidations_reply_sums_sides_and_scopes() {
        use crate::data::liquidations::Liquidation;
        let l = |sym: &str, long: bool, usd: f64, t: i64| Liquidation {
            symbol: sym.into(),
            long,
            price: 10.0,
            qty: usd / 10.0,
            time_ms: t,
        };
        let now = 10 * 60_000;
        let feed = [
            l("A", true, 100.0, 0), // before the window
            l("A", true, 5_000.0, now - 60_000),
            l("B", false, 1_000.0, now - 30_000),
            l("A", true, 50.0, now - 1_000), // under min_usd
        ];
        let v = liquidations_json(feed.iter(), None, now, 5, 60.0, 10);
        assert_eq!(v["count"], 2);
        assert_eq!(v["long_usd"], 5000.0);
        assert_eq!(v["short_usd"], 1000.0);
        assert_eq!(v["recent"][0]["symbol"], "B", "newest first");
        assert_eq!(v["most_liquidated"][0]["symbol"], "A");
        assert!(v["note"]
            .as_str()
            .unwrap()
            .contains("longs are being flushed"));
        let a = liquidations_json(feed.iter(), Some("B"), now, 5, 0.0, 10);
        assert_eq!(a["count"], 1);
        assert!(a.get("most_liquidated").is_none());
        let none = liquidations_json(feed.iter(), Some("C"), now, 5, 0.0, 10);
        assert!(none["note"].as_str().unwrap().starts_with("none"));
    }

    #[test]
    fn levels_reply_counts_what_a_move_would_hit() {
        use crate::liqmap::{Level, LiqMap};
        let lv = |price: f64, l: f64, s: f64| Level {
            price,
            long_usd: l,
            short_usd: s,
        };
        let map = LiqMap {
            symbol: "X".into(),
            step: 0.1,
            levels: vec![lv(95.0, 7.0, 0.0), lv(99.5, 3.0, 0.0), lv(101.5, 0.0, 4.0)],
            price: 100.0,
            from_ms: 1,
            to_ms: 2,
        };
        let v = liq_levels_json(&map, 2);
        let m = &v["if_price_moves"];
        assert_eq!(m[0]["longs_below_usd"], 3.0);
        assert_eq!(m[0]["shorts_above_usd"], 0.0);
        assert_eq!(m[1]["shorts_above_usd"], 4.0);
        assert_eq!(m[2]["longs_below_usd"], 10.0);
        assert_eq!(v["long_clusters"][0]["price"], 95.0);
        assert_eq!(v["long_clusters"][0]["distance_pct"], -5.0);
        assert_eq!(v["long_clusters"].as_array().unwrap().len(), 2);
    }
}
