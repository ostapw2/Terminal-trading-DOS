//! F5 — watchlists: named symbol lists on the left, the active list's
//! symbols with live price / 24h change / volume on the right.

use crate::i18n::pick;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::binance::TickerSummary;
use crate::tokens::Palette;
use crate::watchlists::{Kind, MoveDir, PriceTracker, ScanKind, Watchlist, Watchlists};
use crate::widgets::compare::{render_head, Tone};
use crate::widgets::fmt::{fmt_price_col, render_clipped, truncate};
use crate::widgets::screener::fmt_usdt;

const LEFT_W: u16 = 28;

/// Hit-test rows; identity (list / row index) is resolved by the handler.
#[derive(Default)]
pub struct ListsLayout {
    pub list_rects: Vec<(Rect, usize)>,
    pub row_rects: Vec<(Rect, usize)>,
    /// Free area under the symbol table (empty when too small); the caller
    /// draws the synthetic chart there.
    pub chart: Rect,
}

fn panel<'a>(title: String, palette: &Palette) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg))
}

fn fill_row(frame: &mut Frame, row: Rect, style: Style) {
    let buf = frame.buffer_mut();
    for x in row.x..row.x.saturating_add(row.width) {
        if let Some(cell) = buf.cell_mut((x, row.y)) {
            cell.set_char(' ');
            cell.set_style(style);
        }
    }
}

/// First visible index so that `sel` stays inside a window of `rows`.
fn window_start(sel: usize, total: usize, rows: usize) -> usize {
    if total <= rows {
        0
    } else {
        sel.saturating_sub(rows / 2).min(total - rows)
    }
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    lists: &Watchlists,
    pairs: &[TickerSummary],
    tracker: &PriceTracker,
    now_ms: i64,
    chart_share: bool,
    palette: &Palette,
) -> ListsLayout {
    render_with(
        frame,
        area,
        lists,
        pairs,
        tracker,
        now_ms,
        chart_share,
        true,
        palette,
    )
}

/// The watch page of a desk: the active list's symbols on the whole area,
/// without the column of lists and the chart.
pub fn render_watch(
    frame: &mut Frame,
    area: Rect,
    lists: &Watchlists,
    pairs: &[TickerSummary],
    tracker: &PriceTracker,
    now_ms: i64,
    palette: &Palette,
) -> ListsLayout {
    render_with(
        frame, area, lists, pairs, tracker, now_ms, false, false, palette,
    )
}

fn render_with(
    frame: &mut Frame,
    area: Rect,
    lists: &Watchlists,
    pairs: &[TickerSummary],
    tracker: &PriceTracker,
    now_ms: i64,
    chart_share: bool,
    with_lists: bool,
    palette: &Palette,
) -> ListsLayout {
    let mut layout = ListsLayout::default();
    let left_w = if with_lists {
        LEFT_W.min(area.width / 2)
    } else {
        0
    };
    let left = Rect {
        width: left_w,
        ..area
    };
    let right_full = Rect {
        x: area.x.saturating_add(left_w),
        width: area.width.saturating_sub(left_w),
        ..area
    };
    // With the chart on, the table keeps the top 40% (at least 8 rows).
    let (right, chart) = if chart_share && right_full.height >= 8 + 12 {
        let table_h = (right_full.height * 2 / 5).max(8);
        (
            Rect {
                height: table_h,
                ..right_full
            },
            Rect {
                y: right_full.y + table_h,
                height: right_full.height - table_h,
                ..right_full
            },
        )
    } else {
        (right_full, Rect::default())
    };
    layout.chart = chart;

    // ── left: the lists ──
    if with_lists {
        render_lists_column(frame, left, lists, &mut layout, palette);
    }
    render_symbols(
        frame,
        right,
        lists,
        pairs,
        tracker,
        now_ms,
        with_lists,
        &mut layout,
        palette,
    );
    layout
}

