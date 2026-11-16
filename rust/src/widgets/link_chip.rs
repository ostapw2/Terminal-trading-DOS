//! Link-group control in the top-right corner of the menu bar, and the small
//! picker it opens.
//!
//! The chip shows the window's group (`LINK 2`, or `LINK off`) and, when other
//! windows are open, how many there are.  A click opens the picker: `Off` and
//! groups 1-8, each with its own colour (the number is always shown too, the
//! colour is only a second cue).

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::link::MAX_GROUP;
use crate::tokens::Palette;
use crate::widgets::fmt::{centered, rect_contains, render_clipped, truncate};

/// Named ANSI colours only (DESIGN rule 2); bull / bear greens and reds are
/// left out so a group never looks like a price move.
pub const GROUP_COLORS: [Color; MAX_GROUP as usize] = [
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::LightBlue,
    Color::LightYellow,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// Colour of group `g` (1-8); `None` for "off".
pub fn group_color(g: u8) -> Option<Color> {
    (1..=MAX_GROUP)
        .contains(&g)
        .then(|| GROUP_COLORS[(g - 1) as usize])
}

/// Text of the chip: ` LINK 2 `, ` LINK off `, with ` | 3w` when other
/// windows are open.
pub fn label(group: u8, other_windows: usize) -> String {
    let g = if group == 0 {
        crate::i18n::pick("off", "вимк").to_string()
    } else {
        group.to_string()
    };
    if other_windows > 0 {
        format!(" LINK {g} | {}w ", other_windows + 1)
    } else {
        format!(" LINK {g} ")
    }
}

/// Where the chip sits in `menu` (right-aligned, one cell from the edge).
/// Empty when the bar is too narrow to share with the menu titles.
pub fn chip_rect(menu: Rect, group: u8, other_windows: usize) -> Rect {
    let w = label(group, other_windows).chars().count() as u16;
    if menu.width < w.saturating_add(40) {
        return Rect::default();
    }
    Rect {
        x: menu.x + menu.width - w - 1,
        y: menu.y,
        width: w,
        height: 1,
    }
}

pub fn render_chip(
    frame: &mut Frame,
    menu: Rect,
    group: u8,
    other_windows: usize,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) {
    let r = chip_rect(menu, group, other_windows);
    if r.width == 0 {
        return;
    }
    let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
    let style = match group_color(group) {
        Some(c) => Style::default()
            .fg(palette.black)
            .bg(c)
            .add_modifier(Modifier::BOLD),
        None if hot => Style::default()
            .fg(palette.bg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
        None => Style::default().fg(palette.muted).bg(palette.panel),
    };
    let style = if hot && group != 0 {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    };
    render_clipped(
        frame,
        Paragraph::new(Span::styled(label(group, other_windows), style)),
        r,
    );
}

/// Picker cells: index 0 = Off, 1..=8 = groups.
pub const CELLS: usize = MAX_GROUP as usize + 1;

#[derive(Clone, Copy, Default)]
pub struct PickerLayout {
    pub area: Rect,
    pub close: Rect,
    pub cells: [Rect; CELLS],
}

impl PickerLayout {
    /// Which group (0 = off) a click landed on.
    pub fn cell_at(&self, x: u16, y: u16) -> Option<usize> {
        self.cells.iter().position(|r| rect_contains(*r, x, y))
    }
}

const PICKER_W: u16 = 52;
const PICKER_H: u16 = 9;

/// `group`: the current one; `selected`: the highlighted cell (keyboard).
pub fn render_picker(
    frame: &mut Frame,
    screen: Rect,
    group: u8,
    selected: usize,
    other_windows: usize,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> PickerLayout {
    let area = centered(screen, PICKER_W, PICKER_H);
    render_clipped(frame, Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            crate::i18n::pick(" Link group ", " Група зв'язку "),
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

    let row_at = |dy: u16, x: u16, w: u16| {
        Rect {
            x,
            y: inner.y.saturating_add(dy),
            width: w,
            height: 1,
        }
        .intersection(inner)
    };
    let text_w = inner.width.saturating_sub(4) as usize;
    let cx = inner.x.saturating_add(2);

    // The row of choices.
    let mut x = cx;
    for (i, slot) in layout.cells.iter_mut().enumerate().take(CELLS) {
        let name = if i == 0 {
            crate::i18n::pick(" Off ", " Вимк. ").to_string()
        } else {
            format!(" {i} ")
        };
        let w = name.chars().count() as u16;
        let r = row_at(1, x, w);
        *slot = r;
        let current = i as u8 == group;
        let picked = i == selected;
        let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
        let base = match group_color(i as u8) {
            Some(c) => Style::default().fg(palette.black).bg(c),
            None => Style::default().fg(palette.text).bg(palette.panel),
        };
        let style = if picked || hot {
            base.add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if current {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        render_clipped(frame, Paragraph::new(Span::styled(name, style)), r);
        // Under the current group: a marker that does not rely on colour.
        if current {
            render_clipped(
                frame,
                Paragraph::new(Span::styled(
                    "^".repeat(w as usize),
                    Style::default()
                        .fg(palette.accent)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                )),
                row_at(2, x, w),
            );
        }
        x = x.saturating_add(w + 1);
    }

    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let lines = [
        crate::i18n::pick(
            "Windows in one group follow each other's",
            "Вікна однієї групи слідують одне за одним:",
        )
        .to_string(),
        crate::i18n::pick("symbol, timeframe and list.", "символ, таймфрейм і список.").to_string(),
        if other_windows == 0 {
            crate::i18n::pick("No other window is open.", "Інших вікон немає.").to_string()
        } else {
            crate::i18n::pick(
                format!("{other_windows} other window(s) open."),
                format!("Відкрито ще вікон: {other_windows}."),
            )
        },
        crate::i18n::pick(
            "0-8 choose   <- -> move   Enter apply   Esc close",
            "0-8 вибір   <- -> рух   Enter застосувати   Esc закрити",
        )
        .to_string(),
    ];
    for (k, line) in lines.iter().enumerate() {
        render_clipped(
            frame,
            Paragraph::new(Line::from(Span::styled(
                truncate(line, text_w).to_string(),
                if k == 3 {
                    Style::default().fg(palette.light_cyan).bg(palette.bg)
                } else {
                    muted
                },
            ))),
            row_at(3 + k as u16, cx, text_w as u16),
        );
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_shows_group_and_window_count() {
        assert_eq!(label(0, 0), " LINK off ");
        assert_eq!(label(2, 0), " LINK 2 ");
        assert_eq!(label(2, 2), " LINK 2 | 3w ");
    }

    #[test]
    fn chip_sits_right_and_hides_on_a_narrow_bar() {
        let menu = Rect::new(0, 0, 120, 1);
        let r = chip_rect(menu, 3, 1);
        assert_eq!(r.x + r.width + 1, 120, "one cell from the right edge");
        assert_eq!(chip_rect(Rect::new(0, 0, 30, 1), 3, 1).width, 0);
    }

    #[test]
    fn every_group_has_its_own_colour() {
        for a in 1..=MAX_GROUP {
            assert!(group_color(a).is_some());
            for b in (a + 1)..=MAX_GROUP {
                assert_ne!(group_color(a), group_color(b), "{a} vs {b}");
            }
        }
        assert_eq!(group_color(0), None);
        assert_eq!(group_color(9), None);
    }
}
