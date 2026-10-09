//! Markets view — full-screen layout with header, candle chart, volume bars,
//! and a stats footer.  Switch symbols via Left/Right.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::markets::fmt_volume;
use crate::tokens::Palette;
use crate::widgets::chart;
use crate::widgets::fmt::render_clipped;

pub struct MarketsLayout {
    pub area: Rect,
    pub pane_rects: Vec<Rect>,
    /// Horizontal scroll-bar area beneath volume.  Click x → jump to that
    /// candle position.  Empty rect when not rendered (multi-pane / no data).
    pub scrollbar_rect: Rect,
    /// Total candles in the pane's history (used to map a scroll click x
    /// into a chart_offset).
    pub scroll_total: usize,
    /// Clickable "from date" pill in the header.  Click → open from-date calendar.
    pub from_date_rect: Rect,
    /// Clickable "to date" pill in the header.  Click → open to-date calendar.
    pub to_date_rect: Rect,
    /// Clickable session-window pill in the header.  Click → open session picker.
    pub session_rect: Rect,
}

/// Date / session / history filters applied to a series before charting.
#[derive(Clone, Copy)]
pub struct SeriesFilters<'a> {
    pub date_from: Option<&'a str>,
    pub date_to: Option<&'a str>,
    pub session: crate::markets::SessionWindow,
    pub time_from_ms: i64,
}

impl SeriesFilters<'_> {
    pub const NONE: SeriesFilters<'static> = SeriesFilters {
        date_from: None,
        date_to: None,
        session: crate::markets::SessionWindow::ALL,
        time_from_ms: 0,
    };
}

/// The series every Markets pane draws, built ONE way for single- and
/// multi-pane layouts (audit R8): filters → day gaps → aggregation →
/// scroll trim.  Footprint columns are projected onto the same series, so
/// footprint, candles, axis, separators, scrollbar and hover all index the
/// same columns (audit W2).
pub struct ChartView {
    /// Scroll-trimmed candles; the last one is "now" for the renderer.
    pub ohlc: Vec<crate::markets::Ohlc>,
    /// 1:1 with `ohlc`; empty when no footprint was requested.
    pub footprint: Vec<crate::data::footprint::FootprintCandle>,
    /// Length of the full series before scroll trimming (scrollbar total).
    pub full_len: usize,
}

/// Everything that determines a [`ChartView`].  Data is identified by length
/// plus a cheap checksum over every candle, so an edited series misses the
/// cache even when its last candle looks the same.
#[derive(PartialEq)]
struct ViewKey {
    data_len: usize,
    data_sum: u64,
    fp_len: usize,
    fp_sum: u64,
    date_from: Option<String>,
    date_to: Option<String>,
    session: (u16, u16),
    time_from_ms: i64,
    aggregate: u16,
    offset: usize,
}

fn data_checksum(data: &[crate::markets::Ohlc]) -> u64 {
    let mut acc = 0.0f64;
    for (i, o) in data.iter().enumerate() {
        let w = 1.0 + (i % 7) as f64 * 0.125;
        acc += w * (o.close + o.high * 0.5 + o.low * 0.25 + o.open * 0.125 + o.volume * 1e-3)
            + o.time_ms as f64 * 1e-6
            + if o.is_gap { 1.0 } else { 0.0 };
    }
    acc.to_bits()
}

fn footprint_checksum(fp: &[crate::data::footprint::FootprintCandle]) -> u64 {
    fp.iter()
        .map(|f| f.total_buy + f.total_sell * 1.5 + f.levels.len() as f64)
        .sum::<f64>()
        .to_bits()
}

thread_local! {
    /// A few recent views (one per pane, plus spare): while nothing about a
    /// pane changed, a redraw reuses its series instead of copying,
    /// filtering and aggregating it again.
    static VIEW_CACHE: std::cell::RefCell<Vec<(ViewKey, std::rc::Rc<ChartView>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

const VIEW_CACHE_SLOTS: usize = 6;

/// Cached [`build_view_uncached`].
pub fn build_view(
    data: &[crate::markets::Ohlc],
    footprint: &[crate::data::footprint::FootprintCandle],
    filters: &SeriesFilters,
    aggregate: u16,
    chart_offset: usize,
) -> std::rc::Rc<ChartView> {
    let key = ViewKey {
        data_len: data.len(),
        data_sum: data_checksum(data),
        fp_len: footprint.len(),
        fp_sum: footprint_checksum(footprint),
        date_from: filters.date_from.map(str::to_string),
        date_to: filters.date_to.map(str::to_string),
        session: (filters.session.start_min, filters.session.end_min),
        time_from_ms: filters.time_from_ms,
        aggregate,
        offset: chart_offset,
    };
    VIEW_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if let Some(pos) = cache.iter().position(|(k, _)| *k == key) {
            let hit = cache.remove(pos);
            let view = hit.1.clone();
            cache.push(hit);
            return view;
        }
        let view = std::rc::Rc::new(build_view_uncached(
            data,
            footprint,
            filters,
            aggregate,
            chart_offset,
        ));
        cache.push((key, view.clone()));
        if cache.len() > VIEW_CACHE_SLOTS {
            cache.remove(0);
        }
        view
    })
}

