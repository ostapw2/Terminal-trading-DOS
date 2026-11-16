//! Trading dashboard — F5 view (engine-backed, read-only).

use crate::i18n::pick;
use crate::widgets::fmt::{
    fmt_clock_ms, fmt_money_signed, pnl_color, render_clipped, truncate, SPARKLINE_BARS,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::engine::Runtime;
use crate::gfx::{self, Canvas};
use crate::tokens::Palette;

fn box_block<'a>(title: &'a str, palette: &Palette) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.muted).bg(palette.bg))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg))
}

// ─────────────────  Engine-backed dashboard (view-only F5)  ─────────────────
//
// Reads positions / fills / equity from the trading engine and renders
// them as a read-only dashboard.  No mutation of the runtime happens
// here — the dashboard never executes orders, only displays them.

/// Render the engine-backed F5 dashboard.  Layout:
///
/// ```text
/// +- Trading Dashboard (paper, view-only) ----------------------+
/// | Equity $X   Active +/-$Y (%)   Day +/-$Z (%)   Pos N · Ord M|
/// +- Equity curve --------------------------------------------- +
/// |   <sparkline>                                                |
/// +- Open Positions --------------------------------------------+
/// |   SYM     SIDE  QTY    AVG    MARK   UPNL   %               |
/// +- Active Orders --------------------------------------------- +
/// |   SYM     SIDE  QTY    LIMIT  AGE                            |
/// +- Recent Fills ---------------------------------------------- +
/// |   HH:MM:SS  SYM  SIDE  QTY @ PRICE                           |
/// +-------------------------------------------------------------+
/// ```
/// The account alone, for a small desk window: summary, positions, open
/// orders and recent fills (no equity chart, no balances).
/// The blocks of this page that scroll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Positions,
    Orders,
    Fills,
    Baskets,
}

struct ScrollState {
    off: [usize; 4],
    zones: Vec<(Rect, Zone)>,
}

static SCROLL: std::sync::Mutex<ScrollState> = std::sync::Mutex::new(ScrollState {
    off: [0; 4],
    zones: Vec::new(),
});

fn zone_ix(z: Zone) -> usize {
    match z {
        Zone::Positions => 0,
        Zone::Orders => 1,
        Zone::Fills => 2,
        Zone::Baskets => 3,
    }
}

/// The scrolling block under (x, y) as of the last frame.
pub fn zone_at(x: u16, y: u16) -> Option<Zone> {
    let s = SCROLL.lock().ok()?;
    s.zones
        .iter()
        .rev()
        .find(|(r, _)| r.contains((x, y).into()))
        .map(|(_, z)| *z)
}

/// Move a block's window by `delta` rows (it is clamped when drawn).
pub fn scroll_by(z: Zone, delta: isize) {
    if let Ok(mut s) = SCROLL.lock() {
        let o = &mut s.off[zone_ix(z)];
        *o = (*o as isize + delta).max(0) as usize;
    }
}

/// Forget the zones of the last frame (a page starts drawing).
pub fn begin_zones() {
    if let Ok(mut s) = SCROLL.lock() {
        s.zones.clear();
    }
}

/// The first row to draw of a block of `len` rows showing `rows`: the
/// remembered offset, clamped; the block's rectangle is remembered for the
/// wheel.
pub fn take_offset(z: Zone, area: Rect, len: usize, rows: usize) -> usize {
    let Ok(mut s) = SCROLL.lock() else {
        return 0;
    };
    s.zones.push((area, z));
    let max = len.saturating_sub(rows);
    let o = &mut s.off[zone_ix(z)];
    *o = (*o).min(max);
    *o
}

/// `↑↓ 4-9 / 30` for the corner of a scrolling block (empty when it all fits).
pub fn scroll_hint(off: usize, rows: usize, len: usize) -> String {
    if len <= rows {
        String::new()
    } else {
        format!("↑↓ {}-{} / {len}", off + 1, (off + rows).min(len))
    }
}

