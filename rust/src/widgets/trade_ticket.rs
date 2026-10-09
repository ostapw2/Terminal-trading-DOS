//! Basket trade ticket — every button operates on EVERY selected pair
//! at once.  There is no "active symbol": one click = N orders, one per
//! pair in the selection.
//!
//! Layout:
//!
//! ```text
//! +-------------------------------------------+
//! |  BASKET: 17 pairs    Open 0/17            |
//! |  UPNL +$0.00 (+0.00%)                     |
//! +-------------------------------------------+
//! |  [ BUY MKT ]   [ SELL MKT ]               |
//! |  [ BUY ASK ]   [ SELL ASK ]               |
//! |  [ BUY BID ]   [ SELL BID ]               |
//! |  [   REV   ]   [   CLOSE  ]               |
//! +-------------------------------------------+
//! |  [QTY]  [-] 0.100 [+]   (≈ $5.00 / pair)  |
//! |  LEV [×1] >                               |
//! +-------------------------------------------+
//! ```
//!
//! Sizing semantics:
//! - `[QTY]` mode → every pair gets the SAME `ticket_qty × leverage`
//!   coins.  Useful when all pairs are roughly the same price.
//! - `[USD]` mode → every pair gets `ticket_notional × leverage / last`
//!   coins, so equal-USD exposure regardless of price.  E.g. $5 per
//!   pair × 17 pairs = $85 total notional (× leverage).
//!
//! `REV` reverses every open position (close + open opposite of size
//! `effective_qty` per pair).  `CLOSE` flattens every open position.
//!
//! Paper broker doesn't model margin or liquidation: leverage just
//! scales effective qty.  Real-broker plug-ins should respect this
//! same field.
//!
//! Mouse routing happens in `commander.rs`: every clickable rect lives
//! on the returned [`TradeTicketLayout`].

use crate::widgets::fmt::{
    fmt_money_signed, fmt_qty_plain, pnl_color, rect_contains, render_clipped,
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
use crate::widgets::button;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TicketAction {
    BuyMarket,
    SellMarket,
    BuyAsk,
    SellAsk,
    BuyBid,
    SellBid,
    Reverse,
    Close,
}

impl TicketAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::BuyMarket => "BUY MKT",
            Self::SellMarket => "SELL MKT",
            Self::BuyAsk => "BUY ASK",
            Self::SellAsk => "SELL ASK",
            Self::BuyBid => "BUY BID",
            Self::SellBid => "SELL BID",
            Self::Reverse => "REV",
            Self::Close => "CLOSE",
        }
    }
}

/// Whether the ticket field reads as raw qty (coins / shares) or as a
/// USD notional that the engine divides by the last mark to get qty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeMode {
    Qty,
    Usd,
}

impl SizeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Qty => "QTY",
            Self::Usd => "USD",
        }
    }
    pub fn flip(self) -> SizeMode {
        match self {
            Self::Qty => Self::Usd,
            Self::Usd => Self::Qty,
        }
    }
}

/// Selectable leverage steps.  Cycle button walks this list.  Paper
/// broker doesn't margin-check — leverage just multiplies the position
/// size.  Real-broker plug-ins should respect this same field.
pub const LEVERAGE_STEPS: &[f64] = &[1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0];

pub fn cycle_leverage(current: f64, dir: i32) -> f64 {
    let idx = LEVERAGE_STEPS
        .iter()
        .position(|&v| (v - current).abs() < 1e-9)
        .unwrap_or(0) as i32;
    let n = LEVERAGE_STEPS.len() as i32;
    let next = (idx + dir).rem_euclid(n) as usize;
    LEVERAGE_STEPS[next]
}