pub fn build_view_uncached(
    data: &[crate::markets::Ohlc],
    footprint: &[crate::data::footprint::FootprintCandle],
    filters: &SeriesFilters,
    aggregate: u16,
    chart_offset: usize,
) -> ChartView {
    let filtered: Vec<crate::markets::Ohlc> = if filters.date_from.is_some()
        || filters.date_to.is_some()
        || !filters.session.is_off()
        || filters.time_from_ms > 0
    {
        crate::markets::apply_filters(
            data,
            filters.date_from,
            filters.date_to,
            filters.session,
            filters.time_from_ms,
        )
    } else {
        data.to_vec()
    };
    // Day-boundary gap sentinels on intraday TFs: each new calendar day gets
    // its own empty column for the session divider.
    let filtered = crate::markets::insert_day_gaps(&filtered);
    let full = chart::aggregate(&filtered, aggregate);
    let full_fp = if footprint.is_empty() {
        Vec::new()
    } else {
        crate::data::footprint::project_onto(&full, footprint)
    };
    let full_len = full.len();
    // Keep at least one candle visible when scrolled all the way back.
    let trim_end = full_len.saturating_sub(chart_offset.min(full_len.saturating_sub(1)));
    let mut ohlc = full;
    ohlc.truncate(trim_end);
    let mut fp = full_fp;
    fp.truncate(trim_end);
    ChartView {
        ohlc,
        footprint: fp,
        full_len,
    }
}

/// Plot area (grid + candles/line/footprint + y-axis) for one pane.  Returns
/// `false` — drawing nothing — when there is no price range to scale (empty
/// or all-gap window) so callers can show "loading" instead of a fake axis.
fn render_plot(
    frame: &mut Frame,
    plot: Rect,
    axis: Rect,
    view: &ChartView,
    visible: &[crate::markets::Ohlc],
    zoom: u16,
    chart_type: chart::ChartType,
    palette: &Palette,
) -> bool {
    let Some((hi, lo)) = chart::visible_range(visible) else {
        return false;
    };
    chart::render_grid(frame, plot, hi, lo, palette);
    if chart_type == chart::ChartType::DeltaCluster && !view.footprint.is_empty() {
        chart::render_footprint(frame, plot, &view.footprint, (hi, lo), palette, zoom);
    } else {
        chart::render_chart(frame, plot, &view.ohlc, palette, zoom, chart_type);
    }
    chart::render_yaxis(frame, axis, hi, lo, palette);
    true
}

