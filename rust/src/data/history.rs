//! History the replays read (`crate::carry`, later every rule): funding
//! settlements kept in `market.db` and topped up from the last one, hourly
//! candles of both legs through the candle store (`binance::klines_cached`,
//! on disk since stage 1 of docs/PLAN-SQLITE.md).  Network: call it from a
//! worker thread.

use crate::carry::{align, Series};
use crate::data::binance::{self, Venue};
use crate::data::marketdb;

/// The settlements of `symbol`'s perpetual over the last `days`: what
/// `market.db` holds, topped up from its newest one (all `days` the first
/// time).  Without the database (demo, no disk) straight from the exchange.
pub fn funding(symbol: &str, days: i64, now_ms: i64) -> Vec<(i64, f64)> {
    let from = now_ms - days * 86_400_000;
    let stored = marketdb::with(|c| {
        let id = marketdb::ensure_instrument(c, "BINANCEF", "perp", symbol, now_ms).ok()?;
        let start = marketdb::last_funding_ms(c, id).map_or(from, |t| (t + 1).max(from));
        if let Ok(new) = binance::fetch_funding_history(symbol, start) {
            let _ = marketdb::save_funding(c, id, &new);
        }
        marketdb::load_funding(c, id, from).ok()
    })
    .flatten();
    stored.unwrap_or_else(|| binance::fetch_funding_history(symbol, from).unwrap_or_default())
}

/// A coin's carry series: `days` of hourly spot and perpetual closes and
/// its settlements.  `None` when a leg has no history.
pub fn carry_series(symbol: &str, days: i64, now_ms: i64) -> Option<Series> {
    let hours = (days * 24).clamp(48, binance::MAX_HISTORY as i64) as u32;
    let spot = binance::fetch_klines_history(symbol, "1h", hours, Venue::Spot).ok()?;
    let perp = binance::fetch_klines_history(symbol, "1h", hours, Venue::Futures).ok()?;
    let hours = align(&spot, &perp);
    if hours.len() < 48 {
        return None;
    }
    Some(Series {
        symbol: symbol.to_string(),
        hours,
        funding: funding(symbol, days, now_ms),
    })
}

/// The coins a carry replay looks at: the busiest crypto perpetuals the
/// catalogue knows that the scanner would trade, at most `n`.
pub fn carry_universe(n: usize) -> Vec<String> {
    marketdb::with(|c| marketdb::busiest(c, "BINANCEF", "perp", "crypto", n * 2))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| crate::scanner::tradable(s))
        .take(n)
        .collect()
}

/// The perpetuals a stat-arb replay looks at: the `n` busiest USDT crypto
/// perpetuals the catalogue knows that the scanner would trade and, with
/// `delisted`, the ones Binance no longer lists (it still serves their
/// candles and funding), so the replay sees the coins that died.  Fills
/// the catalogue first when it is empty.
pub fn perp_universe(n: usize, delisted: bool, now_ms: i64) -> Vec<String> {
    ensure_perp_catalogue(now_ms);
    marketdb::with(|c| {
        let keep = |s: &String| s.ends_with("USDT") && crate::scanner::tradable(s);
        let mut out: Vec<String> = marketdb::busiest(c, "BINANCEF", "perp", "crypto", n * 2)
            .into_iter()
            .filter(keep)
            .take(n)
            .collect();
        if delisted {
            for s in marketdb::inactive(c, "BINANCEF", "perp") {
                if keep(&s) && !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        out
    })
    .unwrap_or_default()
}

/// Binance's listings and today's dollars written to `market.db` for the
/// venues that hold no busy instrument yet (a fresh database, a probe run
/// outside the app).  Network; `true` when both catalogues are usable.
pub fn ensure_catalogue(now_ms: i64) -> bool {
    use crate::exchanges::Exch;
    marketdb::with(|c| {
        let mut ok = true;
        for (venue, code, kind, ex) in [
            (Venue::Spot, "BINANCE", "spot", Exch::Binance),
            (Venue::Futures, "BINANCEF", "perp", Exch::BinanceFut),
        ] {
            if !marketdb::busiest(c, code, kind, "crypto", 1).is_empty() {
                continue;
            }
            let Ok(rows) = binance::fetch_listings(venue) else {
                ok = false;
                continue;
            };
            if marketdb::upsert_listings(c, code, kind, &rows, now_ms).is_err() {
                ok = false;
                continue;
            }
            match crate::exchanges::fetch(ex) {
                Ok(q) => ok &= marketdb::upsert_quotes(c, ex, &q, now_ms).is_ok(),
                Err(_) => ok = false,
            }
        }
        ok
    })
    .unwrap_or(false)
}

/// [`ensure_catalogue`] by its older name.
pub fn ensure_perp_catalogue(now_ms: i64) -> bool {
    ensure_catalogue(now_ms)
}

/// The `n` busiest USDT spot coins the scanner would trade: what the
/// minute replays and the pullback test look at.
pub fn spot_universe(n: usize, now_ms: i64) -> Vec<String> {
    ensure_catalogue(now_ms);
    marketdb::with(|c| marketdb::busiest(c, "BINANCE", "spot", "crypto", n * 3))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.ends_with("USDT") && crate::scanner::tradable(s))
        .take(n)
        .collect()
}
