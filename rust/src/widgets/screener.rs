//! Screener — top gap gainers table with sparkline preview.
//!
//! Sorts symbols by gap percentage (today's open vs previous close) and
//! renders a clickable table.  Each row, when clicked, switches the app
//! to the Markets view for that symbol.

use crate::widgets::fmt::{fmt_price_col, render_clipped, truncate, SPARKLINE_BARS};
use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::binance::{TickerSummary, Venue};
use crate::markets::{fmt_volume, Ohlc, Symbol};
use crate::tokens::Palette;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortMode {
    /// Today's gap (open vs prev close).  Default.
    Gap,
    /// Today's percent change (close vs prev close).
    Change,
    /// Today's volume (descending).
    Volume,
    /// Last price (descending).
    Last,
}

impl SortMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Gap => "Range %",
            Self::Change => "Change %",
            Self::Volume => "Volume",
            Self::Last => "Last",
        }
    }
    pub fn display_name(&self, asc: bool) -> String {
        let arrow = if asc { "↑" } else { "↓" };
        format!("{} {}", arrow, self.name())
    }
    pub fn cycle(&self) -> SortMode {
        match self {
            Self::Gap => Self::Change,
            Self::Change => Self::Volume,
            Self::Volume => Self::Last,
            Self::Last => Self::Gap,
        }
    }
}

const SPARK_WIDTH: u16 = 14;

pub struct ScreenerLayout {
    pub area: Rect,
    /// One entry per visible row: bounding rect + index into the original
    /// `symbols` vec.  Used by the click handler to translate a mouse
    /// position back into a symbol selection.
    pub row_rects: Vec<(Rect, usize)>,
    /// Checkbox cell on each visible row (live mode only).  Click to
    /// toggle that pair into the selection panel.  Empty for synthetic mode.
    pub checkbox_rects: Vec<(Rect, usize)>,
    /// Clickable column header rects + the SortMode they should activate.
    pub header_rects: Vec<(Rect, SortMode)>,
}

