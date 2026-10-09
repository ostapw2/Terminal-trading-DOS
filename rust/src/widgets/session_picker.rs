//! Session-window picker — pure ASCII.  Lets the user pick `HH:MM` start
//! and end of the trading session to keep on the chart.  Everything
//! outside the window is hidden and replaced with a visible gap.
//!
//! Layout (32x9):
//!
//!   +------ Session window -------+
//!   |   Start:  [-] HH:MM [+]     |
//!   |   End:    [-] HH:MM [+]     |
//!   |                              |
//!   |   Preset: [24/7] [NY] [LON]  |
//!   |                              |
//!   |   [ Off ]  [ Cancel ] [Apply]|
//!   +------------------------------+

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::markets::SessionWindow;
use crate::tokens::Palette;
use crate::widgets::button;
use crate::widgets::fmt::render_clipped;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SessionButton {
    Off,
    Cancel,
    Apply,
}

#[derive(Clone, Copy)]
pub struct SessionPickerState {
    pub start_min: u16,
    pub end_min: u16,
    /// 0 = start hour, 1 = start minute, 2 = end hour, 3 = end minute.
    pub focus: u8,
}

impl SessionPickerState {
    pub fn from_window(w: SessionWindow) -> Self {
        SessionPickerState {
            start_min: w.start_min,
            end_min: w.end_min,
            focus: 0,
        }
    }

    pub fn to_window(self) -> SessionWindow {
        SessionWindow {
            start_min: self.start_min.min(1440),
            end_min: self.end_min.min(1440),
        }
    }

    pub fn nudge(&mut self, delta: i32) {
        let target = match self.focus {
            0 => &mut self.start_min,
            1 => &mut self.start_min,
            2 => &mut self.end_min,
            _ => &mut self.end_min,
        };
        let step: i32 = match self.focus {
            0 | 2 => 60, // hour
            _ => 5,      // minute
        };
        let mut v = *target as i32 + delta * step;
        // Snap to the step grid so a string of 5-min nudges stays clean.
        v = v.div_euclid(step) * step;
        v = v.clamp(0, 1440);
        *target = v as u16;
    }
}

pub struct SessionPickerLayout {
    pub area: Rect,
    pub start_minus: Rect,
    pub start_plus: Rect,
    pub end_minus: Rect,
    pub end_plus: Rect,
    pub start_field: Rect,
    pub end_field: Rect,
    pub preset_24: Rect,
    pub preset_ny: Rect,
    pub preset_lon: Rect,
    pub off_rect: Rect,
    pub cancel_rect: Rect,
    pub apply_rect: Rect,
}

