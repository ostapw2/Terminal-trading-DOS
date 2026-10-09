//! F9 Settings — full-page configuration view.
//!
//! Layout (uses the entire panels area, not a cramped centered dialog):
//!
//! ```text
//! +-------------------------- Settings -----------------------------+
//! | Market & timeframe       | API credentials                      |
//! |  • Market type           |  Binance Spot                        |
//! |    [x] Crypto            |    Key:    [........]                |
//! |    [ ] US Stocks         |    Secret: [........]                |
//! |    ...                   |  Binance Futures                     |
//! |  • Default timeframe     |    Key:    [........]                |
//! |    [x] 1d                |    Secret: [........]                |
//! |    ...                   |  Yahoo Finance                       |
//! |                          |    (no key required, public chart    |
//! |                          |     endpoints).                      |
//! |                          |  Alpaca / IBKR (coming soon)         |
//! +------------------------------------------------------------------+
//! | Cache: 1.2 MB   DB: ~/.dos/data.db                              |
//! |                              [ Apply ]  [ ClearCache ]  [ Cancel ]
//! +------------------------------------------------------------------+
//! ```
//!
//! Tab cycles focus: Market(5) → Timeframe(5) → ObShowTrades → SpotKey →
//! SpotSecret → FutKey → FutSecret → FontSize → Buttons.

use crate::widgets::fmt::{rect_contains, render_clipped};
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::markets::{MarketType, Timeframe};
use crate::tokens::Palette;
use crate::widgets::chart::HistoryRange;
use crate::widgets::{button, checkbox, input as input_w, input::InputState, radio};

#[derive(Clone, Copy, PartialEq)]
pub enum SettingsAction {
    None,
    Cancel,
    Apply,
    ClearCache,
}

pub const MARKETS: &[(&str, MarketType)] = &[
    (
        "US Stocks    -  AAPL, GOOGL, MSFT, ...",
        MarketType::UsStocks,
    ),
    ("Crypto       -  BTC, ETH, SOL, ...", MarketType::Crypto),
    ("EU Stocks    -  ASML, SAP, NESN, ...", MarketType::EuStocks),
    ("Forex        -  EUR/USD, GBP/USD, ...", MarketType::Forex),
    (
        "Commodities  -  Gold, Oil, Copper, ...",
        MarketType::Commodities,
    ),
];

pub const TIMEFRAMES: &[(&str, Timeframe)] = &[
    ("15 sec    (intraday)", Timeframe::S15),
    ("1 min     (intraday)", Timeframe::M1),
    ("5 min     (intraday)", Timeframe::M5),
    ("1 hour    (intraday)", Timeframe::H1),
    ("1 day     (default)", Timeframe::D1),
];

pub const BUTTONS: &[&str] = &["Apply", "ClearCache", "Cancel"];

pub const FONT_SIZE_DEFAULT: i32 = 14;
pub const FONT_SIZE_MIN: i32 = 6;
pub const FONT_SIZE_MAX: i32 = 48;

/// Focusable controls after the market and timeframe radio rows, in Tab
/// order.  The ONLY place that defines focus positions: `focus_index` and
/// `zone_of` are derived from it, nothing else counts constants (audit W7).
const FIXED_ZONES: [Zone; 9] = [
    Zone::ObShowTrades,
    Zone::ChartTodayOnly,
    Zone::SpotKey,
    Zone::SpotSecret,
    Zone::FutKey,
    Zone::FutSecret,
    Zone::FontSize,
    Zone::LiveToggle,
    Zone::Buttons,
];

fn total_focusable() -> usize {
    MARKETS.len() + TIMEFRAMES.len() + FIXED_ZONES.len()
}

/// Focus index of a zone.
fn focus_index(zone: Zone) -> usize {
    match zone {
        Zone::Market(i) => i,
        Zone::Timeframe(i) => MARKETS.len() + i,
        fixed => {
            MARKETS.len()
                + TIMEFRAMES.len()
                + FIXED_ZONES
                    .iter()
                    .position(|z| *z == fixed)
                    .expect("fixed zone listed in FIXED_ZONES")
        }
    }
}

