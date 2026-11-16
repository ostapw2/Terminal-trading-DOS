//! F6 — Order book (depth-of-market) view: bids + asks at price levels +
//! a live trades feed on the right.  Real-time data via Binance WebSocket
//! `@depth20@100ms` + `@aggTrade`.
//!
//! Layout: title bar with mid+spread+zoom, imbalance gauge row, then a
//! 60/40 horizontal split — book on the left (asks top, spread row,
//! bids bottom) and recent trades feed on the right.

use crate::i18n::pick;
use crate::widgets::fmt::{
    fmt_clock_ms, fmt_price_col, fmt_qty_col, render_clipped, truncate, BEAR, BULL,
};
use std::collections::VecDeque;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::binance::AggTrade;
use crate::data::orderbook::{OrderBookSnapshot, WallMark};
use crate::gfx::{self, Canvas};
use crate::tokens::Palette;

pub struct OrderBookLayout {
    pub area: Rect,
    /// The ladder's rows: the row, whether it is a bid, its price (the
    /// bucket's), so a click can rest a limit there.
    pub rows: Vec<(Rect, bool, f64)>,
    /// Price per row at the current zoom (half of it is "the same price").
    pub bucket: f64,
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
    /// Binance REST paused (refused or near its limit): seconds left.  The
    /// book needs one REST snapshot to start.
    pub paused_secs: Option<i64>,
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
    walls: &[WallMark],
    orders: &[(bool, f64, f64)],
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
    render_walls_line(frame, rows[0], snapshot, walls, palette);

    let (ladder, bucket) = if show_trades {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(rows[1]);
        let l = render_book(
            frame,
            cols[0],
            snapshot,
            zoom,
            last_trade_at_price,
            diag,
            walls,
            orders,
            palette,
        );
        render_trades(frame, cols[1], recent_trades, palette);
        l
    } else {
        render_book(
            frame,
            rows[1],
            snapshot,
            zoom,
            last_trade_at_price,
            diag,
            walls,
            orders,
            palette,
        )
    };

    OrderBookLayout {
        area,
        rows: ladder,
        bucket,
    }
}

/// Is one of our orders resting at this bucket's price?
fn own_order_at(orders: &[(bool, f64, f64)], price: f64, bucket: f64) -> bool {
    orders
        .iter()
        .any(|(_, p, _)| (p - price).abs() < bucket / 2.0)
}

