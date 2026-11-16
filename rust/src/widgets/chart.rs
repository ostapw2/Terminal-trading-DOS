//! Candlestick + volume chart, Unicode block-element rendering.
//!
//! Scoped exception to the project's pure-ASCII rule (see DESIGN.md §9.6).
//!
//! Only renders REAL market data from Yahoo / Binance / WS streams.
//! No synthetic data, no random walks, no fake ticks.

use crate::widgets::fmt::{BEAR, BULL};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    Frame,
};

use crate::data::liquidations::LiqMark;
use crate::gfx::{self, Canvas};
use crate::liqmap::LiqMap;
use crate::markets::Ohlc;
use crate::tokens::Palette;

/// Chart drawing style — cycle in Markets view with `c`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChartType {
    Candle,
    Line,
    Bar,
    DeltaCluster,
}

impl ChartType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Candle => "Candles",
            Self::Line => "Line",
            Self::Bar => "Bars (OHLC)",
            Self::DeltaCluster => "Delta",
        }
    }
    pub fn cycle(&self) -> ChartType {
        match self {
            Self::Candle => Self::Line,
            Self::Line => Self::Bar,
            Self::Bar => Self::DeltaCluster,
            Self::DeltaCluster => Self::Candle,
        }
    }
}

/// History range display filter — set via the History menu in Markets view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistoryRange {
    /// No date filtering — show all fetched data.
    All,
    /// Filter to today's UTC date only.
    Today,
    /// Filter to the last 3.5 days (today and 3 days back).
    Last3_5Days,
}

impl HistoryRange {
    pub fn name(&self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Today => "Today only",
            Self::Last3_5Days => "Last 3.5d",
        }
    }
}

/// Draw `data` as the chosen chart type (linear price scale).
pub fn render_chart(
    frame: &mut Frame,
    area: Rect,
    data: &[Ohlc],
    palette: &Palette,
    candle_w: u16,
    kind: ChartType,
) {
    match kind {
        ChartType::Candle => render_candles(frame, area, data, palette, candle_w),
        ChartType::Line => render_line(frame, area, data, palette, candle_w),
        ChartType::Bar => render_bars(frame, area, data, palette, candle_w),
        ChartType::DeltaCluster => render_delta_cluster(frame, area, data, palette, candle_w),
    }
}

/// Right-margin: keep the latest candle this many candle-widths from the
/// y-axis so price labels never sit on top of new candles.
pub const RIGHT_MARGIN_CW: u16 = 3;

/// Slice respecting `RIGHT_MARGIN_CW` reserved cells on the right and an
/// `offset` (candles scrolled into the past).
pub fn visible_slice_at(data: &[Ohlc], area_width: u16, candle_w: u16, offset: usize) -> &[Ohlc] {
    let cw = candle_w.max(1);
    let margin = RIGHT_MARGIN_CW.saturating_mul(cw);
    let usable = area_width.saturating_sub(margin);
    let n = (usable / cw) as usize;
    let n = n.min(data.len());
    if n == 0 {
        return &[];
    }
    let end = data.len().saturating_sub(offset);
    let start = end.saturating_sub(n);
    if start >= end {
        &[]
    } else {
        &data[start..end]
    }
}

/// Min/max of the visible window — feed into render_grid / render_yaxis.
/// `None` when there is nothing to scale (empty slice or only gap columns),
/// so callers draw "loading" instead of a bogus `0.00…1.00` axis.
pub fn visible_range(slice: &[Ohlc]) -> Option<(f64, f64)> {
    let mut real = slice.iter().filter(|o| !o.is_gap).peekable();
    real.peek()?;
    let (hi, lo) = real.fold((f64::NEG_INFINITY, f64::INFINITY), |(hi, lo), o| {
        (hi.max(o.high), lo.min(o.low))
    });
    Some((hi, lo))
}

/// Pick a "nice" axis step that yields ~target_lines evenly spaced grid lines.
fn nice_step(range: f64, target_lines: f64) -> f64 {
    if range <= 0.0 {
        return 1.0;
    }
    let raw = range / target_lines;
    let mag = raw.log10().floor();
    let base = 10f64.powf(mag);
    let mantissa = raw / base;
    let nice = if mantissa < 1.5 {
        1.0
    } else if mantissa < 3.5 {
        2.0
    } else if mantissa < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * base
}

/// Map a price to a vertical position `0 ..= steps - 1` (0 = `hi`).  Linear.
/// THE price→position mapping: grid, axis, candles, line and footprint all
/// derive from it (audit W2), in eighths of a cell ([`price_to_pixel`]) or in
/// real pixels (the pixel chart, `steps` = image height).
pub fn price_to_y(price: f64, hi: f64, lo: f64, steps: u32) -> u32 {
    if steps == 0 {
        return 0;
    }
    let h = steps as f64;
    let range = (hi - lo).max(1e-9);
    let t = (hi - price) / range;
    (t * (h - 1.0)).round().clamp(0.0, h - 1.0) as u32
}

/// The inverse of [`price_to_y`]: the price at step `y` of `steps` (what
/// the crosshair reads; the one mapping, run backwards).
pub fn y_to_price(y: u32, hi: f64, lo: f64, steps: u32) -> f64 {
    if steps <= 1 {
        return hi;
    }
    let t = y.min(steps - 1) as f64 / (steps - 1) as f64;
    hi - t * (hi - lo).max(1e-9)
}

/// The crosshair: a cell of the plot (column and row from its top-left),
/// and the exact pixel inside the plot when the terminal reports pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cross {
    pub col: u16,
    pub row: u16,
    pub px: Option<(u32, u32)>,
}

impl Cross {
    /// The mouse over `plot` (`mouse` in cells), with its pixel when known.
    pub fn at(plot: Rect, mouse: (u16, u16)) -> Option<Cross> {
        let (mx, my) = mouse;
        if mx < plot.x || my < plot.y || mx >= plot.x + plot.width || my >= plot.y + plot.height {
            return None;
        }
        Some(Cross {
            col: mx - plot.x,
            row: my - plot.y,
            px: gfx::pointer_px().and_then(|p| gfx::px_in(plot, p)),
        })
    }

    /// The price under it: at its pixel, or the middle of its row.
    pub fn price(&self, hi: f64, lo: f64, height: u16) -> f64 {
        if let (Some((_, y)), Some((_, ch))) = (self.px, gfx::cell_px()) {
            return y_to_price(y, hi, lo, height as u32 * ch);
        }
        y_to_price(self.row as u32 * 8 + 4, hi, lo, height as u32 * 8)
    }

    /// Index of the candle under it in the visible slice.
    pub fn candle(&self, candle_w: u16) -> usize {
        (self.col / candle_w.max(1)) as usize
    }
}

/// [`price_to_y`] in eighths of a cell (`0 ..= height*8 - 1`).
pub fn price_to_pixel(price: f64, hi: f64, lo: f64, height: u16) -> u32 {
    price_to_y(price, hi, lo, height as u32 * 8)
}

/// Terminal row (0..height) a price falls on.
pub fn price_to_row(price: f64, hi: f64, lo: f64, height: u16) -> u16 {
    (price_to_pixel(price, hi, lo, height) / 8) as u16
}

/// A labelled horizontal level (a plan's target, stop, a band): a dashed
/// line across the plot and its label on the left, its value on the axis.
#[derive(Clone, Debug, PartialEq)]
pub struct RefLine {
    pub value: f64,
    pub color: Color,
    pub label: String,
}

