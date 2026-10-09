//! SQLite-backed persistence: settings, OHLC cache, strategy slots, paper ledger.
//!
//! Single-file DB at `~/.dos/data.db` (override with `DOS_DB`).  The schema is
//! versioned with `PRAGMA user_version` and upgraded by [`MIGRATIONS`]; the
//! file is created `0600` because it holds API secrets.  If the DB cannot be
//! opened the caller gets the error and must tell the user — the app keeps
//! running with nothing persisted.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

/// Ordered schema migrations; entry `i` upgrades `user_version` i → i+1.
/// Never edit a shipped entry — append a new one.
const MIGRATIONS: &[&str] = &[
    // v1 — baseline.  Also drops the tables of the removed paper-dashboard
    // simulator (`positions`, `trades`) and its stale kv keys.
    r#"
    CREATE TABLE IF NOT EXISTS kv (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS ohlc_cache (
        ticker      TEXT NOT NULL,
        timeframe   TEXT NOT NULL,
        fetched_at  INTEGER NOT NULL,
        payload     TEXT NOT NULL,    -- JSON [[o,h,l,c,vol,date,time_ms], ...]
        PRIMARY KEY (ticker, timeframe)
    );
    CREATE TABLE IF NOT EXISTS strategy_slots (
        symbol       TEXT PRIMARY KEY,
        venue        TEXT NOT NULL,
        strategy_id  TEXT NOT NULL,
        status       TEXT NOT NULL DEFAULT 'idle',  -- idle | running
        added_at     INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS strategy_params (
        symbol  TEXT NOT NULL,
        key     TEXT NOT NULL,
        value   TEXT NOT NULL,                       -- JSON-encoded ParamValue
        PRIMARY KEY (symbol, key)
    );
    DROP TABLE IF EXISTS positions;
    DROP TABLE IF EXISTS trades;
    DELETE FROM kv WHERE key IN ('balance', 'clock_secs');
    "#,
    // v2 — a slot is identified by (symbol, venue): Spot and Futures of the
    // same symbol are different instruments.
    r#"
    CREATE TABLE strategy_slots_v2 (
        symbol       TEXT NOT NULL,
        venue        TEXT NOT NULL,
        strategy_id  TEXT NOT NULL,
        status       TEXT NOT NULL DEFAULT 'idle',
        added_at     INTEGER NOT NULL,
        PRIMARY KEY (symbol, venue)
    );
    INSERT INTO strategy_slots_v2 (symbol, venue, strategy_id, status, added_at)
        SELECT symbol, venue, strategy_id, status, added_at FROM strategy_slots;
    DROP TABLE strategy_slots;
    ALTER TABLE strategy_slots_v2 RENAME TO strategy_slots;
    "#,
];

pub fn db_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DOS_DB") {
        return Some(PathBuf::from(p));
    }
    // Windows has no HOME; USERPROFILE is its equivalent.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let dir = PathBuf::from(home).join(".dos");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    restrict_permissions(&dir, 0o700);
    Some(dir.join("data.db"))
}

/// Owner-only access (unix); a no-op elsewhere.
fn restrict_permissions(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

pub fn open() -> Result<Connection, rusqlite::Error> {
    let path = db_path().ok_or_else(|| {
        rusqlite::Error::InvalidPath("neither HOME nor USERPROFILE is set".into())
    })?;
    open_at(&path)
}

/// Demo mode: a private in-memory database, nothing touches the disk.
pub fn open_memory() -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

/// Open (creating if needed) the database at `path`: owner-only file, WAL,
/// then bring the schema up to date.
pub fn open_at(path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    // Before WAL creates its side files: they inherit the main file's mode.
    restrict_permissions(path, 0o600);
    let _mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Deleted / overwritten values (a cleared API key) are zeroed on disk, not
    // left in free pages.
    conn.pragma_update(None, "secure_delete", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Current schema version (`PRAGMA user_version`).
pub fn schema_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0)
}

fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    let current = schema_version(conn).max(0) as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

pub fn save_kv(conn: &Connection, key: &str, value: &str) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn load_kv(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

// ─── OHLC cache ──────────────────────────────────────────────────────────

use crate::markets::Ohlc;

pub fn cache_save(
    conn: &Connection,
    ticker: &str,
    timeframe: &str,
    data: &[Ohlc],
) -> Result<(), rusqlite::Error> {
    let payload: Vec<(f64, f64, f64, f64, f64, String, i64)> = data
        .iter()
        .map(|o| {
            (
                o.open,
                o.high,
                o.low,
                o.close,
                o.volume,
                o.date.clone(),
                o.time_ms,
            )
        })
        .collect();
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "[]".into());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO ohlc_cache (ticker, timeframe, fetched_at, payload) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(ticker, timeframe) DO UPDATE SET fetched_at = excluded.fetched_at,
            payload = excluded.payload",
        params![ticker, timeframe, now, json],
    )?;
    Ok(())
}

