//! Watchlists — named groups of symbols the user keeps across sessions.
//!
//! A group is either **manual** (symbols the user put there) or **dynamic**
//! (membership computed from a price-move rule, e.g. "moved >10% in the last
//! 15 min").  A group also carries a trading `Side` tag (long / short / any).
//! Dynamic membership needs recent prices, which only the running app can
//! observe, so [`PriceTracker`] samples them and the group is empty while the
//! tracker is still warming up.
//!
//! Pure data + editing rules; the F5 view renders it and `commander` stores
//! it as rows (`db::save_lists`: a row per list, a row per member, only the
//! lists that changed are rewritten).  The old JSON in the `kv` table (key
//! `watchlists`) is read once to move it over and then left as a reserve.
//! Symbols are Binance symbols (`BTCUSDT`); prices come from the live
//! screener feed at render time.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

pub const KV_KEY: &str = "watchlists";
pub const MAX_NAME_CHARS: usize = 24;
/// Most groups, and most symbols in one group (a runaway agent must not be
/// able to grow the saved JSON without bound).
pub const MAX_LISTS: usize = 10_000;
pub const MAX_SYMBOLS_PER_LIST: usize = 50_000;
const DEFAULT_NAME: &str = "Main";

/// Which way the group is meant to be traded.  A label for the user; the
/// Trade page does not enforce it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    #[default]
    Any,
    Long,
    Short,
}

impl Side {
    pub fn next(self) -> Self {
        match self {
            Self::Any => Self::Long,
            Self::Long => Self::Short,
            Self::Short => Self::Any,
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::Any => "",
            Self::Long => "LONG",
            Self::Short => "SHORT",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveDir {
    Up,
    Down,
    Either,
}

/// "Moved at least `threshold_pct` within the last `window_min` minutes."
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveRule {
    pub window_min: u32,
    pub threshold_pct: f64,
    pub dir: MoveDir,
    /// A symbol that matched stays in the group this many more minutes
    /// after the move fades (0 = leaves at once).
    #[serde(default)]
    pub hold_min: u32,
    /// Only symbols whose 24h quote volume (USDT) is at least this much
    /// (0 = no filter).  Keeps illiquid junk out of the group.
    #[serde(default)]
    pub min_volume: f64,
}

pub const MAX_WINDOW_MIN: u32 = 30;

/// Move of the built-in Pump 15m / Dump 15m groups.  10% in 15 minutes
/// almost never happens (the lists stayed empty); 5% does a few times a day.
pub const PRESET_PCT: f64 = 5.0;
/// What the presets used before: such an unedited group is upgraded on load.
const OLD_PRESET_PCT: f64 = 10.0;

/// The name a rule-made group gets when the user gives none.
pub fn dynamic_name(rule: &MoveRule) -> String {
    let dir = match rule.dir {
        MoveDir::Up => "up",
        MoveDir::Down => "down",
        MoveDir::Either => "both",
    };
    let vol = if rule.min_volume > 0.0 {
        format!(" v{}", format_volume(rule.min_volume))
    } else {
        String::new()
    };
    clean_name(&format!(
        "Move {}m {}% {}{}",
        rule.window_min, rule.threshold_pct, dir, vol
    ))
}

/// What a rule line asks for: the rule itself plus optional group name and
/// side (`15 4 up 5 vol=5M name=Pumps side=short`).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupSpec {
    pub rule: MoveRule,
    pub name: Option<String>,
    pub side: Option<Side>,
}

/// `5M` -> 5_000_000.  Plain numbers and K / M / B suffixes.
pub fn parse_volume(text: &str) -> Option<f64> {
    let t = text.trim().to_ascii_lowercase();
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1e3),
        'm' => (&t[..t.len() - 1], 1e6),
        'b' => (&t[..t.len() - 1], 1e9),
        _ => (t.as_str(), 1.0),
    };
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && (0.0..=1e12).contains(&(v * mult))).then_some(v * mult)
}

/// `5000000` -> `5M`, for labels and the rule editor.
pub fn format_volume(v: f64) -> String {
    let (n, suffix) = if v >= 1e9 {
        (v / 1e9, "B")
    } else if v >= 1e6 {
        (v / 1e6, "M")
    } else if v >= 1e3 {
        (v / 1e3, "K")
    } else {
        (v, "")
    };
    format!("{n}{suffix}")
}

impl MoveRule {
    pub fn matches(&self, change_pct: f64) -> bool {
        match self.dir {
            MoveDir::Up => change_pct >= self.threshold_pct,
            MoveDir::Down => change_pct <= -self.threshold_pct,
            MoveDir::Either => change_pct.abs() >= self.threshold_pct,
        }
    }

    /// `15m >10% up` — also the default name of a rule-made group.
    pub fn label(&self) -> String {
        let dir = match self.dir {
            MoveDir::Up => "up",
            MoveDir::Down => "down",
            MoveDir::Either => "both",
        };
        let hold = if self.hold_min > 0 {
            format!(" +{}m", self.hold_min)
        } else {
            String::new()
        };
        let vol = if self.min_volume > 0.0 {
            format!(" vol>{}", format_volume(self.min_volume))
        } else {
            String::new()
        };
        format!(
            "{}m >{}% {}{}{}",
            self.window_min, self.threshold_pct, dir, hold, vol
        )
    }

    /// The form [`MoveRule::parse`] accepts, for pre-filling an editor.
    pub fn input_text(&self) -> String {
        let dir = match self.dir {
            MoveDir::Up => "up",
            MoveDir::Down => "down",
            MoveDir::Either => "both",
        };
        let mut text = match self.hold_min {
            0 => format!("{} {} {}", self.window_min, self.threshold_pct, dir),
            h => format!("{} {} {} {}", self.window_min, self.threshold_pct, dir, h),
        };
        if self.min_volume > 0.0 {
            text.push_str(&format!(" vol={}", format_volume(self.min_volume)));
        }
        text
    }

