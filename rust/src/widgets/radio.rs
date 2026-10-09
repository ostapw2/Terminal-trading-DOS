//! ASCII radio set: vertical list of `(*) Selected` / `( ) Other`.

use crate::widgets::fmt::render_clipped;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::tokens::{glyphs, Palette};

pub fn render(
    frame: &mut Frame,
    area: Rect,
    options: &[&str],
    selected: usize,
    focused: bool,
    palette: &Palette,
) {
    render_titled(frame, area, None, options, selected, focused, palette);
}

pub fn render_titled(
    frame: &mut Frame,
    area: Rect,
    title: Option<&str>,
    options: &[&str],
    selected: usize,
    focused: bool,
    palette: &Palette,
) {
    let border_color = if focused {
        palette.accent
    } else {
        palette.text
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color).bg(palette.bg))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    if let Some(t) = title {
        block = block.title(Span::styled(
            format!(" {} ", t),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    for (i, opt) in options.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let on = i == selected;
        let mark = if on {
            glyphs::RADIO_ON
        } else {
            glyphs::RADIO_OFF
        };
        let mark_style = if on {
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.light_cyan).bg(palette.bg)
        };
        let label_style = if on && focused {
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
            Span::styled(*opt, label_style),
        ]);
        let row_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        render_clipped(frame, Paragraph::new(line), row_area);
    }
}
