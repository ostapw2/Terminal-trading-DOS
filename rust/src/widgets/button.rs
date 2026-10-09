//! Bordered action button.
//!
//! Each button is a 3-row, bordered rectangle:
//!
//!   ┌──────────┐
//!   │   OK     │   ← face row (centered label)
//!   └──────────┘
//!
//! No drop shadow, no 3D simulation — just a clean ASCII/Unicode
//! rectangle.  The border style switches with state to communicate
//! focus/hover/press.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

/// Visual state.  Precedence: `Disabled > Pressed > Focused > Hover > Primary > Default`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Default,
    Primary,
    Hover,
    Focused,
    Pressed,
    Disabled,
}

pub fn pick_state(
    disabled: bool,
    pressed: bool,
    focused: bool,
    hover: bool,
    primary: bool,
) -> State {
    if disabled {
        State::Disabled
    } else if pressed {
        State::Pressed
    } else if focused {
        State::Focused
    } else if hover {
        State::Hover
    } else if primary {
        State::Primary
    } else {
        State::Default
    }
}

/// Total button height — top border + face row + bottom border.
pub const HEIGHT: u16 = 3;

/// Total button width including borders.  Minimum 10 cells so very
/// short labels (`OK`) still look like buttons.
pub fn measure(label: &str) -> u16 {
    (label.chars().count() as u16 + 6).max(10)
}

pub fn render(frame: &mut Frame, area: Rect, label: &str, state: State, palette: &Palette) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let (face_style, border_style, prefix) = styles(state, palette);

    // Outer bordered block.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .style(face_style);
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    // Centered label inside the bordered face.
    let display = if prefix.is_empty() {
        label.to_string()
    } else {
        format!("{} {}", prefix, label)
    };
    let face_row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1.min(inner.height),
    };
    render_clipped(
        frame,
        Paragraph::new(display)
            .style(face_style)
            .alignment(Alignment::Center),
        face_row,
    );
}

/// (face_style, border_style, prefix_marker)
fn styles(state: State, palette: &Palette) -> (Style, Style, &'static str) {
    let bold = Modifier::BOLD;
    match state {
        State::Default => (
            Style::default()
                .fg(palette.black)
                .bg(palette.muted)
                .add_modifier(bold),
            Style::default().fg(palette.black).bg(palette.muted),
            "",
        ),
        State::Primary => (
            Style::default()
                .fg(palette.black)
                .bg(palette.accent)
                .add_modifier(bold),
            Style::default().fg(palette.black).bg(palette.accent),
            "",
        ),
        State::Hover => (
            Style::default()
                .fg(palette.black)
                .bg(palette.light_cyan)
                .add_modifier(bold),
            Style::default().fg(palette.black).bg(palette.light_cyan),
            "",
        ),
        State::Focused => (
            Style::default()
                .fg(palette.black)
                .bg(palette.accent)
                .add_modifier(bold),
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(bold),
            ">",
        ),
        State::Pressed => (
            Style::default()
                .fg(palette.text)
                .bg(palette.accent_dim)
                .add_modifier(bold),
            Style::default().fg(palette.text).bg(palette.accent_dim),
            "",
        ),
        State::Disabled => (
            Style::default().fg(palette.bg).bg(palette.muted),
            Style::default().fg(palette.muted).bg(palette.bg),
            "",
        ),
    }
}
