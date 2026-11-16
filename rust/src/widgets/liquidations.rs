//! The `liq` page (a desk window): liquidations as they happen — the whole
//! market or the linked symbol — with totals and the most liquidated coins.
//! Data: `data::liquidations` (Binance Futures, at most one per symbol per
//! second, so every total is a lower bound).

use std::collections::{HashMap, VecDeque};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::liquidations::{fmt_usd, totals, Liquidation};
use crate::tokens::Palette;
use crate::widgets::fmt::{fmt_clock_ms, fmt_price_col, render_clipped, truncate, BEAR, BULL};

/// What the page shows.
pub struct LiqPage<'a> {
    pub feed: &'a VecDeque<Liquidation>,
    /// The linked symbol (Binance form).
    pub symbol: &'a str,
    /// Only `symbol`, or the whole market.
    pub only_symbol: bool,
    pub min_usd: f64,
    pub connected: bool,
    pub now_ms: i64,
}

/// Clickable rows: each carries its symbol (never an index, rule 13).
#[derive(Default)]
pub struct LiqPageLayout {
    pub rows: Vec<(Rect, String)>,
}

const TEXT_W: u16 = 52;
const SIDE_W: u16 = 34;

pub fn render(frame: &mut Frame, area: Rect, page: &LiqPage, palette: &Palette) -> LiqPageLayout {
    let mut layout = LiqPageLayout::default();
    let scope = if page.only_symbol {
        page.symbol.to_string()
    } else {
        "all market".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(" Liquidations - {scope} "),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if inner.width < 24 || inner.height < 4 {
        return layout;
    }
    let line = |y: u16, x: u16, w: u16| Rect {
        x,
        y,
        width: w.min((inner.x + inner.width).saturating_sub(x)),
        height: 1,
    };
    let text = |frame: &mut Frame, r: Rect, s: &str, style: Style| {
        render_clipped(
            frame,
            Paragraph::new(truncate(s, r.width as usize).to_string()).style(style),
            r,
        );
    };
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let in_scope = |l: &&Liquidation| !page.only_symbol || l.symbol == page.symbol;
    let scoped: Vec<&Liquidation> = page.feed.iter().filter(in_scope).collect();

    // Totals: 5 minutes, an hour, a day.
    let mut x = inner.x;
    for (label, ms) in [("5m", 300_000), ("1h", 3_600_000), ("24h", 86_400_000)] {
        let t = totals(scoped.iter().copied(), page.now_ms - ms);
        for (s, fg) in [
            (format!(" {label} "), palette.muted),
            (format!("L {} ", fmt_usd(t.long_usd)), BEAR),
            (format!("S {} ", fmt_usd(t.short_usd)), BULL),
        ] {
            let w = s.chars().count() as u16;
            text(
                frame,
                line(inner.y, x, w),
                &s,
                Style::default().fg(fg).bg(palette.bg),
            );
            x = x.saturating_add(w);
        }
        x = x.saturating_add(1);
    }
    // The hour's split as a two-colour gauge: longs (bear) | shorts (bull).
    let hour = totals(scoped.iter().copied(), page.now_ms - 3_600_000);
    let sum = hour.long_usd + hour.short_usd;
    if sum > 0.0 {
        let g = line(inner.y + 1, inner.x, inner.width);
        let split = (g.width as f64 * hour.long_usd / sum).round() as u16;
        let buf = frame.buffer_mut();
        for dx in 0..g.width {
            if let Some(c) = buf.cell_mut((g.x + dx, g.y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(if dx < split { BEAR } else { BULL }));
            }
        }
    }

    let side = !page.only_symbol && inner.width >= TEXT_W + SIDE_W + 2;
    let list_w = if side {
        inner.width - SIDE_W - 1
    } else {
        inner.width
    };
    let top = inner.y + 3;
    let bottom = inner.y + inner.height - 1; // the hint row
    let rows_n = bottom.saturating_sub(top) as usize;
    let header = format!(
        "  {:<8}  {:<10}  {:<5}  {:>10}  {:>8}",
        "TIME", "SYMBOL", "SIDE", "PRICE", "USD"
    );
    text(
        frame,
        line(inner.y + 2, inner.x, list_w),
        &header,
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    let shown: Vec<&Liquidation> = scoped
        .iter()
        .rev()
        .filter(|l| l.usd() >= page.min_usd)
        .take(rows_n)
        .copied()
        .collect();
    if shown.is_empty() {
        let why = if !page.connected && page.feed.is_empty() {
            "  (connecting to Binance Futures...)".to_string()
        } else {
            format!(
                "  (none of {} or more yet - a calm market can be quiet for minutes)",
                fmt_usd(page.min_usd)
            )
        };
        text(frame, line(top, inner.x, list_w), &why, muted);
    }
    let max_usd = shown.iter().map(|l| l.usd()).fold(0.0, f64::max).max(1e-9);
    let bar_x = inner.x + TEXT_W.min(list_w);
    let bar_w = (inner.x + list_w).saturating_sub(bar_x + 1);
    for (i, l) in shown.iter().enumerate() {
        let y = top + i as u16;
        let color = if l.long { BEAR } else { BULL };
        let row = format!(
            "  {:<8}  {:<10}  {:<5}  {}  {:>8}",
            fmt_clock_ms(l.time_ms),
            truncate(&l.symbol, 10),
            if l.long { "LONG" } else { "SHORT" },
            fmt_price_col(l.price),
            fmt_usd(l.usd()),
        );
        let mut style = Style::default().fg(color).bg(palette.bg);
        if l.usd() / max_usd > 0.5 {
            style = style.add_modifier(Modifier::BOLD);
        }
        let r = line(y, inner.x, TEXT_W.min(list_w));
        text(frame, r, &row, style);
        let n = ((l.usd() / max_usd).clamp(0.0, 1.0) * bar_w as f64).round() as u16;
        let buf = frame.buffer_mut();
        for dx in 0..n {
            if let Some(c) = buf.cell_mut((bar_x + dx, y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(color));
            }
        }
        layout
            .rows
            .push((line(y, inner.x, list_w), l.symbol.clone()));
    }

    // The most liquidated coins of the hour.
    if side {
        let sx = inner.x + list_w + 1;
        text(
            frame,
            line(inner.y + 2, sx, SIDE_W),
            "MOST LIQUIDATED, 1H   LONG/SHORT",
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        );
        let mut by: HashMap<&str, (f64, f64)> = HashMap::new();
        for l in scoped
            .iter()
            .filter(|l| l.time_ms >= page.now_ms - 3_600_000)
        {
            let e = by.entry(l.symbol.as_str()).or_default();
            if l.long {
                e.0 += l.usd();
            } else {
                e.1 += l.usd();
            }
        }
        let mut ranked: Vec<(&str, f64, f64)> =
            by.into_iter().map(|(s, (a, b))| (s, a, b)).collect();
        ranked.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)).then(a.0.cmp(b.0)));
        for (i, (sym, long, short)) in ranked.iter().take(rows_n).enumerate() {
            let y = top + i as u16;
            let total = long + short;
            let split = format!("{:.0}%/{:.0}%", long / total * 100.0, short / total * 100.0);
            let s = format!(
                " {:<10} {:>8}  {split:>9}",
                truncate(sym, 10),
                fmt_usd(total)
            );
            let fg = if long >= short { BEAR } else { BULL };
            let r = line(y, sx, SIDE_W);
            text(frame, r, &s, Style::default().fg(fg).bg(palette.bg));
            layout.rows.push((r, sym.to_string()));
        }
    }

    let hint = format!(
        " +/- smallest {}   s: {}   click: show the symbol   largest per coin per second only",
        fmt_usd(page.min_usd),
        if page.only_symbol {
            "all market"
        } else {
            "this symbol"
        },
    );
    text(frame, line(bottom, inner.x, inner.width), &hint, muted);
    layout
}

/// The next smallest size on `+` / `-`.
pub fn step_min_usd(v: f64, up: bool) -> f64 {
    const STEPS: [f64; 8] = [
        0.0,
        1_000.0,
        5_000.0,
        10_000.0,
        50_000.0,
        100_000.0,
        500_000.0,
        1_000_000.0,
    ];
    let i = STEPS
        .iter()
        .position(|s| *s >= v)
        .unwrap_or(STEPS.len() - 1);
    let j = match (up, STEPS[i] > v) {
        (true, true) => i,
        (true, false) => (i + 1).min(STEPS.len() - 1),
        (false, _) => i.saturating_sub(1),
    };
    STEPS[j]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_size_steps_and_stops_at_the_ends() {
        assert_eq!(step_min_usd(10_000.0, true), 50_000.0);
        assert_eq!(step_min_usd(10_000.0, false), 5_000.0);
        assert_eq!(step_min_usd(0.0, false), 0.0);
        assert_eq!(step_min_usd(1_000_000.0, true), 1_000_000.0);
        assert_eq!(step_min_usd(7_000.0, true), 10_000.0);
        assert_eq!(step_min_usd(7_000.0, false), 5_000.0);
    }
}