/// Compute (sym_idx, sort_value) for every symbol with at least 2 data
/// points, sorted by `sort_value` (descending when `asc` is false) and
/// truncated to `n`.  The reported value depends on `mode`.
pub fn top_gainers_sorted(
    symbols: &[Symbol],
    n: usize,
    mode: SortMode,
    asc: bool,
) -> Vec<(usize, f64)> {
    let mut ranked: Vec<(usize, f64)> = symbols
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let len = s.data.len();
            if len < 2 {
                return None;
            }
            let prev = s.data[len - 2].close;
            let today = &s.data[len - 1];
            if prev <= 0.0 {
                return None;
            }
            let v = match mode {
                SortMode::Gap => (today.open - prev) / prev * 100.0,
                SortMode::Change => (today.close - prev) / prev * 100.0,
                SortMode::Volume => today.volume,
                SortMode::Last => today.close,
            };
            Some((i, v))
        })
        .collect();
    if asc {
        ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    ranked.truncate(n);
    ranked
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    symbols: &[Symbol],
    selected: usize,
    sort: SortMode,
    palette: &Palette,
    asc: bool,
) -> ScreenerLayout {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(" Screener  -  sort: {} ", sort.display_name(asc)),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 2,
        y: inner.y + 1,
        width: inner.width.saturating_sub(4),
        height: inner.height.saturating_sub(2),
    };

    // Header row — last column label depends on sort mode.
    let last_col = match sort {
        SortMode::Gap => "GAP%",
        SortMode::Change => "CHG%",
        SortMode::Volume => "VOL%",
        SortMode::Last => "GAP%",
    };
    let header = format_row("#", "TICKER", "NAME", "LAST", last_col, "VOLUME", "TREND");
    let h_area = Rect {
        x: pad.x,
        y: pad.y,
        width: pad.width,
        height: 1,
    };
    render_clipped(
        frame,
        Paragraph::new(header).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        h_area,
    );

    // Separator.
    let sep_area = Rect {
        x: pad.x,
        y: pad.y + 1,
        width: pad.width,
        height: 1,
    };
    let sep = "-".repeat(pad.width as usize);
    render_clipped(
        frame,
        Paragraph::new(sep).style(Style::default().fg(palette.muted).bg(palette.bg)),
        sep_area,
    );

    // Rows.
    let gainers = top_gainers_sorted(symbols, 10, sort, asc);
    let mut row_rects = Vec::new();
    let body_top = pad.y + 2;

    for (rank, (sym_idx, gap_pct)) in gainers.iter().enumerate() {
        let y = body_top + rank as u16 * 2; // double-spaced for readability
        if y >= pad.y + pad.height {
            break;
        }

        let s = &symbols[*sym_idx];
        let last = s.data.last();
        let close = last.map(|o| o.close).unwrap_or(0.0);
        let volume = last.map(|o| o.volume).unwrap_or(0.0);
        let up = *gap_pct >= 0.0;

        let row_area = Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        };
        let highlight = rank == selected;

        let bg = if highlight {
            palette.cursor_bg
        } else {
            palette.bg
        };
        let fg = if highlight {
            palette.cursor_fg
        } else {
            palette.text
        };
        let chg_color = if up {
            Color::LightGreen
        } else {
            Color::LightRed
        };

        // Background fill for highlighted row.
        if highlight {
            let buf = frame.buffer_mut();
            for col in 0..row_area.width {
                if let Some(cell) = buf.cell_mut((row_area.x + col, row_area.y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().fg(fg).bg(bg));
                }
            }
        }

        let chg_str = format!("{}{:.2}%", if up { "+" } else { "-" }, gap_pct.abs());
        let line = format_row(
            &(rank + 1).to_string(),
            s.ticker,
            truncate(s.name, 24),
            &format!("{:.2}", close),
            &chg_str,
            &fmt_volume(volume),
            "",
        );

        render_clipped(
            frame,
            Paragraph::new(line).style(Style::default().fg(fg).bg(bg)),
            row_area,
        );

        // Sparkline preview at the right end of the row.
        let spark_x = row_area.x + row_area.width.saturating_sub(SPARK_WIDTH);
        let spark_area = Rect {
            x: spark_x,
            y,
            width: SPARK_WIDTH,
            height: 1,
        };
        if spark_area.x + spark_area.width <= row_area.x + row_area.width {
            render_sparkline(frame, spark_area, &s.data, chg_color, bg);
        }

        // Re-render CHG% cell with its own color (overrides the row paragraph).
        // Layout: rank(3) sp(2) ticker(6) sp(2) name(24) sp(2) last(10) sp(2) chg(8)
        let chg_x = row_area.x + 3 + 2 + 6 + 2 + 24 + 2 + 10 + 2;
        if chg_x + 8 <= row_area.x + row_area.width {
            let chg_area = Rect {
                x: chg_x,
                y,
                width: 8,
                height: 1,
            };
            render_clipped(
                frame,
                Paragraph::new(format!("{:>8}", chg_str)).style(
                    Style::default()
                        .fg(chg_color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                chg_area,
            );
        }

        row_rects.push((row_area, *sym_idx));
    }

    // Footer hint.
    let hint_y = pad.y + pad.height.saturating_sub(1);
    let hint = " Up/Down select | Enter open | s sort | l live Binance | Esc back ";
    render_clipped(
        frame,
        Paragraph::new(hint).style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: hint_y,
            width: pad.width,
            height: 1,
        },
    );

    // Clickable column headers: LAST at offset 39 (width 10),
    // last_col at offset 51 (width 8).
    let last_col_sort = match sort {
        SortMode::Gap => SortMode::Gap,
        SortMode::Change => SortMode::Change,
        SortMode::Volume => SortMode::Volume,
        SortMode::Last => SortMode::Gap,
    };
    let header_rects = vec![
        (
            Rect {
                x: pad.x + 39,
                y: pad.y,
                width: 10,
                height: 1,
            },
            SortMode::Last,
        ),
        (
            Rect {
                x: pad.x + 51,
                y: pad.y,
                width: 8,
                height: 1,
            },
            last_col_sort,
        ),
    ];

    ScreenerLayout {
        area,
        row_rects,
        checkbox_rects: Vec::new(),
        header_rects,
    }
}

