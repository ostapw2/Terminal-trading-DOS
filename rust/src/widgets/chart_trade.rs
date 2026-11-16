//! Trading on the chart, as in MetaTrader: a SELL / size / BUY strip in the
//! plot's top-right corner, and horizontal lines for the position (entry),
//! its stop-loss and take-profit and the resting limit orders.
//!
//! - Click SELL / BUY: a market order of the shown size; CLOSE flattens.
//! - Drag the position line up or down: a stop or a take where it is let go
//!   (below a long's entry it is a stop, above it a take; mirrored for a short).
//! - Drag the SL / TP line: move it; click its `×`: remove it.
//!
//! The widget only draws and reports where its clickable parts are; what a
//! click does lives in `commander.rs`.  Lines go through the chart's one
//! price mapping (`chart::price_to_row`); on the pixel chart the lines
//! themselves are pixels ([`hlines`] into `chart::Overlays`) and only the
//! labels are cells.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    Frame,
};

use crate::engine::Bracket;
use crate::tokens::Palette;
use crate::widgets::chart::price_to_row;
use crate::widgets::fmt::{fmt_value, BEAR, BULL};

/// A horizontal trade line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    Position,
    Sl,
    Tp,
}

/// A clickable part of the trade overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Sell,
    Buy,
    Close,
    /// The size between the buttons (the wheel changes it).
    Size,
    /// Grab a line to drag it.
    Line(Line),
    /// The `×` on a stop / take label.
    Remove(Line),
}

/// What the overlay shows for the chart's symbol.
#[derive(Clone, Debug, Default)]
pub struct TradeMarks {
    /// Signed position (0: flat) and its average entry.
    pub qty: f64,
    pub avg: f64,
    /// The latest price, for the position's open P&L.
    pub last: Option<f64>,
    pub bracket: Bracket,
    /// Resting limit orders: (buy, limit, qty).
    pub orders: Vec<(bool, f64, f64)>,
    /// The order size as the buttons will send it ("$25", "0.010").
    pub size: String,
    /// A line being dragged and where it is now.
    pub drag: Option<(Line, f64)>,
}

impl TradeMarks {
    fn flat(&self) -> bool {
        self.qty.abs() < 1e-12
    }

    /// P&L of the whole position if it closed at `price`.
    pub fn pnl_at(&self, price: f64) -> f64 {
        self.qty * (price - self.avg)
    }

    /// Where each line is drawn: the dragged one at the pointer.
    fn lines(&self) -> Vec<(Line, f64)> {
        let mut out = Vec::new();
        if !self.flat() {
            out.push((Line::Position, self.avg));
            if let Some(p) = self.bracket.sl {
                out.push((Line::Sl, p));
            }
            if let Some(p) = self.bracket.tp {
                out.push((Line::Tp, p));
            }
        }
        if let Some((line, price)) = self.drag {
            match line {
                // Dragging out of the entry makes a new stop or take: the
                // entry stays where it is and the new line follows.
                Line::Position => {
                    if let Some(new) = self.drag_target(price) {
                        out.retain(|(l, _)| *l != new);
                        out.push((new, price));
                    }
                }
                _ => {
                    out.retain(|(l, _)| *l != line);
                    out.push((line, price));
                }
            }
        }
        out
    }

    /// What dragging the position line to `price` makes: a stop on the
    /// losing side of the entry, a take on the winning side.
    pub fn drag_target(&self, price: f64) -> Option<Line> {
        if self.flat() || price == self.avg {
            return None;
        }
        let up = price > self.avg;
        Some(if up == (self.qty > 0.0) {
            Line::Tp
        } else {
            Line::Sl
        })
    }
}

fn color(line: Line, palette: &Palette) -> Color {
    match line {
        Line::Position => palette.accent,
        Line::Sl => BEAR,
        Line::Tp => BULL,
    }
}

/// The lines in pixels for the pixel chart: (price, colour).  Limit orders
/// are drawn in the muted colour.
pub fn hlines(marks: &TradeMarks, palette: &Palette) -> Vec<(f64, Color)> {
    let mut out: Vec<(f64, Color)> = marks
        .orders
        .iter()
        .map(|(_, p, _)| (*p, palette.muted))
        .collect();
    out.extend(
        marks
            .lines()
            .into_iter()
            .map(|(l, p)| (p, color(l, palette))),
    );
    out
}

fn money(v: f64) -> String {
    format!("{}${:.2}", if v < 0.0 { "-" } else { "+" }, v.abs())
}

