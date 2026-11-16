//! Dynamic-group form (F5, `N` new / `E` edit): one field per rule
//! parameter instead of a single rule line.
//!
//! Fields: name, window, move %, direction, hold, min volume, side.  Text
//! fields validate as you type (the focused field explains what is wrong,
//! a `!` marks every bad one), the OK button is disabled until the whole
//! form is valid, and a preview line shows what the rule will be called.
//!
//! The widget owns no app state: `commander` opens it, routes keys and
//! clicks here and applies the returned [`FormAction`].

use std::cell::Cell;

use crossterm::event::KeyCode;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::tokens::{glyphs, Palette};
use crate::watchlists::{
    dynamic_name, format_volume, parse_volume, GroupSpec, MoveDir, MoveRule, Side, MAX_NAME_CHARS,
    MAX_WINDOW_MIN,
};
use crate::widgets::button;
use crate::widgets::fmt::{centered, rect_contains, render_clipped, truncate};
use crate::widgets::input::{self, InputState};

/// Field / focus-stop indices.  The first `FIELDS` are form rows.
pub const NAME: usize = 0;
pub const WINDOW: usize = 1;
pub const PCT: usize = 2;
pub const DIR: usize = 3;
pub const HOLD: usize = 4;
pub const VOLUME: usize = 5;
pub const SIDE: usize = 6;
pub const OK: usize = 7;
pub const CANCEL: usize = 8;
pub const FIELDS: usize = 7;
const STOPS: usize = 9;

const LABELS: [&str; FIELDS] = [
    "Name",
    "Window, min",
    "Move, %",
    "Direction",
    "Hold, min",
    "Min volume, 24h",
    "Side",
];

const HINTS: [&str; STOPS] = [
    "Optional. Empty = named after the rule.",
    "Look-back window in minutes (1-30).",
    "Smallest move inside the window, in % (0.1-100).",
    "Which way the price has to move.",
    "Keep a symbol this many minutes after its move fades (0 = drop at once).",
    "Skip pairs below this 24h volume in USDT, e.g. 5M. Empty = no filter.",
    "A label only: the Trade page does not enforce it.",
    "Enter confirms, Esc cancels.",
    "Enter or Esc closes the form without saving.",
];

const LABELS_UK: [&str; FIELDS] = [
    "Назва",
    "Вікно, хв",
    "Рух, %",
    "Напрям",
    "Утримання, хв",
    "Мін. обсяг за 24г",
    "Бік",
];

const HINTS_UK: [&str; STOPS] = [
    "Не обов'язково. Порожньо = назва за правилом.",
    "Вікно спостереження в хвилинах (1-30).",
    "Найменший рух у вікні, % (0.1-100).",
    "В який бік має рухатись ціна.",
    "Тримати символ стільки хвилин після затухання руху (0 = одразу прибрати).",
    "Пропустити пари з обсягом за 24г нижче цього, USDT, напр. 5M. Порожньо = без фільтра.",
    "Лише позначка: сторінка Торгівля її не примушує.",
    "Enter підтверджує, Esc скасовує.",
    "Enter або Esc закриває форму без збереження.",
];

fn label_text(i: usize) -> &'static str {
    crate::i18n::pick(LABELS[i], LABELS_UK[i])
}

fn hint_text(i: usize) -> &'static str {
    crate::i18n::pick(HINTS[i], HINTS_UK[i])
}

/// A direction / side name in the chosen language.
fn choice_text(en: &'static str) -> &'static str {
    let uk = match en {
        "Up" => "Вгору",
        "Down" => "Вниз",
        "Both" => "Обидва",
        "Any" => "Будь-який",
        "Long" => "Long",
        "Short" => "Short",
        other => other,
    };
    crate::i18n::pick(en, uk)
}

const DIRS: [(MoveDir, &str); 3] = [
    (MoveDir::Up, "Up"),
    (MoveDir::Down, "Down"),
    (MoveDir::Either, "Both"),
];
const SIDES: [(Side, &str); 3] = [
    (Side::Any, "Any"),
    (Side::Long, "Long"),
    (Side::Short, "Short"),
];

const LABEL_W: u16 = 17;
const MAX_TEXT: usize = 40;
const FORM_W: u16 = 60;
/// Top border + blank + 7 fields + hint + preview + keys + buttons + bottom.
const FORM_H: u16 = 16;

/// What the caller has to do after a key or click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormAction {
    None,
    Submit,
    Cancel,
}