pub struct SettingsState {
    pub selected_market: usize,
    pub selected_tf: usize,
    /// Show the live trades feed in the F6 Order Book view.
    pub ob_show_trades: bool,
    /// History range display filter: All / Today / Last 3.5 days.
    /// Set from the Markets History menu or cycled here via Space.
    pub chart_history_range: HistoryRange,
    /// Binance Spot API.
    pub api_key: InputState,
    pub api_secret: InputState,
    /// Binance Futures API (separate key set).
    pub futures_api_key: InputState,
    pub futures_api_secret: InputState,
    /// Preferred terminal font size in points.  Sent as an OSC 50 hint on
    /// Apply; xterm/urxvt honor it directly, iTerm2/Terminal.app/kitty
    /// require a manual Cmd/Ctrl ± step.
    pub font_size: i32,
    /// Live trading toggle (requires API credentials).
    pub live_enabled: bool,
    pub focused: usize,
    pub btn: usize,
}

impl SettingsState {
    pub fn new(market: MarketType, tf: Timeframe, key: &str, secret: &str, live: bool) -> Self {
        Self::new_full(
            market,
            tf,
            key,
            secret,
            "",
            "",
            true,
            HistoryRange::All,
            FONT_SIZE_DEFAULT,
            live,
        )
    }

    pub fn new_full(
        market: MarketType,
        tf: Timeframe,
        spot_key: &str,
        spot_secret: &str,
        fut_key: &str,
        fut_secret: &str,
        ob_show_trades: bool,
        chart_history_range: HistoryRange,
        font_size: i32,
        live_enabled: bool,
    ) -> Self {
        let m_idx = MARKETS.iter().position(|(_, m)| *m == market).unwrap_or(0);
        let t_idx = TIMEFRAMES.iter().position(|(_, t)| *t == tf).unwrap_or(4);
        let mut k = InputState::new("(none)");
        let mut s = InputState::new("(none)");
        let mut fk = InputState::new("(none)");
        let mut fs = InputState::new("(none)");
        s.mask = true;
        fs.mask = true;
        for c in spot_key.chars() {
            k.push_char(c);
        }
        for c in spot_secret.chars() {
            s.push_char(c);
        }
        for c in fut_key.chars() {
            fk.push_char(c);
        }
        for c in fut_secret.chars() {
            fs.push_char(c);
        }
        Self {
            selected_market: m_idx,
            selected_tf: t_idx,
            ob_show_trades,
            chart_history_range,
            api_key: k,
            api_secret: s,
            futures_api_key: fk,
            futures_api_secret: fs,
            font_size: font_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX),
            live_enabled,
            focused: m_idx,
            btn: 0,
        }
    }
}

pub struct SettingsLayout {
    pub area: Rect,
    pub market_rows: Vec<Rect>,
    pub timeframe_rows: Vec<Rect>,
    pub ob_trades_rect: Rect,
    pub chart_today_rect: Rect,
    pub font_dec_rect: Rect,
    pub font_inc_rect: Rect,
    pub key_rect: Rect,
    pub secret_rect: Rect,
    pub fut_key_rect: Rect,
    pub fut_secret_rect: Rect,
    pub live_toggle_rect: Rect,
    pub apply_rect: Rect,
    pub clear_rect: Rect,
    pub cancel_rect: Rect,
}

