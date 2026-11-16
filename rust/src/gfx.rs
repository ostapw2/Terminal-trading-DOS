//! Pixel graphics inside the terminal: charts drawn as real images, text
//! (axes, labels, menus, tables) stays in the character grid.
//!
//! How a frame works:
//! 1. A widget that can draw in pixels asks [`plot_px`] for the pixel size of
//!    its area, draws a [`Canvas`] and calls [`submit`].  The cells under it
//!    get a sentinel symbol.
//! 2. At the end of the frame [`finish_frame`] turns every sentinel cell into
//!    a *skipped* cell (ratatui will not paint it) and remembers the cells
//!    something else painted over the image (a tooltip, a divider).
//! 3. After ratatui has flushed, [`Placer::flush`] sends each image whose
//!    content or overlay changed (iTerm2 inline images, or the kitty graphics
//!    protocol), then paints the overlay cells again on top of it.
//!
//! Off unless the terminal can show images (WezTerm, iTerm2, kitty) and the
//! app switched it on ([`set_enabled`]); `DOS_GFX=off|iterm|kitty` overrides
//! the detection.  When off, or while a modal is open ([`set_suspended`]),
//! widgets draw their character version as before.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::agent::chart_png::{base64, png};

/// Placed in cells that an image will cover; never shown.
pub const SENTINEL: &str = "\u{10FFFD}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    /// `ESC ] 1337 ; File=...` (iTerm2; WezTerm).
    Iterm,
    /// `ESC _ G ...` (kitty).
    Kitty,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// 0 = unknown/none, 1 = iTerm2, 2 = kitty.
static PROTO: AtomicU8 = AtomicU8::new(0);

/// The protocol this terminal understands, from the environment.
pub fn detect() -> Option<Proto> {
    match std::env::var("DOS_GFX").as_deref() {
        Ok("off") | Ok("0") => return None,
        Ok("iterm") => return Some(Proto::Iterm),
        Ok("kitty") => return Some(Proto::Kitty),
        _ => {}
    }
    let tp = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if tp == "WezTerm" || tp == "iTerm.app" {
        return Some(Proto::Iterm);
    }
    // kitty is only used when asked for (`DOS_GFX=kitty`): untested there.
    None
}

/// Switch pixel graphics on (when the terminal supports them) or off.
/// Returns whether they are on now.
pub fn set_enabled(on: bool) -> bool {
    let proto = if on { detect() } else { None };
    PROTO.store(
        match proto {
            Some(Proto::Iterm) => 1,
            Some(Proto::Kitty) => 2,
            None => 0,
        },
        Ordering::Relaxed,
    );
    ENABLED.store(proto.is_some(), Ordering::Relaxed);
    proto.is_some()
}

pub fn proto() -> Option<Proto> {
    match PROTO.load(Ordering::Relaxed) {
        1 => Some(Proto::Iterm),
        2 => Some(Proto::Kitty),
        _ => None,
    }
}

/// Draw in characters this frame (a modal is open: images would hide it).
pub fn set_suspended(s: bool) {
    SUSPENDED.store(s, Ordering::Relaxed);
}

thread_local! {
    static CELL_PX: RefCell<Option<(u32, u32)>> = const { RefCell::new(None) };
    static JOBS: RefCell<Vec<Job>> = const { RefCell::new(Vec::new()) };
    /// The mouse in window pixels, when the terminal reports pixels
    /// (SGR-Pixels, DECSET 1016), and where the left button went down.
    static POINTER: RefCell<Option<(u32, u32)>> = const { RefCell::new(None) };
    static PRESSED: RefCell<Option<(u32, u32)>> = const { RefCell::new(None) };
}

/// The mouse in window pixels (`None`: the terminal reports cells only).
pub fn set_pointer_px(p: Option<(u32, u32)>) {
    POINTER.with(|c| *c.borrow_mut() = p);
}

pub fn pointer_px() -> Option<(u32, u32)> {
    POINTER.with(|c| *c.borrow())
}

/// Where the left button is held down since (window pixels), for a ruler.
pub fn set_pressed_px(p: Option<(u32, u32)>) {
    PRESSED.with(|c| *c.borrow_mut() = p);
}