/// Labels of `lines` over a plot already drawn (the lines themselves are in
/// the picture when `pixels`, else dashed here over empty cells).  A level
/// outside the range gets its label on the edge row with an arrow.
pub fn render_ref_lines(
    frame: &mut Frame,
    plot: Rect,
    axis: Rect,
    (hi, lo): (f64, f64),
    lines: &[RefLine],
    pixels: bool,
    palette: &Palette,
) {
    if plot.width < 8 || plot.height < 2 || hi <= lo {
        return;
    }
    let end = plot.x + plot.width;
    for l in lines {
        let (row, arrow) = if l.value > hi {
            (0, "▲ ")
        } else if l.value < lo {
            (plot.height - 1, "▼ ")
        } else {
            (
                price_to_row(l.value, hi, lo, plot.height).min(plot.height - 1),
                "",
            )
        };
        let y = plot.y + row;
        let buf = frame.buffer_mut();
        if arrow.is_empty() && !pixels {
            for x in plot.x..end {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    let blank = matches!(cell.symbol(), " " | "─" | "┄" | "·");
                    if blank && cell.bg == palette.bg {
                        cell.set_char('┄');
                        cell.set_style(Style::default().fg(l.color).bg(palette.bg));
                    }
                }
            }
        }
        let text = format!(
            " {arrow}{} {} ",
            l.label,
            crate::widgets::fmt::fmt_value(l.value)
        );
        let style = Style::default()
            .fg(palette.black)
            .bg(l.color)
            .add_modifier(Modifier::BOLD);
        for (i, ch) in text.chars().enumerate() {
            let x = plot.x + 1 + i as u16;
            if x >= end {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
        if axis.width > 2 && arrow.is_empty() && y < axis.y + axis.height {
            let w = axis.width.saturating_sub(1) as usize;
            let tag = format!("{:<w$}", format!("{:.*}", axis_precision(hi, lo), l.value));
            for (j, ch) in tag.chars().enumerate() {
                let x = axis.x + 1 + j as u16;
                if x >= axis.x + axis.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }
}

/// What is drawn over (and behind) the candles besides the price itself.
#[derive(Clone, Copy, Default)]
pub struct Overlays<'a> {
    /// Liquidations that happened, placed on their candles.
    pub marks: &'a [LiqMark],
    /// Estimated liquidation levels (`crate::liqmap`), behind the candles.
    pub levels: Option<&'a LiqMap>,
    /// Horizontal lines over everything (trade lines, `chart_trade`):
    /// price and colour.
    pub hlines: &'a [(f64, Color)],
}

/// The plot in real pixels (when the terminal shows images): grid, candles /
/// line / bars and the live-price line, on the same price mapping and the same
/// candle slots as the character chart.  Returns `false` (nothing drawn) when
/// pixels are off this frame or the type has no pixel version (delta
/// cluster): the caller then draws characters.
pub fn render_plot_pixels(
    frame: &mut Frame,
    plot: Rect,
    data: &[Ohlc],
    (hi, lo): (f64, f64),
    candle_w: u16,
    kind: ChartType,
    last: Option<(f64, bool)>,
    over: &Overlays,
    cross: Option<Cross>,
    palette: &Palette,
) -> bool {
    if kind == ChartType::DeltaCluster || hi <= lo {
        return false;
    }
    let (Some((pw, ph)), Some((cell_w, cell_h))) = (gfx::plot_px(plot), gfx::cell_px()) else {
        return false;
    };
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, plot.width, cw, 0);
    // Everything the picture depends on: same key, same pixels, no redraw.
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            pw,
            ph,
            cell_w,
            cell_h,
            hi.to_bits(),
            lo.to_bits(),
            cw,
            kind as u8,
        )
            .hash(&mut h);
        last.map(|(p, up)| (p.to_bits(), up)).hash(&mut h);
        for m in over.marks {
            (m.col, m.price.to_bits(), m.usd.to_bits(), m.long).hash(&mut h);
        }
        if let Some(map) = over.levels {
            map.step.to_bits().hash(&mut h);
            for l in &map.levels {
                (
                    l.price.to_bits(),
                    l.long_usd.to_bits(),
                    l.short_usd.to_bits(),
                )
                    .hash(&mut h);
            }
        }
        for (p, c) in over.hlines {
            (p.to_bits(), format!("{c:?}")).hash(&mut h);
        }
        palette.name.hash(&mut h);
        for c in slice {
            (
                c.open.to_bits(),
                c.high.to_bits(),
                c.low.to_bits(),
                c.close.to_bits(),
                c.is_gap,
            )
                .hash(&mut h);
        }
        h.finish()
    };
    let cv = match gfx::cached(plot, key) {
        Some(cv) => cv,
        None => std::rc::Rc::new(draw_plot_canvas(
            pw,
            ph,
            (cell_w, cell_h),
            slice,
            (hi, lo),
            cw,
            kind,
            last,
            over,
            palette,
        )),
    };
    // With the mouse's pixel known, the crosshair (and a ruler while the
    // left button is held) is drawn into a copy of the picture, to the
    // pixel; the clean picture stays cached for the next frame.
    if let Some((mx, my)) = cross.and_then(|c| c.px) {
        let mut top = (*cv).clone();
        let pt = (cell_w as i64 / 7).max(1);
        let line = top.ink(palette.text);
        top.dashed_vline(mx as i64, pt, 3 * pt as usize, 2 * pt as usize, line);
        top.dashed_hline(my as i64, pt, 3 * pt as usize, 2 * pt as usize, line);
        let ruler = gfx::pressed_px()
            .and_then(|p| gfx::px_in(plot, p))
            .filter(|(ax, ay)| {
                (*ax as i64 - mx as i64).abs() + (*ay as i64 - my as i64).abs() > 3 * pt
            });
        let mut label = None;
        if let Some((ax, ay)) = ruler {
            let (p0, p1) = (
                y_to_price(ay, hi, lo, ph as u32),
                y_to_price(my, hi, lo, ph as u32),
            );
            let ink = top.ink(if p1 >= p0 { BULL } else { BEAR });
            top.line(ax as i64, ay as i64, mx as i64, my as i64, 2 * pt, ink);
            for (x, y) in [(ax as i64, ay as i64), (mx as i64, my as i64)] {
                top.fill(x - 2 * pt, y - 2 * pt, x + 2 * pt, y + 2 * pt, ink);
            }
            let bars = (mx as f64 - ax as f64) / (cw as f64 * cell_w as f64);
            label = Some((
                format!(
                    " {}{} ({:+.2}%) {:+.0} bars ",
                    if p1 >= p0 { "+" } else { "-" },
                    fmt_ruler((p1 - p0).abs()),
                    if p0 != 0.0 {
                        (p1 / p0 - 1.0) * 100.0
                    } else {
                        0.0
                    },
                    bars
                ),
                p1 >= p0,
            ));
        }
        gfx::keep(plot, key, cv);
        gfx::submit(frame.buffer_mut(), plot, top);
        // The ruler's numbers in cells beside the pointer (text never in pixels).
        if let Some((text, up)) = label {
            let c = cross.expect("px implies a cross");
            let w = text.chars().count() as u16;
            let x = (plot.x + c.col + 2).min((plot.x + plot.width).saturating_sub(w));
            let y = if c.row > 0 {
                plot.y + c.row - 1
            } else {
                plot.y + c.row + 1
            };
            let buf = frame.buffer_mut();
            for (i, ch) in text.chars().enumerate() {
                let cx = x + i as u16;
                if cx >= plot.x + plot.width || y >= plot.y + plot.height {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(ch);
                    cell.set_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(if up { BULL } else { BEAR })
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }
        return true;
    }
    // Empty for the crosshair: background (index 0) and grid.
    let empty_inks: Vec<u8> = std::iter::once(0).chain(cv.ink_of(palette.muted)).collect();
    // The crosshair goes on top as characters, only on cells where the
    // picture is empty (background or grid): the image itself never
    // changes with the mouse, so it is not encoded again on every move.
    let mut cross_cells: Vec<(u16, u16, char)> = Vec::new();
    if let Some(c) = cross.filter(|c| c.col < plot.width && c.row < plot.height) {
        let (cwp, chp) = (cell_w as usize, cell_h as usize);
        let empty = |cx: u16, cy: u16| {
            let (x0, y0) = (cx as usize * cwp, cy as usize * chp);
            cv.only(x0, y0, x0 + cwp, y0 + chp, &empty_inks)
        };
        let col = (c.col / cw) * cw + cw / 2;
        for cy in 0..plot.height {
            if col < plot.width && cy != c.row && empty(col, cy) {
                cross_cells.push((col, cy, '│'));
            }
        }
        for cx in 0..plot.width {
            if empty(cx, c.row) {
                cross_cells.push((cx, c.row, if cx == col { '┼' } else { '┄' }));
            }
        }
    }
    gfx::submit_keyed(frame.buffer_mut(), plot, key, cv);
    let buf = frame.buffer_mut();
    for (cx, cy, ch) in cross_cells {
        if let Some(cell) = buf.cell_mut((plot.x + cx, plot.y + cy)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(palette.text).bg(palette.bg));
        }
    }
    true
}