pub fn render(
    frame: &mut Frame,
    screen: Rect,
    st: &SettingsState,
    current_market: MarketType,
    current_tf: Timeframe,
    cache_size_label: &str,
    mouse: Option<(u16, u16)>,
    pressed_button: Option<usize>,
    palette: &Palette,
) -> SettingsLayout {
    // Use the entire panel area, not a centered dialog.
    let area = screen;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            format!(
                " Settings    market={}    timeframe={}    cache={} ",
                current_market.name(),
                current_tf.name(),
                cache_size_label
            ),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);

    // Vertical split: body (most of the height) + footer (3 lines).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(inner);
    let body = rows[0];
    let footer = rows[1];

    // Body: two equal columns separated by a 1-col gap.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(48),
            Constraint::Length(2),
            Constraint::Percentage(48),
        ])
        .split(body);
    let left_col = cols[0];
    let right_col = cols[2];

    let (
        market_rows,
        timeframe_rows,
        ob_trades_rect,
        chart_today_rect,
        font_dec_rect,
        font_inc_rect,
    ) = render_left(frame, left_col, st, mouse, palette);
    let (key_rect, secret_rect, fut_key_rect, fut_secret_rect, live_toggle_rect) =
        render_right(frame, right_col, st, palette);
    let (apply_rect, clear_rect, cancel_rect) =
        render_footer(frame, footer, st, mouse, pressed_button, palette);

    SettingsLayout {
        area,
        market_rows,
        timeframe_rows,
        ob_trades_rect,
        chart_today_rect,
        font_dec_rect,
        font_inc_rect,
        key_rect,
        secret_rect,
        fut_key_rect,
        fut_secret_rect,
        live_toggle_rect,
        apply_rect,
        clear_rect,
        cancel_rect,
    }
}