pub fn pressed_px() -> Option<(u32, u32)> {
    PRESSED.with(|c| *c.borrow())
}

/// A window-pixel point inside `area`, in `area`'s own pixels.
pub fn px_in(area: Rect, p: (u32, u32)) -> Option<(u32, u32)> {
    let (cw, ch) = cell_px()?;
    let (x0, y0) = (area.x as u32 * cw, area.y as u32 * ch);
    let (w, h) = (area.width as u32 * cw, area.height as u32 * ch);
    (p.0 >= x0 && p.1 >= y0 && p.0 < x0 + w && p.1 < y0 + h).then(|| (p.0 - x0, p.1 - y0))
}

/// Measure the cell size for this frame (pixels per cell).  Call once per
/// frame before drawing; `None` turns pixel drawing off for the frame.
pub fn begin_frame() {
    let px = if ENABLED.load(Ordering::Relaxed) && !SUSPENDED.load(Ordering::Relaxed) {
        crossterm::terminal::window_size()
            .ok()
            .filter(|w| w.width > 0 && w.height > 0 && w.columns > 0 && w.rows > 0)
            .map(|w| {
                (
                    w.width as u32 / w.columns as u32,
                    w.height as u32 / w.rows as u32,
                )
            })
            .filter(|(cw, ch)| *cw >= 2 && *ch >= 2)
    } else {
        None
    };
    CELL_PX.with(|c| *c.borrow_mut() = px);
    JOBS.with(|j| j.borrow_mut().clear());
}

/// Set the cell size by hand (tests, tools without a terminal): pixel
/// drawing is on for this thread until `None` or the next [`begin_frame`].
pub fn set_cell_px(px: Option<(u32, u32)>) {
    CELL_PX.with(|c| *c.borrow_mut() = px);
}

/// Pixels per cell this frame, when pixel drawing is on.
pub fn cell_px() -> Option<(u32, u32)> {
    CELL_PX.with(|c| *c.borrow())
}

/// Pixel size of `area` when it can be drawn in pixels this frame.
pub fn plot_px(area: Rect) -> Option<(usize, usize)> {
    let (cw, ch) = cell_px()?;
    if area.width == 0 || area.height == 0 {
        return None;
    }
    Some((
        area.width as usize * cw as usize,
        area.height as usize * ch as usize,
    ))
}

/// An image for `rect`: its cells get the sentinel; [`finish_frame`] and
/// [`Placer::flush`] do the rest.
pub fn submit(buf: &mut Buffer, rect: Rect, canvas: Canvas) {
    let hash = canvas.hash();
    submit_hashed(buf, rect, std::rc::Rc::new(canvas), hash);
}

fn submit_hashed(buf: &mut Buffer, rect: Rect, canvas: std::rc::Rc<Canvas>, hash: u64) {
    let rect = rect.intersection(buf.area);
    if rect.is_empty() {
        return;
    }
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.reset();
                c.set_symbol(SENTINEL);
            }
        }
    }
    JOBS.with(|j| {
        let mut j = j.borrow_mut();
        // A later image over the same cells replaces the earlier one.
        j.retain(|o| o.rect.intersection(rect).is_empty());
        j.push(Job {
            rect,
            canvas,
            hash,
            overlay: Vec::new(),
        })
    });
}

/// One painted cell over an image: position, symbol, colours, modifier.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayCell {
    pub x: u16,
    pub y: u16,
    pub symbol: String,
    pub fg: Color,
    pub bg: Color,
    pub modifier: Modifier,
}

pub struct Job {
    pub rect: Rect,
    pub canvas: std::rc::Rc<Canvas>,
    /// [`Canvas::hash`], computed once per picture.
    pub hash: u64,
    pub overlay: Vec<OverlayCell>,
}

/// A remembered picture: the key it was drawn from, the picture, its hash.
type KeyedPicture = (u64, std::rc::Rc<Canvas>, u64);

thread_local! {
    /// Last picture per rect with the key of what it was drawn from
    /// ([`cached`] / [`submit_keyed`]).
    static KEYED: RefCell<HashMap<RectKey, KeyedPicture>> =
        RefCell::new(HashMap::new());
}