/// A price difference for the ruler: as many decimals as its size needs.
fn fmt_ruler(d: f64) -> String {
    match d {
        d if d >= 100.0 => format!("{d:.1}"),
        d if d >= 1.0 => format!("{d:.2}"),
        d if d >= 0.01 => format!("{d:.4}"),
        _ => format!("{d:.6}"),
    }
}

/// The plot's picture: grid, candles / bars / line, the live-price line.
#[allow(clippy::too_many_arguments)]
fn draw_plot_canvas(
    pw: usize,
    ph: usize,
    (cell_w, cell_h): (u32, u32),
    slice: &[Ohlc],
    (hi, lo): (f64, f64),
    cw: u16,
    kind: ChartType,
    last: Option<(f64, bool)>,
    over: &Overlays,
    palette: &Palette,
) -> Canvas {
    let mut cv = Canvas::new(pw, ph, palette.bg);
    let grid = cv.ink(palette.muted);
    let (bull, bear) = (cv.ink(BULL), cv.ink(BEAR));
    let y_of = |p: f64| price_to_y(p, hi, lo, ph as u32) as i64;
    // Hairlines scale with the cell so they stay one "dot" of the font.
    let hair = (cell_h as i64 / 16).max(1);
    // One screen point in image pixels (a cell is about 7 points wide):
    // 1 on a plain display, 2 on Retina.
    let pt = (cell_w as i64 / 7).max(1);

    // Grid of dots: dense at round steps (.....), sparse at half steps
    // (. . . .), so the levels read without hiding the bars.
    let step = nice_step(hi - lo, 5.0);
    let half = step / 2.0;
    let mut p = (lo / half).ceil() * half;
    let dot = hair.max(pt) as usize;
    while p <= hi + 1e-9 {
        let round = ((p / step).round() * step - p).abs() < 1e-6;
        let y = y_of(p);
        let off = if round { 2 * dot } else { 8 * dot };
        cv.dashed_hline(y, dot as i64, dot, off, grid);
        p += half;
    }

    // Estimated liquidation levels, behind the candles: a profile from the
    // right edge in rows a few pixels tall.
    if let Some(map) = over.levels {
        let band = (ph / 120).max(2);
        let rows = level_profile(map, (hi, lo), ph.div_ceil(band), |p| y_of(p) / band as i64);
        let max = rows.iter().map(|(l, s)| l.max(*s)).fold(0.0, f64::max);
        let (long_ink, short_ink) = (cv.ink(LEVEL_LONG), cv.ink(LEVEL_SHORT));
        let reach = pw as f64 * LEVEL_REACH;
        for (i, (l, sh)) in rows.iter().enumerate().filter(|_| max > 0.0) {
            let (usd, ink) = if l >= sh {
                (*l, long_ink)
            } else {
                (*sh, short_ink)
            };
            let w = (reach * usd / max).round() as i64;
            let y = (i * band) as i64;
            cv.fill(pw as i64 - w, y, pw as i64, y + band as i64 - 1, ink);
        }
    }

    let slot = (cw as u32 * cell_w) as i64;
    let gap = if slot >= 4 { (slot / 5).max(1) } else { 0 };
    // Wicks and bars 2 points thick (never wider than the body).
    let wick = (2 * pt).min((slot - gap).max(1));
    let mut prev: Option<(i64, i64)> = None;
    for (i, c) in slice.iter().enumerate() {
        if c.is_gap {
            prev = None;
            continue;
        }
        let x0 = i as i64 * slot;
        let mid = x0 + slot / 2;
        let ink = if c.close >= c.open { bull } else { bear };
        match kind {
            ChartType::Candle => {
                let (yh, yl) = (y_of(c.high), y_of(c.low));
                let (yo, yc) = (y_of(c.open), y_of(c.close));
                cv.fill(mid - wick / 2, yh, mid - wick / 2 + wick, yl + 1, ink);
                let (top, bot) = (yo.min(yc), yo.max(yc));
                // A doji still shows a body: at least one hairline tall.
                cv.fill(
                    x0 + gap / 2,
                    top,
                    x0 + slot - (gap - gap / 2),
                    (bot + 1).max(top + hair),
                    ink,
                );
            }
            ChartType::Bar => {
                let (yh, yl) = (y_of(c.high), y_of(c.low));
                let (yo, yc) = (y_of(c.open), y_of(c.close));
                cv.fill(mid - wick / 2, yh, mid - wick / 2 + wick, yl + 1, ink);
                cv.fill(x0 + gap / 2, yo, mid, yo + wick, ink);
                cv.fill(mid, yc, x0 + slot - gap / 2, yc + wick, ink);
            }
            ChartType::Line => {
                let at = (mid, y_of(c.close));
                if let Some((px, py)) = prev {
                    cv.line(px, py, at.0, at.1, (pt + 1).max(2), bull);
                } else {
                    cv.fill(at.0 - pt, at.1 - pt, at.0 + pt, at.1 + pt, bull);
                }
                prev = Some(at);
            }
            ChartType::DeltaCluster => {}
        }
    }
    // Liquidations: a ring per candle and side, its area the dollars; a
    // background halo keeps it readable over a candle of its own colour.
    let r_max = (cell_h as i64).max(slot / 2).max(3 * pt);
    let r_min = 2 * pt;
    let halo = cv.ink(palette.bg);
    for b in liq_bubbles(over.marks, y_of, r_max) {
        let r = r_min + ((r_max - r_min) as f64 * b.size).round() as i64;
        let (cx, cy) = (b.col as i64 * slot + slot / 2, b.y);
        let thick = (r / 3).max(pt);
        cv.ring(cx, cy, r + pt, thick + 2 * pt, halo);
        cv.ring(cx, cy, r, thick, if b.long { bear } else { bull });
    }
    for (price, color) in over.hlines.iter().filter(|(p, _)| (lo..=hi).contains(p)) {
        let ink = cv.ink(*color);
        let y = y_of(*price);
        cv.dashed_hline(
            y,
            (2 * pt).max(2),
            2 * cell_w as usize,
            cell_w as usize / 2,
            ink,
        );
    }
    if let Some((price, up)) = last.filter(|(p, _)| (lo..=hi).contains(p)) {
        let ink = if up { bull } else { bear };
        cv.dashed_hline(
            y_of(price),
            hair.max(2),
            cell_w as usize,
            cell_w as usize / 2,
            ink,
        );
    }
    cv
}

