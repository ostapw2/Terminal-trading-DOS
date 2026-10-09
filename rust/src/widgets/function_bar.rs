use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::Paragraph,
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

#[derive(Clone, Copy)]
pub struct FKey {
    pub number: u8,
    pub label: &'static str,
}

pub fn render_with_hover(
    frame: &mut Frame,
    area: Rect,
    keys: &[FKey],
    hover: Option<(u16, u16)>,
    palette: &Palette,
) {
    let cell_count = keys.len() as u16;
    if cell_count == 0 {
        return;
    }

    // Draw a subtle top border line for the function bar area
    let top_filler: String = " ".repeat(area.width as usize);
    let bar_bg = Style::default().fg(palette.text).bg(palette.black);
    render_clipped(frame, Paragraph::new(top_filler).style(bar_bg), area);

    let constraints: Vec<Constraint> = (0..cell_count)
        .map(|_| Constraint::Ratio(1, cell_count.into()))
        .collect();

    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (cell, key) in cells.iter().zip(keys.iter()) {
        let hovered = match hover {
            Some((mx, my)) => my == cell.y && mx >= cell.x && mx < cell.x + cell.width,
            None => false,
        };

        let inner = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(*cell);

        // "F" prefix + number
        let num_text = format!("F{}", key.number);
        let num_style = if hovered {
            Style::default()
                .fg(palette.black)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(palette.accent)
                .bg(palette.black)
                .add_modifier(Modifier::BOLD)
        };
        let num = Paragraph::new(Span::raw(num_text)).style(num_style);
        render_clipped(frame, num, inner[0]);

        // Label
        let lbl_style = if hovered {
            Style::default()
                .fg(palette.black)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.black)
        };
        let lbl = Paragraph::new(Span::raw(key.label)).style(lbl_style);
        render_clipped(frame, lbl, inner[1]);
    }
}