/// The picture drawn last time for `rect` from inputs with this `key`, if
/// any: drawing it again would give the same pixels, so it is not redrawn
/// (nor hashed again).  The caller keys everything the picture depends on.
pub fn cached(rect: Rect, key: u64) -> Option<std::rc::Rc<Canvas>> {
    let k = (rect.x, rect.y, rect.width, rect.height);
    KEYED.with(|m| {
        m.borrow()
            .get(&k)
            .filter(|(kk, _, _)| *kk == key)
            .map(|(_, c, _)| c.clone())
    })
}

/// Remember a picture drawn from inputs `key` for [`cached`] without
/// showing it (what is shown is a copy with something drawn on top).
pub fn keep(rect: Rect, key: u64, canvas: std::rc::Rc<Canvas>) {
    let k = (rect.x, rect.y, rect.width, rect.height);
    KEYED.with(|m| {
        let mut m = m.borrow_mut();
        if !m
            .get(&k)
            .is_some_and(|(kk, c, _)| *kk == key && std::rc::Rc::ptr_eq(c, &canvas))
        {
            let h = canvas.hash();
            m.insert(k, (key, canvas, h));
        }
    });
}

/// [`submit`] a picture drawn from inputs `key`, remembered for [`cached`].
pub fn submit_keyed(buf: &mut Buffer, rect: Rect, key: u64, canvas: std::rc::Rc<Canvas>) {
    let k = (rect.x, rect.y, rect.width, rect.height);
    let hash = KEYED.with(|m| {
        let mut m = m.borrow_mut();
        match m.get(&k) {
            Some((kk, c, h)) if *kk == key && std::rc::Rc::ptr_eq(c, &canvas) => *h,
            _ => {
                let h = canvas.hash();
                m.insert(k, (key, canvas.clone(), h));
                h
            }
        }
    });
    submit_hashed(buf, rect, canvas, hash);
}

/// End of the frame (still inside `terminal.draw`): sentinel cells become
/// skipped, cells painted over an image are remembered for [`Placer::flush`].
/// Returns the frame's images.
pub fn finish_frame(buf: &mut Buffer) -> Vec<Job> {
    let mut jobs = JOBS.with(|j| std::mem::take(&mut *j.borrow_mut()));
    for job in &mut jobs {
        let r = job.rect;
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                let Some(c) = buf.cell_mut((x, y)) else {
                    continue;
                };
                if c.symbol() == SENTINEL {
                    c.set_symbol(" ");
                    c.skip = true;
                } else {
                    job.overlay.push(OverlayCell {
                        x,
                        y,
                        symbol: c.symbol().to_string(),
                        fg: c.fg,
                        bg: c.bg,
                        modifier: c.modifier,
                    });
                }
            }
        }
    }
    jobs
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
}

/// An image's place: x, y, width, height in cells.
type RectKey = (u16, u16, u16, u16);

/// Sends the frame's images to the terminal, only those that changed.
#[derive(Default)]
pub struct Placer {
    /// What is on screen now: rect -> hash of image + overlay.
    shown: HashMap<(u16, u16, u16, u16), u64>,
    /// The last encoding per rect: (canvas hash, PNG length, base64).  When
    /// only the cells over an image change (a crosshair, the data window),
    /// the image is sent again without encoding it again.
    encoded: HashMap<RectKey, (u64, usize, String)>,
    next_id: u32,
}

impl Placer {
    /// The screen was cleared (resize, full redraw): send everything again.
    pub fn reset(&mut self) {
        self.shown.clear();
    }

    /// PNG length and base64 of `canvas` for `key`, encoded only when the
    /// picture changed since the last time.
    fn encode(&mut self, key: RectKey, canvas: &Canvas, hash: u64) -> (usize, String) {
        if let Some((h, len, b64)) = self.encoded.get(&key) {
            if *h == hash {
                return (*len, b64.clone());
            }
        }
        let data = canvas.png();
        let b64 = base64(&data);
        self.encoded.insert(key, (hash, data.len(), b64.clone()));
        (data.len(), b64)
    }