/// Where things were drawn last frame (for hit-testing).  Rows carry no
/// list indices into app data, only focus stops.
#[derive(Clone, Copy, Default)]
pub struct RuleFormLayout {
    pub area: Rect,
    pub close: Rect,
    pub rows: [Rect; FIELDS],
    pub dir_opts: [Rect; 3],
    pub side_opts: [Rect; 3],
    pub ok: Rect,
    pub cancel: Rect,
}

pub struct RuleForm {
    pub editing: bool,
    pub focus: usize,
    pub name: InputState,
    pub window: InputState,
    pub pct: InputState,
    pub hold: InputState,
    pub volume: InputState,
    pub dir: MoveDir,
    pub side: Side,
    /// An error only the caller can know (name already used); cleared by
    /// the next edit.
    pub external_error: Option<(usize, String)>,
    pub layout: Cell<RuleFormLayout>,
}

/// Result of validating the whole form.
pub struct Checked {
    pub errors: [Option<String>; FIELDS],
    pub spec: Option<GroupSpec>,
}

fn text(initial: &str, placeholder: &str) -> InputState {
    let mut st = InputState::new(placeholder);
    st.text = initial.to_string();
    st.cursor = st.text.len();
    st
}

impl RuleForm {
    /// A form for a new group (sensible defaults) or, with `existing`, for
    /// editing a group's name, rule and side.
    pub fn new(existing: Option<(&str, &MoveRule, Side)>) -> Self {
        let (name, rule, side) = match existing {
            Some((n, r, s)) => (n.to_string(), *r, s),
            None => (
                String::new(),
                MoveRule {
                    window_min: 15,
                    threshold_pct: 5.0,
                    dir: MoveDir::Up,
                    hold_min: 5,
                    min_volume: 0.0,
                },
                Side::Any,
            ),
        };
        let volume = if rule.min_volume > 0.0 {
            format_volume(rule.min_volume)
        } else {
            String::new()
        };
        Self {
            editing: existing.is_some(),
            focus: WINDOW,
            name: text(&name, "auto"),
            window: text(&rule.window_min.to_string(), ""),
            pct: text(&rule.threshold_pct.to_string(), ""),
            hold: text(&rule.hold_min.to_string(), "0"),
            volume: text(&volume, "any"),
            dir: rule.dir,
            side,
            external_error: None,
            layout: Cell::new(RuleFormLayout::default()),
        }
    }

    fn input(&self, i: usize) -> Option<&InputState> {
        match i {
            NAME => Some(&self.name),
            WINDOW => Some(&self.window),
            PCT => Some(&self.pct),
            HOLD => Some(&self.hold),
            VOLUME => Some(&self.volume),
            _ => None,
        }
    }

    fn input_mut(&mut self, i: usize) -> Option<&mut InputState> {
        match i {
            NAME => Some(&mut self.name),
            WINDOW => Some(&mut self.window),
            PCT => Some(&mut self.pct),
            HOLD => Some(&mut self.hold),
            VOLUME => Some(&mut self.volume),
            _ => None,
        }
    }