/// Liquidations merged for drawing: one bubble per candle, side and spot
/// on the price axis (`y_of` maps price to the drawing's rows or pixels;
/// marks closer than `merge` merge).  `size` is in `0..=1`, by the square
/// root of the dollars against the largest bubble (area = dollars).  Bubbles
/// under 2 % of the largest are dropped: dust would hide the candles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiqBubble {
    pub col: usize,
    pub y: i64,
    pub usd: f64,
    pub long: bool,
    pub size: f64,
}

pub fn liq_bubbles(marks: &[LiqMark], y_of: impl Fn(f64) -> i64, merge: i64) -> Vec<LiqBubble> {
    let mut out: Vec<LiqBubble> = Vec::new();
    for m in marks {
        let y = y_of(m.price);
        match out
            .iter_mut()
            .find(|b| b.col == m.col && b.long == m.long && (b.y - y).abs() < merge.max(1))
        {
            Some(b) => {
                // The bubble sits where most of its dollars were.
                let w = m.usd / (b.usd + m.usd).max(1e-12);
                b.y = (b.y as f64 + (y - b.y) as f64 * w).round() as i64;
                b.usd += m.usd;
            }
            None => out.push(LiqBubble {
                col: m.col,
                y,
                usd: m.usd,
                long: m.long,
                size: 0.0,
            }),
        }
    }
    let max = out.iter().map(|b| b.usd).fold(0.0, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    out.retain(|b| b.usd >= max * 0.02);
    for b in &mut out {
        b.size = (b.usd / max).sqrt();
    }
    // Small ones last, so they are not buried under a big one.
    out.sort_by(|a, b| b.usd.total_cmp(&a.usd));
    out
}

/// Liquidations in characters: `●` for the big ones, `•` for the rest, in
/// the colour of the side liquidated (a long: bear, a short: bull).  On a
/// candle body of the same colour the dot is drawn black.
pub fn render_liq_cells(
    frame: &mut Frame,
    plot: Rect,
    marks: &[LiqMark],
    (hi, lo): (f64, f64),
    candle_w: u16,
    palette: &Palette,
) {
    if plot.width == 0 || plot.height == 0 || hi <= lo {
        return;
    }
    let cw = candle_w.max(1);
    let rows = |p: f64| price_to_row(p, hi, lo, plot.height) as i64;
    let buf = frame.buffer_mut();
    for b in liq_bubbles(marks, rows, 1) {
        let x = b.col as u16 * cw + cw / 2;
        if x >= plot.width || b.y < 0 || b.y >= plot.height as i64 {
            continue;
        }
        let Some(cell) = buf.cell_mut((plot.x + x, plot.y + b.y as u16)) else {
            continue;
        };
        let ink = if b.long { BEAR } else { BULL };
        let bg = cell.bg;
        cell.set_char(if b.size >= 0.5 { '●' } else { '•' });
        cell.set_style(
            Style::default()
                .fg(if bg == ink { Color::Black } else { ink })
                .bg(if bg == Color::Reset { palette.bg } else { bg }),
        );
    }
}

/// Estimated levels: dark red (longs wait below), dark green (shorts above).
pub const LEVEL_LONG: Color = Color::Red;
pub const LEVEL_SHORT: Color = Color::Green;
/// The widest level bar, as a share of the plot's width.
pub const LEVEL_REACH: f64 = 0.25;

/// The levels in `(hi, lo)` summed per drawing row (`row_of` maps a price
/// to a row): `(longs, shorts)` in USD.  A bucket that spans several rows
/// spreads over them.  Levels outside the range are left out — the mapping
/// clamps, so they would pile up on the edge rows.
pub fn level_profile(
    map: &LiqMap,
    (hi, lo): (f64, f64),
    rows: usize,
    row_of: impl Fn(f64) -> i64,
) -> Vec<(f64, f64)> {
    let mut out = vec![(0.0, 0.0); rows];
    for l in map.levels.iter().filter(|l| (lo..=hi).contains(&l.price)) {
        let (a, b) = (
            row_of(l.price + map.step / 2.0),
            row_of(l.price - map.step / 2.0),
        );
        let (top, bot) = (a.min(b), a.max(b));
        if bot < 0 || top >= rows as i64 {
            continue;
        }
        let (top, bot) = (top.max(0), bot.min(rows as i64 - 1));
        let n = (bot - top + 1) as f64;
        for r in top..=bot {
            out[r as usize].0 += l.long_usd / n;
            out[r as usize].1 += l.short_usd / n;
        }
    }
    out
}

/// Estimated levels in characters: background bars from the right edge,
/// drawn before the candles so they stay on top.
pub fn render_level_cells(frame: &mut Frame, plot: Rect, map: &LiqMap, (hi, lo): (f64, f64)) {
    if plot.width == 0 || plot.height == 0 || hi <= lo {
        return;
    }
    let rows = level_profile(map, (hi, lo), plot.height as usize, |p| {
        price_to_row(p, hi, lo, plot.height) as i64
    });
    let max = rows.iter().map(|(l, s)| l.max(*s)).fold(0.0, f64::max);
    if max <= 0.0 {
        return;
    }
    let reach = plot.width as f64 * LEVEL_REACH;
    let buf = frame.buffer_mut();
    for (i, (l, s)) in rows.iter().enumerate() {
        let (usd, color) = if l >= s {
            (*l, LEVEL_LONG)
        } else {
            (*s, LEVEL_SHORT)
        };
        let w = ((reach * usd / max).round() as u16).min(plot.width);
        for dx in 0..w {
            let x = plot.x + plot.width - 1 - dx;
            if let Some(c) = buf.cell_mut((x, plot.y + i as u16)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(color));
            }
        }
    }
}

/// The liquidation lines in the plot's top-left corner, in cells over the
/// picture: `Liq 1h  longs $1.2M  shorts $300K` and, under it, the
/// estimated levels in view.
pub fn render_liq_legend(
    frame: &mut Frame,
    plot: Rect,
    hour: Option<(f64, f64)>,
    est: Option<(f64, f64)>,
    palette: &Palette,
) {
    use crate::data::liquidations::fmt_usd;
    if plot.width < 12 || plot.height < 4 {
        return;
    }
    let mut lines: Vec<Vec<(String, Color)>> = Vec::new();
    if let Some((l, s)) = hour {
        lines.push(vec![
            (
                format!(" {} ", crate::i18n::pick("Liq 1h", "Ліквід. 1г")),
                palette.muted,
            ),
            (
                format!(" {} {} ", crate::i18n::pick("longs", "лонги"), fmt_usd(l)),
                BEAR,
            ),
            (
                format!(" {} {} ", crate::i18n::pick("shorts", "шорти"), fmt_usd(s)),
                BULL,
            ),
        ]);
    }
    if let Some((l, s)) = est {
        lines.push(vec![
            (
                format!(
                    " {} ",
                    crate::i18n::pick("est. levels in view", "оцінка рівнів у вікні")
                ),
                palette.muted,
            ),
            (
                format!(" {} {} ", crate::i18n::pick("longs", "лонги"), fmt_usd(l)),
                BEAR,
            ),
            (
                format!(" {} {} ", crate::i18n::pick("shorts", "шорти"), fmt_usd(s)),
                BULL,
            ),
        ]);
    }
    let buf = frame.buffer_mut();
    let end = plot.x + plot.width;
    for (row, parts) in lines.into_iter().enumerate() {
        let y = plot.y + row as u16;
        let mut x = plot.x;
        for (text, fg) in parts {
            for ch in text.chars() {
                if x >= end {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(fg).bg(palette.bg));
                }
                x += 1;
            }
        }
    }
}

