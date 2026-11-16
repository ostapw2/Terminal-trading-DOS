//! `:find` — the instrument reference (`market.db`): every listed
//! instrument of every venue, searched by a piece of the name, one page at a
//! time (the catalogue holds thousands; a screen reads only what it shows).

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::data::marketdb::Found;
use crate::i18n::pick;
use crate::tokens::Palette;
use crate::widgets::fmt::{centered, rect_contains, render_clipped, truncate};
use crate::widgets::input::{self, InputState};
use crate::widgets::screener::fmt_usdt;

/// What the window shows: the query, the page read for it, the highlight.
#[derive(Default, Clone)]
pub struct FindState {
    pub input: InputState,
    /// The page of matches read last.
    pub rows: Vec<Found>,
    /// All matches of the query.
    pub total: i64,
    /// Index of the page's first row among all matches.
    pub offset: usize,
    /// Highlighted row within the page.
    pub sel: usize,
}

impl FindState {
    pub fn selected(&self) -> Option<&Found> {
        self.rows.get(self.sel)
    }
}

/// Where things were drawn; rows carry the instrument's id (hard rule 13).
#[derive(Default, Clone)]
pub struct FindLayout {
    pub area: Rect,
    pub close: Rect,
    pub rows: Vec<(Rect, i64)>,
    /// Rows a page holds at this size.
    pub page: usize,
}

impl FindLayout {
    /// The instrument a click landed on.
    pub fn id_at(&self, x: u16, y: u16) -> Option<i64> {
        self.rows
            .iter()
            .find(|(r, _)| rect_contains(*r, x, y))
            .map(|(_, id)| *id)
    }
}

const W: u16 = 78;
const H: u16 = 30;
/// Lines that are not rows: query, header, footer.
const CHROME: u16 = 3;

/// How many rows a page holds on `screen` (the caller reads that many).
pub fn page_rows(screen: Rect) -> usize {
    centered(screen, W, H).height.saturating_sub(2 + CHROME) as usize
}

fn asset_uk(a: &str) -> &str {
    match a {
        "crypto" => "монета",
        "stock" => "акція",
        "commodity" => "сировина",
        "fx" => "валюта",
        "index" => "індекс",
        _ => "інше",
    }
}

pub fn render(
    frame: &mut Frame,
    screen: Rect,
    st: &FindState,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> FindLayout {
    let area = centered(screen, W, H);
    render_clipped(frame, Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            pick(" Instruments ", " Інструменти "),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let close = Rect {
        x: area.x + area.width.saturating_sub(5),
        y: area.y,
        width: 4,
        height: 1,
    }
    .intersection(area);
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

    // The query.
    let q = line(0);
    let label = pick("Find: ", "Шукати: ");
    let lw = (label.chars().count() as u16).min(q.width);
    render_clipped(
        frame,
        Paragraph::new(Span::styled(label, muted)),
        Rect { width: lw, ..q },
    );
    input::render(
        frame,
        Rect {
            x: q.x + lw,
            width: q.width - lw,
            ..q
        },
        &st.input,
        true,
        palette,
    );

    let head = format!(
        "{:<9}{:<6}{:<10}{:<18}{:>8}",
        pick("Venue", "Біржа"),
        pick("Kind", "Тип"),
        pick("Asset", "Актив"),
        pick("Symbol", "Символ"),
        pick("$/day", "$/добу"),
    );
    render_clipped(
        frame,
        Paragraph::new(Span::styled(truncate(&head, w).to_string(), muted)),
        line(1),
    );

    let page = inner.height.saturating_sub(CHROME) as usize;
    let mut layout = FindLayout {
        area,
        close,
        rows: Vec::new(),
        page,
    };
    for (i, f) in st.rows.iter().take(page).enumerate() {
        let r = line(2 + i as u16);
        if r.height == 0 {
            break;
        }
        layout.rows.push((r, f.id));
        let hot = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
        let style = if i == st.sel {
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if hot {
            Style::default().fg(palette.text).bg(palette.panel)
        } else if !f.active {
            muted
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        let asset = pick(f.asset.as_str(), asset_uk(&f.asset));
        let vol = if f.vol_usd > 0.0 {
            fmt_usdt(f.vol_usd)
        } else {
            "-".into()
        };
        let mut text = format!(
            "{:<9}{:<6}{:<10}{:<18}{:>8}",
            truncate(&f.venue, 8),
            truncate(&f.kind, 5),
            truncate(asset, 9),
            truncate(&f.symbol, 17),
            vol
        );
        if !f.active {
            text.push_str(pick("  not trading", "  не торгується"));
        }
        render_clipped(
            frame,
            Paragraph::new(Span::styled(format!("{:<w$}", truncate(&text, w)), style)),
            r,
        );
    }
    if st.rows.is_empty() {
        let msg = if st.total == 0 && st.input.text.trim().is_empty() {
            pick(
                "The reference is empty: it fills from the scanner and the venues' listings.",
                "Довідник порожній: його наповнюють сканер і списки бірж.",
            )
        } else {
            pick("Nothing matches.", "Нічого не знайдено.")
        };
        render_clipped(
            frame,
            Paragraph::new(Span::styled(truncate(msg, w).to_string(), muted)),
            line(2),
        );
    }

    let from = if st.rows.is_empty() { 0 } else { st.offset + 1 };
    let to = st.offset + st.rows.len().min(page);
    let foot = Line::from(vec![Span::styled(
        truncate(
            &pick(
                format!(
                    "{from}-{to} of {} · Enter chart · Tab add to list · PgUp/PgDn · Esc",
                    st.total
                ),
                format!(
                    "{from}-{to} з {} · Enter графік · Tab у список · PgUp/PgDn · Esc",
                    st.total
                ),
            ),
            w,
        )
        .to_string(),
        muted,
    )]);
    render_clipped(
        frame,
        Paragraph::new(foot),
        line(inner.height.saturating_sub(1)),
    );
    layout
}