/// Sort + filter live Binance 24h pairs, returning indices into the original
/// `pairs` slice paired with the sort value.  Volume filter expressed in
/// quote-asset (USDT) terms.
pub fn rank_live(
    pairs: &[TickerSummary],
    n: usize,
    mode: SortMode,
    min_quote_volume: f64,
    asc: bool,
) -> Vec<(usize, f64)> {
    let mut ranked: Vec<(usize, f64)> = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.volume_quote >= min_quote_volume)
        .map(|(i, p)| {
            let v = match mode {
                // 24h intraday range as a % of the low — "how volatile was this pair".
                SortMode::Gap => {
                    let lo = p.low_24h.max(1e-12);
                    ((p.high_24h - p.low_24h) / lo) * 100.0
                }
                // 24h change % (top gainers/losers).
                SortMode::Change => p.change_pct,
                // Quote-asset volume (USDT).
                SortMode::Volume => p.volume_quote,
                // Last price.
                SortMode::Last => p.last_price,
            };
            (i, v)
        })
        .collect();
    if asc {
        ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    ranked.truncate(n);
    ranked
}

pub fn render_live(
    frame: &mut Frame,
    area: Rect,
    pairs: &[TickerSummary],
    selected: usize,
    sort: SortMode,
    asc: bool,
    venue: Venue,
    min_quote_volume: f64,
    // selected_set: symbols currently checked into the strategy panel.
    // Pass `&HashSet::new()` for read-only use cases.
    selected_set: &HashSet<String>,
    palette: &Palette,
    ws_status: &str,
) -> ScreenerLayout {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(
                " Screener LIVE ({}) {}  -  sort: {}  -  min vol: {} USDT  -  {} pairs ",
                venue.name(),
                ws_status,
                sort.display_name(asc),
                fmt_usdt(min_quote_volume),
                pairs.len(),
            ),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    let pad = Rect {
        x: inner.x + 2,
        y: inner.y + 1,
        width: inner.width.saturating_sub(4),
        height: inner.height.saturating_sub(2),
    };

    let last_col = match sort {
        SortMode::Gap => "RANGE%",
        SortMode::Change => "CHG24H%",
        SortMode::Volume => "VOL_USDT",
        SortMode::Last => "RANGE%",
    };
    let header = format_live_row("#", "PAIR", "VENUE", "LAST", last_col, "QV(USDT)", "TREND");
    // Header has a 4-char gutter to align with the checkbox column on
    // each row.
    let header = format!("    {header}");
    render_clipped(
        frame,
        Paragraph::new(header).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: pad.x,
            y: pad.y,
            width: pad.width,
            height: 1,
        },
    );
    let sep = "-".repeat(pad.width as usize);
    render_clipped(
        frame,
        Paragraph::new(sep).style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: pad.y + 1,
            width: pad.width,
            height: 1,
        },
    );

    let body_top = pad.y + 2;
    let max_visible = ((pad.height.saturating_sub(3)) / 2) as usize;
    let max_visible = max_visible.max(1);
    // Pull EVERY pair that passes the volume filter, sorted, so user can
    // scroll through hundreds.  Cap at 1000 for memory hygiene.
    let ranked = rank_live(pairs, 1000, sort, min_quote_volume, asc);
    // Scroll window so `selected` is always visible.  Bias upward so the
    // first jump from selected=0 keeps top-of-list pinned.
    let total = ranked.len();
    let window_start = if total <= max_visible {
        0
    } else {
        let half = max_visible / 2;
        let s = selected.saturating_sub(half);
        s.min(total.saturating_sub(max_visible))
    };
    let window_end = (window_start + max_visible).min(total);
    let visible_slice = &ranked[window_start..window_end];

    let mut row_rects = Vec::new();
    let mut checkbox_rects = Vec::new();
    const CB_WIDTH: u16 = 4;
    for (i, (sym_idx, _)) in visible_slice.iter().enumerate() {
        let absolute_rank = window_start + i;
        let y = body_top + i as u16 * 2;
        if y >= pad.y + pad.height.saturating_sub(1) {
            break;
        }
        let p = &pairs[*sym_idx];
        let up = p.change_pct >= 0.0;
        let highlight = absolute_rank == selected;
        let checked = selected_set.contains(&p.symbol);
        let bg = if highlight {
            palette.cursor_bg
        } else {
            palette.bg
        };
        let fg = if highlight {
            palette.cursor_fg
        } else {
            palette.text
        };
        let chg_color = if up {
            Color::LightGreen
        } else {
            Color::LightRed
        };
        let row_area = Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        };
        if highlight {
            let buf = frame.buffer_mut();
            for col in 0..row_area.width {
                if let Some(cell) = buf.cell_mut((row_area.x + col, row_area.y)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().fg(fg).bg(bg));
                }
            }
        }
        // Checkbox column.  Clicking this cell toggles the pair into
        // the strategy panel; clicking elsewhere on the row opens the chart.
        let cb_area = Rect {
            x: row_area.x,
            y,
            width: CB_WIDTH,
            height: 1,
        };
        let cb_str = if checked { "[X] " } else { "[ ] " };
        let cb_fg = if checked { palette.ok } else { palette.muted };
        render_clipped(
            frame,
            Paragraph::new(cb_str).style(Style::default().fg(cb_fg).bg(bg).add_modifier(
                if checked {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                },
            )),
            cb_area,
        );
        checkbox_rects.push((cb_area, *sym_idx));

        let line_area = Rect {
            x: row_area.x + CB_WIDTH,
            y,
            width: row_area.width.saturating_sub(CB_WIDTH),
            height: 1,
        };

        // Display value matches the active sort mode so the column reflects
        // what's actually ranked.
        let chg_str = match sort {
            SortMode::Gap => {
                let lo = p.low_24h.max(1e-12);
                let range = ((p.high_24h - p.low_24h) / lo) * 100.0;
                format!("{:.2}%", range)
            }
            SortMode::Change => {
                format!("{}{:.2}%", if up { "+" } else { "-" }, p.change_pct.abs())
            }
            SortMode::Volume => fmt_usdt(p.volume_quote),
            SortMode::Last => {
                let lo = p.low_24h.max(1e-12);
                let range = ((p.high_24h - p.low_24h) / lo) * 100.0;
                format!("{:.2}%", range)
            }
        };
        let line = format_live_row(
            &(absolute_rank + 1).to_string(),
            &p.symbol,
            venue.name(),
            &fmt_price_col(p.last_price),
            &chg_str,
            &fmt_usdt(p.volume_quote),
            "",
        );
        render_clipped(
            frame,
            Paragraph::new(line).style(Style::default().fg(fg).bg(bg)),
            line_area,
        );
        let chg_x = line_area.x + LIVE_CHG_OFF;
        if chg_x + 8 <= line_area.x + line_area.width {
            render_clipped(
                frame,
                Paragraph::new(format!("{:>8}", chg_str)).style(
                    Style::default()
                        .fg(chg_color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Rect {
                    x: chg_x,
                    y,
                    width: 8,
                    height: 1,
                },
            );
        }
        row_rects.push((row_area, *sym_idx));
    }

    let hint_y = pad.y + pad.height.saturating_sub(1);
    let hint = " Up/Down | Enter open chart | s sort | v venue | m min-vol | r refresh | Esc back ";
    render_clipped(
        frame,
        Paragraph::new(hint).style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: hint_y,
            width: pad.width,
            height: 1,
        },
    );

    // Clickable column headers: LAST and last_col sit at fixed offsets after
    // the 4-char gutter (see `format_live_row`).
    // The last_col header rect always maps to the data type it shows,
    // not the current sort mode (so clicking RANGE% always sorts by Gap).
    let last_col_sort = match sort {
        SortMode::Gap => SortMode::Gap,
        SortMode::Change => SortMode::Change,
        SortMode::Volume => SortMode::Volume,
        SortMode::Last => SortMode::Gap,
    };
    let header_rects = vec![
        (
            Rect {
                x: pad.x + 4 + LIVE_LAST_OFF,
                y: pad.y,
                width: 10,
                height: 1,
            },
            SortMode::Last,
        ),
        (
            Rect {
                x: pad.x + 4 + LIVE_CHG_OFF,
                y: pad.y,
                width: 8,
                height: 1,
            },
            last_col_sort,
        ),
    ];

    ScreenerLayout {
        area,
        row_rects,
        checkbox_rects,
        header_rects,
    }
}