    /// Validate every field; `spec` is `Some` only when all are fine.
    pub fn check(&self) -> Checked {
        let mut errors: [Option<String>; FIELDS] = Default::default();
        let window = self
            .window
            .text
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|w| (1..=MAX_WINDOW_MIN).contains(w));
        if window.is_none() {
            errors[WINDOW] = Some(crate::i18n::pick(
                format!("Whole minutes, 1-{MAX_WINDOW_MIN}."),
                format!("Цілі хвилини, 1-{MAX_WINDOW_MIN}."),
            ));
        }
        let pct = self
            .pct
            .text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|p| p.is_finite() && (0.1..=100.0).contains(p));
        if pct.is_none() {
            errors[PCT] = Some(
                crate::i18n::pick("A percent from 0.1 to 100.", "Відсоток від 0.1 до 100.").into(),
            );
        }
        let hold = match self.hold.text.trim() {
            "" => Some(0),
            t => t.parse::<u32>().ok().filter(|h| *h <= MAX_WINDOW_MIN),
        };
        if hold.is_none() {
            errors[HOLD] = Some(crate::i18n::pick(
                format!("Whole minutes, 0-{MAX_WINDOW_MIN}."),
                format!("Цілі хвилини, 0-{MAX_WINDOW_MIN}."),
            ));
        }
        let volume = match self.volume.text.trim() {
            "" => Some(0.0),
            t => parse_volume(t),
        };
        if volume.is_none() {
            errors[VOLUME] = Some(
                crate::i18n::pick(
                    "A number with K, M or B, e.g. 5M.",
                    "Число з K, M або B, напр. 5M.",
                )
                .into(),
            );
        }
        let name = self.name.text.trim();
        if name.chars().count() > MAX_NAME_CHARS {
            errors[NAME] = Some(crate::i18n::pick(
                format!("At most {MAX_NAME_CHARS} characters."),
                format!("Не більше {MAX_NAME_CHARS} символів."),
            ));
        }
        let spec = match (window, pct, hold, volume) {
            (Some(window_min), Some(threshold_pct), Some(hold_min), Some(min_volume))
                if errors[NAME].is_none() =>
            {
                Some(GroupSpec {
                    rule: MoveRule {
                        window_min,
                        threshold_pct,
                        dir: self.dir,
                        hold_min,
                        min_volume,
                    },
                    name: (!name.is_empty()).then(|| name.to_string()),
                    side: Some(self.side),
                })
            }
            _ => None,
        };
        Checked { errors, spec }
    }

    /// Put the focus on the first field that fails validation.
    pub fn focus_first_error(&mut self) {
        if let Some((i, _)) = self.external_error {
            self.focus = i;
            return;
        }
        if let Some(i) = self.check().errors.iter().position(Option::is_some) {
            self.focus = i;
        }
    }

    fn step(&mut self, forward: bool) {
        self.focus = if forward {
            (self.focus + 1) % STOPS
        } else {
            (self.focus + STOPS - 1) % STOPS
        };
    }
}

fn accepts(field: usize, c: char) -> bool {
    match field {
        NAME => c.is_ascii() && !c.is_control(),
        WINDOW | HOLD => c.is_ascii_digit(),
        PCT => c.is_ascii_digit() || c == '.',
        VOLUME => {
            c.is_ascii_digit() || c == '.' || matches!(c.to_ascii_lowercase(), 'k' | 'm' | 'b')
        }
        _ => false,
    }
}

fn edit(f: &mut RuleForm, field: usize, op: fn(&mut InputState)) {
    if let Some(st) = f.input_mut(field) {
        op(st);
    }
}

fn cycle<T: Copy + PartialEq>(items: &[(T, &str)], current: T, forward: bool) -> T {
    let n = items.len();
    let i = items.iter().position(|(v, _)| *v == current).unwrap_or(0);
    items[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
    .0
}

pub fn handle_key(f: &mut RuleForm, code: KeyCode) -> FormAction {
    match code {
        KeyCode::Esc => return FormAction::Cancel,
        KeyCode::Tab | KeyCode::Down => {
            f.step(true);
            return FormAction::None;
        }
        KeyCode::BackTab | KeyCode::Up => {
            f.step(false);
            return FormAction::None;
        }
        KeyCode::Enter => {
            return if f.focus == CANCEL {
                FormAction::Cancel
            } else {
                FormAction::Submit
            }
        }
        _ => {}
    }
    match f.focus {
        DIR => match code {
            KeyCode::Left => f.dir = cycle(&DIRS, f.dir, false),
            KeyCode::Right | KeyCode::Char(' ') => f.dir = cycle(&DIRS, f.dir, true),
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'u' => f.dir = MoveDir::Up,
                'd' => f.dir = MoveDir::Down,
                'b' => f.dir = MoveDir::Either,
                _ => {}
            },
            _ => {}
        },
        SIDE => match code {
            KeyCode::Left => f.side = cycle(&SIDES, f.side, false),
            KeyCode::Right | KeyCode::Char(' ') => f.side = cycle(&SIDES, f.side, true),
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'a' => f.side = Side::Any,
                'l' => f.side = Side::Long,
                's' => f.side = Side::Short,
                _ => {}
            },
            _ => {}
        },
        OK | CANCEL => match code {
            KeyCode::Left | KeyCode::Right => f.focus = if f.focus == OK { CANCEL } else { OK },
            KeyCode::Char(' ') => {
                return if f.focus == OK {
                    FormAction::Submit
                } else {
                    FormAction::Cancel
                }
            }
            _ => {}
        },
        field => {
            let edited = match code {
                KeyCode::Char(c) if accepts(field, c) => {
                    if let Some(st) = f.input_mut(field) {
                        if st.text.len() < MAX_TEXT {
                            st.push_char(c);
                        }
                    }
                    true
                }
                KeyCode::Backspace => {
                    edit(f, field, InputState::backspace);
                    true
                }
                KeyCode::Delete => {
                    edit(f, field, InputState::delete);
                    true
                }
                KeyCode::Left => {
                    edit(f, field, InputState::left);
                    false
                }
                KeyCode::Right => {
                    edit(f, field, InputState::right);
                    false
                }
                KeyCode::Home => {
                    edit(f, field, InputState::home);
                    false
                }
                KeyCode::End => {
                    edit(f, field, InputState::end);
                    false
                }
                _ => false,
            };
            if edited {
                f.external_error = None;
            }
        }
    }
    FormAction::None
}

