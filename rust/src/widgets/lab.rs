//! F2 — Lab: set up a strategy backtest (strategy, symbol, timeframe,
//! parameters), run it, and read the result next to its equity curve.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::{pnl_color, render_clipped, truncate};

pub struct LabRow {
    pub label: String,
    pub value: String,
    /// Dimmed: not editable with the arrow keys (e.g. the symbol).
    pub note: String,
}

pub struct LabView<'a> {
    pub rows: &'a [LabRow],
    pub selected: usize,
    /// One line each, shown under "Result".
    pub summary: &'a [String],
    /// Return % of the last run, to colour the headline.
    pub headline_pct: Option<f64>,
    /// Message line (running, error, hint).
    pub status: &'a str,
    /// The last run in plain words: the question it answers, the answer
    /// (and whether it is good news), how to read the chart.  `None`: the
    /// summary lines alone.
    pub answer: Option<(&'a str, &'a str, bool, &'a str)>,
    /// What the chosen strategy does, in a sentence (under the setup).
    pub about: &'a str,
}

#[derive(Default)]
pub struct LabLayout {
    pub rows: Vec<(Rect, usize)>,
    /// Free area under the summary for the equity chart.
    pub chart: Rect,
}

const LEFT_W: u16 = 50;

fn panel<'a>(title: &str, palette: &Palette) -> Block<'a> {
    Block::default()
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
        .style(Style::default().fg(palette.text).bg(palette.bg))
}

