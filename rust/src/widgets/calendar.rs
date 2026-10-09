//! Calendar (date-picker) modal — pure ASCII, mouse + keyboard.
//!
//! Shows a single month grid with prev / next / today / clear / cancel /
//! apply buttons.  All borders use `+ - |`; weekday header uses 2-letter
//! ASCII labels.  Returned `CalendarLayout` exposes per-day rects so the
//! mouse handler in `commander.rs` can route clicks back to days.
//!
//! Used by both "From" and "To" date pickers; the caller decides which
//! field to write the chosen date into.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::tokens::Palette;
use crate::widgets::button;
use crate::widgets::fmt::render_clipped;

#[derive(Clone, Copy)]
pub struct CalendarState {
    /// Currently displayed month (1..=12).
    pub month: u32,
    /// Currently displayed year.
    pub year: i32,
    /// Day currently highlighted by keyboard navigation (1..=31).  Click
    /// always selects regardless of this value.
    pub focus_day: u32,
}

impl CalendarState {
    pub fn from_date(date: Option<&str>) -> Self {
        if let Some(s) = date {
            if let Some((y, m, d)) = parse_ymd(s) {
                return CalendarState {
                    year: y,
                    month: m,
                    focus_day: d.clamp(1, 31),
                };
            }
        }
        // Default to today (UTC; good enough for v1).
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (y, m, d) = crate::markets::unix_secs_to_ymd(secs as i64);
        CalendarState {
            year: y as i32,
            month: m,
            focus_day: d,
        }
    }

    pub fn shift_month(&mut self, delta: i32) {
        let mut total = self.year * 12 + self.month as i32 - 1 + delta;
        let new_year = total.div_euclid(12);
        total = total.rem_euclid(12);
        self.year = new_year;
        self.month = (total + 1) as u32;
        let dim = days_in_month(self.year, self.month);
        if self.focus_day > dim {
            self.focus_day = dim;
        }
    }

    pub fn move_focus(&mut self, delta: i32) {
        let dim = days_in_month(self.year, self.month) as i32;
        let new_day = self.focus_day as i32 + delta;
        if new_day < 1 {
            // Roll into previous month.
            self.shift_month(-1);
            let dim_prev = days_in_month(self.year, self.month);
            self.focus_day = dim_prev;
        } else if new_day > dim {
            self.shift_month(1);
            self.focus_day = 1;
        } else {
            self.focus_day = new_day as u32;
        }
    }

    pub fn current_iso(&self) -> String {
        let dim = days_in_month(self.year, self.month);
        let d = self.focus_day.min(dim);
        format!("{:04}-{:02}-{:02}", self.year, self.month, d)
    }
}

pub struct CalendarLayout {
    pub area: Rect,
    pub prev_rect: Rect,
    pub next_rect: Rect,
    pub today_rect: Rect,
    pub clear_rect: Rect,
    pub cancel_rect: Rect,
    pub apply_rect: Rect,
    /// One rect per drawn day cell, paired with the day number.
    pub day_rects: Vec<(Rect, u32)>,
}

