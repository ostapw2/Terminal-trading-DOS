//! Candlestick + volume chart, Unicode block-element rendering.
//!
//! Scoped exception to the project's pure-ASCII rule (see DESIGN.md §9.6).
//!
//! Only renders REAL market data from Yahoo / Binance / WS streams.
//! No synthetic data, no random walks, no fake ticks.

use crate::widgets::fmt::{BEAR, BULL};
use ratatui::{layout::Rect, style::Style, Frame};

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

/// Map a price to a vertical position in eighths of a cell
/// (`0 ..= height*8 - 1`, 0 = `hi`).  Linear.  THE price→position mapping:
/// grid, axis, candles, line and footprint all derive from it (audit W2).
pub fn price_to_pixel(price: f64, hi: f64, lo: f64, height: u16) -> u32 {
    if height == 0 {
        return 0;
    }
    let h_pixels = height as f64 * 8.0;
    let range = (hi - lo).max(1e-9);
    let t = (hi - price) / range;
    (t * (h_pixels - 1.0)).round().clamp(0.0, h_pixels - 1.0) as u32
}

/// Terminal row (0..height) a price falls on.
pub fn price_to_row(price: f64, hi: f64, lo: f64, height: u16) -> u16 {
    (price_to_pixel(price, hi, lo, height) / 8) as u16
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
