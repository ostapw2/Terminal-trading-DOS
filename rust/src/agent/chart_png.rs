//! A PNG candlestick chart for agents that can look at images: candles,
//! volume, SMA 20 / 50, support and resistance, the key levels of detected
//! patterns, markers on recent candlestick patterns.
//!
//! Dependency-free on purpose: a 5x7 pixel font, a palette-indexed canvas
//! and a small PNG writer (fixed-Huffman deflate with run / previous-row
//! matches, so a typical chart is tens of kilobytes, not half a megabyte).
//! Pure: candles in, bytes out.

use crate::agent::ta;
use crate::markets::{Ohlc, Timeframe};

pub const WIDTH: usize = 960;
pub const HEIGHT: usize = 540;
/// Fewest and most candles drawn.
pub const MIN_CANDLES: usize = 20;
pub const MAX_CANDLES: usize = 300;

// Palette indices.
const BG: u8 = 0;
const GRID: u8 = 1;
const TEXT: u8 = 2;
const BULL: u8 = 3;
const BEAR: u8 = 4;
const SMA20: u8 = 5;
const SMA50: u8 = 6;
const SUPPORT: u8 = 7;
const RESIST: u8 = 8;
const VOL_BULL: u8 = 9;
const VOL_BEAR: u8 = 10;
const WHITE: u8 = 11;
const DIM: u8 = 12;

const PALETTE: [[u8; 3]; 13] = [
    [0x0f, 0x14, 0x20], // BG
    [0x27, 0x2f, 0x42], // GRID
    [0xc9, 0xd1, 0xe3], // TEXT
    [0x2e, 0xcc, 0x71], // BULL
    [0xe7, 0x4c, 0x3c], // BEAR
    [0xf5, 0xc5, 0x42], // SMA20
    [0x4a, 0xa3, 0xff], // SMA50
    [0x1f, 0x8f, 0x55], // SUPPORT
    [0xb0, 0x3a, 0x30], // RESIST
    [0x1f, 0x6b, 0x42], // VOL_BULL
    [0x80, 0x2e, 0x27], // VOL_BEAR
    [0xff, 0xff, 0xff], // WHITE
    [0x6b, 0x75, 0x90], // DIM
];

// ─── 5x7 font ───────────────────────────────────────────────────────────

/// Rows of a glyph, 5 bits each, bit 4 = leftmost.  Lower case is drawn as
/// upper case; unknown characters as a blank.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ',' => [0, 0, 0, 0, 0x0C, 0x04, 0x08],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '+' => [0, 0x04, 0x04, 0x1F, 0x04, 0x04, 0],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '=' => [0, 0, 0x1F, 0, 0x1F, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0x1F],
        _ => [0; 7],
    }
}

// ─── Canvas ─────────────────────────────────────────────────────────────

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas {
            w,
            h,
            px: vec![BG; w * h],
        }
    }

    fn put(&mut self, x: i32, y: i32, c: u8) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u8) {
        for y in y0.max(0)..=y1.min(self.h as i32 - 1) {
            for x in x0.max(0)..=x1.min(self.w as i32 - 1) {
                self.px[y as usize * self.w + x as usize] = c;
            }
        }
    }

    fn hline(&mut self, x0: i32, x1: i32, y: i32, c: u8, dash: Option<(i32, i32)>) {
        for x in x0..=x1 {
            if let Some((on, off)) = dash {
                if (x - x0).rem_euclid(on + off) >= on {
                    continue;
                }
            }
            self.put(x, y, c);
        }
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u8) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            self.put(x, y, c);
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

    /// Text with its top-left at (x, y); `scale` pixels per font pixel.
    fn text(&mut self, x: i32, y: i32, s: &str, scale: i32, c: u8) {
        let mut cx = x;
        for ch in s.chars() {
            let g = glyph(ch);
            for (ry, row) in g.iter().enumerate() {
                for rx in 0..5 {
                    if row & (0x10 >> rx) != 0 {
                        self.rect(
                            cx + rx * scale,
                            y + ry as i32 * scale,
                            cx + rx * scale + scale - 1,
                            y + ry as i32 * scale + scale - 1,
                            c,
                        );
                    }
                }
            }
            cx += 6 * scale;
        }
    }

    fn text_width(s: &str, scale: i32) -> i32 {
        s.chars().count() as i32 * 6 * scale
    }

    /// A small triangle (up or down) centred on x.
    fn marker(&mut self, x: i32, y: i32, up: bool, c: u8) {
        for k in 0..4 {
            let (yy, half) = if up { (y + k, k) } else { (y - k, k) };
            self.hline(x - half, x + half, yy, c, None);
        }
    }
}