/// Identifies which calendar button is currently pressed, for the
/// short mouse-down "depress" flash on the 3D button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CalendarButton {
    Today,
    Clear,
    Cancel,
    Apply,
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub fn render(
    frame: &mut Frame,
    parent: Rect,
    state: &CalendarState,
    title: &str,
    current_value: Option<&str>,
    mouse: Option<(u16, u16)>,
    pressed_button: Option<CalendarButton>,
    palette: &Palette,
) -> CalendarLayout {
    // Layout: header + weekday + 6 weeks + 2 button rows × 3 rows each.
    let w: u16 = 34;
    let h: u16 = 16;
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
            format!(" {} ", title),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    if inner.width < 28 || inner.height < 11 {
        return CalendarLayout {
            area,
            prev_rect: Rect::default(),
            next_rect: Rect::default(),
            today_rect: Rect::default(),
            clear_rect: Rect::default(),
            cancel_rect: Rect::default(),
            apply_rect: Rect::default(),
            day_rects: Vec::new(),
        };
    }

    // Row 0: < April  2026 >
    let header_row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let prev_rect = Rect {
        x: header_row.x,
        y: header_row.y,
        width: 3,
        height: 1,
    };
    let next_rect = Rect {
        x: header_row.x + header_row.width.saturating_sub(3),
        y: header_row.y,
        width: 3,
        height: 1,
    };
    let title_text = format!(
        " {} {} ",
        MONTH_NAMES[(state.month.saturating_sub(1)) as usize % 12],
        state.year
    );
    render_clipped(
        frame,
        Paragraph::new(Line::from(vec![
            Span::styled(
                "[<]",
                Style::default()
                    .fg(palette.bg)
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{:^width$}",
                    title_text,
                    width = (inner.width.saturating_sub(6)) as usize
                ),
                Style::default()
                    .fg(palette.text)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[>]",
                Style::default()
                    .fg(palette.bg)
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        header_row,
    );

    // Row 1: weekday header.  Mon-first, 2 chars wide, single space gutter.
    let weekday_row = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let weekday_line = " Mo Tu We Th Fr Sa Su";
    render_clipped(
        frame,
        Paragraph::new(weekday_line).style(Style::default().fg(palette.muted).bg(palette.bg)),
        weekday_row,
    );

    // Day grid: 6 rows max, each row is 7 cells of width 3 (" DD" or "  .").
    let dim = days_in_month(state.year, state.month);
    // Day-of-week of the 1st (Mon=0..Sun=6).
    let first_dow = day_of_week(state.year, state.month, 1);
    let mut day_rects: Vec<(Rect, u32)> = Vec::with_capacity(dim as usize);

    // Anchor + week iteration.
    for week in 0..6u16 {
        let row_y = inner.y + 2 + week;
        if row_y >= inner.y + inner.height {
            break;
        }
        for col in 0..7u16 {
            let cell_index = (week as i32) * 7 + col as i32;
            let day_num = cell_index - first_dow as i32 + 1;
            let cell_rect = Rect {
                x: inner.x + 1 + col * 3,
                y: row_y,
                width: 3,
                height: 1,
            };
            if day_num < 1 || day_num as u32 > dim {
                // Out-of-month — render a faint "  ." filler.
                render_clipped(
                    frame,
                    Paragraph::new("  .").style(Style::default().fg(palette.muted).bg(palette.bg)),
                    cell_rect,
                );
                continue;
            }
            let day = day_num as u32;
            let is_focused = day == state.focus_day;
            let is_selected = current_value
                .and_then(parse_ymd)
                .map(|(y, m, d)| y == state.year && m == state.month && d == day)
                .unwrap_or(false);

            let label = format!("{:>3}", day);
            let style = if is_selected {
                Style::default()
                    .fg(palette.bg)
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else if is_focused {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(palette.text).bg(palette.bg)
            };
            render_clipped(frame, Paragraph::new(label).style(style), cell_rect);
            day_rects.push((cell_rect, day));
        }
    }

    // 3D action buttons.  Two rows × 2 buttons.
    //   row N-3 .. N-2 : [ Today  ] [ Clear  ]
    //   row N-1 .. N   : [ Cancel ] [ Apply  ]
    // Each button is `button::HEIGHT` rows tall (face + shadow).
    let top_y = inner.y + inner.height.saturating_sub(2 * button::HEIGHT);
    let bot_y = inner.y + inner.height.saturating_sub(button::HEIGHT);

    let today_w = button::measure("Today");
    let clear_w = button::measure("Clear");
    let cancel_w = button::measure("Cancel");
    let apply_w = button::measure("Apply");

    let today_rect = Rect {
        x: inner.x,
        y: top_y,
        width: today_w,
        height: button::HEIGHT,
    };
    let clear_rect = Rect {
        x: inner.x + today_w + 1,
        y: top_y,
        width: clear_w,
        height: button::HEIGHT,
    };
    let cancel_rect = Rect {
        x: inner.x,
        y: bot_y,
        width: cancel_w,
        height: button::HEIGHT,
    };
    let apply_rect = Rect {
        x: inner.x + inner.width.saturating_sub(apply_w),
        y: bot_y,
        width: apply_w,
        height: button::HEIGHT,
    };

    let render_btn =
        |frame: &mut Frame, r: Rect, label: &str, primary: bool, btn_id: CalendarButton| {
            let hover = match mouse {
                Some((mx, my)) => {
                    my >= r.y && my < r.y + r.height && mx >= r.x && mx < r.x + r.width
                }
                None => false,
            };
            let pressed = pressed_button == Some(btn_id);
            let state = button::pick_state(false, pressed, false, hover, primary);
            button::render(frame, r, label, state, palette);
        };

    render_btn(frame, today_rect, "Today", false, CalendarButton::Today);
    render_btn(frame, clear_rect, "Clear", false, CalendarButton::Clear);
    render_btn(frame, cancel_rect, "Cancel", false, CalendarButton::Cancel);
    render_btn(frame, apply_rect, "Apply", true, CalendarButton::Apply);

    CalendarLayout {
        area,
        prev_rect,
        next_rect,
        today_rect,
        clear_rect,
        cancel_rect,
        apply_rect,
        day_rects,
    }
}

// ─── date math helpers ─────────────────────────────────────────────────

pub fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.split('-');
    let y = it.next()?.parse::<i32>().ok()?;
    let m = it.next()?.parse::<u32>().ok()?;
    let d = it.next()?.parse::<u32>().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let dim = days_in_month(y, m);
    if !(1..=dim).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Howard Hinnant's "days from civil" algorithm — Mon-first weekday in 0..=6.
fn day_of_week(y: i32, m: u32, d: u32) -> u32 {
    let z = civil_from_ymd(y as i64, m, d);
    // 1970-01-01 = Thursday (Mon=3).  z = days since epoch.
    // dow0_thu = (z + 3) mod 7 — but we want Mon=0.  1970-01-01 was Thu = 3.
    ((z + 3).rem_euclid(7)) as u32
}

fn civil_from_ymd(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as u64;
    let m = m as i64;
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1) as u64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}