/// Write `text` at (x, y) in `style`, clipped to `end`; returns the rect
/// it took (empty when nothing fit).
fn put(frame: &mut Frame, x: u16, y: u16, end: u16, text: &str, style: Style) -> Rect {
    let buf = frame.buffer_mut();
    let area = buf.area;
    if y < area.y || y >= area.y + area.height {
        return Rect::default();
    }
    let mut cx = x;
    for ch in text.chars() {
        if cx >= end || cx >= area.x + area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        cx += 1;
    }
    Rect {
        x,
        y,
        width: cx.saturating_sub(x),
        height: 1,
    }
}

/// A dashed line across the empty cells of `row` (character chart only:
/// the pixel chart draws its lines itself).
fn dash_row(frame: &mut Frame, plot: Rect, row: u16, fg: Color, palette: &Palette) {
    let buf = frame.buffer_mut();
    let y = plot.y + row;
    for x in plot.x..plot.x + plot.width {
        if let Some(cell) = buf.cell_mut((x, y)) {
            let blank = matches!(cell.symbol(), " " | "─" | "┄" | "·");
            if blank && cell.bg == palette.bg {
                cell.set_char('┄');
                cell.set_style(Style::default().fg(fg).bg(palette.bg));
            }
        }
    }
}

/// Draw the overlay over the plot (after the candles) and its price tags on
/// the axis; returns the clickable parts.  `pixels`: the lines are already
/// in the picture.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    plot: Rect,
    axis: Rect,
    (hi, lo): (f64, f64),
    marks: &TradeMarks,
    pixels: bool,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> Vec<(Rect, Hit)> {
    let mut hits = Vec::new();
    if plot.width < 20 || plot.height < 3 || hi <= lo {
        return hits;
    }
    let end = plot.x + plot.width;
    let on = |r: Rect| mouse.is_some_and(|(x, y)| crate::widgets::fmt::rect_contains(r, x, y));

    // Limit orders: a thin muted line and a small tag on the right.
    for (buy, price, qty) in &marks.orders {
        if !(lo..=hi).contains(price) {
            continue;
        }
        let row = price_to_row(*price, hi, lo, plot.height).min(plot.height - 1);
        if !pixels {
            dash_row(frame, plot, row, palette.muted, palette);
        }
        let tag = format!(
            " {} {} @ {} ",
            if *buy { "BUY LMT" } else { "SELL LMT" },
            crate::widgets::fmt::fmt_qty_plain(*qty),
            fmt_value(*price)
        );
        let w = tag.chars().count() as u16;
        put(
            frame,
            end.saturating_sub(w + 1).max(plot.x),
            plot.y + row,
            end,
            &tag,
            Style::default().fg(palette.text).bg(palette.bg),
        );
    }

    // Position, stop and take: a line and a label on the left; off the
    // visible range, a tag on the top or bottom row with an arrow.
    let drag_line = marks.drag.map(|(l, p)| match l {
        Line::Position => marks.drag_target(p).unwrap_or(Line::Position),
        other => other,
    });
    for (line, price) in marks.lines() {
        let fg = color(line, palette);
        let (row, edge) = if price > hi {
            (0, Some('▲'))
        } else if price < lo {
            (plot.height - 1, Some('▼'))
        } else {
            (
                price_to_row(price, hi, lo, plot.height).min(plot.height - 1),
                None,
            )
        };
        if edge.is_none() && !pixels {
            dash_row(frame, plot, row, fg, palette);
        }
        let mut label = match line {
            Line::Position => {
                let side = if marks.qty > 0.0 { "LONG" } else { "SHORT" };
                let pnl = marks.last.map(|l| marks.pnl_at(l)).unwrap_or(0.0);
                format!(
                    " {side} {} @ {}  {} ",
                    crate::widgets::fmt::fmt_qty_plain(marks.qty.abs()),
                    fmt_value(marks.avg),
                    money(pnl)
                )
            }
            Line::Sl | Line::Tp => format!(
                " {} {}  {} ",
                if line == Line::Sl { "SL" } else { "TP" },
                fmt_value(price),
                money(marks.pnl_at(price))
            ),
        };
        if let Some(arrow) = edge {
            label = format!(" {arrow}{label}");
        }
        let y = plot.y + row;
        let dragging = drag_line == Some(line);
        let style = Style::default()
            .fg(palette.black)
            .bg(fg)
            .add_modifier(if dragging {
                Modifier::BOLD | Modifier::REVERSED
            } else {
                Modifier::BOLD
            });
        let r = put(frame, plot.x, y, end, &label, style);
        if r.width > 0 && edge.is_none() {
            // The whole row grabs the line, the label too.
            hits.push((
                Rect {
                    x: plot.x,
                    y,
                    width: plot.width,
                    height: 1,
                },
                Hit::Line(line),
            ));
        }
        if line != Line::Position && marks.drag.is_none() && r.width > 0 {
            let x = r.x + r.width;
            let close = Rect {
                x,
                y,
                width: 3,
                height: 1,
            };
            let st = if on(close) {
                Style::default().fg(palette.black).bg(palette.text)
            } else {
                Style::default().fg(palette.black).bg(fg)
            };
            let drawn = put(frame, x, y, end, " × ", st);
            if drawn.width == 3 {
                // Before the row's grab area: the `×` wins.
                hits.insert(hits.len().saturating_sub(1), (drawn, Hit::Remove(line)));
            }
        }
        // The level on the price axis, in the line's colour.
        if axis.width > 2 && edge.is_none() {
            let w = axis.width.saturating_sub(1) as usize;
            let tag = format!("{:<w$}", fmt_value(price));
            put(
                frame,
                axis.x + 1,
                y,
                axis.x + axis.width,
                &tag,
                Style::default()
                    .fg(palette.black)
                    .bg(fg)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    // The strip: [ SELL ] size [ BUY ] [ CLOSE ], top right of the plot.
    let mut parts: Vec<(String, Hit, Color)> = vec![
        (" SELL ".into(), Hit::Sell, BEAR),
        (format!(" {} ", marks.size), Hit::Size, palette.muted),
        (" BUY ".into(), Hit::Buy, BULL),
    ];
    if !marks.flat() {
        parts.push((" CLOSE ".into(), Hit::Close, palette.text));
    }
    let width: u16 = parts
        .iter()
        .map(|(t, _, _)| t.chars().count() as u16 + 1)
        .sum();
    if width + 2 <= plot.width {
        let mut x = end - width;
        let y = plot.y;
        for (text, hit, bg) in parts {
            let w = text.chars().count() as u16;
            let r = Rect {
                x,
                y,
                width: w,
                height: 1,
            };
            let style = match hit {
                Hit::Size => Style::default().fg(palette.text).bg(palette.bg),
                _ if on(r) => Style::default()
                    .fg(bg)
                    .bg(palette.black)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default()
                    .fg(palette.black)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            };
            let drawn = put(frame, x, y, end, &text, style);
            // The strip sits on top of everything else in the plot.
            hits.insert(0, (drawn, hit));
            x += w + 1;
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long() -> TradeMarks {
        TradeMarks {
            qty: 2.0,
            avg: 100.0,
            last: Some(105.0),
            size: "$25".into(),
            ..TradeMarks::default()
        }
    }

    #[test]
    fn dragging_the_entry_makes_a_stop_below_a_long_and_a_take_above() {
        let m = long();
        assert_eq!(m.drag_target(95.0), Some(Line::Sl));
        assert_eq!(m.drag_target(110.0), Some(Line::Tp));
        assert_eq!(m.drag_target(100.0), None);
        let short = TradeMarks {
            qty: -1.0,
            ..long()
        };
        assert_eq!(short.drag_target(95.0), Some(Line::Tp));
        assert_eq!(short.drag_target(110.0), Some(Line::Sl));
        assert_eq!(TradeMarks::default().drag_target(1.0), None, "flat");
        assert_eq!(m.pnl_at(90.0), -20.0);
    }

    #[test]
    fn a_dragged_line_replaces_the_one_it_moves() {
        let mut m = long();
        m.bracket.sl = Some(90.0);
        m.drag = Some((Line::Sl, 95.0));
        let lines = m.lines();
        assert!(lines.contains(&(Line::Sl, 95.0)) && !lines.contains(&(Line::Sl, 90.0)));
        m.drag = Some((Line::Position, 92.0));
        assert!(
            m.lines().contains(&(Line::Sl, 92.0)),
            "the entry drag shows the new stop"
        );
        assert!(
            m.lines().contains(&(Line::Position, 100.0)),
            "the entry stays"
        );
    }

    #[test]
    fn renders_at_every_size_and_reports_the_buttons() {
        let palette = crate::tokens::CLASSIC;
        let mut m = long();
        m.bracket.sl = Some(95.0);
        m.bracket.tp = Some(500.0); // off the top
        m.orders.push((true, 98.0, 1.0));
        for (w, h) in [(1, 1), (10, 3), (20, 3), (40, 10), (120, 30)] {
            let backend = ratatui::backend::TestBackend::new(w + 9, h);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| {
                let plot = Rect::new(0, 0, w, h);
                let axis = Rect::new(w, 0, 9, h);
                let hits = render(f, plot, axis, (110.0, 90.0), &m, false, None, &palette);
                if w >= 40 && h >= 10 {
                    for want in [Hit::Sell, Hit::Buy, Hit::Close, Hit::Line(Line::Sl)] {
                        assert!(hits.iter().any(|(_, h)| *h == want), "{want:?} at {w}x{h}");
                    }
                    // Off-range take: shown, but not grabbable.
                    assert!(!hits.iter().any(|(_, h)| *h == Hit::Line(Line::Tp)));
                }
            })
            .unwrap();
        }
    }
}
