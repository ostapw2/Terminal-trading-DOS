//! Desks: saved arrangements of linked `dos` windows.
//!
//! A desk is a list of windows: which page each one shows, its link group,
//! symbol and timeframe, where it stands on the screen and how big it is.
//! [`open`] starts one WezTerm process (one Dock icon) that opens every
//! window frameless, each running `dos-commander --view .. --link ..`; the
//! windows then talk through the existing link sockets (`crate::link`), so a
//! desk needs nothing new at run time.  `:desk save` asks every linked window for its [`DeskWindow`] and
//! writes the file.
//!
//! Geometry: `x`/`y` are screen pixels of the window's top-left corner (what
//! `wezterm start --position` takes), `cols`/`rows` its size in cells.  On
//! macOS a window finds its own position through the window server: among
//! the windows of its WezTerm process, by its pixel size and, for windows of
//! the same size, by where it last was ([`pick_window`]); elsewhere the
//! position stays as it was in the file.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const MAX_WINDOWS: usize = 16;
pub const MAX_NAME_CHARS: usize = 32;
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 5;
pub const MAX_COLS: u16 = 600;
pub const MAX_ROWS: u16 = 300;
/// Screen coordinates beyond this are a typo (8K is 7680 wide).
const MAX_COORD: i32 = 20_000;
pub const DEFAULT_FONT_SIZE: f64 = 12.0;

/// Pages a desk window may show (`--view`).  `terminal` (ticket and
/// strategies), `book` and `account` are single-function pages for desk
/// windows; `trade` is F6 with its chart and book.
pub const VIEWS: &[&str] = &[
    "chart", "screener", "lists", "trade", "terminal", "stats", "agents", "lab", "settings",
    "book", "account", "tape", "liq", "watch", "compare", "main",
];