pub fn cache_load(conn: &Connection, ticker: &str, timeframe: &str) -> Option<(Vec<Ohlc>, i64)> {
    let (json, ts): (String, i64) = conn
        .query_row(
            "SELECT payload, fetched_at FROM ohlc_cache WHERE ticker = ?1 AND timeframe = ?2",
            params![ticker, timeframe],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok()?;
    let raw: Vec<(f64, f64, f64, f64, f64, String, i64)> = serde_json::from_str(&json).ok()?;
    let data = raw
        .into_iter()
        .map(|(o, h, l, c, v, d, t)| Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            date: d,
            time_ms: t,
            is_gap: false,
        })
        .collect();
    Some((data, ts))
}

pub fn db_file_size_bytes() -> Option<u64> {
    let p = db_path()?;
    std::fs::metadata(p).ok().map(|m| m.len())
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[(&str, u64)] = &[("GB", 1_073_741_824), ("MB", 1_048_576), ("KB", 1_024)];
    for (unit, base) in UNITS {
        if n >= *base {
            let v = n as f64 / *base as f64;
            // Hide tiny fractions: show 1 decimal up to <100, integer otherwise.
            return if v < 100.0 {
                format!("{:.1} {}", v, unit)
            } else {
                format!("{:.0} {}", v, unit)
            };
        }
    }
    format!("{} B", n)
}

/// Drop the OHLC cache.  Touches nothing else: settings, API keys, strategy
/// slots and the paper ledger survive (use [`reset_paper_ledger`] for that).
pub fn clear_cache(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM ohlc_cache", [])?;
    Ok(())
}

/// Forget the persisted paper ledger (broker positions/orders + risk state).
pub fn reset_paper_ledger(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "DELETE FROM kv WHERE key IN ('paper_broker', 'paper_risk')",
        [],
    )?;
    Ok(())
}

/// One persisted strategy slot.
#[derive(Clone, Debug)]
pub struct StoredSlot {
    pub symbol: String,
    pub venue: String,
    pub strategy_id: String,
    pub running: bool,
}

pub fn save_slot(
    conn: &Connection,
    slot: &StoredSlot,
    added_at: i64,
) -> Result<(), rusqlite::Error> {
    let status = if slot.running { "running" } else { "idle" };
    conn.execute(
        "INSERT INTO strategy_slots (symbol, venue, strategy_id, status, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(symbol, venue) DO UPDATE SET
            strategy_id = excluded.strategy_id,
            status      = excluded.status",
        params![slot.symbol, slot.venue, slot.strategy_id, status, added_at],
    )?;
    // The in-memory selection is still one slot per symbol, so a venue
    // change must replace the stored row rather than add a second one.
    conn.execute(
        "DELETE FROM strategy_slots WHERE symbol = ?1 AND venue <> ?2",
        params![slot.symbol, slot.venue],
    )?;
    Ok(())
}

pub fn delete_slot(conn: &Connection, symbol: &str) -> Result<(), rusqlite::Error> {
    conn.execute(
        "DELETE FROM strategy_slots WHERE symbol = ?1",
        params![symbol],
    )?;
    conn.execute(
        "DELETE FROM strategy_params WHERE symbol = ?1",
        params![symbol],
    )?;
    Ok(())
}

