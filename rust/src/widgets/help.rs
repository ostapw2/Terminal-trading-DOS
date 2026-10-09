use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::{centered, render_clipped, truncate};

pub struct HelpPage {
    pub sections: Vec<HelpSection>,
    pub page: usize,
    pub page_count: usize,
}

pub struct HelpSection {
    pub title: Option<String>,
    pub items: Vec<(String, String, String)>,
}

pub struct HelpLayout {
    pub area: Rect,
    pub prev_rect: Rect,
    pub next_rect: Rect,
    pub close_rect: Rect,
}

pub fn render(
    frame: &mut Frame,
    screen: Rect,
    page: &HelpPage,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> HelpLayout {
    let total_w: u16 = screen.width.saturating_sub(8).clamp(50, 72);
    let total_h: u16 = screen.height.saturating_sub(4).clamp(12, 30);
    let area = centered(screen, total_w, total_h);

    render_clipped(frame, Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.accent).bg(palette.bg))
        .title(Span::styled(
            format!(
                " {} ({}/{}) ",
                "Keyboard Shortcuts",
                page.page + 1,
                page.page_count
            ),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    if page.sections.is_empty() {
        return HelpLayout {
            area,
            prev_rect: Rect::default(),
            next_rect: Rect::default(),
            close_rect: Rect::default(),
        };
    }
    let section = &page.sections[page.page.min(page.page_count.saturating_sub(1))];

    let pad = Rect {
        x: inner.x.saturating_add(2),
        y: inner.y.saturating_add(1),
        width: inner.width.saturating_sub(4),
        height: inner.height.saturating_sub(2),
    };

    let lines = layout_help_section(section, pad.width, palette);

    let p = Paragraph::new(lines)
        .style(Style::default().fg(palette.text).bg(palette.bg))
        .wrap(Wrap { trim: false });
    render_clipped(frame, p, pad);

    // Navigation buttons at the bottom of the help area
    let nav_y = area.y + area.height.saturating_sub(2);
    let btn_style = |hovered: bool| -> Style {
        if hovered {
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.accent).bg(palette.bg)
        }
    };

    // Prev button
    let prev_label = "< Prev";
    let prev_w = prev_label.len() as u16;
    let prev_x = area.x + 2;
    let prev_rect = Rect {
        x: prev_x,
        y: nav_y,
        width: prev_w,
        height: 1,
    }
    .intersection(area);
    let prev_hovered = mouse.is_some_and(|(mx, my)| prev_rect.contains((mx, my).into()));
    render_clipped(
        frame,
        Paragraph::new(Span::styled(prev_label, btn_style(prev_hovered))),
        prev_rect,
    );

    // Next button
    let next_label = "Next >";
    let next_w = next_label.len() as u16;
    let next_x = area.x + area.width.saturating_sub(next_w + 2);
    let next_rect = Rect {
        x: next_x,
        y: nav_y,
        width: next_w,
        height: 1,
    }
    .intersection(area);
    let next_hovered = mouse.is_some_and(|(mx, my)| next_rect.contains((mx, my).into()));
    render_clipped(
        frame,
        Paragraph::new(Span::styled(next_label, btn_style(next_hovered))),
        next_rect,
    );

    // Close button in title bar area (right side)
    let close_label = " [X] ";
    let close_w = close_label.len() as u16;
    let close_x = area.x + area.width.saturating_sub(close_w + 1);
    let close_rect = Rect {
        x: close_x,
        y: area.y,
        width: close_w,
        height: 1,
    }
    .intersection(area);
    let close_hovered = mouse.is_some_and(|(mx, my)| close_rect.contains((mx, my).into()));
    let close_style = if close_hovered {
        Style::default()
            .fg(palette.error)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    render_clipped(
        frame,
        Paragraph::new(Span::styled(close_label, close_style)),
        close_rect,
    );

    HelpLayout {
        area,
        prev_rect,
        next_rect,
        close_rect,
    }
}

fn layout_help_section(
    section: &HelpSection,
    max_w: u16,
    palette: &Palette,
) -> Vec<ratatui::text::Line<'static>> {
    let mut lines: Vec<ratatui::text::Line> = Vec::new();

    if let Some(title) = &section.title {
        let header_style = Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        lines.push(ratatui::text::Line::from(Span::styled(
            format!(" {} ", title),
            header_style,
        )));
        lines.push(ratatui::text::Line::from(""));
    }

    let key_col: u16 = section
        .items
        .iter()
        .map(|(k, _, _)| k.chars().count() as u16)
        .max()
        .unwrap_or(0)
        .min(20);

    for (key, desc, comment) in &section.items {
        let key_padded = format!("{:<width$}", key, width = key_col as usize);
        let key_span = Span::styled(key_padded, Style::default().fg(palette.light_cyan));

        let mut spans = vec![key_span, Span::raw("  ")];

        if comment.is_empty() {
            spans.push(Span::styled(
                desc.clone(),
                Style::default().fg(palette.text),
            ));
        } else {
            let desc_w = max_w.saturating_sub(key_col).saturating_sub(6) as usize;
            let truncated = if comment.chars().count() > desc_w {
                format!("{}…", truncate(comment, desc_w.saturating_sub(1)))
            } else {
                comment.clone()
            };
            spans.push(Span::styled(
                desc.clone(),
                Style::default().fg(palette.text),
            ));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                truncated,
                Style::default()
                    .fg(palette.muted)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        lines.push(ratatui::text::Line::from(spans));
    }

    lines
}