fn yes() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_font() -> f64 {
    DEFAULT_FONT_SIZE
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeskWindow {
    pub view: String,
    /// Link group 1-8; 0 = not linked.
    #[serde(default)]
    pub link: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tf: Option<String>,
    /// Only the page and a one-line header: no menu bar, F-key bar or
    /// status line.
    #[serde(default = "yes")]
    pub bare: bool,
    /// Keeps its own timeframe: follows the group's symbol and list, not its
    /// timeframe, and does not send its own (charts of several timeframes
    /// in one group).
    #[serde(default, skip_serializing_if = "is_false")]
    pub own_tf: bool,
    /// Its own text size (points), when not the desk's: a smaller one fits
    /// more rows (an order book with more levels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<f64>,
    pub x: i32,
    pub y: i32,
    pub cols: u16,
    pub rows: u16,
}

impl DeskWindow {
    /// Everything off is refused: the file is edited by hand and the same
    /// struct arrives from other windows over the link sockets.
    pub fn validate(&self) -> Result<(), String> {
        if !VIEWS.contains(&self.view.as_str()) {
            return Err(format!(
                "unknown view '{}' (one of: {})",
                self.view,
                VIEWS.join(", ")
            ));
        }
        if self.link > crate::link::MAX_GROUP {
            return Err(format!("link must be 0-{}", crate::link::MAX_GROUP));
        }
        if let Some(s) = &self.symbol {
            if !crate::link::valid_symbol(s) {
                return Err(format!("bad symbol '{s}'"));
            }
        }
        if let Some(t) = &self.tf {
            if !crate::link::valid_timeframe(t) {
                return Err(format!("bad timeframe '{t}'"));
            }
        }
        if !(-MAX_COORD..=MAX_COORD).contains(&self.x)
            || !(-MAX_COORD..=MAX_COORD).contains(&self.y)
        {
            return Err("position is off any screen".into());
        }
        if self
            .font
            .is_some_and(|f| !(MIN_FONT..=MAX_FONT).contains(&f))
        {
            return Err(format!("font must be {MIN_FONT}-{MAX_FONT}"));
        }
        if !(MIN_COLS..=MAX_COLS).contains(&self.cols)
            || !(MIN_ROWS..=MAX_ROWS).contains(&self.rows)
        {
            return Err(format!(
                "size must be {MIN_COLS}-{MAX_COLS} columns and {MIN_ROWS}-{MAX_ROWS} rows"
            ));
        }
        Ok(())
    }

    /// `dos-commander` arguments that reproduce this window.
    pub fn dos_args(&self) -> Vec<String> {
        let mut a = vec!["--view".to_string(), self.view.clone()];
        if self.link > 0 {
            a.extend(["--link".to_string(), self.link.to_string()]);
        }
        if let Some(s) = &self.symbol {
            a.extend(["--symbol".to_string(), s.clone()]);
        }
        if let Some(t) = &self.tf {
            a.extend(["--tf".to_string(), t.clone()]);
        }
        if self.bare {
            a.push("--bare".into());
        }
        if self.own_tf {
            a.push("--own-tf".into());
        }
        if let Some(f) = self.font {
            a.extend(["--font".to_string(), format!("{f:.1}")]);
        }
        a
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Desk {
    pub name: String,
    #[serde(default = "default_font")]
    pub font_size: f64,
    pub windows: Vec<DeskWindow>,
}

impl Desk {
    /// One window and no `main`: that window is the desk (it holds the
    /// desk's lock, scans the market, takes the trade lease at start).
    pub fn is_solo(&self) -> bool {
        self.windows.len() == 1 && self.windows[0].view != "main"
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!("desk name: 1-{MAX_NAME_CHARS} of A-Z a-z 0-9 - _"));
        }
        if !(MIN_FONT..=MAX_FONT).contains(&self.font_size) {
            return Err(format!("font_size must be {MIN_FONT}-{MAX_FONT}"));
        }
        if self.windows.is_empty() || self.windows.len() > MAX_WINDOWS {
            return Err(format!("a desk has 1-{MAX_WINDOWS} windows"));
        }
        for (i, w) in self.windows.iter().enumerate() {
            w.validate().map_err(|e| format!("window {}: {e}", i + 1))?;
        }
        Ok(())
    }
}

/// Desk names become file names: plain ASCII only.
pub fn valid_name(s: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Where desks live: `DOS_DESK_DIR`, else `desks/` next to the database
/// (`~/.dos/desks`); in demo mode next to the demo agent directory, so a
/// demo never touches `~/.dos`.
pub fn desk_dir(demo: bool) -> Option<PathBuf> {
    if let Ok(d) = std::env::var("DOS_DESK_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    if demo {
        return crate::agent::agent_dir(true).map(|d| d.join("desks"));
    }
    crate::data::db::db_path()?
        .parent()
        .map(|p| p.join("desks"))
}

fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

pub fn load(dir: &Path, name: &str) -> Result<Desk, String> {
    if !valid_name(name) {
        return Err(format!("no desk named '{name}'"));
    }
    let text = std::fs::read_to_string(path_of(dir, name))
        .map_err(|_| format!("no desk named '{name}' in {}", dir.display()))?;
    let desk: Desk =
        serde_json::from_str(&text).map_err(|e| format!("desk '{name}' is not valid: {e}"))?;
    desk.validate()?;
    Ok(desk)
}

pub fn save(dir: &Path, desk: &Desk) -> Result<PathBuf, String> {
    desk.validate()?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = path_of(dir, &desk.name);
    let text = serde_json::to_string_pretty(desk).map_err(|e| e.to_string())?;
    // Write then rename: a crash never leaves half a desk.
    let tmp = dir.join(format!(".{}.tmp", desk.name));
    std::fs::write(&tmp, text + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Desk names in `dir`, sorted.
pub fn list(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().into_string().ok()?;
                    let stem = n.strip_suffix(".json")?;
                    valid_name(stem).then(|| stem.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The `basic` template on a screen of `w` x `h` pixels from 0,0: chart,
/// order book and tape on top, the trading terminal and the main window
/// below, all in link group 1.  Cell sizes are estimated from the font (`cell_px`).
pub fn starter(name: &str, w: i32, h: i32, cell_px: (i32, i32)) -> Desk {
    let screen = Frame { x: 0, y: 0, w, h };
    builtin("basic", name, screen, cell_px).expect("basic is built in")
}

// ─── Templates ──────────────────────────────────────────────────────────

/// One window of a built-in template: page, timeframe (`Some` = the window
/// keeps its own), link group, symbol (`None`: the group's), and its place
/// as a share of the screen (x, y, w, h).
type Slot = (
    &'static str,
    Option<&'static str>,
    u8,
    Option<&'static str>,
    [f64; 4],
);

/// Built-in templates: name, what it is for, windows.  Every one has a
/// `main` window (menu, account, open positions); the others show one page
/// each, without the menu.
pub const TEMPLATES: &[(&str, &str, &[Slot])] = &[
    (
        "basic",
        "chart, book, tape, terminal, main",
        &[
            ("chart", None, 1, None, [0.0, 0.0, 0.6, 0.6]),
            ("book", None, 1, None, [0.6, 0.0, 0.2, 0.6]),
            ("tape", None, 1, None, [0.8, 0.0, 0.2, 0.6]),
            ("terminal", None, 1, None, [0.0, 0.6, 0.6, 0.4]),
            ("main", None, 1, None, [0.6, 0.6, 0.4, 0.4]),
        ],
    ),
    (
        "trade",
        "active trading: 1m + 5m charts, book, tape, terminal, main",
        &[
            ("chart", Some("1m"), 1, None, [0.0, 0.0, 0.42, 0.65]),
            ("chart", Some("5m"), 1, None, [0.42, 0.0, 0.3, 0.65]),
            ("book", None, 1, None, [0.72, 0.0, 0.16, 0.65]),
            ("tape", None, 1, None, [0.88, 0.0, 0.12, 0.65]),
            ("terminal", None, 1, None, [0.0, 0.65, 0.55, 0.35]),
            ("main", None, 1, None, [0.55, 0.65, 0.45, 0.35]),
        ],
    ),
    (
        "research",
        "research: 1d + 1h charts, compare, lab, watch, main",
        &[
            ("chart", Some("1d"), 1, None, [0.0, 0.0, 0.4, 0.62]),
            ("chart", Some("1h"), 1, None, [0.4, 0.0, 0.3, 0.62]),
            ("compare", None, 1, None, [0.7, 0.0, 0.3, 0.62]),
            ("lab", None, 1, None, [0.0, 0.62, 0.4, 0.38]),
            ("watch", None, 1, None, [0.4, 0.62, 0.25, 0.38]),
            ("main", None, 1, None, [0.65, 0.62, 0.35, 0.38]),
        ],
    ),
    (
        "research2",
        "research 2: Compare, the trading terminal and the account with open positions and baskets",
        &[
            ("compare", None, 1, None, [0.0, 0.0, 0.6, 1.0]),
            ("terminal", None, 1, None, [0.6, 0.0, 0.4, 0.5]),
            ("main", None, 1, None, [0.6, 0.5, 0.4, 0.5]),
        ],
    ),
    (
        "liquidations",
        "liquidations: the liq page, the whole screen (no main window)",
        &[("liq", None, 1, None, [0.0, 0.0, 1.0, 1.0])],
    ),
    (
        "arbitrage",
        "arbitrage: two legs (chart + book each, link 1 and 2), compare, main",
        &[
            ("chart", None, 1, Some("BTCUSDT"), [0.0, 0.0, 0.34, 0.6]),
            ("book", None, 1, Some("BTCUSDT"), [0.34, 0.0, 0.16, 0.6]),
            ("chart", None, 2, Some("ETHUSDT"), [0.5, 0.0, 0.34, 0.6]),
            ("book", None, 2, Some("ETHUSDT"), [0.84, 0.0, 0.16, 0.6]),
            ("compare", None, 0, None, [0.0, 0.6, 0.55, 0.4]),
            ("main", None, 1, None, [0.55, 0.6, 0.45, 0.4]),
        ],
    ),
    (
        "scalp",
        "1m chart, book, tape, terminal, main",
        &[
            ("chart", Some("1m"), 1, None, [0.0, 0.0, 0.55, 0.65]),
            ("book", None, 1, None, [0.55, 0.0, 0.25, 0.65]),
            ("tape", None, 1, None, [0.8, 0.0, 0.2, 0.65]),
            ("terminal", None, 1, None, [0.0, 0.65, 0.6, 0.35]),
            ("main", None, 1, None, [0.6, 0.65, 0.4, 0.35]),
        ],
    ),
    (
        "day",
        "1h + 5m charts, book, watch, terminal, main",
        &[
            ("chart", Some("1h"), 1, None, [0.0, 0.0, 0.4, 0.6]),
            ("chart", Some("5m"), 1, None, [0.4, 0.0, 0.35, 0.6]),
            ("book", None, 1, None, [0.75, 0.0, 0.25, 0.6]),
            ("watch", None, 1, None, [0.0, 0.6, 0.2, 0.4]),
            ("terminal", None, 1, None, [0.2, 0.6, 0.5, 0.4]),
            ("main", None, 1, None, [0.7, 0.6, 0.3, 0.4]),
        ],
    ),
    (
        "swing",
        "1d + 1h charts, watch, main",
        &[
            ("chart", Some("1d"), 1, None, [0.0, 0.0, 0.5, 0.7]),
            ("chart", Some("1h"), 1, None, [0.5, 0.0, 0.3, 0.7]),
            ("watch", None, 1, None, [0.8, 0.0, 0.2, 0.7]),
            ("main", None, 1, None, [0.0, 0.7, 1.0, 0.3]),
        ],
    ),
    (
        "analysis",
        "four charts: 1d 1h 5m 1m, main",
        &[
            ("chart", Some("1d"), 1, None, [0.0, 0.0, 0.5, 0.38]),
            ("chart", Some("1h"), 1, None, [0.5, 0.0, 0.5, 0.38]),
            ("chart", Some("5m"), 1, None, [0.0, 0.38, 0.5, 0.38]),
            ("chart", Some("1m"), 1, None, [0.5, 0.38, 0.5, 0.38]),
            ("main", None, 1, None, [0.0, 0.76, 1.0, 0.24]),
        ],
    ),
    (
        "agents",
        "chart, agents (F8), main",
        &[
            ("chart", None, 1, None, [0.0, 0.0, 0.6, 0.65]),
            ("agents", None, 1, None, [0.6, 0.0, 0.4, 0.65]),
            ("main", None, 1, None, [0.0, 0.65, 1.0, 0.35]),
        ],
    ),
];

/// One main window per desk: the main window holds this lock (an OS file
/// lock, released when its process ends, even by a crash).  `None`: another
/// main window of `desk` holds it (or the directory is not writable).
pub fn lock_main(dir: &Path, desk: &str) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{desk}.main.lock")))
        .ok()?;
    f.try_lock().ok()?;
    Some(f)
}

/// Is a main window of `desk` open (holding [`lock_main`])?
pub fn main_is_open(dir: &Path, desk: &str) -> bool {
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{desk}.main.lock")))
    else {
        return false;
    };
    f.try_lock().is_err()
}

/// A desk saved before main windows existed gets one: its first `account`
/// window becomes `main` (the same positions, plus the menu), or a main
/// window is added beside the last one.  Extra main windows become
/// `account`.  A solo desk (one window, [`Desk::is_solo`]) is left alone:
/// its one window holds the desk's lock and scans in main's place.
/// Returns whether anything changed.
pub fn ensure_main(desk: &mut Desk) -> bool {
    if desk.is_solo() {
        return false;
    }
    let mut changed = false;
    let mut seen = false;
    for w in desk.windows.iter_mut() {
        if w.view == "main" {
            if seen {
                w.view = "account".into();
                w.bare = true;
                changed = true;
            }
            seen = true;
        }
    }
    if seen {
        return changed;
    }
    if let Some(w) = desk.windows.iter_mut().find(|w| w.view == "account") {
        w.view = "main".into();
        w.bare = false;
        return true;
    }
    let (x, y) = desk
        .windows
        .last()
        .map_or((0, 60), |w| (w.x + 40, w.y + 40));
    desk.windows.push(DeskWindow {
        view: "main".into(),
        link: 1,
        symbol: None,
        tf: None,
        bare: false,
        own_tf: false,
        font: None,
        x,
        y,
        cols: 120.clamp(MIN_COLS, MAX_COLS),
        rows: 30.clamp(MIN_ROWS, MAX_ROWS),
    });
    true
}

/// Workspace presets offered in the main window's menu: the desk's name, the
/// template it starts from, the menu label.
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("Trade", "trade", "Active trading"),
    ("Research", "research", "Research"),
    ("Research2", "research2", "Research 2"),
    ("Liquidations", "liquidations", "Liquidations"),
    ("Arbitrage", "arbitrage", "Arbitrage"),
];

/// A built-in template laid out on a usable screen `(x, y, w, h)` in
/// pixels, with cells of `cell_px`.  Windows share edges: each one runs to
/// where the next begins.
pub fn builtin(template: &str, name: &str, screen: Frame, cell_px: (i32, i32)) -> Option<Desk> {
    let (_, _, slots) = TEMPLATES.iter().find(|(t, _, _)| *t == template)?;
    let (cw, ch) = (cell_px.0.max(1), cell_px.1.max(1));
    let px = |share: f64, len: i32| (share * len as f64).round() as i32;
    let windows = slots
        .iter()
        .map(|(view, tf, link, symbol, [fx, fy, fw, fh])| {
            let (x0, y0) = (px(*fx, screen.w), px(*fy, screen.h));
            let (x1, y1) = (px(fx + fw, screen.w), px(fy + fh, screen.h));
            DeskWindow {
                view: (*view).into(),
                link: *link,
                symbol: symbol.map(str::to_string),
                tf: tf.map(str::to_string),
                bare: *view != "main",
                own_tf: tf.is_some(),
                font: None,
                x: screen.x + x0,
                y: screen.y + y0,
                cols: (((x1 - x0) / cw).max(1) as u16).clamp(MIN_COLS, MAX_COLS),
                rows: (((y1 - y0) / ch).max(1) as u16).clamp(MIN_ROWS, MAX_ROWS),
            }
        })
        .collect();
    Some(Desk {
        name: name.into(),
        font_size: DEFAULT_FONT_SIZE,
        windows,
    })
}

/// User templates (saved desks) live in `templates/` next to the desks.
pub fn template_dir(desk_dir: &Path) -> PathBuf {
    desk_dir.join("templates")
}

/// Keep desk `desk` as the template `template`.  Built-in names are taken.
pub fn save_template(dir: &Path, desk: &str, template: &str) -> Result<PathBuf, String> {
    if TEMPLATES
        .iter()
        .any(|(t, _, _)| t.eq_ignore_ascii_case(template))
    {
        return Err(format!(
            "'{template}' is a built-in template: pick another name"
        ));
    }
    let mut d = load(dir, desk)?;
    d.name = template.to_string();
    save(&template_dir(dir), &d)
}

/// A new desk `name` from `template`: a user template if one has that name,
/// else a built-in one laid out on `screen`.
pub fn from_template(
    dir: &Path,
    template: &str,
    name: &str,
    screen: Frame,
    cell_px: (i32, i32),
) -> Result<Desk, String> {
    if let Ok(mut d) = load(&template_dir(dir), template) {
        d.name = name.to_string();
        return Ok(d);
    }
    builtin(&template.to_ascii_lowercase(), name, screen, cell_px).ok_or_else(|| {
        format!(
            "no template '{template}' (templates: {})",
            template_names(dir).join(", ")
        )
    })
}

/// Built-in template names, then the user's.
pub fn template_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = TEMPLATES.iter().map(|(t, _, _)| t.to_string()).collect();
    v.extend(list(&template_dir(dir)));
    v
}

// ─── WezTerm ────────────────────────────────────────────────────────────

/// The WezTerm binary: `DOS_WEZTERM`, the copy on the DEV carrier,
/// `/Applications`, then `wezterm` on `PATH`.
pub fn wezterm_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DOS_WEZTERM") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    for p in [
        "/Volumes/DEV/Apps/WezTerm.app/Contents/MacOS/wezterm",
        "/Applications/WezTerm.app/Contents/MacOS/wezterm",
    ] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| {
            d.join(if cfg!(windows) {
                "wezterm.exe"
            } else {
                "wezterm"
            })
        })
        .find(|p| p.exists())
}

