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
    // v3 — audit log of every agent tool call (agent connector).
    r#"
    CREATE TABLE IF NOT EXISTS agent_log (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        ts      INTEGER NOT NULL,
        role    TEXT NOT NULL,
        tool    TEXT NOT NULL,
        args    TEXT NOT NULL,
        status  TEXT NOT NULL
    );
    "#,
    // v4 — research notes written by the researcher agent.
    r#"
    CREATE TABLE IF NOT EXISTS agent_notes (
        id     INTEGER PRIMARY KEY AUTOINCREMENT,
        ts     INTEGER NOT NULL,
        role   TEXT NOT NULL,
        title  TEXT NOT NULL,
        body   TEXT NOT NULL,
        tags   TEXT NOT NULL
    );
    "#,
    // v5 — history of scalper mandates (a mandate itself never survives a restart).
    r#"
    CREATE TABLE IF NOT EXISTS mandates (
        id               INTEGER PRIMARY KEY,
        issued_ms        INTEGER NOT NULL,
        list             TEXT NOT NULL,
        max_order_usd    REAL NOT NULL,
        max_orders_hour  INTEGER NOT NULL,
        max_loss_usd     REAL NOT NULL,
        expires_ms       INTEGER NOT NULL,
        status           TEXT NOT NULL,
        ended_ms         INTEGER,
        orders_total     INTEGER NOT NULL DEFAULT 0,
        loss_usd         REAL NOT NULL DEFAULT 0
    );
    "#,
    // v6 — each mandate trades its own paper ledger: its starting balance,
    // its PnL and the ledger itself (BrokerSnapshot JSON), kept after the end.
    r#"
    ALTER TABLE mandates ADD COLUMN balance REAL NOT NULL DEFAULT 0;
    ALTER TABLE mandates ADD COLUMN pnl_usd REAL NOT NULL DEFAULT 0;
    ALTER TABLE mandates ADD COLUMN ledger TEXT;
    "#,
    // v7 — liquidations heard on Binance Futures (`data::liquidations`),
    // kept a few days so a chart has them after a restart.  Several windows
    // hear the same events: the unique key keeps one row each.
    r#"
    CREATE TABLE IF NOT EXISTS liquidations (
        symbol   TEXT NOT NULL,
        time_ms  INTEGER NOT NULL,
        long     INTEGER NOT NULL,
        price    REAL NOT NULL,
        qty      REAL NOT NULL,
        UNIQUE (symbol, time_ms, long, price, qty)
    );
    CREATE INDEX IF NOT EXISTS liquidations_time ON liquidations (time_ms);
    "#,
    // v8 — lists as rows (`save_lists`): tens of thousands of members, only
    // the changed lists rewritten.  `sig` is the list's content fingerprint.
    // A member names its venue and kind (empty: the window's Binance venue).
    r#"
    CREATE TABLE IF NOT EXISTS lists (
        id      INTEGER PRIMARY KEY,
        ord     INTEGER NOT NULL,
        name    TEXT NOT NULL UNIQUE,
        kind    TEXT NOT NULL,              -- watchlists::Kind as JSON
        side    TEXT NOT NULL,              -- watchlists::Side as JSON
        sig     INTEGER NOT NULL,
        updated INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS list_members (
        list   INTEGER NOT NULL,
        ord    INTEGER NOT NULL,
        venue  TEXT NOT NULL DEFAULT '',
        kind   TEXT NOT NULL DEFAULT '',
        symbol TEXT NOT NULL,
        PRIMARY KEY (list, ord)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS list_members_symbol ON list_members (symbol);
    "#,
    // v9 — the journal of closed baskets (`journal_add`): what each source of
    // auto-trading actually made, kept after the ledger forgets the basket.
    r#"
    CREATE TABLE IF NOT EXISTS basket_journal (
        id       INTEGER PRIMARY KEY,
        basket   INTEGER NOT NULL,
        created  INTEGER NOT NULL,
        closed   INTEGER NOT NULL,
        source   TEXT NOT NULL,             -- the rule's kind ("spreads"), or "manual"
        tag      TEXT NOT NULL,
        name     TEXT NOT NULL,
        legs     TEXT NOT NULL,             -- [[symbol, long, usd], ...] as JSON
        usd      REAL NOT NULL,
        fees     REAL NOT NULL,
        net      REAL NOT NULL,             -- after fees, dollars
        why      TEXT NOT NULL,             -- target | stop | time | closed | dropped
        UNIQUE (basket, created)
    );
    CREATE INDEX IF NOT EXISTS basket_journal_source ON basket_journal (source, closed);
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

/// Empty every table (the schema and its version stay): a fresh start for the
/// shared demo database.
pub fn reset_all(conn: &Connection) -> Result<(), rusqlite::Error> {
    let tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let tx = conn.unchecked_transaction()?;
    for t in tables {
        // Names come from sqlite_master; quote them anyway.
        tx.execute(&format!("DELETE FROM \"{}\"", t.replace('"', "\"\"")), [])?;
    }
    tx.commit()
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

// ─── Agent audit log ─────────────────────────────────────────────────────

/// Most rows kept in `agent_log`; older ones are dropped on insert.
const AGENT_LOG_KEEP: i64 = 5_000;
/// Longest stored argument text (chars).
const AGENT_LOG_ARGS_MAX: usize = 2_000;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentLogRow {
    pub ts: i64,
    pub role: String,
    pub tool: String,
    pub status: String,
}

pub fn agent_log_add(
    conn: &Connection,
    ts: i64,
    role: &str,
    tool: &str,
    args: &str,
    status: &str,
) -> Result<(), rusqlite::Error> {
    let args: String = args.chars().take(AGENT_LOG_ARGS_MAX).collect();
    conn.execute(
        "INSERT INTO agent_log (ts, role, tool, args, status) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts, role, tool, args, status],
    )?;
    conn.execute(
        "DELETE FROM agent_log WHERE id <= (SELECT MAX(id) FROM agent_log) - ?1",
        params![AGENT_LOG_KEEP],
    )?;
    Ok(())
}

/// Newest first.
pub fn agent_log_recent(conn: &Connection, limit: usize) -> Vec<AgentLogRow> {
    let Ok(mut stmt) =
        conn.prepare("SELECT ts, role, tool, status FROM agent_log ORDER BY id DESC LIMIT ?1")
    else {
        return Vec::new();
    };
    stmt.query_map(params![limit as i64], |r| {
        Ok(AgentLogRow {
            ts: r.get(0)?,
            role: r.get(1)?,
            tool: r.get(2)?,
            status: r.get(3)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

// ─── Scalper mandates (history) ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct MandateRow {
    pub id: i64,
    pub issued_ms: i64,
    pub list: String,
    pub max_order_usd: f64,
    pub max_orders_hour: i64,
    pub max_loss_usd: f64,
    pub expires_ms: i64,
    pub status: String,
    pub ended_ms: Option<i64>,
    pub orders_total: i64,
    pub loss_usd: f64,
    /// Starting cash of the mandate's own ledger (0 for mandates before v6).
    pub balance: f64,
    /// That ledger's PnL (equity - balance) when last saved.
    pub pnl_usd: f64,
}

/// Insert or update a mandate row (by id).
pub fn mandate_save(conn: &Connection, m: &MandateRow) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO mandates (id, issued_ms, list, max_order_usd, max_orders_hour, max_loss_usd,
                               expires_ms, status, ended_ms, orders_total, loss_usd, balance, pnl_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET status = excluded.status, ended_ms = excluded.ended_ms,
             orders_total = excluded.orders_total, loss_usd = excluded.loss_usd,
             pnl_usd = excluded.pnl_usd",
        params![
            m.id,
            m.issued_ms,
            m.list,
            m.max_order_usd,
            m.max_orders_hour,
            m.max_loss_usd,
            m.expires_ms,
            m.status,
            m.ended_ms,
            m.orders_total,
            m.loss_usd,
            m.balance,
            m.pnl_usd
        ],
    )?;
    Ok(())
}

/// Store the mandate's ledger (a `BrokerSnapshot` as JSON).  The row must
/// exist ([`mandate_save`] first).
pub fn mandate_save_ledger(conn: &Connection, id: i64, json: &str) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE mandates SET ledger = ?2 WHERE id = ?1",
        params![id, json],
    )?;
    Ok(())
}

/// The newest mandate that has a ledger: `(id, ledger JSON)`.
pub fn mandate_latest_ledger(conn: &Connection) -> Option<(i64, String)> {
    conn.query_row(
        "SELECT id, ledger FROM mandates WHERE ledger IS NOT NULL ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

/// Newest first.
pub fn mandates_recent(conn: &Connection, limit: usize) -> Vec<MandateRow> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, issued_ms, list, max_order_usd, max_orders_hour, max_loss_usd, expires_ms,
                status, ended_ms, orders_total, loss_usd, balance, pnl_usd
         FROM mandates ORDER BY id DESC LIMIT ?1",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![limit as i64], |r| {
        Ok(MandateRow {
            id: r.get(0)?,
            issued_ms: r.get(1)?,
            list: r.get(2)?,
            max_order_usd: r.get(3)?,
            max_orders_hour: r.get(4)?,
            max_loss_usd: r.get(5)?,
            expires_ms: r.get(6)?,
            status: r.get(7)?,
            ended_ms: r.get(8)?,
            orders_total: r.get(9)?,
            loss_usd: r.get(10)?,
            balance: r.get(11)?,
            pnl_usd: r.get(12)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

/// A restart ends every mandate that was still running: authority must not
/// outlive the session in which the human granted it.  Returns how many.
pub fn mandates_end_active(conn: &Connection, now_ms: i64) -> usize {
    conn.execute(
        "UPDATE mandates SET status = 'ended: terminal restarted', ended_ms = ?1
         WHERE status IN ('active', 'exhausted (loss limit)')",
        params![now_ms],
    )
    .unwrap_or(0)
}

// ─── Agent research notes ────────────────────────────────────────────────

pub const NOTE_TITLE_MAX: usize = 200;
pub const NOTE_BODY_MAX: usize = 8_000;
/// Most notes kept; older ones are dropped on insert.
const NOTES_KEEP: i64 = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentNote {
    pub id: i64,
    pub ts: i64,
    pub role: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// Save a note (text is cut to its limits); returns its id.
pub fn note_add(
    conn: &Connection,
    ts: i64,
    role: &str,
    title: &str,
    body: &str,
    tags: &[String],
) -> Result<i64, rusqlite::Error> {
    let title: String = title.chars().take(NOTE_TITLE_MAX).collect();
    let body: String = body.chars().take(NOTE_BODY_MAX).collect();
    // Tags are stored as ",a,b," so a tag can be matched exactly with LIKE.
    let tag_text = if tags.is_empty() {
        String::new()
    } else {
        format!(
            ",{},",
            tags.iter()
                .map(|t| t.trim().to_ascii_lowercase().replace(',', " "))
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    conn.execute(
        "INSERT INTO agent_notes (ts, role, title, body, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts, role, title, body, tag_text],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM agent_notes WHERE id <= (SELECT MAX(id) FROM agent_notes) - ?1",
        params![NOTES_KEEP],
    )?;
    Ok(id)
}

/// Newest first, optionally only notes carrying `tag`.
pub fn notes_recent(conn: &Connection, limit: usize, tag: Option<&str>) -> Vec<AgentNote> {
    let pattern = tag.map(|t| format!("%,{},%", t.trim().to_ascii_lowercase()));
    let sql = "SELECT id, ts, role, title, body, tags FROM agent_notes \
               WHERE (?1 IS NULL OR tags LIKE ?1) ORDER BY id DESC LIMIT ?2";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    stmt.query_map(params![pattern, limit as i64], |r| {
        let tags: String = r.get(5)?;
        Ok(AgentNote {
            id: r.get(0)?,
            ts: r.get(1)?,
            role: r.get(2)?,
            title: r.get(3)?,
            body: r.get(4)?,
            tags: tags
                .split(',')
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
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

// ─── Lists ───────────────────────────────────────────────────────────────

use crate::watchlists::{Watchlist, Watchlists};

/// A stable fingerprint of what a list keeps (FNV-1a): name, kind, side and,
/// unless it is a dynamic group (members computed, never kept), its members.
fn list_sig(l: &Watchlist, kind: &str, side: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b.iter().chain(&[0u8]) {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(l.name.as_bytes());
    eat(kind.as_bytes());
    eat(side.as_bytes());
    if l.rule().is_none() {
        for m in &l.symbols {
            eat(m.as_bytes());
        }
    }
    h as i64
}

/// Write the lists as rows in one transaction: a list whose fingerprint or
/// place changed is updated, its members rewritten only when its content
/// changed; lists gone from `w` are deleted.  Returns how many lists had
/// their members rewritten.
pub fn save_lists(conn: &Connection, w: &Watchlists, now_ms: i64) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut stored: std::collections::HashMap<String, (i64, i64, i64)> = {
        let mut st = tx.prepare("SELECT name, id, sig, ord FROM lists")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut rewritten = 0;
    {
        let mut ins_list = tx.prepare_cached(
            "INSERT INTO lists (ord, name, kind, side, sig, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut upd_list = tx.prepare_cached(
            "UPDATE lists SET ord = ?1, kind = ?2, side = ?3, sig = ?4, updated = ?5 WHERE id = ?6",
        )?;
        let mut del_members = tx.prepare_cached("DELETE FROM list_members WHERE list = ?1")?;
        let mut ins_member =
            tx.prepare_cached("INSERT INTO list_members (list, ord, symbol) VALUES (?1, ?2, ?3)")?;
        for (ord, l) in w.lists().iter().enumerate() {
            let kind = serde_json::to_string(&l.kind).unwrap_or_default();
            let side = serde_json::to_string(&l.side).unwrap_or_default();
            let sig = list_sig(l, &kind, &side);
            let ord = ord as i64;
            let id = match stored.remove(&l.name) {
                Some((id, old_sig, old_ord)) => {
                    if old_sig == sig && old_ord == ord {
                        continue;
                    }
                    upd_list.execute(params![ord, kind, side, sig, now_ms, id])?;
                    if old_sig == sig {
                        continue;
                    }
                    del_members.execute([id])?;
                    id
                }
                None => {
                    ins_list.execute(params![ord, l.name, kind, side, sig, now_ms])?;
                    tx.last_insert_rowid()
                }
            };
            rewritten += 1;
            if l.rule().is_none() {
                for (i, m) in l.symbols.iter().enumerate() {
                    ins_member.execute(params![id, i as i64, m])?;
                }
            }
        }
        for (_, (id, _, _)) in stored.drain() {
            del_members.execute([id])?;
            tx.execute("DELETE FROM lists WHERE id = ?1", [id])?;
        }
    }
    tx.commit()?;
    Ok(rewritten)
}

/// The lists as rows, in order, with their members; empty when none are
/// stored yet (or a row cannot be read back).
pub fn load_lists(conn: &Connection) -> Vec<Watchlist> {
    let read = || -> rusqlite::Result<Vec<Watchlist>> {
        let mut out = Vec::new();
        let mut st = conn.prepare("SELECT id, name, kind, side FROM lists ORDER BY ord")?;
        let heads = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut members =
            conn.prepare("SELECT symbol FROM list_members WHERE list = ?1 ORDER BY ord")?;
        for (id, name, kind, side) in heads {
            let (Ok(kind), Ok(side)) = (serde_json::from_str(&kind), serde_json::from_str(&side))
            else {
                continue;
            };
            let symbols = members
                .query_map([id], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            out.push(Watchlist {
                name,
                symbols,
                side,
                kind,
            });
        }
        Ok(out)
    };
    read().unwrap_or_default()
}

// ─── Basket journal ──────────────────────────────────────────────────────

/// Write a closed basket once (a basket seen again after a restart is
/// ignored).  `source`: the kind of the rule that opened it, or "manual".
/// `true` when it was new.
pub fn journal_add(
    conn: &Connection,
    b: &crate::basket::Basket,
    source: &str,
) -> rusqlite::Result<bool> {
    let crate::basket::State::Closed { at_ms, why } = b.state else {
        return Ok(false);
    };
    let p = b.pnl(&Default::default());
    let legs: Vec<(&str, bool, f64)> = b
        .legs
        .iter()
        .map(|l| (l.symbol.as_str(), l.long, l.usd))
        .collect();
    let n = conn.execute(
        "INSERT OR IGNORE INTO basket_journal
            (basket, created, closed, source, tag, name, legs, usd, fees, net, why)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            b.id as i64,
            b.created_ms,
            at_ms,
            source,
            b.tag,
            b.name,
            serde_json::to_string(&legs).unwrap_or_default(),
            b.total_usd(),
            p.fees,
            p.net(),
            why.label()
        ],
    )?;
    Ok(n > 0)
}

/// What one source of baskets has done, from the journal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceFact {
    pub source: String,
    pub baskets: i64,
    pub wins: i64,
    /// Dollars after fees, and the fees.
    pub net: f64,
    pub fees: f64,
    /// Dollars the baskets were sized with, all together.
    pub usd: f64,
    pub mean_minutes: f64,
}

impl SourceFact {
    pub fn win_rate(&self) -> f64 {
        if self.baskets == 0 {
            0.0
        } else {
            self.wins as f64 / self.baskets as f64 * 100.0
        }
    }
    /// Net as % of the dollars the baskets used.
    pub fn net_pct(&self) -> f64 {
        if self.usd > 0.0 {
            self.net / self.usd * 100.0
        } else {
            0.0
        }
    }
}

/// Per source, the baskets closed since `since_ms` (dropped ones, which
/// never entered, left out), the busiest source first.
pub fn journal_facts(conn: &Connection, since_ms: i64) -> Vec<SourceFact> {
    let Ok(mut st) = conn.prepare(
        "SELECT source, COUNT(*), SUM(net > 0), SUM(net), SUM(fees), SUM(usd),
                AVG((closed - created) / 60000.0)
         FROM basket_journal WHERE closed >= ?1 AND why != 'dropped'
         GROUP BY source ORDER BY COUNT(*) DESC",
    ) else {
        return Vec::new();
    };
    st.query_map([since_ms], |r| {
        Ok(SourceFact {
            source: r.get(0)?,
            baskets: r.get(1)?,
            wins: r.get(2)?,
            net: r.get(3)?,
            fees: r.get(4)?,
            usd: r.get(5)?,
            mean_minutes: r.get(6)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
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

// ─── Liquidations ────────────────────────────────────────────────────────

/// Store `liqs` (repeats are ignored) and drop rows older than `keep_from_ms`.
pub fn liquidations_save(
    conn: &Connection,
    liqs: &[crate::data::liquidations::Liquidation],
    keep_from_ms: i64,
) -> Result<(), rusqlite::Error> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO liquidations (symbol, time_ms, long, price, qty)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for l in liqs {
            stmt.execute(params![l.symbol, l.time_ms, l.long, l.price, l.qty])?;
        }
    }
    tx.execute(
        "DELETE FROM liquidations WHERE time_ms < ?1",
        params![keep_from_ms],
    )?;
    tx.commit()
}

/// Liquidations at or after `since_ms`, oldest first.
pub fn liquidations_since(
    conn: &Connection,
    since_ms: i64,
) -> Vec<crate::data::liquidations::Liquidation> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT symbol, time_ms, long, price, qty FROM liquidations
         WHERE time_ms >= ?1 ORDER BY time_ms",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![since_ms], |r| {
        Ok(crate::data::liquidations::Liquidation {
            symbol: r.get(0)?,
            time_ms: r.get(1)?,
            long: r.get(2)?,
            price: r.get(3)?,
            qty: r.get(4)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

/// Forget the persisted paper ledger (broker positions/orders + risk state).
pub fn reset_paper_ledger(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "DELETE FROM kv WHERE key IN ('paper_broker', 'paper_risk', 'paper_baskets')",
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
    baskets_json: &str,
) -> Result<(), rusqlite::Error> {
    save_kv(conn, "paper_broker", broker_json)?;
    save_kv(conn, "paper_risk", risk_json)?;
    save_kv(conn, "paper_baskets", baskets_json)?;
    Ok(())
}

/// (broker, risk, baskets) as saved; each `None` when never saved.
pub fn load_paper_snapshot(conn: &Connection) -> (Option<String>, Option<String>, Option<String>) {
    (
        load_kv(conn, "paper_broker"),
        load_kv(conn, "paper_risk"),
        load_kv(conn, "paper_baskets"),
    )
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
    fn lists_round_trip_as_rows_and_only_changes_are_rewritten() {
        use crate::watchlists::{MoveRule, Side};
        let c = mem();
        let mut w = Watchlists::default();
        w.add_list("Big");
        let many: Vec<String> = (0..30_000).map(|i| format!("S{i}USDT")).collect();
        assert_eq!(w.add_many(many.iter().map(String::as_str)), 30_000);
        w.add_dynamic(MoveRule::parse("5 3 up").unwrap());
        let n = w.lists().len();
        assert_eq!(save_lists(&c, &w, 1).unwrap(), n, "first save writes all");
        assert_eq!(save_lists(&c, &w, 2).unwrap(), 0, "nothing changed");
        let back = load_lists(&c);
        assert_eq!(back.len(), n);
        let big = back.iter().find(|l| l.name == "Big").unwrap();
        assert_eq!(big.symbols.len(), 30_000);
        assert_eq!(big.symbols[29_999], "S29999USDT", "order kept");
        assert!(
            back.iter().any(|l| l.rule().is_some()),
            "dynamic group kept"
        );
        // One list edited: only it is rewritten.
        let idx = w.lists().iter().position(|l| l.name == "Big").unwrap();
        w.select_list(idx);
        w.set_side_at(idx, Side::Short);
        assert_eq!(save_lists(&c, &w, 3).unwrap(), 1);
        assert_eq!(
            load_lists(&c)
                .iter()
                .find(|l| l.name == "Big")
                .unwrap()
                .side,
            Side::Short
        );
        // Deleted and renamed lists follow.
        w.delete_active();
        save_lists(&c, &w, 4).unwrap();
        let after = load_lists(&c);
        assert_eq!(after.len(), n - 1);
        assert!(!after.iter().any(|l| l.name == "Big"));
        let members: i64 = c
            .query_row("SELECT COUNT(*) FROM list_members", [], |r| r.get(0))
            .unwrap();
        assert!(
            members < 1000,
            "the deleted list's members went too: {members}"
        );
        // The window takes another's rows.
        let mut other = Watchlists::default();
        assert!(other.adopt(after.clone()));
        assert!(!other.adopt(after), "the same again is no change");
    }

    #[test]
    fn the_journal_keeps_each_closed_basket_once_and_sums_per_source() {
        use crate::basket::{Basket, Entry, State, Why, DEFAULT_EXIT};
        let c = mem();
        let legs = vec![
            ("AUSDT".to_string(), true, 10.0),
            ("BUSDT".to_string(), false, 10.0),
        ];
        let mut b = Basket::new(
            1,
            "buy A / sell B",
            "o",
            &legs,
            Entry::Market,
            DEFAULT_EXIT,
            0,
        )
        .unwrap();
        assert!(
            !journal_add(&c, &b, "spreads").unwrap(),
            "an open basket is not journaled"
        );
        b.legs[0].realized = 1.0;
        b.legs[0].fees = 0.1;
        b.state = State::Closed {
            at_ms: 600_000,
            why: Why::Target,
        };
        assert!(journal_add(&c, &b, "spreads").unwrap());
        assert!(
            !journal_add(&c, &b, "spreads").unwrap(),
            "seen again after a restart"
        );
        let mut l = Basket::new(2, "x", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
        l.legs[1].realized = -0.5;
        l.state = State::Closed {
            at_ms: 1_200_000,
            why: Why::Stop,
        };
        journal_add(&c, &l, "spreads").unwrap();
        let mut d = Basket::new(3, "x", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
        d.state = State::Closed {
            at_ms: 5,
            why: Why::Abandoned,
        };
        journal_add(&c, &d, "spreads").unwrap();
        let f = journal_facts(&c, 0);
        assert_eq!(f.len(), 1);
        let s = &f[0];
        assert_eq!(
            (s.source.as_str(), s.baskets, s.wins),
            ("spreads", 2, 1),
            "dropped left out"
        );
        assert!(
            (s.net - 0.4).abs() < 1e-9 && (s.fees - 0.1).abs() < 1e-9,
            "{s:?}"
        );
        assert!((s.mean_minutes - 15.0).abs() < 1e-9);
        assert!((s.net_pct() - 1.0).abs() < 1e-9 && s.win_rate() == 50.0);
        assert!(journal_facts(&c, 2_000_000).is_empty());
    }

    #[test]
    fn clear_cache_only_touches_the_ohlc_cache() {
        let c = mem();
        cache_save(&c, "BTCUSDT", "1m", &[bar(1.0, 1)]).unwrap();
        save_kv(&c, "binance_futures_api_secret", "f").unwrap();
        save_kv(&c, "market", "Crypto").unwrap();
        save_paper_snapshot(&c, "{\"cash\":1}", "{}", "{}").unwrap();
        clear_cache(&c).unwrap();
        assert!(cache_load(&c, "BTCUSDT", "1m").is_none());
        assert_eq!(
            load_kv(&c, "binance_futures_api_secret").as_deref(),
            Some("f")
        );
        assert_eq!(load_kv(&c, "market").as_deref(), Some("Crypto"));
        let kept = load_paper_snapshot(&c);
        assert!(
            kept.0.is_some() && kept.2.is_some(),
            "ledger and baskets must survive Clear cache"
        );
        reset_paper_ledger(&c).unwrap();
        assert_eq!(load_paper_snapshot(&c), (None, None, None));
        assert_eq!(load_kv(&c, "market").as_deref(), Some("Crypto"));
    }

    #[test]
    fn agent_log_keeps_newest_first_truncates_args_and_prunes() {
        let c = open_memory().unwrap();
        agent_log_add(&c, 1, "analyst", "whoami", "{}", "ok").unwrap();
        agent_log_add(
            &c,
            2,
            "scalper",
            "place_order",
            &"x".repeat(5_000),
            "denied",
        )
        .unwrap();
        let rows = agent_log_recent(&c, 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].ts, rows[0].role.as_str(), rows[0].status.as_str()),
            (2, "scalper", "denied")
        );
        let stored: String = c
            .query_row("SELECT args FROM agent_log WHERE ts = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored.chars().count(), AGENT_LOG_ARGS_MAX);
        for i in 0..(AGENT_LOG_KEEP + 50) {
            agent_log_add(&c, 10 + i, "analyst", "whoami", "{}", "ok").unwrap();
        }
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM agent_log", [], |r| r.get(0))
            .unwrap();
        assert!(n <= AGENT_LOG_KEEP + 1, "{n}");
        assert_eq!(agent_log_recent(&c, 3).len(), 3);
    }

    #[test]
    fn notes_round_trip_filter_by_tag_cut_and_prune() {
        let c = open_memory().unwrap();
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = note_add(
            &c,
            1,
            "researcher",
            "ETH/BTC",
            "z=2.1",
            &t(&["Spread", "eth"]),
        )
        .unwrap();
        note_add(&c, 2, "researcher", "SOL", "x", &t(&["momentum"])).unwrap();
        note_add(
            &c,
            3,
            "researcher",
            &"T".repeat(900),
            &"b".repeat(20_000),
            &[],
        )
        .unwrap();
        let all = notes_recent(&c, 10, None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].title.chars().count(), NOTE_TITLE_MAX);
        assert_eq!(all[0].body.chars().count(), NOTE_BODY_MAX);
        let spread = notes_recent(&c, 10, Some("SPREAD"));
        assert_eq!(spread.len(), 1, "tags are case-insensitive and exact");
        assert_eq!(
            (spread[0].id, spread[0].tags.clone()),
            (a, t(&["spread", "eth"]))
        );
        assert!(
            notes_recent(&c, 10, Some("spre")).is_empty(),
            "no partial tag matches"
        );
        assert_eq!(notes_recent(&c, 1, None).len(), 1);
        for i in 0..(NOTES_KEEP + 20) {
            note_add(&c, 10 + i, "researcher", "n", "b", &[]).unwrap();
        }
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM agent_notes", [], |r| r.get(0))
            .unwrap();
        assert!(n <= NOTES_KEEP + 1, "{n}");
    }

    #[test]
    fn mandate_rows_upsert_list_newest_first_and_end_on_restart() {
        let c = open_memory().unwrap();
        let row = |id: i64, status: &str| MandateRow {
            id,
            issued_ms: id * 10,
            list: "Scalp".into(),
            max_order_usd: 50.0,
            max_orders_hour: 30,
            max_loss_usd: 100.0,
            expires_ms: id * 10 + 600,
            status: status.into(),
            ended_ms: None,
            orders_total: 0,
            loss_usd: 0.0,
            balance: 1_000.0,
            pnl_usd: 0.0,
        };
        mandate_save(&c, &row(1, "active")).unwrap();
        mandate_save(&c, &row(2, "active")).unwrap();
        let mut done = row(1, "expired");
        done.ended_ms = Some(999);
        done.orders_total = 7;
        done.loss_usd = 12.5;
        done.pnl_usd = -12.5;
        mandate_save(&c, &done).unwrap();
        let rows = mandates_recent(&c, 10);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
        let one = rows.iter().find(|r| r.id == 1).unwrap();
        assert_eq!(
            (
                one.status.as_str(),
                one.ended_ms,
                one.orders_total,
                one.loss_usd
            ),
            ("expired", Some(999), 7, 12.5)
        );
        assert_eq!(one.max_order_usd, 50.0, "the limits stay as issued");
        assert_eq!((one.balance, one.pnl_usd), (1_000.0, -12.5));
        assert_eq!(mandate_latest_ledger(&c), None);
        mandate_save_ledger(&c, 1, "{\"a\":1}").unwrap();
        mandate_save_ledger(&c, 2, "{\"b\":2}").unwrap();
        assert_eq!(mandate_latest_ledger(&c), Some((2, "{\"b\":2}".into())));
        assert_eq!(
            mandates_end_active(&c, 5_000),
            1,
            "only the still-active one"
        );
        let rows = mandates_recent(&c, 10);
        assert_eq!(rows[0].status, "ended: terminal restarted");
        assert_eq!(rows[1].status, "expired");
    }

    #[test]
    fn reset_all_empties_every_table_but_keeps_the_schema() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        save_kv(&c, "k", "v").unwrap();
        save_slot(
            &c,
            &StoredSlot {
                symbol: "BTCUSDT".into(),
                venue: "BINANCE-SPOT".into(),
                strategy_id: "manual_market".into(),
                running: true,
            },
            1,
        )
        .unwrap();
        let version = schema_version(&c);
        reset_all(&c).unwrap();
        assert_eq!(load_kv(&c, "k"), None);
        assert!(load_slots(&c).unwrap().is_empty());
        assert_eq!(schema_version(&c), version, "schema untouched");
        save_kv(&c, "k", "again").unwrap();
        assert_eq!(load_kv(&c, "k").as_deref(), Some("again"));
    }

    #[test]
    fn liquidations_round_trip_once_each() {
        use crate::data::liquidations::Liquidation;
        let c = open_memory().unwrap();
        let l = |t: i64, long: bool| Liquidation {
            symbol: "BTCUSDT".into(),
            long,
            price: 100.5,
            qty: 0.25,
            time_ms: t,
        };
        liquidations_save(&c, &[l(1_000, true), l(2_000, false)], 0).unwrap();
        // Another window heard the same one.
        liquidations_save(&c, &[l(2_000, false), l(3_000, true)], 0).unwrap();
        let all = liquidations_since(&c, 0);
        assert_eq!(all.len(), 3);
        assert_eq!(all[1], l(2_000, false));
        assert_eq!(liquidations_since(&c, 2_500).len(), 1);
        liquidations_save(&c, &[], 2_000).unwrap();
        assert_eq!(liquidations_since(&c, 0).len(), 2, "old rows dropped");
    }
}
