use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::Paragraph,
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

/// Draw `text` at cursor `x` in `width` cells, clipped to `area`, then
/// advance the cursor.  Narrow terminals simply lose the tail.
fn put(frame: &mut Frame, area: Rect, x: &mut u16, width: u16, span: Span<'_>) {
    let r = Rect {
        x: *x,
        y: area.y,
        width,
        height: 1,
    }
    .intersection(area);
    if r.width > 0 && r.height > 0 {
        render_clipped(frame, Paragraph::new(span), r);
    }
    *x = x.saturating_add(width);
}

fn cells(s: &str) -> u16 {
    s.chars().count().min(u16::MAX as usize) as u16
}

pub fn render_extended(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    is_live: bool,
    ws_connected: bool,
    palette: &Palette,
) {
    let bg = Style::default().fg(palette.text).bg(palette.bg);
    let filler: String = " ".repeat(area.width as usize);
    render_clipped(frame, Paragraph::new(filler).style(bg), area);

    let mut x = area.x;

    // Mode indicator
    if is_live {
        let style = Style::default()
            .fg(palette.ok)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD);
        let label = " ● LIVE ";
        put(
            frame,
            area,
            &mut x,
            cells(label),
            Span::styled(label, style),
        );
    } else if crate::data::demo::is_on() {
        let style = Style::default()
            .fg(palette.warn)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD);
        let label = " ◆ DEMO DATA · paper ";
        put(
            frame,
            area,
            &mut x,
            cells(label),
            Span::styled(label, style),
        );
    } else {
        let style = Style::default()
            .fg(palette.muted)
            .bg(palette.bg)
            .add_modifier(Modifier::DIM);
        let label = " ○ paper ";
        put(
            frame,
            area,
            &mut x,
            cells(label),
            Span::styled(label, style),
        );
    }

    // WS indicator
    if ws_connected {
        let style = Style::default()
            .fg(palette.light_cyan)
            .bg(palette.bg)
            .add_modifier(Modifier::DIM);
        put(frame, area, &mut x, 2, Span::styled("⚡", style));
    }

    // Separator
    let sep_style = Style::default()
        .fg(palette.muted)
        .bg(palette.bg)
        .add_modifier(Modifier::DIM);
    put(frame, area, &mut x, 2, Span::styled("│", sep_style));

    // Parse the text for " | " separator to show hints differently
    let (main, hint) = if let Some(pos) = text.find("  |  ") {
        let (left, right) = text.split_at(pos);
        (left.trim(), Some(right.trim_start_matches("  |  ")))
    } else {
        (text.trim(), None)
    };

    // Main text
    let main_style = Style::default().fg(palette.text).bg(palette.bg);
    let main_w = cells(main).saturating_add(1);
    put(
        frame,
        area,
        &mut x,
        main_w,
        Span::styled(format!(" {}", main), main_style),
    );

    // Hint text in dim/accent
    if let Some(h) = hint {
        let hint_style = Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::DIM);
        let hint_w = cells(h).saturating_add(1);
        put(
            frame,
            area,
            &mut x,
            hint_w,
            Span::styled(format!(" {}", h), hint_style),
        );
    }
}