/// A left click, resolved against the last drawn layout.
pub fn handle_click(f: &mut RuleForm, x: u16, y: u16) -> FormAction {
    let l = f.layout.get();
    if rect_contains(l.close, x, y) || !rect_contains(l.area, x, y) {
        return FormAction::Cancel;
    }
    if rect_contains(l.ok, x, y) {
        f.focus = OK;
        return FormAction::Submit;
    }
    if rect_contains(l.cancel, x, y) {
        return FormAction::Cancel;
    }
    if let Some(i) = l.dir_opts.iter().position(|r| rect_contains(*r, x, y)) {
        f.focus = DIR;
        f.dir = DIRS[i].0;
        return FormAction::None;
    }
    if let Some(i) = l.side_opts.iter().position(|r| rect_contains(*r, x, y)) {
        f.focus = SIDE;
        f.side = SIDES[i].0;
        return FormAction::None;
    }
    if let Some(i) = l.rows.iter().position(|r| rect_contains(*r, x, y)) {
        f.focus = i;
        if let Some(st) = f.input_mut(i) {
            st.end();
        }
    }
    FormAction::None
}

/// One-row rect inside `inner` (empty when it falls outside).
fn at(inner: Rect, dy: u16, x: u16, w: u16) -> Rect {
    Rect {
        x,
        y: inner.y.saturating_add(dy),
        width: w,
        height: 1,
    }
    .intersection(inner)
}

