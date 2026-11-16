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

/// A feed older than this many seconds is called stale.
pub const STALE_SECS: f64 = 10.0;

/// `feed_age_secs`: seconds since the last price tick reached the chart or the
/// screener (`None` = no feed information, `INFINITY` = nothing ever arrived).
pub fn render_extended(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    is_live: bool,
    ws_connected: bool,
    feed_age_secs: Option<f64>,
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

    // Feed indicator: the age of the last tick tells whether prices really
    // move; a connected socket alone does not.
    match feed_age_secs {
        Some(age) if age < STALE_SECS => {
            let label = format!("⚡{age:.1}s ");
            let style = Style::default()
                .fg(palette.light_cyan)
                .bg(palette.bg)
                .add_modifier(Modifier::DIM);
            put(
                frame,
                area,
                &mut x,
                cells(&label),
                Span::styled(label, style),
            );
        }
        Some(age) => {
            let label = if age.is_finite() {
                format!(" NO TICKS {}s ", age.round() as u64)
            } else {
                " NO TICKS ".to_string()
            };
            let style = Style::default()
                .fg(palette.warn)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD);
            put(
                frame,
                area,
                &mut x,
                cells(&label),
                Span::styled(label, style),
            );
        }
        None if ws_connected => {
            let style = Style::default()
                .fg(palette.light_cyan)
                .bg(palette.bg)
                .add_modifier(Modifier::DIM);
            put(frame, area, &mut x, 2, Span::styled("⚡", style));
        }
        None => {}
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

/// A desk window's header row: the page and what it shows, a fresh message
/// (`note`, empty when old), and the feed only when it is stale.  The mode,
/// the account and the feed's health live in the main window, once.
pub fn render_bare(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    note: &str,
    feed_age_secs: Option<f64>,
    palette: &Palette,
) {
    let bg = Style::default().fg(palette.text).bg(palette.bg);
    render_clipped(
        frame,
        Paragraph::new(" ".repeat(area.width as usize)).style(bg),
        area,
    );
    let mut x = area.x;
    let head = format!(" {title} ");
    put(
        frame,
        area,
        &mut x,
        cells(&head),
        Span::styled(
            head,
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
    );
    if let Some(age) = feed_age_secs.filter(|a| *a >= STALE_SECS) {
        let label = if age.is_finite() {
            format!(" NO TICKS {}s ", age.round() as u64)
        } else {
            " NO TICKS ".to_string()
        };
        put(
            frame,
            area,
            &mut x,
            cells(&label),
            Span::styled(
                label,
                Style::default()
                    .fg(palette.warn)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    if !note.is_empty() {
        let n = format!(" {note}");
        put(
            frame,
            area,
            &mut x,
            cells(&n),
            Span::styled(n, Style::default().fg(palette.muted).bg(palette.bg)),
        );
    }
}