/// The crosshair in characters: its row and column get a lighter
/// background where the chart is empty (candles keep their colours).
pub fn render_cross_cells(
    frame: &mut Frame,
    plot: Rect,
    cross: Cross,
    candle_w: u16,
    palette: &Palette,
) {
    if cross.col >= plot.width || cross.row >= plot.height {
        return;
    }
    let cw = candle_w.max(1);
    let col = plot.x + (cross.col / cw) * cw + cw / 2;
    let row = plot.y + cross.row;
    let buf = frame.buffer_mut();
    let mut tint = |x: u16, y: u16| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            if cell.bg == palette.bg {
                cell.set_bg(palette.accent_dim);
            }
        }
    };
    for x in plot.x..plot.x + plot.width {
        tint(x, row);
    }
    if col < plot.x + plot.width {
        for y in plot.y..plot.y + plot.height {
            tint(col, y);
        }
    }
}

/// Crosshair labels: the price on the axis at its row, `time` on the x-axis
/// row under its candle.
pub fn render_cross_labels(
    frame: &mut Frame,
    plot: Rect,
    axis: Rect,
    xaxis: Rect,
    cross: Cross,
    candle_w: u16,
    (hi, lo): (f64, f64),
    time: &str,
    palette: &Palette,
) {
    if cross.col >= plot.width || cross.row >= plot.height || hi <= lo {
        return;
    }
    let style = Style::default()
        .fg(palette.bg)
        .bg(palette.text)
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    let mut put = |x: u16, y: u16, text: &str, right: u16| {
        for (j, ch) in text.chars().enumerate() {
            let cx = x.saturating_add(j as u16);
            if cx >= right {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
    };
    let y = plot.y + cross.row;
    if axis.width > 2 && y >= axis.y && y < axis.y + axis.height {
        let price = cross.price(hi, lo, plot.height);
        let w = axis.width.saturating_sub(1) as usize;
        let label = format!("{:<w$}", format!("{:.*}", axis_precision(hi, lo), price));
        put(axis.x + 1, y, &label, axis.x + axis.width);
    }
    if xaxis.height > 0 && xaxis.width > 0 && !time.is_empty() {
        let cw = candle_w.max(1);
        let mid = xaxis.x + (cross.col / cw) * cw + cw / 2;
        let w = time.chars().count() as u16 + 2;
        let right = xaxis.x + xaxis.width;
        let x = mid
            .saturating_sub(w / 2)
            .max(xaxis.x)
            .min(right.saturating_sub(w).max(xaxis.x));
        put(x, xaxis.y, &format!(" {time} "), right);
        // A date label cut by ours would leave a stray piece: blank it.
        let stray = |c: &ratatui::buffer::Cell| !matches!(c.symbol(), " " | "|" | "│");
        let mut cx = x.saturating_add(w);
        while cx < right && buf.cell((cx, xaxis.y)).is_some_and(stray) {
            if let Some(c) = buf.cell_mut((cx, xaxis.y)) {
                c.set_char(' ');
            }
            cx += 1;
        }
        let mut cx = x;
        while cx > xaxis.x && buf.cell((cx - 1, xaxis.y)).is_some_and(stray) {
            cx -= 1;
            if let Some(c) = buf.cell_mut((cx, xaxis.y)) {
                c.set_char(' ');
            }
        }
    }
}

/// Render horizontal price grid: solid line at every round step, dotted
/// at half-steps. Call BEFORE candles so they overdraw the grid.
pub fn render_grid(frame: &mut Frame, area: Rect, hi: f64, lo: f64, palette: &Palette) {
    if area.width == 0 || area.height == 0 || hi <= lo {
        return;
    }
    let buf = frame.buffer_mut();

    // Clear entire chart area to bg.
    for row in 0..area.height {
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.set_char(' ');
                cell.set_style(Style::default().fg(palette.text).bg(palette.bg));
            }
        }
    }

    let range = hi - lo;
    let step = nice_step(range, 5.0);
    let half = step / 2.0;

    // Half-step dotted lines.
    let mut p = (lo / half).ceil() * half;
    while p <= hi + 1e-9 {
        let on_round = ((p / step).round() * step - p).abs() < 1e-6;
        if !on_round {
            let row = price_to_row(p, hi, lo, area.height);
            let y = area.y + row;
            for col in 0..area.width {
                if col % 2 == 0 {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char('┄');
                        cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
                    }
                }
            }
        }
        p += half;
    }

    // Solid round-number lines.
    let mut p = (lo / step).ceil() * step;
    while p <= hi + 1e-9 {
        let row = price_to_row(p, hi, lo, area.height);
        let y = area.y + row;
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                cell.set_char('─');
                cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
            }
        }
        p += step;
    }
}

/// Render candlesticks: wick + solid body. Body uses bg=color for reliable
/// rendering on any terminal.
pub fn render_candles(
    frame: &mut Frame,
    area: Rect,
    data: &[Ohlc],
    palette: &Palette,
    candle_w: u16,
) {
    if data.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, area.width, cw, 0);
    let Some((max_high, min_low)) = visible_range(slice) else {
        return;
    };
    if max_high <= min_low {
        return;
    }

    let buf = frame.buffer_mut();

    for (i, c) in slice.iter().enumerate() {
        if c.is_gap {
            continue;
        }
        let x_left = area.x + (i as u16) * cw;
        let center = x_left + cw / 2;
        let up = c.close >= c.open;
        let color = if up { BULL } else { BEAR };

        let row_high = price_to_row(c.high, max_high, min_low, area.height);
        let row_low = price_to_row(c.low, max_high, min_low, area.height);
        let row_open = price_to_row(c.open, max_high, min_low, area.height);
        let row_close = price_to_row(c.close, max_high, min_low, area.height);
        let row_btop = row_open.min(row_close);
        let row_bbot = row_open.max(row_close);

        // Force wick visibility: if high/low exceed body but round to same row.
        let mut wick_top = row_high;
        let mut wick_bot = row_low;
        if c.high > c.open.max(c.close) + 1e-12 && wick_top >= row_btop {
            wick_top = row_btop.saturating_sub(1);
        }
        if c.low < c.open.min(c.close) - 1e-12 && wick_bot <= row_bbot {
            wick_bot = (row_bbot + 1).min(area.height.saturating_sub(1));
        }

        // 1. Wick at center column.
        for row in wick_top..=wick_bot {
            let y = area.y + row;
            if y >= area.y + area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((center, y)) {
                cell.set_char('│');
                cell.set_style(Style::default().fg(color).bg(palette.bg));
            }
        }

        // 2. Body — solid bg=color for reliable visibility.
        let solid = Style::default().bg(color).fg(palette.bg);
        for row in row_btop..=row_bbot {
            let y = area.y + row;
            if y >= area.y + area.height {
                continue;
            }
            for k in 0..cw {
                if let Some(cell) = buf.cell_mut((x_left + k, y)) {
                    cell.set_char(' ');
                    cell.set_style(solid);
                }
            }
        }
    }
}

/// Line chart — close-to-close, sub-cell vertical precision via `▔ ─ _`.
/// One point per candle, `candle_w` cells wide (so zoom works like the other
/// chart types); the line is broken at gap sentinels instead of dropping to 0.
pub fn render_line(frame: &mut Frame, area: Rect, data: &[Ohlc], palette: &Palette, candle_w: u16) {
    if data.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, area.width, cw, 0);
    let Some((max_high, min_low)) = visible_range(slice) else {
        return;
    };
    if max_high <= min_low {
        return;
    }

    let buf = frame.buffer_mut();
    let mut prev: Option<u32> = None; // pixel of the previous real candle
    for (i, c) in slice.iter().enumerate() {
        if c.is_gap {
            prev = None;
            continue;
        }
        let p = price_to_pixel(c.close, max_high, min_low, area.height);
        let p_prev = prev.unwrap_or(p);
        prev = Some(p);
        let row = (p / 8) as u16;
        let prev_row = (p_prev / 8) as u16;
        let frac = (p % 8) as u8;
        let color = if p <= p_prev { BULL } else { BEAR };
        let style = Style::default().fg(color).bg(palette.bg);
        let ch = if frac < 2 {
            '▔'
        } else if frac < 6 {
            '─'
        } else {
            '_'
        };
        let x_left = area.x + (i as u16) * cw;
        for k in 0..cw {
            let x = x_left + k;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y + row)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        // Vertical bridge for steep slopes, at the candle's first column.
        let lo_r = row.min(prev_row);
        let hi_r = row.max(prev_row);
        for r in (lo_r + 1)..hi_r {
            if r >= area.height {
                break;
            }
            if let Some(cell) = buf.cell_mut((x_left, area.y + r)) {
                if cell.symbol() == " " {
                    cell.set_char('│');
                    cell.set_style(style);
                }
            }
        }
    }
}