pub fn render(
    frame: &mut Frame,
    screen: Rect,
    form: &RuleForm,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) {
    let area = centered(screen, FORM_W, FORM_H);
    render_clipped(frame, Clear, area);

    let title = if form.editing {
        crate::i18n::pick(" Edit dynamic group ", " Змінити динамічну групу ")
    } else {
        crate::i18n::pick(" New dynamic group ", " Нова динамічна група ")
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

    let mut layout = RuleFormLayout {
        area,
        ..Default::default()
    };

    // Close [X] in the title bar.
    let close = Rect {
        x: area.x + area.width.saturating_sub(5),
        y: area.y,
        width: 4,
        height: 1,
    }
    .intersection(area);
    layout.close = close;
    let close_hot = mouse.is_some_and(|(mx, my)| rect_contains(close, mx, my));
    let close_style = if close_hot {
        Style::default()
            .fg(palette.error)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    render_clipped(
        frame,
        Paragraph::new(Span::styled("[X]", close_style)),
        close,
    );

    let checked = form.check();
    let cx = inner.x.saturating_add(2);
    let cw = inner.width.saturating_sub(4);
    let ctl_x = cx.saturating_add(LABEL_W);
    let ctl_w = cw.saturating_sub(LABEL_W + 2);

    for i in 0..FIELDS {
        let label = &label_text(i);
        let dy = 1 + i as u16;
        let focused = form.focus == i;
        let row = at(inner, dy, cx, cw);
        layout.rows[i] = row;

        let label_style = if focused {
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        let marker = if focused { "> " } else { "  " };
        render_clipped(
            frame,
            Paragraph::new(format!("{marker}{label}")).style(label_style),
            at(inner, dy, cx, LABEL_W),
        );

        let ctl = at(inner, dy, ctl_x, ctl_w);
        match i {
            DIR | SIDE => {
                let (names, selected): (Vec<&str>, usize) = if i == DIR {
                    (
                        DIRS.iter().map(|d| choice_text(d.1)).collect(),
                        DIRS.iter().position(|d| d.0 == form.dir).unwrap_or(0),
                    )
                } else {
                    (
                        SIDES.iter().map(|d| choice_text(d.1)).collect(),
                        SIDES.iter().position(|d| d.0 == form.side).unwrap_or(0),
                    )
                };
                let mut spans: Vec<Span> = Vec::new();
                let mut off: u16 = 0;
                for (k, n) in names.iter().enumerate() {
                    let on = k == selected;
                    let mark = if on {
                        glyphs::RADIO_ON
                    } else {
                        glyphs::RADIO_OFF
                    };
                    let w = (mark.chars().count() + 1 + n.chars().count()) as u16;
                    let r = at(inner, dy, ctl_x.saturating_add(off), w);
                    if i == DIR {
                        layout.dir_opts[k] = r;
                    } else {
                        layout.side_opts[k] = r;
                    }
                    let style = if on {
                        Style::default()
                            .fg(palette.accent)
                            .bg(palette.bg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.light_cyan).bg(palette.bg)
                    };
                    spans.push(Span::styled(format!("{mark} {n}"), style));
                    spans.push(Span::raw("   "));
                    off = off.saturating_add(w + 3);
                }
                render_clipped(
                    frame,
                    Paragraph::new(Line::from(spans))
                        .style(Style::default().fg(palette.text).bg(palette.bg)),
                    ctl,
                );
            }
            _ => {
                if let Some(st) = form.input(i) {
                    input::render(frame, ctl, st, focused, palette);
                }
            }
        }

        let bad = checked.errors[i].is_some()
            || form.external_error.as_ref().is_some_and(|(k, _)| *k == i);
        if bad {
            render_clipped(
                frame,
                Paragraph::new("!").style(
                    Style::default()
                        .fg(palette.error)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                at(inner, dy, ctl_x.saturating_add(ctl_w).saturating_add(1), 1),
            );
        }
    }

    // Hint for the focused field, or its error.
    let external = form
        .external_error
        .as_ref()
        .filter(|(k, _)| *k == form.focus)
        .map(|(_, m)| m.as_str());
    let error = external.or(checked.errors.get(form.focus).and_then(|e| e.as_deref()));
    let (msg, color) = match error {
        Some(e) => (e.to_string(), palette.error),
        None => (
            hint_text(form.focus.min(STOPS - 1)).to_string(),
            palette.muted,
        ),
    };
    render_clipped(
        frame,
        Paragraph::new(truncate(&msg, cw as usize).to_string())
            .style(Style::default().fg(color).bg(palette.bg)),
        at(inner, 8, cx, cw),
    );

    // Preview.
    let preview = match &checked.spec {
        Some(spec) => {
            let side = match spec.side.unwrap_or_default().tag() {
                "" => String::new(),
                t => format!(", {t}"),
            };
            let name = spec
                .name
                .clone()
                .unwrap_or_else(|| dynamic_name(&spec.rule));
            crate::i18n::pick(
                format!("Group \"{name}\": {}{side}", spec.rule.label()),
                format!("Група \"{name}\": {}{side}", spec.rule.label()),
            )
        }
        None => crate::i18n::pick(
            "Fix the marked fields to continue.",
            "Виправ позначені поля, щоб продовжити.",
        )
        .to_string(),
    };
    render_clipped(
        frame,
        Paragraph::new(truncate(&preview, cw as usize).to_string()).style(
            Style::default()
                .fg(palette.light_cyan)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        at(inner, 9, cx, cw),
    );
    render_clipped(
        frame,
        Paragraph::new(
            truncate(
                crate::i18n::pick(
                    "Tab next  ←→ choose  Enter OK  Esc cancel",
                    "Tab далі  ←→ вибір  Enter ОК  Esc скасувати",
                ),
                cw as usize,
            )
            .to_string(),
        )
        .style(Style::default().fg(palette.muted).bg(palette.bg)),
        at(inner, 10, cx, cw),
    );

    // Buttons, centred.
    let ok_label = if form.editing {
        crate::i18n::pick("Save", "Зберегти")
    } else {
        crate::i18n::pick("Create", "Створити")
    };
    let cancel_label = crate::i18n::pick("Cancel", "Скасувати");
    let (w_ok, w_cancel) = (button::measure(ok_label), button::measure(cancel_label));
    let total = w_ok.saturating_add(2).saturating_add(w_cancel);
    let bx = inner
        .x
        .saturating_add(inner.width.saturating_sub(total) / 2);
    let by = inner.y.saturating_add(11);
    let ok_rect = Rect {
        x: bx,
        y: by,
        width: w_ok,
        height: button::HEIGHT,
    }
    .intersection(inner);
    let cancel_rect = Rect {
        x: bx.saturating_add(w_ok).saturating_add(2),
        y: by,
        width: w_cancel,
        height: button::HEIGHT,
    }
    .intersection(inner);
    layout.ok = ok_rect;
    layout.cancel = cancel_rect;
    let hot = |r: Rect| mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
    let ok_disabled = checked.spec.is_none();
    button::render(
        frame,
        ok_rect,
        ok_label,
        button::pick_state(ok_disabled, false, form.focus == OK, hot(ok_rect), true),
        palette,
    );
    button::render(
        frame,
        cancel_rect,
        cancel_label,
        button::pick_state(false, false, form.focus == CANCEL, hot(cancel_rect), false),
        palette,
    );

    form.layout.set(layout);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(f: &mut RuleForm, s: &str) {
        for c in s.chars() {
            handle_key(f, KeyCode::Char(c));
        }
    }

    #[test]
    fn new_form_is_valid_and_builds_a_spec() {
        let f = RuleForm::new(None);
        let c = f.check();
        assert!(c.errors.iter().all(Option::is_none));
        let s = c.spec.unwrap();
        assert_eq!(s.rule.window_min, 15);
        assert_eq!(s.rule.threshold_pct, 5.0);
        assert_eq!(s.rule.hold_min, 5);
        assert_eq!(s.name, None);
        assert_eq!(s.side, Some(Side::Any));
    }

    #[test]
    fn edit_form_round_trips_the_rule() {
        let rule = MoveRule::parse("20 7.5 down 3 vol=2M").unwrap();
        let f = RuleForm::new(Some(("Dumps", &rule, Side::Long)));
        let s = f.check().spec.unwrap();
        assert_eq!(s.rule, rule);
        assert_eq!(s.name.as_deref(), Some("Dumps"));
        assert_eq!(s.side, Some(Side::Long));
    }

    #[test]
    fn typing_is_filtered_and_bad_values_are_flagged() {
        let mut f = RuleForm::new(None);
        f.focus = WINDOW;
        handle_key(&mut f, KeyCode::Backspace);
        handle_key(&mut f, KeyCode::Backspace);
        typed(&mut f, "x9z9");
        assert_eq!(f.window.text, "99");
        let c = f.check();
        assert!(c.errors[WINDOW].is_some(), "99 min is out of range");
        assert!(c.spec.is_none());
        f.focus = NAME;
        f.focus_first_error();
        assert_eq!(f.focus, WINDOW);

        f.focus = VOLUME;
        typed(&mut f, "5x M");
        assert_eq!(f.volume.text, "5M");
        assert_eq!(f.check().errors[VOLUME], None);
    }

    #[test]
    fn keys_move_focus_and_choose_options() {
        let mut f = RuleForm::new(None);
        f.focus = DIR;
        handle_key(&mut f, KeyCode::Right);
        assert_eq!(f.dir, MoveDir::Down);
        handle_key(&mut f, KeyCode::Char('b'));
        assert_eq!(f.dir, MoveDir::Either);
        f.focus = SIDE;
        handle_key(&mut f, KeyCode::Char('s'));
        assert_eq!(f.side, Side::Short);
        f.focus = CANCEL;
        handle_key(&mut f, KeyCode::Tab);
        assert_eq!(f.focus, NAME, "Tab wraps");
        handle_key(&mut f, KeyCode::BackTab);
        assert_eq!(f.focus, CANCEL);
        assert_eq!(handle_key(&mut f, KeyCode::Enter), FormAction::Cancel);
        f.focus = PCT;
        assert_eq!(handle_key(&mut f, KeyCode::Enter), FormAction::Submit);
        assert_eq!(handle_key(&mut f, KeyCode::Esc), FormAction::Cancel);
    }

    #[test]
    fn long_name_is_refused() {
        let mut f = RuleForm::new(None);
        f.focus = NAME;
        typed(&mut f, &"n".repeat(MAX_NAME_CHARS + 1));
        assert!(f.check().errors[NAME].is_some());
    }
}
