//! The market database: what the main database is not for.  `data.db` holds
//! the ledger, the keys and the settings (small, precious); `market.db`
//! holds what the terminal learns about the markets (large, rebuildable):
//! the catalogue of instruments of every venue, the quotes recorded for
//! replays, and, later, candle history.  Its own file, its own schema
//! version, WAL; deleting it loses nothing that cannot be fetched again.
//!
//! Instruments are rows with an id, so lists can be rows of ids (tens of
//! thousands of lists, hundreds of thousands of instruments) and a screen
//! reads one page of them, not the whole.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use crate::exchanges::{Exch, Quote};
use crate::markets::Ohlc;

/// Ordered migrations: entry `i` upgrades `user_version` i -> i + 1.
const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE IF NOT EXISTS instruments (
        id         INTEGER PRIMARY KEY,
        venue      TEXT NOT NULL,           -- Exch::code(): BINANCE, BYBITF, ...
        kind       TEXT NOT NULL,           -- spot | perp
        symbol     TEXT NOT NULL,           -- BTCUSDT
        base       TEXT NOT NULL,
        quote      TEXT NOT NULL,
        first_seen INTEGER NOT NULL,
        last_seen  INTEGER NOT NULL,
        vol_usd    REAL NOT NULL DEFAULT 0, -- 24h dollars at last_seen
        UNIQUE (venue, kind, symbol)
    );
    CREATE INDEX IF NOT EXISTS instruments_symbol ON instruments (symbol);
    CREATE INDEX IF NOT EXISTS instruments_vol ON instruments (vol_usd DESC);
    -- Substring search over names (three-letter pieces).
    CREATE VIRTUAL TABLE IF NOT EXISTS instruments_fts USING fts5(
        symbol, venue, content = 'instruments', content_rowid = 'id', tokenize = 'trigram'
    );
    CREATE TRIGGER IF NOT EXISTS instruments_ai AFTER INSERT ON instruments BEGIN
        INSERT INTO instruments_fts (rowid, symbol, venue) VALUES (new.id, new.symbol, new.venue);
    END;
    CREATE TRIGGER IF NOT EXISTS instruments_ad AFTER DELETE ON instruments BEGIN
        INSERT INTO instruments_fts (instruments_fts, rowid, symbol, venue)
            VALUES ('delete', old.id, old.symbol, old.venue);
    END;
    -- The touch of the instruments in the best cross-exchange rows, every pass.
    CREATE TABLE IF NOT EXISTS cross_quotes (
        ts    INTEGER NOT NULL,
        instr INTEGER NOT NULL,
        bid   REAL NOT NULL,
        ask   REAL NOT NULL
    );
    CREATE INDEX IF NOT EXISTS cross_quotes_ts ON cross_quotes (ts);
    CREATE INDEX IF NOT EXISTS cross_quotes_instr ON cross_quotes (instr, ts);
"#,
    r#"
    -- Closed candles of every series a window has asked for, one row each.
    CREATE TABLE IF NOT EXISTS candles (
        instr INTEGER NOT NULL,
        tf    TEXT NOT NULL,                -- 1m 5m 1h 1d ... as the venue names it
        ts    INTEGER NOT NULL,             -- open time, ms
        o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL, v REAL NOT NULL,
        PRIMARY KEY (instr, tf, ts)
    ) WITHOUT ROWID;
"#,
    r#"
    -- What the venue's own listing says (exchangeInfo): the reference part.
    ALTER TABLE instruments ADD COLUMN asset TEXT NOT NULL DEFAULT 'crypto';
    ALTER TABLE instruments ADD COLUMN tick REAL;
    ALTER TABLE instruments ADD COLUMN lot REAL;
    ALTER TABLE instruments ADD COLUMN min_notional REAL;
    ALTER TABLE instruments ADD COLUMN active INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE instruments ADD COLUMN listed_at INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX IF NOT EXISTS instruments_asset ON instruments (asset);
"#,
    r#"
    -- Funding of perpetuals: each settlement's rate (a fraction of the
    -- position, per the contract's own interval: 8, 4 or 1 h).
    CREATE TABLE IF NOT EXISTS funding (
        instr INTEGER NOT NULL,
        ts    INTEGER NOT NULL,
        rate  REAL NOT NULL,
        PRIMARY KEY (instr, ts)
    ) WITHOUT ROWID;