fn render_section_header(frame: &mut Frame, area: Rect, label: &str, palette: &Palette) {
    render_clipped(
        frame,
        Paragraph::new(label).style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn render_left(
    frame: &mut Frame,
    area: Rect,
    st: &SettingsState,
    mouse: Option<(u16, u16)>,
    palette: &Palette,
) -> (Vec<Rect>, Vec<Rect>, Rect, Rect, Rect, Rect) {
    if area.width < 8 {
        return (
            Vec::new(),
            Vec::new(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
        );
    }
    let pad = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
    .intersection(frame.area());

    let mut y = pad.y;
    render_section_header(
        frame,
        Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        },
        "--Market type",
        palette,
    );
    y += 2;
    let market_h = (MARKETS.len() as u16) + 2;
    let market_rect = Rect {
        x: pad.x,
        y,
        width: pad.width,
        height: market_h,
    }
    .intersection(pad);
    let m_labels: Vec<&str> = MARKETS.iter().map(|(l, _)| *l).collect();
    radio::render(
        frame,
        market_rect,
        &m_labels,
        st.selected_market,
        st.focused < MARKETS.len(),
        palette,
    );
    let market_rows: Vec<Rect> = (0..MARKETS.len())
        .map(|i| {
            Rect {
                x: market_rect.x + 1,
                y: market_rect.y + 1 + i as u16,
                width: market_rect.width.saturating_sub(2),
                height: 1,
            }
            .intersection(pad)
        })
        .collect();
    y += market_h + 1;

    if y < pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Default timeframe",
            palette,
        );
        y += 2;
    }
    let tf_h = (TIMEFRAMES.len() as u16) + 2;
    let tf_rect = Rect {
        x: pad.x,
        y,
        width: pad.width,
        height: tf_h.min((pad.y + pad.height).saturating_sub(y)),
    }
    .intersection(pad);
    let t_labels: Vec<&str> = TIMEFRAMES.iter().map(|(l, _)| *l).collect();
    let tf_focused = st.focused >= MARKETS.len() && st.focused < MARKETS.len() + TIMEFRAMES.len();
    radio::render(
        frame,
        tf_rect,
        &t_labels,
        st.selected_tf,
        tf_focused,
        palette,
    );
    let timeframe_rows: Vec<Rect> = (0..TIMEFRAMES.len())
        .map(|i| {
            Rect {
                x: tf_rect.x + 1,
                y: tf_rect.y + 1 + i as u16,
                width: tf_rect.width.saturating_sub(2),
                height: 1,
            }
            .intersection(pad)
        })
        .collect();
    y += tf_h + 1;

    // ── Order Book ─────────────────────────────────────────────────────
    let mut ob_trades_rect = Rect::default();
    if y + 2 <= pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Order Book",
            palette,
        );
        y += 1;
        let ob_focused = st.focused == focus_index(Zone::ObShowTrades);
        ob_trades_rect = Rect {
            x: pad.x + 1,
            y,
            width: pad.width.saturating_sub(2),
            height: 1,
        }
        .intersection(pad);
        checkbox::render(
            frame,
            ob_trades_rect,
            "Show trades feed in Order Book view",
            st.ob_show_trades,
            ob_focused,
            palette,
        );
        y += 2;
    }

    // ── Chart ──────────────────────────────────────────────────────────
    let mut chart_today_rect = Rect::default();
    if y + 2 <= pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Chart  (Space to cycle)",
            palette,
        );
        y += 1;
        let chart_focused = matches!(zone_of(st.focused), Zone::ChartTodayOnly);
        chart_today_rect = Rect {
            x: pad.x + 1,
            y,
            width: pad.width.saturating_sub(2),
            height: 1,
        }
        .intersection(pad);
        // Radio-style inline display for the three range modes.
        let range_label = format!(
            "History range:  {} {} {}",
            if st.chart_history_range == HistoryRange::All {
                "[ x ] All"
            } else {
                "[   ] All"
            },
            if st.chart_history_range == HistoryRange::Today {
                "[ x ] Today"
            } else {
                "[   ] Today"
            },
            if st.chart_history_range == HistoryRange::Last3_5Days {
                "[ x ] 3.5d"
            } else {
                "[   ] 3.5d"
            },
        );
        let range_style = if chart_focused {
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.bg)
        };
        render_clipped(
            frame,
            ratatui::widgets::Paragraph::new(range_label).style(range_style),
            chart_today_rect,
        );
        y += 2;
    }

    // ── Font size ──────────────────────────────────────────────────────
    let mut font_dec_rect = Rect::default();
    let mut font_inc_rect = Rect::default();
    if y + 3 <= pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Font size",
            palette,
        );
        y += 1;
        let font_focused = matches!(zone_of(st.focused), Zone::FontSize);
        let dec_x = pad.x + 2;
        font_dec_rect = Rect {
            x: dec_x,
            y,
            width: 3,
            height: 1,
        }
        .intersection(pad);
        let val_rect = Rect {
            x: dec_x + 4,
            y,
            width: 4,
            height: 1,
        }
        .intersection(pad);
        font_inc_rect = Rect {
            x: dec_x + 9,
            y,
            width: 3,
            height: 1,
        }
        .intersection(pad);

        let dec_hover = matches!(mouse, Some((mx, my))
            if my == font_dec_rect.y
                && mx >= font_dec_rect.x
                && mx < font_dec_rect.x + font_dec_rect.width);
        let inc_hover = matches!(mouse, Some((mx, my))
            if my == font_inc_rect.y
                && mx >= font_inc_rect.x
                && mx < font_inc_rect.x + font_inc_rect.width);

        let stepper_style = |hover: bool, at_limit: bool| -> Style {
            if at_limit {
                Style::default().fg(palette.muted).bg(palette.bg)
            } else if font_focused || hover {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text).bg(palette.panel)
            }
        };
        render_clipped(
            frame,
            Paragraph::new("[-]").style(stepper_style(dec_hover, st.font_size <= FONT_SIZE_MIN)),
            font_dec_rect,
        );
        render_clipped(
            frame,
            Paragraph::new(format!(" {:>2} ", st.font_size)).style(
                Style::default()
                    .fg(palette.text)
                    .bg(palette.bg)
                    .add_modifier(if font_focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            val_rect,
        );
        render_clipped(
            frame,
            Paragraph::new("[+]").style(stepper_style(inc_hover, st.font_size >= FONT_SIZE_MAX)),
            font_inc_rect,
        );
        // Hint line — explains why some terminals ignore the OSC.
        if y + 1 < pad.y + pad.height {
            let hint_y = y + 1;
            let hint_w = pad.width.saturating_sub(2);
            if hint_w > 0 {
                render_clipped(
                    frame,
                    Paragraph::new("  xterm/urxvt apply via OSC 50; iTerm2/Terminal: Cmd ±")
                        .style(Style::default().fg(palette.muted).bg(palette.bg)),
                    Rect {
                        x: pad.x + 2,
                        y: hint_y,
                        width: hint_w,
                        height: 1,
                    },
                );
            }
        }
    }

    (
        market_rows,
        timeframe_rows,
        ob_trades_rect,
        chart_today_rect,
        font_dec_rect,
        font_inc_rect,
    )
}