// ─── Formatting helpers ─────────────────────────────────────────────────

fn fmt_price(p: f64) -> String {
    let a = p.abs();
    let d = if a >= 1000.0 {
        1
    } else if a >= 10.0 {
        2
    } else if a >= 1.0 {
        3
    } else if a >= 0.01 {
        5
    } else {
        7
    };
    format!("{p:.d$}")
}

/// (year, month, day, hour, minute) in UTC.
fn civil(ms: i64) -> (i64, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's days -> civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32)
}

fn time_label(ms: i64, tf: Timeframe) -> String {
    let (_, mo, d, h, mi) = civil(ms);
    match tf {
        Timeframe::D1 => format!("{mo:02}-{d:02}"),
        Timeframe::H1 => format!("{mo:02}-{d:02} {h:02}H"),
        _ => format!("{d:02} {h:02}:{mi:02}"),
    }
}

// ─── The chart ──────────────────────────────────────────────────────────

/// Draw the last `limit` candles (20-300) of `candles` and return PNG bytes.
pub fn render(
    symbol: &str,
    tf: Timeframe,
    candles: &[Ohlc],
    limit: usize,
    now_ms: i64,
) -> Result<Vec<u8>, String> {
    let all: Vec<Ohlc> = candles.iter().filter(|k| !k.is_gap).cloned().collect();
    if all.len() < MIN_CANDLES {
        return Err(format!(
            "need at least {MIN_CANDLES} candles for {symbol}, got {}",
            all.len()
        ));
    }
    let limit = limit.clamp(MIN_CANDLES, MAX_CANDLES);
    let from = all.len().saturating_sub(limit);
    let c = &all[from..];
    let n = c.len();
    // Indicators and patterns see closed candles only; the one still being
    // built is drawn (the header says so) but not analysed.
    let forming = ta::is_forming(&c[n - 1], tf, now_ms);
    let closed = &c[..n - usize::from(forming)];
    let closes_all: Vec<f64> = all.iter().map(|k| k.close).collect();
    let sma20 = ta::sma(&closes_all, 20);
    let sma50 = ta::sma(&closes_all, 50);

    let (left, right, top) = (10i32, 90i32, 44i32);
    let (price_bottom, vol_top, vol_bottom) = (385i32, 395i32, 470i32);
    let plot_w = WIDTH as i32 - left - right;
    let step = plot_w as f64 / n as f64;
    let xc = |i: usize| left + (i as f64 * step + step / 2.0) as i32;

    // Price scale.
    let mut hi = c.iter().map(|k| k.high).fold(f64::NEG_INFINITY, f64::max);
    let mut lo = c.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
    let pad = (hi - lo).max(hi.abs() * 1e-6) * 0.04;
    hi += pad;
    lo -= pad;
    let py = |p: f64| top + ((hi - p) / (hi - lo) * (price_bottom - top) as f64) as i32;

    let mut cv = Canvas::new(WIDTH, HEIGHT);

    // Grid + price axis.
    for g in 0..=5 {
        let p = lo + (hi - lo) * g as f64 / 5.0;
        let y = py(p);
        cv.hline(left, left + plot_w, y, GRID, None);
        cv.text(left + plot_w + 6, y - 3, &fmt_price(p), 1, TEXT);
    }
    // Time axis.
    for g in 0..5 {
        let i = (n - 1) * g / 4;
        let x = xc(i);
        cv.rect(x, price_bottom, x, price_bottom + 3, GRID);
        let label = time_label(c[i].time_ms, tf);
        let w = Canvas::text_width(&label, 1);
        cv.text(
            (x - w / 2).clamp(left, left + plot_w - w),
            vol_bottom + 8,
            &label,
            1,
            DIM,
        );
    }

    // Support / resistance and pattern levels (behind the candles).
    let (highs, lows) = ta::pivots(closed, 3);
    let atr = ta::atr(closed, 14).last().copied().flatten();
    let tol = atr.map_or(closed[closed.len() - 1].close * 0.004, |a| a * 0.6);
    let price = c[n - 1].close;
    let lv = ta::levels(&highs, &lows, tol);
    let mut sr: Vec<(f64, usize)> = lv.into_iter().filter(|l| l.0 > lo && l.0 < hi).collect();
    sr.sort_by(|a, b| (a.0 - price).abs().total_cmp(&(b.0 - price).abs()));
    // Row of every label drawn so far: a label that would sit on another is
    // left out (its line stays), so the chart stays readable.
    let mut label_rows: Vec<i32> = Vec::new(); // left side: support / resistance
    let mut right_rows: Vec<i32> = Vec::new(); // right side: pattern levels
    let mut pattern_labels: Vec<(i32, i32, String)> = Vec::new(); // drawn over the candles
    for (p, touches) in sr.into_iter().take(5) {
        let col = if p < price { SUPPORT } else { RESIST };
        let y = py(p);
        cv.hline(left, left + plot_w, y, col, Some((6, 4)));
        let tag = format!(
            "{}{} {}",
            if p < price { "S" } else { "R" },
            touches,
            fmt_price(p)
        );
        if label_rows.iter().all(|r| (r - y).abs() >= 11) {
            cv.text(left + 4, y - 9, &tag, 1, col);
            label_rows.push(y);
        }
    }
    let chart_pats = ta::chart_patterns(closed);
    for p in chart_pats.iter().filter(|p| p.confidence != "low") {
        for (name, level) in &p.levels {
            if ![
                "neckline",
                "top",
                "bottom",
                "broken_level",
                "last_higher_low",
                "last_lower_high",
            ]
            .contains(name)
                || *level <= lo
                || *level >= hi
            {
                continue;
            }
            let y = py(*level);
            cv.hline(left, left + plot_w, y, WHITE, Some((2, 5)));
            if right_rows.iter().any(|r| (r - y).abs() < 11) {
                continue; // the line is drawn, the label would collide
            }
            right_rows.push(y);
            let tag = format!("{} {}", name.replace('_', " "), fmt_price(*level));
            let tx = left + plot_w - Canvas::text_width(&tag, 1) - 4;
            pattern_labels.push((tx, y, tag));
        }
    }

    // Volume.
    let vmax = c
        .iter()
        .map(|k| k.volume)
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    let body = ((step * 0.7) as i32).max(1);
    for (i, k) in c.iter().enumerate() {
        let x = xc(i);
        let h = ((k.volume / vmax) * (vol_bottom - vol_top) as f64) as i32;
        let col = if k.close >= k.open {
            VOL_BULL
        } else {
            VOL_BEAR
        };
        cv.rect(
            x - body / 2,
            vol_bottom - h,
            x - body / 2 + body - 1,
            vol_bottom,
            col,
        );
    }
    cv.text(left + 4, vol_top - 1, "VOLUME", 1, DIM);

    // Candles.
    for (i, k) in c.iter().enumerate() {
        let x = xc(i);
        let col = if k.close >= k.open { BULL } else { BEAR };
        cv.rect(x, py(k.high), x, py(k.low), col);
        let (a, b) = (py(k.open), py(k.close));
        cv.rect(
            x - body / 2,
            a.min(b),
            x - body / 2 + body - 1,
            a.max(b),
            col,
        );
    }

    // Moving averages.
    for (series, col) in [(&sma20, SMA20), (&sma50, SMA50)] {
        let mut prev: Option<(i32, i32)> = None;
        for i in 0..n {
            match series[from + i] {
                Some(v) if v > lo && v < hi => {
                    let pt = (xc(i), py(v));
                    if let Some(p) = prev {
                        cv.line(p.0, p.1, pt.0, pt.1, col);
                    }
                    prev = Some(pt);
                }
                _ => prev = None,
            }
        }
    }

    // Pattern-level labels last of the lines, on a dark plate, over everything.
    for (tx, y, tag) in &pattern_labels {
        let w = Canvas::text_width(tag, 1);
        cv.rect(tx - 2, y - 11, tx + w + 1, y - 1, BG);
        cv.text(*tx, y - 9, tag, 1, WHITE);
    }

    // Candlestick-pattern markers on the last bars.
    let cands = ta::candle_patterns(closed, 6);
    for p in &cands {
        let i = closed.len() - 1 - p.bars_ago;
        let x = xc(i);
        match p.bias {
            "bullish" => cv.marker(x, py(c[i].low) + 4, true, BULL),
            "bearish" => cv.marker(x, py(c[i].high) - 4, false, BEAR),
            _ => cv.marker(x, py(c[i].high) - 4, false, DIM),
        }
    }

    // Header and legend.
    let last = &c[n - 1];
    let change = (last.close - c[0].open) / c[0].open * 100.0;
    cv.text(left, 8, &format!("{symbol} {}", tf.name()), 2, WHITE);
    cv.text(
        left + Canvas::text_width(&format!("{symbol} {} ", tf.name()), 2) + 10,
        12,
        &format!(
            "{}O {} H {} L {} C {}  {:+.2}% OVER {} BARS",
            if forming { "LAST BAR FORMING  " } else { "" },
            fmt_price(last.open),
            fmt_price(last.high),
            fmt_price(last.low),
            fmt_price(last.close),
            change,
            n
        ),
        1,
        TEXT,
    );
    cv.text(left, 30, "SMA20", 1, SMA20);
    cv.text(left + 40, 30, "SMA50", 1, SMA50);
    cv.text(
        left + 80,
        30,
        "S = SUPPORT  R = RESISTANCE (TOUCHES)",
        1,
        DIM,
    );

    let mut notes: Vec<String> = chart_pats
        .iter()
        .filter(|p| p.confidence != "low")
        .map(|p| p.name.replace('_', " "))
        .collect();
    notes.extend(
        cands
            .iter()
            .filter(|p| p.bars_ago <= 1 && p.bias != "neutral")
            .map(|p| format!("{} ({})", p.name.replace('_', " "), p.bars_ago)),
    );
    if notes.is_empty() {
        notes.push("no clear pattern".into());
    }
    let line = format!("PATTERNS: {}", notes.join("; "));
    let max_chars = (WIDTH as i32 - 2 * left) / 6;
    let line: String = line.chars().take(max_chars as usize).collect();
    cv.text(left, vol_bottom + 24, &line, 1, TEXT);
    cv.text(
        left,
        HEIGHT as i32 - 14,
        "HEURISTIC DETECTION ON CLOSED CANDLES - A HINT, NOT A SIGNAL",
        1,
        DIM,
    );

    Ok(png(cv.w, cv.h, &PALETTE, &cv.px))
}

