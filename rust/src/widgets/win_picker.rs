//! Desk windows: the `+` chip next to the link chip and the picker it opens
//! (`:win`) — add a window of a page next to this one, or close this one;
//! the `×` chip beside it closes this window at once.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::{centered, rect_contains, render_clipped, truncate};

/// Pages a new desk window can show, with a one-line hint.
pub const PAGES: &[(&str, &str)] = &[
    ("chart", "candles of the linked symbol"),
    ("book", "order book"),
    ("tape", "time & sales"),
    ("liq", "liquidations, all market"),
    ("watch", "the active list"),
    ("account", "positions, orders, fills"),
    ("terminal", "ticket and strategies"),
    ("screener", "all pairs"),
    ("lists", "list groups"),
    ("stats", "equity and results"),
    ("agents", "agent connector"),
    ("lab", "backtests"),
    ("compare", "lead-lag, correlations"),
    ("main", "menu, account, open positions"),
];

fn page_hint_uk(page: &str) -> &'static str {
    match page {
        "chart" => "свічки зв'язаного символу",
        "book" => "стакан",
        "tape" => "стрічка угод",
        "liq" => "ліквідації, увесь ринок",
        "watch" => "активний список",
        "account" => "позиції, ордери, виконання",
        "terminal" => "тікет і стратегії",
        "screener" => "усі пари",
        "lists" => "групи списків",
        "stats" => "капітал і результати",
        "agents" => "конектор агентів",
        "lab" => "бектести",
        "compare" => "хто веде, кореляції",
        "main" => "меню, рахунок, відкриті позиції",
        _ => "",
    }
}

/// Rows of the picker: every page, the workspace presets
/// (`crate::desk::PRESETS`), then "close this window".
pub const ROWS: usize = PAGES.len() + crate::desk::PRESETS.len() + 1;
/// The last row.
pub const CLOSE_ROW: usize = ROWS - 1;

/// The workspace preset on picker row `row`, if it is one.
pub fn preset_at(row: usize) -> Option<usize> {
    row.checked_sub(PAGES.len())
        .filter(|i| *i < crate::desk::PRESETS.len())
}

const CHIP: &str = " + ";

/// Where the chip sits: just left of the link chip, or at the right edge
/// when the link chip is hidden (narrow window).  Empty when too narrow.
pub fn chip_rect(menu: Rect, link_chip: Rect) -> Rect {
    let w = CHIP.chars().count() as u16;
    let right = if link_chip.width > 0 {
        link_chip.x.saturating_sub(1)
    } else {
        (menu.x + menu.width).saturating_sub(1)
    };
    if menu.width < 24 || right < menu.x + w {
        return Rect::default();
    }
    Rect {
        x: right - w,
        y: menu.y,
        width: w,
        height: 1,
    }
}

pub fn render_chip(frame: &mut Frame, r: Rect, mouse: Option<(u16, u16)>, palette: &Palette) {
    if r.width == 0 {
        return;
    }
    let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
    let style = if hot {
        Style::default()
            .fg(palette.bg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.text)
            .bg(palette.panel)
            .add_modifier(Modifier::BOLD)
    };
    render_clipped(frame, Paragraph::new(Span::styled(CHIP, style)), r);
}

const CLOSE: &str = " × ";

/// The close chip: just left of the `+` chip (empty when that one is).
pub fn close_rect(chip: Rect, menu: Rect) -> Rect {
    let w = CLOSE.chars().count() as u16;
    if chip.width == 0 || chip.x < menu.x + w + 1 {
        return Rect::default();
    }
    Rect {
        x: chip.x - w - 1,
        width: w,
        ..chip
    }
}

/// `×`: closes this window (as F10 does: its state is saved).
pub fn render_close(frame: &mut Frame, r: Rect, mouse: Option<(u16, u16)>, palette: &Palette) {
    if r.width == 0 {
        return;
    }
    let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
    let style = if hot {
        Style::default()
            .fg(palette.black)
            .bg(palette.warn)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.text)
            .bg(palette.panel)
            .add_modifier(Modifier::BOLD)
    };
    render_clipped(frame, Paragraph::new(Span::styled(CLOSE, style)), r);
}