pub fn fmt_usdt(v: f64) -> String {
    if v >= 1e9 {
        format!("{:>5.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:>5.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:>5.1}K", v / 1e3)
    } else {
        format!("{:>5.0} ", v)
    }
}

/// Live-pair rows: Binance symbols are up to 12 chars ("1000PEPEUSDT"), so the
/// ticker column is wider than the stock screener's.  Offsets of the columns
/// that are overlaid or clickable are derived from the same widths.
const LIVE_TICK_W: usize = 12;
const LIVE_VENUE_W: usize = 8;
const LIVE_LAST_OFF: u16 = (3 + 2 + LIVE_TICK_W + 2 + LIVE_VENUE_W + 2) as u16;
const LIVE_CHG_OFF: u16 = LIVE_LAST_OFF + 10 + 2;

fn format_live_row(
    rank: &str,
    ticker: &str,
    venue: &str,
    last: &str,
    chg: &str,
    vol: &str,
    trend: &str,
) -> String {
    format!(
        "{:>3}  {:<tw$.tw$}  {:<vw$.vw$}  {:>10}  {:>8}  {:>14}  {}",
        rank,
        ticker,
        venue,
        last,
        chg,
        vol,
        trend,
        tw = LIVE_TICK_W,
        vw = LIVE_VENUE_W,
    )
}