// ─── PNG ────────────────────────────────────────────────────────────────

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, t) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    let mut c = !0u32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Palette-indexed 8-bit PNG.
pub fn png(w: usize, h: usize, palette: &[[u8; 3]], pixels: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w + 1) * h);
    for row in pixels.chunks(w) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]); // 8 bit, palette, deflate, filter 0, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"PLTE",
        &palette.iter().flatten().copied().collect::<Vec<u8>>(),
    );
    chunk(&mut out, b"IDAT", &zlib(&raw, w + 1));
    chunk(&mut out, b"IEND", &[]);
    out
}

// Deflate, fixed Huffman, matches at distance 1 (runs) and `stride` (the
// pixel above, since a scanline is `stride` bytes).

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        // LSB first.
        self.acc |= value << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code goes out most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        let mut rev = 0;
        for i in 0..bits {
            if code & (1 << i) != 0 {
                rev |= 1 << (bits - 1 - i);
            }
        }
        self.put(rev, bits);
    }

    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push((self.acc & 0xFF) as u8);
            self.acc = 0;
            self.n = 0;
        }
    }

    fn literal(&mut self, b: u8) {
        match b {
            0..=143 => self.code(0x30 + b as u32, 8),
            _ => self.code(0x190 + (b as u32 - 144), 9),
        }
    }

    fn lit_len_symbol(&mut self, sym: u32) {
        match sym {
            256..=279 => self.code(sym - 256, 7),
            280..=287 => self.code(0xC0 + (sym - 280), 8),
            _ => unreachable!(),
        }
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn zlib(data: &[u8], stride: usize) -> Vec<u8> {
    let mut b = Bits {
        out: vec![0x78, 0x01],
        acc: 0,
        n: 0,
    };
    b.put(1, 1); // final block
    b.put(1, 2); // fixed Huffman
    let mut i = 0;
    while i < data.len() {
        let mut best = (0usize, 0usize); // (length, distance)
        for d in [1usize, stride] {
            if d == 0 || d > i {
                continue;
            }
            let mut l = 0;
            while l < 258 && i + l < data.len() && data[i + l] == data[i + l - d] {
                l += 1;
            }
            if l > best.0 {
                best = (l, d);
            }
        }
        if best.0 >= 3 {
            let (len, dist) = best;
            let li = LEN_BASE
                .iter()
                .rposition(|&x| x as usize <= len)
                .unwrap_or(0);
            b.lit_len_symbol(257 + li as u32);
            b.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = DIST_BASE
                .iter()
                .rposition(|&x| x as usize <= dist)
                .unwrap_or(0);
            b.code(di as u32, 5);
            b.put(
                (dist - DIST_BASE[di] as usize) as u32,
                DIST_EXTRA[di] as u32,
            );
            i += len;
        } else {
            b.literal(data[i]);
            i += 1;
        }
    }
    b.lit_len_symbol(256);
    b.flush();
    let mut out = b.out;
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Standard base64 (RFC 4648) with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just enough inflate for blocks this encoder writes (one fixed block).
    fn inflate(z: &[u8]) -> Vec<u8> {
        let data = &z[2..z.len() - 4];
        let (mut pos, mut out) = (0usize, Vec::<u8>::new());
        let bit = |pos: &mut usize| {
            let v = (data[*pos / 8] >> (*pos % 8)) & 1;
            *pos += 1;
            v as u32
        };
        let bits = |pos: &mut usize, n: u32| (0..n).fold(0, |a, i| a | bit(pos) << i);
        assert_eq!(bits(&mut pos, 1), 1, "final");
        assert_eq!(bits(&mut pos, 2), 1, "fixed huffman");
        loop {
            // Read a lit/len code MSB first, bit by bit.
            let mut code = 0u32;
            let mut len = 0;
            let sym = loop {
                code = code << 1 | bit(&mut pos);
                len += 1;
                match (len, code) {
                    (7, 0..=0x17) => break 256 + code,
                    (8, 0x30..=0xBF) => break code - 0x30,
                    (8, 0xC0..=0xC7) => break 280 + code - 0xC0,
                    (9, 0x190..=0x1FF) => break 144 + code - 0x190,
                    _ if len > 9 => panic!("bad code"),
                    _ => {}
                }
            };
            match sym {
                0..=255 => out.push(sym as u8),
                256 => return out,
                _ => {
                    let li = (sym - 257) as usize;
                    let length =
                        LEN_BASE[li] as usize + bits(&mut pos, LEN_EXTRA[li] as u32) as usize;
                    let mut d = 0u32;
                    for _ in 0..5 {
                        d = d << 1 | bit(&mut pos);
                    }
                    let dist = DIST_BASE[d as usize] as usize
                        + bits(&mut pos, DIST_EXTRA[d as usize] as u32) as usize;
                    for _ in 0..length {
                        out.push(out[out.len() - dist]);
                    }
                }
            }
        }
    }

    #[test]
    fn deflate_round_trips_and_compresses_flat_images() {
        // Random-ish bytes, long runs, and repeated rows of width 17.
        let mut data: Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        data.extend(std::iter::repeat_n(7u8, 700));
        let row: Vec<u8> = (0..17u8).map(|x| x * 9).collect();
        for _ in 0..40 {
            data.extend_from_slice(&row);
        }
        let z = zlib(&data, 17);
        assert_eq!(inflate(&z), data);
        assert_eq!(&z[z.len() - 4..], adler32(&data).to_be_bytes());
        let flat = vec![3u8; 100_000];
        assert!(
            zlib(&flat, 100).len() < 1_000,
            "a flat image shrinks to almost nothing"
        );
        assert_eq!(inflate(&zlib(&flat, 100)), flat);
        assert_eq!(inflate(&zlib(&[], 5)), Vec::<u8>::new());
        assert_eq!(inflate(&zlib(&[1, 2], 5)), vec![1, 2]);
    }

    #[test]
    fn checksums_and_base64_match_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn civil_time_is_utc() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0));
        assert_eq!(
            civil(1_704_067_200_000 + 3_600_000 * 5 + 60_000 * 7),
            (2024, 1, 1, 5, 7)
        );
        assert_eq!(civil(1_709_164_800_000), (2024, 2, 29, 0, 0));
    }

    fn candles(n: usize) -> Vec<Ohlc> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64 * 0.4 + (i as f64 * 0.6).sin() * 3.0;
                let o = 100.0 + (i as f64 - 1.0) * 0.4 + ((i as f64 - 1.0) * 0.6).sin() * 3.0;
                Ohlc {
                    open: o,
                    high: o.max(c) + 0.6,
                    low: o.min(c) - 0.6,
                    close: c,
                    volume: 100.0 + (i % 7) as f64 * 20.0,
                    date: String::new(),
                    time_ms: 1_704_067_200_000 + i as i64 * 3_600_000,
                    is_gap: false,
                }
            })
            .collect()
    }

    #[test]
    fn a_chart_is_a_valid_small_png_with_candles_in_it() {
        let png = render("TESTUSDT", Timeframe::H1, &candles(120), 100, i64::MAX / 2).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes(png[16..20].try_into().unwrap()) as usize,
            WIDTH
        );
        assert_eq!(
            u32::from_be_bytes(png[20..24].try_into().unwrap()) as usize,
            HEIGHT
        );
        assert!(png.len() < 120_000, "{} bytes", png.len());
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // Decode the pixels: both candle colours and the text colour are present.
        let idat_at = png.windows(4).position(|w| w == b"IDAT").unwrap();
        let len = u32::from_be_bytes(png[idat_at - 4..idat_at].try_into().unwrap()) as usize;
        let raw = inflate(&png[idat_at + 4..idat_at + 4 + len]);
        assert_eq!(raw.len(), (WIDTH + 1) * HEIGHT);
        for colour in [BULL, BEAR, SMA20, SMA50, TEXT, GRID, VOL_BULL] {
            assert!(raw.contains(&colour), "colour {colour} missing");
        }
        assert!(render("X", Timeframe::H1, &candles(10), 100, i64::MAX / 2).is_err());
        // Extreme limits are clamped, a short history still renders.
        assert!(render("X", Timeframe::D1, &candles(25), 1000, i64::MAX / 2).is_ok());
        // With the last candle still forming it renders too (and says so).
        let c = candles(60);
        assert!(render("X", Timeframe::H1, &c, 60, c[59].time_ms + 1_000).is_ok());
    }
}