/// A Lua string literal.
fn lua_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The WezTerm config of one desk: no title bars (edges still resize), no
/// tab bar, no padding, the dos palette's black, Cmd/Super + drag to move a
/// window (there is no title bar to grab), and every window of the desk
/// spawned at start in this one GUI process, so the Dock shows one icon.
pub fn wezterm_config(desk: &Desk, dos: &Path, extra: &[String]) -> String {
    let mut spawns = String::new();
    for w in &desk.windows {
        let mut args = vec![dos.display().to_string()];
        args.extend(w.dos_args());
        args.extend(["--desk-pos".to_string(), format!("{},{}", w.x, w.y)]);
        args.extend(extra.iter().cloned());
        let args: Vec<String> = args.iter().map(|a| lua_str(a)).collect();
        spawns.push_str(&format!(
            "  mux.spawn_window {{ args = {{ {} }}, width = {}, height = {}, \
             position = {{ x = {}, y = {}, origin = \"ScreenCoordinateSystem\" }}, \
             set_environment_variables = {{ {DESK_ENV} = NAME }} }}\n",
            args.join(", "),
            w.cols,
            w.rows,
            w.x,
            w.y
        ));
    }
    let views: Vec<String> = VIEWS
        .iter()
        .map(|v| format!("[{}] = true", lua_str(v)))
        .collect();
    let tfs: Vec<String> = crate::markets::Timeframe::ALL
        .iter()
        .map(|t| format!("[{}] = true", lua_str(t.name())))
        .collect();
    let extra: Vec<String> = extra.iter().map(|a| lua_str(a)).collect();
    format!(
        r##"-- Generated by dos-commander for the desk {name}; rewritten on every `desk open`.
local wezterm = require 'wezterm'
local mux = wezterm.mux
local NAME = {name}
local DOS = {dos}
local EXTRA = {{ {extra} }}
local VIEWS = {{ {views} }}
local TFS = {{ {tfs} }}
wezterm.on('gui-startup', function(cmd)
{spawns}end)

-- A dos window talks to its desk through user vars (OSC 1337 SetUserVar):
-- `:win add` asks for a new window (same process, one Dock icon); snapping
-- and `:win tile` move or resize the window that asks.  Requests are data
-- only: the program and its flags are fixed here, every number is clamped.
local function int(v, lo, hi, def)
  v = tonumber(v)
  if v == nil then return def end
  v = math.floor(v)
  if v < lo then return lo elseif v > hi then return hi end
  return v
end
local function spawn(r)
  if not VIEWS[r.view] then return end
  local args = {{ DOS, '--view', r.view }}
  local link = int(r.link, 0, {max_group}, 0)
  if link > 0 then table.insert(args, '--link'); table.insert(args, tostring(link)) end
  local s = r.symbol
  if type(s) == 'string' and #s >= 2 and #s <= 20 and s:match('^[A-Z0-9]+$') then
    table.insert(args, '--symbol'); table.insert(args, s)
  end
  if type(r.tf) == 'string' and TFS[r.tf] then table.insert(args, '--tf'); table.insert(args, r.tf) end
  if r.bare == true then table.insert(args, '--bare') end
  if r.own_tf == true then table.insert(args, '--own-tf') end
  local f = tonumber(r.font)
  if f ~= nil and f >= {min_font} and f <= {max_font} then
    table.insert(args, '--font'); table.insert(args, string.format('%.1f', f))
  end
  local x = int(r.x, -{max_coord}, {max_coord}, 0)
  local y = int(r.y, -{max_coord}, {max_coord}, 0)
  table.insert(args, '--desk-pos'); table.insert(args, x .. ',' .. y)
  for _, a in ipairs(EXTRA) do table.insert(args, a) end
  mux.spawn_window {{
    args = args,
    width = int(r.cols, {min_cols}, {max_cols}, 80),
    height = int(r.rows, {min_rows}, {max_rows}, 24),
    position = {{ x = x, y = y, origin = "ScreenCoordinateSystem" }},
    set_environment_variables = {{ {DESK_ENV} = NAME }},
  }}
end
local function move(window, r)
  if r.w ~= nil and r.h ~= nil then
    window:set_inner_size(int(r.w, 100, {max_coord}, 800), int(r.h, 60, {max_coord}, 600))
  end
  window:set_position(int(r.x, -{max_coord}, {max_coord}, 0), int(r.y, -{max_coord}, {max_coord}, 0))
end
wezterm.on('user-var-changed', function(window, pane, var, value)
  if var ~= {var} and var ~= {move_var} and var ~= {font_var} then return end
  local ok, r = pcall(wezterm.json_parse, value)
  if not ok or type(r) ~= 'table' then return end
  if var == {font_var} then
    local f = tonumber(r.size)
    if f ~= nil and f >= {min_font} and f <= {max_font} then
      window:set_config_overrides({{ font_size = f }})
    end
  elseif var == {var} then spawn(r) else move(window, r) end
end)

return {{
  window_decorations = "RESIZE",
  enable_tab_bar = false,
  window_padding = {{ left = 0, right = 0, top = 0, bottom = 0 }},
  font_size = {font_size:.1},
  colors = {{ background = "#000000" }},
  check_for_updates = false,
  window_close_confirmation = "NeverPrompt",
  exit_behavior = "Close",
  quit_when_all_windows_are_closed = true,
  adjust_window_size_when_changing_font_size = false,
  mouse_bindings = {{
    {{ event = {{ Drag = {{ streak = 1, button = 'Left' }} }}, mods = 'SUPER', action = wezterm.action.StartWindowDrag }},
  }},
}}
"##,
        name = lua_str(&desk.name),
        dos = lua_str(&dos.display().to_string()),
        extra = extra.join(", "),
        views = views.join(", "),
        tfs = tfs.join(", "),
        var = lua_str(SPAWN_VAR),
        move_var = lua_str(MOVE_VAR),
        font_var = lua_str(FONT_VAR),
        min_font = MIN_FONT,
        max_font = MAX_FONT,
        max_group = crate::link::MAX_GROUP,
        max_coord = MAX_COORD,
        min_cols = MIN_COLS,
        max_cols = MAX_COLS,
        min_rows = MIN_ROWS,
        max_rows = MAX_ROWS,
        font_size = desk.font_size,
    )
}