pub fn load_slots(conn: &Connection) -> Result<Vec<StoredSlot>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT symbol, venue, strategy_id, status FROM strategy_slots ORDER BY added_at",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let status: String = row.get(3)?;
            Ok(StoredSlot {
                symbol: row.get(0)?,
                venue: row.get(1)?,
                strategy_id: row.get(2)?,
                running: status == "running",
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn save_param(
    conn: &Connection,
    symbol: &str,
    key: &str,
    value_json: &str,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO strategy_params (symbol, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(symbol, key) DO UPDATE SET value = excluded.value",
        params![symbol, key, value_json],
    )?;
    Ok(())
}

pub fn load_params(
    conn: &Connection,
    symbol: &str,
) -> Result<Vec<(String, String)>, rusqlite::Error> {
    let mut stmt = conn.prepare("SELECT key, value FROM strategy_params WHERE symbol = ?1")?;
    let rows = stmt
        .query_map(params![symbol], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─── Paper broker persistence ────────────────────────────────────────
//
// Paper trades never leave the local broker, so the user's positions
// only exist in the running process unless we snapshot them to disk.
// We round-trip the broker as a JSON blob in the kv table — small,
// self-describing, schema-less.  Risk state goes in the same envelope.

pub fn save_paper_snapshot(
    conn: &Connection,
    broker_json: &str,
    risk_json: &str,
) -> Result<(), rusqlite::Error> {
    save_kv(conn, "paper_broker", broker_json)?;
    save_kv(conn, "paper_risk", risk_json)?;
    Ok(())
}

pub fn load_paper_snapshot(conn: &Connection) -> (Option<String>, Option<String>) {
    (load_kv(conn, "paper_broker"), load_kv(conn, "paper_risk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    fn bar(vol: f64, t: i64) -> Ohlc {
        Ohlc {
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: vol,
            date: "2026-01-01".into(),
            time_ms: t,
            is_gap: false,
        }
    }

    #[test]
    fn ohlc_cache_roundtrip_keeps_fractional_volume() {
        let c = mem();
        cache_save(
            &c,
            "BTCUSDT",
            "1m",
            &[bar(12.7, 60_000), bar(0.004, 120_000)],
        )
        .unwrap();
        let (data, _) = cache_load(&c, "BTCUSDT", "1m").unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].volume, 12.7);
        assert_eq!(data[1].volume, 0.004);
        assert_eq!(data[1].time_ms, 120_000);
        // Keyed by (ticker, timeframe).
        assert!(cache_load(&c, "BTCUSDT", "5m").is_none());
    }

    #[test]
    fn legacy_integer_volume_payload_still_loads() {
        let c = mem();
        c.execute(
            "INSERT INTO ohlc_cache (ticker, timeframe, fetched_at, payload) VALUES ('X','1d',0,?1)",
            params![r#"[[1.0,2.0,0.5,1.5,1000,"2026-01-01",1]]"#],
        )
        .unwrap();
        let (data, _) = cache_load(&c, "X", "1d").unwrap();
        assert_eq!(data[0].volume, 1000.0);
    }

    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("dos-db-test-{tag}-{}.db", std::process::id()));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
        p
    }

    #[test]
    fn fresh_db_is_at_latest_version_and_wal() {
        let path = temp_path("fresh");
        let c = open_at(&path).unwrap();
        assert_eq!(schema_version(&c), MIGRATIONS.len() as i64);
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let sync: i64 = c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!(sync, 1, "NORMAL");
        // Re-opening is idempotent.
        drop(c);
        let c = open_at(&path).unwrap();
        assert_eq!(schema_version(&c), MIGRATIONS.len() as i64);
    }

    #[cfg(unix)]
    #[test]
    fn db_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("perm");
        let c = open_at(&path).unwrap();
        save_kv(&c, "binance_api_secret", "s3cret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn legacy_db_is_migrated_in_place() {
        let path = temp_path("legacy");
        {
            // A pre-versioning database: user_version 0, old tables, slot PK = symbol.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE positions (ticker TEXT PRIMARY KEY, qty INTEGER, entry REAL, current REAL);
                 CREATE TABLE trades (id INTEGER PRIMARY KEY, ts TEXT);
                 CREATE TABLE strategy_slots (symbol TEXT PRIMARY KEY, venue TEXT NOT NULL,
                    strategy_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'idle', added_at INTEGER NOT NULL);
                 INSERT INTO kv VALUES ('balance','100'), ('market','Crypto'), ('paper_broker','{}');
                 INSERT INTO strategy_slots VALUES ('BTCUSDT','spot','hedge_grid','running',1);",
            )
            .unwrap();
        }
        let c = open_at(&path).unwrap();
        assert_eq!(schema_version(&c), MIGRATIONS.len() as i64);
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(!tables.contains(&"positions".to_string()));
        assert!(!tables.contains(&"trades".to_string()));
        assert_eq!(load_kv(&c, "balance"), None, "stale simulator key dropped");
        assert_eq!(load_kv(&c, "market").as_deref(), Some("Crypto"));
        let slots = load_slots(&c).unwrap();
        assert_eq!(slots.len(), 1);
        assert!(
            slots[0].running,
            "existing slot data survives the PK change"
        );
    }

    #[test]
    fn slot_identity_is_symbol_and_venue() {
        let c = mem();
        let slot = |venue: &str| StoredSlot {
            symbol: "BTCUSDT".into(),
            venue: venue.into(),
            strategy_id: "hedge_grid".into(),
            running: false,
        };
        save_slot(&c, &slot("spot"), 1).unwrap();
        // Same symbol, other venue: replaces (one slot per symbol in memory).
        save_slot(&c, &slot("futures"), 2).unwrap();
        let rows = load_slots(&c).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].venue, "futures");
        // The schema itself allows both (PRIMARY KEY (symbol, venue)).
        c.execute(
            "INSERT INTO strategy_slots (symbol, venue, strategy_id, added_at) VALUES ('BTCUSDT','spot','x',3)",
            [],
        )
        .unwrap();
        assert_eq!(load_slots(&c).unwrap().len(), 2);
    }

    #[test]
    fn clear_cache_only_touches_the_ohlc_cache() {
        let c = mem();
        cache_save(&c, "BTCUSDT", "1m", &[bar(1.0, 1)]).unwrap();
        save_kv(&c, "binance_futures_api_secret", "f").unwrap();
        save_kv(&c, "market", "Crypto").unwrap();
        save_paper_snapshot(&c, "{\"cash\":1}", "{}").unwrap();
        clear_cache(&c).unwrap();
        assert!(cache_load(&c, "BTCUSDT", "1m").is_none());
        assert_eq!(
            load_kv(&c, "binance_futures_api_secret").as_deref(),
            Some("f")
        );
        assert_eq!(load_kv(&c, "market").as_deref(), Some("Crypto"));
        assert!(
            load_paper_snapshot(&c).0.is_some(),
            "ledger must survive Clear cache"
        );
        reset_paper_ledger(&c).unwrap();
        assert_eq!(load_paper_snapshot(&c), (None, None));
        assert_eq!(load_kv(&c, "market").as_deref(), Some("Crypto"));
    }
}