fn render_lists_column(
    frame: &mut Frame,
    left: Rect,
    lists: &Watchlists,
    layout: &mut ListsLayout,
    palette: &Palette,
) {
    let block = panel(format!(" {} ", pick("Lists", "Списки")), palette);
    let inner = block.inner(left);
    render_clipped(frame, block, left);
    let rows = inner.height as usize;
    let start = window_start(lists.active_index(), lists.lists().len(), rows);
    for (i, l) in lists.lists().iter().enumerate().skip(start).take(rows) {
        let row = Rect {
            x: inner.x,
            y: inner.y + (i - start) as u16,
            width: inner.width,
            height: 1,
        };
        let active = i == lists.active_index();
        let style = if active {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        fill_row(frame, row, style);
        let mark = if l.rule().is_some() { "~" } else { " " };
        let side = match l.side.tag() {
            "" => String::new(),
            t => format!(" {t}"),
        };
        let label = format!("{mark}{} ({}){side}", l.name, l.symbols.len());
        render_clipped(
            frame,
            Paragraph::new(truncate(&label, row.width as usize).to_string()).style(style),
            row,
        );
        layout.list_rects.push((row, i));
    }
}

/// The active list in words: the question it answers, the answer now, and
/// how to read and use it.
pub fn story(
    l: &Watchlist,
    pairs: &[TickerSummary],
    tracker: &PriceTracker,
    now_ms: i64,
) -> (String, String, String) {
    let short = |s: &str| s.trim_end_matches("USDT").to_string();
    let names = |v: &[String]| {
        let shown: Vec<String> = v.iter().take(6).map(|s| short(s)).collect();
        let more = v.len().saturating_sub(6);
        if more > 0 {
            pick(
                format!("{} and {more} more", shown.join(", ")),
                format!("{} і ще {more}", shown.join(", ")),
            )
        } else {
            shown.join(", ")
        }
    };
    let n = l.symbols.len();
    match l.kind {
        Kind::Manual => {
            let q = pick(
                format!("How are the coins of '{}' doing?", l.name),
                format!("Як справи в монет зі списку '{}'?", l.name),
            );
            let mut known: Vec<(&str, f64, f64)> = l
                .symbols
                .iter()
                .filter_map(|s| {
                    let p = pairs.iter().find(|p| &p.symbol == s)?;
                    Some((s.as_str(), p.change_pct, p.volume_quote))
                })
                .collect();
            let a = if n == 0 {
                pick("The list is empty.", "Список порожній.").to_string()
            } else if known.is_empty() {
                pick(
                    "No prices yet (they come with the 24h tickers).",
                    "Цін ще немає (прийдуть із 24-годинними тікерами).",
                )
                .to_string()
            } else {
                known.sort_by(|a, b| b.1.total_cmp(&a.1));
                let up = known.iter().filter(|k| k.1 > 0.0).count();
                let mid = known[known.len() / 2].1;
                let (best, worst) = (known[0], known[known.len() - 1]);
                let money = crate::data::liquidations::fmt_usd(known.iter().map(|k| k.2).sum());
                let mut a = pick(
                    format!(
                        "{up} of {} up in 24 hours, the middle one {mid:+.1}%; best {} {:+.1}%, worst {} {:+.1}%; {money} traded.",
                        known.len(),
                        short(best.0),
                        best.1,
                        short(worst.0),
                        worst.1
                    ),
                    format!(
                        "{up} з {} зросли за добу, середня {mid:+.1}%; найкраща {} {:+.1}%, найгірша {} {:+.1}%; торгів на {money}.",
                        known.len(),
                        short(best.0),
                        best.1,
                        short(worst.0),
                        worst.1
                    ),
                );
                if known.len() < n {
                    a.push_str(&pick(
                        format!(" {} without data.", n - known.len()),
                        format!(" {} без даних.", n - known.len()),
                    ));
                }
                a
            };
            let how = pick(
                "Your list: a adds, x removes. Trade it on F6 (Lists); Compare (2) shows how its coins move together.",
                "Твій список: a додає, x прибирає. Торгуй ним на F6 (Списки); Порівняння (2) показує, як монети рухаються разом.",
            )
            .to_string();
            (q, a, how)
        }
        Kind::Dynamic(r) => {
            let dir = match r.dir {
                MoveDir::Up => pick("up by", "угору на"),
                MoveDir::Down => pick("down by", "униз на"),
                MoveDir::Either => pick("by", "на"),
            };
            let vol = if r.min_volume > 0.0 {
                pick(
                    format!(
                        ", trading at least {} a day",
                        crate::data::liquidations::fmt_usd(r.min_volume)
                    ),
                    format!(
                        ", торгів щодоби від {}",
                        crate::data::liquidations::fmt_usd(r.min_volume)
                    ),
                )
            } else {
                String::new()
            };
            let q = pick(
                format!(
                    "Which coins moved {dir} more than {}% within {} minutes{vol}?",
                    r.threshold_pct, r.window_min
                ),
                format!(
                    "Які монети змінились {dir} понад {}% за {} хв{vol}?",
                    r.threshold_pct, r.window_min
                ),
            );
            let a = if tracker.backfilling && !tracker.is_warm(1, now_ms) {
                pick(
                    "Loading the last minutes of prices (a few seconds)...",
                    "Завантажую ціни за останні хвилини (кілька секунд)...",
                )
                .to_string()
            } else if !tracker.is_warm(r.window_min, now_ms) {
                pick(
                    format!(
                        "Not yet: {} of {} minutes of prices collected.",
                        tracker.warm_minutes(now_ms),
                        r.window_min
                    ),
                    format!(
                        "Ще ні: зібрано {} з {} хв цін.",
                        tracker.warm_minutes(now_ms),
                        r.window_min
                    ),
                )
            } else if n == 0 {
                pick("None right now.", "Зараз немає.").to_string()
            } else {
                let moves: Vec<String> = l
                    .symbols
                    .iter()
                    .take(5)
                    .map(|s| match tracker.change_pct(s, r.window_min, now_ms) {
                        Some(c) => format!("{} {c:+.1}%", short(s)),
                        None => short(s),
                    })
                    .collect();
                pick(
                    format!("{n} now: {}.", moves.join(", ")),
                    format!("Зараз {n}: {}.", moves.join(", ")),
                )
            };
            let hold = if r.hold_min > 0 {
                pick(
                    format!(
                        " A coin stays {} more minutes after the move fades.",
                        r.hold_min
                    ),
                    format!(
                        " Монета лишається ще {} хв після затухання руху.",
                        r.hold_min
                    ),
                )
            } else {
                String::new()
            };
            let how = pick(
                format!(
                    "Checked on every pair's prices as they come.{hold} E edits the rule. It shows what moved, not what will: test an idea on Compare (3) first."
                ),
                format!(
                    "Перевіряється на цінах кожної пари наживо.{hold} E змінює правило. Це що вже рухалось, а не що буде: спершу перевір ідею в Порівнянні (3)."
                ),
            );
            (q, a, how)
        }
        Kind::Scan(k) => {
            let (qe, qu) = match k {
                ScanKind::PairsLong => (
                    "Which coins are cheap against a coin they usually move with?",
                    "Які монети дешеві щодо монети, з якою зазвичай рухаються?",
                ),
                ScanKind::PairsShort => (
                    "Which coins are rich against a coin they usually move with?",
                    "Які монети дорогі щодо монети, з якою зазвичай рухаються?",
                ),
                ScanKind::StretchedUp => (
                    "Which coins are far above their own normal?",
                    "Які монети далеко вище власної норми?",
                ),
                ScanKind::StretchedDown => (
                    "Which coins are far below their own normal?",
                    "Які монети далеко нижче власної норми?",
                ),
                ScanKind::Knives => (
                    "Which coins fall hard, still fall, near the low, on heavy volume?",
                    "Які монети різко падають, ще падають, біля мінімуму, на великих обсягах?",
                ),
                ScanKind::Pumps => (
                    "Which coins jumped up fast on heavy volume?",
                    "Які монети швидко виросли на великих обсягах?",
                ),
                ScanKind::Dumps => (
                    "Which coins dropped fast on heavy volume?",
                    "Які монети швидко впали на великих обсягах?",
                ),
            };
            let q = pick(qe, qu).to_string();
            let a = if n == 0 {
                pick("None right now.", "Зараз немає.").to_string()
            } else {
                pick(
                    format!("{n} now: {}.", names(&l.symbols)),
                    format!("Зараз {n}: {}.", names(&l.symbols)),
                )
            };
            let (he, hu) = match k {
                ScanKind::PairsLong | ScanKind::PairsShort => (
                    "One leg of a pair whose gap is 2+ sd off normal and used to close. Buy 'pairs long' against 'pairs short'; Compare (6) t trades a pair with the right sizes.",
                    "Одна нога пари, чий розрив 2+ sd від норми й раніше закривався. Купуй 'pairs long' проти 'pairs short'; у Порівнянні (6) t торгує пару з правильними розмірами.",
                ),
                ScanKind::StretchedUp | ScanKind::StretchedDown => (
                    "2.5+ sd from the coin's last 50 hourly closes. Stretched can stretch further: a candidate, not a signal.",
                    "2.5+ sd від останніх 50 годинних закриттів. Розтягнуте може розтягнутись далі: кандидат, а не сигнал.",
                ),
                ScanKind::Knives => (
                    "Down 8%+ in a day, still falling in the last 4 hours, near the day's low, on 1.5x volume. Catching one is a bet on a bounce.",
                    "Мінус 8%+ за добу, ще падає останні 4 години, біля мінімуму дня, обсяг 1.5x. Ловити це ставка на відскок.",
                ),
                ScanKind::Pumps | ScanKind::Dumps => (
                    "3% in 5 min, 5% in 15 or 8% in an hour, on 2x the usual dollars; listed 30 min. Live, whole market.",
                    "3% за 5 хв, 5% за 15 або 8% за годину, на 2x звичайних доларів; у списку 30 хв. Наживо, увесь ринок.",
                ),
            };
            let how = pick(
                format!("{he} Filled by the scanner (Compare 6), not by hand."),
                format!("{hu} Заповнює сканер (Порівняння 6), не вручну."),
            );
            (q, a, how)
        }
    }
}

/// The active list's symbols with live price, move, 24h change and volume.
fn render_symbols(
    frame: &mut Frame,
    right: Rect,
    lists: &Watchlists,
    pairs: &[TickerSummary],
    tracker: &PriceTracker,
    now_ms: i64,
    with_story: bool,
    layout: &mut ListsLayout,
    palette: &Palette,
) {
    let cur = lists.current();
    let rule = cur.rule();
    let kind = match cur.kind {
        Kind::Manual => pick("manual", "вручну").to_string(),
        Kind::Dynamic(r) => format!("{}: {}", pick("dynamic", "динамічний"), r.label()),
        Kind::Scan(_) => pick(
            "filled by the scanner (Compare 6)",
            "заповнює сканер (Порівняння 6)",
        )
        .to_string(),
    };
    let side = match cur.side.tag() {
        "" => pick("any side", "будь-який бік"),
        t => t,
    };
    let block = panel(
        format!(
            " {} - {} {} - {} - {} ",
            cur.name,
            cur.symbols.len(),
            pick("symbols", "символів"),
            kind,
            side
        ),
        palette,
    );
    let inner = block.inner(right);
    render_clipped(frame, block, right);
    if inner.height == 0 {
        return;
    }
    // The list in words on top, when there is room for it and some rows.
    let inner = if with_story && inner.height >= 14 && inner.width >= 40 {
        let (q, a, how) = story(cur, pairs, tracker, now_ms);
        let head = Rect {
            x: inner.x + 1,
            width: inner.width - 2,
            height: 8,
            ..inner
        };
        let rest = render_head(frame, head, &q, &a, Tone::Plain, &how, palette);
        let used = rest.y - inner.y;
        Rect {
            y: rest.y,
            height: inner.height.saturating_sub(used),
            ..inner
        }
    } else {
        inner
    };

    let move_col = rule.map_or(String::new(), |r| {
        format!(
            "{}{} {}",
            r.window_min,
            pick("M", "хв"),
            pick("MOVE", "РУХ")
        )
    });
    let move_col = move_col.as_str();
    let move_w = if rule.is_some() { 9 } else { 0 };
    let header = format!(
        " {:<12} {:>10} {:>mw$}{}{:>9} {:>10}",
        pick("PAIR", "ПАРА"),
        pick("PRICE", "ЦІНА"),
        move_col,
        if rule.is_some() { " " } else { "" },
        pick("24H", "24Г"),
        pick("VOLUME 24H", "ОБСЯГ 24Г"),
        mw = move_w
    );
    render_clipped(
        frame,
        Paragraph::new(truncate(&header, inner.width as usize).to_string()).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect { height: 1, ..inner },
    );

    if cur.symbols.is_empty() {
        let warm = tracker.warm_minutes(now_ms);
        let hint = match rule {
            Some(_) if tracker.backfilling && !tracker.is_warm(1, now_ms) => pick(
                "Loading the last minutes of prices (a few seconds)...",
                "Завантажую ціни за останні хвилини (кілька секунд)...",
            )
            .to_string(),
            Some(r) if !tracker.is_warm(r.window_min, now_ms) => pick(
                format!(
                    "Collecting prices: {warm}m of {}m. Nothing can be measured before.",
                    r.window_min
                ),
                format!(
                    "Збираю ціни: {warm} з {} хв. Раніше міряти нічого.",
                    r.window_min
                ),
            ),
            Some(_) => pick(
                "No symbol matches the rule right now.",
                "Жоден символ зараз не підходить під правило.",
            )
            .to_string(),
            None => pick(
                "Empty: a adds a symbol (or mark pairs on F4 and press w).",
                "Порожньо: a додає символ (або познач пари на F4 і натисни w).",
            )
            .to_string(),
        };
        if inner.height > 2 {
            render_clipped(
                frame,
                Paragraph::new(truncate(&hint, inner.width.saturating_sub(2) as usize).to_string())
                    .style(Style::default().fg(palette.muted).bg(palette.bg)),
                Rect {
                    x: inner.x + 1,
                    y: inner.y + 2,
                    width: inner.width.saturating_sub(2),
                    height: 1,
                },
            );
        }
        return;
    }

    let rows = inner.height.saturating_sub(1) as usize;
    let start = window_start(lists.row(), cur.symbols.len(), rows);
    for (i, sym) in cur.symbols.iter().enumerate().skip(start).take(rows) {
        let row = Rect {
            x: inner.x,
            y: inner.y + 1 + (i - start) as u16,
            width: inner.width,
            height: 1,
        };
        let selected = i == lists.row();
        let base = if selected {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        fill_row(frame, row, base);
        let line = match pairs.iter().find(|p| p.symbol == *sym) {
            Some(p) => {
                let chg = format!("{:+.2}%", p.change_pct);
                let mv = rule
                    .and_then(|r| tracker.change_pct(sym, r.window_min, now_ms))
                    .map(|c| format!("{c:+.2}%"));
                let mv_cell = match (&rule, &mv) {
                    (Some(_), Some(m)) => format!("{m:>9} "),
                    (Some(_), None) => format!("{:>9} ", "-"),
                    _ => String::new(),
                };
                let line = format!(
                    " {:<12} {:>10} {}{:>9} {:>10}",
                    sym,
                    fmt_price_col(p.last_price).trim(),
                    mv_cell,
                    chg,
                    fmt_usdt(p.volume_quote).trim(),
                );
                // Colour only the change column; keep the cursor row legible.
                let color = if p.change_pct >= 0.0 {
                    Color::LightGreen
                } else {
                    Color::LightRed
                };
                let chg_x = row.x + 1 + 12 + 1 + 10 + 1 + if rule.is_some() { 10 } else { 0 };
                let chg_rect = Rect {
                    x: chg_x,
                    y: row.y,
                    width: 9,
                    height: 1,
                };
                render_clipped(
                    frame,
                    Paragraph::new(truncate(&line, row.width as usize).to_string()).style(base),
                    row,
                );
                let chg_style = if selected {
                    base
                } else {
                    Style::default().fg(color).bg(palette.bg)
                };
                if chg_x < row.x + row.width {
                    render_clipped(
                        frame,
                        Paragraph::new(format!("{:>9}", chg)).style(chg_style),
                        chg_rect.intersection(row),
                    );
                }
                layout.row_rects.push((row, i));
                continue;
            }
            None => format!(" {:<12} {:>10}", sym, pick("no data", "немає")),
        };
        render_clipped(
            frame,
            Paragraph::new(truncate(&line, row.width as usize).to_string()).style(base),
            row,
        );
        layout.row_rects.push((row, i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchlists::{MoveRule, Side};

    fn pair(sym: &str, chg: f64) -> TickerSummary {
        TickerSummary {
            symbol: sym.into(),
            last_price: 1.0,
            change_pct: chg,
            high_24h: 1.0,
            low_24h: 1.0,
            volume_base: 1.0,
            volume_quote: 2e6,
        }
    }

    fn list(kind: Kind, syms: &[&str]) -> Watchlist {
        Watchlist {
            name: "Main".into(),
            symbols: syms.iter().map(|s| s.to_string()).collect(),
            side: Side::Any,
            kind,
        }
    }

    #[test]
    fn a_manual_list_says_how_its_coins_do() {
        let pairs = [pair("AUSDT", 5.0), pair("BUSDT", -2.0), pair("CUSDT", 1.0)];
        let l = list(Kind::Manual, &["AUSDT", "BUSDT", "CUSDT", "XUSDT"]);
        let (q, a, how) = story(&l, &pairs, &PriceTracker::default(), 0);
        assert_eq!(q, "How are the coins of 'Main' doing?");
        assert!(
            a.starts_with("2 of 3 up in 24 hours, the middle one +1.0%"),
            "{a}"
        );
        assert!(
            a.contains("best A +5.0%, worst B -2.0%") && a.contains("$6.0M traded"),
            "{a}"
        );
        assert!(a.ends_with("1 without data."), "{a}");
        assert!(how.contains("F6"));
        let empty = list(Kind::Manual, &[]);
        assert_eq!(
            story(&empty, &pairs, &PriceTracker::default(), 0).1,
            "The list is empty."
        );
    }

    #[test]
    fn a_rule_and_a_scan_list_say_what_they_hold() {
        let rule = MoveRule {
            window_min: 15,
            threshold_pct: 3.0,
            dir: MoveDir::Up,
            hold_min: 5,
            min_volume: 1e6,
        };
        let (q, a, how) = story(
            &list(Kind::Dynamic(rule), &[]),
            &[],
            &PriceTracker::default(),
            0,
        );
        assert_eq!(
            q,
            "Which coins moved up by more than 3% within 15 minutes, trading at least $1.0M a day?"
        );
        assert!(a.starts_with("Not yet: 0 of 15 minutes"), "{a}");
        assert!(how.contains("5 more minutes"));
        let scan = list(Kind::Scan(ScanKind::Pumps), &["PEPEUSDT", "WIFUSDT"]);
        let (q, a, _) = story(&scan, &[], &PriceTracker::default(), 0);
        assert!(q.contains("jumped up fast"));
        assert_eq!(a, "2 now: PEPE, WIF.");
    }
}