#[derive(Clone, Copy, Default)]
pub struct PickerLayout {
    pub area: Rect,
    pub close: Rect,
    pub rows: [Rect; ROWS],
}

impl PickerLayout {
    /// Which row a click landed on.
    pub fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        self.rows.iter().position(|r| rect_contains(*r, x, y))
    }
}

const PICKER_W: u16 = 46;
const PICKER_H: u16 = ROWS as u16 + 6;

/// `selected`: the highlighted row; `desk`: this window's desk, if any.
pub fn render_picker(
    frame: &mut Frame,
    screen: Rect,
    selected: usize,
    desk: Option<&str>,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> PickerLayout {
    let area = centered(screen, PICKER_W, PICKER_H);
    render_clipped(frame, Clear, area);
    let title = match desk {
        Some(d) => crate::i18n::pick(format!(" Desk {d}: windows "), format!(" Стіл {d}: вікна ")),
        None => crate::i18n::pick(" Windows ", " Вікна ").to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let mut layout = PickerLayout {
        area,
        ..Default::default()
    };
    let close = Rect {
        x: area.x + area.width.saturating_sub(5),
        y: area.y,
        width: 4,
        height: 1,
    }
    .intersection(area);
    layout.close = close;
    let close_hot = mouse.is_some_and(|(mx, my)| rect_contains(close, mx, my));
    render_clipped(
        frame,
        Paragraph::new(Span::styled(
            "[X]",
            if close_hot {
                Style::default()
                    .fg(palette.error)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.muted).bg(palette.bg)
            },
        )),
        close,
    );

    let line = |dy: u16| {
        Rect {
            x: inner.x.saturating_add(1),
            y: inner.y.saturating_add(dy),
            width: inner.width.saturating_sub(2),
            height: 1,
        }
        .intersection(inner)
    };
    let w = inner.width.saturating_sub(2) as usize;
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    render_clipped(
        frame,
        Paragraph::new(Span::styled(
            truncate(
                if desk.is_some() {
                    crate::i18n::pick("Add a window next to this one:", "Додати вікно поруч:")
                } else {
                    crate::i18n::pick(
                        "Not a desk window: :desk open NAME first.",
                        "Не вікно столу: спершу :desk open НАЗВА.",
                    )
                },
                w,
            ),
            muted,
        )),
        line(0),
    );
    for (i, slot) in layout.rows.iter_mut().enumerate() {
        // Blank lines before the presets and before "close".
        let dy = 1 + i as u16 + u16::from(i >= PAGES.len()) + u16::from(i == CLOSE_ROW);
        let r = line(dy);
        *slot = r;
        let text = match (PAGES.get(i), preset_at(i)) {
            (Some((page, hint)), _) => {
                format!(
                    " + {page:<9} {}",
                    crate::i18n::pick(*hint, page_hint_uk(page))
                )
            }
            (None, Some(p)) => format!(
                " ◆ {}: {}",
                crate::i18n::pick("Workspace", "Стіл"),
                crate::desk::PRESETS[p].2
            ),
            _ => crate::i18n::pick(" Close this window", " Закрити це вікно").to_string(),
        };
        let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
        let style = if i == selected || hot {
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if i == CLOSE_ROW {
            Style::default().fg(palette.error).bg(palette.bg)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        render_clipped(
            frame,
            Paragraph::new(Span::styled(format!("{:<w$}", truncate(&text, w)), style)),
            r,
        );
    }
    render_clipped(
        frame,
        Paragraph::new(Span::styled(
            truncate(
                crate::i18n::pick(
                    "Up/Down choose   Enter   Esc close",
                    "Вгору/вниз вибір   Enter   Esc закрити",
                ),
                w,
            ),
            muted,
        )),
        line(ROWS as u16 + 3),
    );
    layout
}