    /// Returns whether an image shown last frame is gone: its cells were
    /// blanked to the terminal's background when it was placed, and only
    /// the ones ratatui changed got repainted since, so the caller repaints
    /// the whole screen.
    pub fn flush(&mut self, jobs: Vec<Job>, out: &mut impl Write) -> io::Result<bool> {
        let Some(proto) = proto() else {
            let gone = !self.shown.is_empty();
            self.shown.clear();
            return Ok(gone);
        };
        let mut now: HashMap<(u16, u16, u16, u16), u64> = HashMap::new();
        for job in jobs {
            let r = job.rect;
            let key = (r.x, r.y, r.width, r.height);
            let picture = job.hash;
            let mut h = picture;
            for c in &job.overlay {
                fnv(
                    &mut h,
                    &[c.x as u8, (c.x >> 8) as u8, c.y as u8, (c.y >> 8) as u8],
                );
                fnv(&mut h, c.symbol.as_bytes());
                fnv(
                    &mut h,
                    format!("{:?}{:?}{:?}", c.fg, c.bg, c.modifier).as_bytes(),
                );
            }
            now.insert(key, h);
            if self.shown.get(&key) == Some(&h) {
                continue;
            }
            let (len, b64) = self.encode(key, &job.canvas, picture);
            // Blank the rect first: the terminal draws an image UNDER the
            // text of its cells, so whatever was painted over the last
            // image (a crosshair, a tooltip) would otherwise stay.
            write!(out, "\x1b7\x1b[0m")?;
            let blank = " ".repeat(r.width as usize);
            for y in r.y..r.y + r.height {
                write!(out, "\x1b[{};{}H{blank}", y + 1, r.x + 1)?;
            }
            // Go to the rect, image, restore cursor.
            write!(out, "\x1b[{};{}H", r.y + 1, r.x + 1)?;
            match proto {
                Proto::Iterm => write!(
                    out,
                    "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:{}\x07",
                    len,
                    r.width,
                    r.height,
                    b64
                )?,
                Proto::Kitty => {
                    self.next_id = self.next_id.wrapping_add(1).max(1);
                    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
                    for (i, c) in chunks.iter().enumerate() {
                        let more = u8::from(i + 1 < chunks.len());
                        if i == 0 {
                            write!(
                                out,
                                "\x1b_Ga=T,f=100,q=2,C=1,c={},r={},m={more};",
                                r.width, r.height
                            )?;
                        } else {
                            write!(out, "\x1b_Gm={more};")?;
                        }
                        out.write_all(c)?;
                        out.write_all(b"\x1b\\")?;
                    }
                }
            }
            write!(out, "\x1b8")?;
            // Whatever was painted over the image goes on top of it again.
            for c in &job.overlay {
                use crossterm::style::{
                    Attribute, SetAttribute, SetBackgroundColor, SetForegroundColor,
                };
                crossterm::queue!(
                    out,
                    crossterm::cursor::MoveTo(c.x, c.y),
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(c.fg.into()),
                    SetBackgroundColor(c.bg.into()),
                )?;
                if c.modifier.contains(Modifier::BOLD) {
                    crossterm::queue!(out, SetAttribute(Attribute::Bold))?;
                }
                crossterm::queue!(
                    out,
                    crossterm::style::Print(&c.symbol),
                    SetAttribute(Attribute::Reset)
                )?;
            }
            if !job.overlay.is_empty() {
                write!(out, "\x1b8")?;
            }
        }
        let gone = self.shown.keys().any(|k| !now.contains_key(k));
        self.encoded.retain(|k, _| now.contains_key(k));
        self.shown = now;
        out.flush()?;
        Ok(gone)
    }
}

// ─── Canvas ─────────────────────────────────────────────────────────────

/// RGB of a palette colour (named colours as the VGA / xterm defaults the
/// design uses: `LightGreen` = 55FF55, `LightRed` = FF5555).
pub fn rgb(c: Color) -> [u8; 3] {
    match c {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Black => [0, 0, 0],
        Color::Red => [170, 0, 0],
        Color::Green => [0, 170, 0],
        Color::Yellow => [170, 85, 0],
        Color::Blue => [0, 0, 170],
        Color::Magenta => [170, 0, 170],
        Color::Cyan => [0, 170, 170],
        Color::Gray => [170, 170, 170],
        Color::DarkGray => [85, 85, 85],
        Color::LightRed => [255, 85, 85],
        Color::LightGreen => [85, 255, 85],
        Color::LightYellow => [255, 255, 85],
        Color::LightBlue => [85, 85, 255],
        Color::LightMagenta => [255, 85, 255],
        Color::LightCyan => [85, 255, 255],
        Color::White => [255, 255, 255],
        _ => [255, 255, 255],
    }
}