"#,
];

/// Where it lives: next to the main database (`DOS_DB`'s folder, else `~/.dos`).
pub fn path() -> Option<PathBuf> {
    super::db::db_path().map(|p| p.with_file_name("market.db"))
}

/// A new connection; none in demo mode (the demo never writes the disk).
pub fn open() -> Option<Connection> {
    if crate::data::demo::is_on() {
        return None;
    }
    open_at(&path()?).ok()
}

/// `f` over this thread's connection, opened once (the main loop reads
/// candles for a first paint without paying for an open each time).
pub fn with<R>(f: impl FnOnce(&Connection) -> R) -> Option<R> {
    thread_local! {
        static CONN: std::cell::RefCell<Option<Connection>> = const { std::cell::RefCell::new(None) };
    }
    CONN.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = open();
        }
        slot.as_ref().map(f)
    })
}

pub fn open_memory() -> Result<Connection, rusqlite::Error> {
    let c = Connection::open_in_memory()?;
    migrate(&c)?;
    Ok(c)
}

pub fn open_at(p: &Path) -> Result<Connection, rusqlite::Error> {
    let c = Connection::open_with_flags(
        p,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    let _mode: String = c.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.busy_timeout(std::time::Duration::from_secs(3))?;
    migrate(&c)?;
    Ok(c)
}

fn migrate(c: &Connection) -> Result<(), rusqlite::Error> {
    let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v) {
        let tx = c.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

fn split_pair(symbol: &str) -> (&str, &str) {
    for q in ["USDT", "USDC", "BTC", "ETH"] {
        if let Some(b) = symbol.strip_suffix(q) {
            if !b.is_empty() {
                return (b, q);
            }
        }
    }
    (symbol, "")
}

/// Record the instruments `quotes` name on `ex` (new ones added, the others
/// touched), with their latest dollars.  One transaction.
pub fn upsert_quotes(
    c: &Connection,
    ex: Exch,
    quotes: &[Quote],
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let kind = if ex.is_perp() { "perp" } else { "spot" };
    let tx = c.unchecked_transaction()?;
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO instruments (venue, kind, symbol, base, quote, first_seen, last_seen, vol_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
             ON CONFLICT (venue, kind, symbol) DO UPDATE SET last_seen = ?6, vol_usd = ?7",
        )?;
        for q in quotes {
            let (base, quote) = split_pair(&q.symbol);
            st.execute(params![
                ex.code(),
                kind,
                q.symbol,
                base,
                quote,
                now_ms,
                q.quote_volume
            ])?;
        }
    }
    tx.commit()?;
    Ok(quotes.len())
}

pub fn instrument_count(c: &Connection) -> i64 {
    c.query_row("SELECT COUNT(*) FROM instruments", [], |r| r.get(0))
        .unwrap_or(0)
}

/// The id of an instrument, if known.
pub fn instrument_id(c: &Connection, ex: Exch, symbol: &str) -> Option<i64> {
    let kind = if ex.is_perp() { "perp" } else { "spot" };
    find_instrument(c, ex.code(), kind, symbol)
}

/// The id of an instrument by venue code, kind and symbol, if known (a read:
/// never waits on a writer).
pub fn find_instrument(c: &Connection, venue: &str, kind: &str, symbol: &str) -> Option<i64> {
    c.prepare_cached("SELECT id FROM instruments WHERE venue = ?1 AND kind = ?2 AND symbol = ?3")
        .ok()?
        .query_row(params![venue, kind, symbol], |r| r.get(0))
        .ok()
}

/// One row of a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: i64,
    /// Venue code (`Exch::code()`, or YAHOO).
    pub venue: String,
    /// spot | perp | other
    pub kind: String,
    pub symbol: String,
    /// crypto | stock | commodity | fx | index | other
    pub asset: String,
    pub active: bool,
    /// Dollars a day at the last scan (0: never scanned).
    pub vol_usd: f64,
}

const FOUND_COLS: &str = "i.id, i.venue, i.kind, i.symbol, i.asset, i.active, i.vol_usd";

fn found(r: &rusqlite::Row) -> rusqlite::Result<Found> {
    Ok(Found {
        id: r.get(0)?,
        venue: r.get(1)?,
        kind: r.get(2)?,
        symbol: r.get(3)?,
        asset: r.get(4)?,
        active: r.get(5)?,
        vol_usd: r.get(6)?,
    })
}