/// The environment variable every desk window gets: the desk's name.  A
/// window without it is not in a desk and cannot add windows.
pub const DESK_ENV: &str = "DOS_DESK";
/// The user var a window sets to ask its desk for a new window.
pub const SPAWN_VAR: &str = "dos_spawn";
/// The user var a window sets to move (and resize) itself.
pub const MOVE_VAR: &str = "dos_move";
/// The user var a window sets to change its own text size.
pub const FONT_VAR: &str = "dos_font";
/// Text sizes a desk or a window may have (points).
pub const MIN_FONT: f64 = 6.0;
pub const MAX_FONT: f64 = 40.0;

/// The escape sequence that sets this window's text size (`MIN_FONT`..
/// `MAX_FONT` points); the window keeps its size in pixels, so a smaller
/// text gives more rows.
pub fn font_request(size: f64, nonce: u64) -> String {
    let v = serde_json::json!({ "size": size.clamp(MIN_FONT, MAX_FONT), "n": nonce });
    let b64 = crate::agent::chart_png::base64(v.to_string().as_bytes());
    format!("\x1b]1337;SetUserVar={FONT_VAR}={b64}\x07")
}

/// The escape sequence that moves this window to `x`,`y` (screen pixels)
/// and, with `size`, sets its inner size in pixels.  `nonce` makes a repeat
/// a change, so WezTerm reports it.
pub fn move_request(x: i32, y: i32, size: Option<(i32, i32)>, nonce: u64) -> String {
    let mut v = serde_json::json!({ "x": x, "y": y, "n": nonce });
    if let Some((w, h)) = size {
        v["w"] = w.into();
        v["h"] = h.into();
    }
    let b64 = crate::agent::chart_png::base64(v.to_string().as_bytes());
    format!("\x1b]1337;SetUserVar={MOVE_VAR}={b64}\x07")
}

/// The desk this window belongs to (`DOS_DESK`, set by the desk's WezTerm).
pub fn current_desk() -> Option<String> {
    std::env::var(DESK_ENV).ok().filter(|n| valid_name(n))
}

/// Is this window the only one of its desk (a solo desk, [`Desk::is_solo`])?
/// Read from the desk file; `false` outside a desk or when the file is gone.
pub fn current_is_solo(demo: bool) -> bool {
    let (Some(name), Some(dir)) = (current_desk(), desk_dir(demo)) else {
        return false;
    };
    load(&dir, &name).is_ok_and(|d| d.is_solo())
}

/// The escape sequence that asks this window's desk for `win`: an OSC 1337
/// SetUserVar, which WezTerm hands to the desk config's `user-var-changed`.
/// `nonce` makes two identical requests two windows.
pub fn spawn_request(win: &DeskWindow, nonce: u64) -> Result<String, String> {
    win.validate()?;
    let mut v = serde_json::to_value(win).map_err(|e| e.to_string())?;
    v["n"] = serde_json::Value::from(nonce);
    let b64 = crate::agent::chart_png::base64(v.to_string().as_bytes());
    Ok(format!("\x1b]1337;SetUserVar={SPAWN_VAR}={b64}\x07"))
}

/// Default size in cells of a window added to a desk.
pub fn default_size(view: &str) -> (u16, u16) {
    match view {
        "book" | "tape" => (48, 40),
        "liq" => (90, 30),
        "watch" => (40, 30),
        "account" => (100, 16),
        "terminal" => (100, 26),
        _ => (120, 40),
    }
}

/// Where a new window of `size` pixels goes: the first free spot right of,
/// left of, below or above `own` (this window), then beside each of the
/// `others` (the desk's other windows, in screen pixels), inside `screen`.
/// With no free spot: the spot that covers the least of the other windows,
/// moved down and right while it would sit exactly on one; on the screen.
pub fn place_beside(own: Frame, size: (i32, i32), others: &[Frame], screen: Frame) -> (i32, i32) {
    let (w, h) = size;
    let fits = |x: i32, y: i32| {
        x >= screen.x
            && y >= screen.y
            && x + w <= screen.x + screen.w
            && y + h <= screen.y + screen.h
    };
    // Edges that touch (or overlap by rounding) are not in the way.
    const SLACK: i32 = 8;
    let free = |x: i32, y: i32| {
        std::iter::once(&own).chain(others).all(|f| {
            x + w <= f.x + SLACK
                || f.x + f.w <= x + SLACK
                || y + h <= f.y + SLACK
                || f.y + f.h <= y + SLACK
        })
    };
    let around = |f: &Frame| {
        let top = f.y.clamp(screen.y, (screen.y + screen.h - h).max(screen.y));
        let left = f.x.clamp(screen.x, (screen.x + screen.w - w).max(screen.x));
        [
            (f.x + f.w, top),
            (f.x - w, top),
            (left, f.y + f.h),
            (left, f.y - h),
        ]
    };
    let spots: Vec<(i32, i32)> = std::iter::once(&own)
        .chain(others)
        .flat_map(around)
        .collect();
    if let Some(&p) = spots.iter().find(|(x, y)| fits(*x, *y) && free(*x, *y)) {
        return p;
    }
    // A full screen: the spot that covers the least of the other windows,
    // and never exactly on top of one (it would hide it completely).
    let covered = |x: i32, y: i32| -> i64 {
        std::iter::once(&own)
            .chain(others)
            .map(|f| {
                let ow = ((x + w).min(f.x + f.w) - x.max(f.x)).max(0) as i64;
                let oh = ((y + h).min(f.y + f.h) - y.max(f.y)).max(0) as i64;
                ow * oh
            })
            .sum()
    };
    let (mut x, mut y) = spots
        .iter()
        .copied()
        .filter(|(x, y)| fits(*x, *y))
        .min_by_key(|(x, y)| covered(*x, *y))
        .unwrap_or((own.x + 80, own.y + 80));
    for _ in 0..MAX_WINDOWS {
        let stacked = std::iter::once(&own)
            .chain(others)
            .any(|f| (f.x - x).abs() <= SLACK && (f.y - y).abs() <= SLACK);
        if !stacked {
            break;
        }
        x += 80;
        y += 80;
    }
    let x = x.min(screen.x + screen.w - w).max(screen.x);
    let y = y.min(screen.y + screen.h - h).max(screen.y);
    (x, y)
}