/// Clickable rectangles produced by [`render`].  All in screen coords.
#[derive(Clone, Debug, Default)]
pub struct TradeTicketLayout {
    pub area: Rect,
    pub buttons: Vec<(TicketAction, Rect)>,
    pub size_mode_toggle: Rect,
    pub qty_minus: Rect,
    pub qty_plus: Rect,
    pub qty_field: Rect,
    pub leverage_cycle: Rect,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    runtime: &Runtime,
    ticket_qty: f64,
    ticket_notional: f64,
    size_mode: SizeMode,
    leverage: f64,
    mouse: Option<(u16, u16)>,
    pressed: Option<TicketAction>,
    palette: &Palette,
) -> TradeTicketLayout {
    let mut layout = TradeTicketLayout {
        area,
        ..Default::default()
    };

    let live = runtime.is_live();
    let title = if live {
        " Trade Ticket - BASKET [LIVE BINANCE] "
    } else {
        " Trade Ticket - BASKET [PAPER] "
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

    // ── Header (2 lines: basket size + UPNL) ──────────────────────────
    render_header(frame, inner, runtime, palette);

    // ── Button grid (4 rows × 2 cols of 3-row buttons = 12 rows) ──────
    let grid_top = inner.y + 2;
    let grid_height = 12u16.min(inner.height.saturating_sub(2));
    if grid_height >= 3 {
        let grid_area = Rect {
            x: inner.x,
            y: grid_top,
            width: inner.width,
            height: grid_height,
        };
        layout.buttons = render_button_grid(frame, grid_area, runtime, mouse, pressed, palette);
    }

    // ── Footer: 2 lines ───────────────────────────────────────────────
    //   line 1: [QTY/USD] toggle | [-] value [+] | (≈ derived view)
    //   line 2: LEV [×N] >
    let footer_y = grid_top + grid_height;
    if footer_y < inner.y + inner.height {
        let line1 = Rect {
            x: inner.x + 1,
            y: footer_y,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        let n_pairs = runtime.slots.len();
        let (mode_r, qm, qf, qp) = render_footer_size_line(
            frame,
            line1,
            ticket_qty,
            ticket_notional,
            size_mode,
            n_pairs,
            mouse,
            palette,
        );
        layout.size_mode_toggle = mode_r;
        layout.qty_minus = qm;
        layout.qty_field = qf;
        layout.qty_plus = qp;

        if footer_y + 1 < inner.y + inner.height {
            let line2 = Rect {
                x: inner.x + 1,
                y: footer_y + 1,
                width: inner.width.saturating_sub(2),
                height: 1,
            };
            layout.leverage_cycle =
                render_footer_options_line(frame, line2, leverage, mouse, palette);
        }
    }

    layout
}

fn render_header(frame: &mut Frame, inner: Rect, runtime: &Runtime, palette: &Palette) {
    let pad = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: 2.min(inner.height),
    };

    let n_pairs = runtime.slots.len();
    let n_open = runtime
        .broker
        .all_positions()
        .values()
        .filter(|p| !p.is_flat())
        .count();

    // Aggregate UPNL across every open position in the basket.
    let upnl: f64 = runtime
        .broker
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(s, p)| {
            let last = runtime.last_mark(s).unwrap_or(p.avg);
            p.unrealized(last)
        })
        .sum();
    // Aggregate cost basis (|qty| × avg) for the % computation.
    let basis: f64 = runtime
        .broker
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(_, p)| p.avg * p.qty.abs())
        .sum();
    let pct = if basis > 0.0 {
        upnl / basis * 100.0
    } else {
        0.0
    };
    let pnl_color = pnl_color(upnl);

    let line1 = vec![
        Span::styled(
            "BASKET: ",
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
        Span::styled(
            format!("{} pairs", n_pairs),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("    Open {}/{} positions", n_open, n_pairs),
            Style::default().fg(palette.text).bg(palette.bg),
        ),
    ];
    let line2 = vec![
        Span::styled("UPNL ", Style::default().fg(palette.muted).bg(palette.bg)),
        Span::styled(
            fmt_money_signed(upnl),
            Style::default()
                .fg(pnl_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({:>+6.2}%)", pct),
            Style::default().fg(pnl_color).bg(palette.bg),
        ),
    ];

    render_clipped(
        frame,
        Paragraph::new(vec![Line::from(line1), Line::from(line2)]),
        pad,
    );
}

fn render_button_grid(
    frame: &mut Frame,
    area: Rect,
    runtime: &Runtime,
    mouse: Option<(u16, u16)>,
    pressed: Option<TicketAction>,
    palette: &Palette,
) -> Vec<(TicketAction, Rect)> {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    let basket_present = !runtime.slots.is_empty();
    let any_open_position = runtime
        .broker
        .all_positions()
        .values()
        .any(|p| !p.is_flat());

    let row_specs = [
        (TicketAction::BuyMarket, TicketAction::SellMarket),
        (TicketAction::BuyAsk, TicketAction::SellAsk),
        (TicketAction::BuyBid, TicketAction::SellBid),
        (TicketAction::Reverse, TicketAction::Close),
    ];

    let mut out = Vec::with_capacity(8);
    for (i, (left, right)) in row_specs.iter().enumerate() {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[i]);
        let l_rect = inset(cols[0], 1, 0);
        let r_rect = inset(cols[1], 1, 0);
        let l_disabled = !basket_present
            || (matches!(left, TicketAction::Reverse | TicketAction::Close) && !any_open_position);
        let r_disabled = !basket_present
            || (matches!(right, TicketAction::Reverse | TicketAction::Close) && !any_open_position);
        render_action_button(frame, l_rect, *left, l_disabled, mouse, pressed, palette);
        render_action_button(frame, r_rect, *right, r_disabled, mouse, pressed, palette);
        out.push((*left, l_rect));
        out.push((*right, r_rect));
    }
    out
}