/// Aggregation of candles for zoom-out.  Gap sentinels carry no prices, so
/// they never take part in open/high/low/close/volume; a chunk made only of
/// gaps stays a gap (audit W1).
pub fn aggregate(data: &[Ohlc], factor: u16) -> Vec<Ohlc> {
    if factor <= 1 {
        return data.to_vec();
    }
    let f = factor as usize;
    let mut out = Vec::with_capacity(data.len() / f + 1);
    for chunk in data.chunks(f) {
        let mut real = chunk.iter().filter(|o| !o.is_gap);
        let Some(first) = real.next() else {
            out.push(Ohlc {
                is_gap: true,
                date: chunk[0].date.clone(),
                time_ms: chunk[0].time_ms,
                ..Ohlc::default()
            });
            continue;
        };
        let mut agg = first.clone();
        for o in real {
            agg.high = agg.high.max(o.high);
            agg.low = agg.low.min(o.low);
            agg.close = o.close;
            agg.volume += o.volume;
        }
        out.push(agg);
    }
    out
}

/// OHLC bar chart — vertical line from low to high with left/right ticks.
pub fn render_bars(frame: &mut Frame, area: Rect, data: &[Ohlc], palette: &Palette, candle_w: u16) {
    if data.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, area.width, cw, 0);
    let Some((max_high, min_low)) = visible_range(slice) else {
        return;
    };
    if max_high <= min_low {
        return;
    }

    let buf = frame.buffer_mut();
    for (i, c) in slice.iter().enumerate() {
        if c.is_gap {
            continue;
        }
        let x_left = area.x + (i as u16) * cw;
        let center = x_left + cw / 2;
        let up = c.close >= c.open;
        let color = if up { BULL } else { BEAR };

        let mut r_high = price_to_row(c.high, max_high, min_low, area.height);
        let mut r_low = price_to_row(c.low, max_high, min_low, area.height);
        let r_open = price_to_row(c.open, max_high, min_low, area.height);
        let r_close = price_to_row(c.close, max_high, min_low, area.height);
        let body_top = r_open.min(r_close);
        let body_bot = r_open.max(r_close);

        if c.high > c.open.max(c.close) + 1e-12 && r_high >= body_top {
            r_high = body_top.saturating_sub(1);
        }
        if c.low < c.open.min(c.close) - 1e-12 && r_low <= body_bot {
            r_low = (body_bot + 1).min(area.height.saturating_sub(1));
        }

        // Vertical line.
        for row in r_high..=r_low {
            let y = area.y + row;
            if y >= area.y + area.height {
                continue;
            }
            if center < area.x + area.width {
                if let Some(cell) = buf.cell_mut((center, y)) {
                    cell.set_char('│');
                    cell.set_style(Style::default().fg(color).bg(palette.bg));
                }
            }
        }

        // Left tick at open, right tick at close.
        if cw >= 2 {
            let y_open = area.y + r_open;
            if y_open < area.y + area.height && x_left < area.x + area.width {
                if let Some(cell) = buf.cell_mut((x_left, y_open)) {
                    cell.set_char('─');
                    cell.set_style(Style::default().fg(color).bg(palette.bg));
                }
            }
            let right = x_left + cw - 1;
            let y_close = area.y + r_close;
            if y_close < area.y + area.height && right < area.x + area.width {
                if let Some(cell) = buf.cell_mut((right, y_close)) {
                    cell.set_char('─');
                    cell.set_style(Style::default().fg(color).bg(palette.bg));
                }
            }
        }
    }
}

/// Delta-cluster — approximate volume-at-price histogram.  For each candle,
/// distribute volume across price rows weighted toward the close.
pub fn render_delta_cluster(
    frame: &mut Frame,
    area: Rect,
    data: &[Ohlc],
    palette: &Palette,
    candle_w: u16,
) {
    if data.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, area.width, cw, 0);
    let Some((max_high, min_low)) = visible_range(slice) else {
        return;
    };
    if max_high <= min_low {
        return;
    }

    let to_row = |price: f64| price_to_row(price, max_high, min_low, area.height);

    type RowVol = (u16, f64, bool);
    let mut candle_rows: Vec<Vec<RowVol>> = Vec::with_capacity(slice.len());
    let mut global_max: f64 = 1e-9;

    for c in slice {
        if c.is_gap {
            candle_rows.push(Vec::new());
            continue;
        }
        let r_high = to_row(c.high);
        let r_low = to_row(c.low);
        let n_rows = (r_low.saturating_sub(r_high) + 1) as f64;
        let r_close = to_row(c.close);
        let total = c.volume;
        let mut weights: Vec<f64> = Vec::new();
        let span = (n_rows - 1.0).max(1.0);
        for r in r_high..=r_low {
            let dist = (r as i32 - r_close as i32).abs() as f64;
            let w = 1.0 - (dist / span) * 0.7;
            weights.push(w.max(0.05));
        }
        let sum_w: f64 = weights.iter().sum();
        let is_buy = c.close >= c.open;
        let mut row_vec: Vec<RowVol> = Vec::with_capacity(weights.len());
        for (k, w) in weights.iter().enumerate() {
            let r = r_high + k as u16;
            let vol_at_row = total * (w / sum_w);
            if vol_at_row > global_max {
                global_max = vol_at_row;
            }
            row_vec.push((r, vol_at_row, is_buy));
        }
        candle_rows.push(row_vec);
    }

    let buf = frame.buffer_mut();
    for (i, rows) in candle_rows.iter().enumerate() {
        let x_left = area.x + (i as u16) * cw;
        for (row, vol_at_row, is_buy) in rows {
            let y = area.y + row;
            if y >= area.y + area.height {
                continue;
            }
            let intensity = (*vol_at_row / global_max).clamp(0.0, 1.0);
            let shade = if intensity > 0.85 {
                '█'
            } else if intensity > 0.6 {
                '▓'
            } else if intensity > 0.3 {
                '▒'
            } else if intensity > 0.08 {
                '░'
            } else {
                continue;
            };
            let color = if *is_buy { BULL } else { BEAR };
            for k in 0..cw {
                let x = x_left + k;
                if x >= area.x + area.width {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(shade);
                    cell.set_style(Style::default().fg(color).bg(palette.bg));
                }
            }
        }
    }
}

