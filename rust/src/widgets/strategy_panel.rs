//! Right-hand selection list under the trade ticket on F4.
//!
//! Compact one-line-per-slot view — every checkbox-selected pair lives
//! here.  Click a row to make it the active ticket.  Each row exposes
//! per-slot action buttons: `[Rn]` start, `[Sp]` stop, `[Ed]` cycle
//! strategy, `[X ]` remove.

use crate::widgets::fmt::{fmt_price, fmt_qty, render_clipped, truncate};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::engine::broker::Broker;
use crate::engine::{Runtime, SlotStatus};
use crate::tokens::Palette;

/// Clickable rectangles for one slot row.
#[derive(Clone, Debug)]
pub struct SlotRects {
    /// Slot identity.  Hit-tests run against the previous frame's layout,
    /// so the slot list may have changed since: resolve by symbol, never
    /// by index (audit A1).
    pub symbol: String,
    /// Click anywhere in the row body (outside the action buttons) to
    /// make this slot active in the trade ticket.
    pub body: Rect,
    pub start: Rect,
    pub stop: Rect,
    pub edit: Rect,
    pub remove: Rect,
}

#[derive(Clone, Debug, Default)]
pub struct StrategyPanelLayout {
    pub area: Rect,
    pub slots: Vec<SlotRects>,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    runtime: &Runtime,
    active_slot: Option<usize>,
    palette: &Palette,
) -> StrategyPanelLayout {
    let mut layout = StrategyPanelLayout {
        area,
        ..Default::default()
    };

    let ws_age_ms = if runtime.last_mark_update_ms > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        (now_ms - runtime.last_mark_update_ms).max(0)
    } else {
        i64::MAX
    };
    let ticks_label = if runtime.ws_tick_count >= 1_000 {
        format!("{:.1}K", runtime.ws_tick_count as f64 / 1000.0)
    } else {
        format!("{}", runtime.ws_tick_count)
    };
    let ws_label = if runtime.ws_connected && ws_age_ms < 60_000 {
        if ws_age_ms < 1000 {
            format!("WS:{}ms ({})", ws_age_ms.max(1), ticks_label)
        } else {
            format!("WS:{:.1}s ({})", ws_age_ms as f64 / 1000.0, ticks_label)
        }
    } else if runtime.ws_connected {
        format!("WS:stalled ({})", ticks_label)
    } else if !runtime.ws_status.is_empty() {
        format!("WS:{}", runtime.ws_status)
    } else {
        "WS:off".into()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(
                " Selection ({}) - Equity ${:.2} - {} {} ",
                runtime.slots.len(),
                runtime.equity(),
                ws_label,
                kill_label(runtime),
            ),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    // Header row — matches the per-row column widths above.
    let header = format!(
        "{:<7} {:>10} {:>3} {:>6} {:<13}    ACT",
        "PAIR", "PRICE", "STAT", "PNL", "POS@AVG"
    );
    if inner.height >= 1 {
        render_clipped(
            frame,
            Paragraph::new(truncate(&header, inner.width as usize)).style(
                Style::default()
                    .fg(palette.accent_dim)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect {
                x: inner.x + 1,
                y: inner.y,
                width: inner.width.saturating_sub(2),
                height: 1,
            },
        );
    }

    if runtime.slots.is_empty() && inner.height >= 3 {
        render_clipped(
            frame,
            Paragraph::new("(no pairs selected — tick [X] in the screener on the left)")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: inner.x + 2,
                y: inner.y + 2,
                width: inner.width.saturating_sub(4),
                height: 1,
            },
        );
        return layout;
    }

    let body_top = inner.y + 1;
    let body_bot = inner.y + inner.height;
    let max_visible = body_bot.saturating_sub(body_top) as usize;
    for (i, slot) in runtime.slots.iter().enumerate().take(max_visible) {
        let y = body_top + i as u16;
        if y >= body_bot {
            break;
        }
        let row = render_slot_row(
            frame,
            Rect {
                x: inner.x + 1,
                y,
                width: inner.width.saturating_sub(2),
                height: 1,
            },
            slot,
            runtime,
            active_slot == Some(i),
            palette,
        );
        layout.slots.push(row);
    }

    layout
}