fn render_action_button(
    frame: &mut Frame,
    area: Rect,
    action: TicketAction,
    disabled: bool,
    mouse: Option<(u16, u16)>,
    pressed: Option<TicketAction>,
    palette: &Palette,
) {
    let hover = mouse
        .map(|(x, y)| rect_contains(area, x, y))
        .unwrap_or(false);
    let is_pressed = pressed == Some(action);
    let primary = matches!(
        action,
        TicketAction::BuyMarket
            | TicketAction::BuyAsk
            | TicketAction::BuyBid
            | TicketAction::SellMarket
            | TicketAction::SellAsk
            | TicketAction::SellBid
    );
    let state = button::pick_state(disabled, is_pressed, false, hover, primary);
    button::render(frame, area, action.label(), state, palette);

    // Override colour for buy/sell semantics (green/red) if not pressed
    // or disabled.  We do this by repainting the inner cell row after
    // `button::render` to colour-tint the face.
    if !disabled && !is_pressed {
        let tint = match action {
            TicketAction::BuyMarket | TicketAction::BuyAsk | TicketAction::BuyBid => {
                Some(Color::LightGreen)
            }
            TicketAction::SellMarket | TicketAction::SellAsk | TicketAction::SellBid => {
                Some(Color::LightRed)
            }
            TicketAction::Reverse => Some(palette.accent),
            TicketAction::Close => Some(palette.warn),
        };
        if let Some(bg) = tint {
            // Only repaint the centre face row (1 row of inner content).
            if area.height >= 3 && area.width >= 4 {
                let face = Rect {
                    x: area.x + 1,
                    y: area.y + 1,
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                let buf = frame.buffer_mut();
                let label = action.label();
                let pad = (face.width as usize).saturating_sub(label.len()) / 2;
                for col in 0..face.width {
                    if let Some(cell) = buf.cell_mut((face.x + col, face.y)) {
                        let ch = if (col as usize) >= pad && (col as usize) < pad + label.len() {
                            label
                                .as_bytes()
                                .get((col as usize) - pad)
                                .map(|b| *b as char)
                                .unwrap_or(' ')
                        } else {
                            ' '
                        };
                        cell.set_char(ch);
                        cell.set_style(
                            Style::default()
                                .fg(palette.black)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                }
            }
        }
    }
}

/// First footer line: [QTY/USD] toggle, then [-] value [+], then a
/// muted hint reading "× N pairs" (basket-mode reminder).
fn render_footer_size_line(
    frame: &mut Frame,
    area: Rect,
    ticket_qty: f64,
    ticket_notional: f64,
    size_mode: SizeMode,
    n_pairs: usize,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> (Rect, Rect, Rect, Rect) {
    let mode_label = format!(" [{}] ", size_mode.label());
    let minus = " [-] ";
    let plus = " [+] ";

    let mut x = area.x;
    let put = |frame: &mut Frame, x: u16, width: u16, text: &str, style: Style| -> Rect {
        let r = Rect {
            x,
            y: area.y,
            width: width.min((area.x + area.width).saturating_sub(x)),
            height: 1,
        };
        render_clipped(frame, Paragraph::new(text.to_string()).style(style), r);
        r
    };

    // [QTY/USD] toggle.
    let mode_hover = mouse
        .map(|(mx, my)| {
            let r = Rect {
                x,
                y: area.y,
                width: mode_label.len() as u16,
                height: 1,
            };
            rect_contains(r, mx, my)
        })
        .unwrap_or(false);
    let mode_rect = put(
        frame,
        x,
        mode_label.len() as u16,
        &mode_label,
        Style::default()
            .fg(if mode_hover {
                palette.cursor_fg
            } else {
                palette.bg
            })
            .bg(if mode_hover {
                palette.cursor_bg
            } else {
                palette.accent
            })
            .add_modifier(Modifier::BOLD),
    );
    x += mode_label.len() as u16;

    // [-] button.
    let m_hover = mouse
        .map(|(mx, my)| {
            let r = Rect {
                x,
                y: area.y,
                width: minus.len() as u16,
                height: 1,
            };
            rect_contains(r, mx, my)
        })
        .unwrap_or(false);
    let m_rect = put(
        frame,
        x,
        minus.len() as u16,
        minus,
        Style::default()
            .fg(if m_hover {
                palette.cursor_fg
            } else {
                palette.text
            })
            .bg(if m_hover {
                palette.cursor_bg
            } else {
                palette.accent_dim
            })
            .add_modifier(Modifier::BOLD),
    );
    x += minus.len() as u16;

    // Value field.
    let value_text = match size_mode {
        SizeMode::Qty => format!(" {:>10} ", fmt_qty_plain(ticket_qty)),
        SizeMode::Usd => format!(" ${:>9.2} ", ticket_notional),
    };
    let q_rect = put(
        frame,
        x,
        value_text.len() as u16,
        &value_text,
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    x += value_text.len() as u16;

    // [+] button.
    let p_hover = mouse
        .map(|(mx, my)| {
            let r = Rect {
                x,
                y: area.y,
                width: plus.len() as u16,
                height: 1,
            };
            rect_contains(r, mx, my)
        })
        .unwrap_or(false);
    let p_rect = put(
        frame,
        x,
        plus.len() as u16,
        plus,
        Style::default()
            .fg(if p_hover {
                palette.cursor_fg
            } else {
                palette.text
            })
            .bg(if p_hover {
                palette.cursor_bg
            } else {
                palette.accent_dim
            })
            .add_modifier(Modifier::BOLD),
    );
    x += plus.len() as u16;

    // Per-pair preview — the value is applied to EVERY pair on submit.
    let preview = match size_mode {
        SizeMode::Qty => format!("  qty × {} pairs", n_pairs),
        SizeMode::Usd => format!("  ${:.2} × {} pairs", ticket_notional, n_pairs),
    };
    let pw = (preview.len() as u16).min((area.x + area.width).saturating_sub(x));
    if pw > 0 {
        let _ = put(
            frame,
            x,
            pw,
            &preview,
            Style::default().fg(palette.muted).bg(palette.bg),
        );
    }

    (mode_rect, m_rect, q_rect, p_rect)
}

/// Second footer line: leverage cycle button (per-slot strategy
/// assignment lives in the selection list below, not in the basket
/// ticket).
fn render_footer_options_line(
    frame: &mut Frame,
    area: Rect,
    leverage: f64,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> Rect {
    let lev_text = format!(" LEV [x{}] > ", fmt_lev(leverage));

    let put = |frame: &mut Frame, x: u16, width: u16, text: &str, style: Style| -> Rect {
        let r = Rect {
            x,
            y: area.y,
            width: width.min((area.x + area.width).saturating_sub(x)),
            height: 1,
        };
        render_clipped(frame, Paragraph::new(text.to_string()).style(style), r);
        r
    };

    let lev_hover = mouse
        .map(|(mx, my)| {
            let r = Rect {
                x: area.x,
                y: area.y,
                width: lev_text.len() as u16,
                height: 1,
            };
            rect_contains(r, mx, my)
        })
        .unwrap_or(false);
    put(
        frame,
        area.x,
        lev_text.len() as u16,
        &lev_text,
        Style::default()
            .fg(if lev_hover {
                palette.cursor_fg
            } else {
                palette.text
            })
            .bg(if lev_hover {
                palette.cursor_bg
            } else {
                palette.accent_dim
            })
            .add_modifier(Modifier::BOLD),
    )
}

fn fmt_lev(l: f64) -> String {
    if (l - l.round()).abs() < 1e-9 {
        format!("{}", l as i64)
    } else {
        format!("{:.1}", l)
    }
}

fn inset(r: Rect, dx: u16, dy: u16) -> Rect {
    Rect {
        x: r.x + dx,
        y: r.y + dy,
        width: r.width.saturating_sub(dx * 2),
        height: r.height.saturating_sub(dy * 2),
    }
}

/// Step the qty up or down by one decimal of magnitude.  E.g. 0.10 ↔ 0.20,
/// 1.0 ↔ 2.0, 30 ↔ 31.  Floor at 0.001.
pub fn step_qty(q: f64, dir: i32) -> f64 {
    let abs = q.abs().max(1e-9);
    let mag = abs.log10().floor();
    let step = 10f64.powf(mag - 1.0).max(0.001);
    let next = q + step * (dir.signum() as f64);
    next.max(0.001)
}