    /// Parse `"15 10 up 5"`: minutes, percent, up|down|both, hold minutes.
    /// Direction defaults to both, hold to 0.  `key=value` words
    /// (`vol=`, `name=`, `side=`) are accepted and ignored here; see
    /// [`MoveRule::parse_spec`].
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_spec(text).map(|s| s.rule)
    }

    /// Like [`MoveRule::parse`], plus the optional words
    /// `vol=<min 24h USDT volume, e.g. 5M>`, `name=<group name>` and
    /// `side=any|long|short`, anywhere after the positional part.
    pub fn parse_spec(text: &str) -> Option<GroupSpec> {
        let mut positional: Vec<&str> = Vec::new();
        let mut min_volume = 0.0;
        let mut name = None;
        let mut side = None;
        for word in text.split_whitespace() {
            match word.split_once('=') {
                None => positional.push(word),
                Some((key, value)) => match key.to_ascii_lowercase().as_str() {
                    "vol" | "volume" => min_volume = parse_volume(value)?,
                    "name" => {
                        let n = clean_name(&value.replace('_', " "));
                        if n.is_empty() {
                            return None;
                        }
                        name = Some(n);
                    }
                    "side" => {
                        side = Some(match value.to_ascii_lowercase().as_str() {
                            "any" => Side::Any,
                            "long" => Side::Long,
                            "short" => Side::Short,
                            _ => return None,
                        })
                    }
                    _ => return None,
                },
            }
        }
        let mut it = positional.into_iter();
        let window_min: u32 = it.next()?.trim_end_matches('m').parse().ok()?;
        let threshold_pct: f64 = it.next()?.trim_end_matches('%').parse().ok()?;
        let dir = match it.next().map(str::to_ascii_lowercase).as_deref() {
            None | Some("both") | Some("either") => MoveDir::Either,
            Some("up") => MoveDir::Up,
            Some("down") => MoveDir::Down,
            Some(_) => return None,
        };
        let hold_min: u32 = match it.next() {
            None => 0,
            Some(h) => h.trim_end_matches('m').parse().ok()?,
        };
        if it.next().is_some()
            || hold_min > MAX_WINDOW_MIN
            || !(1..=MAX_WINDOW_MIN).contains(&window_min)
            || !threshold_pct.is_finite()
            || !(0.1..=100.0).contains(&threshold_pct)
        {
            return None;
        }
        Some(GroupSpec {
            rule: Self {
                window_min,
                threshold_pct,
                dir,
                hold_min,
                min_volume,
            },
            name,
            side,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Manual,
    Dynamic(MoveRule),
    /// Filled by the market scanner (`crate::scanner`); members are saved
    /// (one window scans, the others read them).
    Scan(ScanKind),
}

/// What a scanner list holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScanKind {
    /// Pair trading: the cheap leg of each pair far from its usual spread.
    PairsLong,
    /// ... and the rich leg.
    PairsShort,
    /// Far above / below their own recent normal.
    StretchedUp,
    StretchedDown,
    /// Falling hard, still falling, near the low, on heavy volume.
    Knives,
    /// Up (down) fast on heavy volume, from the live all-market stream
    /// (`crate::movers`).
    Pumps,
    Dumps,
}

impl ScanKind {
    pub const ALL: [ScanKind; 7] = [
        ScanKind::PairsLong,
        ScanKind::PairsShort,
        ScanKind::StretchedUp,
        ScanKind::StretchedDown,
        ScanKind::Knives,
        ScanKind::Pumps,
        ScanKind::Dumps,
    ];

    /// Made by the live movers, not by the 15-minute scan.
    pub fn is_live(self) -> bool {
        matches!(self, ScanKind::Pumps | ScanKind::Dumps)
    }

    /// The list's name and side when the scanner makes it.
    pub fn preset(self) -> (&'static str, Side) {
        match self {
            ScanKind::PairsLong => ("Scan: pairs long", Side::Long),
            ScanKind::PairsShort => ("Scan: pairs short", Side::Short),
            ScanKind::StretchedUp => ("Scan: stretched up", Side::Any),
            ScanKind::StretchedDown => ("Scan: stretched down", Side::Any),
            ScanKind::Knives => ("Scan: falling knives", Side::Any),
            ScanKind::Pumps => ("Scan: pumps", Side::Any),
            ScanKind::Dumps => ("Scan: dumps", Side::Any),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Watchlist {
    pub name: String,
    /// Manual: the user's symbols.  Dynamic: a cache rewritten by
    /// [`Watchlists::refresh_dynamic`] (not persisted).
    pub symbols: Vec<String>,
    #[serde(default)]
    pub side: Side,
    #[serde(default)]
    pub kind: Kind,
}

impl Watchlist {
    fn manual(name: &str) -> Self {
        Self {
            name: name.into(),
            symbols: Vec::new(),
            side: Side::Any,
            kind: Kind::Manual,
        }
    }

    pub fn rule(&self) -> Option<MoveRule> {
        match self.kind {
            Kind::Dynamic(r) => Some(r),
            Kind::Manual | Kind::Scan(_) => None,
        }
    }

    /// Filled by the program (dynamic or scanner), not by hand.
    pub fn is_auto(&self) -> bool {
        self.kind != Kind::Manual
    }

    pub fn scan_kind(&self) -> Option<ScanKind> {
        match self.kind {
            Kind::Scan(k) => Some(k),
            _ => None,
        }
    }
}

/// Rolling last-price samples per symbol, used to answer "how much did it
/// move in the last N minutes".  Time is passed in, never read.
#[derive(Default)]
pub struct PriceTracker {
    samples: HashMap<String, VecDeque<(i64, f64)>>,
    last_sample_ms: i64,
    /// Latest 24h quote volume per symbol (for `MoveRule::min_volume`).
    volumes: HashMap<String, f64>,
    /// A history download is running (the Lists page says so instead of
    /// "collecting").
    pub backfilling: bool,
}

/// At most one sample per symbol per this many ms.
pub const SAMPLE_EVERY_MS: i64 = 10_000;

impl PriceTracker {
    /// Record a price snapshot; ignored if the previous one is too recent.
    /// Returns whether a sample was taken.
    pub fn record<'a>(
        &mut self,
        now_ms: i64,
        prices: impl Iterator<Item = (&'a str, f64)>,
    ) -> bool {
        if self.last_sample_ms != 0 && now_ms - self.last_sample_ms < SAMPLE_EVERY_MS {
            return false;
        }
        self.last_sample_ms = now_ms;
        let keep_from = now_ms - (MAX_WINDOW_MIN as i64 + 1) * 60_000;
        for (sym, price) in prices {
            if !(price.is_finite() && price > 0.0) {
                continue;
            }
            let q = match self.samples.get_mut(sym) {
                Some(q) => q,
                None => self.samples.entry(sym.to_string()).or_default(),
            };
            q.push_back((now_ms, price));
            while q.front().is_some_and(|&(t, _)| t < keep_from) {
                q.pop_front();
            }
        }
        true
    }

    /// Put older history (e.g. 1-minute candles downloaded at startup) in
    /// front of the live samples, so a window is measurable at once instead
    /// of after a full warm-up.  Points at or after the oldest sample, and
    /// points older than the tracker keeps, are ignored.
    pub fn backfill(&mut self, symbol: &str, points: &[(i64, f64)], now_ms: i64) {
        let keep_from = now_ms - (MAX_WINDOW_MIN as i64 + 1) * 60_000;
        let q = self.samples.entry(symbol.to_string()).or_default();
        let before = q.front().map_or(i64::MAX, |&(t, _)| t);
        let mut older: Vec<(i64, f64)> = points
            .iter()
            .copied()
            .filter(|&(t, p)| t >= keep_from && t < before && p.is_finite() && p > 0.0)
            .collect();
        older.sort_by_key(|&(t, _)| t);
        older.dedup_by_key(|pt| pt.0);
        for pt in older.into_iter().rev() {
            q.push_front(pt);
        }
    }

    /// Replace the 24h quote volumes.
    pub fn set_volumes<'a>(&mut self, volumes: impl Iterator<Item = (&'a str, f64)>) {
        self.volumes = volumes
            .filter(|(_, v)| v.is_finite())
            .map(|(s, v)| (s.to_string(), v))
            .collect();
    }

    pub fn volume(&self, symbol: &str) -> Option<f64> {
        self.volumes.get(symbol).copied()
    }

    /// % change between the newest sample and the one closest to
    /// `window_min` minutes earlier.  `None` until the history spans at
    /// least 90% of the window (warm-up) — never a guess from less data.
    pub fn change_pct(&self, symbol: &str, window_min: u32, now_ms: i64) -> Option<f64> {
        let q = self.samples.get(symbol)?;
        let &(_, last) = q.back()?;
        let window_ms = window_min as i64 * 60_000;
        let &(oldest_t, _) = q.front()?;
        if now_ms - oldest_t < window_ms * 9 / 10 {
            return None;
        }
        let target = now_ms - window_ms;
        let &(_, base) = q.iter().min_by_key(|&&(t, _)| (t - target).abs())?;
        Some((last - base) / base * 100.0)
    }

    /// Is any symbol's history long enough for [`PriceTracker::change_pct`] to
    /// answer for `window_min` (the same 90% rule)?
    pub fn is_warm(&self, window_min: u32, now_ms: i64) -> bool {
        let need = window_min as i64 * 60_000 * 9 / 10;
        self.samples
            .values()
            .any(|q| q.front().is_some_and(|&(t, _)| now_ms - t >= need))
    }

    /// How long the tracker has been collecting, in whole minutes.
    pub fn warm_minutes(&self, now_ms: i64) -> i64 {
        self.samples
            .values()
            .filter_map(|q| q.front().map(|&(t, _)| (now_ms - t) / 60_000))
            .max()
            .unwrap_or(0)
    }
}

/// The part of [`Watchlists`] kept beside the rows (kv `watchlists_meta`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub active: usize,
    pub scan_lists_added: bool,
    pub movers_lists_added: bool,
}

pub const META_KV_KEY: &str = "watchlists_meta";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Watchlists {
    lists: Vec<Watchlist>,
    active: usize,
    /// Cursor inside the active list (not persisted).
    #[serde(skip)]
    row: usize,
    /// Per group name: symbol -> last time it matched the rule (`hold_min`).
    #[serde(skip)]
    last_hit: HashMap<String, HashMap<String, i64>>,
    /// The scanner lists were added once (a deleted one stays deleted).
    #[serde(default)]
    scan_lists_added: bool,
    /// The same for the pumps / dumps lists (added later).
    #[serde(default)]
    movers_lists_added: bool,
}

impl Default for Watchlists {
    fn default() -> Self {
        Self {
            lists: vec![
                Watchlist::manual(DEFAULT_NAME),
                Watchlist {
                    kind: Kind::Dynamic(MoveRule {
                        window_min: 15,
                        threshold_pct: PRESET_PCT,
                        dir: MoveDir::Up,
                        hold_min: 5,
                        min_volume: 0.0,
                    }),
                    side: Side::Short,
                    ..Watchlist::manual("Pump 15m")
                },
                Watchlist {
                    kind: Kind::Dynamic(MoveRule {
                        window_min: 15,
                        threshold_pct: PRESET_PCT,
                        dir: MoveDir::Down,
                        hold_min: 5,
                        min_volume: 0.0,
                    }),
                    side: Side::Long,
                    ..Watchlist::manual("Dump 15m")
                },
            ],
            active: 0,
            row: 0,
            last_hit: HashMap::new(),
            scan_lists_added: false,
            movers_lists_added: false,
        }
    }
}

fn clean_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if crate::widgets::fmt::is_unsafe_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect()
}