/// Where `own` goes when it lets go near other windows: each of its edges
/// within `dist` pixels of a neighbour's edge (a window beside it, i.e.
/// overlapping it across) or of the screen's edge moves onto it; the
/// nearest wins, x and y separately.  `None` when nothing is that close (or
/// it is already there).
pub fn snap(own: Frame, others: &[Frame], screen: Frame, dist: i32) -> Option<(i32, i32)> {
    // Ranges that overlap or nearly touch.
    let across = |a0: i32, a1: i32, b0: i32, b1: i32| a0 < b1 + dist && b0 < a1 + dist;
    let best = |cur: i32, targets: &mut dyn Iterator<Item = i32>| {
        targets
            .filter(|t| (t - cur).abs() <= dist)
            .min_by_key(|t| (t - cur).abs())
            .unwrap_or(cur)
    };
    let mut xs = vec![screen.x, screen.x + screen.w - own.w];
    let mut ys = vec![screen.y, screen.y + screen.h - own.h];
    for o in others {
        if across(own.y, own.y + own.h, o.y, o.y + o.h) {
            xs.extend([o.x + o.w, o.x, o.x - own.w, o.x + o.w - own.w]);
        }
        if across(own.x, own.x + own.w, o.x, o.x + o.w) {
            ys.extend([o.y + o.h, o.y, o.y - own.h, o.y + o.h - own.h]);
        }
    }
    let x = best(own.x, &mut xs.into_iter());
    let y = best(own.y, &mut ys.into_iter());
    ((x, y) != (own.x, own.y)).then_some((x, y))
}

/// `n` windows tiled over `screen`: rows of equal height, as square a grid
/// as `n` allows; a short last row gets wider windows.  Row by row, left to
/// right.
pub fn tile(n: usize, screen: Frame) -> Vec<Frame> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let at = |start: i32, len: i32, i: usize, of: usize| {
        start + (len as i64 * i as i64 / of as i64) as i32
    };
    let mut out = Vec::with_capacity(n);
    for r in 0..rows {
        let k = if r + 1 < rows {
            cols
        } else {
            n - cols * (rows - 1)
        };
        let (y0, y1) = (
            at(screen.y, screen.h, r, rows),
            at(screen.y, screen.h, r + 1, rows),
        );
        for c in 0..k {
            let (x0, x1) = (
                at(screen.x, screen.w, c, k),
                at(screen.x, screen.w, c + 1, k),
            );
            out.push(Frame {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            });
        }
    }
    out
}

/// Plausible on-screen geometry (it arrives from other windows).
pub fn valid_frame(f: &Frame) -> bool {
    (-MAX_COORD..=MAX_COORD).contains(&f.x)
        && (-MAX_COORD..=MAX_COORD).contains(&f.y)
        && (1..=MAX_COORD).contains(&f.w)
        && (1..=MAX_COORD).contains(&f.h)
}

/// Arguments after the wezterm binary: one GUI process for the whole desk
/// (its own, so the user's other WezTerm windows are never touched).
pub fn wezterm_args(config: &Path) -> Vec<String> {
    vec![
        "--config-file".to_string(),
        config.display().to_string(),
        "start".to_string(),
        "--always-new-process".to_string(),
        "--class".to_string(),
        "dos.desk".to_string(),
    ]
}

/// Start the desk: one WezTerm process that opens every window.  `extra`
/// goes to each dos-commander (e.g. `--demo`).  Returns how many windows.
pub fn open(desk: &Desk, dir: &Path, extra: &[String]) -> Result<usize, String> {
    if main_is_open(dir, &desk.name) {
        return Err(format!("desk '{}' is already open", desk.name));
    }
    // Every desk has its main window (older files get one, saved back).
    let mut desk = desk.clone();
    if ensure_main(&mut desk) {
        let _ = save(dir, &desk);
    }
    let desk = &desk;
    desk.validate()?;
    let wez = wezterm_bin().ok_or(
        "WezTerm not found: install it (or set DOS_WEZTERM to its binary); \
         desks use it for windows without title bars",
    )?;
    let dos = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let config = dir.join(format!("wezterm-{}.lua", desk.name));
    std::fs::write(&config, wezterm_config(desk, &dos, extra))
        .map_err(|e| format!("{}: {e}", config.display()))?;
    Command::new(&wez)
        .args(wezterm_args(&config))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start WezTerm: {e}"))?;
    Ok(desk.windows.len())
}

// ─── Own position (macOS) ───────────────────────────────────────────────

/// A window on screen, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Which of the terminal's windows is this one?  All windows of a desk
/// belong to one process, so the window server cannot say.  The size
/// decides (`px`: this terminal's size in pixels); among windows of the same
/// size the one nearest `hint` (where this window opened or was last found)
/// wins.  `None` when it stays ambiguous.
pub fn pick_window(
    frames: &[Frame],
    px: Option<(i32, i32)>,
    hint: Option<(i32, i32)>,
) -> Option<(i32, i32)> {
    if frames.is_empty() {
        return None;
    }
    let mut cands: Vec<Frame> = frames.to_vec();
    if let Some((pw, ph)) = px.filter(|(w, h)| *w > 0 && *h > 0) {
        let score = |f: &Frame| (f.w - pw).abs() + (f.h - ph).abs();
        let best = cands.iter().map(score).min().unwrap_or(0);
        // Windows of "the same size" differ by rounding only.
        cands.retain(|f| score(f) <= best + 8);
    }
    if cands.len() == 1 {
        return Some((cands[0].x, cands[0].y));
    }
    let (hx, hy) = hint?;
    let dist = |f: &Frame| {
        let (dx, dy) = ((f.x - hx) as i64, (f.y - hy) as i64);
        dx * dx + dy * dy
    };
    let best = cands.iter().min_by_key(|f| dist(f))?;
    Some((best.x, best.y))
}

/// Parent PIDs of this process, nearest first (up to `n`).  The chain does
/// not change while the process runs: asked once.
#[cfg(target_os = "macos")]
fn ancestors(n: usize) -> &'static [u32] {
    static CHAIN: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    CHAIN.get_or_init(|| {
        let mut out = Vec::new();
        let mut pid = std::process::id();
        for _ in 0..n {
            let Ok(o) = Command::new("ps")
                .args(["-o", "ppid=", "-p", &pid.to_string()])
                .output()
            else {
                break;
            };
            match String::from_utf8_lossy(&o.stdout).trim().parse::<u32>() {
                Ok(p) if p > 1 => {
                    out.push(p);
                    pid = p;
                }
                _ => break,
            }
        }
        out
    })
}

/// The window server, read directly (about a millisecond; no process, no
/// permission: bounds and owners need none, titles would).
#[cfg(target_os = "macos")]
mod cg {
    use super::Frame;
    use std::ffi::c_void;

