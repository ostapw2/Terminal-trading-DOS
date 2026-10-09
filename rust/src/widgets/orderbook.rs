//! F6 — Order book (depth-of-market) view: bids + asks at price levels +
//! a live trades feed on the right.  Real-time data via Binance WebSocket
//! `@depth20@100ms` + `@aggTrade`.
//!
//! Layout: title bar with mid+spread+zoom, imbalance gauge row, then a
//! 60/40 horizontal split — book on the left (asks top, spread row,
//! bids bottom) and recent trades feed on the right.

use crate::widgets::fmt::{fmt_clock_ms, fmt_price_col, fmt_qty_col, render_clipped, BEAR, BULL};
use std::collections::VecDeque;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::binance::AggTrade;
use crate::data::orderbook::OrderBookSnapshot;
use crate::tokens::Palette;

pub struct OrderBookLayout {
    pub area: Rect,
}

/// `zoom` is a multiplier over the native tick size (1, 5, 10, 50, 100).
/// `last_trade_at_price` is the price of the most recent trade — rows in
/// the book covering this price flash for visibility.  Pass `None` if no
/// recent trade or the highlight has expired.
/// Diagnostic counters surfaced while the book is still loading so the user
/// can see why "(waiting for depth stream…)" persists — buffer growing means
/// WS is alive but seed catch-up keeps gapping; both at zero usually means
/// REST seed is in flight or WS handshake is still pending.
#[derive(Clone, Copy, Default)]
pub struct LoadDiag {
    pub buffer_len: usize,
    pub seed_in_flight: bool,
    pub diff_ws_open: bool,
    pub trade_ws_open: bool,
    pub trades_received: usize,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    ticker: &str,
    snapshot: Option<&OrderBookSnapshot>,
    recent_trades: &VecDeque<AggTrade>,
    zoom: u32,
    last_trade_at_price: Option<f64>,
    stale_secs: u64,
    show_trades: bool,
    diag: LoadDiag,
    palette: &Palette,
) -> OrderBookLayout {
    // ── Title bar with live mid / spread / imbalance ──
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            build_title(ticker, snapshot, zoom, stale_secs),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    // Vertical: header (1 row imbalance bar) + body (book + trades).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(4)])
        .split(inner);
    render_header(frame, rows[0], snapshot, palette);

    if show_trades {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(rows[1]);
        render_book(
            frame,
            cols[0],
            snapshot,
            zoom,
            last_trade_at_price,
            diag,
            palette,
        );
        render_trades(frame, cols[1], recent_trades, palette);
    } else {
        render_book(
            frame,
            rows[1],
            snapshot,
            zoom,
            last_trade_at_price,
            diag,
            palette,
        );
    }

    OrderBookLayout { area }
}