/// Compact pane — chart + thin header + volume.  Used when several panes
/// share the screen.  Returns the full series length (scrollbar total).
pub fn render_pane(
    frame: &mut Frame,
    area: Rect,
    ticker: &str,
    data: &[crate::markets::Ohlc],
    footprint: &[crate::data::footprint::FootprintCandle],
    filters: &SeriesFilters,
    chart_offset: usize,
    zoom: u16,
    aggregate: u16,
    timeframe: crate::markets::Timeframe,
    chart_type: chart::ChartType,
    active: bool,
    palette: &Palette,
) -> usize {
    let border_style = if active {
        Style::default().fg(palette.accent).bg(palette.bg)
    } else {
        Style::default().fg(palette.muted).bg(palette.bg)
    };
    let view = build_view(data, footprint, filters, aggregate, chart_offset);
    let title = match (
        view.ohlc.iter().rfind(|o| !o.is_gap),
        view.ohlc.iter().find(|o| !o.is_gap),
    ) {
        (Some(last), Some(first)) => {
            let chg_pct = if first.open > 0.0 {
                ((last.close - first.open) / first.open) * 100.0
            } else {
                0.0
            };
            let arrow = if chg_pct >= 0.0 { "+" } else { "-" };
            format!(
                " {}  ${:.2}  {}{:.2}%  | {} | {} ",
                ticker,
                last.close,
                arrow,
                chg_pct.abs(),
                timeframe.name(),
                chart_type.name(),
            )
        }
        _ => format!(
            " {} | {} | {} | (loading) ",
            ticker,
            timeframe.name(),
            chart_type.name()
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .title(Span::styled(
            title,
            Style::default()
                .fg(if active { palette.accent } else { palette.text })
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if view.ohlc.iter().all(|o| o.is_gap) {
        // Nothing to chart yet — the title already says "(loading)".
        return view.full_len;
    }

    // Vertical split: chart (most of the height) + thin x-axis row + volume.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),    // chart
            Constraint::Length(1), // x-axis dates
            Constraint::Length(3), // volume
        ])
        .split(inner);
    let chart_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[0]);
    let visible = chart::visible_slice_at(&view.ohlc, chart_split[0].width, zoom, 0);
    render_plot(
        frame,
        chart_split[0],
        chart_split[1],
        &view,
        visible,
        zoom,
        chart_type,
        palette,
    );

    // X-axis dates.
    let xaxis_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[1]);
    render_xaxis_dates(frame, xaxis_split[0], visible, zoom, palette);
    render_axis_label(frame, xaxis_split[1], "", palette);

    // Volume.
    let vol_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[2]);
    chart::render_volume(frame, vol_split[0], &view.ohlc, palette, zoom);
    render_axis_label(frame, vol_split[1], " Volume", palette);
    view.full_len
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    pane_ticker: &str,
    pane_name: &str,
    pane_data: &[crate::markets::Ohlc],
    pane_footprint: &[crate::data::footprint::FootprintCandle],
    zoom: u16,
    aggregate: u16,
    timeframe: crate::markets::Timeframe,
    chart_type: chart::ChartType,
    chart_offset: usize,
    filters: &SeriesFilters,
    hover: Option<(u16, u16)>,
    palette: &Palette,
) -> MarketsLayout {
    let (date_from, date_to, session) = (filters.date_from, filters.date_to, filters.session);
    // --- Early exit: no data → show placeholder instead of panicking ---
    if pane_data.is_empty() && pane_footprint.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(palette.text).bg(palette.bg))
            .title(Span::styled(
                " Markets ",
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().fg(palette.text).bg(palette.bg));
        let inner = block.inner(area);
        render_clipped(frame, block, area);
        let placeholder = Paragraph::new("No data — press R to refresh")
            .style(Style::default().fg(palette.muted).bg(palette.bg))
            .alignment(ratatui::layout::Alignment::Center);
        render_clipped(frame, placeholder, inner);
        return MarketsLayout {
            area,
            pane_rects: Vec::new(),
            scrollbar_rect: Rect::default(),
            scroll_total: 0,
            from_date_rect: Rect::default(),
            to_date_rect: Rect::default(),
            session_rect: Rect::default(),
        };
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            " Markets ",
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    // Layout: header (3) + chart (Min) + x-axis (1) + volume (4) +
    // scrollbar (2 rows: track + position labels).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Length(2),
        ])
        .split(inner);

    let view = build_view(pane_data, pane_footprint, filters, aggregate, chart_offset);
    let working_data = &view.ohlc;
    // If after filtering there's nothing to show, render placeholder.
    if working_data.is_empty() {
        let placeholder = Paragraph::new("No data in selected range — adjust filters or press R")
            .style(Style::default().fg(palette.muted).bg(palette.bg))
            .alignment(ratatui::layout::Alignment::Center);
        render_clipped(frame, placeholder, rows[1]);
        return MarketsLayout {
            area,
            pane_rects: Vec::new(),
            scrollbar_rect: Rect::default(),
            scroll_total: 0,
            from_date_rect: Rect::default(),
            to_date_rect: Rect::default(),
            session_rect: Rect::default(),
        };
    }

    // Header — also returns clickable rects for the date / session pills.
    let (from_date_rect, to_date_rect, session_rect) = render_header(
        frame,
        rows[0],
        pane_ticker,
        pane_name,
        timeframe,
        chart_type,
        working_data,
        aggregate,
        chart_offset,
        view.full_len,
        date_from,
        date_to,
        session,
        palette,
    );

    // Chart with y-axis on the RIGHT (price axis on the right).
    let chart_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[1]);

    // Compute the SAME visible slice that render_candles will use, so the
    // axis labels and grid lines reflect the actual zoomed-in price range.
    let visible = chart::visible_slice_at(working_data, chart_split[0].width, zoom, 0);
    if !render_plot(
        frame,
        chart_split[0],
        chart_split[1],
        &view,
        visible,
        zoom,
        chart_type,
        palette,
    ) {
        let placeholder = Paragraph::new("(loading)")
            .style(Style::default().fg(palette.muted).bg(palette.bg))
            .alignment(ratatui::layout::Alignment::Center);
        render_clipped(frame, placeholder, chart_split[0]);
    }

    // X-axis date markers row (between chart and volume).  Gap on the
    // RIGHT to align with the price axis.
    let xaxis_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[2]);
    render_xaxis_dates(frame, xaxis_split[0], visible, zoom, palette);
    render_axis_label(frame, xaxis_split[1], "", palette);

    // Volume.  Gap on the RIGHT for symmetry; "Volume" label sits there.
    let vol_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[3]);
    chart::render_volume(frame, vol_split[0], working_data, palette, zoom);
    render_axis_label(frame, vol_split[1], " Volume", palette);

    // Vertical session dividers — drawn AFTER chart + volume so they sit
    // on top.  Three layers:
    //   * gap columns get a full-height `│` (column is empty anyway);
    //   * day-boundary columns get a full vertical line when candle
    //     width >= 2 (line at left edge, candle keeps cw-1 visible);
    //   * AT cw == 1 the chart+volume areas are left untouched
    //     (overpainting would wipe candles), but the X-axis row gets a
    //     thin `│` tick at every boundary so the user still sees a
    //     session marker on the time axis.
    render_session_dividers(
        frame,
        chart_split[0],
        xaxis_split[0],
        vol_split[0],
        visible,
        zoom,
        palette,
    );

    // Horizontal scroll-bar beneath volume — same width as the chart area
    // (excludes the y-axis column).
    let scroll_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(9)])
        .split(rows[4]);
    let scrollbar_rect = scroll_split[0];
    let scroll_total = view.full_len;
    render_scrollbar(
        frame,
        scrollbar_rect,
        scroll_total,
        chart_offset,
        visible.len(),
        palette,
    );

    if let Some((mx, my)) = hover {
        render_hover_tooltip(
            frame,
            chart_split[0],
            vol_split[0],
            visible,
            zoom,
            mx,
            my,
            palette,
        );
    }

    MarketsLayout {
        area,
        pane_rects: Vec::new(),
        scrollbar_rect,
        scroll_total,
        from_date_rect,
        to_date_rect,
        session_rect,
    }
}