fn render_right(
    frame: &mut Frame,
    area: Rect,
    st: &SettingsState,
    palette: &Palette,
) -> (Rect, Rect, Rect, Rect, Rect) {
    if area.width < 16 {
        return (
            Rect::default(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
        );
    }
    let pad = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
    .intersection(frame.area());
    let label_w: u16 = 9;
    let mut y = pad.y;

    // ── Binance Spot ──
    render_section_header(
        frame,
        Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        },
        "--Binance Spot",
        palette,
    );
    y += 1;
    render_clipped(
        frame,
        Paragraph::new("  Read-only API key (klines / depth / aggTrades). Stored locally.")
            .style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        },
    );
    y += 1;
    let key_focus = st.focused == focus_index(Zone::SpotKey);
    let secret_focus = st.focused == focus_index(Zone::SpotSecret);
    render_clipped(
        frame,
        Paragraph::new("Key:").style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: label_w,
            height: 1,
        },
    );
    let key_rect = Rect {
        x: pad.x + label_w,
        y,
        width: pad.width.saturating_sub(label_w),
        height: 1,
    }
    .intersection(pad);
    input_w::render(frame, key_rect, &st.api_key, key_focus, palette);
    y += 1;
    render_clipped(
        frame,
        Paragraph::new("Secret:").style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: label_w,
            height: 1,
        },
    );
    let secret_rect = Rect {
        x: pad.x + label_w,
        y,
        width: pad.width.saturating_sub(label_w),
        height: 1,
    }
    .intersection(pad);
    input_w::render(frame, secret_rect, &st.api_secret, secret_focus, palette);
    y += 2;

    // ── Binance Futures ──
    render_section_header(
        frame,
        Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        },
        "--Binance Futures (USDT-M)",
        palette,
    );
    y += 1;
    render_clipped(
        frame,
        Paragraph::new("  Optional. Used when screener venue = Futures.")
            .style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: pad.width,
            height: 1,
        },
    );
    y += 1;
    let fut_key_focus = st.focused == focus_index(Zone::FutKey);
    let fut_secret_focus = st.focused == focus_index(Zone::FutSecret);
    render_clipped(
        frame,
        Paragraph::new("Key:").style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: label_w,
            height: 1,
        },
    );
    let fut_key_rect = Rect {
        x: pad.x + label_w,
        y,
        width: pad.width.saturating_sub(label_w),
        height: 1,
    }
    .intersection(pad);
    input_w::render(
        frame,
        fut_key_rect,
        &st.futures_api_key,
        fut_key_focus,
        palette,
    );
    y += 1;
    render_clipped(
        frame,
        Paragraph::new("Secret:").style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y,
            width: label_w,
            height: 1,
        },
    );
    let fut_secret_rect = Rect {
        x: pad.x + label_w,
        y,
        width: pad.width.saturating_sub(label_w),
        height: 1,
    }
    .intersection(pad);
    input_w::render(
        frame,
        fut_secret_rect,
        &st.futures_api_secret,
        fut_secret_focus,
        palette,
    );
    y += 2;

    // ── Live Trading ──
    let live_toggle_rect = if y + 3 <= pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Live Trading",
            palette,
        );
        y += 1;
        let live_focus = matches!(zone_of(st.focused), Zone::LiveToggle);
        let toggle_rect = Rect {
            x: pad.x + 2,
            y,
            width: 6,
            height: 1,
        }
        .intersection(pad);
        crate::widgets::switch::render(frame, toggle_rect, st.live_enabled, live_focus, palette);
        let status_label = if st.live_enabled { "ON" } else { "OFF" };
        let status_color = if st.live_enabled {
            palette.error
        } else {
            palette.muted
        };
        render_clipped(
            frame,
            Paragraph::new(format!(
                "  Live mode is {status_label}   (requires API key + secret)"
            ))
            .style(Style::default().fg(status_color).bg(palette.bg)),
            Rect {
                x: toggle_rect.x + 7,
                y,
                width: pad.width.saturating_sub(9),
                height: 1,
            },
        );
        y += 2;
        toggle_rect
    } else {
        Rect::default()
    };

    // ── Yahoo Finance (informational, no key) ──
    if y < pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Yahoo Finance",
            palette,
        );
        y += 1;
    }
    if y < pad.y + pad.height {
        render_clipped(
            frame,
            Paragraph::new("  No key required — public v8/chart endpoints.")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
        );
        y += 2;
    }

    // ── Alpaca / IBKR placeholder ──
    if y < pad.y + pad.height {
        render_section_header(
            frame,
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
            "--Alpaca / IBKR / Polygon",
            palette,
        );
        y += 1;
    }
    if y < pad.y + pad.height {
        render_clipped(
            frame,
            Paragraph::new("  (coming soon — for US/EU stocks live data)")
                .style(Style::default().fg(palette.muted).bg(palette.bg)),
            Rect {
                x: pad.x,
                y,
                width: pad.width,
                height: 1,
            },
        );
    }

    (
        key_rect,
        secret_rect,
        fut_key_rect,
        fut_secret_rect,
        live_toggle_rect,
    )
}

