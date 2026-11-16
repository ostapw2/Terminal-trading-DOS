//! The Compare page as "list -> chart -> what to do" (`crate::ideas`).
//!
//! ```text
//! What stands out ... (the question)          the answer in one line
//! FOUND (Up/Down or click)       | [ ACT ] Buy AVAX, sell SOL: the gap tends to close
//! [PAIR ] buy AVAX / sell SOL  ..| +-- chart with the plan's levels ---------------+
//! [DIP  ] BNB          -2.6 sd ..| |  -- target  -- stop  -- entry                 |
//! [PUMP ] WIF     +6% in 15m   ..| +-----------------------------------------------+
//!                                | 1. ...  2. ...  3. ...  4. ...
//!                                | Enter Open on chart   t Trade   e Test it
//! ```
//!
//! The widget draws and says where its rows and its action bar are; the
//! chart comes from `markets::render_pane` with the plan's levels.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    Frame,
};

use crate::i18n::pick;
use crate::ideas::{Idea, Kind, LevelKind, Plan, Verdict};
use crate::markets::Ohlc;
use crate::tokens::Palette;
use crate::widgets::chart::{ChartType, RefLine};
use crate::widgets::compare::wrap;
use crate::widgets::fmt::{rect_contains, render_clipped, truncate, BEAR, BULL};

/// Narrower than this, the list goes on top and the preview under it.
pub const STACK_BELOW: u16 = 100;

/// Where the clickable parts are.
#[derive(Clone, Debug, Default)]
pub struct IdeasLayout {
    /// A row of the list and the idea's index.
    pub rows: Vec<(Rect, usize)>,
    /// The row the caller draws the action buttons into.
    pub actions: Rect,
}

/// The selected idea's preview: its candles (when they are in), why they
/// are not (loading, an error), and the plan made from them.
pub struct Preview<'a> {
    pub candles: Option<&'a [Ohlc]>,
    pub note: &'a str,
    pub plan: Option<&'a Plan>,
}

pub fn kind_color(k: Kind, palette: &Palette) -> Color {
    match k {
        Kind::Pair => Color::LightCyan,
        Kind::Pump | Kind::High => BULL,
        Kind::Dump | Kind::Dip | Kind::Knife => BEAR,
        Kind::Chart => palette.accent,
        Kind::Link | Kind::Live => Color::LightMagenta,
        Kind::Rule => Color::LightBlue,
        Kind::Gain => BULL,
        Kind::Loss => BEAR,
        Kind::Lag => Color::LightYellow,
        Kind::Group | Kind::Spread | Kind::Syn => Color::LightCyan,
        Kind::SynPair => Color::LightMagenta,
        Kind::Parity | Kind::Box => Color::LightBlue,
    }
}

pub fn verdict_color(v: Verdict, palette: &Palette) -> Color {
    match v {
        Verdict::Act => BULL,
        Verdict::Watch => palette.warn,
        Verdict::Avoid => BEAR,
        Verdict::Info => Color::LightCyan,
    }
}

/// The plan's levels as chart lines.
pub fn ref_lines(plan: &Plan, palette: &Palette) -> Vec<RefLine> {
    plan.levels
        .iter()
        .map(|l| RefLine {
            value: l.value,
            color: match l.kind {
                LevelKind::Entry => palette.accent,
                LevelKind::Target => BULL,
                LevelKind::Stop => BEAR,
                LevelKind::Band => palette.muted,
            },
            label: l.label.clone(),
        })
        .collect()
}