    type Ref = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(a: Ref) -> isize;
        fn CFArrayGetValueAtIndex(a: Ref, i: isize) -> Ref;
        fn CFDictionaryGetValue(d: Ref, key: Ref) -> Ref;
        fn CFNumberGetValue(n: Ref, kind: isize, out: *mut c_void) -> bool;
        fn CFRelease(r: Ref);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        static kCGWindowOwnerPID: Ref;
        static kCGWindowLayer: Ref;
        static kCGWindowBounds: Ref;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> Ref;
        fn CGRectMakeWithDictionaryRepresentation(d: Ref, rect: *mut Rect) -> bool;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyDisplayMode(display: u32) -> Ref;
        fn CGDisplayModeGetWidth(mode: Ref) -> usize;
        fn CGDisplayModeGetPixelWidth(mode: Ref) -> usize;
        fn CGDisplayModeRelease(mode: Ref);
    }

    const ON_SCREEN_ONLY: u32 = 1;
    const SINT64: isize = 4;

    fn number(d: Ref, key: Ref) -> Option<i64> {
        let mut v: i64 = 0;
        // SAFETY: `d` is a live CFDictionary; a missing key gives null.
        unsafe {
            let n = CFDictionaryGetValue(d, key);
            (!n.is_null() && CFNumberGetValue(n, SINT64, &mut v as *mut i64 as *mut c_void))
                .then_some(v)
        }
    }

    /// Pixels per point of the main display.
    pub fn scale() -> f64 {
        // SAFETY: the mode is released after use; null is checked.
        unsafe {
            let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
            if mode.is_null() {
                return 1.0;
            }
            let (pts, px) = (
                CGDisplayModeGetWidth(mode),
                CGDisplayModeGetPixelWidth(mode),
            );
            CGDisplayModeRelease(mode);
            if pts > 0 && px > 0 {
                px as f64 / pts as f64
            } else {
                1.0
            }
        }
    }

    /// On-screen normal windows of `pid`, in pixels.
    pub fn windows_of(pid: u32) -> Vec<Frame> {
        let k = scale();
        let mut out = Vec::new();
        // SAFETY: the array is owned here (Copy rule) and released once;
        // every element is a CFDictionary per the API; keys are CF constants.
        unsafe {
            let arr = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY, 0);
            if arr.is_null() {
                return out;
            }
            for i in 0..CFArrayGetCount(arr) {
                let d = CFArrayGetValueAtIndex(arr, i);
                if d.is_null()
                    || number(d, kCGWindowOwnerPID) != Some(pid as i64)
                    || number(d, kCGWindowLayer) != Some(0)
                {
                    continue;
                }
                let b = CFDictionaryGetValue(d, kCGWindowBounds);
                let mut r = Rect::default();
                if b.is_null() || !CGRectMakeWithDictionaryRepresentation(b, &mut r) || r.w <= 0.0 {
                    continue;
                }
                out.push(Frame {
                    x: (r.x * k).round() as i32,
                    y: (r.y * k).round() as i32,
                    w: (r.w * k).round() as i32,
                    h: (r.h * k).round() as i32,
                });
            }
            CFRelease(arr);
        }
        out
    }
}