pub fn render(
    frame: &mut Frame,
    parent: Rect,
    state: &SessionPickerState,
    mouse: Option<(u16, u16)>,
    pressed_button: Option<SessionButton>,
    palette: &Palette,
) -> SessionPickerLayout {
    let w: u16 = 38;
    let h: u16 = 11;
    let x = parent.x + parent.width.saturating_sub(w) / 2;
    let y = parent.y + parent.height.saturating_sub(h) / 2;
    let area = Rect {
        x,
        y,
        width: w.min(parent.width),
        height: h.min(parent.height),
    };
    render_clipped(frame, Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.accent).bg(palette.bg))
        .title(Span::styled(
            " Session window ",
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let label_style = Style::default().fg(palette.text).bg(palette.bg);
    let btn_minus = Style::default()
        .fg(palette.bg)
        .bg(palette.muted)
        .add_modifier(Modifier::BOLD);
    let btn_plus = Style::default()
        .fg(palette.bg)
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD);

    // Row 0: "  Start:   [-] 16:00 [+]   <-- HH | MM"
    let start_y = inner.y;
    let start_label_rect = Rect {
        x: inner.x + 1,
        y: start_y,
        width: 8,
        height: 1,
    };
    render_clipped(
        frame,
        Paragraph::new("Start:").style(label_style),
        start_label_rect,
    );
    let start_minus = Rect {
        x: inner.x + 9,
        y: start_y,
        width: 3,
        height: 1,
    };
    let start_field = Rect {
        x: inner.x + 13,
        y: start_y,
        width: 5,
        height: 1,
    };
    let start_plus = Rect {
        x: inner.x + 19,
        y: start_y,
        width: 3,
        height: 1,
    };
    render_clipped(frame, Paragraph::new("[-]").style(btn_minus), start_minus);
    render_clipped(frame, Paragraph::new("[+]").style(btn_plus), start_plus);
    render_clipped(
        frame,
        Paragraph::new(format_hhmm(state.start_min, state.focus, true)).style(
            Style::default()
                .fg(palette.text)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        start_field,
    );

    // Row 1: End:
    let end_y = inner.y + 1;
    let end_label_rect = Rect {
        x: inner.x + 1,
        y: end_y,
        width: 8,
        height: 1,
    };
    render_clipped(
        frame,
        Paragraph::new("End:").style(label_style),
        end_label_rect,
    );
    let end_minus = Rect {
        x: inner.x + 9,
        y: end_y,
        width: 3,
        height: 1,
    };
    let end_field = Rect {
        x: inner.x + 13,
        y: end_y,
        width: 5,
        height: 1,
    };
    let end_plus = Rect {
        x: inner.x + 19,
        y: end_y,
        width: 3,
        height: 1,
    };
    render_clipped(frame, Paragraph::new("[-]").style(btn_minus), end_minus);
    render_clipped(frame, Paragraph::new("[+]").style(btn_plus), end_plus);
    render_clipped(
        frame,
        Paragraph::new(format_hhmm(state.end_min, state.focus, false)).style(
            Style::default()
                .fg(palette.text)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        end_field,
    );

    // Row 3: presets.  Skip row 2 for breathing room.
    let preset_y = inner.y + 3;
    let preset_24 = Rect {
        x: inner.x + 1,
        y: preset_y,
        width: 8,
        height: 1,
    };
    let preset_ny = Rect {
        x: inner.x + 10,
        y: preset_y,
        width: 13,
        height: 1,
    };
    let preset_lon = Rect {
        x: inner.x + 24,
        y: preset_y,
        width: 12,
        height: 1,
    };
    let preset_style = Style::default()
        .fg(palette.bg)
        .bg(palette.text)
        .add_modifier(Modifier::BOLD);
    render_clipped(
        frame,
        Paragraph::new("[ 24/7 ]").style(preset_style),
        preset_24,
    );
    render_clipped(
        frame,
        Paragraph::new("[ NY 13:30-20 ]").style(preset_style),
        preset_ny,
    );
    render_clipped(
        frame,
        Paragraph::new("[ LON 8-16:30 ]").style(preset_style),
        preset_lon,
    );

    // Hint row, positioned between presets and the action button row.
    let hint_y = inner.y + 5;
    if hint_y + button::HEIGHT < inner.y + inner.height {
        render_clipped(
            frame,
            Paragraph::new(" Tab focus  Up/Dn nudge  Enter apply ")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: inner.x,
                y: hint_y,
                width: inner.width,
                height: 1,
            },
        );
    }

    // Action button row — 3D raised, 2 rows tall.
    let action_y = inner.y + inner.height.saturating_sub(button::HEIGHT);
    let off_w = button::measure("Off");
    let cancel_w = button::measure("Cancel");
    let apply_w = button::measure("Apply");
    let off_rect = Rect {
        x: inner.x + 1,
        y: action_y,
        width: off_w,
        height: button::HEIGHT,
    };
    let cancel_rect = Rect {
        x: inner.x + 1 + off_w + 1,
        y: action_y,
        width: cancel_w,
        height: button::HEIGHT,
    };
    let apply_rect = Rect {
        x: inner.x + inner.width.saturating_sub(apply_w + 1),
        y: action_y,
        width: apply_w,
        height: button::HEIGHT,
    };

    let render_btn =
        |frame: &mut Frame, r: Rect, label: &str, primary: bool, btn_id: SessionButton| {
            let hover = match mouse {
                Some((mx, my)) => {
                    my >= r.y && my < r.y + r.height && mx >= r.x && mx < r.x + r.width
                }
                None => false,
            };
            let pressed = pressed_button == Some(btn_id);
            let s = button::pick_state(false, pressed, false, hover, primary);
            button::render(frame, r, label, s, palette);
        };
    render_btn(frame, off_rect, "Off", false, SessionButton::Off);
    render_btn(frame, cancel_rect, "Cancel", false, SessionButton::Cancel);
    render_btn(frame, apply_rect, "Apply", true, SessionButton::Apply);

    SessionPickerLayout {
        area,
        start_minus,
        start_plus,
        end_minus,
        end_plus,
        start_field,
        end_field,
        preset_24,
        preset_ny,
        preset_lon,
        off_rect,
        cancel_rect,
        apply_rect,
    }
}

fn format_hhmm(total: u16, focus: u8, is_start: bool) -> String {
    let hh = total / 60;
    let mm = total % 60;
    let _ = focus;
    let _ = is_start;
    format!("{:02}:{:02}", hh, mm)
}