pub fn render(frame: &mut Frame, area: Rect, view: &LabView, palette: &Palette) -> LabLayout {
    let mut layout = LabLayout::default();
    let left_w = LEFT_W.min(area.width / 2);
    let left = Rect {
        width: left_w,
        ..area
    };
    let right = Rect {
        x: area.x.saturating_add(left_w),
        width: area.width.saturating_sub(left_w),
        ..area
    };

    // ── setup ──
    let block = panel(crate::i18n::pick(" Setup ", " Налаштування "), palette);
    let inner = block.inner(left);
    render_clipped(frame, block, left);
    let max_rows = inner.height as usize;
    let start = if view.rows.len() <= max_rows {
        0
    } else {
        view.selected
            .saturating_sub(max_rows / 2)
            .min(view.rows.len() - max_rows)
    };
    for (i, r) in view.rows.iter().enumerate().skip(start).take(max_rows) {
        let row = Rect {
            x: inner.x,
            y: inner.y + (i - start) as u16,
            width: inner.width,
            height: 1,
        };
        let sel = i == view.selected;
        let style = if sel {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        let marker = if sel { '>' } else { ' ' };
        let text = format!(
            "{marker} {:<13} {:<13} {}",
            truncate(&r.label, 13),
            truncate(&r.value, 13),
            r.note
        );
        let padded = format!("{text:<w$}", w = row.width as usize);
        render_clipped(
            frame,
            Paragraph::new(truncate(&padded, row.width as usize).to_string()).style(style),
            row,
        );
        layout.rows.push((row, i));
    }

    // What the strategy does, under its settings.
    let used = view.rows.len().min(max_rows) as u16;
    if !view.about.is_empty() && inner.height > used + 3 {
        let w = inner.width.saturating_sub(2) as usize;
        let lines = crate::widgets::compare::wrap(
            &format!(
                "{} {}",
                crate::i18n::pick("What it does:", "Що робить:"),
                view.about
            ),
            w,
        );
        for (y, l) in (inner.y + used + 1..inner.y + inner.height).zip(lines) {
            render_clipped(
                frame,
                Paragraph::new(l).style(Style::default().fg(palette.muted).bg(palette.bg)),
                Rect {
                    x: inner.x + 1,
                    y,
                    width: inner.width.saturating_sub(2),
                    height: 1,
                },
            );
        }
    }

    // ── result ──
    let block = panel(crate::i18n::pick(" Result ", " Результат "), palette);
    let inner = block.inner(right);
    render_clipped(frame, block, right);
    if let Some((question, answer, good, how)) = view.answer {
        let head = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        let busy = !view.status.is_empty() && view.status != crate::i18n::pick("Done.", "Готово.");
        let head = if busy {
            render_clipped(
                frame,
                Paragraph::new(truncate(view.status, head.width as usize).to_string())
                    .style(Style::default().fg(palette.muted).bg(palette.bg)),
                Rect { height: 1, ..head },
            );
            Rect {
                y: head.y + 1,
                height: head.height.saturating_sub(1),
                ..head
            }
        } else {
            head
        };
        let tone = if good {
            crate::widgets::compare::Tone::Yes
        } else {
            crate::widgets::compare::Tone::No
        };
        let rest =
            crate::widgets::compare::render_head(frame, head, question, answer, tone, how, palette);
        let mut y = rest.y;
        let bottom = rest.y + rest.height;
        for s in view.summary {
            if y >= bottom {
                break;
            }
            render_clipped(
                frame,
                Paragraph::new(truncate(s, rest.width as usize).to_string())
                    .style(Style::default().fg(palette.text).bg(palette.bg)),
                Rect {
                    y,
                    height: 1,
                    ..rest
                },
            );
            y += 1;
        }
        if bottom.saturating_sub(y + 1) >= 8 {
            layout.chart = Rect {
                x: inner.x,
                y: y + 1,
                width: inner.width,
                height: bottom - y - 1,
            };
        }
        return layout;
    }
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    let line = |y: u16| Rect {
        x: inner.x + 1,
        y,
        width: inner.width.saturating_sub(2),
        height: 1,
    };
    let status_style = Style::default().fg(palette.muted).bg(palette.bg);
    if y < bottom {
        render_clipped(
            frame,
            Paragraph::new(truncate(view.status, line(y).width as usize).to_string())
                .style(status_style),
            line(y),
        );
        y += 1;
    }
    for (i, s) in view.summary.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let style = if i == 0 {
            let c = view.headline_pct.map(pnl_color).unwrap_or(palette.text);
            Style::default()
                .fg(c)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        render_clipped(
            frame,
            Paragraph::new(truncate(s, line(y).width as usize).to_string()).style(style),
            line(y),
        );
        y += 1;
    }
    // The chart gets what is left, if it is worth drawing.
    if bottom.saturating_sub(y + 1) >= 8 {
        layout.chart = Rect {
            x: inner.x,
            y: y + 1,
            width: inner.width,
            height: bottom - y - 1,
        };
    }
    layout
}

/// The run against simply holding: the account (green when it ended up,
/// red when down) and the coin (grey), both in % from the same start, buys
/// ▲ and sells ▼ on the account line; the legend and the scale in cells.
/// `false` when pixels are off (the caller draws characters).
pub fn render_equity_chart(
    frame: &mut Frame,
    area: Rect,
    equity: &[f64],
    prices: &[f64],
    marks: &[(usize, bool)],
    symbol: &str,
    palette: &Palette,
) -> bool {
    use crate::gfx::{self, Canvas};
    use crate::widgets::fmt::{BEAR, BULL};
    use ratatui::style::Color;
    let n = equity.len().min(prices.len());
    if n < 2 || area.height < 5 || area.width < 24 {
        return false;
    }
    let plot = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(10),
        height: area.height - 2,
    };
    let Some((pw, ph)) = gfx::plot_px(plot) else {
        return false;
    };
    let pct = |v: &[f64]| -> Vec<f64> {
        let f = v[0];
        v[..n]
            .iter()
            .map(|x| if f > 0.0 { (x / f - 1.0) * 100.0 } else { 0.0 })
            .collect()
    };
    let (eq, hold) = (pct(equity), pct(prices));
    let (lo, hi) = eq
        .iter()
        .chain(&hold)
        .fold((0.0f64, 0.0f64), |(l, h), v| (l.min(*v), h.max(*v)));
    let pad = ((hi - lo) * 0.05).max(0.05);
    let (lo, hi) = (lo - pad, hi + pad);
    let mut cv = Canvas::new(pw, ph, palette.bg);
    let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
    let x_of = |i: usize| (i as f64 / (n - 1) as f64 * (pw as f64 - 1.0)).round() as i64;
    let y_of = |v: f64| ((hi - v) / (hi - lo) * (ph as f64 - 1.0)).round() as i64;
    let grid = cv.ink(palette.muted);
    cv.dashed_hline(y_of(0.0), pt, pt as usize, 3 * pt as usize, grid);
    let grey = cv.ink(Color::Gray);
    let up = eq[n - 1] >= 0.0;
    let mine = cv.ink(if up { BULL } else { BEAR });
    for (series, ink, w) in [(&hold, grey, pt), (&eq, mine, 2 * pt)] {
        for i in 1..n {
            cv.line(
                x_of(i - 1),
                y_of(series[i - 1]),
                x_of(i),
                y_of(series[i]),
                w,
                ink,
            );
        }
    }
    let (buy, sell) = (cv.ink(Color::LightCyan), cv.ink(Color::LightMagenta));
    let r = 3 * pt;
    for &(i, is_buy) in marks.iter().filter(|m| m.0 < n) {
        let (x, y) = (x_of(i), y_of(eq[i]));
        // A small triangle: ▲ under the line for a buy, ▼ over it for a sell.
        for k in 0..=r {
            if is_buy {
                cv.fill(x - k, y + r + k, x + k + 1, y + r + k + 1, buy);
            } else {
                cv.fill(x - k, y - r - k, x + k + 1, y - r - k + 1, sell);
            }
        }
    }
    gfx::submit(frame.buffer_mut(), plot, cv);
    // Legend and scale.
    let buf = frame.buffer_mut();
    let mut put = |x: u16, y: u16, s: &str, st: Style| {
        for (i, ch) in s.chars().enumerate() {
            let cx = x + i as u16;
            if cx >= area.x + area.width || y >= area.y + area.height {
                break;
            }
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(ch);
                c.set_style(st);
            }
        }
    };
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let mut x = area.x + 1;
    let a = format!(
        "── {} {:+.2}%   ",
        crate::i18n::pick("your account", "твій рахунок"),
        eq[n - 1]
    );
    put(
        x,
        area.y,
        &a,
        Style::default()
            .fg(if up { BULL } else { BEAR })
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    x += a.chars().count() as u16;
    let b = format!(
        "── {} {symbol} {:+.2}%   ",
        crate::i18n::pick("holding", "тримання"),
        hold[n - 1]
    );
    put(
        x,
        area.y,
        &b,
        Style::default().fg(Color::Gray).bg(palette.bg),
    );
    x += b.chars().count() as u16;
    put(
        x,
        area.y,
        crate::i18n::pick("▲ buy  ▼ sell", "▲ купівля  ▼ продаж"),
        muted,
    );
    let ax = plot.x + plot.width + 1;
    put(ax, plot.y, &format!("{hi:+.1}%"), muted);
    put(ax, plot.y + plot.height - 1, &format!("{lo:+.1}%"), muted);
    let zr = plot.y + ((hi / (hi - lo)) * (plot.height as f64 - 1.0)).round() as u16;
    put(ax, zr, " 0%", muted);
    true
}