fn render_footer(
    frame: &mut Frame,
    area: Rect,
    st: &SettingsState,
    mouse: Option<(u16, u16)>,
    pressed_button: Option<usize>,
    palette: &Palette,
) -> (Rect, Rect, Rect) {
    let pad = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(4),
        height: area.height,
    }
    .intersection(frame.area());
    // Hint left.
    render_clipped(
        frame,
        Paragraph::new("  Tab navigates · Enter activates · Space toggles · Esc cancels")
            .style(Style::default().fg(palette.muted).bg(palette.bg)),
        Rect {
            x: pad.x,
            y: pad.y,
            width: pad.width,
            height: 1,
        },
    );

    // Buttons row at the bottom — 2 rows tall (face + shadow).
    let total_btn_w: u16 =
        BUTTONS.iter().map(|b| button::measure(b)).sum::<u16>() + (BUTTONS.len() as u16 - 1);
    let row_y = pad.y + pad.height.saturating_sub(button::HEIGHT);
    let mut bx = pad.x + pad.width.saturating_sub(total_btn_w);
    let mut button_rects: Vec<Rect> = Vec::new();
    let buttons_focused = st.focused == focus_index(Zone::Buttons);

    for (i, b) in BUTTONS.iter().enumerate() {
        let w = button::measure(b);
        let r = Rect {
            x: bx,
            y: row_y,
            width: w,
            height: button::HEIGHT,
        }
        .intersection(pad);
        let hover = mouse.is_some_and(|(mx, my)| rect_contains(r, mx, my));
        let pressed = pressed_button == Some(i);
        let focused = st.btn == i && buttons_focused;
        let state = button::pick_state(false, pressed, focused, hover, i == 0);
        button::render(frame, r, b, state, palette);
        button_rects.push(r);
        bx = bx.saturating_add(w + 1);
    }

    (button_rects[0], button_rects[1], button_rects[2])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Market(usize),
    Timeframe(usize),
    ObShowTrades,
    ChartTodayOnly,
    SpotKey,
    SpotSecret,
    FutKey,
    FutSecret,
    FontSize,
    LiveToggle,
    Buttons,
}

