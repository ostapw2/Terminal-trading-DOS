//! One-row (or wrapped) strip of labelled buttons.
//!
//! Every action that has a hotkey also gets a button here, so the UI shows
//! what can be done without knowing the keys.  Each button reads `key label`
//! (e.g. `w + List`); the key is the same one the keyboard handler takes.
//! Rendering returns the rect of every button that fit, so the caller can
//! hit-test clicks.  Disabled buttons are drawn muted and return no rect.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::{rect_contains, render_clipped};

pub struct ToolButton {
    /// Hotkey shown in front of the label ("" for none).
    pub key: String,
    pub label: String,
    pub enabled: bool,
    /// Drawn as the current choice (e.g. the selected target mode).
    pub active: bool,
}

impl ToolButton {
    pub fn new(key: &str, label: impl Into<String>) -> Self {
        Self {
            key: key.to_string(),
            label: label.into(),
            enabled: true,
            active: false,
        }
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub fn active(mut self, on: bool) -> Self {
        self.active = on;
        self
    }

    fn text(&self) -> String {
        if self.key.is_empty() {
            format!(" {} ", self.label)
        } else {
            format!(" {} {} ", self.key, self.label)
        }
    }
}

const GAP: u16 = 1;

/// Draw `buttons` after an optional muted `label`, wrapping to the next row
/// of `area` when the row is full.  Returns one entry per button: the rect
/// it occupies, or `None` if it is disabled or did not fit.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    buttons: &[ToolButton],
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> Vec<Option<Rect>> {
    let mut out = vec![None; buttons.len()];
    if area.width == 0 || area.height == 0 {
        return out;
    }
    let right = area.x.saturating_add(area.width);
    let mut x = area.x;
    let mut y = area.y;

    if !label.is_empty() {
        let w = (label.chars().count() as u16 + 1).min(area.width);
        render_clipped(
            frame,
            Paragraph::new(label.to_string())
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
        );
        x = x.saturating_add(w);
    }

    for (i, b) in buttons.iter().enumerate() {
        let text = b.text();
        let w = text.chars().count() as u16;
        if x.saturating_add(w) > right {
            // Wrap, but only to a row that still belongs to `area`.
            if y + 1 >= area.y + area.height {
                break;
            }
            y += 1;
            x = area.x;
            if w > area.width {
                continue;
            }
        }
        let rect = Rect {
            x,
            y,
            width: w,
            height: 1,
        };
        let hovered = b.enabled && mouse.is_some_and(|(mx, my)| rect_contains(rect, mx, my));
        let style = if !b.enabled {
            Style::default().fg(palette.muted).bg(palette.bg)
        } else if hovered {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else if b.active {
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        let key_style = if b.enabled && !hovered && !b.active {
            style.fg(palette.accent).add_modifier(Modifier::BOLD)
        } else {
            style
        };
        let mut spans = Vec::new();
        if b.key.is_empty() {
            spans.push(Span::styled(text, style));
        } else {
            spans.push(Span::styled(" ", style));
            spans.push(Span::styled(b.key.clone(), key_style));
            spans.push(Span::styled(format!(" {} ", b.label), style));
        }
        render_clipped(frame, Paragraph::new(Line::from(spans)), rect);
        if b.enabled {
            out[i] = Some(rect);
        }
        x = x.saturating_add(w + GAP);
    }
    out
}
