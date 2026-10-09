//! Trading dashboard — F5 view (engine-backed, read-only).

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
pub fn render_engine(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let live = runtime.is_live();
    let title = if live {
        " Trading Dashboard [LIVE BINANCE] "
    } else {
        " Trading Dashboard [PAPER, view-only] "
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
        Constraint::Length(3), // header summary
    ];
    let has_balances = !runtime.live_balances.is_empty();
    if has_balances {
        let rows = runtime.live_balances.len().min(10) as u16;
        constraints.push(Constraint::Length(rows + 2)); // header + rows
    }
    constraints.extend([
        Constraint::Length(7), // equity curve
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
    render_engine_positions(frame, rows[idx], runtime, palette);
    idx += 1;
    render_engine_orders(frame, rows[idx], runtime, palette);
    idx += 1;
    render_engine_fills(frame, rows[idx], runtime, palette);
}

fn render_engine_balances(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block("Live Balances", palette);
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
                Span::styled(" free ", Style::default().fg(palette.muted).bg(palette.bg)),
                Span::styled(
                    format!("{:>12.4}   ", b.free),
                    Style::default().fg(palette.ok).bg(palette.bg),
                ),
                Span::styled("locked ", Style::default().fg(palette.muted).bg(palette.bg)),
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
            Paragraph::new("(no balances)")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            pad,
        );
    } else {
        render_clipped(frame, Paragraph::new(rows), pad);
    }
}

fn render_engine_summary(frame: &mut Frame, area: Rect, runtime: &Runtime, palette: &Palette) {
    let block = box_block("Account", palette);
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
        "no marks".to_string()
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
        Span::styled("Equity ", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            format!("${:>10.2}   ", equity),
            Style::default()
                .fg(palette.text)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Active P&L ",
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
            format!("   Realized {}", fmt_money_signed(realized)),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("   Total {}", fmt_money_signed(total_pnl)),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ];

    let line2 = vec![
        Span::styled("Today  ", Style::default().fg(palette.muted).bg(palette.bg)),
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
        Span::styled(
            format!("Open {} pos · {} orders   ", n_positions, n_orders),
            Style::default().fg(palette.text).bg(palette.bg),
        ),
        Span::styled("marks ", Style::default().fg(palette.muted).bg(palette.bg)),
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
    let block = box_block("Equity Curve", palette);
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
            Paragraph::new("(awaiting equity samples...)")
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

    let max = slice.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = slice.iter().cloned().fold(f64::INFINITY, f64::min);
    let range = (max - min).max(1e-9);

    let buf = frame.buffer_mut();
    let h_pixels = pad.height as f64 * 8.0;

    for (i, v) in slice.iter().enumerate() {
        let x = pad.x + i as u16;
        let t = (max - v) / range;
        let p = (t * (h_pixels - 1.0)).round().clamp(0.0, h_pixels - 1.0) as u32;
        let row = (p / 8) as u16;
        let frac = (p % 8) as usize;

        let prev = if i > 0 { slice[i - 1] } else { *v };
        let up = *v >= prev;
        let color = if up {
            Color::LightGreen
        } else {
            Color::LightRed
        };
        let solid = Style::default().bg(color).fg(palette.bg);
        let bot_rows = pad.height - 1 - row;
        for j in 0..bot_rows {
            let y = pad.y + pad.height - 1 - j;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(solid);
            }
        }
        let top_y = pad.y + row;
        if top_y < pad.y + pad.height {
            let ch = SPARKLINE_BARS[7 - frac.min(7)];
            if let Some(cell) = buf.cell_mut((x, top_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(color).bg(palette.bg));
            }
        }
    }

    // Min / max labels.
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
    let block = box_block("Open Positions", palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let header = format!(
        "{:<10} {:<5} {:>10} {:>12} {:>12} {:>12} {:>8}",
        "SYMBOL", "SIDE", "QTY", "AVG", "MARK", "UPNL", "%"
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
            Paragraph::new("(no open positions — pick pairs in F4 to start)")
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

    for (i, (sym, side, qty, avg, mark, upnl, pct)) in rows.iter().enumerate() {
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
        let line = format!(
            "{:<10} {:<5} {:>10.4} {:>12.4} {:>12.4} {:>12} {:>+7.2}%",
            sym,
            side,
            qty,
            avg,
            mark,
            fmt_money_signed(*upnl),
            pct
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
        let side_x = pad.x + 11;
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
        let upnl_x = pad.x + 54;
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
    let block = box_block("Active Orders", palette);
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
        "{:<10} {:<5} {:>10} {:>12} {:>10}",
        "SYMBOL", "SIDE", "QTY", "LIMIT", "AGE"
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
            Paragraph::new("(no active limit orders)")
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
    for (i, o) in orders.iter().enumerate() {
        let y = pad.y + 1 + i as u16;
        if y >= pad.y + pad.height {
            break;
        }
        let side = match o.side {
            crate::strategies::Side::Buy => "BUY",
            crate::strategies::Side::Sell => "SELL",
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
            "{:<10} {:<5} {:>10.4} {:>12.4} {:>10}",
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
        let side_x = pad.x + 11;
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
    let block = box_block("Recent Fills", palette);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let header = format!(
        "{:<10} {:<10} {:<5} {:>10} {:>12}",
        "TIME", "SYMBOL", "SIDE", "QTY", "PRICE"
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
            Paragraph::new("(no fills yet)")
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
    for (i, (ts, sym, fill)) in runtime.fill_log.iter().rev().take(max_rows).enumerate() {
        let y = pad.y + 1 + i as u16;
        if y >= pad.y + pad.height {
            break;
        }
        let side = match fill.side {
            crate::strategies::Side::Buy => "BUY",
            crate::strategies::Side::Sell => "SELL",
        };
        let side_color = if matches!(fill.side, crate::strategies::Side::Buy) {
            Color::LightGreen
        } else {
            Color::LightRed
        };
        let line = format!(
            "{:<10} {:<10} {:<5} {:>10.4} {:>12.4}",
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
        let side_x = pad.x + 22;
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