fn zone_of(focused: usize) -> Zone {
    let n_m = MARKETS.len();
    let n_t = TIMEFRAMES.len();
    if focused < n_m {
        Zone::Market(focused)
    } else if focused < n_m + n_t {
        Zone::Timeframe(focused - n_m)
    } else {
        FIXED_ZONES
            .get(focused - n_m - n_t)
            .copied()
            .unwrap_or(Zone::Buttons)
    }
}

pub fn handle_key(state: &mut SettingsState, code: KeyCode) -> SettingsAction {
    let total = total_focusable();
    match code {
        KeyCode::Esc => return SettingsAction::Cancel,
        KeyCode::Tab | KeyCode::Down => {
            state.focused = (state.focused + 1) % total;
        }
        KeyCode::BackTab | KeyCode::Up => {
            state.focused = (state.focused + total - 1) % total;
        }
        KeyCode::Right if matches!(zone_of(state.focused), Zone::Buttons) => {
            state.btn = (state.btn + 1) % BUTTONS.len();
        }
        KeyCode::Left if matches!(zone_of(state.focused), Zone::Buttons) => {
            state.btn = (state.btn + BUTTONS.len() - 1) % BUTTONS.len();
        }
        KeyCode::Right if matches!(zone_of(state.focused), Zone::FontSize) => {
            state.font_size = (state.font_size + 1).min(FONT_SIZE_MAX);
        }
        KeyCode::Left if matches!(zone_of(state.focused), Zone::FontSize) => {
            state.font_size = (state.font_size - 1).max(FONT_SIZE_MIN);
        }
        KeyCode::Char(c) => match zone_of(state.focused) {
            Zone::SpotKey => state.api_key.push_char(c),
            Zone::SpotSecret => state.api_secret.push_char(c),
            Zone::FutKey => state.futures_api_key.push_char(c),
            Zone::FutSecret => state.futures_api_secret.push_char(c),
            Zone::ObShowTrades if c == ' ' => return activate(state),
            Zone::ChartTodayOnly if c == ' ' => return activate(state),
            Zone::FontSize => {
                if c == '+' || c == '=' {
                    state.font_size = (state.font_size + 1).min(FONT_SIZE_MAX);
                } else if c == '-' || c == '_' {
                    state.font_size = (state.font_size - 1).max(FONT_SIZE_MIN);
                } else if c == ' ' {
                    return activate(state);
                }
            }
            Zone::LiveToggle if c == ' ' => return activate(state),
            _ if c == ' ' => return activate(state),
            _ => {}
        },
        KeyCode::Backspace => match zone_of(state.focused) {
            Zone::SpotKey => state.api_key.backspace(),
            Zone::SpotSecret => state.api_secret.backspace(),
            Zone::FutKey => state.futures_api_key.backspace(),
            Zone::FutSecret => state.futures_api_secret.backspace(),
            _ => {}
        },
        KeyCode::Delete => match zone_of(state.focused) {
            Zone::SpotKey => state.api_key.delete(),
            Zone::SpotSecret => state.api_secret.delete(),
            Zone::FutKey => state.futures_api_key.delete(),
            Zone::FutSecret => state.futures_api_secret.delete(),
            _ => {}
        },
        KeyCode::Enter => return activate(state),
        _ => {}
    }
    SettingsAction::None
}

fn activate(state: &mut SettingsState) -> SettingsAction {
    match zone_of(state.focused) {
        Zone::Market(i) => {
            state.selected_market = i;
            SettingsAction::None
        }
        Zone::Timeframe(i) => {
            state.selected_tf = i;
            SettingsAction::None
        }
        Zone::ObShowTrades => {
            state.ob_show_trades = !state.ob_show_trades;
            SettingsAction::None
        }
        Zone::ChartTodayOnly => {
            state.chart_history_range = match state.chart_history_range {
                HistoryRange::All => HistoryRange::Today,
                HistoryRange::Today => HistoryRange::Last3_5Days,
                HistoryRange::Last3_5Days => HistoryRange::All,
            };
            SettingsAction::None
        }
        Zone::SpotKey | Zone::SpotSecret | Zone::FutKey | Zone::FutSecret => SettingsAction::None,
        Zone::FontSize => SettingsAction::None,
        Zone::LiveToggle => {
            state.live_enabled = !state.live_enabled;
            SettingsAction::None
        }
        Zone::Buttons => match BUTTONS[state.btn] {
            "Apply" => SettingsAction::Apply,
            "Cancel" => SettingsAction::Cancel,
            "ClearCache" => SettingsAction::ClearCache,
            _ => SettingsAction::None,
        },
    }
}

