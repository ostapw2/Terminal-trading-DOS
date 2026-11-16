//! Line chart of a pair spread: deviation in percent around a zero line,
//! with ±1σ and ±2σ guides and a stats header (see [`crate::pairs`]).
//!
//! The y axis is symmetric around 0 so "A rich" (green, above) and "A cheap"
//! (red, below) read at a glance.  The series is resampled to the plot width
//! (last value per column); steep moves are joined with a vertical stroke.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::gfx::{self, Canvas};
use crate::pairs::Stats;
use crate::tokens::Palette;
use crate::widgets::fmt::{render_clipped, truncate, BEAR, BULL, SPARKLINE_BARS};

const AXIS_W: u16 = 9;

fn put(frame: &mut Frame, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

/// Resample `v` to `w` columns, keeping the last value of each bucket.
fn resample(v: &[f64], w: usize) -> Vec<f64> {
    if v.is_empty() || w == 0 {
        return Vec::new();
    }
    if v.len() <= w {
        return v.to_vec();
    }
    (0..w)
        .map(|i| {
            let end = ((i + 1) * v.len() / w).clamp(1, v.len());
            v[end - 1]
        })
        .collect()
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    dev: &[f64],
    stats: &Stats,
    palette: &Palette,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if inner.height < 3 || inner.width < AXIS_W + 4 || dev.is_empty() {
        return;
    }

    // Header: the numbers a trader reads first.
    let z_color = if stats.z.abs() >= 2.0 {
        Color::Yellow
    } else {
        palette.text
    };
    let header = format!(
        " dev {:+.2}%   z {:+.2}   sigma {:.2}%   corr {:.2}   beta {:.2}   range {:+.1}..{:+.1}%   n {}",
        stats.last, stats.z, stats.sigma, stats.corr, stats.beta, stats.min, stats.max, stats.n
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, inner.width as usize).to_string()).style(
            Style::default()
                .fg(z_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect { height: 1, ..inner },
    );

    let plot = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width - AXIS_W,
        height: inner.height - 1,
    };
    let h_pixels = plot.height as f64 * 8.0;
    let span = dev
        .iter()
        .fold(0.0_f64, |m, v| m.max(v.abs()))
        .max(2.0 * stats.sigma)
        .max(0.05)
        * 1.1;
    let to_pixel = |v: f64| -> u32 {
        ((span - v) / (2.0 * span) * (h_pixels - 1.0))
            .round()
            .clamp(0.0, h_pixels - 1.0) as u32
    };
    let row_of = |v: f64| (to_pixel(v) / 8) as u16;
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let zero_row = row_of(0.0);

    // In pixels: the series at two pixels per point instead of one per
    // column, guides as hairlines.
    if let (Some((pw, ph)), Some((cw, ch))) = (gfx::plot_px(plot), gfx::cell_px()) {
        let mut cv = Canvas::new(pw, ph, palette.bg);
        let (text, grid) = (cv.ink(palette.text), cv.ink(palette.muted));
        let (bull, bear) = (cv.ink(BULL), cv.ink(BEAR));
        let y_of = |v: f64| ((span - v) / (2.0 * span) * (ph as f64 - 1.0)).round() as i64;
        let hair = (ch as i64 / 16).max(1);
        cv.fill(0, y_of(0.0), pw as i64, y_of(0.0) + hair, text);
        if stats.sigma > 0.0 {
            for k in [1.0, 2.0, -1.0, -2.0] {
                cv.dashed_hline(
                    y_of(k * stats.sigma),
                    hair,
                    cw as usize / 2,
                    cw as usize / 2,
                    grid,
                );
            }
        }
        // The same width as the character line (one column per point,
        // right-aligned), but up to one point per two pixels inside it.
        let used = dev.len().min(plot.width as usize) * cw as usize;
        let pts = resample(dev, dev.len().min(used / 2).max(1));
        let step = used as f64 / pts.len() as f64;
        let x0 = (pw - used) as f64;
        let x_of = |i: usize| (x0 + step * (i as f64 + 0.5)).round() as i64;
        let thick = (cw as i64 / 7).max(2);
        for i in 1..pts.len() {
            let ink = if pts[i] >= 0.0 { bull } else { bear };
            cv.line(
                x_of(i - 1),
                y_of(pts[i - 1]),
                x_of(i),
                y_of(pts[i]),
                thick,
                ink,
            );
        }
        gfx::submit(frame.buffer_mut(), plot, cv);
    }
    let pixels = gfx::plot_px(plot).is_some();

    // Guides: zero line, then ±1σ and ±2σ.
    let guide_cols = if pixels { 0 } else { plot.width };
    for x in plot.x..plot.x + guide_cols {
        put(
            frame,
            x,
            plot.y + zero_row,
            '─',
            Style::default().fg(palette.text).bg(palette.bg),
        );
    }
    for k in [1.0, 2.0] {
        if stats.sigma <= 0.0 {
            break;
        }
        for sign in [1.0, -1.0] {
            let r = row_of(sign * k * stats.sigma);
            if r == zero_row {
                continue;
            }
            for x in (plot.x..plot.x + guide_cols).step_by(2) {
                put(frame, x, plot.y + r, '┄', muted);
            }
        }
    }

    // The line, right-aligned so the newest value sits at the right edge.
    let cols = resample(dev, plot.width as usize);
    let line_cols = if pixels { 0 } else { cols.len() };
    let x0 = plot.x + plot.width - cols.len() as u16;
    let mut prev_row: Option<u16> = None;
    for (i, v) in cols.iter().take(line_cols).enumerate() {
        let x = x0 + i as u16;
        let p = to_pixel(*v);
        let (row, frac) = ((p / 8) as u16, (p % 8) as usize);
        let color = if *v >= 0.0 { BULL } else { BEAR };
        let style = Style::default().fg(color).bg(palette.bg);
        if let Some(pr) = prev_row {
            let (lo, hi) = (pr.min(row), pr.max(row));
            for r in lo + 1..hi {
                put(frame, x, plot.y + r, '│', style);
            }
        }
        put(
            frame,
            x,
            plot.y + row,
            SPARKLINE_BARS[7 - frac.min(7)],
            style,
        );
        prev_row = Some(row);
    }

    // Right axis: top / zero / bottom, plus the last value at its height.
    let axis_x = plot.x + plot.width + 1;
    let label = |frame: &mut Frame, row: u16, text: String, style: Style| {
        for (j, ch) in text.chars().take(AXIS_W as usize - 1).enumerate() {
            put(frame, axis_x + j as u16, plot.y + row, ch, style);
        }
    };
    label(frame, 0, format!("{:+.1}%", span), muted);
    label(frame, zero_row, "0%".to_string(), muted);
    label(frame, plot.height - 1, format!("{:+.1}%", -span), muted);
    if let Some(last) = cols.last() {
        let color = if *last >= 0.0 { BULL } else { BEAR };
        label(
            frame,
            row_of(*last),
            format!("{:+.2}%", last),
            Style::default()
                .fg(color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        );
    }
}
