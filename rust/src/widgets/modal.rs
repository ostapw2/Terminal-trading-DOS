//! Centered modal overlay — Volkov Commander aesthetic.
//!
//! Double-line frame, drop-shadow underneath, body text + button row.
//! Buttons are 2 rows tall (face + shadow); the layout reserves the
//! extra row automatically.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::button;
use crate::widgets::fmt::{centered, rect_contains, render_clipped};

pub struct Modal<'a> {
    pub title: &'a str,
    pub body: &'a [&'a str],
    pub buttons: &'a [Button<'a>],
    pub focused_button: usize,
    /// Mouse coordinates for hover state on the button row.  None disables hover.
    pub mouse: Option<(u16, u16)>,
    /// Index of the button currently being pressed (mouse-down flash).
    pub pressed_button: Option<usize>,
}

#[derive(Clone, Copy)]
pub struct Button<'a> {
    pub label: &'a str,
    pub primary: bool,
}

pub struct ModalLayout {
    pub area: Rect,
    pub button_rects: Vec<Rect>,
}

pub fn render(frame: &mut Frame, screen: Rect, modal: &Modal, palette: &Palette) -> ModalLayout {
    let body_h = (modal.body.len() as u16).max(1);
    // Inner content: body + spacer + button face + button shadow row.
    let inner_h = body_h + 1 /*spacer*/ + button::HEIGHT;
    let total_h = inner_h + 4; // top/bot border + 1 padding each
    let total_w: u16 = 50;

    let area = centered(screen, total_w, total_h);

    // Wipe whatever was underneath.
    render_clipped(frame, Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(" {} ", modal.title),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x.saturating_add(2),
        y: inner.y.saturating_add(1),
        width: inner.width.saturating_sub(4),
        height: inner.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(body_h),
            Constraint::Length(1),
            Constraint::Length(button::HEIGHT),
        ])
        .split(pad);

    // Body lines.
    let body_lines: Vec<Line> = modal
        .body
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                *l,
                Style::default().fg(palette.text).bg(palette.bg),
            ))
        })
        .collect();
    let body = Paragraph::new(body_lines).style(Style::default().fg(palette.text).bg(palette.bg));
    render_clipped(frame, body, chunks[0]);

    // Buttons row — laid out right-aligned with 1-cell gaps.
    let mut button_rects = Vec::new();
    if !modal.buttons.is_empty() {
        let row = chunks[2];
        let total_btn_w: u16 = modal
            .buttons
            .iter()
            .map(|b| button::measure(b.label))
            .sum::<u16>()
            + (modal.buttons.len() as u16 - 1); // gaps
        let mut x = row.x + row.width.saturating_sub(total_btn_w);
        for (i, b) in modal.buttons.iter().enumerate() {
            let w = button::measure(b.label);
            let r = Rect {
                x,
                y: row.y,
                width: w,
                height: button::HEIGHT,
            }
            .intersection(row);
            let hover = modal.mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
            let pressed = modal.pressed_button == Some(i);
            let state =
                button::pick_state(false, pressed, i == modal.focused_button, hover, b.primary);
            button::render(frame, r, b.label, state, palette);
            button_rects.push(r);
            x = x.saturating_add(w + 1);
        }
    }

    ModalLayout { area, button_rects }
}
