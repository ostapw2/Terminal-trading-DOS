use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

pub struct MenuEntry {
    pub title: &'static str,
    pub hotkey: char,
}

/// A menu title in the chosen language (the English title is the menu's id in
/// the code; the hotkey stays a Latin letter).
pub fn title_label(en: &'static str) -> &'static str {
    use crate::i18n::pick;
    let uk = match en {
        "Symbol" => "Символ",
        "Chart" => "Графік",
        "Timeframe" => "Таймфрейм",
        "History" => "Історія",
        "Panes" => "Панелі",
        "Sort" => "Сортування",
        "Filter" => "Фільтр",
        "Open" => "Відкрити",
        "Refresh" => "Оновити",
        "List" => "Список",
        "Market" => "Ринок",
        "API" => "API",
        "Cache" => "Кеш",
        "Target" => "Ціль",
        "Zoom" => "Масштаб",
        "Reconnect" => "Перепідключити",
        "Windows" => "Вікна",
        "Workspace" => "Стіл",
        other => other,
    };
    pick(en, uk)
}

/// Cells a menu title takes in the bar (as drawn by [`render_with_hover`]).
pub fn title_width(e: &MenuEntry) -> u16 {
    let shown = title_label(e.title);
    let n = shown.chars().count() as u16;
    if shown == e.title {
        n
    } else {
        n + 2
    }
}

pub fn render_with_hover(
    frame: &mut Frame,
    area: Rect,
    entries: &[MenuEntry],
    hover: Option<(u16, u16)>,
    palette: &Palette,
) {
    let bar_style = Style::default().fg(palette.accent).bg(palette.panel);
    let filler: String = " ".repeat(area.width as usize);
    render_clipped(frame, Paragraph::new(filler).style(bar_style), area);

    // Draw a subtle bottom border line (accent underline)
    let separator = Span::styled(
        "─".repeat(area.width as usize),
        Style::default()
            .fg(palette.accent_dim)
            .bg(palette.panel)
            .add_modifier(Modifier::DIM),
    );
    render_clipped(
        frame,
        Paragraph::new(separator).style(Style::default().fg(palette.accent_dim).bg(palette.panel)),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    let mut x = area.x.saturating_add(2);
    for e in entries {
        let shown = title_label(e.title);
        // A translated title has no Latin hotkey letter inside: it goes in front.
        let owned;
        let (title, hotkey_front) = if shown == e.title {
            (shown, false)
        } else {
            owned = format!("{} {}", e.hotkey, shown);
            (owned.as_str(), true)
        };
        let title_w = title.chars().count() as u16;
        let cell = Rect {
            x,
            y: area.y,
            width: title_w,
            height: 1,
        };
        let hovered = match hover {
            Some((mx, my)) => my == cell.y && mx >= cell.x && mx < cell.x + cell.width,
            None => false,
        };

        let base_style = if hovered {
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(palette.text)
                .bg(palette.panel)
                .add_modifier(Modifier::DIM)
        };

        let hotkey_lower = e.hotkey.to_ascii_lowercase();
        let title_lower = title.to_lowercase();
        let hotkey_pos = if hotkey_front {
            Some(0)
        } else {
            title_lower.find(hotkey_lower)
        };

        if let Some(pos) = hotkey_pos {
            let before = &title[..pos];
            let hotkey_char = title[pos..].chars().next().unwrap_or(e.hotkey);
            let after = &title[pos + hotkey_char.len_utf8()..];

            let mut spans = Vec::new();
            if !before.is_empty() {
                spans.push(Span::styled(before.to_string(), base_style));
            }
            spans.push(Span::styled(
                hotkey_char.to_string(),
                Style::default()
                    .fg(palette.accent)
                    .bg(if hovered {
                        palette.accent
                    } else {
                        palette.panel
                    })
                    .add_modifier(Modifier::BOLD),
            ));
            if !after.is_empty() {
                spans.push(Span::styled(after.to_string(), base_style));
            }
            let p = Paragraph::new(Line::from(spans)).style(base_style);
            render_clipped(frame, p, cell);
        } else {
            let p = Paragraph::new(Span::styled(title, base_style)).style(base_style);
            render_clipped(frame, p, cell);
        }

        x = x.saturating_add(title_w + 3);
        if x >= area.x + area.width {
            break;
        }
    }
    let _ = Line::from("");
}