fn render_slot_row(
    frame: &mut Frame,
    area: Rect,
    slot: &crate::engine::Slot,
    runtime: &Runtime,
    active: bool,
    palette: &Palette,
) -> SlotRects {
    let bg = if active {
        palette.cursor_bg
    } else {
        palette.bg
    };
    let fg = if active {
        palette.cursor_fg
    } else {
        palette.text
    };

    if active {
        fill_bg(frame, area, bg);
    }

    let last = runtime.last_mark(&slot.symbol).unwrap_or(0.0);
    let pos = runtime.broker.position(&slot.symbol);
    let pnl = pos.realized + pos.unrealized(last);
    // Flash the PRICE cell briefly on every fresh tick so the user
    // sees movement even when the displayed price barely changes
    // (thin alts where mid drifts by sub-tick fractions).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let tick_age_ms = runtime
        .last_tick_per_symbol
        .get(&slot.symbol)
        .map(|(_, ts)| (now_ms - ts).max(0))
        .unwrap_or(i64::MAX);
    let flash = tick_age_ms < 250;
    let pnl_color = if pnl > 1e-9 {
        Color::LightGreen
    } else if pnl < -1e-9 {
        Color::LightRed
    } else {
        palette.muted
    };
    let stat_label = match slot.status {
        SlotStatus::Idle => "Idle",
        SlotStatus::Running => "Run",
        SlotStatus::Error => "Err",
    };
    let stat_color = match slot.status {
        SlotStatus::Running => palette.ok,
        SlotStatus::Error => palette.error,
        SlotStatus::Idle => palette.muted,
    };
    let pos_str = if pos.is_flat() {
        "    ----   ".to_string()
    } else {
        format!("{}@{}", fmt_qty(pos.qty), fmt_price(pos.avg))
    };
    let _strat_meta = slot.strategy.meta();

    // Layout: PAIR(7) PRICE(11) STAT(4) PNL(7) POS@AVG(14) = 43 cols
    // body content + 16 cols at the right edge for action buttons.
    // Total inner = 59 cols (fits a 33-col panel).
    const BODY_WIDTH: u16 = 7 + 11 + 4 + 7 + 14;
    let mut x = area.x;
    let put_cell = |frame: &mut Frame, x: &mut u16, w: u16, text: String, style: Style| {
        let avail = (area.x + area.width).saturating_sub(*x);
        if avail == 0 {
            return;
        }
        let r = Rect {
            x: *x,
            y: area.y,
            width: w.min(avail),
            height: 1,
        };
        render_clipped(frame, Paragraph::new(text).style(style), r);
        *x += w;
    };
    // Compact row layout (43 cols + 16 for buttons = 59).
    // PAIR(7) PRICE(11) STAT(4) PNL(7) POS@AVG(14)
    let price_bg = if flash { palette.cursor_bg } else { bg };
    let price_fg = if flash || active {
        palette.cursor_fg
    } else {
        palette.text
    };
    put_cell(
        frame,
        &mut x,
        7,
        truncate(&slot.symbol, 7).to_string(),
        Style::default()
            .fg(palette.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    );
    put_cell(
        frame,
        &mut x,
        11,
        format!(" {:>10}", fmt_price(last)),
        Style::default()
            .fg(price_fg)
            .bg(price_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_cell(
        frame,
        &mut x,
        4,
        format!(" {:>3}", stat_label),
        Style::default()
            .fg(stat_color)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    );
    put_cell(
        frame,
        &mut x,
        7,
        format!(" {:>+6.2}", pnl),
        Style::default()
            .fg(pnl_color)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    );
    put_cell(
        frame,
        &mut x,
        14,
        format!(" {}", truncate(&pos_str, 13)),
        Style::default().fg(fg).bg(bg),
    );
    let body_rect = Rect {
        x: area.x,
        y: area.y,
        width: BODY_WIDTH.min(area.width),
        height: 1,
    };

    // Action buttons (right-aligned).
    let act_w_total: u16 = 4 * 4; // [Rn][Sp][Ed][X]
    let act_x = (area.x + area.width).saturating_sub(act_w_total);
    let start = button_cell(
        frame,
        Rect {
            x: act_x,
            y: area.y,
            width: 4,
            height: 1,
        },
        "[Rn]",
        slot.status == SlotStatus::Idle,
        palette,
        bg,
    );
    let stop = button_cell(
        frame,
        Rect {
            x: act_x + 4,
            y: area.y,
            width: 4,
            height: 1,
        },
        "[Sp]",
        slot.status == SlotStatus::Running,
        palette,
        bg,
    );
    let edit = button_cell(
        frame,
        Rect {
            x: act_x + 8,
            y: area.y,
            width: 4,
            height: 1,
        },
        "[Ed]",
        true,
        palette,
        bg,
    );
    let remove = button_cell(
        frame,
        Rect {
            x: act_x + 12,
            y: area.y,
            width: 4,
            height: 1,
        },
        "[X ]",
        true,
        palette,
        bg,
    );

    SlotRects {
        symbol: slot.symbol.clone(),
        body: body_rect,
        start,
        stop,
        edit,
        remove,
    }
}

fn button_cell(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    enabled: bool,
    palette: &Palette,
    row_bg: Color,
) -> Rect {
    let fg = if enabled {
        palette.accent
    } else {
        palette.muted
    };
    render_clipped(
        frame,
        Paragraph::new(label).style(Style::default().fg(fg).bg(row_bg)),
        area,
    );
    area
}

fn fill_bg(frame: &mut Frame, area: Rect, bg: Color) {
    let buf = frame.buffer_mut();
    for col in 0..area.width {
        for row in 0..area.height {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(bg));
            }
        }
    }
}

fn kill_label(rt: &Runtime) -> &'static str {
    if rt.limits.kill_switch {
        "[KILL]"
    } else {
        ""
    }
}
