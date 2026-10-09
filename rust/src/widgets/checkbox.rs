//! ASCII checkbox: `[X] Label` / `[ ] Label`.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tokens::glyphs;
use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: bool,
    focused: bool,
    palette: &Palette,
) {
    let mark = if value {
        glyphs::CHECKBOX_ON
    } else {
        glyphs::CHECKBOX_OFF
    };
    let mark_style = if value {
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.light_cyan).bg(palette.bg)
    };
    let label_style = if focused {
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(palette.bg)
    };

    let line = Line::from(vec![
        Span::styled(mark, mark_style),
        Span::raw(" "),
        Span::styled(label, label_style),
    ]);
    let p = Paragraph::new(line);
    render_clipped(frame, p, area);
}
