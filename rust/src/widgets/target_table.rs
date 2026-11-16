//! F6 in "Lists" mode: one row per symbol the ticket will act on, with the
//! position and PnL it currently has, plus a summary line for the group.
//! Replaces the per-slot strategy panel, which is noise at 20+ symbols.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::{fmt_price_col, pnl_color, render_clipped, truncate};

pub struct TargetRow {
    pub symbol: String,
    pub last: f64,
    /// Signed position size (0 = flat).
    pub qty: f64,
    pub avg: f64,
    pub upnl: f64,
    /// "L" long-only, "S" short-only, "!" conflicting groups, "" any.
    pub side: &'static str,
}

/// Rows that fit below the header and summary lines of a panel `area`.
pub fn visible_rows(area: Rect) -> usize {
    // border 2 + summary 1 + header 1
    area.height.saturating_sub(4) as usize
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[TargetRow],
    offset: usize,
    palette: &Palette,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(ratatui::text::Span::styled(
            title.to_string(),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let line = |y: u16| Rect {
        x: inner.x + 1,
        y,
        width: inner.width.saturating_sub(2),
        height: 1,
    };

    let open = rows.iter().filter(|r| r.qty != 0.0).count();
    let upnl: f64 = rows.iter().map(|r| r.upnl).sum();
    let summary = format!(
        "{} symbols   open {}   uPnL {:+.2}   (PgUp/PgDn scroll)",
        rows.len(),
        open,
        upnl
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&summary, line(inner.y).width as usize).to_string()).style(
            Style::default()
                .fg(pnl_color(upnl))
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        line(inner.y),
    );
    if inner.height < 2 {
        return;
    }
    let header = format!(
        "{:<10} {:>11} {:>17} {:>8} {}",
        "PAIR", "PRICE", "POS@AVG", "UPNL", "SIDE"
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, line(inner.y + 1).width as usize).to_string()).style(
            Style::default()
                .fg(palette.accent_dim)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        line(inner.y + 1),
    );

    let max = inner.height.saturating_sub(2) as usize;
    let offset = offset.min(rows.len().saturating_sub(max));
    for (i, r) in rows.iter().skip(offset).take(max).enumerate() {
        let y = inner.y + 2 + i as u16;
        let pos = if r.qty == 0.0 {
            "-".to_string()
        } else {
            format!("{:+.4}@{}", r.qty, fmt_price_col(r.avg).trim())
        };
        let text = format!(
            "{:<10} {:>11} {:>17} {:>8} {}",
            truncate(&r.symbol, 10),
            fmt_price_col(r.last).trim(),
            truncate(&pos, 17),
            if r.qty == 0.0 {
                "-".to_string()
            } else {
                format!("{:+.2}", r.upnl)
            },
            r.side
        );
        let fg = if r.qty == 0.0 {
            palette.muted
        } else {
            pnl_color(r.upnl)
        };
        render_clipped(
            frame,
            Paragraph::new(truncate(&text, line(y).width as usize).to_string())
                .style(Style::default().fg(fg).bg(palette.bg)),
            line(y),
        );
    }
}