/// Every window of this terminal's process, in screen pixels: the windows
/// of the nearest ancestor that has any (dos runs under a shell or straight
/// under WezTerm).  About a millisecond.
#[cfg(target_os = "macos")]
pub fn own_frames() -> Vec<Frame> {
    ancestors(4)
        .iter()
        .map(|p| cg::windows_of(*p))
        .find(|v| !v.is_empty())
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
pub fn own_frames() -> Vec<Frame> {
    Vec::new()
}

/// Pixels per point of the main display (1 where unknown).
#[cfg(target_os = "macos")]
pub fn screen_scale() -> f64 {
    cg::scale()
}

#[cfg(not(target_os = "macos"))]
pub fn screen_scale() -> f64 {
    1.0
}

/// Where this window's top-left corner is, in screen pixels (see
/// [`pick_window`]).
pub fn own_position(px: Option<(i32, i32)>, hint: Option<(i32, i32)>) -> Option<(i32, i32)> {
    pick_window(&own_frames(), px, hint)
}

/// The main screen's usable area (no menu bar, no Dock) in pixels:
/// `(width, height, scale, top)`, where `top` is the first usable row.
#[cfg(target_os = "macos")]
pub fn screen_pixels() -> Option<(i32, i32, f64, i32)> {
    const JS: &str = "ObjC.import('AppKit'); var s = $.NSScreen.mainScreen; \
        var f = s.frame, v = s.visibleFrame, k = s.backingScaleFactor; \
        [v.size.width * k, v.size.height * k, k, (f.size.height - v.origin.y - v.size.height) * k].map(Math.round).join(' ')";
    let out = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", JS])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Vec<f64> = text
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    match v.as_slice() {
        [w, h, k, top] if *w > 0.0 && *h > 0.0 && *k > 0.0 => {
            Some((*w as i32, *h as i32, *k, *top as i32))
        }
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn screen_pixels() -> Option<(i32, i32, f64, i32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_desk_gets_one_main_window_and_one_lock() {
        let mut d = starter("D", 2000, 1000, (10, 20));
        assert!(!ensure_main(&mut d), "templates already have one");
        // An old desk: its account window becomes main.
        for w in d.windows.iter_mut().filter(|w| w.view == "main") {
            w.view = "account".into();
            w.bare = true;
        }
        assert!(ensure_main(&mut d));
        let m: Vec<_> = d.windows.iter().filter(|w| w.view == "main").collect();
        assert_eq!(m.len(), 1);
        assert!(!m[0].bare);
        // No account window: one is added; two mains: the second goes.
        d.windows.retain(|w| w.view != "main");
        let n = d.windows.len();
        assert!(ensure_main(&mut d) && d.windows.len() == n + 1);
        d.windows.push(d.windows[n].clone());
        assert!(ensure_main(&mut d));
        assert_eq!(d.windows.iter().filter(|w| w.view == "main").count(), 1);
        assert!(d.validate().is_ok());
        // The lock: one holder; free again when it is dropped.
        let dir = std::env::temp_dir().join(format!("dos-main-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!main_is_open(&dir, "D"));
        let held = lock_main(&dir, "D").unwrap();
        assert!(main_is_open(&dir, "D") && lock_main(&dir, "D").is_none());
        assert!(!main_is_open(&dir, "Other"), "per desk");
        drop(held);
        assert!(!main_is_open(&dir, "D"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn win() -> DeskWindow {
        DeskWindow {
            view: "book".into(),
            link: 2,
            symbol: Some("BTCUSDT".into()),
            tf: Some("1h".into()),
            bare: true,
            own_tf: false,
            font: None,
            x: 100,
            y: 50,
            cols: 60,
            rows: 30,
        }
    }

    #[test]
    fn windows_are_validated_field_by_field() {
        assert!(win().validate().is_ok());
        for bad in [
            DeskWindow {
                view: "nope".into(),
                ..win()
            },
            DeskWindow { link: 9, ..win() },
            DeskWindow {
                symbol: Some("btc usdt".into()),
                ..win()
            },
            DeskWindow {
                tf: Some("2h".into()),
                ..win()
            },
            DeskWindow { x: 99_999, ..win() },
            DeskWindow { cols: 5, ..win() },
            DeskWindow {
                rows: 1_000,
                ..win()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_window_becomes_dos_and_wezterm_arguments() {
        assert_eq!(
            win().dos_args(),
            ["--view", "book", "--link", "2", "--symbol", "BTCUSDT", "--tf", "1h", "--bare"]
        );
        let plain = DeskWindow {
            link: 0,
            symbol: None,
            tf: None,
            bare: false,
            ..win()
        };
        assert_eq!(plain.dos_args(), ["--view", "book"]);
        let a = wezterm_args(Path::new("/c.lua")).join(" ");
        assert_eq!(
            a,
            "--config-file /c.lua start --always-new-process --class dos.desk"
        );
    }

    #[test]
    fn desks_round_trip_through_files_and_bad_ones_are_refused() {
        let dir = std::env::temp_dir().join(format!("dos-desk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = starter("Day", 3840, 2160, (16, 32));
        assert_eq!(d.windows.len(), 5);
        assert!(d.validate().is_ok(), "{:?}", d.validate());
        save(&dir, &d).unwrap();
        assert_eq!(load(&dir, "Day").unwrap(), d);
        assert_eq!(list(&dir), vec!["Day".to_string()]);
        assert!(load(&dir, "../etc").is_err(), "names never become paths");
        assert!(load(&dir, "Missing").is_err());
        std::fs::write(dir.join("Broken.json"), "{").unwrap();
        assert!(load(&dir, "Broken").unwrap_err().contains("not valid"));
        let mut too_many = d.clone();
        too_many.windows = vec![win(); MAX_WINDOWS + 1];
        assert!(save(&dir, &too_many).is_err());
        assert!(save(
            &dir,
            &Desk {
                name: "a b".into(),
                ..d.clone()
            }
        )
        .is_err());
        // Hand-written files may omit the defaults.
        std::fs::write(
            dir.join("Min.json"),
            r#"{"name":"Min","windows":[{"view":"chart","x":0,"y":0,"cols":80,"rows":24}]}"#,
        )
        .unwrap();
        let m = load(&dir, "Min").unwrap();
        assert_eq!(
            (m.font_size, m.windows[0].bare, m.windows[0].link),
            (DEFAULT_FONT_SIZE, true, 0)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_starter_desk_tiles_the_screen() {
        let d = starter("S", 2000, 1000, (10, 20));
        let total_w: i32 = d.windows[..3].iter().map(|w| w.cols as i32 * 10).sum();
        assert!((1990..=2000).contains(&total_w), "{total_w}");
        assert_eq!(d.windows[1].x, d.windows[0].x + 1200);
        assert!(d
            .windows
            .iter()
            .all(|w| w.link == 1 && w.bare == (w.view != "main")));
    }

    #[test]
    fn one_config_spawns_every_window_without_title_bars() {
        let mut d = starter("Day", 2000, 1000, (10, 20));
        d.windows[0].symbol = Some("BTCUSDT".into());
        let c = wezterm_config(&d, Path::new("/x/dos \"q\""), &["--demo".into()]);
        assert!(c.contains(r#"window_decorations = "RESIZE""#) && c.contains("StartWindowDrag"));
        assert!(c.contains("enable_tab_bar = false") && c.contains("gui-startup"));
        // Five at start, one in the handler for added windows.
        assert_eq!(c.matches("mux.spawn_window").count(), 6);
        assert!(
            c.contains(
                r#""/x/dos \"q\"", "--view", "chart", "--link", "1", "--symbol", "BTCUSDT", "--bare", "--desk-pos", "0,0", "--demo""#
            ),
            "{c}"
        );
        assert!(
            c.contains(r#"position = { x = 1200, y = 0, origin = "ScreenCoordinateSystem" }"#),
            "{c}"
        );
        assert_eq!(lua_str("a\\b\n"), "\"a\\\\b\\n\"");
    }

    #[test]
    fn a_window_asks_its_desk_for_another_through_a_user_var() {
        let req = spawn_request(&win(), 7).unwrap();
        let b64 = req
            .strip_prefix("\x1b]1337;SetUserVar=dos_spawn=")
            .and_then(|r| r.strip_suffix('\x07'))
            .unwrap();
        assert!(b64
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)));
        assert!(spawn_request(
            &DeskWindow {
                view: "x".into(),
                ..win()
            },
            1
        )
        .is_err());
        // The config spawns only what it validates itself.
        let c = wezterm_config(&starter("Day", 2000, 1000, (10, 20)), Path::new("/d"), &[]);
        assert!(c.contains("wezterm.on('user-var-changed'") && c.contains(r#"var ~= "dos_spawn""#));
        assert!(c.contains(r#"local DOS = "/d""#) && c.contains(r#"["book"] = true"#));
        assert!(c.contains(r#"["1h"] = true"#) && c.contains("pcall(wezterm.json_parse"));
        assert_eq!(
            c.matches("set_environment_variables = { DOS_DESK = NAME }")
                .count(),
            6
        );
    }

    #[test]
    fn a_new_window_goes_beside_this_one_and_stays_on_screen() {
        let f = |x, y, w, h| Frame { x, y, w, h };
        let screen = f(0, 60, 3000, 1900);
        let p = |own, size, others: &[Frame]| place_beside(own, size, others, screen);
        // Room on the right.
        assert_eq!(p(f(0, 60, 1000, 800), (500, 800), &[]), (1000, 60));
        // No room on the right: left.
        assert_eq!(p(f(2000, 60, 1000, 800), (500, 800), &[]), (1500, 60));
        // A full-width window: below.
        assert_eq!(p(f(0, 60, 3000, 800), (500, 600), &[]), (0, 860));
        // Nowhere on screen free: cascaded, inside the screen.
        let (x, y) = p(f(0, 60, 3000, 1900), (2900, 1800), &[]);
        assert!(
            x >= 0 && y >= 60 && x + 2900 <= 3000 && y + 1800 <= 1960,
            "{x},{y}"
        );
        // A taller new window is pulled up so it fits beside.
        assert_eq!(p(f(0, 1000, 1000, 400), (500, 1200), &[]), (1000, 760));
        // The right is taken by another window: below this one instead.
        let book = f(1000, 60, 2000, 800);
        assert_eq!(p(f(0, 60, 1000, 800), (500, 800), &[book]), (0, 860));
        // Every side of this one taken: beside another window.
        let (a, b) = (f(0, 860, 1000, 1100), f(1000, 60, 1000, 800));
        assert_eq!(p(f(0, 60, 1000, 800), (500, 800), &[a, b]), (1000, 860));
        // A full screen: not exactly on top of another window.
        let full = [f(1000, 60, 2000, 1900), f(0, 860, 1000, 1100)];
        let (x, y) = p(f(0, 60, 1000, 800), (500, 800), &full);
        assert!(
            full.iter().all(|w| (w.x, w.y) != (x, y)) && x + 500 <= 3000 && y + 800 <= 1960,
            "{x},{y}"
        );
        // Touching edges (rounding) do not count as taken.
        assert_eq!(
            p(f(0, 60, 1000, 800), (500, 800), &[f(1504, 60, 100, 100)]),
            (1000, 60)
        );
    }

    #[test]
    fn templates_tile_the_screen_and_charts_keep_their_timeframes() {
        let screen = Frame {
            x: 0,
            y: 60,
            w: 3000,
            h: 1800,
        };
        for (t, _, slots) in TEMPLATES {
            let d = builtin(t, "X", screen, (10, 20)).unwrap();
            assert!(d.validate().is_ok(), "{t}: {:?}", d.validate());
            assert_eq!(d.windows.len(), slots.len());
            // Every window on the screen, none on top of another.
            for (i, a) in d.windows.iter().enumerate() {
                let (aw, ah) = (a.cols as i32 * 10, a.rows as i32 * 20);
                assert!(
                    a.x >= 0 && a.y >= 60 && a.x + aw <= 3000 && a.y + ah <= 1860,
                    "{t} {i}"
                );
                for b in &d.windows[i + 1..] {
                    let apart = a.x + aw <= b.x
                        || b.x + b.cols as i32 * 10 <= a.x
                        || a.y + ah <= b.y
                        || b.y + b.rows as i32 * 20 <= a.y;
                    assert!(apart, "{t}: {a:?} overlaps {b:?}");
                }
            }
        }
        let a = builtin("analysis", "A", screen, (10, 20)).unwrap();
        let charts: Vec<_> = a.windows.iter().filter(|w| w.view == "chart").collect();
        let tfs: Vec<_> = charts.iter().map(|w| w.tf.as_deref().unwrap()).collect();
        assert_eq!(tfs, ["1d", "1h", "5m", "1m"]);
        assert!(charts.iter().all(|w| w.own_tf && w.link == 1));
        // Every template has one main window, the only one with the menu —
        // except a solo one, whose single window stands in for it.
        for (t, _, _) in TEMPLATES {
            let d = builtin(t, "X", screen, (10, 20)).unwrap();
            let mains: Vec<_> = d.windows.iter().filter(|w| w.view == "main").collect();
            assert_eq!(mains.len(), usize::from(!d.is_solo()), "{t}");
            assert!(
                d.windows.iter().all(|w| w.bare == (w.view != "main")),
                "{t}"
            );
        }
        // Arbitrage: two legs in two link groups, each with its symbol.
        let arb = builtin("arbitrage", "A", screen, (10, 20)).unwrap();
        let legs: Vec<_> = arb
            .windows
            .iter()
            .filter(|w| w.view == "book")
            .map(|w| (w.link, w.symbol.as_deref()))
            .collect();
        assert_eq!(legs, [(1, Some("BTCUSDT")), (2, Some("ETHUSDT"))]);
        for (_, t, _) in PRESETS {
            assert!(builtin(t, "P", screen, (10, 20)).is_some(), "{t}");
        }
        assert!(a.windows[0].dos_args().contains(&"--own-tf".to_string()));
        assert!(builtin("nope", "A", screen, (10, 20)).is_none());
    }

    #[test]
    fn research2_has_compare_terminal_and_the_account() {
        let screen = Frame {
            x: 0,
            y: 0,
            w: 2000,
            h: 1000,
        };
        let mut d = builtin("research2", "R2", screen, (10, 20)).unwrap();
        let views: Vec<&str> = d.windows.iter().map(|w| w.view.as_str()).collect();
        assert_eq!(views, ["compare", "terminal", "main"]);
        assert!(!d.is_solo());
        assert!(!ensure_main(&mut d), "it has its main already");
        assert!(d.validate().is_ok());
    }

    #[test]
    fn a_solo_desk_keeps_its_one_window() {
        let screen = Frame {
            x: 0,
            y: 0,
            w: 2000,
            h: 1000,
        };
        let mut d = builtin("liquidations", "R2", screen, (10, 20)).unwrap();
        assert!(d.is_solo());
        assert_eq!(d.windows[0].view, "liq");
        assert!(d.windows[0].bare && d.windows[0].link == 1);
        assert!(!ensure_main(&mut d), "a solo desk gets no main window");
        assert_eq!(d.windows.len(), 1);
        assert!(d.validate().is_ok());
        // Two windows are not solo: main is added as before.
        d.windows.push(d.windows[0].clone());
        assert!(!d.is_solo());
        assert!(ensure_main(&mut d));
        assert_eq!(d.windows.iter().filter(|w| w.view == "main").count(), 1);
        // A lone main window is a desk with a main, not a solo one.
        let mut m = builtin("swing", "S", screen, (10, 20)).unwrap();
        m.windows.retain(|w| w.view == "main");
        assert!(!m.is_solo() && !ensure_main(&mut m));
        // The liquidations preset is solo too.
        let mut l = builtin("liquidations", "L", screen, (10, 20)).unwrap();
        assert!(l.is_solo() && l.windows[0].view == "liq");
        assert!(!ensure_main(&mut l) && l.validate().is_ok());
    }

    #[test]
    fn a_saved_desk_becomes_a_template_for_new_desks() {
        let dir = std::env::temp_dir().join(format!("dos-tpl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let screen = Frame {
            x: 0,
            y: 0,
            w: 2000,
            h: 1000,
        };
        let mut d = builtin("scalp", "Mine", screen, (10, 20)).unwrap();
        d.windows[0].symbol = Some("ETHUSDT".into());
        save(&dir, &d).unwrap();
        assert!(
            save_template(&dir, "Mine", "Scalp").is_err(),
            "built-in name"
        );
        assert!(save_template(&dir, "Missing", "T").is_err());
        save_template(&dir, "Mine", "MyScalp").unwrap();
        assert!(template_names(&dir).ends_with(&["MyScalp".to_string()]));
        let n = from_template(&dir, "MyScalp", "New", screen, (10, 20)).unwrap();
        assert_eq!(
            (n.name.as_str(), n.windows.clone()),
            ("New", d.windows.clone())
        );
        let b = from_template(&dir, "SCALP", "B", screen, (10, 20)).unwrap();
        assert_eq!(b.windows.len(), 5);
        assert!(from_template(&dir, "zzz", "C", screen, (10, 20))
            .unwrap_err()
            .contains("analysis"));
        // Templates are not desks.
        assert_eq!(list(&dir), vec!["Mine".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_window_snaps_to_its_neighbours_and_the_screen_edges() {
        let f = |x, y, w, h| Frame { x, y, w, h };
        let screen = f(0, 60, 3000, 1900);
        let book = f(1000, 60, 500, 800);
        // 10 px short of the book's left edge: right edge snaps onto it.
        assert_eq!(
            snap(f(12, 100, 980, 700), &[book], screen, 24),
            Some((20, 100))
        );
        // The book's left edge is nearer than the screen's; the top snaps too.
        assert_eq!(
            snap(f(20, 70, 970, 700), &[book], screen, 24),
            Some((30, 60))
        );
        // Nothing near: stays.
        assert_eq!(snap(f(400, 900, 300, 300), &[book], screen, 24), None);
        // Already touching: stays.
        assert_eq!(snap(f(0, 60, 1000, 800), &[book], screen, 24), None);
        // A window far below the book is not its neighbour across x.
        assert_eq!(snap(f(1490, 1500, 300, 200), &[book], screen, 24), None);
        // The nearest edge wins: under the book, its left edge 5 px away.
        assert_eq!(
            snap(f(1005, 870, 400, 300), &[book], screen, 24),
            Some((1000, 860))
        );
    }

    #[test]
    fn tiling_covers_the_screen_without_gaps() {
        let screen = Frame {
            x: 0,
            y: 60,
            w: 3000,
            h: 1900,
        };
        for n in 1..=MAX_WINDOWS {
            let t = tile(n, screen);
            assert_eq!(t.len(), n);
            let area: i64 = t.iter().map(|f| f.w as i64 * f.h as i64).sum();
            assert_eq!(area, 3000 * 1900, "{n} windows cover the screen exactly");
            assert!(t.iter().all(valid_frame));
        }
        let five = tile(5, screen);
        assert_eq!(
            five[0],
            Frame {
                x: 0,
                y: 60,
                w: 1000,
                h: 950
            }
        );
        assert_eq!(
            five[3],
            Frame {
                x: 0,
                y: 1010,
                w: 1500,
                h: 950
            }
        );
        assert!(tile(0, screen).is_empty());
        let m = move_request(10, 20, Some((300, 400)), 1);
        assert!(m.starts_with("\x1b]1337;SetUserVar=dos_move=") && m.ends_with('\x07'));
        let c = wezterm_config(&starter("D", 2000, 1000, (10, 20)), Path::new("/d"), &[]);
        assert!(c.contains("window:set_position") && c.contains(r#"var ~= "dos_move""#));
    }

    #[test]
    fn a_window_finds_itself_by_size_then_by_where_it_was() {
        let f = |x, y, w, h| Frame { x, y, w, h };
        let frames = [
            f(0, 60, 2296, 1152),
            f(2304, 60, 1526, 1152),
            f(0, 1216, 2296, 768),
        ];
        // A unique size decides alone.
        assert_eq!(
            pick_window(&frames, Some((2296, 768)), None),
            Some((0, 1216))
        );
        // Rounding of a few pixels still matches.
        assert_eq!(
            pick_window(&frames, Some((1520, 1150)), None),
            Some((2304, 60))
        );
        // Same size twice: the hint decides; without one it is ambiguous.
        let twins = [f(0, 0, 800, 600), f(900, 0, 800, 600), f(0, 700, 400, 300)];
        assert_eq!(
            pick_window(&twins, Some((800, 600)), Some((880, 20))),
            Some((900, 0))
        );
        assert_eq!(pick_window(&twins, Some((800, 600)), None), None);
        // No pixel size: one window is enough, several need the hint.
        assert_eq!(pick_window(&[f(5, 6, 7, 8)], None, None), Some((5, 6)));
        assert_eq!(pick_window(&twins, None, Some((10, 690))), Some((0, 700)));
        assert_eq!(pick_window(&[], Some((1, 1)), Some((0, 0))), None);
    }
}