/// Instruments whose name contains `query` (three letters or more; shorter
/// asks for a prefix), trading ones first, then the busiest, one page of
/// `limit` from `offset`; with the number of all matches.
pub fn search(
    c: &Connection,
    query: &str,
    limit: usize,
    offset: usize,
) -> rusqlite::Result<(Vec<Found>, i64)> {
    let q: String = query
        .trim()
        .to_uppercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    // The exact name first (every venue's BTCUSDT before WBTCUSDT), then
    // trading ones, then the busiest.
    let order =
        "ORDER BY i.symbol = ?4 DESC, i.active DESC, i.vol_usd DESC, i.id LIMIT ?2 OFFSET ?3";
    // Every form binds ?1 (the empty query to a test that always holds).
    let (from, arg) = if q.is_empty() {
        ("instruments i WHERE ?1 = ''", String::new())
    } else if q.len() >= 3 {
        (
            "instruments_fts f JOIN instruments i ON i.id = f.rowid WHERE instruments_fts MATCH ?1",
            format!("\"{q}\""),
        )
    } else {
        ("instruments i WHERE i.symbol LIKE ?1", format!("{q}%"))
    };
    let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {from}"), [&arg], |r| {
        r.get(0)
    })?;
    let mut st = c.prepare_cached(&format!("SELECT {FOUND_COLS} FROM {from} {order}"))?;
    let rows = st
        .query_map(params![arg, limit as i64, offset as i64, q], found)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((rows, total))
}

/// Per venue: how many instruments the catalogue holds.
pub fn venue_counts(c: &Connection) -> Vec<(String, i64)> {
    let Ok(mut st) =
        c.prepare("SELECT venue, COUNT(*) FROM instruments GROUP BY venue ORDER BY 2 DESC")
    else {
        return Vec::new();
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Record the touch of `rows` (instrument id, bid, ask) at `ts`.
pub fn record_cross(c: &Connection, ts: i64, rows: &[(i64, f64, f64)]) -> rusqlite::Result<()> {
    let tx = c.unchecked_transaction()?;
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO cross_quotes (ts, instr, bid, ask) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (i, b, a) in rows {
            st.execute(params![ts, i, b, a])?;
        }
    }
    tx.commit()
}

/// Drop recordings older than `keep_ms`; returns how many rows went.
pub fn prune_cross(c: &Connection, now_ms: i64, keep_ms: i64) -> rusqlite::Result<usize> {
    c.execute("DELETE FROM cross_quotes WHERE ts < ?1", [now_ms - keep_ms])
}

pub fn cross_rows(c: &Connection) -> i64 {
    c.query_row("SELECT COUNT(*) FROM cross_quotes", [], |r| r.get(0))
        .unwrap_or(0)
}

/// One instrument as its venue's listing (`exchangeInfo`) describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub symbol: String,
    pub base: String,
    pub quote: String,
    /// Trading now (not halted, not delisted).
    pub active: bool,
    /// Price step, quantity step, smallest order in quote currency (0: none).
    pub tick: f64,
    pub lot: f64,
    pub min_notional: f64,
    /// crypto | stock | commodity | fx | index | other
    pub asset: &'static str,
}

/// Write a venue's listing: rows added or updated with what it says, and
/// the venue's instruments it no longer names marked inactive.  Returns
/// (listed, gone inactive).
pub fn upsert_listings(
    c: &Connection,
    venue: &str,
    kind: &str,
    rows: &[Listing],
    now_ms: i64,
) -> rusqlite::Result<(usize, usize)> {
    let tx = c.unchecked_transaction()?;
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO instruments (venue, kind, symbol, base, quote, first_seen, last_seen,
                                      asset, tick, lot, min_notional, active, listed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?6)
             ON CONFLICT (venue, kind, symbol) DO UPDATE SET
                base = ?4, quote = ?5, asset = ?7, tick = ?8, lot = ?9, min_notional = ?10,
                active = ?11, listed_at = ?6",
        )?;
        for l in rows {
            st.execute(params![
                venue,
                kind,
                l.symbol,
                l.base,
                l.quote,
                now_ms,
                l.asset,
                l.tick,
                l.lot,
                l.min_notional,
                l.active
            ])?;
        }
    }
    let gone = tx.execute(
        "UPDATE instruments SET active = 0
         WHERE venue = ?1 AND kind = ?2 AND listed_at < ?3 AND active = 1",
        params![venue, kind, now_ms],
    )?;
    tx.commit()?;
    Ok((rows.len(), gone))
}