pub fn handle_click(
    state: &mut SettingsState,
    layout: &SettingsLayout,
    x: u16,
    y: u16,
) -> SettingsAction {
    for (i, r) in layout.market_rows.iter().enumerate() {
        if rect_contains(*r, x, y) {
            state.selected_market = i;
            state.focused = i;
            return SettingsAction::None;
        }
    }
    for (i, r) in layout.timeframe_rows.iter().enumerate() {
        if rect_contains(*r, x, y) {
            state.selected_tf = i;
            state.focused = focus_index(Zone::Timeframe(i));
            return SettingsAction::None;
        }
    }
    if rect_contains(layout.ob_trades_rect, x, y) {
        state.ob_show_trades = !state.ob_show_trades;
        state.focused = focus_index(Zone::ObShowTrades);
        return SettingsAction::None;
    }
    if rect_contains(layout.chart_today_rect, x, y) {
        state.chart_history_range = match state.chart_history_range {
            HistoryRange::All => HistoryRange::Today,
            HistoryRange::Today => HistoryRange::Last3_5Days,
            HistoryRange::Last3_5Days => HistoryRange::All,
        };
        state.focused = focus_index(Zone::ChartTodayOnly);
        return SettingsAction::None;
    }
    if rect_contains(layout.key_rect, x, y) {
        state.focused = focus_index(Zone::SpotKey);
        return SettingsAction::None;
    }
    if rect_contains(layout.secret_rect, x, y) {
        state.focused = focus_index(Zone::SpotSecret);
        return SettingsAction::None;
    }
    if rect_contains(layout.fut_key_rect, x, y) {
        state.focused = focus_index(Zone::FutKey);
        return SettingsAction::None;
    }
    if rect_contains(layout.fut_secret_rect, x, y) {
        state.focused = focus_index(Zone::FutSecret);
        return SettingsAction::None;
    }
    if rect_contains(layout.font_dec_rect, x, y) {
        state.focused = focus_index(Zone::FontSize);
        state.font_size = (state.font_size - 1).max(FONT_SIZE_MIN);
        return SettingsAction::None;
    }
    if rect_contains(layout.font_inc_rect, x, y) {
        state.focused = focus_index(Zone::FontSize);
        state.font_size = (state.font_size + 1).min(FONT_SIZE_MAX);
        return SettingsAction::None;
    }
    if rect_contains(layout.live_toggle_rect, x, y) {
        state.live_enabled = !state.live_enabled;
        state.focused = focus_index(Zone::LiveToggle);
        return SettingsAction::None;
    }
    if rect_contains(layout.apply_rect, x, y) {
        return SettingsAction::Apply;
    }
    if rect_contains(layout.clear_rect, x, y) {
        return SettingsAction::ClearCache;
    }
    if rect_contains(layout.cancel_rect, x, y) {
        return SettingsAction::Cancel;
    }
    // NOTE: clicks outside `layout.area` (e.g. on the menu bar above) are NOT
    // cancels — Settings is a top-level VIEW, not a modal.  The caller is
    // expected to filter those out before invoking handle_click.
    SettingsAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_index_and_zone_of_are_inverse() {
        for i in 0..total_focusable() {
            assert!(
                focus_index(zone_of(i)) == i,
                "focus {i} does not round-trip"
            );
        }
        // The Buttons row is the last focus slot, not the live toggle (W7).
        assert!(zone_of(total_focusable() - 1) == Zone::Buttons);
        assert!(focus_index(Zone::LiveToggle) + 1 == focus_index(Zone::Buttons));
    }
}