/// Draw `text` at the right end of `row`, muted.
pub fn corner(frame: &mut Frame, row: Rect, text: &str, palette: &Palette) {
    if text.is_empty() || row.width as usize <= text.chars().count() + 2 {
        return;
    }
    let w = text.chars().count() as u16;
    render_clipped(
        frame,
        Paragraph::new(text.to_string()).style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: row.x + row.width - w,
            width: w,
            height: 1,
            ..row
        },
    );
}

pub fn render_account(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    begin_zones();
    // Every block gets a share of the height (positions the most) and
    // scrolls when its rows do not fit.
    let has_baskets = !runtime.baskets.baskets.is_empty();
    let baskets_fit = has_baskets;
    let mut constraints = vec![Constraint::Length(4), Constraint::Fill(4)];
    if has_baskets {
        constraints.push(Constraint::Fill(3));
    }
    constraints.extend([Constraint::Fill(2), Constraint::Fill(2)]);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let mut i = 0;
    render_engine_summary(frame, rows[i], runtime, palette);
    i += 1;
    render_engine_positions(frame, rows[i], runtime, palette);
    i += 1;
    if baskets_fit {
        render_baskets_block(frame, rows[i], runtime, palette);
        i += 1;
    }
    render_engine_orders(frame, rows[i], runtime, palette);
    render_engine_fills(frame, rows[i + 1], runtime, palette);
}

fn render_baskets_block(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Baskets", "Кошики"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    crate::widgets::baskets::render(
        frame,
        pad,
        &runtime.baskets,
        &runtime.last_marks,
        crate::engine::runtime::current_now_ms(),
        palette,
    );
}

