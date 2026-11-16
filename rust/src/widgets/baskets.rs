//! The baskets block of F7 (`crate::basket`): one row per basket — its
//! state, legs held, dollars, net PnL, fees, age — and a header with the
//! totals.  Rows carry the basket's id (never an index) for the caller.

use crate::i18n::pick;
use std::collections::HashMap;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};

use crate::basket::{Book, State};
use crate::tokens::Palette;
use crate::widgets::fmt::{pnl_color, render_clipped, truncate};

/// Rows the block wants: the header line plus one per basket (closed ones
/// after the open ones), at most `max`.
pub fn rows_wanted(book: &Book, max: usize) -> usize {
    if book.baskets.is_empty() {
        0
    } else {
        (book.baskets.len() + 1).min(max)
    }
}

/// Draw the block into `area` (no border: the caller frames it).  Returns
/// (row rect, basket id) for every row drawn.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    book: &Book,
    marks: &HashMap<String, f64>,
    now_ms: i64,
    palette: &Palette,
) -> Vec<(Rect, u64)> {
    let mut hits = Vec::new();
    if area.width == 0 || area.height == 0 || book.baskets.is_empty() {
        return hits;
    }
    let bg = Style::default().fg(palette.text).bg(palette.bg);
    let mut net = 0.0;
    let mut fees = 0.0;
    for b in book.baskets.iter().filter(|b| !b.state.is_closed()) {
        let p = b.pnl(marks);
        net += p.net();
        fees += p.fees;
    }
    let header = pick(
        format!(
            "BASKETS  {} open  net {}{:.2}  fees ${:.2}   (exits held by the trading window, not the exchange)",
            book.open_count(),
            if net >= 0.0 { "+$" } else { "-$" },
            net.abs(),
            fees
        ),
        format!(
            "КОШИКИ  відкрито {}  чисто {}{:.2}  комісії ${:.2}   (виходи тримає торгове вікно, а не біржа)",
            book.open_count(),
            if net >= 0.0 { "+$" } else { "-$" },
            net.abs(),
            fees
        ),
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, area.width as usize)).style(
            Style::default()
                .fg(palette.accent_dim)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect { height: 1, ..area },
    );
    // Open ones first, newest first; then the closed ones, newest first.
    let mut order: Vec<&crate::basket::Basket> = book.baskets.iter().collect();
    order.sort_by_key(|b| (b.state.is_closed(), std::cmp::Reverse(b.id)));
    let visible = area.height.saturating_sub(1) as usize;
    let off = crate::widgets::dashboard::take_offset(
        crate::widgets::dashboard::Zone::Baskets,
        area,
        order.len(),
        visible,
    );
    crate::widgets::dashboard::corner(
        frame,
        Rect { height: 1, ..area },
        &crate::widgets::dashboard::scroll_hint(off, visible, order.len()),
        palette,
    );
    for (i, b) in order.iter().enumerate().skip(off) {
        let i = i - off;
        let y = area.y + 1 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let p = b.pnl(marks);
        let age_min = match b.state {
            State::Entering { since_ms, .. }
            | State::Open { since_ms }
            | State::Exiting { since_ms, .. } => (now_ms - since_ms) / 60_000,
            State::Closed { at_ms, .. } => (now_ms - at_ms) / 60_000,
        };
        let age = if age_min >= 60 {
            format!("{}h{:02}", age_min / 60, age_min % 60)
        } else {
            format!("{age_min}m")
        };
        let total = b.legs.len();
        let legs = match b.state {
            State::Entering { .. } => gauge(b.entered_legs(), total),
            State::Exiting { .. } => gauge(
                total - b.legs.iter().filter(|l| !l.flat()).count().min(total),
                total,
            ),
            _ => gauge(total, total),
        };
        let state = match (&b.state, &b.blocked) {
            (_, Some(_)) => pick("BLOCKED", "ЗАБЛОК.").to_string(),
            (State::Closed { why, .. }, _) => {
                format!("{} {}", pick("CLOSED", "ЗАКРИТО"), state_word(why.label()))
            }
            (s, _) => state_word(s.label()).to_string(),
        };
        let line = format!(
            "#{:<3} {:<22} {:<14} {:<15} {:>9} {:>9} ({:+.1}%)  {} {:>6.2}  {:>5}",
            b.id,
            truncate(&b.name, 22),
            state,
            legs,
            format!("${:.2}", b.total_usd()),
            format!(
                "{}{:.2}",
                if p.net() >= 0.0 { "+$" } else { "-$" },
                p.net().abs()
            ),
            p.pct(),
            pick("fees", "комісії"),
            p.fees,
            age
        );
        let style = if b.blocked.is_some() {
            Style::default().fg(palette.accent).bg(palette.bg)
        } else if b.state.is_closed() {
            Style::default().fg(palette.muted).bg(palette.bg)
        } else {
            bg.fg(pnl_color(p.net()))
        };
        let r = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        render_clipped(
            frame,
            Paragraph::new(truncate(&line, area.width as usize)).style(style),
            r,
        );
        hits.push((r, b.id));
    }
    hits
}

/// A basket's state or exit reason in the chosen language.
fn state_word(en: &'static str) -> &'static str {
    let uk = match en {
        "ENTERING" => "ВХІД",
        "OPEN" => "ВІДКРИТО",
        "EXITING" => "ВИХІД",
        "CLOSED" => "ЗАКРИТО",
        "target" => "ціль",
        "stop" => "стоп",
        "time" => "час",
        "closed" => "вручну",
        "dropped" => "скинуто",
        other => other,
    };
    pick(en, uk)
}

/// `████░░░░ 12/40`: how many legs are in (or out, while exiting).
fn gauge(done: usize, total: usize) -> String {
    const W: usize = 8;
    let full = (done * W)
        .checked_div(total)
        .map_or(0, |_| (done * W).div_ceil(total))
        .min(W);
    format!(
        "{}{} {done}/{total}",
        "█".repeat(full),
        "░".repeat(W - full)
    )
}