/// When a venue's listing was last written (ms; 0: never).
pub fn listed_at(c: &Connection, venue: &str, kind: &str) -> i64 {
    c.query_row(
        "SELECT COALESCE(MAX(listed_at), 0) FROM instruments WHERE venue = ?1 AND kind = ?2",
        params![venue, kind],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Symbols the listings call something other than a coin (tokenized stocks,
/// commodities, FX, indices): the scanner leaves them out.
pub fn non_crypto_symbols(c: &Connection) -> std::collections::HashSet<String> {
    let Ok(mut st) = c.prepare("SELECT DISTINCT symbol FROM instruments WHERE asset != 'crypto'")
    else {
        return Default::default();
    };
    st.query_map([], |r| r.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The id of an instrument, catalogued now when new (a series asked for by
/// a chart that no scan has seen, a stock from Yahoo).
pub fn ensure_instrument(
    c: &Connection,
    venue: &str,
    kind: &str,
    symbol: &str,
    now_ms: i64,
) -> rusqlite::Result<i64> {
    let (base, quote) = split_pair(symbol);
    c.prepare_cached(
        "INSERT INTO instruments (venue, kind, symbol, base, quote, first_seen, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) ON CONFLICT (venue, kind, symbol) DO NOTHING",
    )?
    .execute(params![venue, kind, symbol, base, quote, now_ms])?;
    find_instrument(c, venue, kind, symbol).ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// The newest `want` candles of a series named by venue, kind and symbol
/// (see [`load_candles`]); empty when nothing is on disk or in demo mode.
pub fn series_load(
    venue: &str,
    kind: &str,
    symbol: &str,
    tf: &str,
    want: usize,
    step_ms: Option<i64>,
) -> Vec<Ohlc> {
    with(|c| {
        let id = find_instrument(c, venue, kind, symbol)?;
        load_candles(c, id, tf, want, step_ms).ok()
    })
    .flatten()
    .unwrap_or_default()
}

/// Write a series named by venue, kind and symbol (see [`save_candles`]);
/// call it off the main loop: it may wait for another window's write.
pub fn series_save(
    venue: &str,
    kind: &str,
    symbol: &str,
    tf: &str,
    candles: &[Ohlc],
    open_from_ms: i64,
    now_ms: i64,
) {
    with(|c| {
        if let Ok(id) = ensure_instrument(c, venue, kind, symbol, now_ms) {
            let _ = save_candles(c, id, tf, candles, open_from_ms);
        }
    });
}

/// Write the closed candles of `candles` (gap sentinels and those opened at
/// or after `open_from_ms`, still forming, are left out); a candle already
/// there is replaced.  One transaction; returns how many were written.
pub fn save_candles(
    c: &Connection,
    instr: i64,
    tf: &str,
    candles: &[Ohlc],
    open_from_ms: i64,
) -> rusqlite::Result<usize> {
    let tx = c.unchecked_transaction()?;
    let mut n = 0;
    {
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO candles (instr, tf, ts, o, h, l, c, v)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for k in candles
            .iter()
            .filter(|k| !k.is_gap && k.time_ms > 0 && k.time_ms < open_from_ms)
        {
            st.execute(params![
                instr, tf, k.time_ms, k.open, k.high, k.low, k.close, k.volume
            ])?;
            n += 1;
        }
    }
    tx.commit()?;
    Ok(n)
}

/// The newest `want` candles of a series on disk, oldest first.  With
/// `step_ms` only the unbroken run that ends at the newest one: a series
/// with a hole (two windows filled it at different times) reads short, so
/// the caller fetches it whole instead of drawing a jump.
pub fn load_candles(
    c: &Connection,
    instr: i64,
    tf: &str,
    want: usize,
    step_ms: Option<i64>,
) -> rusqlite::Result<Vec<Ohlc>> {
    let mut st = c.prepare_cached(
        "SELECT ts, o, h, l, c, v FROM candles WHERE instr = ?1 AND tf = ?2
         ORDER BY ts DESC LIMIT ?3",
    )?;
    let mut rows = st
        .query_map(params![instr, tf, want as i64], |r| {
            let ts: i64 = r.get(0)?;
            Ok(Ohlc {
                open: r.get(1)?,
                high: r.get(2)?,
                low: r.get(3)?,
                close: r.get(4)?,
                volume: r.get(5)?,
                date: crate::markets::unix_secs_to_iso_date(ts / 1000),
                time_ms: ts,
                is_gap: false,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(step) = step_ms {
        let unbroken = rows
            .windows(2)
            .position(|w| w[0].time_ms - w[1].time_ms > step)
            .map_or(rows.len(), |i| i + 1);
        rows.truncate(unbroken);
    }
    rows.reverse();
    Ok(rows)
}

/// The newest candle's open time of a series on disk.
pub fn last_candle_ms(c: &Connection, instr: i64, tf: &str) -> Option<i64> {
    c.query_row(
        "SELECT MAX(ts) FROM candles WHERE instr = ?1 AND tf = ?2",
        params![instr, tf],
        |r| r.get(0),
    )
    .ok()
    .flatten()
}

/// Record settlements (ts ms, rate) of a perpetual; one transaction.
pub fn save_funding(c: &Connection, instr: i64, rows: &[(i64, f64)]) -> rusqlite::Result<usize> {
    let tx = c.unchecked_transaction()?;
    {
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO funding (instr, ts, rate) VALUES (?1, ?2, ?3)",
        )?;
        for (ts, rate) in rows.iter().filter(|(_, r)| r.is_finite()) {
            st.execute(params![instr, ts, rate])?;
        }
    }
    tx.commit()?;
    Ok(rows.len())
}

/// Settlements of a perpetual from `from_ms` on, oldest first.
pub fn load_funding(c: &Connection, instr: i64, from_ms: i64) -> rusqlite::Result<Vec<(i64, f64)>> {
    let mut st =
        c.prepare_cached("SELECT ts, rate FROM funding WHERE instr = ?1 AND ts >= ?2 ORDER BY ts")?;
    let rows = st
        .query_map(params![instr, from_ms], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect();
    rows
}

pub fn last_funding_ms(c: &Connection, instr: i64) -> Option<i64> {
    c.query_row(
        "SELECT MAX(ts) FROM funding WHERE instr = ?1",
        [instr],
        |r| r.get(0),
    )
    .ok()
    .flatten()
}

/// The busiest trading instruments of a venue's kind and asset (by the
/// dollars of the last scan), at most `limit`.
pub fn busiest(c: &Connection, venue: &str, kind: &str, asset: &str, limit: usize) -> Vec<String> {
    let Ok(mut st) = c.prepare(
        "SELECT symbol FROM instruments WHERE venue = ?1 AND kind = ?2 AND asset = ?3
         AND active = 1 AND vol_usd > 0 ORDER BY vol_usd DESC LIMIT ?4",
    ) else {
        return Vec::new();
    };
    st.query_map(params![venue, kind, asset, limit as i64], |r| r.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// A venue's instruments of a kind it no longer lists (delisted: Binance
/// still serves their candles and funding, and a replay must see the coins
/// that died).
pub fn inactive(c: &Connection, venue: &str, kind: &str) -> Vec<String> {
    let Ok(mut st) = c.prepare(
        "SELECT symbol FROM instruments WHERE venue = ?1 AND kind = ?2 AND active = 0
         ORDER BY symbol",
    ) else {
        return Vec::new();
    };
    st.query_map(params![venue, kind], |r| r.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

pub fn candle_rows(c: &Connection) -> i64 {
    c.query_row("SELECT COUNT(*) FROM candles", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Drop every candle (Settings: Clear cache); the catalogue stays.
pub fn clear_candles(c: &Connection) -> rusqlite::Result<usize> {
    c.execute("DELETE FROM candles", [])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(sym: &str, vol: f64) -> Quote {
        Quote {
            symbol: sym.into(),
            bid: 1.0,
            ask: 1.01,
            last: 1.0,
            quote_volume: vol,
        }
    }

    #[test]
    fn instruments_are_added_once_and_found_by_a_piece_of_the_name() {
        let c = open_memory().unwrap();
        upsert_quotes(
            &c,
            Exch::Binance,
            &[q("BTCUSDT", 9e9), q("PEPEUSDT", 1e8), q("WBTCUSDT", 1e6)],
            10,
        )
        .unwrap();
        upsert_quotes(&c, Exch::BybitFut, &[q("BTCUSDT", 5e9)], 20).unwrap();
        // The same again: touched, not doubled.
        upsert_quotes(&c, Exch::Binance, &[q("BTCUSDT", 8e9)], 30).unwrap();
        assert_eq!(instrument_count(&c), 4);
        // Spot and the perpetual of the same name are two instruments.
        let (rows, total) = search(&c, "btc", 10, 0).unwrap();
        assert_eq!(total, 3, "{rows:?}");
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(
            rows[0].vol_usd, 8e9,
            "the busiest first, with its latest dollars"
        );
        assert_eq!((rows[0].asset.as_str(), rows[0].active), ("crypto", true));
        // A short query is a prefix; no query lists the busiest.
        assert_eq!(search(&c, "PE", 10, 0).unwrap().1, 1);
        let (all, n) = search(&c, "", 2, 0).unwrap();
        assert_eq!((all.len(), n), (2, 4));
        assert_eq!(search(&c, "", 2, 2).unwrap().0.len(), 2);
        let (b, quote) = split_pair("PEPEUSDT");
        assert_eq!((b, quote), ("PEPE", "USDT"));
        assert!(instrument_id(&c, Exch::BybitFut, "BTCUSDT").is_some());
        assert!(instrument_id(&c, Exch::Bybit, "BTCUSDT").is_none());
        assert_eq!(venue_counts(&c).len(), 2);
        // The exact name before a busier one that only contains it.
        upsert_quotes(&c, Exch::Okx, &[q("WBTCUSDT", 1e12)], 40).unwrap();
        let (exact, _) = search(&c, "btcusdt", 10, 0).unwrap();
        assert!(
            exact[..2].iter().all(|f| f.symbol == "BTCUSDT"),
            "{exact:?}"
        );
    }

    fn listing(sym: &str, asset: &'static str) -> Listing {
        Listing {
            symbol: sym.into(),
            base: sym.trim_end_matches("USDT").into(),
            quote: "USDT".into(),
            active: true,
            tick: 0.01,
            lot: 0.001,
            min_notional: 5.0,
            asset,
        }
    }

    #[test]
    fn listings_fill_the_reference_and_retire_what_is_gone() {
        let c = open_memory().unwrap();
        // A scan saw these first (dollars known, no reference yet).
        upsert_quotes(
            &c,
            Exch::Binance,
            &[q("BTCUSDT", 9e9), q("GONEUSDT", 1e3)],
            1,
        )
        .unwrap();
        let rows = [listing("BTCUSDT", "crypto"), listing("TSLABUSDT", "stock")];
        assert_eq!(
            upsert_listings(&c, "BINANCE", "spot", &rows, 10).unwrap(),
            (2, 1)
        );
        assert_eq!(listed_at(&c, "BINANCE", "spot"), 10);
        assert_eq!(listed_at(&c, "BINANCEF", "perp"), 0);
        let (found, total) = search(&c, "", 10, 0).unwrap();
        assert_eq!(total, 3);
        let gone = found.iter().find(|f| f.symbol == "GONEUSDT").unwrap();
        assert!(!gone.active, "no longer listed");
        assert_eq!(
            found.last().unwrap().symbol,
            "GONEUSDT",
            "inactive ones last"
        );
        let btc = &found[0];
        assert_eq!(
            (btc.symbol.as_str(), btc.vol_usd),
            ("BTCUSDT", 9e9),
            "the scan's dollars kept"
        );
        let other = non_crypto_symbols(&c);
        assert!(other.contains("TSLABUSDT") && !other.contains("BTCUSDT"));
        // Listed again later: back to active.
        upsert_listings(&c, "BINANCE", "spot", &[listing("GONEUSDT", "crypto")], 20).unwrap();
        assert!(search(&c, "GONE", 10, 0).unwrap().0[0].active);
    }

    #[test]
    fn funding_is_kept_once_per_settlement_and_read_from_a_time() {
        let c = open_memory().unwrap();
        let id = ensure_instrument(&c, "BINANCEF", "perp", "BTCUSDT", 1).unwrap();
        assert_eq!(last_funding_ms(&c, id), None);
        save_funding(&c, id, &[(100, 0.0001), (200, -0.0002), (300, f64::NAN)]).unwrap();
        save_funding(&c, id, &[(200, -0.0003)]).unwrap();
        assert_eq!(last_funding_ms(&c, id), Some(200), "NaN is not kept");
        assert_eq!(load_funding(&c, id, 150).unwrap(), vec![(200, -0.0003)]);
        assert_eq!(load_funding(&c, id, 0).unwrap().len(), 2);
        upsert_quotes(
            &c,
            Exch::BinanceFut,
            &[q("BTCUSDT", 9e9), q("ETHUSDT", 5e9)],
            2,
        )
        .unwrap();
        assert_eq!(
            busiest(&c, "BINANCEF", "perp", "crypto", 1),
            vec!["BTCUSDT".to_string()]
        );
    }

    #[test]
    fn recorded_quotes_are_pruned_by_age() {
        let c = open_memory().unwrap();
        record_cross(&c, 100, &[(1, 1.0, 1.1), (2, 2.0, 2.1)]).unwrap();
        record_cross(&c, 200, &[(1, 1.0, 1.1)]).unwrap();
        assert_eq!(cross_rows(&c), 3);
        assert_eq!(prune_cross(&c, 250, 100).unwrap(), 2);
        assert_eq!(cross_rows(&c), 1);
    }

    fn k(t: i64) -> Ohlc {
        Ohlc {
            open: t as f64,
            high: t as f64 + 1.0,
            low: t as f64 - 1.0,
            close: t as f64,
            volume: 0.5,
            date: String::new(),
            time_ms: t,
            is_gap: false,
        }
    }

    #[test]
    fn candles_keep_only_closed_ones_and_read_the_newest_unbroken_run() {
        let c = open_memory().unwrap();
        let id = ensure_instrument(&c, "BINANCE", "spot", "BTCUSDT", 1).unwrap();
        assert_eq!(
            ensure_instrument(&c, "BINANCE", "spot", "BTCUSDT", 2).unwrap(),
            id
        );
        // 10..=50 step 10; the one at 50 is still forming.
        let s: Vec<Ohlc> = (1..=5).map(|i| k(i * 10)).collect();
        assert_eq!(save_candles(&c, id, "1m", &s, 50).unwrap(), 4);
        assert_eq!(last_candle_ms(&c, id, "1m"), Some(40));
        // The tail again, now closed, with a changed price: replaced, not doubled.
        let mut tail = vec![k(40), k(50)];
        tail[0].close = 99.0;
        save_candles(&c, id, "1m", &tail, 60).unwrap();
        assert_eq!(candle_rows(&c), 5);
        let got = load_candles(&c, id, "1m", 3, Some(10)).unwrap();
        let ts: Vec<i64> = got.iter().map(|k| k.time_ms).collect();
        assert_eq!(ts, vec![30, 40, 50], "oldest first");
        assert_eq!(got[1].close, 99.0);
        assert_eq!(got[0].volume, 0.5);
        // A hole (60..80 missing): only the run after it counts.
        save_candles(&c, id, "1m", &[k(90), k(100)], 1000).unwrap();
        let run = load_candles(&c, id, "1m", 10, Some(10)).unwrap();
        assert_eq!(run.len(), 2, "{run:?}");
        // Without a step (stocks: weekends) everything asked is read.
        assert_eq!(load_candles(&c, id, "1m", 10, None).unwrap().len(), 7);
        // Another timeframe, another series; gaps are never written.
        let mut gap = k(10);
        gap.is_gap = true;
        assert_eq!(save_candles(&c, id, "5m", &[gap], 1000).unwrap(), 0);
        assert!(load_candles(&c, id, "5m", 10, None).unwrap().is_empty());
        assert_eq!(clear_candles(&c).unwrap(), 7);
        assert_eq!(instrument_count(&c), 1, "the catalogue survives");
    }

    #[test]
    fn a_file_database_migrates_once_and_reopens() {
        let dir = std::env::temp_dir().join(format!("dos-market-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("market.db");
        {
            let c = open_at(&p).unwrap();
            upsert_quotes(&c, Exch::Okx, &[q("ETHUSDT", 1.0)], 1).unwrap();
        }
        let c = open_at(&p).unwrap();
        assert_eq!(instrument_count(&c), 1);
        let v: usize = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len());
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