/// `◆` in the row's first cell: one of our orders rests here.
fn mark_own_order(frame: &mut Frame, row: Rect, palette: &Palette) {
    if let Some(c) = frame.buffer_mut().cell_mut((row.x, row.y)) {
        c.set_char('◆');
        c.set_style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn build_title(
    ticker: &str,
    snap: Option<&OrderBookSnapshot>,
    zoom: u32,
    stale_secs: u64,
) -> String {
    let stale_tag = if stale_secs > 3 {
        pick(
            format!("  ⚠ STALLED {}s", stale_secs),
            format!("  ⚠ ЗАВИС {} с", stale_secs),
        )
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
                pick(
                    format!(
                        " Order Book — {}    Mid {:.4}    Spread {:.4} ({:.1} bp)    zoom x{}{} ",
                        ticker, mid, spread, bp, zoom, stale_tag
                    ),
                    format!(
                        " Стакан — {}    Mid {:.4}    Спред {:.4} ({:.1} б.п.)    масштаб x{}{} ",
                        ticker, mid, spread, bp, zoom, stale_tag
                    ),
                )
            }
            _ => pick(
                format!(" Order Book — {}    zoom x{}{} ", ticker, zoom, stale_tag),
                format!(" Стакан — {}    масштаб x{}{} ", ticker, zoom, stale_tag),
            ),
        },
        None => pick(
            format!(
                " Order Book — {} (loading…)    zoom x{}{} ",
                ticker, zoom, stale_tag
            ),
            format!(
                " Стакан — {} (завантаження…)    масштаб x{}{} ",
                ticker, zoom, stale_tag
            ),
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
    let bid_label = format!(
        " {} {}% ({}) ",
        pick("Bid", "Купівля"),
        bid_pct,
        fmt_qty_col(bid_total).trim()
    );
    let ask_label = format!(
        " {} {}% ({}) ",
        pick("Ask", "Продаж"),
        ask_pct,
        fmt_qty_col(ask_total).trim()
    );

    // In pixels the split is exact; the labels stay text on top.
    let bar_rect = Rect {
        x: area.x,
        y: bar_y,
        width: bar_w,
        height: 1,
    };
    let style_at = |x: u16| {
        Style::default()
            .fg(palette.bg)
            .bg(if x < bid_cells { BULL } else { BEAR })
            .add_modifier(Modifier::BOLD)
    };
    if let Some((pw, ph)) = gfx::plot_px(bar_rect) {
        let mut cv = Canvas::new(pw, ph, palette.bg);
        let (bull, bear) = (cv.ink(BULL), cv.ink(BEAR));
        let split = (pw as f64 * (bid_total / total)).round() as i64;
        cv.fill(0, 0, split, ph as i64, bull);
        cv.fill(split, 0, pw as i64, ph as i64, bear);
        gfx::submit(buf, bar_rect, cv);
    } else {
        for x in 0..bar_w {
            if let Some(c) = buf.cell_mut((area.x + x, bar_y)) {
                c.set_char(' ');
                c.set_style(style_at(x));
            }
        }
    }
    // Labels: bid left-anchored, ask right-anchored (text over the image).
    let ask_w = ask_label.chars().count() as u16;
    let ask_start = bar_w.saturating_sub(ask_w + 1);
    let labels = bid_label
        .chars()
        .enumerate()
        .map(|(i, ch)| (1 + i as u16, ch))
        .chain(
            ask_label
                .chars()
                .enumerate()
                .map(|(i, ch)| (ask_start + i as u16, ch)),
        );
    for (dx, ch) in labels {
        if dx >= bar_w {
            continue;
        }
        if let Some(c) = buf.cell_mut((area.x + dx, bar_y)) {
            c.set_char(ch);
            c.set_style(style_at(dx));
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
    walls: &[WallMark],
    orders: &[(bool, f64, f64)],
    palette: &Palette,
) -> (Vec<(Rect, bool, f64)>, f64) {
    let mut ladder: Vec<(Rect, bool, f64)> = Vec::new();
    if area.width < 30 || area.height < 6 {
        return (ladder, 0.0);
    }
    let header = format!(
        "  {:>10}  {:>9}  {:>9}  {}",
        pick("PRICE", "ЦІНА"),
        pick("SIZE", "ОБСЯГ"),
        pick("CUM", "РАЗОМ"),
        pick("DEPTH", "ГЛИБИНА")
    );
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
            let line1 = match diag.paused_secs {
                Some(s) => pick(
                    format!("  Binance limits requests: the book starts in {s} s"),
                    format!("  Binance обмежує запити: стакан почнеться за {s} с"),
                ),
                None => pick(
                    "  (waiting for the depth stream…)",
                    "  (чекаю потік глибини…)",
                )
                .to_string(),
            };
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
            return (ladder, 0.0);
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
    // Depth bars in pixels when the terminal shows images.
    let pixels = gfx::cell_px().is_some() && area.width > 40;
    let mut ask_rows: Vec<DepthRow> = Vec::new();
    let mut bid_rows: Vec<DepthRow> = Vec::new();
    let mut ask_walls: Vec<(u16, WallMark)> = Vec::new();
    let mut bid_walls: Vec<(u16, WallMark)> = Vec::new();

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
        let wall = wall_at(walls, false, price, bucket);
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
            wall,
            palette,
            pixels,
        );
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        if own_order_at(orders, price, bucket) {
            mark_own_order(frame, row, palette);
        }
        ladder.push((row, false, price));
        ask_rows.push(DepthRow {
            y,
            frac: (size / max_ask_size).clamp(0.0, 1.0),
            color: BEAR,
            flash,
            wall: wall.is_some(),
        });
        if let Some(m) = wall {
            ask_walls.push((y, m));
        }
        y += 1;
    }
    if pixels {
        depth_image(frame, area, depth_w, &ask_rows, palette);
    }
    paint_wall_labels(frame, area, &ask_rows, &ask_walls);

    // Spread row.
    if y < area.y + area.height {
        let spread_str = match (snap.best_bid(), snap.best_ask()) {
            (Some(b), Some(a)) => format!(
                "  ─── {} / {}   {} {:.6}   tick {:.6} (x{}) ───",
                fmt_price_col(b),
                fmt_price_col(a),
                pick("spread", "спред"),
                a - b,
                tick,
                zoom
            ),
            _ => pick("  ─── (no top bid/ask) ───", "  ─── (немає кращих цін) ───").into(),
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
        let wall = wall_at(walls, true, *price, bucket);
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
            wall,
            palette,
            pixels,
        );
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        if own_order_at(orders, *price, bucket) {
            mark_own_order(frame, row, palette);
        }
        ladder.push((row, true, *price));
        bid_rows.push(DepthRow {
            y,
            frac: (size / max_bid_size).clamp(0.0, 1.0),
            color: BULL,
            flash,
            wall: wall.is_some(),
        });
        if let Some(m) = wall {
            bid_walls.push((y, m));
        }
        y += 1;
    }
    if pixels {
        depth_image(frame, area, depth_w, &bid_rows, palette);
    }
    paint_wall_labels(frame, area, &bid_rows, &bid_walls);
    (ladder, bucket)
}

/// One book row's depth bar, for the pixel version.
struct DepthRow {
    y: u16,
    frac: f64,
    color: Color,
    flash: bool,
    /// A wall: the whole row is tinted in the side's darker colour.
    wall: bool,
}

/// The wall's mark at the right end of its row: how many times the usual
/// size, how long it has stood.  Painted after the depth image (on top).
fn paint_wall_labels(frame: &mut Frame, area: Rect, rows: &[DepthRow], walls: &[(u16, WallMark)]) {
    let buf = frame.buffer_mut();
    for (y, m) in walls {
        let color = rows
            .iter()
            .find(|r| r.y == *y)
            .map_or(if m.wall.bid { BULL } else { BEAR }, |r| r.color);
        let label = format!(" ◆ x{:.1} {} ", m.wall.ratio, fmt_age(m.age_ms));
        let w = label.chars().count() as u16;
        if area.width < 36 + w {
            continue;
        }
        let x0 = area.x + area.width - w;
        for (i, ch) in label.chars().enumerate() {
            if let Some(c) = buf.cell_mut((x0 + i as u16, *y)) {
                c.set_char(ch);
                c.set_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

/// The wall standing at this row's price, if any.
fn wall_at(walls: &[WallMark], bid: bool, price: f64, bucket: f64) -> Option<WallMark> {
    walls
        .iter()
        .find(|m| m.wall.bid == bid && (m.wall.price - price).abs() < bucket / 2.0)
        .copied()
}

/// How long a wall has stood: `12s`, `4m`, `2h`.
pub fn fmt_age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// The second header row: the strongest wall of each side, in words.
fn render_walls_line(
    frame: &mut Frame,
    area: Rect,
    snap: Option<&OrderBookSnapshot>,
    walls: &[WallMark],
    palette: &Palette,
) {
    if area.height < 2 || area.width < 20 {
        return;
    }
    let mid = snap.and_then(|s| s.mid());
    let side = |bid: bool| -> Option<String> {
        let m = walls
            .iter()
            .filter(|m| m.wall.bid == bid)
            .max_by(|a, b| a.wall.ratio.total_cmp(&b.wall.ratio))?;
        let dist = mid
            .filter(|m| *m > 0.0)
            .map(|mid| format!(" {:+.2}%", (m.wall.price / mid - 1.0) * 100.0))
            .unwrap_or_default();
        Some(format!(
            "{} {} x{:.1}{dist} {}",
            if bid {
                pick("bid", "куп.")
            } else {
                pick("ask", "прод.")
            },
            fmt_price_col(m.wall.price).trim(),
            m.wall.ratio,
            fmt_age(m.age_ms)
        ))
    };
    let parts: Vec<String> = [side(false), side(true)].into_iter().flatten().collect();
    let (text, style) = if parts.is_empty() {
        (
            pick(
                format!(
                    " no walls (a level {}x the usual size or more)",
                    crate::data::orderbook::WALL_RATIO
                ),
                format!(
                    " стін немає (рівень у {}x звичайного обсягу й більше)",
                    crate::data::orderbook::WALL_RATIO
                ),
            ),
            Style::default().fg(palette.muted).bg(palette.bg),
        )
    } else {
        (
            format!(" ◆ {}: {}", pick("walls", "стіни"), parts.join("   ")),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        )
    };
    render_clipped(
        frame,
        Paragraph::new(Span::styled(
            truncate(&text, area.width as usize).to_string(),
            style,
        )),
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
    );
}

/// The DEPTH column of a block of consecutive rows as one image: bars to the
/// exact pixel, a hairline gap between rows, the last-trade row tinted.
fn depth_image(
    frame: &mut Frame,
    area: Rect,
    depth_w: usize,
    rows: &[DepthRow],
    palette: &Palette,
) {
    let x = area.x + 36;
    let width = (depth_w as u16).min((area.x + area.width).saturating_sub(x));
    bar_image(frame, x, width, rows, palette);
}

/// Horizontal bars for consecutive rows as one image, `width` cells wide at
/// column `x`: exact to the pixel, a hairline gap between rows, tinted rows.
fn bar_image(frame: &mut Frame, x: u16, width: u16, rows: &[DepthRow], palette: &Palette) {
    let (Some(first), Some(last)) = (
        rows.iter().map(|r| r.y).min(),
        rows.iter().map(|r| r.y).max(),
    ) else {
        return;
    };
    let rect = Rect {
        x,
        y: first,
        width,
        height: last - first + 1,
    };
    let (Some((pw, _)), Some((_, ch))) = (gfx::plot_px(rect), gfx::cell_px()) else {
        return;
    };
    let ch = ch as i64;
    let mut cv = Canvas::new(pw, rect.height as usize * ch as usize, palette.bg);
    let tint = cv.ink(Color::Yellow);
    let pad = (ch / 10).max(1);
    for r in rows {
        let top = (r.y - first) as i64 * ch;
        if r.flash {
            cv.fill(0, top, pw as i64, top + ch, tint);
        } else if r.wall {
            let dark = cv.ink(if r.color == BULL {
                Color::Green
            } else {
                Color::Red
            });
            cv.fill(0, top, pw as i64, top + ch, dark);
        }
        let ink = cv.ink(r.color);
        let w = (r.frac * pw as f64).round() as i64;
        cv.fill(0, top + pad, w, top + ch - pad, ink);
    }
    gfx::submit(frame.buffer_mut(), rect, cv);
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
    wall: Option<WallMark>,
    palette: &Palette,
    pixels: bool,
) {
    let buf = frame.buffer_mut();
    // Background tint when this row contains the most recent trade; a wall
    // is solid in its side's colour (it should catch the eye).
    let row_bg = if flash {
        ratatui::style::Color::Yellow
    } else if wall.is_some() {
        color
    } else {
        palette.bg
    };
    let ink = |normal: Color| {
        if wall.is_some() && !flash {
            Color::Black
        } else {
            normal
        }
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
                    .fg(if flash { palette.bg } else { ink(color) })
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
            let st = Style::default().fg(ink(palette.text)).bg(row_bg);
            c.set_style(if wall.is_some() {
                st.add_modifier(Modifier::BOLD)
            } else {
                st
            });
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
            c.set_style(Style::default().fg(ink(palette.muted)).bg(row_bg));
        }
    }

    // DEPTH bar at offset 36..end (drawn as an image when `pixels`).
    if pixels {
        return;
    }
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
    let header = format!(
        "  {:<8}  {:>10}  {:>9}  S",
        pick("TIME", "ЧАС"),
        pick("PRICE", "ЦІНА"),
        pick("SIZE", "ОБСЯГ")
    );
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
            Paragraph::new(pick("  (waiting for trades…)", "  (чекаю угоди…)"))
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

/// The tape page: time & sales of one symbol on the whole window, newest
/// first, with the last minute's buy / sell balance on top and each trade's
/// size as a bar (pixels when the terminal shows images).
pub fn render_tape(
    frame: &mut Frame,
    area: Rect,
    ticker: &str,
    trades: &VecDeque<AggTrade>,
    palette: &Palette,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(" {} - {ticker} ", pick("Time & Sales", "Стрічка угод")),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if inner.width < 30 || inner.height < 5 {
        return;
    }
    let line = |y: u16| Rect {
        x: inner.x,
        y,
        width: inner.width,
        height: 1,
    };
    let Some(newest) = trades.back().map(|t| t.time_ms) else {
        render_clipped(
            frame,
            Paragraph::new(pick("  (waiting for trades…)", "  (чекаю угоди…)"))
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            line(inner.y + 1),
        );
        return;
    };

    // The last minute: who was aggressive.
    let (mut buy_q, mut sell_q, mut buy_usd, mut sell_usd, mut n) = (0.0, 0.0, 0.0, 0.0, 0);
    for t in trades.iter().filter(|t| t.time_ms > newest - 60_000) {
        n += 1;
        if t.is_buyer_maker {
            sell_q += t.qty;
            sell_usd += t.qty * t.price;
        } else {
            buy_q += t.qty;
            buy_usd += t.qty * t.price;
        }
    }
    let delta = buy_usd - sell_usd;
    let summary = format!(
        " 60{}  {} {} (${:.0})   {} {} (${:.0})   delta {}${:.0}   {} {}",
        pick("s", "с"),
        pick("buys", "купівлі"),
        fmt_qty_col(buy_q).trim(),
        buy_usd,
        pick("sells", "продажі"),
        fmt_qty_col(sell_q).trim(),
        sell_usd,
        if delta >= 0.0 { "+" } else { "-" },
        delta.abs(),
        n,
        pick("trades", "угод")
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&summary, inner.width as usize).to_string()).style(
            Style::default()
                .fg(if delta >= 0.0 { BULL } else { BEAR })
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        line(inner.y),
    );
    // Buy share of the minute as a two-colour gauge.
    let gauge = line(inner.y + 1);
    let total = (buy_usd + sell_usd).max(1e-9);
    if let Some((pw, ph)) = gfx::plot_px(gauge) {
        let mut cv = Canvas::new(pw, ph, palette.bg);
        let (bull, bear) = (cv.ink(BULL), cv.ink(BEAR));
        let pad = (ph as i64 / 4).max(1);
        let split = (pw as f64 * buy_usd / total).round() as i64;
        cv.fill(0, pad, split, ph as i64 - pad, bull);
        cv.fill(split, pad, pw as i64, ph as i64 - pad, bear);
        gfx::submit(frame.buffer_mut(), gauge, cv);
    } else {
        let split = (gauge.width as f64 * buy_usd / total).round() as u16;
        let buf = frame.buffer_mut();
        for dx in 0..gauge.width {
            if let Some(c) = buf.cell_mut((gauge.x + dx, gauge.y)) {
                c.set_char(' ');
                c.set_style(Style::default().bg(if dx < split { BULL } else { BEAR }));
            }
        }
    }

    // Columns: time, price, size, USD, side, then the size bar.
    const TEXT_W: u16 = 50;
    let header = format!(
        "  {:<8}  {:>10}  {:>9}  {:>10}  S",
        pick("TIME", "ЧАС"),
        pick("PRICE", "ЦІНА"),
        pick("SIZE", "ОБСЯГ"),
        "USD"
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, inner.width as usize).to_string()).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        line(inner.y + 2),
    );
    let top = inner.y + 3;
    let rows_n = (inner.y + inner.height).saturating_sub(top) as usize;
    let shown: Vec<&AggTrade> = trades.iter().rev().take(rows_n).collect();
    let max_usd = shown
        .iter()
        .map(|t| t.qty * t.price)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let bar_x = inner.x + TEXT_W.min(inner.width);
    let bar_w = (inner.x + inner.width).saturating_sub(bar_x + 1);
    let pixels = gfx::cell_px().is_some() && bar_w > 2;
    let mut bars: Vec<DepthRow> = Vec::new();
    for (i, t) in shown.iter().enumerate() {
        let y = top + i as u16;
        let usd = t.qty * t.price;
        let color = if t.is_buyer_maker { BEAR } else { BULL };
        let big = usd / max_usd > 0.7;
        let text = format!(
            "  {:<8}  {}  {}  {:>10.0}  {}",
            fmt_clock_ms(t.time_ms),
            fmt_price_col(t.price),
            fmt_qty_col(t.qty),
            usd,
            if t.is_buyer_maker { 'S' } else { 'B' }
        );
        let mut style = Style::default().fg(color).bg(palette.bg);
        if big {
            style = style.add_modifier(Modifier::BOLD);
        }
        render_clipped(
            frame,
            Paragraph::new(truncate(&text, TEXT_W.min(inner.width) as usize).to_string())
                .style(style),
            Rect {
                width: TEXT_W.min(inner.width),
                ..line(y)
            },
        );
        let frac = (usd / max_usd).clamp(0.0, 1.0);
        if pixels {
            bars.push(DepthRow {
                y,
                frac,
                color,
                flash: false,
                wall: false,
            });
        } else if bar_w > 0 {
            let n = (frac * bar_w as f64).round() as u16;
            let buf = frame.buffer_mut();
            for dx in 0..n {
                if let Some(c) = buf.cell_mut((bar_x + dx, y)) {
                    c.set_char(' ');
                    c.set_style(Style::default().bg(color));
                }
            }
        }
    }
    if pixels {
        bar_image(frame, bar_x, bar_w, &bars, palette);
    }
}