/// Render volume bars at the bottom of the chart.
pub fn render_volume(
    frame: &mut Frame,
    area: Rect,
    data: &[Ohlc],
    palette: &Palette,
    candle_w: u16,
) {
    if data.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let slice = visible_slice_at(data, area.width, cw, 0);
    let max_v = slice
        .iter()
        .filter(|o| !o.is_gap)
        .map(|o| o.volume)
        .fold(0.0, f64::max)
        .max(1e-12);
    if let (Some((pw, ph)), Some((cell_w, _))) = (gfx::plot_px(area), gfx::cell_px()) {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (pw, ph, cell_w, cw, palette.name).hash(&mut h);
            for c in slice {
                (c.volume.to_bits(), c.close >= c.open, c.is_gap).hash(&mut h);
            }
            h.finish()
        };
        if let Some(cv) = gfx::cached(area, key) {
            gfx::submit_keyed(frame.buffer_mut(), area, key, cv);
            return;
        }
        let mut cv = Canvas::new(pw, ph, palette.bg);
        let (bull, bear) = (cv.ink(BULL), cv.ink(BEAR));
        let slot = (cw as u32 * cell_w) as i64;
        let gap = (slot / 6).max(1).min(slot - 1).max(0);
        for (i, c) in slice.iter().enumerate() {
            if c.is_gap {
                continue;
            }
            let x0 = i as i64 * slot;
            let hpx = ((c.volume / max_v) * ph as f64).round() as i64;
            let ink = if c.close >= c.open { bull } else { bear };
            cv.fill(
                x0 + gap / 2,
                ph as i64 - hpx,
                x0 + slot - (gap - gap / 2),
                ph as i64,
                ink,
            );
        }
        gfx::submit_keyed(frame.buffer_mut(), area, key, std::rc::Rc::new(cv));
        return;
    }

    let buf = frame.buffer_mut();
    // Clear volume area.
    for row in 0..area.height {
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.set_char(' ');
                cell.set_style(Style::default().fg(palette.text).bg(palette.bg));
            }
        }
    }

    for (i, c) in slice.iter().enumerate() {
        if c.is_gap {
            continue;
        }
        let x_left = area.x + (i as u16) * cw;
        let bar_height = ((c.volume / max_v) * area.height as f64).round() as u16;
        let bar_height = bar_height.min(area.height);
        let color = if c.close >= c.open { BULL } else { BEAR };
        let solid = Style::default().bg(color).fg(palette.bg);
        for j in 0..bar_height {
            let y = area.y + area.height - 1 - j;
            for k in 0..cw {
                if let Some(cell) = buf.cell_mut((x_left + k, y)) {
                    cell.set_char(' ');
                    cell.set_style(solid);
                }
            }
        }
    }
}

/// Pick decimal precision based on price magnitude.
fn axis_precision(hi: f64, lo: f64) -> usize {
    let mag = hi.abs().max(lo.abs());
    if mag == 0.0 || !mag.is_finite() {
        return 2;
    }
    let log10 = mag.log10();
    if log10 < -3.0 {
        7
    } else if log10 < -2.0 {
        5
    } else if log10 < -1.0 {
        4
    } else if log10 < 0.0 {
        3
    } else if log10 < 2.0 {
        2
    } else if log10 < 4.0 {
        1
    } else {
        0
    }
}

/// The live price: a dotted line across the plot at the last close and its
/// value on the axis, green when the price is at or above the candle's open,
/// red below.  Candles stay on top (only blank / grid cells are drawn on), so
/// a moving price is visible even when the candle itself barely changes.
pub fn render_last_price(
    frame: &mut Frame,
    plot: Rect,
    axis: Rect,
    price: f64,
    up: bool,
    hi: f64,
    lo: f64,
    palette: &Palette,
    line: bool,
) {
    if plot.width == 0 || plot.height == 0 || hi <= lo || !(lo..=hi).contains(&price) {
        return;
    }
    let row = price_to_row(price, hi, lo, plot.height);
    if row >= plot.height {
        return;
    }
    let color = if up { BULL } else { BEAR };
    let buf = frame.buffer_mut();
    let y = plot.y + row;
    // The pixel chart draws the line itself (`line = false`): only the label.
    let line_cols = if line { plot.width } else { 0 };
    for x in plot.x..plot.x + line_cols {
        if let Some(cell) = buf.cell_mut((x, y)) {
            let blank = matches!(cell.symbol(), " " | "─" | "┄" | "·");
            if blank && cell.bg == palette.bg {
                cell.set_char('┄');
                cell.set_style(Style::default().fg(color).bg(palette.bg));
            }
        }
    }
    if axis.width > 2 && y < axis.y + axis.height {
        // Padded to the axis width: a shorter label must not leave the end
        // of the grid label under it ("83063" over " 83000" read "830630").
        let w = axis.width.saturating_sub(1) as usize;
        let label = format!("{:<w$}", format!("{:.*}", axis_precision(hi, lo), price));
        for (j, ch) in label.chars().enumerate() {
            let x = axis.x + 1 + j as u16;
            if x >= axis.x + axis.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(
                    Style::default()
                        .fg(palette.black)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

/// Right-hand price axis; labels sit on the same rows as the grid lines.
pub fn render_yaxis(frame: &mut Frame, area: Rect, hi: f64, lo: f64, palette: &Palette) {
    if area.width == 0 || area.height == 0 || hi <= lo {
        return;
    }
    let range = hi - lo;
    let step = nice_step(range, 5.0);

    let prec = axis_precision(hi, lo);
    let label_hi = format!("{:.*}", prec, hi);
    let label_lo = format!("{:.*}", prec, lo);
    let label_w = label_hi.len().max(label_lo.len()).max(4) as u16;
    let label_w = label_w.min(area.width.saturating_sub(2)).max(1);

    let buf = frame.buffer_mut();

    // Clear y-axis area.
    for row in 0..area.height {
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.set_char(' ');
                cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
            }
        }
    }

    // Vertical axis line.
    let axis_x = area.x;
    for row in 0..area.height {
        let y = area.y + row;
        if let Some(cell) = buf.cell_mut((axis_x, y)) {
            cell.set_char('│');
            cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
        }
    }

    // Tick labels.
    let mut p = (lo / step).ceil() * step;
    while p <= hi + 1e-9 {
        let row = price_to_row(p, hi, lo, area.height);
        if row >= area.height {
            p += step;
            continue;
        }
        let raw = format!("{:.*}", prec, p);
        let padded = if raw.len() as u16 >= label_w {
            raw
        } else {
            format!("{:>width$}", raw, width = label_w as usize)
        };
        for (j, ch) in padded.chars().enumerate() {
            let x = area.x + 2 + j as u16;
            if x < area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, area.y + row)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
                }
            }
        }
        // Tick cross.
        if let Some(cell) = buf.cell_mut((axis_x, area.y + row)) {
            cell.set_char('┼');
            cell.set_style(Style::default().fg(palette.accent).bg(palette.bg));
        }
        p += step;
    }
}