fn format_row(
    rank: &str,
    ticker: &str,
    name: &str,
    last: &str,
    chg: &str,
    vol: &str,
    trend: &str,
) -> String {
    format!(
        "{:>3}  {:<6}  {:<24}  {:>10}  {:>8}  {:>14}  {}",
        rank, ticker, name, last, chg, vol, trend,
    )
}

fn render_sparkline(frame: &mut Frame, area: Rect, data: &[Ohlc], color: Color, row_bg: Color) {
    if data.is_empty() || area.width == 0 {
        return;
    }
    let n = (area.width as usize).min(data.len());
    let slice = &data[data.len() - n..];

    let max_p = slice
        .iter()
        .map(|o| o.close)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_p = slice.iter().map(|o| o.close).fold(f64::INFINITY, f64::min);
    let range = (max_p - min_p).max(1e-9);

    let buf = frame.buffer_mut();
    for (i, o) in slice.iter().enumerate() {
        let x = area.x + i as u16;
        if x >= area.x + area.width {
            break;
        }
        let t = (o.close - min_p) / range;
        let idx = (t * 7.0).round().clamp(0.0, 7.0) as usize;
        if let Some(cell) = buf.cell_mut((x, area.y)) {
            cell.set_char(SPARKLINE_BARS[idx]);
            cell.set_style(Style::default().fg(color).bg(row_bg));
        }
    }
}

#[cfg(test)]
mod live_row_tests {
    use super::*;

    #[test]
    fn long_symbols_do_not_shift_the_overlaid_columns() {
        for sym in ["BTCUSDT", "ATOMUSDT", "1000PEPEUSDT", "1000000MOGUSDT"] {
            let row = format_live_row("1", sym, "Spot", "1.0", "9.99%", "1M", "");
            let chg: String = row.chars().skip(LIVE_CHG_OFF as usize).take(8).collect();
            assert_eq!(chg, "   9.99%", "{sym}");
            let last: String = row.chars().skip(LIVE_LAST_OFF as usize).take(10).collect();
            assert_eq!(last.trim(), "1.0", "{sym}");
        }
    }
}