/// Write `text` on row `y` of `area`, cut to its width.
fn put(frame: &mut Frame, area: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    if y >= area.height || x >= area.width {
        return 0;
    }
    let w = (area.width - x) as usize;
    let t = truncate(text, w);
    let n = t.chars().count() as u16;
    let buf = frame.buffer_mut();
    for (i, ch) in t.chars().enumerate() {
        if let Some(cell) = buf.cell_mut((area.x + x + i as u16, area.y + y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
    n
}

/// Draw the page body; returns its clickable parts.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    area: Rect,
    question: &str,
    answer: &str,
    empty: &str,
    how: &str,
    ideas: &[Idea],
    sel: usize,
    preview: &Preview,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> IdeasLayout {
    let bg = Style::default().fg(palette.text).bg(palette.bg);
    render_clipped(frame, ratatui::widgets::Block::default().style(bg), area);
    let mut out = IdeasLayout::default();
    if area.width < 30 || area.height < 8 {
        return out;
    }
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    put(frame, area, 0, 0, question, muted);
    put(
        frame,
        area,
        0,
        1,
        answer,
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    let how_lines = wrap(how, area.width.saturating_sub(1) as usize);
    for (i, l) in how_lines.iter().take(2).enumerate() {
        put(frame, area, 0, 2 + i as u16, l, muted);
    }
    let used = 3 + how_lines.len().min(2) as u16;
    let body = Rect {
        y: area.y + used,
        height: area.height - used,
        ..area
    };

    // The list on the left; in a narrow window on top, the preview under it.
    let stacked = body.width < STACK_BELOW;
    let list_w = if stacked {
        body.width
    } else {
        (body.width * 2 / 5).clamp(30, 56).min(body.width)
    };
    let list_h = if stacked {
        // Under 40 % of the height the list is a keyhole; it scrolls anyway.
        (ideas.len().max(1) as u16 + 2)
            .min(body.height * 2 / 3)
            .max(body.height * 2 / 5)
            .max(6)
            .min(body.height)
    } else {
        body.height
    };
    let list = Rect {
        width: list_w,
        height: list_h,
        ..body
    };
    put(
        frame,
        list,
        0,
        0,
        pick("FOUND  (Up/Down, click)", "ЗНАЙДЕНО  (вгору/вниз, клік)"),
        Style::default()
            .fg(palette.text)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    let sel = sel.min(ideas.len().saturating_sub(1));
    if ideas.is_empty() {
        for (i, l) in wrap(empty, list_w.saturating_sub(1) as usize)
            .into_iter()
            .enumerate()
            .take(list.height.saturating_sub(2) as usize)
        {
            put(frame, list, 0, 2 + i as u16, &l, muted);
        }
    }
    // Keep the selection in view.
    let rows = list.height.saturating_sub(2) as usize;
    let first = if sel >= rows { sel + 1 - rows } else { 0 };
    for (k, idea) in ideas.iter().enumerate().skip(first).take(rows) {
        let y = 1 + (k - first) as u16;
        let r = Rect {
            x: list.x,
            y: list.y + y,
            width: list_w.saturating_sub(1),
            height: 1,
        };
        let hover = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
        let row_style = if k == sel {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else if hover {
            Style::default().fg(palette.text).bg(palette.accent_dim)
        } else {
            bg
        };
        // The whole row in its style first, then the parts.
        put(frame, list, 0, y, &" ".repeat(r.width as usize), row_style);
        let tag = format!(" {:<5} ", idea.kind().tag());
        let mut x = put(
            frame,
            list,
            0,
            y,
            &tag,
            Style::default()
                .fg(palette.black)
                .bg(kind_color(idea.kind(), palette))
                .add_modifier(Modifier::BOLD),
        );
        x += put(frame, list, x, y, &format!(" {}", idea.title()), row_style);
        let facts = idea.facts();
        let fw = facts.chars().count() as u16;
        if r.width > x + fw + 2 {
            let st = if k == sel {
                row_style
            } else {
                Style::default()
                    .fg(palette.muted)
                    .bg(row_style.bg.unwrap_or(palette.bg))
            };
            put(frame, list, r.width - fw - 1, y, &facts, st);
        }
        out.rows.push((r, k));
    }
    if ideas.len() > rows && rows > 0 {
        put(
            frame,
            list,
            0,
            list.height - 1,
            &format!("{} of {}", sel + 1, ideas.len()),
            muted,
        );
    }

    // The preview on the right (under the list when stacked), past a divider.
    let right = if stacked {
        put(
            frame,
            body,
            0,
            list_h,
            &"─".repeat(body.width as usize),
            muted,
        );
        Rect {
            x: body.x.saturating_sub(1),
            y: body.y + list_h + 1,
            width: body.width + u16::from(body.x > 0),
            height: body.height.saturating_sub(list_h + 1),
        }
    } else {
        for y in 0..body.height {
            put(frame, body, list_w, y, "│", muted);
        }
        Rect {
            x: body.x + list_w + 1,
            width: body.width.saturating_sub(list_w + 1),
            ..body
        }
    };
    let Some(idea) = ideas.get(sel) else {
        return out;
    };
    if right.width < 20 || right.height < 6 {
        return out;
    }
    let right = Rect {
        x: right.x + 1,
        width: right.width - 1,
        ..right
    };
    let mut y = 0u16;
    match preview.plan {
        Some(plan) => {
            let badge = format!(" {} ", plan.verdict.label());
            let x = put(
                frame,
                right,
                0,
                y,
                &badge,
                Style::default()
                    .fg(palette.black)
                    .bg(verdict_color(plan.verdict, palette))
                    .add_modifier(Modifier::BOLD),
            );
            put(
                frame,
                right,
                x + 1,
                y,
                &plan.headline,
                Style::default()
                    .fg(palette.text)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        None => {
            put(frame, right, 0, y, preview.note, muted);
        }
    }
    y += 1;
    // Steps under the chart, the actions at the bottom.
    let steps: Vec<String> = preview.plan.map_or(Vec::new(), |p| {
        let w = right.width.saturating_sub(4) as usize;
        p.steps
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                wrap(s, w)
                    .into_iter()
                    .enumerate()
                    .map(move |(j, l)| {
                        if j == 0 {
                            format!("{}. {l}", i + 1)
                        } else {
                            format!("   {l}")
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    });
    let steps_h = (steps.len() as u16).min(right.height.saturating_sub(y + 10));
    let chart_h = right.height.saturating_sub(y + steps_h + 2);
    let (formula, tf) = idea.chart();
    if chart_h >= 6 {
        let chart = Rect {
            x: right.x,
            y: right.y + y,
            width: right.width,
            height: chart_h,
        };
        match preview.candles.filter(|c| !c.is_empty()) {
            Some(c) => {
                let refs = preview
                    .plan
                    .map(|p| ref_lines(p, palette))
                    .unwrap_or_default();
                let filters = crate::widgets::markets::SeriesFilters {
                    refs: &refs,
                    ..crate::widgets::markets::SeriesFilters::NONE
                };
                let kind = match idea.kind() {
                    Kind::Pair | Kind::Chart | Kind::Link | Kind::Rule => ChartType::Line,
                    _ => ChartType::Candle,
                };
                let title = match idea.kind() {
                    Kind::Pair => format!("{} gap, sd", idea.title()),
                    Kind::Chart => idea.title(),
                    _ => formula.trim_start_matches('=').to_string(),
                };
                let tf = crate::markets::Timeframe::ALL
                    .iter()
                    .copied()
                    .find(|t| t.name() == tf)
                    .unwrap_or(crate::markets::Timeframe::H1);
                crate::widgets::markets::render_pane(
                    frame,
                    chart,
                    &title,
                    c,
                    &[],
                    &filters,
                    0,
                    1,
                    1,
                    tf,
                    kind,
                    true,
                    palette,
                );
            }
            None => {
                let b = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(muted)
                    .style(bg);
                render_clipped(frame, b, chart);
                let note = if preview.note.is_empty() {
                    pick("loading...", "завантаження...")
                } else {
                    preview.note
                };
                put(frame, chart, 2, chart_h / 2, note, muted);
            }
        }
        y += chart_h;
    }
    for l in steps.iter().take(steps_h as usize) {
        put(frame, right, 0, y, l, bg);
        y += 1;
    }
    out.actions = Rect {
        x: right.x,
        y: right.y + right.height - 1,
        width: right.width,
        height: 1,
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_at_every_size() {
        let palette = crate::tokens::CLASSIC;
        let ideas = crate::ideas::presets();
        let candles: Vec<Ohlc> = (0..200)
            .map(|i| {
                let x = 100.0 + (i % 9) as f64;
                Ohlc {
                    open: x,
                    high: x + 1.0,
                    low: x - 1.0,
                    close: x,
                    volume: 1.0,
                    date: String::new(),
                    time_ms: 0,
                    is_gap: false,
                }
            })
            .collect();
        let plan = crate::ideas::plan(&ideas[0], &candles, 25.0);
        for (w, h) in [(1, 1), (20, 5), (40, 10), (60, 40), (90, 30), (160, 45)] {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| {
                let p = Preview {
                    candles: Some(&candles),
                    note: "",
                    plan: Some(&plan),
                };
                let l = render(
                    f,
                    f.area(),
                    "q",
                    "a",
                    "none",
                    "how",
                    &ideas,
                    3,
                    &p,
                    None,
                    &palette,
                );
                if w >= 60 {
                    assert!(!l.rows.is_empty());
                    assert!(l.actions.width > 0, "actions at {w}x{h}");
                }
                let empty = Preview {
                    candles: None,
                    note: "loading",
                    plan: None,
                };
                render(
                    f,
                    f.area(),
                    "q",
                    "a",
                    "none",
                    "how",
                    &[],
                    0,
                    &empty,
                    None,
                    &palette,
                );
            })
            .unwrap();
        }
    }
}
