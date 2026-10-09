//! Single-line text input.  ASCII-only.
//!
//! State holds the buffer + a cursor index (byte offset, but our strings are
//! ASCII so byte == char).  Render shows `text` with a `_` cursor when
//! focused, padded to fill `area.width`.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::fmt::render_clipped;

#[derive(Default, Clone)]
pub struct InputState {
    pub text: String,
    pub cursor: usize,
    pub placeholder: String,
    /// Show `*` instead of the text (secrets).
    pub mask: bool,
}

impl InputState {
    pub fn new(placeholder: &str) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            placeholder: placeholder.into(),
            mask: false,
        }
    }

    pub fn push_char(&mut self, c: char) {
        if !c.is_ascii() || c.is_control() {
            return;
        }
        self.text.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.text.remove(self.cursor);
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }
    pub fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += 1;
        }
    }
    pub fn home(&mut self) {
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }
}

pub fn render(frame: &mut Frame, area: Rect, st: &InputState, focused: bool, palette: &Palette) {
    let bg = palette.black;
    let fg = if focused {
        palette.accent
    } else {
        palette.light_cyan
    };

    let shown = if st.mask {
        "*".repeat(st.text.chars().count())
    } else {
        st.text.clone()
    };
    let text = if st.text.is_empty() && !focused {
        format!(" {}", st.placeholder)
    } else if focused {
        // Render with cursor: split at cursor, insert reversed cell marker.
        // Easy approach: leave plain text, mouse-cursor-style invert in
        // post-render.  Here we just render the text + an underscore as
        // "edit cursor hint" at end.
        format!(" {}_", shown)
    } else {
        format!(" {}", shown)
    };
    let style = if st.text.is_empty() && !focused {
        Style::default().fg(palette.muted).bg(bg)
    } else {
        Style::default().fg(fg).bg(bg)
    };

    let p = Paragraph::new(text).style(style);
    render_clipped(frame, p, area);

    if focused {
        // Inverted cursor cell at position (area.x + 1 + cursor, area.y).
        let cx = area.x + 1 + st.cursor as u16;
        let cy = area.y;
        if cx < area.x + area.width && cy < area.y + area.height {
            let buf = frame.buffer_mut();
            if let Some(cell) = buf.cell_mut((cx, cy)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}
