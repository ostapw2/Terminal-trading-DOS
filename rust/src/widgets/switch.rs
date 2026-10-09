//! ASCII switch: `[ON ]` / `[off]`.

use crate::widgets::fmt::render_clipped;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};

use crate::tokens::{glyphs, Palette};

pub fn render(frame: &mut Frame, area: Rect, value: bool, focused: bool, palette: &Palette) {
    let text = if value {
        glyphs::SWITCH_ON
    } else {
        glyphs::SWITCH_OFF
    };
    let style = if focused {
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else if value {
        Style::default().fg(palette.accent).bg(palette.bg)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    let p = Paragraph::new(text).style(style);
    render_clipped(frame, p, area);
}