pub fn render_engine(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    begin_zones();
    let live = runtime.is_live();
    let title = if live {
        pick(
            " Trading Dashboard [LIVE BINANCE] ",
            " Статистика торгів [LIVE BINANCE] ",
        )
    } else {
        pick(
            " Trading Dashboard [PAPER, view-only] ",
            " Статистика торгів [ПАПІР, лише перегляд] ",
        )
    };
    let title_color = if live {
        Color::LightRed
    } else {
        palette.accent
    };
    let border_color = if live { Color::LightRed } else { palette.text };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color).bg(palette.bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(title_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let mut constraints = vec![
        Constraint::Length(4), // header summary: two lines (equity; today, kill switch)
    ];
    let has_balances = !runtime.live_balances.is_empty();
    if has_balances {
        let rows = runtime.live_balances.len().min(10) as u16;
        constraints.push(Constraint::Length(rows + 2)); // header + rows
    }
    let basket_rows = crate::widgets::baskets::rows_wanted(&runtime.baskets, 8) as u16;
    constraints.push(Constraint::Length(7)); // equity curve
    if basket_rows > 0 {
        constraints.push(Constraint::Length(basket_rows + 2)); // baskets
    }
    constraints.extend([
        Constraint::Min(6),    // positions
        Constraint::Length(6), // active orders
        Constraint::Length(7), // recent fills
    ]);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut idx = 0;
    render_engine_summary(frame, rows[idx], runtime, palette);
    idx += 1;
    if has_balances {
        render_engine_balances(frame, rows[idx], runtime, palette);
        idx += 1;
    }
    render_engine_equity(frame, rows[idx], runtime, palette);
    idx += 1;
    if basket_rows > 0 {
        render_baskets_block(frame, rows[idx], runtime, palette);
        idx += 1;
    }
    render_engine_positions(frame, rows[idx], runtime, palette);
    idx += 1;
    render_engine_orders(frame, rows[idx], runtime, palette);
    idx += 1;
    render_engine_fills(frame, rows[idx], runtime, palette);
}

fn render_engine_balances(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Live Balances", "Живі баланси"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let rows: Vec<Line> = runtime
        .live_balances
        .iter()
        .take(10)
        .map(|b| {
            Line::from(vec![
                Span::styled(
                    format!(" {:>6}", b.asset),
                    Style::default()
                        .fg(palette.text)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    pick(" free ", " вільно "),
                    Style::default().fg(palette.muted).bg(palette.bg),
                ),
                Span::styled(
                    format!("{:>12.4}   ", b.free),
                    Style::default().fg(palette.ok).bg(palette.bg),
                ),
                Span::styled(
                    pick("locked ", "заблок. "),
                    Style::default().fg(palette.muted).bg(palette.bg),
                ),
                Span::styled(
                    format!("{:>12.4}", b.locked),
                    Style::default().fg(palette.warn).bg(palette.bg),
                ),
            ])
        })
        .collect();

    if rows.is_empty() {
        render_clipped(
            frame,
            Paragraph::new(pick("(no balances)", "(балансів немає)"))
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            pad,
        );
    } else {
        render_clipped(frame, Paragraph::new(rows), pad);
    }
}

fn render_engine_summary(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Account", "Рахунок"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let equity = runtime.equity();
    let unrealized = runtime.unrealized_total();
    let realized = runtime.realized_total();
    let active_pnl = unrealized; // active = open positions only
    let total_pnl = unrealized + realized;
    let day_pnl = runtime.day_pnl();
    let day_start = runtime.risk_state.day_start_equity;
    let day_pct = if day_start.abs() > 1e-9 {
        day_pnl / day_start * 100.0
    } else {
        0.0
    };

    let active_color = pnl_color(active_pnl);
    let day_color = pnl_color(day_pnl);

    let n_positions = runtime
        .broker
        .all_positions()
        .values()
        .filter(|p| !p.is_flat())
        .count();
    let n_orders = runtime.broker.open_order_count();
    let kill = if runtime.limits.kill_switch {
        "[KILL]"
    } else {
        ""
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mark_age_ms = (now_ms - runtime.last_mark_update_ms).max(0);
    let mark_age_str = if runtime.last_mark_update_ms == 0 {
        pick("no marks", "цін немає").to_string()
    } else if mark_age_ms < 1000 {
        format!("{}ms", mark_age_ms)
    } else if mark_age_ms < 60_000 {
        format!("{:.1}s", mark_age_ms as f64 / 1000.0)
    } else {
        format!("{}m", mark_age_ms / 60_000)
    };
    let mark_color = if mark_age_ms < 5_000 {
        palette.ok
    } else if mark_age_ms < 30_000 {
        palette.warn
    } else {
        palette.error
    };

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    // Line 1: Equity + Active PnL.
    let line1 = vec![
        Span::styled(
            pick("Equity ", "Капітал "),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("${:>10.2}   ", equity),
            Style::default()
                .fg(palette.text)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            pick("Active P&L ", "Активний P&L "),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            fmt_money_signed(active_pnl).to_string(),
            Style::default()
                .fg(active_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "   {} {}",
                pick("Realized", "Зафіксовано"),
                fmt_money_signed(realized)
            ),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!(
                "   {} {}",
                pick("Total", "Разом"),
                fmt_money_signed(total_pnl)
            ),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!(
                "   {} ${:.2}",
                pick("Fees", "Комісії"),
                runtime.broker.fees_paid()
            ),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];

    let line2 = vec![
        Span::styled(
            pick("Today  ", "Сьогодні  "),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            fmt_money_signed(day_pnl).to_string(),
            Style::default()
                .fg(day_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " ({}{:.2}%)   ",
                if day_pnl >= 0.0 { "+" } else { "" },
                day_pct
            ),
            Style::default().fg(day_color).bg(palette.bg),
        ),
        // "Today" and the loss limits count the scalper's own ledger too.
        Span::styled(
            match runtime.scalper.as_ref().filter(|_| !runtime.is_live()) {
                Some(_) => format!(
                    "{} {}   ",
                    pick("incl. scalper", "з боту-скальпера"),
                    fmt_money_signed(runtime.scalper_pnl())
                ),
                None => String::new(),
            },
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            pick(
                format!("Open {} pos · {} orders   ", n_positions, n_orders),
                format!("Відкрито {} поз. · {} ордерів   ", n_positions, n_orders),
            ),
            Style::default().fg(palette.text).bg(palette.bg),
        ),
        Span::styled(
            {
                // Margin of the perpetuals and the open spot positions.
                let used = runtime.broker.margin_used();
                if used > 0.0 {
                    format!(
                        "{} ${:.0} / {} ${:.0}   ",
                        pick("margin", "маржа"),
                        used,
                        pick("free", "вільно"),
                        runtime.free_margin()
                    )
                } else {
                    String::new()
                }
            },
            Style::default().fg(palette.text).bg(palette.bg),
        ),
        Span::styled(
            pick("prices ", "ціни "),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            mark_age_str,
            Style::default()
                .fg(mark_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            kill.to_string(),
            Style::default()
                .fg(palette.error)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    render_clipped(
        frame,
        Paragraph::new(vec![Line::from(line1), Line::from(line2)]),
        pad,
    );
}

fn render_engine_equity(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(
        pick(
            "Equity Curve · 1 col = 5s · ┄ = start of window",
            "Крива капіталу · 1 колонка = 5 с · ┄ = початок вікна",
        ),
        palette,
    );
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    if runtime.equity_history.len() < 2 || inner.width < 3 || inner.height < 2 {
        let pad = Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(4),
            height: 1,
        };
        render_clipped(
            frame,
            Paragraph::new(pick(
                "(awaiting equity samples...)",
                "(чекаю точки капіталу...)",
            ))
            .style(Style::default().fg(palette.muted).bg(palette.bg)),
            pad,
        );
        return;
    }

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let n_visible = (pad.width as usize).min(runtime.equity_history.len());
    let start = runtime.equity_history.len() - n_visible;
    let slice: Vec<f64> = runtime.equity_history.iter().skip(start).copied().collect();

    // Scale includes the baseline (equity at the left edge) and spans at
    // least 0.1% of it, so cents of noise stay a flat line, not a mountain.
    let base = slice[0];
    let hi = slice.iter().cloned().fold(base, f64::max);
    let lo = slice.iter().cloned().fold(base, f64::min);
    let min_span = (base.abs() * 0.001).max(0.01);
    let mid = (hi + lo) / 2.0;
    let half = ((hi - lo) / 2.0).max(min_span / 2.0);
    let (max, min) = (mid + half, mid - half);
    let range = max - min;

    let buf = frame.buffer_mut();
    let last = *slice.last().unwrap_or(&base);
    // In pixels when the terminal shows images: a continuous line.
    if let (Some((pw, ph)), Some((cw, ch))) = (gfx::plot_px(pad), gfx::cell_px()) {
        let mut cv = Canvas::new(pw, ph, palette.bg);
        let muted = cv.ink(palette.muted);
        let ink = cv.ink(if last >= base {
            Color::LightGreen
        } else {
            Color::LightRed
        });
        let y_of = |v: f64| ((max - v) / range * (ph as f64 - 1.0)).round() as i64;
        let hair = (ch as i64 / 16).max(1);
        cv.dashed_hline(y_of(base), hair, cw as usize / 2, cw as usize / 2, muted);
        let off = pad.width as usize - slice.len();
        let x_of = |i: usize| ((off + i) as u32 * cw + cw / 2) as i64;
        let thick = (cw as i64 / 6).max(2);
        for i in 1..slice.len() {
            cv.line(
                x_of(i - 1),
                y_of(slice[i - 1]),
                x_of(i),
                y_of(slice[i]),
                thick,
                ink,
            );
        }
        gfx::submit(buf, pad, cv);
    }
    let pixels = gfx::cell_px().is_some();
    let h_pixels = pad.height as f64 * 8.0;
    let to_pixel = |v: f64| -> u32 {
        ((max - v) / range * (h_pixels - 1.0))
            .round()
            .clamp(0.0, h_pixels - 1.0) as u32
    };

    // Dotted baseline = starting equity of the visible window.
    let base_row = (to_pixel(base) / 8) as u16;
    let cols = if pixels { 0 } else { pad.width };
    for x in pad.x..pad.x + cols {
        if let Some(cell) = buf.cell_mut((x, pad.y + base_row)) {
            cell.set_char('┄');
            cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
        }
    }

    // Right-aligned: the newest sample sits on the right edge.
    let color = if last >= base {
        Color::LightGreen
    } else {
        Color::LightRed
    };
    let x0 = pad.x + pad.width - slice.len() as u16;
    let shown = if pixels { 0 } else { slice.len() };
    for (i, v) in slice.iter().take(shown).enumerate() {
        let p = to_pixel(*v);
        let (row, frac) = ((p / 8) as u16, (p % 8) as usize);
        if let Some(cell) = buf.cell_mut((x0 + i as u16, pad.y + row)) {
            cell.set_char(SPARKLINE_BARS[7 - frac.min(7)]);
            cell.set_style(Style::default().fg(color).bg(palette.bg));
        }
    }

    // Top / bottom scale labels.
    let max_label = format!("${:>10.2}", max);
    let min_label = format!("${:>10.2}", min);
    for (j, ch) in max_label.chars().enumerate() {
        if let Some(cell) = buf.cell_mut((inner.x + j as u16, inner.y)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
        }
    }
    for (j, ch) in min_label.chars().enumerate() {
        if let Some(cell) = buf.cell_mut((inner.x + j as u16, inner.y + inner.height - 1)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
        }
    }
}

fn render_engine_positions(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Open Positions", "Відкриті позиції"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let header = format!(
        "{:<16} {:<5} {:>10} {:>12} {:>12} {:>12} {:>8} {:>5} {:>12}",
        pick("SYMBOL", "СИМВОЛ"),
        pick("SIDE", "БІК"),
        pick("QTY", "К-СТЬ"),
        pick("AVG", "СЕРЕД"),
        pick("MARK", "ЦІНА"),
        "UPNL",
        "%",
        pick("LEV", "ПЛЕЧЕ"),
        pick("LIQ AT", "ЛІКВІД.")
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, pad.width as usize)).style(
            Style::default()
                .fg(palette.accent_dim)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: pad.x,
            y: pad.y,
            width: pad.width,
            height: 1,
        },
    );

    let mut rows: Vec<(String, String, f64, f64, f64, f64, f64)> = runtime
        .broker
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(sym, p)| {
            let mark = runtime.last_marks.get(sym).copied().unwrap_or(p.avg);
            let upnl = p.unrealized(mark);
            let pct = if p.avg > 0.0 {
                (mark - p.avg) / p.avg * 100.0 * p.qty.signum()
            } else {
                0.0
            };
            let side = if p.qty > 0.0 { "LONG" } else { "SHRT" };
            (
                sym.clone(),
                side.into(),
                p.qty.abs(),
                p.avg,
                mark,
                upnl,
                pct,
            )
        })
        .collect();
    rows.sort_by(|a, b| b.5.partial_cmp(&a.5).unwrap_or(std::cmp::Ordering::Equal));

    if rows.is_empty() {
        render_clipped(
            frame,
            Paragraph::new(pick("(no open positions)", "(відкритих позицій немає)"))
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: pad.x,
                y: pad.y + 2,
                width: pad.width,
                height: 1,
            },
        );
        return;
    }

    let visible = pad.height.saturating_sub(1) as usize;
    let off = take_offset(Zone::Positions, area, rows.len(), visible);
    corner(
        frame,
        Rect { height: 1, ..pad },
        &scroll_hint(off, visible, rows.len()),
        palette,
    );
    for (i, (sym, side, qty, avg, mark, upnl, pct)) in rows.iter().enumerate().skip(off) {
        let i = i - off;
        let y = pad.y + 1 + i as u16;
        if y >= pad.y + pad.height {
            break;
        }
        let color = pnl_color(*upnl);
        let side_color = if side == "LONG" {
            Color::LightGreen
        } else {
            Color::LightRed
        };

        // Base line in default colour.
        let side_txt = if side == "LONG" {
            pick("LONG", "ЛОНГ")
        } else {
            pick("SHRT", "ШОРТ")
        };
        let line = format!(
            "{:<16.16} {:<5} {:>10.4} {:>12.4} {:>12.4} {:>12} {:>+7.2}% {:>5} {:>12}",
            sym,
            side_txt,
            qty,
            avg,
            mark,
            fmt_money_signed(*upnl),
            pct,
            if crate::futures::is_perp(sym) {
                format!("x{}", runtime.broker.leverage(sym))
            } else {
                "-".to_string()
            },
            runtime
                .broker
                .liquidation_price(sym)
                .map_or("-".to_string(), |p| format!("{p:.4}"))
        );
        render_clipped(
            frame,
            Paragraph::new(truncate(&line, pad.width as usize))
                .style(Style::default().fg(palette.text).bg(palette.bg)),
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
        );

        // Recolour SIDE column.
        let side_x = pad.x + 17;
        if side_x + 5 <= pad.x + pad.width {
            render_clipped(
                frame,
                Paragraph::new(format!("{:<5}", side)).style(
                    Style::default()
                        .fg(side_color)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect {
                    x: side_x,
                    y,
                    width: 5,
                    height: 1,
                },
            );
        }
        // Recolour UPNL + %.
        // Layout offsets: symbol(10) sp(1) side(5) sp(1) qty(10) sp(1) avg(12) sp(1) mark(12) sp(1) upnl(12) sp(1) pct(8)
        // upnl starts at: 10+1+5+1+10+1+12+1+12+1 = 54
        let upnl_x = pad.x + 60;
        if upnl_x + 21 <= pad.x + pad.width {
            render_clipped(
                frame,
                Paragraph::new(format!("{:>12} {:>+7.2}%", fmt_money_signed(*upnl), pct)).style(
                    Style::default()
                        .fg(color)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect {
                    x: upnl_x,
                    y,
                    width: 21,
                    height: 1,
                },
            );
        }
    }
}

fn render_engine_orders(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Active Orders", "Активні ордери"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let orders = runtime.broker.all_open_orders();
    let header = format!(
        "{:<16} {:<5} {:>10} {:>12} {:>10}",
        pick("SYMBOL", "СИМВОЛ"),
        pick("SIDE", "БІК"),
        pick("QTY", "К-СТЬ"),
        pick("LIMIT", "ЛІМІТ"),
        pick("AGE", "ВІК")
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, pad.width as usize)).style(
            Style::default()
                .fg(palette.accent_dim)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: pad.x,
            y: pad.y,
            width: pad.width,
            height: 1,
        },
    );

    if orders.is_empty() {
        render_clipped(
            frame,
            Paragraph::new(pick(
                "(no active limit orders)",
                "(активних лімітних ордерів немає)",
            ))
            .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: pad.x,
                y: pad.y + 2,
                width: pad.width,
                height: 1,
            },
        );
        return;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let visible = pad.height.saturating_sub(1) as usize;
    let off = take_offset(Zone::Orders, area, orders.len(), visible);
    corner(
        frame,
        Rect { height: 1, ..pad },
        &scroll_hint(off, visible, orders.len()),
        palette,
    );
    for (i, o) in orders.iter().enumerate().skip(off) {
        let i = i - off;
        let y = pad.y + 1 + i as u16;
        if y >= pad.y + pad.height {
            break;
        }
        let side = match o.side {
            crate::strategies::Side::Buy => pick("BUY", "КУП"),
            crate::strategies::Side::Sell => pick("SELL", "ПРОД"),
        };
        let side_color = if matches!(o.side, crate::strategies::Side::Buy) {
            Color::LightGreen
        } else {
            Color::LightRed
        };
        let age_secs = ((now_ms - o.created_ms).max(0) / 1000) as u64;
        let age_str = if age_secs < 60 {
            format!("{}s", age_secs)
        } else if age_secs < 3600 {
            format!("{}m{}s", age_secs / 60, age_secs % 60)
        } else {
            format!("{}h{}m", age_secs / 3600, (age_secs % 3600) / 60)
        };
        let line = format!(
            "{:<16.16} {:<5} {:>10.4} {:>12.4} {:>10}",
            o.symbol, side, o.qty, o.limit, age_str
        );
        render_clipped(
            frame,
            Paragraph::new(truncate(&line, pad.width as usize))
                .style(Style::default().fg(palette.text).bg(palette.bg)),
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
        );
        // Recolour SIDE.
        let side_x = pad.x + 17;
        if side_x + 5 <= pad.x + pad.width {
            render_clipped(
                frame,
                Paragraph::new(format!("{:<5}", side)).style(
                    Style::default()
                        .fg(side_color)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect {
                    x: side_x,
                    y,
                    width: 5,
                    height: 1,
                },
            );
        }
    }
}

fn render_engine_fills(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block(pick("Recent Fills", "Останні виконання"), palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let header = format!(
        "{:<10} {:<16} {:<5} {:>10} {:>12}",
        pick("TIME", "ЧАС"),
        pick("SYMBOL", "СИМВОЛ"),
        pick("SIDE", "БІК"),
        pick("QTY", "К-СТЬ"),
        pick("PRICE", "ЦІНА")
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, pad.width as usize)).style(
            Style::default()
                .fg(palette.accent_dim)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: pad.x,
            y: pad.y,
            width: pad.width,
            height: 1,
        },
    );

    if runtime.fill_log.is_empty() {
        render_clipped(
            frame,
            Paragraph::new(pick("(no fills yet)", "(виконань ще немає)"))
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: pad.x,
                y: pad.y + 2,
                width: pad.width,
                height: 1,
            },
        );
        return;
    }

    let max_rows = pad.height.saturating_sub(1) as usize;
    let off = take_offset(Zone::Fills, area, runtime.fill_log.len(), max_rows);
    corner(
        frame,
        Rect { height: 1, ..pad },
        &scroll_hint(off, max_rows, runtime.fill_log.len()),
        palette,
    );
    for (i, (ts, sym, fill)) in runtime
        .fill_log
        .iter()
        .rev()
        .skip(off)
        .take(max_rows)
        .enumerate()
    {
        let y = pad.y + 1 + i as u16;
        if y >= pad.y + pad.height {
            break;
        }
        let side = match fill.side {
            crate::strategies::Side::Buy => pick("BUY", "КУП"),
            crate::strategies::Side::Sell => pick("SELL", "ПРОД"),
        };
        let side_color = if matches!(fill.side, crate::strategies::Side::Buy) {
            Color::LightGreen
        } else {
            Color::LightRed
        };
        let line = format!(
            "{:<10} {:<16.16} {:<5} {:>10.4} {:>12.4}",
            fmt_clock_ms(*ts),
            sym,
            side,
            fill.qty,
            fill.price
        );
        render_clipped(
            frame,
            Paragraph::new(truncate(&line, pad.width as usize))
                .style(Style::default().fg(palette.text).bg(palette.bg)),
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
        );
        // Recolour SIDE.
        let side_x = pad.x + 28;
        if side_x + 5 <= pad.x + pad.width {
            render_clipped(
                frame,
                Paragraph::new(format!("{:<5}", side)).style(
                    Style::default()
                        .fg(side_color)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect {
                    x: side_x,
                    y,
                    width: 5,
                    height: 1,
                },
            );
        }
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn the_hint_names_the_window_and_stays_quiet_when_all_fits() {
        assert_eq!(scroll_hint(0, 10, 8), "");
        assert_eq!(scroll_hint(0, 10, 10), "");
        assert_eq!(scroll_hint(0, 5, 30), "↑↓ 1-5 / 30");
        assert_eq!(scroll_hint(25, 5, 30), "↑↓ 26-30 / 30");
    }
}