/// Render footprint (numeric delta cluster with per-price-level volumes).
/// `footprints` must be aligned 1:1 with the OHLC columns the axis was built
/// from, and `(hi, lo)` is that same OHLC range, so rows and axis labels
/// describe the same prices (audit W2).
pub fn render_footprint(
    frame: &mut Frame,
    area: Rect,
    footprints: &[crate::data::footprint::FootprintCandle],
    (max_high, min_low): (f64, f64),
    palette: &Palette,
    candle_w: u16,
) {
    if footprints.is_empty() || area.width == 0 || area.height == 0 || max_high <= min_low {
        return;
    }
    let cw = candle_w.max(1);
    let margin = RIGHT_MARGIN_CW.saturating_mul(cw);
    let usable = area.width.saturating_sub(margin);
    let n_visible = ((usable / cw) as usize).min(footprints.len());
    if n_visible == 0 {
        return;
    }
    let slice = &footprints[footprints.len() - n_visible..];

    let to_row = |price: f64| price_to_row(price, max_high, min_low, area.height);

    let numeric = cw >= 9;
    let buf = frame.buffer_mut();
    let bg = palette.bg;

    for (i, fc) in slice.iter().enumerate() {
        let x_left = area.x + (i as u16) * cw;
        let mut by_row: std::collections::BTreeMap<u16, (f64, f64)> =
            std::collections::BTreeMap::new();
        for lvl in &fc.levels {
            let r = to_row(lvl.price);
            let entry = by_row.entry(r).or_insert((0.0, 0.0));
            entry.0 += lvl.buy_volume;
            entry.1 += lvl.sell_volume;
        }
        let poc_row = to_row(fc.poc);
        let mut lowest_row: u16 = 0;

        if numeric {
            let mut highest_row: u16 = u16::MAX;
            for row in by_row.keys() {
                if *row < highest_row {
                    highest_row = *row;
                }
            }

            for (row, (buy, sell)) in &by_row {
                let (row, buy, sell) = (*row, *buy, *sell);
                lowest_row = lowest_row.max(row);
                let y = area.y + row;
                if y >= area.y + area.height {
                    continue;
                }
                if buy <= 0.0 && sell <= 0.0 {
                    continue;
                }
                let sell_str = fmt_vol4(sell);
                let buy_str = fmt_vol4(buy);
                let sep = if row == poc_row { '◆' } else { '│' };

                let total_row = buy + sell;
                let buy_ratio = if total_row > 0.0 {
                    buy / total_row
                } else {
                    0.5
                };
                let strong_buy = buy_ratio >= 0.75;
                let strong_sell = buy_ratio <= 0.25;
                let mild_buy = buy_ratio >= 0.6 && !strong_buy;
                let mild_sell = buy_ratio <= 0.4 && !strong_sell;

                let sell_style = if strong_sell {
                    Style::default()
                        .fg(palette.bg)
                        .bg(BEAR)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else if strong_buy {
                    Style::default().fg(palette.muted).bg(bg)
                } else if mild_sell {
                    Style::default()
                        .fg(BEAR)
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().fg(BEAR).bg(bg)
                };
                let buy_style = if strong_buy {
                    Style::default()
                        .fg(palette.bg)
                        .bg(BULL)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else if strong_sell {
                    Style::default().fg(palette.muted).bg(bg)
                } else if mild_buy {
                    Style::default()
                        .fg(BULL)
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().fg(BULL).bg(bg)
                };
                let sep_style = if row == poc_row {
                    Style::default()
                        .fg(palette.accent)
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().fg(palette.muted).bg(bg)
                };

                let s_chars: Vec<char> = sell_str.chars().collect();
                let b_chars: Vec<char> = buy_str.chars().collect();
                for k in 0..cw {
                    let x = x_left + k;
                    if x >= area.x + area.width {
                        continue;
                    }
                    let (ch, st) = if k < 4 {
                        (s_chars.get(k as usize).copied().unwrap_or(' '), sell_style)
                    } else if k == 4 {
                        (sep, sep_style)
                    } else if k < 9 {
                        (
                            b_chars.get((k - 5) as usize).copied().unwrap_or(' '),
                            buy_style,
                        )
                    } else {
                        (' ', Style::default().fg(palette.text).bg(bg))
                    };
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(st);
                    }
                }
            }

            // Header: total volume above topmost row.
            if highest_row != u16::MAX && highest_row > 0 {
                let header_y = area.y + highest_row - 1;
                if header_y < area.y + area.height {
                    let total = fc.total_buy + fc.total_sell;
                    let label = format!("Σ{}", fmt_vol4(total));
                    for k in 0..cw {
                        let x = x_left + k;
                        if x >= area.x + area.width {
                            continue;
                        }
                        let ch = label.chars().nth(k as usize).unwrap_or(' ');
                        if let Some(cell) = buf.cell_mut((x, header_y)) {
                            cell.set_char(ch);
                            cell.set_style(
                                Style::default()
                                    .fg(palette.muted)
                                    .bg(bg)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            );
                        }
                    }
                }
            }

            // Footer: per-candle delta one row below lowest level.
            let delta = fc.delta();
            let total = fc.total_buy + fc.total_sell;
            if total > 0.0 {
                let footer_y = area.y + lowest_row.saturating_add(1);
                if footer_y < area.y + area.height {
                    let sign = if delta >= 0.0 { '+' } else { '-' };
                    let label = format!("Δ{}{}", sign, fmt_vol4(delta.abs()));
                    let color = if delta >= 0.0 { BULL } else { BEAR };
                    for k in 0..cw {
                        let x = x_left + k;
                        if x >= area.x + area.width {
                            continue;
                        }
                        let ch = label.chars().nth(k as usize).unwrap_or(' ');
                        if let Some(cell) = buf.cell_mut((x, footer_y)) {
                            cell.set_char(ch);
                            cell.set_style(
                                Style::default()
                                    .fg(color)
                                    .bg(bg)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            );
                        }
                    }
                }
            }
        } else {
            // Narrow fallback: heatmap with shaded blocks.
            let mut global_max = 1e-9;
            for fc2 in slice {
                for lvl in &fc2.levels {
                    let v = lvl.total();
                    if v > global_max {
                        global_max = v;
                    }
                }
            }
            for (row, (buy, sell)) in by_row {
                let y = area.y + row;
                if y >= area.y + area.height {
                    continue;
                }
                let total = buy + sell;
                if total <= 0.0 {
                    continue;
                }
                let intensity = (total / global_max).clamp(0.0, 1.0);
                let mut shade = if intensity > 0.85 {
                    '█'
                } else if intensity > 0.6 {
                    '▓'
                } else if intensity > 0.3 {
                    '▒'
                } else if intensity > 0.05 {
                    '░'
                } else {
                    continue;
                };
                if row == poc_row {
                    shade = '◆';
                }
                let color = if buy >= sell { BULL } else { BEAR };
                for k in 0..cw {
                    let x = x_left + k;
                    if x >= area.x + area.width {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(shade);
                        cell.set_style(Style::default().fg(color).bg(bg));
                    }
                }
            }
        }
    }
}

fn fmt_vol4(v: f64) -> String {
    if v.is_nan() || v <= 0.0 {
        return "  - ".to_string();
    }
    if v >= 1e9 {
        format!("{:>3.1}G", v / 1e9)
    } else if v >= 1e6 {
        format!("{:>3.1}M", v / 1e6)
    } else if v >= 1e3 {
        if v >= 100e3 {
            format!("{:>3.0}K", v / 1e3)
        } else {
            format!("{:>3.1}K", v / 1e3)
        }
    } else if v >= 10.0 {
        format!("{:>3.0} ", v)
    } else if v >= 1.0 {
        format!("{:>3.1} ", v)
    } else {
        format!("{:>3.2} ", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crosshair_reads_prices_through_the_one_mapping() {
        let (hi, lo, h) = (110.0, 100.0, 20u16);
        for row in 0..h {
            let c = Cross {
                col: 0,
                row,
                px: None,
            };
            let p = c.price(hi, lo, h);
            assert!((lo..=hi).contains(&p));
            // Back through the forward mapping: the same row.
            assert_eq!(price_to_row(p, hi, lo, h), row, "row {row}");
        }
        assert_eq!(y_to_price(0, hi, lo, 160), hi);
        assert_eq!(y_to_price(159, hi, lo, 160), lo);
        assert_eq!(y_to_price(500, hi, lo, 160), lo, "clamped");
        let c7 = Cross {
            col: 7,
            row: 0,
            px: None,
        };
        assert_eq!(c7.candle(3), 2);
        assert_eq!(c7.candle(0), 7);
        // To the pixel: the price at the mouse's pixel, the same mapping back.
        gfx::set_cell_px(Some((10, 20)));
        for y in [0u32, 1, 57, 399] {
            let c = Cross {
                col: 0,
                row: (y / 20) as u16,
                px: Some((3, y)),
            };
            let p = c.price(hi, lo, h);
            assert_eq!(price_to_y(p, hi, lo, h as u32 * 20), y, "pixel {y}");
        }
        gfx::set_cell_px(None);
    }
}