fn build_title(
    ticker: &str,
    snap: Option<&OrderBookSnapshot>,
    zoom: u32,
    stale_secs: u64,
) -> String {
    let stale_tag = if stale_secs > 3 {
        format!("  ⚠ STALLED {}s", stale_secs)
    } else {
        String::new()
    };
    match snap {
        Some(s) => match (s.best_bid(), s.best_ask()) {
            (Some(b), Some(a)) => {
                let mid = (a + b) / 2.0;
                let spread = a - b;
                let bp = if mid > 0.0 {
                    spread / mid * 10_000.0
                } else {
                    0.0
                };
                format!(
                    " Order Book — {}    Mid {:.4}    Spread {:.4} ({:.1} bp)    zoom x{}{} ",
                    ticker, mid, spread, bp, zoom, stale_tag
                )
            }
            _ => format!(" Order Book — {}    zoom x{}{} ", ticker, zoom, stale_tag),
        },
        None => format!(
            " Order Book — {} (loading…)    zoom x{}{} ",
            ticker, zoom, stale_tag
        ),
    }
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    snap: Option<&OrderBookSnapshot>,
    palette: &Palette,
) {
    if area.width < 20 || area.height == 0 {
        return;
    }
    let snap = match snap {
        Some(s) => s,
        None => return,
    };
    let bid_total = snap.total_bid_size();
    let ask_total = snap.total_ask_size();
    let total = (bid_total + ask_total).max(1e-9);
    let bid_pct = (bid_total / total * 100.0).round() as u16;
    let ask_pct = 100u16.saturating_sub(bid_pct);

    // Imbalance bar: row 0 is the imbalance gauge, row 1 is column header.
    let bar_y = area.y;
    let buf = frame.buffer_mut();
    let bar_w = area.width;
    let bid_cells = (bar_w as f64 * (bid_total / total)).round() as u16;
    let bid_label = format!(" Bid {}% ({}) ", bid_pct, fmt_qty_col(bid_total).trim());
    let ask_label = format!(" Ask {}% ({}) ", ask_pct, fmt_qty_col(ask_total).trim());

    // Pre-fill row.
    for x in 0..bar_w {
        if let Some(c) = buf.cell_mut((area.x + x, bar_y)) {
            c.set_char(' ');
            let bg = if x < bid_cells { BULL } else { BEAR };
            c.set_style(
                Style::default()
                    .fg(palette.bg)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
    // Overlay labels: bid_label left-anchored, ask_label right-anchored.
    for (i, ch) in bid_label.chars().enumerate() {
        let x = area.x + 1 + i as u16;
        if x >= area.x + bar_w {
            break;
        }
        if let Some(c) = buf.cell_mut((x, bar_y)) {
            c.set_char(ch);
        }
    }
    let ask_w = ask_label.chars().count() as u16;
    let ask_start_x = area.x + bar_w.saturating_sub(ask_w + 1);
    for (i, ch) in ask_label.chars().enumerate() {
        let x = ask_start_x + i as u16;
        if x >= area.x + bar_w {
            break;
        }
        if let Some(c) = buf.cell_mut((x, bar_y)) {
            c.set_char(ch);
        }
    }
}

fn render_book(
    frame: &mut Frame,
    area: Rect,
    snap: Option<&OrderBookSnapshot>,
    zoom: u32,
    last_trade_at_price: Option<f64>,
    diag: LoadDiag,
    palette: &Palette,
) {
    if area.width < 30 || area.height < 6 {
        return;
    }
    let header = format!("  {:>10}  {:>9}  {:>9}  DEPTH", "PRICE", "SIZE", "CUM");
    render_clipped(
        frame,
        Paragraph::new(header).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    let snap = match snap {
        Some(s) => s,
        None => {
            let line1 = "  (waiting for depth stream…)".to_string();
            let line2 = format!(
                "  diag: ws-diff={}  ws-trades={}  seed={}  buf={}  trades_rx={}",
                if diag.diff_ws_open { "open" } else { "OFF" },
                if diag.trade_ws_open { "open" } else { "OFF" },
                if diag.seed_in_flight {
                    "in-flight"
                } else {
                    "none"
                },
                diag.buffer_len,
                diag.trades_received,
            );
            render_clipped(
                frame,
                Paragraph::new(line1).style(Style::default().fg(palette.muted).bg(palette.bg)),
                Rect {
                    x: area.x,
                    y: area.y + 2,
                    width: area.width,
                    height: 1,
                },
            );
            render_clipped(
                frame,
                Paragraph::new(line2).style(Style::default().fg(palette.muted).bg(palette.bg)),
                Rect {
                    x: area.x,
                    y: area.y + 3,
                    width: area.width,
                    height: 1,
                },
            );
            return;
        }
    };

    let tick = snap.estimate_tick();
    let bucket = tick * zoom.max(1) as f64;
    let agg = snap.aggregated(bucket);

    // body_h excludes the column header (1) and the spread row (1).
    let body_h = area.height.saturating_sub(2);
    let half = (body_h / 2) as usize;
    let max_asks = half.min(agg.asks.len());
    let max_bids = half.min(agg.bids.len());
    let body_top_pad = 0;

    // Independent max-size per side so bid bars stay visible even when one
    // big ask whale would otherwise dominate the combined normaliser.
    let max_ask_size = agg
        .asks
        .iter()
        .take(max_asks)
        .map(|(_, q)| *q)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let max_bid_size = agg
        .bids
        .iter()
        .take(max_bids)
        .map(|(_, q)| *q)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let depth_w = (area.width as usize).saturating_sub(38).max(4);

    // Asks: top-down display, but data is ascending → reverse top portion.
    let take_asks: Vec<&(f64, f64)> = agg.asks.iter().take(max_asks).collect();
    // Cumulative from best ask outward (so cum[best] = best_size, cum[2nd] = +2nd, etc.)
    let mut ask_cum_for_pos: Vec<f64> = Vec::with_capacity(take_asks.len());
    let mut acc = 0.0;
    for (_, q) in &take_asks {
        acc += q;
        ask_cum_for_pos.push(acc);
    }
    // For top-down render we reverse — index 0 in display is the FURTHEST ask.
    let mut y = area.y + 1 + body_top_pad;
    for i in (0..take_asks.len()).rev() {
        if y >= area.y + area.height {
            break;
        }
        let (price, size) = *take_asks[i];
        let cum = ask_cum_for_pos[i];
        let flash = matches!(last_trade_at_price, Some(p) if (p - price).abs() < bucket / 2.0);
        render_book_row(
            frame,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            price,
            size,
            cum,
            BEAR,
            depth_w,
            max_ask_size,
            flash,
            palette,
        );
        y += 1;
    }

    // Spread row.
    if y < area.y + area.height {
        let spread_str = match (snap.best_bid(), snap.best_ask()) {
            (Some(b), Some(a)) => format!(
                "  ─── {} / {}   spread {:.6}   tick {:.6} (x{}) ───",
                fmt_price_col(b),
                fmt_price_col(a),
                a - b,
                tick,
                zoom
            ),
            _ => "  ─── (no top bid/ask) ───".into(),
        };
        render_clipped(
            frame,
            Paragraph::new(spread_str).style(
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
        y += 1;
    }

    // Bids: highest first → top of bids block.  Cumulative from best bid down.
    let mut bid_acc = 0.0;
    for (price, size) in agg.bids.iter().take(max_bids) {
        if y >= area.y + area.height {
            break;
        }
        bid_acc += size;
        let flash = matches!(last_trade_at_price, Some(p) if (p - price).abs() < bucket / 2.0);
        render_book_row(
            frame,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            *price,
            *size,
            bid_acc,
            BULL,
            depth_w,
            max_bid_size,
            flash,
            palette,
        );
        y += 1;
    }
}

fn render_book_row(
    frame: &mut Frame,
    area: Rect,
    price: f64,
    size: f64,
    cum: f64,
    color: Color,
    depth_w: usize,
    max_size: f64,
    flash: bool,
    palette: &Palette,
) {
    let buf = frame.buffer_mut();
    // Background tint when this row contains the most recent trade.
    let row_bg = if flash {
        ratatui::style::Color::Yellow
    } else {
        palette.bg
    };
    for x in 0..area.width {
        if let Some(c) = buf.cell_mut((area.x + x, area.y)) {
            c.set_char(' ');
            c.set_style(Style::default().fg(palette.text).bg(row_bg));
        }
    }

    // PRICE column at offset 2..14.
    let price_str = format!("  {}  ", fmt_price_col(price));
    for (i, ch) in price_str.chars().enumerate() {
        let x = area.x + i as u16;
        if x >= area.x + area.width {
            break;
        }
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_char(ch);
            c.set_style(
                Style::default()
                    .fg(if flash { palette.bg } else { color })
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    // SIZE column at offset 14..23.
    let size_str = format!("{}  ", fmt_qty_col(size));
    let size_x = area.x + 14;
    for (i, ch) in size_str.chars().enumerate() {
        let x = size_x + i as u16;
        if x >= area.x + area.width {
            break;
        }
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_char(ch);
            c.set_style(Style::default().fg(palette.text).bg(row_bg));
        }
    }

    // CUM column at offset 25..34.
    let cum_str = format!("{}  ", fmt_qty_col(cum));
    let cum_x = area.x + 25;
    for (i, ch) in cum_str.chars().enumerate() {
        let x = cum_x + i as u16;
        if x >= area.x + area.width {
            break;
        }
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_char(ch);
            c.set_style(Style::default().fg(palette.muted).bg(row_bg));
        }
    }

    // DEPTH bar at offset 36..end.
    let bar_x = area.x + 36;
    let frac = (size / max_size).clamp(0.0, 1.0);
    let bar_eighths = (frac * (depth_w as f64) * 8.0).round() as usize;
    let full = bar_eighths / 8;
    let partial = bar_eighths % 8;
    for k in 0..full {
        let x = bar_x + k as u16;
        if x >= area.x + area.width {
            break;
        }
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_char(' ');
            c.set_style(Style::default().fg(palette.bg).bg(color));
        }
    }
    if partial > 0 && full < depth_w {
        let x = bar_x + full as u16;
        if x < area.x + area.width {
            let glyph = match partial {
                1 => '▏',
                2 => '▎',
                3 => '▍',
                4 => '▌',
                5 => '▋',
                6 => '▊',
                _ => '▉',
            };
            if let Some(c) = buf.cell_mut((x, area.y)) {
                c.set_char(glyph);
                c.set_style(Style::default().fg(color).bg(row_bg));
            }
        }
    }
}

fn render_trades(frame: &mut Frame, area: Rect, trades: &VecDeque<AggTrade>, palette: &Palette) {
    if area.width < 16 {
        return;
    }
    let header = format!("  {:<8}  {:>10}  {:>9}  S", "TIME", "PRICE", "SIZE");
    render_clipped(
        frame,
        Paragraph::new(header).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    if trades.is_empty() {
        render_clipped(
            frame,
            Paragraph::new("  (waiting for trade stream…)")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: area.x,
                y: area.y + 2,
                width: area.width,
                height: 1,
            },
        );
        return;
    }

    // Newest first, alternating subtle bg for readability.
    let max = (area.height as usize).saturating_sub(1);
    let max_qty = trades
        .iter()
        .map(|t| t.qty)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    for (i, t) in trades.iter().rev().take(max).enumerate() {
        let y = area.y + 1 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let side_char = if t.is_buyer_maker { 'S' } else { 'B' };
        let color = if t.is_buyer_maker { BEAR } else { BULL };
        let big = t.qty / max_qty > 0.7;
        let line = format!(
            "  {:<8}  {}  {}  {}",
            fmt_clock_ms(t.time_ms),
            fmt_price_col(t.price),
            fmt_qty_col(t.qty),
            side_char
        );
        let mut style = Style::default().fg(color).bg(palette.bg);
        if big {
            style = style.add_modifier(Modifier::BOLD);
        }
        render_clipped(
            frame,
            Paragraph::new(line).style(style),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
    }
}