/// A symbol is 1-20 ASCII letters/digits: it is later put into request URLs.
fn valid_symbol(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn clean_symbol(raw: &str) -> String {
    raw.trim().to_uppercase()
}

impl Watchlists {
    /// Parse stored JSON; anything unusable falls back to the default.
    pub fn from_json(json: &str) -> Self {
        let mut w: Self = match serde_json::from_str(json) {
            Ok(w) => w,
            Err(_) => return Self::default(),
        };
        if w.lists.is_empty() {
            return Self::default();
        }
        w.active = w.active.min(w.lists.len() - 1);
        w.upgrade_old_presets();
        w
    }

    /// Built from stored rows (`db::load_lists`) and this window's
    /// [`Meta`]; `None` when there are no lists.
    pub fn from_parts(lists: Vec<Watchlist>, meta: Meta) -> Option<Self> {
        if lists.is_empty() {
            return None;
        }
        let mut w = Self {
            active: meta.active.min(lists.len() - 1),
            lists,
            row: 0,
            last_hit: HashMap::new(),
            scan_lists_added: meta.scan_lists_added,
            movers_lists_added: meta.movers_lists_added,
        };
        w.upgrade_old_presets();
        Some(w)
    }

    /// What is kept beside the rows: which list this window shows, which
    /// built-in lists were added once.
    pub fn meta(&self) -> Meta {
        Meta {
            active: self.active,
            scan_lists_added: self.scan_lists_added,
            movers_lists_added: self.movers_lists_added,
        }
    }

    /// Add the scanner's lists the first time (kept deleted if the user
    /// deletes one later); the app calls it once at start.
    pub fn ensure_scan_lists(&mut self) {
        let add = |w: &mut Self, live: bool| {
            for k in ScanKind::ALL.into_iter().filter(|k| k.is_live() == live) {
                let (name, side) = k.preset();
                if w.lists.len() < MAX_LISTS && !w.has_name(name, None) {
                    w.lists.push(Watchlist {
                        kind: Kind::Scan(k),
                        side,
                        ..Watchlist::manual(name)
                    });
                }
            }
        };
        if !self.scan_lists_added {
            add(self, false);
            self.scan_lists_added = true;
        }
        if !self.movers_lists_added {
            add(self, true);
            self.movers_lists_added = true;
        }
    }

    /// Set the members of every scanner list of `kind`.  `true` if any changed.
    pub fn set_scan(&mut self, kind: ScanKind, members: &[String]) -> bool {
        let kept = self.selected_symbol().map(str::to_string);
        let mut changed = false;
        for (i, l) in self.lists.iter_mut().enumerate() {
            if l.scan_kind() == Some(kind) && l.symbols != members {
                l.symbols = members.iter().take(MAX_SYMBOLS_PER_LIST).cloned().collect();
                changed = true;
                if i == self.active {
                    let n = l.symbols.len();
                    self.row = kept
                        .as_ref()
                        .and_then(|k| l.symbols.iter().position(|s| s == k))
                        .unwrap_or(self.row)
                        .min(n.saturating_sub(1));
                }
            }
        }
        changed
    }

    pub fn has_scan(&self) -> bool {
        self.lists.iter().any(|l| l.scan_kind().is_some())
    }

    /// The first presets asked for a 10% move.  A group that still has
    /// exactly that preset (name, window 15, 10%, hold 5, no filter) gets the
    /// new default; anything the user edited is left alone.
    fn upgrade_old_presets(&mut self) {
        for l in self.lists.iter_mut() {
            let dir = match l.name.as_str() {
                "Pump 15m" => MoveDir::Up,
                "Dump 15m" => MoveDir::Down,
                _ => continue,
            };
            if let Kind::Dynamic(r) = &mut l.kind {
                let old = r.window_min == 15
                    && r.threshold_pct == OLD_PRESET_PCT
                    && r.dir == dir
                    && r.hold_min == 5
                    && r.min_volume == 0.0;
                if old {
                    r.threshold_pct = PRESET_PCT;
                }
            }
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = self.clone();
        for l in out.lists.iter_mut().filter(|l| l.rule().is_some()) {
            l.symbols.clear();
        }
        serde_json::to_string(&out).unwrap_or_default()
    }

    /// Take over lists another window saved.  This window keeps looking at
    /// the same list (by name; the first one if it is gone) and the same
    /// symbol; dynamic groups keep their computed members until the next
    /// refresh.  `false` when the stored document is unusable or equal.
    pub fn adopt_shared(&mut self, json: &str) -> bool {
        match serde_json::from_str::<Self>(json) {
            Ok(new) => self.adopt(new.lists),
            Err(_) => false,
        }
    }

    /// [`Self::adopt_shared`] over lists read from the rows.
    pub fn adopt(&mut self, lists: Vec<Watchlist>) -> bool {
        let same = lists.len() == self.lists.len()
            && lists.iter().zip(&self.lists).all(|(a, b)| {
                a.name == b.name
                    && a.side == b.side
                    && a.kind == b.kind
                    && (a.rule().is_some() || a.symbols == b.symbols)
            });
        if lists.is_empty() || same {
            return false;
        }
        let name = self.current().name.clone();
        let symbol = self.selected_symbol().map(str::to_string);
        let old = std::mem::replace(&mut self.lists, lists);
        for l in self.lists.iter_mut().filter(|l| l.rule().is_some()) {
            if let Some(o) = old.iter().find(|o| o.name == l.name) {
                l.symbols = o.symbols.clone();
            }
        }
        self.active = self.lists.iter().position(|l| l.name == name).unwrap_or(0);
        let n = self.current().symbols.len();
        self.row = symbol
            .and_then(|s| self.current().symbols.iter().position(|x| *x == s))
            .unwrap_or(self.row)
            .min(n.saturating_sub(1));
        true
    }

    pub fn lists(&self) -> &[Watchlist] {
        &self.lists
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn row(&self) -> usize {
        self.row
    }

    pub fn current(&self) -> &Watchlist {
        &self.lists[self.active.min(self.lists.len() - 1)]
    }

    pub fn selected_symbol(&self) -> Option<&str> {
        self.current().symbols.get(self.row).map(String::as_str)
    }

    pub fn contains(&self, symbol: &str) -> bool {
        let s = clean_symbol(symbol);
        self.current().symbols.contains(&s)
    }

    /// Make list `idx` active (ignored when out of range).
    pub fn select_list(&mut self, idx: usize) {
        if idx < self.lists.len() && idx != self.active {
            self.active = idx;
            self.row = 0;
        }
    }

    pub fn step_list(&mut self, dir: i32) {
        let n = self.lists.len() as i32;
        let next = (self.active as i32 + dir).rem_euclid(n) as usize;
        self.select_list(next);
    }

    pub fn select_row(&mut self, idx: usize) {
        let n = self.current().symbols.len();
        if n > 0 {
            self.row = idx.min(n - 1);
        }
    }

    pub fn step_row(&mut self, dir: i32) {
        let n = self.current().symbols.len();
        if n == 0 {
            return;
        }
        let next = (self.row as i64 + dir as i64).clamp(0, n as i64 - 1);
        self.row = next as usize;
    }

    /// Create a list and make it active.  Empty or duplicate names are
    /// refused (case-insensitive).
    pub fn add_list(&mut self, raw_name: &str) -> bool {
        let name = clean_name(raw_name);
        if name.is_empty() || self.lists.len() >= MAX_LISTS || self.has_name(&name, None) {
            return false;
        }
        self.lists.push(Watchlist::manual(&name));
        self.active = self.lists.len() - 1;
        self.row = 0;
        true
    }

    /// Create a dynamic group and make it active.  Named after its rule.
    pub fn add_dynamic(&mut self, rule: MoveRule) -> bool {
        self.add_dynamic_named(rule, None, None)
    }

    /// Like [`Watchlists::add_dynamic`] with an optional name and side.
    pub fn add_dynamic_named(
        &mut self,
        rule: MoveRule,
        name: Option<&str>,
        side: Option<Side>,
    ) -> bool {
        let name = match name {
            Some(n) => clean_name(n),
            None => dynamic_name(&rule),
        };
        if name.is_empty() || self.lists.len() >= MAX_LISTS || self.has_name(&name, None) {
            return false;
        }
        self.lists.push(Watchlist {
            kind: Kind::Dynamic(rule),
            side: side.unwrap_or_default(),
            ..Watchlist::manual(&name)
        });
        self.active = self.lists.len() - 1;
        self.row = 0;
        true
    }

    /// Change the rule of the active group; `false` if it is manual.
    pub fn set_rule(&mut self, rule: MoveRule) -> bool {
        let l = &mut self.lists[self.active];
        if l.rule().is_none() {
            return false;
        }
        l.kind = Kind::Dynamic(rule);
        true
    }

    /// Set the side tag of list `idx`; `false` if it does not exist.
    pub fn set_side_at(&mut self, idx: usize, side: Side) -> bool {
        match self.lists.get_mut(idx) {
            Some(l) => {
                l.side = side;
                true
            }
            None => false,
        }
    }

    pub fn cycle_side(&mut self) -> Side {
        let l = &mut self.lists[self.active];
        l.side = l.side.next();
        l.side
    }

    pub fn has_dynamic(&self) -> bool {
        self.lists.iter().any(|l| l.rule().is_some())
    }

    pub fn is_dynamic(&self) -> bool {
        self.current().rule().is_some()
    }

    /// Recompute every dynamic group from the tracker.  Members are ordered
    /// by size of the move; the cursor stays on the same symbol if it is
    /// still a member.
    pub fn refresh_dynamic(&mut self, tracker: &PriceTracker, now_ms: i64) {
        let active = self.active;
        let kept = self.selected_symbol().map(str::to_string);
        for (i, l) in self.lists.iter_mut().enumerate() {
            let Some(rule) = l.rule() else { continue };
            let mut hits: Vec<(String, f64)> = tracker
                .samples
                .keys()
                .filter_map(|s| {
                    if rule.min_volume > 0.0 && tracker.volume(s)? < rule.min_volume {
                        return None;
                    }
                    let c = tracker.change_pct(s, rule.window_min, now_ms)?;
                    rule.matches(c).then(|| (s.clone(), c))
                })
                .collect();
            hits.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
            let seen = self.last_hit.entry(l.name.clone()).or_default();
            for (sym, _) in &hits {
                seen.insert(sym.clone(), now_ms);
            }
            let hold_ms = rule.hold_min as i64 * 60_000;
            seen.retain(|_, t| now_ms - *t <= hold_ms);
            let mut held: Vec<(&String, &i64)> = seen
                .iter()
                .filter(|(sym, _)| !hits.iter().any(|(h, _)| h == *sym))
                .collect();
            held.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            let mut members: Vec<String> = hits.into_iter().map(|(s, _)| s).collect();
            members.extend(held.into_iter().map(|(s, _)| s.clone()));
            l.symbols = members;
            if i == active {
                let n = l.symbols.len();
                self.row = kept
                    .as_ref()
                    .and_then(|k| l.symbols.iter().position(|s| s == k))
                    .unwrap_or(self.row)
                    .min(n.saturating_sub(1));
            }
        }
    }

    /// Add symbols to list `idx` (any list, not only the active one) and make
    /// it active.  `None` if the list is missing or dynamic; else how many
    /// were new.
    pub fn add_many_to<'a>(
        &mut self,
        idx: usize,
        symbols: impl Iterator<Item = &'a str>,
    ) -> Option<usize> {
        if self.lists.get(idx)?.is_auto() {
            return None;
        }
        self.select_list(idx);
        Some(self.add_many(symbols))
    }

    /// Add many symbols at once to the active manual group; returns how
    /// many were new.  One pass with a set (tens of thousands stay quick).
    pub fn add_many<'a>(&mut self, symbols: impl Iterator<Item = &'a str>) -> usize {
        if self.current().is_auto() {
            return 0;
        }
        let list = &mut self.lists[self.active];
        let mut seen: std::collections::HashSet<String> = list.symbols.iter().cloned().collect();
        let mut added = 0;
        for raw in symbols {
            let s = clean_symbol(raw);
            if list.symbols.len() >= MAX_SYMBOLS_PER_LIST {
                break;
            }
            if valid_symbol(&s) && seen.insert(s.clone()) {
                list.symbols.push(s);
                added += 1;
            }
        }
        added
    }

    pub fn rename_active(&mut self, raw_name: &str) -> bool {
        let name = clean_name(raw_name);
        if name.is_empty() || self.has_name(&name, Some(self.active)) {
            return false;
        }
        self.lists[self.active].name = name;
        true
    }

    /// Delete the active list.  The last remaining list is never removed.
    pub fn delete_active(&mut self) -> bool {
        if self.lists.len() <= 1 {
            return false;
        }
        self.lists.remove(self.active);
        self.active = self.active.min(self.lists.len() - 1);
        self.row = 0;
        true
    }

    /// Add to the active list.  `false` when empty or already present.
    pub fn add_symbol(&mut self, raw: &str) -> bool {
        let s = clean_symbol(raw);
        if !valid_symbol(&s)
            || self.current().is_auto()
            || self.current().symbols.len() >= MAX_SYMBOLS_PER_LIST
            || self.contains(&s)
        {
            return false;
        }
        self.lists[self.active].symbols.push(s);
        true
    }

    /// Remove the symbol under the cursor and return it.
    pub fn remove_selected(&mut self) -> Option<String> {
        let list = &mut self.lists[self.active];
        if list.is_auto() || self.row >= list.symbols.len() {
            return None;
        }
        let removed = list.symbols.remove(self.row);
        self.row = self.row.min(list.symbols.len().saturating_sub(1));
        Some(removed)
    }

    pub fn has_name(&self, name: &str, except: Option<usize>) -> bool {
        self.lists
            .iter()
            .enumerate()
            .any(|(i, l)| Some(i) != except && l.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_main_and_two_preset_dynamic_groups() {
        let w = Watchlists::default();
        assert_eq!(w.lists().len(), 3);
        assert_eq!(w.current().name, "Main");
        assert!(!w.is_dynamic());
        assert_eq!(w.selected_symbol(), None);
        assert!(w.lists()[1].rule().is_some() && w.lists()[2].rule().is_some());
        assert_eq!(w.lists()[1].rule().unwrap().threshold_pct, PRESET_PCT);
        assert_eq!(w.lists()[2].rule().unwrap().threshold_pct, PRESET_PCT);
    }

    #[test]
    fn untouched_old_presets_are_upgraded_and_edited_ones_are_not() {
        // A saved document from before: both presets at 10%.
        let mut old = Watchlists::default();
        for i in [1, 2] {
            if let Kind::Dynamic(r) = &mut old.lists[i].kind {
                r.threshold_pct = OLD_PRESET_PCT;
            }
        }
        let loaded = Watchlists::from_json(&old.to_json());
        assert_eq!(loaded.lists()[1].rule().unwrap().threshold_pct, PRESET_PCT);
        assert_eq!(loaded.lists()[2].rule().unwrap().threshold_pct, PRESET_PCT);
        // The user changed the Pump rule (here: hold 9): untouched by the upgrade.
        let mut edited = old.clone();
        edited.select_list(1);
        edited.set_rule(MoveRule::parse("15 10 up 9").unwrap());
        let loaded = Watchlists::from_json(&edited.to_json());
        assert_eq!(loaded.lists()[1].rule().unwrap().threshold_pct, 10.0);
        assert_eq!(loaded.lists()[2].rule().unwrap().threshold_pct, PRESET_PCT);
        // A user's own 10% group with another name is never touched.
        let mut own = Watchlists::default();
        assert!(own.add_dynamic(MoveRule::parse("15 10 up 5").unwrap()));
        let loaded = Watchlists::from_json(&own.to_json());
        assert_eq!(loaded.current().rule().unwrap().threshold_pct, 10.0);
    }

    #[test]
    fn scanner_lists_are_added_once_filled_by_the_scanner_and_kept_in_json() {
        let mut w = Watchlists::default();
        w.ensure_scan_lists();
        assert_eq!(w.lists().len(), 3 + ScanKind::ALL.len());
        let i = w
            .lists()
            .iter()
            .position(|l| l.scan_kind() == Some(ScanKind::PairsLong))
            .unwrap();
        assert_eq!(w.lists()[i].side, Side::Long);
        assert!(w.set_scan(ScanKind::PairsLong, &["ETHUSDT".into(), "SOLUSDT".into()]));
        assert!(
            !w.set_scan(ScanKind::PairsLong, &["ETHUSDT".into(), "SOLUSDT".into()]),
            "no change"
        );
        // Not editable by hand.
        w.select_list(i);
        assert!(!w.add_symbol("BTCUSDT") && w.remove_selected().is_none());
        // Members survive the JSON (other windows read them); deleted lists stay deleted.
        let mut back = Watchlists::from_json(&w.to_json());
        assert_eq!(
            back.lists()[i].symbols,
            vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]
        );
        back.select_list(i);
        assert!(back.delete_active());
        back.ensure_scan_lists();
        assert_eq!(back.lists().len(), 3 + ScanKind::ALL.len() - 1);
    }

    #[test]
    fn add_symbol_normalizes_and_dedups() {
        let mut w = Watchlists::default();
        assert!(w.add_symbol(" btcusdt "));
        assert!(!w.add_symbol("BTCUSDT"));
        assert!(!w.add_symbol("  "));
        assert_eq!(w.current().symbols, vec!["BTCUSDT"]);
    }

    #[test]
    fn lists_reject_empty_and_duplicate_names() {
        let mut w = Watchlists::default();
        assert!(!w.add_list("  "));
        assert!(!w.add_list("main"));
        assert!(w.add_list("Scalp"));
        assert_eq!(w.active_index(), 3);
        assert!(!w.rename_active("MAIN"));
        assert!(w.rename_active("Scalp 2"));
        assert_eq!(w.current().name, "Scalp 2");
    }

    #[test]
    fn symbols_must_be_plain_and_groups_are_bounded() {
        let mut w = Watchlists::default();
        for bad in [
            "BTC&limit=9",
            "ETH/x",
            "A B",
            "",
            "  ",
            "a\u{1b}b",
            &"X".repeat(21),
        ] {
            assert!(!w.add_symbol(bad), "{bad:?}");
        }
        assert!(w.current().symbols.is_empty());
        assert!(w.add_symbol(" btcusdt "));
        let many: Vec<String> = (0..(MAX_SYMBOLS_PER_LIST + 50))
            .map(|i| format!("S{i}"))
            .collect();
        w.add_many(many.iter().map(String::as_str));
        assert!(!w.add_symbol("ZZZ"), "the cap holds for one at a time too");
        assert_eq!(
            w.current().symbols.len(),
            MAX_SYMBOLS_PER_LIST,
            "per-group cap"
        );
        let mut w = Watchlists::default();
        for i in 0..(MAX_LISTS + 20) {
            w.add_list(&format!("L{i}"));
        }
        assert_eq!(w.lists().len(), MAX_LISTS, "group cap");
        assert!(
            !w.add_dynamic(MoveRule::parse("5 3 up").unwrap()),
            "dynamic groups count too"
        );
    }

    #[test]
    fn group_names_cannot_carry_escape_sequences() {
        let mut w = Watchlists::default();
        assert!(w.add_list("Evil\u{1b}]0;PWNED\u{7}"));
        let n = &w.current().name;
        assert!(!n.chars().any(crate::widgets::fmt::is_unsafe_char), "{n:?}");
        assert!(w.rename_active("a\u{9b}2Jb"));
        assert_eq!(w.current().name, "a 2Jb");
    }

    #[test]
    fn long_names_are_cut_by_chars() {
        let mut w = Watchlists::default();
        assert!(w.add_list(&"ж".repeat(40)));
        assert_eq!(w.current().name.chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn last_list_cannot_be_deleted() {
        let mut w = Watchlists::default();
        w.add_list("B");
        assert!(w.delete_active());
        assert_eq!(w.lists().len(), 3);
        assert_eq!(w.active_index(), 2);
        while w.lists().len() > 1 {
            w.delete_active();
        }
        assert!(!w.delete_active());
    }

    #[test]
    fn cursor_clamps_and_follows_removal() {
        let mut w = Watchlists::default();
        for s in ["A", "B", "C"] {
            w.add_symbol(s);
        }
        w.step_row(5);
        assert_eq!(w.selected_symbol(), Some("C"));
        assert_eq!(w.remove_selected().as_deref(), Some("C"));
        assert_eq!(w.selected_symbol(), Some("B"));
        w.step_row(-9);
        assert_eq!(w.selected_symbol(), Some("A"));
    }

    #[test]
    fn step_list_wraps_and_resets_cursor() {
        let mut w = Watchlists::default();
        w.add_symbol("A");
        w.add_symbol("B");
        w.step_row(1);
        w.add_list("Two");
        w.step_list(1);
        assert_eq!(w.active_index(), 0);
        assert_eq!(w.row(), 0);
    }

    #[test]
    fn json_round_trip_and_bad_input() {
        let mut w = Watchlists::default();
        w.add_symbol("ETHUSDT");
        w.add_list("Two");
        let back = Watchlists::from_json(&w.to_json());
        assert_eq!(back.lists(), w.lists());
        assert_eq!(back.active_index(), 3);
        assert_eq!(Watchlists::from_json("not json"), Watchlists::default());
        assert_eq!(
            Watchlists::from_json(r#"{"lists":[],"active":3}"#),
            Watchlists::default()
        );
        // Old files without side/kind still load (as manual, any side).
        let clamped = Watchlists::from_json(r#"{"lists":[{"name":"A","symbols":[]}],"active":9}"#);
        assert_eq!(clamped.active_index(), 0);
        assert_eq!(clamped.current().side, Side::Any);
    }

    fn rule(dir: MoveDir) -> MoveRule {
        MoveRule {
            window_min: 15,
            threshold_pct: 10.0,
            dir,
            hold_min: 0,
            min_volume: 0.0,
        }
    }

    #[test]
    fn spec_words_volume_name_side() {
        let s = MoveRule::parse_spec("15 4 up 5 vol=5M name=Fast_pumps side=short").unwrap();
        assert_eq!(s.rule.min_volume, 5_000_000.0);
        assert_eq!(s.rule.hold_min, 5);
        assert_eq!(s.name.as_deref(), Some("Fast pumps"));
        assert_eq!(s.side, Some(Side::Short));
        assert_eq!(s.rule.input_text(), "15 4 up 5 vol=5M");
        assert_eq!(
            MoveRule::parse_spec("15 4 vol=250k")
                .unwrap()
                .rule
                .min_volume,
            250_000.0
        );
        // Order of the words does not matter; junk is refused.
        assert!(MoveRule::parse_spec("vol=1M 15 4 up").is_some());
        assert!(MoveRule::parse_spec("15 4 up vol=lots").is_none());
        assert!(MoveRule::parse_spec("15 4 up side=sideways").is_none());
        assert!(MoveRule::parse_spec("15 4 up colour=red").is_none());
        // Plain `parse` still takes the same text.
        assert_eq!(MoveRule::parse("15 10 up"), Some(rule(MoveDir::Up)));
    }

    #[test]
    fn volume_filter_and_named_group() {
        let mut w = Watchlists::default();
        let spec = MoveRule::parse_spec("15 5 up name=Liquid side=long vol=1M").unwrap();
        assert!(w.add_dynamic_named(spec.rule, spec.name.as_deref(), spec.side));
        assert_eq!(w.current().name, "Liquid");
        assert_eq!(w.current().side, Side::Long);
        let mut t = PriceTracker::default();
        let t0 = 1_000_000;
        feed(
            &mut t,
            t0,
            &[
                ("BIGUSDT", 100.0),
                ("TINYUSDT", 100.0),
                ("NOVOLUSDT", 100.0),
            ],
        );
        feed(
            &mut t,
            t0 + 15 * MIN,
            &[
                ("BIGUSDT", 110.0),
                ("TINYUSDT", 110.0),
                ("NOVOLUSDT", 110.0),
            ],
        );
        t.set_volumes([("BIGUSDT", 2e6), ("TINYUSDT", 5e4)].into_iter());
        w.refresh_dynamic(&t, t0 + 15 * MIN);
        assert_eq!(
            w.current().symbols,
            vec!["BIGUSDT"],
            "unknown volume is excluded too"
        );
        assert!(
            !w.add_dynamic_named(spec.rule, Some("liquid"), None),
            "name taken"
        );
    }

    #[test]
    fn rule_parsing_and_matching() {
        assert_eq!(MoveRule::parse("15 10 up"), Some(rule(MoveDir::Up)));
        assert_eq!(MoveRule::parse("15m 10% DOWN"), Some(rule(MoveDir::Down)));
        assert_eq!(MoveRule::parse("15 10"), Some(rule(MoveDir::Either)));
        let held = MoveRule::parse("15 10 up 5").unwrap();
        assert_eq!(held.hold_min, 5);
        assert_eq!(MoveRule::parse(&held.input_text()), Some(held));
        assert_eq!(MoveRule::parse("15 10 up 31"), None);
        let r = rule(MoveDir::Down);
        assert_eq!(
            MoveRule::parse(&r.input_text()),
            Some(r),
            "editor round trip"
        );
        for bad in [
            "",
            "15",
            "0 10",
            "31 10",
            "15 0",
            "15 10 sideways",
            "15 10 up x",
            "a b",
        ] {
            assert_eq!(MoveRule::parse(bad), None, "{bad:?}");
        }
        assert!(rule(MoveDir::Up).matches(10.0) && !rule(MoveDir::Up).matches(-12.0));
        assert!(rule(MoveDir::Down).matches(-10.5) && !rule(MoveDir::Down).matches(3.0));
        assert!(rule(MoveDir::Either).matches(-11.0) && !rule(MoveDir::Either).matches(9.9));
    }

    const MIN: i64 = 60_000;

    fn feed(t: &mut PriceTracker, now: i64, prices: &[(&str, f64)]) {
        assert!(t.record(now, prices.iter().map(|&(s, p)| (s, p))));
    }

    #[test]
    fn tracker_needs_warmup_then_measures_window() {
        let mut t = PriceTracker::default();
        let t0 = 1_000_000;
        feed(&mut t, t0, &[("AUSDT", 100.0)]);
        assert_eq!(t.change_pct("AUSDT", 15, t0 + 5 * MIN), None);
        for m in 1..=15 {
            let p = if m == 15 { 112.0 } else { 100.0 };
            feed(&mut t, t0 + m * MIN, &[("AUSDT", p)]);
        }
        let c = t.change_pct("AUSDT", 15, t0 + 15 * MIN).unwrap();
        assert!((c - 12.0).abs() < 1e-9, "{c}");
        assert_eq!(t.change_pct("NOPE", 15, t0 + 15 * MIN), None);
    }

    #[test]
    fn adopting_another_windows_lists_keeps_this_windows_view() {
        let mut a = Watchlists::default();
        let mut b = Watchlists::default();
        // This window (b) looks at the second list, on its second symbol.
        b.add_list("Mine");
        b.add_symbol("AAA");
        b.add_symbol("BBB");
        b.select_row(1);
        let b_json = b.to_json();
        a.adopt_shared(&b_json);
        a.select_list(0);
        // Another window adds a list and a symbol to "Mine" and saves.
        let mut other = a.clone();
        other.add_list("Fresh");
        other.add_symbol("ZZZ");
        other.select_list(a.lists().iter().position(|l| l.name == "Mine").unwrap());
        other.add_symbol("CCC");
        assert!(b.adopt_shared(&other.to_json()));
        assert_eq!(b.current().name, "Mine", "still on the same list");
        assert_eq!(b.selected_symbol(), Some("BBB"), "and on the same symbol");
        assert!(b.lists().iter().any(|l| l.name == "Fresh"));
        assert_eq!(b.current().symbols, ["AAA", "BBB", "CCC"]);
        // Same content (only the other window's active list differs): no change.
        let mut same = other.clone();
        same.select_list(0);
        assert!(!b.adopt_shared(&same.to_json()));
        // Junk is ignored; a deleted active list falls back to the first.
        assert!(!b.adopt_shared("{nope"));
        let mut gone = other.clone();
        gone.select_list(gone.lists().iter().position(|l| l.name == "Mine").unwrap());
        gone.delete_active();
        assert!(b.adopt_shared(&gone.to_json()));
        assert_eq!(b.current().name, b.lists()[0].name);
    }

    #[test]
    fn backfilled_history_makes_a_window_measurable_at_once() {
        let mut t = PriceTracker::default();
        let now = 100 * MIN;
        feed(&mut t, now, &[("AUSDT", 112.0)]); // the only live sample
        assert_eq!(t.change_pct("AUSDT", 15, now), None, "not warm yet");
        // 1-minute history for the 16 minutes before; newer ones and junk are ignored.
        let mut pts: Vec<(i64, f64)> = (1..=16).map(|m| (now - m * MIN, 100.0)).collect();
        pts.push((now + MIN, 999.0));
        pts.push((now - 2 * MIN, f64::NAN));
        pts.push((now - 3 * MIN, -1.0));
        pts.push((now - 90 * MIN, 1.0));
        t.backfill("AUSDT", &pts, now);
        let c = t.change_pct("AUSDT", 15, now).unwrap();
        assert!((c - 12.0).abs() < 1e-9, "{c}");
        assert!(t.is_warm(15, now) && !t.is_warm(30, now));
        assert_eq!(t.samples["AUSDT"].len(), 17);
        assert!(
            t.samples["AUSDT"]
                .iter()
                .zip(t.samples["AUSDT"].iter().skip(1))
                .all(|(a, b)| a.0 < b.0),
            "ordered"
        );
        // A symbol with no live samples yet is created from history alone.
        t.backfill("BUSDT", &[(now - MIN, 5.0)], now);
        assert_eq!(t.samples["BUSDT"].len(), 1);
        // Backfilling twice does not duplicate.
        t.backfill("AUSDT", &pts, now);
        assert_eq!(t.samples["AUSDT"].len(), 17);
    }

    #[test]
    fn tracker_rate_limits_and_prunes() {
        let mut t = PriceTracker::default();
        feed(&mut t, 1_000, &[("A", 1.0)]);
        assert!(!t.record(1_000 + SAMPLE_EVERY_MS - 1, [("A", 2.0)].into_iter()));
        feed(&mut t, 1_000 + 40 * MIN, &[("A", 3.0)]);
        assert_eq!(t.samples["A"].len(), 1, "old samples pruned");
    }

    #[test]
    fn dynamic_groups_follow_the_tracker() {
        let mut w = Watchlists::default();
        let mut t = PriceTracker::default();
        let t0 = 5_000_000;
        feed(&mut t, t0, &[("UP", 100.0), ("DN", 100.0), ("FLAT", 100.0)]);
        feed(
            &mut t,
            t0 + 15 * MIN,
            &[("UP", 120.0), ("DN", 85.0), ("FLAT", 101.0)],
        );
        w.refresh_dynamic(&t, t0 + 15 * MIN);
        assert!(w.lists()[0].symbols.is_empty(), "manual group untouched");
        assert_eq!(w.lists()[1].symbols, vec!["UP"]);
        assert_eq!(w.lists()[2].symbols, vec!["DN"]);
        // Dynamic groups are read-only and not persisted.
        w.select_list(1);
        assert!(!w.add_symbol("X") && w.remove_selected().is_none());
        assert!(Watchlists::from_json(&w.to_json()).lists()[1]
            .symbols
            .is_empty());
    }

    #[test]
    fn hold_keeps_a_faded_symbol_for_a_while() {
        let mut w = Watchlists::default();
        assert!(w.add_dynamic(MoveRule::parse("15 10 up 5").unwrap()));
        let mut t = PriceTracker::default();
        let t0 = 20_000_000;
        feed(&mut t, t0, &[("A", 100.0)]);
        feed(&mut t, t0 + 15 * MIN, &[("A", 115.0)]);
        w.refresh_dynamic(&t, t0 + 15 * MIN);
        assert_eq!(w.current().symbols, vec!["A"]);
        // The move fades (window base catches up): still held 5 minutes.
        feed(&mut t, t0 + 16 * MIN, &[("A", 100.0)]);
        feed(&mut t, t0 + 17 * MIN, &[("A", 100.0)]);
        w.refresh_dynamic(&t, t0 + 19 * MIN);
        assert_eq!(w.current().symbols, vec!["A"], "held");
        w.refresh_dynamic(&t, t0 + 21 * MIN);
        assert!(w.current().symbols.is_empty(), "hold expired");
    }

    #[test]
    fn cursor_sticks_to_symbol_across_refresh() {
        let mut w = Watchlists::default();
        assert!(w.add_dynamic(MoveRule::parse("15 5 both").unwrap()));
        let mut t = PriceTracker::default();
        let t0 = 9_000_000;
        feed(&mut t, t0, &[("A", 100.0), ("B", 100.0)]);
        feed(&mut t, t0 + 15 * MIN, &[("A", 106.0), ("B", 120.0)]);
        w.refresh_dynamic(&t, t0 + 15 * MIN);
        assert_eq!(w.current().symbols, vec!["B", "A"]);
        w.step_row(1);
        assert_eq!(w.selected_symbol(), Some("A"));
        // B cools off, A overtakes: ordering flips, cursor stays on A.
        feed(&mut t, t0 + 16 * MIN, &[("A", 125.0), ("B", 104.0)]);
        w.refresh_dynamic(&t, t0 + 16 * MIN);
        assert_eq!(w.selected_symbol(), Some("A"));
    }

    #[test]
    fn add_many_to_targets_a_chosen_manual_list() {
        let mut w = Watchlists::default();
        w.add_list("Two");
        w.select_list(0);
        assert_eq!(w.add_many_to(3, ["A", "B", "A"].into_iter()), Some(2));
        assert_eq!(w.active_index(), 3, "target list becomes active");
        assert_eq!(w.current().symbols, vec!["A", "B"]);
        assert_eq!(w.add_many_to(1, ["X"].into_iter()), None, "dynamic refuses");
        assert_eq!(w.add_many_to(9, ["X"].into_iter()), None, "missing list");
    }

    #[test]
    fn side_cycles_and_add_many_dedups() {
        let mut w = Watchlists::default();
        assert_eq!(w.cycle_side(), Side::Long);
        assert_eq!(w.cycle_side(), Side::Short);
        assert_eq!(w.cycle_side(), Side::Any);
        assert_eq!(w.add_many(["A", "B", "A"].into_iter()), 2);
    }
}