/// A palette-indexed pixel canvas (at most 256 colours), drawn crisp: no
/// anti-aliasing, so it keeps the hard-edged look of the character UI.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    px: Vec<u8>,
    pal: Vec<[u8; 3]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: Color) -> Self {
        Self {
            w,
            h,
            px: vec![0; w * h],
            pal: vec![rgb(bg)],
        }
    }

    /// Palette index of `c` (added on first use; the last slot is reused
    /// when 256 colours are exceeded).
    pub fn ink(&mut self, c: Color) -> u8 {
        let v = rgb(c);
        if let Some(i) = self.pal.iter().position(|p| *p == v) {
            return i as u8;
        }
        if self.pal.len() < 256 {
            self.pal.push(v);
        }
        (self.pal.len() - 1) as u8
    }

    /// Palette index of `c` if the picture uses it (no new entry).
    pub fn ink_of(&self, c: Color) -> Option<u8> {
        let v = rgb(c);
        self.pal.iter().position(|p| *p == v).map(|i| i as u8)
    }

    /// Fill `[x0, x1) x [y0, y1)`, clipped.
    pub fn fill(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, c: u8) {
        let (x0, x1) = (x0.max(0) as usize, (x1.max(0) as usize).min(self.w));
        let (y0, y1) = (y0.max(0) as usize, (y1.max(0) as usize).min(self.h));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            self.px[y * self.w + x0..y * self.w + x1].fill(c);
        }
    }

    pub fn put(&mut self, x: i64, y: i64, c: u8) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    /// Horizontal dashes (`on` pixels drawn, `off` skipped), `thick` rows.
    pub fn dashed_hline(&mut self, y: i64, thick: i64, on: usize, off: usize, c: u8) {
        let step = (on + off).max(1);
        let mut x = 0usize;
        while x < self.w {
            self.fill(x as i64, y, (x + on) as i64, y + thick, c);
            x += step;
        }
    }

    /// Vertical dashes at `x` (`on` pixels drawn, `off` skipped), `thick` wide.
    pub fn dashed_vline(&mut self, x: i64, thick: i64, on: usize, off: usize, c: u8) {
        let step = (on + off).max(1);
        let mut y = 0usize;
        while y < self.h {
            self.fill(x, y as i64, x + thick, (y + on) as i64, c);
            y += step;
        }
    }

    /// A line `thick` pixels wide (square brush), Bresenham.
    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, thick: i64, c: u8) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        let half = thick / 2;
        loop {
            self.fill(x - half, y - half, x - half + thick, y - half + thick, c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// A ring centred on `(cx, cy)`: outer radius `r`, `thick` pixels wide
    /// (a filled disc when `thick >= r`).
    pub fn ring(&mut self, cx: i64, cy: i64, r: i64, thick: i64, c: u8) {
        let inner = (r - thick).max(0);
        let half = |rad: i64, dy: i64| {
            let d = rad * rad - dy * dy;
            (d >= 0).then(|| (d as f64).sqrt().round() as i64)
        };
        for dy in -r..=r {
            let Some(o) = half(r, dy) else { continue };
            match half(inner, dy).filter(|_| inner > 0) {
                Some(i) if i < o => {
                    self.fill(cx - o, cy + dy, cx - i, cy + dy + 1, c);
                    self.fill(cx + i + 1, cy + dy, cx + o + 1, cy + dy + 1, c);
                }
                Some(_) => {}
                None => self.fill(cx - o, cy + dy, cx + o + 1, cy + dy + 1, c),
            }
        }
    }

    /// Does `[x0, x1) x [y0, y1)` (clipped) hold only colours in `inks`?
    /// (Is a cell's block empty chart, so text can sit there without
    /// hiding anything?)
    pub fn only(&self, x0: usize, y0: usize, x1: usize, y1: usize, inks: &[u8]) -> bool {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        (y0..y1).all(|y| {
            self.px[y * self.w + x0.min(x1)..y * self.w + x1]
                .iter()
                .all(|p| inks.contains(p))
        })
    }

    /// Identity of the picture (pixels and colours).
    pub fn hash(&self) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        fnv(&mut h, &(self.w as u64).to_le_bytes());
        fnv(&mut h, &(self.h as u64).to_le_bytes());
        for p in &self.pal {
            fnv(&mut h, p);
        }
        // Hash 8 bytes at a time: the canvas is millions of pixels.
        for chunk in self.px.chunks(8) {
            let mut v = [0u8; 8];
            v[..chunk.len()].copy_from_slice(chunk);
            h ^= u64::from_le_bytes(v);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    pub fn png(&self) -> Vec<u8> {
        png(self.w, self.h, &self.pal, &self.px)
    }

    #[cfg(test)]
    pub fn at(&self, x: usize, y: usize) -> [u8; 3] {
        self.pal[self.px[y * self.w + x] as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_reads_as_empty_only_in_the_given_colours() {
        let mut cv = Canvas::new(20, 10, Color::Black);
        let grid = cv.ink(Color::Gray);
        let red = cv.ink(Color::LightRed);
        cv.fill(0, 5, 20, 6, grid);
        cv.fill(15, 0, 17, 3, red);
        assert!(cv.only(0, 0, 10, 10, &[0, grid]));
        assert!(!cv.only(0, 0, 10, 10, &[0]));
        assert!(!cv.only(10, 0, 20, 10, &[0, grid]));
        assert!(cv.only(10, 4, 30, 10, &[0, grid]), "clipped to the canvas");
    }

    #[test]
    fn an_image_is_encoded_once_while_only_the_text_over_it_changes() {
        let mut p = Placer::default();
        let cv = Canvas::new(4, 4, Color::Black);
        let key = (0, 0, 2, 2);
        let (len, b64) = p.encode(key, &cv, cv.hash());
        // A forged cache entry proves the second call does not re-encode.
        p.encoded.insert(key, (cv.hash(), len, "cached".into()));
        assert_eq!(p.encode(key, &cv, cv.hash()).1, "cached");
        let mut other = cv.clone();
        other.put(0, 0, 1);
        assert_ne!(p.encode(key, &other, other.hash()).1, "cached");
        assert!(!b64.is_empty());
    }

    #[test]
    fn canvas_draws_clipped_and_hashes_its_content() {
        let mut c = Canvas::new(20, 10, Color::Rgb(0, 0, 170));
        let g = c.ink(Color::LightGreen);
        assert_eq!(c.ink(Color::LightGreen), g, "same colour, same slot");
        c.fill(-5, -5, 3, 3, g);
        assert_eq!(c.at(0, 0), [85, 255, 85]);
        assert_eq!(c.at(3, 3), [0, 0, 170]);
        c.fill(15, 8, 99, 99, g);
        assert_eq!(c.at(19, 9), [85, 255, 85]);
        let h1 = c.hash();
        c.put(10, 5, g);
        assert_ne!(c.hash(), h1);
        c.line(0, 9, 19, 0, 1, g);
        assert_eq!(c.at(19, 0), [85, 255, 85]);
        let png = c.png();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn images_skip_their_cells_and_keep_what_was_painted_over_them() {
        set_enabled(false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        let r = Rect::new(2, 1, 5, 2);
        submit(&mut buf, r, Canvas::new(10, 4, Color::Black));
        buf.cell_mut((3, 1)).unwrap().set_symbol("X");
        let jobs = finish_frame(&mut buf);
        assert_eq!(jobs.len(), 1);
        assert!(buf.cell((2, 1)).unwrap().skip, "image cell is skipped");
        assert!(!buf.cell((3, 1)).unwrap().skip, "the painted cell is not");
        assert_eq!(jobs[0].overlay.len(), 1);
        assert_eq!(jobs[0].overlay[0].symbol, "X");
        assert!(!buf.cell((0, 0)).unwrap().skip, "outside untouched");
        // Off: nothing is measured, nothing is drawn in pixels.
        begin_frame();
        assert_eq!(plot_px(r), None);
    }
}