/// Horizontal scroll-bar with sub-cell precision + position labels.
///
/// Row 1: classic bracketed track [<-------=====-------->]
///   * Outer caps `[` `]` in bright text (always anchor the bar visually).
///   * Track filled with `-` between caps.
///   * Thumb: bright bg + `<` `>` grip arrows on the edges and `=` ribbing
///     between them.  The thumb is ALWAYS rendered with a high-contrast
///     bright color (LightGreen at LIVE, LightYellow when scrolled) so it
///     stays clearly visible on any palette — the previous version used
///     `palette.accent`, which on the modern dark theme reads as a dim
///     blue that disappears against the bg.
///   * Minimum thumb width = 3 cells so it's always grabbable.
///
/// Row 2: position label "  1   drag · click · wheel · Home=LIVE   N/T (P%) LIVE   now".
fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total: usize,
    offset: usize,
    visible: usize,
    palette: &Palette,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width;
    let track_y = area.y;

    // Track (row 1): bright `[` `]` end-caps + `-` track body.  Caps stay
    // bright (text color, bold) so the bar's extent is unambiguous; track
    // is muted so the thumb pops against it.
    let buf = frame.buffer_mut();
    for k in 0..w {
        let (glyph, style) = if k == 0 || k == w - 1 {
            let ch = if k == 0 { '[' } else { ']' };
            (
                ch,
                Style::default()
                    .fg(palette.text)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ('-', Style::default().fg(palette.muted).bg(palette.bg))
        };
        if let Some(c) = buf.cell_mut((area.x + k, track_y)) {
            c.set_char(glyph);
            c.set_style(style);
        }
    }

    if total == 0 {
        if area.height >= 2 {
            let label_y = area.y + 1;
            let hint = "  (no history yet)";
            for (i, ch) in hint.chars().enumerate() {
                let x = area.x + i as u16;
                if x >= area.x + w {
                    break;
                }
                if let Some(c) = buf.cell_mut((x, label_y)) {
                    c.set_char(ch);
                    c.set_style(Style::default().fg(palette.muted).bg(palette.bg));
                }
            }
        }
        return;
    }

    // Visible window in the full series:
    //   end = total - offset (right edge of visible)
    //   start = end - visible (left edge)
    let end = total.saturating_sub(offset);
    let start = end.saturating_sub(visible.max(1));

    // Thumb is constrained INSIDE the [ ] caps so it never overlaps them.
    let track_lo = 1u16;
    let track_hi = w.saturating_sub(1);
    let track_w = track_hi.saturating_sub(track_lo) as i32;
    if track_w <= 0 {
        return;
    }

    let total_f = total as f64;
    let track_w_f = track_w as f64;
    let mut thumb_lo = ((start as f64 / total_f) * track_w_f).round() as i32;
    let mut thumb_hi = ((end as f64 / total_f) * track_w_f).round() as i32;
    if thumb_hi <= thumb_lo {
        thumb_hi = thumb_lo + 1;
    }
    let min_w = 3i32;
    if thumb_hi - thumb_lo < min_w {
        let pad = (min_w - (thumb_hi - thumb_lo)) / 2;
        thumb_lo -= pad;
        thumb_hi = thumb_lo + min_w;
        if thumb_lo < 0 {
            thumb_hi -= thumb_lo;
            thumb_lo = 0;
        }
        if thumb_hi > track_w {
            thumb_lo -= thumb_hi - track_w;
            thumb_hi = track_w;
            if thumb_lo < 0 {
                thumb_lo = 0;
            }
        }
    }
    let thumb_lo = (thumb_lo.max(0) as u16) + track_lo;
    let thumb_hi = ((thumb_hi as u16) + track_lo).min(track_hi);

    // Always-bright thumb color.  LightGreen = LIVE (at end of stream),
    // LightYellow = scrolled-into-history.  Both ANSI named colors so they
    // render correctly on every terminal regardless of truecolor support,
    // and both pop against any palette.bg we ship (deep blue or near-black).
    let thumb_color = if offset == 0 {
        Color::LightGreen
    } else {
        Color::LightYellow
    };

    // Thumb body: `<` and `>` grip arrows on the edges + `=` ribbing in
    // the middle so the thumb has visible "handle texture" even on a
    // monochrome / colorblind terminal.  fg=black-on-bright-bg ensures
    // the grip glyphs remain readable.
    let inner_first = thumb_lo;
    let inner_last = thumb_hi.saturating_sub(1);
    for k in thumb_lo..thumb_hi {
        let x = area.x;
        if k >= w {
            break;
        }
        let glyph = if k == inner_first && (thumb_hi - thumb_lo) >= 3 {
            '<'
        } else if k == inner_last && (thumb_hi - thumb_lo) >= 3 {
            '>'
        } else {
            '='
        };
        if let Some(c) = buf.cell_mut((x + k, track_y)) {
            c.set_char(glyph);
            c.set_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(thumb_color)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    // Row 2: position label + interaction hint.
    if area.height >= 2 {
        let label_y = area.y + 1;
        for k in 0..w {
            if let Some(c) = buf.cell_mut((area.x + k, label_y)) {
                c.set_char(' ');
                c.set_style(Style::default().fg(palette.muted).bg(palette.bg));
            }
        }
        let pct = ((end as f64 / total as f64) * 100.0).round() as u32;
        let live_label = if offset == 0 { " LIVE" } else { "" };
        let middle = format!("  {}/{} ({}%){}  ", end, total, pct, live_label);
        let left = "  1";
        let right = "now  ";
        let hint = " drag | click | wheel | Home=LIVE ";

        // Left.
        for (i, ch) in left.chars().enumerate() {
            let x = area.x + i as u16;
            if x >= area.x + w {
                break;
            }
            if let Some(c) = buf.cell_mut((x, label_y)) {
                c.set_char(ch);
            }
        }
        // Middle position.
        let mw = middle.chars().count() as u16;
        if mw < w {
            let mx = area.x + (w.saturating_sub(mw)) / 2;
            for (i, ch) in middle.chars().enumerate() {
                let x = mx + i as u16;
                if x >= area.x + w {
                    break;
                }
                if let Some(c) = buf.cell_mut((x, label_y)) {
                    c.set_char(ch);
                    let mut st = Style::default().fg(palette.text).bg(palette.bg);
                    if offset == 0 {
                        st = st.fg(Color::LightGreen).add_modifier(Modifier::BOLD);
                    } else {
                        st = st.fg(Color::LightYellow).add_modifier(Modifier::BOLD);
                    }
                    c.set_style(st);
                }
            }
        }
        // Right.
        let rw = right.chars().count() as u16;
        if rw < w {
            let rx = area.x + w.saturating_sub(rw);
            for (i, ch) in right.chars().enumerate() {
                let x = rx + i as u16;
                if x >= area.x + w {
                    break;
                }
                if let Some(c) = buf.cell_mut((x, label_y)) {
                    c.set_char(ch);
                }
            }
        }
        // Interaction hint between left and middle (only if we have room).
        let total_static_w = left.chars().count() + mw as usize + rw as usize;
        if w as usize > total_static_w + hint.chars().count() + 4 {
            let hx = area.x + left.chars().count() as u16 + 2;
            for (i, ch) in hint.chars().enumerate() {
                let x = hx + i as u16;
                if x >= area.x + w {
                    break;
                }
                if let Some(c) = buf.cell_mut((x, label_y)) {
                    c.set_char(ch);
                    c.set_style(Style::default().fg(palette.muted).bg(palette.bg));
                }
            }
        }
    }
}

fn render_hover_tooltip(
    frame: &mut Frame,
    chart_area: Rect,
    vol_area: Rect,
    visible: &[crate::markets::Ohlc],
    candle_w: u16,
    mx: u16,
    my: u16,
    palette: &Palette,
) {
    if visible.is_empty() {
        return;
    }
    let cw = candle_w.max(1);
    let in_chart = mx >= chart_area.x
        && mx < chart_area.x + chart_area.width
        && my >= chart_area.y
        && my < chart_area.y + chart_area.height;
    let in_vol = mx >= vol_area.x
        && mx < vol_area.x + vol_area.width
        && my >= vol_area.y
        && my < vol_area.y + vol_area.height;
    if !in_chart && !in_vol {
        return;
    }
    let col = mx.saturating_sub(chart_area.x) / cw;
    let idx = (col as usize).min(visible.len().saturating_sub(1));
    let c = &visible[idx];
    if c.is_gap {
        // Hovering a session gap — no real OHLC to show.
        return;
    }
    let up = c.close >= c.open;
    let arrow = if up { "+" } else { "-" };
    let dir = if up {
        Color::LightGreen
    } else {
        Color::LightRed
    };
    let lines = [
        format!(" {}  {} ", c.date, arrow),
        format!(" O {:.2}", c.open),
        format!(" H {:.2}", c.high),
        format!(" L {:.2}", c.low),
        format!(" C {:.2}", c.close),
        format!(" V {}", crate::markets::fmt_volume(c.volume)),
    ];
    let max_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(8) as u16 + 2;
    let h = lines.len() as u16 + 2;
    // Place tooltip to the right of cursor unless it would clip; then to left.
    let mut tx = mx + 2;
    if tx + max_w > chart_area.x + chart_area.width {
        tx = mx.saturating_sub(max_w + 2);
    }
    let ty = my.saturating_sub(h / 2).max(chart_area.y);
    let r = Rect {
        x: tx,
        y: ty,
        width: max_w,
        height: h,
    };
    render_clipped(frame, ratatui::widgets::Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.accent).bg(palette.bg))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(r);
    render_clipped(frame, block, r);
    for (i, line) in lines.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let style = if i == 0 {
            Style::default()
                .fg(dir)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        render_clipped(
            frame,
            Paragraph::new(line.clone()).style(style),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    ticker: &str,
    name: &str,
    tf: crate::markets::Timeframe,
    chart_type: chart::ChartType,
    working: &[crate::markets::Ohlc],
    aggregate: u16,
    chart_offset: usize,
    history_total: usize,
    date_from: Option<&str>,
    date_to: Option<&str>,
    session: crate::markets::SessionWindow,
    palette: &Palette,
) -> (Rect, Rect, Rect) {
    // Empty data path.  Two distinct cases handled here:
    //   (a) symbol just got opened, the fetch is in flight  -> "loading..."
    //   (b) user-applied filter excludes every candle (e.g. invalid range
    //       Apr 9 -> Apr 1) -> render the FILTER PILLS so the user can see
    //       what's currently restricting the view and click to adjust it.
    //
    // We can't distinguish (a) from (b) without raw-data context here, so
    // we use the presence of any active filter as the heuristic: if at
    // least one filter is set, we're in case (b).
    if working.is_empty() {
        let filtering = date_from.is_some() || date_to.is_some() || !session.is_off();

        // Line 1 — ticker pill + symbol name (same as the populated path).
        let line1 = Line::from(vec![
            Span::styled(
                format!("  {}  ", ticker),
                Style::default()
                    .fg(palette.bg)
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                name.to_string(),
                Style::default().fg(palette.text).bg(palette.bg),
            ),
        ]);

        // Line 2 — empty-state explanation.
        let (status, status_color) = if filtering {
            (
                "  no candles match the current filter -- click a pill to adjust".to_string(),
                palette.error,
            )
        } else {
            ("  loading...".to_string(), palette.muted)
        };
        let line2 = Line::from(vec![Span::styled(
            status,
            Style::default().fg(status_color).bg(palette.bg),
        )]);

        // Line 3 prefix — same TF / type metadata as the populated path,
        // followed by the date pills (only when filtering).  The pills need
        // to remain clickable so the user can fix an invalid range without
        // having to clear filters via the menu.
        let prefix = if filtering {
            format!("  {}  |  {}  |  ", tf.name(), chart_type.name())
        } else {
            format!(
                "  {}  |  {}  |  press R to retry",
                tf.name(),
                chart_type.name()
            )
        };
        let p = Paragraph::new(vec![line1, line2])
            .style(Style::default().fg(palette.text).bg(palette.bg));
        render_clipped(frame, p, area);

        // Render line 3 manually so we can emit clickable pill rects.
        if !filtering {
            // No filter to draw — fall back to the static prefix line.
            let line3 = Line::from(vec![Span::styled(
                prefix,
                Style::default().fg(palette.muted).bg(palette.bg),
            )]);
            let p3 =
                Paragraph::new(vec![line3]).style(Style::default().fg(palette.text).bg(palette.bg));
            render_clipped(
                frame,
                p3,
                Rect {
                    x: area.x,
                    y: area.y + 2,
                    width: area.width,
                    height: 1,
                },
            );
            return (Rect::default(), Rect::default(), Rect::default());
        }

        // Filtering is active — paint the prefix and pills cell-by-cell so
        // each pill gets a precise clickable rect.
        let muted = Style::default().fg(palette.muted).bg(palette.bg);
        let pill_active = Style::default()
            .fg(palette.bg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD);
        let pill_idle = Style::default()
            .fg(palette.text)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD);
        let row_y = area.y + 2;
        let row_end_x = area.x + area.width;
        let mut cur_x = area.x.saturating_add(2);
        let mut paint = |x: u16, text: &str, style: Style| -> u16 {
            let buf = frame.buffer_mut();
            let mut k = 0u16;
            for ch in text.chars() {
                let cx = x + k;
                if cx >= row_end_x {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, row_y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                k = k.saturating_add(1);
            }
            x.saturating_add(k)
        };

        let prefix_inline = format!("{}  |  {}  |  ", tf.name(), chart_type.name());
        cur_x = paint(cur_x, &prefix_inline, muted);

        // FROM pill — fall back to "(from)" placeholder when unset.
        let from_label = date_from.unwrap_or("(from)").to_string();
        let from_text = format!("[ {} ]", from_label);
        let from_w = from_text.chars().count() as u16;
        let from_rect = Rect {
            x: cur_x,
            y: row_y,
            width: from_w,
            height: 1,
        };
        let from_style = if date_from.is_some() {
            pill_active
        } else {
            pill_idle
        };
        cur_x = paint(cur_x, &from_text, from_style);

        cur_x = paint(cur_x, " -> ", muted);

        // TO pill.
        let to_label = date_to.unwrap_or("(to)").to_string();
        let to_text = format!("[ {} ]", to_label);
        let to_w = to_text.chars().count() as u16;
        let to_rect = Rect {
            x: cur_x,
            y: row_y,
            width: to_w,
            height: 1,
        };
        let to_style = if date_to.is_some() {
            pill_active
        } else {
            pill_idle
        };
        cur_x = paint(cur_x, &to_text, to_style);

        cur_x = paint(cur_x, "  |  ", muted);

        // Session pill.
        let session_label = format!("[ sess {} ]", session.label());
        let session_w = session_label.chars().count() as u16;
        let session_rect = Rect {
            x: cur_x,
            y: row_y,
            width: session_w,
            height: 1,
        };
        let session_style = if session.is_off() {
            pill_idle
        } else {
            pill_active
        };
        let _ = paint(cur_x, &session_label, session_style);

        return (from_rect, to_rect, session_rect);
    }

    let last = &working[working.len() - 1];
    let first = &working[0];
    let change_abs = last.close - first.open;
    let change_pct = if first.open > 0.0 {
        (change_abs / first.open) * 100.0
    } else {
        0.0
    };
    let up = change_abs >= 0.0;
    let arrow = if up { "+" } else { "-" };
    let change_color = if up {
        Color::LightGreen
    } else {
        Color::LightRed
    };

    let line1 = Line::from(vec![
        Span::styled(
            format!("  {}  ", ticker),
            Style::default()
                .fg(palette.bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            name.to_string(),
            Style::default().fg(palette.text).bg(palette.bg),
        ),
    ]);

    let line2 = Line::from(vec![
        Span::styled(
            format!("  ${:>8.2}  ", last.close),
            Style::default()
                .fg(palette.text)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{}{:.2} ({}{:.2}%)  ",
                arrow,
                change_abs.abs(),
                arrow,
                change_pct.abs()
            ),
            Style::default()
                .fg(change_color)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "O {:.2}  H {:.2}  L {:.2}  C {:.2}  V {}",
                last.open,
                last.high,
                last.low,
                last.close,
                fmt_volume(last.volume)
            ),
            Style::default().fg(palette.muted).bg(palette.bg),
        ),
    ]);

    let n = working.len();
    let agg_label = if aggregate > 1 {
        format!("  |  agg x{}", aggregate)
    } else {
        String::new()
    };
    let secs = tf.secs_to_close();
    let countdown = if tf.duration_secs() <= 60 {
        format!("{:02}s", secs)
    } else if tf.duration_secs() <= 3600 {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    } else {
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    };
    let scroll_label = if chart_offset == 0 {
        "LIVE".to_string()
    } else {
        format!("scrolled -{}/{}", chart_offset, history_total)
    };
    // Render the static lines 1+2 first.
    let p =
        Paragraph::new(vec![line1, line2]).style(Style::default().fg(palette.text).bg(palette.bg));
    render_clipped(frame, p, area);

    // Line 3 — render manually so we can return precise rects for the
    // clickable date / session pills.  Format:
    //
    //   TF  |  TYPE  |  N candles[ x agg]  |  [ FROM ] -> [ TO ]
    //         |  sess [ window ]  |  next close in HH:MM:SS  |  LIVE
    //
    // For synthetic data (`first.date` starts with 'D') we skip the date
    // pills entirely — there's no real calendar to filter by.
    let synthetic = first.date.starts_with('D');
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let pill_active = Style::default()
        .fg(palette.bg)
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let pill_idle = Style::default()
        .fg(palette.text)
        .bg(palette.bg)
        .add_modifier(Modifier::BOLD);

    let row_y = area.y + 2;
    let mut cur_x = area.x.saturating_add(2);
    let row_end_x = area.x + area.width;

    let mut paint = |x: u16, text: &str, style: Style| -> u16 {
        let buf = frame.buffer_mut();
        let mut k = 0u16;
        for ch in text.chars() {
            let cx = x + k;
            if cx >= row_end_x {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, row_y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            k = k.saturating_add(1);
        }
        x.saturating_add(k)
    };

    // Static prefix.
    let prefix = format!(
        "{}  |  {}  |  {} candles{}  |  ",
        tf.name(),
        chart_type.name(),
        n,
        agg_label
    );
    cur_x = paint(cur_x, &prefix, muted);

    // Date pills.  Each pill is 12 chars wide ("[ YYYY-MM-DD ]") so the
    // user has a generous click target.
    let mut from_rect = Rect::default();
    let mut to_rect = Rect::default();
    if !synthetic {
        let from_label = date_from
            .map(|s| s.to_string())
            .unwrap_or(first.date.clone());
        let to_label = date_to.map(|s| s.to_string()).unwrap_or(last.date.clone());

        let from_text = format!("[ {} ]", from_label);
        let from_style = if date_from.is_some() {
            pill_active
        } else {
            pill_idle
        };
        let from_w = from_text.chars().count() as u16;
        from_rect = Rect {
            x: cur_x,
            y: row_y,
            width: from_w,
            height: 1,
        };
        cur_x = paint(cur_x, &from_text, from_style);

        cur_x = paint(cur_x, " -> ", muted);

        let to_text = format!("[ {} ]", to_label);
        let to_style = if date_to.is_some() {
            pill_active
        } else {
            pill_idle
        };
        let to_w = to_text.chars().count() as u16;
        to_rect = Rect {
            x: cur_x,
            y: row_y,
            width: to_w,
            height: 1,
        };
        cur_x = paint(cur_x, &to_text, to_style);

        cur_x = paint(cur_x, "  |  ", muted);
    }

    // Session pill.
    let session_label = format!("[ sess {} ]", session.label());
    let session_w = session_label.chars().count() as u16;
    let session_rect = Rect {
        x: cur_x,
        y: row_y,
        width: session_w,
        height: 1,
    };
    let session_style = if session.is_off() {
        pill_idle
    } else {
        pill_active
    };
    cur_x = paint(cur_x, &session_label, session_style);

    // Tail: countdown + scroll label.
    let tail = format!("  |  next close in {}  |  {}", countdown, scroll_label);
    let _ = paint(cur_x, &tail, muted);

    (from_rect, to_rect, session_rect)
}

/// Paint thin vertical dividers between trading sessions:
///   * `is_gap` columns (session-filter boundaries) → full-height `│`
///     in muted color, drawn at the column center.  These columns are
///     empty by construction so there's no candle to clobber.
///   * Day-boundary columns (date string changed vs. previous non-gap
///     candle) → `│` at the LEFT EDGE of the new-day column, but only
///     when each candle is at least 2 cells wide so the line doesn't
///     wipe out the candle entirely.  The remaining `cw - 1` cells of
///     the candle stay readable.
///   * On daily TFs (≈ 1 candle per date) day-boundary dividers are
///     suppressed because they would draw on every column.
fn render_session_dividers(
    frame: &mut Frame,
    chart_area: Rect,
    xaxis_area: Rect,
    vol_area: Rect,
    visible: &[crate::markets::Ohlc],
    candle_w: u16,
    palette: &Palette,
) {
    if visible.is_empty() {
        return;
    }
    let cw = candle_w.max(1);

    // Bright accent tick for the X-axis row — yellow stands out
    // against muted date labels.
    let tick_style = Style::default()
        .fg(palette.accent)
        .bg(palette.bg)
        .add_modifier(Modifier::BOLD);
    // Solid muted line for the chart + volume body of the gap column.
    let line_style = Style::default()
        .fg(palette.muted)
        .bg(palette.bg)
        .add_modifier(Modifier::BOLD);

    let buf = frame.buffer_mut();
    for (i, c) in visible.iter().enumerate() {
        if !c.is_gap {
            continue;
        }
        let x_center = chart_area.x + (i as u16) * cw + cw / 2;
        if x_center >= chart_area.x + chart_area.width {
            continue;
        }

        // Full-height vertical line through chart + volume — the gap
        // column is empty by construction, so nothing to clobber.
        for y in chart_area.y..(chart_area.y + chart_area.height) {
            if let Some(cell) = buf.cell_mut((x_center, y)) {
                cell.set_char('│');
                cell.set_style(line_style);
            }
        }
        for y in vol_area.y..(vol_area.y + vol_area.height) {
            if x_center >= vol_area.x + vol_area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x_center, y)) {
                cell.set_char('│');
                cell.set_style(line_style);
            }
        }

        // Bright X-axis tick so the boundary is obvious between dates.
        if x_center < xaxis_area.x + xaxis_area.width {
            if let Some(cell) = buf.cell_mut((x_center, xaxis_area.y)) {
                cell.set_char('|');
                cell.set_style(tick_style);
            }
        }
    }
}

fn render_xaxis_dates(
    frame: &mut Frame,
    area: Rect,
    visible: &[crate::markets::Ohlc],
    candle_w: u16,
    palette: &Palette,
) {
    if visible.is_empty() || area.width == 0 {
        return;
    }
    let cw = candle_w.max(1);
    let buf = frame.buffer_mut();
    let n = visible.len();

    // Pre-clear row.
    for col in 0..area.width {
        if let Some(cell) = buf.cell_mut((area.x + col, area.y)) {
            cell.set_char(' ');
            cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
        }
    }

    // 4 markers across.
    let markers = 4u16.min(n as u16);
    if markers == 0 {
        return;
    }
    for i in 0..markers {
        let frac = i as f64 / (markers - 1).max(1) as f64;
        let mut candle_idx = ((n - 1) as f64 * frac).round() as usize;
        candle_idx = candle_idx.min(n - 1);
        // Walk away from a gap sentinel so the marker lands on a real candle.
        if visible[candle_idx].is_gap {
            let mut found = None;
            for off in 1..n {
                let lo = candle_idx.saturating_sub(off);
                if !visible[lo].is_gap {
                    found = Some(lo);
                    break;
                }
                if candle_idx + off < n && !visible[candle_idx + off].is_gap {
                    found = Some(candle_idx + off);
                    break;
                }
            }
            if let Some(idx) = found {
                candle_idx = idx;
            } else {
                continue;
            }
        }
        let label = short_date(&visible[candle_idx].date);
        let label_w = label.chars().count() as u16;

        let target_x = area.x + ((area.width - 1) as f64 * frac).round() as u16;
        // Center label around target_x (clamp to bounds).
        let start_x = target_x.saturating_sub(label_w / 2);
        let start_x = start_x.min(area.x + area.width.saturating_sub(label_w));

        // Optional tick mark at the candle center column.
        let tick_x = area.x + (candle_idx as u16) * cw + cw / 2;
        if tick_x < area.x + area.width {
            if let Some(cell) = buf.cell_mut((tick_x, area.y)) {
                cell.set_char('|');
                cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
            }
        }

        for (j, ch) in label.chars().enumerate() {
            let x = start_x + j as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(palette.muted).bg(palette.bg));
            }
        }
    }
}

/// Return a short label suitable for the chart x-axis.  Yahoo dates are
/// `YYYY-MM-DD`; we trim to `MM-DD`.  Synthetic dates are `Dnnn` already
/// short.
fn short_date(s: &str) -> String {
    if s.len() == 10 && s.chars().nth(4) == Some('-') {
        s[5..].to_string() // "MM-DD"
    } else {
        s.to_string()
    }
}

fn render_axis_label(frame: &mut Frame, area: Rect, text: &str, palette: &Palette) {
    let p = Paragraph::new(text).style(Style::default().fg(palette.muted).bg(palette.bg));
    render_clipped(frame, p, area);
}
