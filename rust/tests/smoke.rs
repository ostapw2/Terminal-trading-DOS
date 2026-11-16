//! Smoke tests for the dos runtime — render every view via TestBackend,
//! check core invariants (no panics, sorted screener, dashboard state advances,
//! chart slicing, settings constants).

use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};

use dos::{
    markets::{market_symbols, MarketType, Ohlc, Timeframe},
    tokens::CLASSIC,
    widgets::{
        chart, dashboard, markets, modal, screener,
        settings::{self, MARKETS as SETTINGS_MARKETS},
    },
};

// ───────────────────────────  helpers  ───────────────────────────

/// A small set of test OHLC candles for chart/dashboard tests.
fn test_candles() -> Vec<Ohlc> {
    vec![
        Ohlc {
            open: 100.0,
            high: 105.0,
            low: 99.0,
            close: 104.0,
            volume: 1000.0,
            date: "2024-01-01".into(),
            time_ms: 1704067200000,
            is_gap: false,
        },
        Ohlc {
            open: 104.0,
            high: 108.0,
            low: 103.0,
            close: 107.0,
            volume: 1200.0,
            date: "2024-01-02".into(),
            time_ms: 1704153600000,
            is_gap: false,
        },
        Ohlc {
            open: 107.0,
            high: 110.0,
            low: 106.0,
            close: 109.0,
            volume: 900.0,
            date: "2024-01-03".into(),
            time_ms: 1704240000000,
            is_gap: false,
        },
        Ohlc {
            open: 109.0,
            high: 112.0,
            low: 108.0,
            close: 111.0,
            volume: 1100.0,
            date: "2024-01-04".into(),
            time_ms: 1704326400000,
            is_gap: false,
        },
        Ohlc {
            open: 111.0,
            high: 115.0,
            low: 110.0,
            close: 114.0,
            volume: 1300.0,
            date: "2024-01-05".into(),
            time_ms: 1704412800000,
            is_gap: false,
        },
        Ohlc {
            open: 114.0,
            high: 116.0,
            low: 112.0,
            close: 113.0,
            volume: 800.0,
            date: "2024-01-06".into(),
            time_ms: 1704499200000,
            is_gap: false,
        },
        Ohlc {
            open: 113.0,
            high: 117.0,
            low: 112.0,
            close: 116.0,
            volume: 1500.0,
            date: "2024-01-07".into(),
            time_ms: 1704585600000,
            is_gap: false,
        },
        Ohlc {
            open: 116.0,
            high: 120.0,
            low: 115.0,
            close: 119.0,
            volume: 2000.0,
            date: "2024-01-08".into(),
            time_ms: 1704672000000,
            is_gap: false,
        },
        Ohlc {
            open: 119.0,
            high: 121.0,
            low: 117.0,
            close: 118.0,
            volume: 1100.0,
            date: "2024-01-09".into(),
            time_ms: 1704758400000,
            is_gap: false,
        },
        Ohlc {
            open: 118.0,
            high: 122.0,
            low: 117.0,
            close: 121.0,
            volume: 1800.0,
            date: "2024-01-10".into(),
            time_ms: 1704844800000,
            is_gap: false,
        },
    ]
}

fn test_symbols() -> Vec<dos::markets::Symbol> {
    let data = test_candles();
    vec![
        dos::markets::Symbol {
            ticker: "TEST",
            name: "Test Asset",
            data: data.clone(),
        },
        dos::markets::Symbol {
            ticker: "DEMO",
            name: "Demo Asset",
            data,
        },
    ]
}

fn term(w: u16, h: u16) -> Terminal<TestBackend> {
    // The checks read English text; the shipped default is Ukrainian.
    dos::i18n::set(dos::i18n::Lang::En);
    Terminal::new(TestBackend::new(w, h)).expect("backend")
}

fn render_markets(f: &mut ratatui::Frame, area: Rect, syms: &[dos::markets::Symbol], zoom: u16) {
    markets::render(
        f,
        area,
        syms[0].ticker,
        syms[0].name,
        &syms[0].data,
        &[],
        zoom,
        1,
        Timeframe::D1,
        dos::widgets::chart::ChartType::Candle,
        0,
        &markets::SeriesFilters::NONE,
        None,
        None,
        None,
        &CLASSIC,
    );
}

fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area;
    let mut s = String::with_capacity(((area.width as usize) + 1) * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s.push('\n');
    }
    s
}

// ───────────────────────────  market data  ───────────────────────────

#[test]
fn market_symbols_returns_ten_per_market() {
    for m in [
        MarketType::UsStocks,
        MarketType::Crypto,
        MarketType::EuStocks,
        MarketType::Forex,
        MarketType::Commodities,
    ] {
        let v = market_symbols(m);
        assert_eq!(v.len(), 10, "market {:?} should have 10 symbols", m);
        assert!(!v[0].ticker.is_empty(), "first symbol has empty ticker");
    }
}

#[test]
fn market_type_names_match_settings_const() {
    let constants: Vec<MarketType> = SETTINGS_MARKETS.iter().map(|(_, m)| *m).collect();
    assert_eq!(
        constants,
        vec![
            MarketType::UsStocks,
            MarketType::Crypto,
            MarketType::EuStocks,
            MarketType::Forex,
            MarketType::Commodities
        ]
    );
    // No name collision.
    let names: Vec<&str> = constants.iter().map(|m| m.name()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(sorted, deduped, "market names must be unique");
}

// ───────────────────────────  chart helpers  ───────────────────────────

#[test]
fn chart_visible_slice_returns_last_n_at_zoom_1() {
    let data = test_candles();
    let visible = chart::visible_slice_at(&data, 5 + chart::RIGHT_MARGIN_CW, 1, 0);
    assert_eq!(visible.len(), 5);
    assert_eq!(
        visible.last().unwrap().date,
        data.last().unwrap().date,
        "must include most-recent candle"
    );
}

#[test]
fn chart_visible_slice_scales_with_zoom() {
    let data = test_candles();
    let z1 = chart::visible_slice_at(&data, 10 + chart::RIGHT_MARGIN_CW, 1, 0).len();
    let z2 = chart::visible_slice_at(&data, 10 + chart::RIGHT_MARGIN_CW * 2, 2, 0).len();
    let z3 = chart::visible_slice_at(&data, 10 + chart::RIGHT_MARGIN_CW * 3, 3, 0).len();
    assert!(
        z1 > z2 && z2 >= z3,
        "zoom should reduce visible count: {} {} {}",
        z1,
        z2,
        z3
    );
}

#[test]
fn chart_visible_range_handles_empty() {
    let empty: Vec<Ohlc> = vec![];
    assert!(chart::visible_range(&empty).is_none());
    let gaps = vec![
        Ohlc {
            is_gap: true,
            ..Ohlc::default()
        };
        3
    ];
    assert!(
        chart::visible_range(&gaps).is_none(),
        "only gaps → nothing to scale"
    );
}

#[test]
fn chart_visible_range_bounds_visible_data() {
    let data = test_candles();
    let visible = chart::visible_slice_at(&data, 10 + chart::RIGHT_MARGIN_CW, 1, 0);
    let (hi, lo) = chart::visible_range(visible).expect("non-empty window");
    for o in visible {
        assert!(o.high <= hi + 1e-9, "candle high exceeds visible_range hi");
        assert!(o.low >= lo - 1e-9, "candle low  below visible_range lo");
    }
}

// ───────────────────────────  screener  ───────────────────────────

#[test]
fn screener_top_gainers_sorted_descending() {
    let syms = test_symbols();
    let g = screener::top_gainers_sorted(&syms, 10, screener::SortMode::Gap, false);
    // top_gainers returns Vec<(usize, f64)> where usize is the symbol index.
    for w in g.windows(2) {
        assert!(w[0].1 >= w[1].1, "top_gainers must be sorted descending");
    }
}

#[test]
fn screener_renders_without_panic() {
    let mut t = term(140, 30);
    let syms = test_symbols();
    t.draw(|f| {
        screener::render(
            f,
            Rect::new(0, 0, 140, 30),
            &syms,
            0,
            screener::SortMode::Gap,
            &CLASSIC,
            false,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    // Header row should appear.
    assert!(s.contains("TICKER"), "screener header missing");
    // Default sort = Gap, header column shows "GAP%".
    assert!(s.contains("GAP%"), "screener GAP% header missing");
    // At least one ticker should be visible.
    let some_ticker_visible = ["TEST", "DEMO"].iter().any(|t| s.contains(t));
    assert!(some_ticker_visible, "no test tickers in screener output");
}

// ───────────────────────────  markets view  ───────────────────────────

#[test]
fn markets_renders_for_each_market_type() {
    for m in [
        MarketType::UsStocks,
        MarketType::Crypto,
        MarketType::EuStocks,
        MarketType::Forex,
        MarketType::Commodities,
    ] {
        let mut t = term(120, 40);
        let mut syms = market_symbols(m);
        // Populate with test data so chart rendering doesn't panic on empty.
        let data = test_candles();
        for s in &mut syms {
            s.data = data.clone();
        }
        t.draw(|f| {
            render_markets(f, Rect::new(0, 0, 120, 40), &syms, 1);
        })
        .unwrap();
        let s = buffer_to_string(t.backend().buffer());
        let first_ticker = syms[0].ticker;
        assert!(
            s.contains(first_ticker),
            "markets/{:?}: ticker {} missing from buffer",
            m,
            first_ticker
        );
    }
}

#[test]
fn markets_zoom_changes_visible_slice() {
    let syms = test_symbols();
    let mut t1 = term(120, 40);
    t1.draw(|f| {
        render_markets(f, Rect::new(0, 0, 120, 40), &syms, 1);
    })
    .unwrap();
    let mut t2 = term(120, 40);
    t2.draw(|f| {
        render_markets(f, Rect::new(0, 0, 120, 40), &syms, 3);
    })
    .unwrap();
    let s1 = buffer_to_string(t1.backend().buffer());
    let s2 = buffer_to_string(t2.backend().buffer());
    assert_ne!(s1, s2, "different zoom should produce different output");
}

// ───────────────────────────  dashboard  ───────────────────────────

#[test]
fn dashboard_engine_renders_without_panic() {
    let mut t = term(140, 40);
    let runtime = dos::engine::Runtime::new(10_000.0);
    t.draw(|f| {
        dashboard::render_engine(f, Rect::new(0, 0, 140, 40), &runtime, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("Trading Dashboard"), "dashboard title missing");
}

// ───────────────────────────  settings  ───────────────────────────

#[test]
fn settings_renders_without_panic() {
    let mut t = term(120, 30);
    let state = settings::SettingsState::new(MarketType::UsStocks, Timeframe::D1, "", "", false);
    t.draw(|f| {
        settings::render(
            f,
            Rect::new(0, 0, 120, 30),
            &state,
            MarketType::UsStocks,
            Timeframe::D1,
            "0 B",
            None,
            None,
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("Settings"));
    assert!(s.contains("Market type"));
    assert!(s.contains("US Stocks"));
    assert!(s.contains("Crypto"));
    assert!(s.contains("Apply"));
    assert!(s.contains("Cancel"));
}

#[test]
fn settings_state_initializes_to_current_market() {
    for m in [MarketType::UsStocks, MarketType::Crypto, MarketType::Forex] {
        let st = settings::SettingsState::new(m, Timeframe::D1, "", "", false);
        let expected = SETTINGS_MARKETS
            .iter()
            .position(|(_, mt)| *mt == m)
            .unwrap();
        assert_eq!(st.selected_market, expected);
        assert_eq!(st.focused, expected);
    }
}

// ───────────────────────────  modals  ───────────────────────────

#[test]
fn help_modal_renders() {
    let mut t = term(80, 24);
    t.draw(|f| {
        let modal = modal::Modal {
            title: "Help",
            body: &["Line one", "Line two"],
            buttons: &[modal::Button {
                label: "OK",
                primary: true,
            }],
            focused_button: 0,
            mouse: None,
            pressed_button: None,
        };
        modal::render(f, Rect::new(0, 0, 80, 24), &modal, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("Help"));
    assert!(s.contains("Line one"));
    assert!(s.contains("OK"));
}

// ───────────────────────────  tiny terminals (audit W5/W6/A3)  ───────────────────────────

const TINY: &[(u16, u16)] = &[(120, 40), (40, 10), (20, 5), (10, 3), (3, 2), (1, 1)];

#[test]
fn settings_survives_tiny_terminals() {
    for &(w, h) in TINY {
        let mut t = term(w, h);
        let state =
            settings::SettingsState::new(MarketType::UsStocks, Timeframe::D1, "", "", false);
        t.draw(|f| {
            settings::render(
                f,
                Rect::new(0, 0, w, h),
                &state,
                MarketType::UsStocks,
                Timeframe::D1,
                "0 B",
                Some((w / 2, h / 2)),
                None,
                &CLASSIC,
            );
        })
        .unwrap_or_else(|e| panic!("settings {w}x{h}: {e}"));
    }
}

#[test]
fn modal_and_help_survive_tiny_terminals() {
    use dos::widgets::help::{self, HelpPage, HelpSection};
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            let m = modal::Modal {
                title: "Confirm",
                body: &["Line one", "Line two"],
                buttons: &[
                    modal::Button {
                        label: "Yes",
                        primary: true,
                    },
                    modal::Button {
                        label: "No",
                        primary: false,
                    },
                ],
                focused_button: 0,
                mouse: Some((w / 2, h / 2)),
                pressed_button: None,
            };
            modal::render(f, Rect::new(0, 0, w, h), &m, &CLASSIC);
            // Non-ASCII comments must not be byte-sliced.
            let page = HelpPage {
                sections: vec![HelpSection {
                    title: Some("Keys".into()),
                    items: vec![(
                        "←/→".into(),
                        "switch".into(),
                        "Δ-aware → comment that is far longer than the column".into(),
                    )],
                }],
                page: 0,
                page_count: 1,
            };
            help::render(f, Rect::new(0, 0, w, h), &page, Some((1, 1)), &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("modal/help {w}x{h}: {e}"));
    }
}

#[test]
fn status_line_survives_tiny_terminals() {
    use dos::widgets::status_line;
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            status_line::render_extended(
                f,
                Rect::new(0, h.saturating_sub(1), w, 1),
                "A long status message  |  with a hint after the separator",
                true,
                true,
                Some(if w % 2 == 0 { 0.4 } else { 45.0 }),
                &CLASSIC,
            );
            status_line::render_extended(
                f,
                Rect::new(0, 0, w, 1),
                "no feed yet",
                false,
                false,
                Some(f64::INFINITY),
                &CLASSIC,
            );
        })
        .unwrap_or_else(|e| panic!("status {w}x{h}: {e}"));
    }
}

#[test]
fn markets_pane_handles_empty_and_scrolled_data() {
    let mut t = term(60, 20);
    let data = test_candles();
    t.draw(|f| {
        // Empty (fetch in flight) and scrolled far past the history: neither may panic.
        for (slice, offset) in [(&data[..0], 0), (&data[..1], 0), (&data[..], 10_000)] {
            markets::render_pane(
                f,
                Rect::new(0, 0, 60, 20),
                "BTCUSDT",
                slice,
                &[],
                &markets::SeriesFilters::NONE,
                offset,
                1,
                1,
                Timeframe::D1,
                dos::widgets::chart::ChartType::Candle,
                true,
                &CLASSIC,
            );
        }
    })
    .unwrap();
}

#[test]
fn engine_panels_survive_tiny_terminals() {
    use dos::widgets::{strategy_panel, trade_ticket};
    let runtime = dos::engine::Runtime::new(10_000.0);
    let syms = test_symbols();
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            let area = Rect::new(0, 0, w, h);
            trade_ticket::render(
                f,
                area,
                &runtime,
                &trade_ticket::TicketTarget {
                    label: "SINGLE",
                    count: 1,
                },
                1.0,
                100.0,
                trade_ticket::SizeMode::Qty,
                1.0,
                Some((1, 1)),
                None,
                &CLASSIC,
            );
            strategy_panel::render(f, area, &runtime, None, &Default::default(), &CLASSIC);
            dashboard::render_engine(f, area, &runtime, &CLASSIC);
            screener::render(f, area, &syms, 0, screener::SortMode::Gap, &CLASSIC, false);
        })
        .unwrap_or_else(|e| panic!("engine panels {w}x{h}: {e}"));
    }
}

#[test]
fn lists_survive_tiny_terminals_and_show_live_data() {
    use dos::{
        data::binance::TickerSummary,
        watchlists::{PriceTracker, Watchlists},
        widgets::lists,
    };
    let tracker = PriceTracker::default();
    let mut wl = Watchlists::default();
    wl.add_symbol("BTCUSDT");
    wl.add_symbol("NOPEUSDT");
    let pairs = vec![TickerSummary {
        symbol: "BTCUSDT".into(),
        last_price: 65_000.0,
        change_pct: 2.5,
        high_24h: 66_000.0,
        low_24h: 63_000.0,
        volume_base: 100.0,
        volume_quote: 6_500_000.0,
    }];
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            lists::render(
                f,
                Rect::new(0, 0, w, h),
                &wl,
                &pairs,
                &tracker,
                0,
                false,
                &CLASSIC,
            );
        })
        .unwrap_or_else(|e| panic!("lists {w}x{h}: {e}"));
    }
    wl.select_list(0);
    let mut t = term(100, 12);
    t.draw(|f| {
        lists::render(
            f,
            Rect::new(0, 0, 100, 12),
            &wl,
            &pairs,
            &tracker,
            0,
            false,
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("BTCUSDT") && s.contains("65000"),
        "live row missing"
    );
    assert!(s.contains("no data"), "unknown symbol must say so");
}

#[test]
fn toolbar_shows_keys_returns_hit_rects_and_survives_tiny_areas() {
    use dos::widgets::toolbar::{self, ToolButton};
    let buttons = vec![
        ToolButton::new("n", "New"),
        ToolButton::new("d", "Delete").enabled(false),
        ToolButton::new("Enter", "Chart"),
        ToolButton::new("", "Single").active(true),
    ];
    let mut t = term(60, 2);
    let mut rects = Vec::new();
    t.draw(|f| {
        rects = toolbar::render(
            f,
            Rect::new(0, 0, 60, 2),
            "List:",
            &buttons,
            Some((9, 0)),
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("List:") && s.contains("n New") && s.contains("Enter Chart"));
    assert!(rects[0].is_some() && rects[2].is_some() && rects[3].is_some());
    assert!(rects[1].is_none(), "disabled buttons are not clickable");
    for r in rects.iter().flatten() {
        assert!(r.x + r.width <= 60 && r.y < 2, "{r:?} outside the area");
    }
    // Narrow: wraps to the second row, never past the area.
    let mut t = term(24, 2);
    let mut rects = Vec::new();
    t.draw(|f| {
        rects = toolbar::render(f, Rect::new(0, 0, 24, 2), "List:", &buttons, None, &CLASSIC);
    })
    .unwrap();
    assert!(rects.iter().flatten().any(|r| r.y == 1), "second row used");
    for r in rects.iter().flatten() {
        assert!(r.x + r.width <= 24 && r.y < 2);
    }
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            toolbar::render(
                f,
                Rect::new(0, 0, w, h),
                "Pair:",
                &buttons,
                Some((1, 0)),
                &CLASSIC,
            );
        })
        .unwrap_or_else(|e| panic!("toolbar {w}x{h}: {e}"));
    }
}

#[test]
fn target_table_renders_summary_rows_and_survives_tiny_areas() {
    use dos::widgets::target_table::{self, TargetRow};
    let rows: Vec<TargetRow> = (0..30)
        .map(|i| TargetRow {
            symbol: format!("SYM{i}USDT"),
            last: 10.0 + i as f64,
            qty: if i % 2 == 0 { 1.5 } else { 0.0 },
            avg: 9.0,
            upnl: if i % 2 == 0 { 2.0 } else { 0.0 },
            side: if i == 0 { "L" } else { "" },
        })
        .collect();
    let mut t = term(70, 12);
    t.draw(|f| {
        target_table::render(
            f,
            Rect::new(0, 0, 70, 12),
            " Trading lists: Main ",
            &rows,
            0,
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("30 symbols") && s.contains("open 15"),
        "summary line"
    );
    assert!(s.contains("uPnL +30.00"), "summed PnL");
    assert!(
        s.contains("SYM0USDT") && !s.contains("SYM29USDT"),
        "scroll window"
    );
    // Offset past the end is clamped so the last rows stay visible.
    let mut t = term(70, 12);
    t.draw(|f| {
        target_table::render(f, Rect::new(0, 0, 70, 12), "x", &rows, 9_999, &CLASSIC);
    })
    .unwrap();
    assert!(buffer_to_string(t.backend().buffer()).contains("SYM29USDT"));
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            target_table::render(f, Rect::new(0, 0, w, h), "t", &rows, 3, &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("target table {w}x{h}: {e}"));
    }
}

#[test]
fn spread_chart_draws_header_axis_and_survives_tiny_areas() {
    use dos::{pairs::Stats, widgets::spread_chart};
    let dev: Vec<f64> = (0..120).map(|i| 3.0 * (i as f64 / 9.0).sin()).collect();
    let stats = Stats {
        n: 120,
        last: dev[119],
        sigma: 2.1,
        z: 2.4,
        corr: 0.93,
        beta: 1.07,
        min: -3.0,
        max: 3.0,
    };
    let mut t = term(90, 14);
    t.draw(|f| {
        spread_chart::render(f, Rect::new(0, 0, 90, 14), " A/B ", &dev, &stats, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("z +2.40") && s.contains("corr 0.93") && s.contains("beta 1.07"));
    assert!(s.contains("0%"), "zero label on the axis");
    assert!(s.contains('┄'), "sigma guides");
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            spread_chart::render(f, Rect::new(0, 0, w, h), "x", &dev, &stats, &CLASSIC);
            spread_chart::render(f, Rect::new(0, 0, w, h), "x", &[], &stats, &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("spread chart {w}x{h}: {e}"));
    }
}

#[test]
fn agents_screen_shows_state_roles_and_log_and_survives_tiny_areas() {
    use dos::{
        data::db::AgentLogRow,
        widgets::agents::{self, AgentsView, RoleRow},
    };
    let mk = |socket: Option<&'static str>, error: Option<&'static str>| AgentsView {
        socket,
        error,
        rows: vec![
            RoleRow {
                role: "analyst",
                tools: 1,
                calls: 3,
                denied: 0,
                last_call_ms: 1_000,
            },
            RoleRow {
                role: "scalper",
                tools: 1,
                calls: 2,
                denied: 2,
                last_call_ms: 0,
            },
        ],
        log: vec![
            AgentLogRow {
                ts: 5_000,
                role: "analyst".into(),
                tool: "whoami".into(),
                status: "ok".into(),
            },
            AgentLogRow {
                ts: 4_000,
                role: "scalper".into(),
                tool: "place_order".into(),
                status: "denied: forbidden".into(),
            },
        ],
        now_ms: 61_000,
        rejected: 4,
        mandate: vec!["#1 Scalp: active, 12 min left".to_string()],
        ledger: vec!["ledger: balance $1000.00  equity $990.00  PnL -10.00".to_string()],
        mandate_active: true,
        mandates: vec![],
    };
    let mut t = term(120, 24);
    t.draw(|f| {
        agents::render(
            f,
            Rect::new(0, 0, 120, 24),
            &mk(Some("/tmp/x/agent.sock"), None),
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("Connector ON") && s.contains("agent.sock"));
    assert!(s.contains("4 request(s) refused"), "failed auth is visible");
    assert!(s.contains("Scalper mandate") && s.contains("12 min left"));
    assert!(
        s.contains("balance $1000.00"),
        "the scalper's own ledger is shown"
    );
    assert!(s.contains("analyst") && s.contains("1m ago") && s.contains("never"));
    assert!(s.contains("place_order") && s.contains("denied"));
    let mut t = term(120, 24);
    t.draw(|f| {
        agents::render(f, Rect::new(0, 0, 120, 24), &mk(None, None), &CLASSIC);
    })
    .unwrap();
    assert!(
        buffer_to_string(t.backend().buffer()).contains("--agent"),
        "tells how to turn it on"
    );
    let mut t = term(120, 24);
    t.draw(|f| {
        agents::render(
            f,
            Rect::new(0, 0, 120, 24),
            &mk(None, Some("address in use")),
            &CLASSIC,
        );
    })
    .unwrap();
    assert!(buffer_to_string(t.backend().buffer()).contains("address in use"));
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            agents::render(f, Rect::new(0, 0, w, h), &mk(Some("s"), None), &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("agents {w}x{h}: {e}"));
    }
}

#[test]
fn lab_shows_setup_rows_result_lines_and_leaves_room_for_a_chart() {
    use dos::widgets::lab::{self, LabRow, LabView};
    let rows: Vec<LabRow> = (0..12)
        .map(|i| LabRow {
            label: format!("Param {i}"),
            value: format!("{i}"),
            note: String::new(),
        })
        .collect();
    let summary = vec![
        "+12.34 %  net +123.40".to_string(),
        "buy&hold +5.00 %".to_string(),
    ];
    let view = LabView {
        rows: &rows,
        selected: 3,
        summary: &summary,
        headline_pct: Some(12.34),
        status: "done",
        answer: None,
        about: "Buys when the fast average crosses above the slow one.",
    };
    let mut t = term(110, 30);
    let mut layout = lab::LabLayout::default();
    t.draw(|f| {
        layout = lab::render(f, Rect::new(0, 0, 110, 30), &view, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains("Param 3") && s.contains("+12.34 %") && s.contains("buy&hold"));
    assert!(s.contains(">"), "selection marker");
    assert_eq!(layout.rows.len(), 12);
    assert!(
        layout.chart.height >= 8,
        "room for the equity chart: {:?}",
        layout.chart
    );
    // A short window still scrolls to keep the selected row visible.
    let view = LabView {
        rows: &rows,
        selected: 11,
        ..view
    };
    let mut t = term(110, 8);
    t.draw(|f| {
        lab::render(f, Rect::new(0, 0, 110, 8), &view, &CLASSIC);
    })
    .unwrap();
    assert!(buffer_to_string(t.backend().buffer()).contains("Param 11"));
    // In plain words, with the equity chart in characters and in pixels.
    let answered = LabView {
        answer: Some((
            "How would sma_cross have done on BTCUSDT?",
            "Lost 0.55% (-$55.44), worse than simply holding BTCUSDT (+3.08%).",
            false,
            "Chart: your account against simply holding BTCUSDT.",
        )),
        ..view
    };
    let equity: Vec<f64> = (0..200)
        .map(|i| 1000.0 + (i as f64 * 0.1).sin() * 20.0)
        .collect();
    let prices: Vec<f64> = (0..200).map(|i| 100.0 + i as f64 * 0.05).collect();
    for px in [None, Some((10, 20))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(110, 40)]) {
            let mut t = term(w, h);
            t.draw(|f| {
                let l = lab::render(f, Rect::new(0, 0, w, h), &answered, &CLASSIC);
                lab::render_equity_chart(
                    f,
                    l.chart,
                    &equity,
                    &prices,
                    &[(3, true), (90, false), (500, true)],
                    "BTCUSDT",
                    &CLASSIC,
                );
            })
            .unwrap_or_else(|e| panic!("lab {w}x{h} {px:?}: {e}"));
        }
    }
    dos::gfx::set_cell_px(None);
    let mut t = term(110, 40);
    t.draw(|f| {
        lab::render(f, Rect::new(0, 0, 110, 40), &answered, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("Lost 0.55%") && s.contains("What it does"),
        "{s}"
    );
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            lab::render(f, Rect::new(0, 0, w, h), &view, &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("lab {w}x{h}: {e}"));
    }
}

#[test]
fn escape_sequences_in_displayed_text_never_reach_the_terminal() {
    use dos::widgets::fmt::{is_unsafe_char, sanitize_buffer};
    use ratatui::widgets::Paragraph;
    let evil = "A\x1b]0;PWNED\x07B\x1b[2JC\u{9b}31mD\u{202E}E";
    let mut t = term(60, 3);
    let mut fixed = 0;
    t.draw(|f| {
        f.render_widget(Paragraph::new(evil), Rect::new(0, 0, 60, 3));
        fixed = sanitize_buffer(f.buffer_mut());
    })
    .unwrap();
    assert!(fixed >= 4, "the unsafe cells were replaced: {fixed}");
    let b = t.backend().buffer();
    for y in 0..3 {
        for x in 0..60 {
            assert!(
                !b[(x, y)].symbol().chars().any(is_unsafe_char),
                "cell {x},{y} still unsafe"
            );
        }
    }
    let line: String = (0..60).map(|x| b[(x, 0)].symbol()).collect();
    assert!(
        line.contains('A') && line.contains('B') && line.contains('D'),
        "ordinary text is untouched: {line}"
    );
    // A buffer with nothing unsafe is left alone.
    let mut t = term(20, 1);
    t.draw(|f| {
        f.render_widget(Paragraph::new("plain привіт"), Rect::new(0, 0, 20, 1));
        assert_eq!(sanitize_buffer(f.buffer_mut()), 0);
    })
    .unwrap();
}

#[test]
fn markets_and_pane_survive_tiny_terminals() {
    let mut syms = test_symbols();
    for s in &mut syms {
        s.data = test_candles();
    }
    for &(w, h) in TINY {
        for chart_type in [
            chart::ChartType::Candle,
            chart::ChartType::Line,
            chart::ChartType::Bar,
            chart::ChartType::DeltaCluster,
        ] {
            let mut t = term(w, h);
            t.draw(|f| {
                let area = Rect::new(0, 0, w, h);
                markets::render(
                    f,
                    area,
                    syms[0].ticker,
                    syms[0].name,
                    &syms[0].data,
                    &[],
                    1,
                    1,
                    Timeframe::D1,
                    chart_type,
                    0,
                    &markets::SeriesFilters::NONE,
                    Some((w / 2, h / 2)),
                    None,
                    None,
                    &CLASSIC,
                );
                markets::render_pane(
                    f,
                    area,
                    syms[0].ticker,
                    &syms[0].data,
                    &[],
                    &markets::SeriesFilters::NONE,
                    0,
                    1,
                    1,
                    Timeframe::D1,
                    chart_type,
                    true,
                    &CLASSIC,
                );
            })
            .unwrap_or_else(|e| panic!("markets {w}x{h} {chart_type:?}: {e}"));
        }
    }
}

// ───────────────────────────  chart honesty (audit W1-W4, D3)  ───────────────────────────

fn candle(o: f64, h: f64, l: f64, c: f64, time_ms: i64) -> Ohlc {
    Ohlc {
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 1.0,
        date: "2026-01-01".into(),
        time_ms,
        is_gap: false,
    }
}

fn gap() -> Ohlc {
    Ohlc {
        is_gap: true,
        ..Ohlc::default()
    }
}

#[test]
fn aggregate_ignores_gap_sentinels() {
    let data = vec![
        candle(10.0, 12.0, 9.0, 11.0, 1),
        gap(),
        candle(11.0, 13.0, 10.0, 12.0, 3),
    ];
    let out = chart::aggregate(&data, 3);
    assert_eq!(out.len(), 1);
    let c = &out[0];
    assert!(!c.is_gap);
    assert_eq!((c.open, c.high, c.low, c.close), (10.0, 13.0, 9.0, 12.0));
    assert_ne!(c.low, 0.0, "a gap inside the chunk must not drag low to 0");
    // A chunk made only of gaps stays a gap.
    let only = chart::aggregate(&[gap(), gap()], 2);
    assert!(only[0].is_gap);
}

#[test]
fn line_chart_breaks_at_gaps_and_respects_candle_width() {
    let data = vec![
        candle(10.0, 10.0, 10.0, 10.0, 1),
        gap(),
        candle(20.0, 20.0, 20.0, 20.0, 3),
    ];
    let mut t = term(40, 10);
    t.draw(|f| {
        chart::render_line(f, Rect::new(0, 0, 30, 10), &data, &CLASSIC, 2);
    })
    .unwrap();
    let buf = t.backend().buffer();
    // Candle 0 → x 0..2, gap → x 2..4 stays blank, candle 2 → x 4..6.
    let col_has_ink = |x: u16| (0..10).any(|y| buf[(x, y)].symbol() != " ");
    assert!(
        col_has_ink(0) && col_has_ink(1),
        "first point drawn candle_w wide"
    );
    assert!(
        !col_has_ink(2) && !col_has_ink(3),
        "no line through the gap"
    );
    assert!(col_has_ink(4), "line resumes after the gap");
    assert!(
        !col_has_ink(10),
        "line is not stretched over the whole width"
    );
}

#[test]
fn one_price_mapping_for_rows_and_pixels() {
    for h in [1u16, 5, 24, 60] {
        for p in [0.0, 12.3, 50.0, 99.9, 100.0] {
            let px = chart::price_to_pixel(p, 100.0, 0.0, h);
            assert_eq!(chart::price_to_row(p, 100.0, 0.0, h) as u32, px / 8);
            assert!(px < h as u32 * 8);
        }
    }
}

#[test]
fn build_view_keeps_footprint_aligned_with_candles() {
    use dos::data::binance::AggTrade;
    use dos::data::footprint::{align_to_klines, build_from_trades};
    let klines: Vec<Ohlc> = (1..=6)
        .map(|i| candle(1.0, 1.0, 1.0, 1.0, i * 60_000))
        .collect();
    let trades: Vec<AggTrade> = (1..=6)
        .map(|i| AggTrade {
            price: 1.0,
            qty: i as f64,
            time_ms: i * 60_000,
            is_buyer_maker: false,
        })
        .collect();
    let aligned = align_to_klines(
        &klines,
        &build_from_trades(&trades, Timeframe::M1, 1.0),
        Timeframe::M1,
    );
    // Zoom-out ×2 and scrolled back by one: footprint must follow both.
    let view = markets::build_view(&klines, &aligned, &markets::SeriesFilters::NONE, 2, 1);
    assert_eq!(view.ohlc.len(), view.footprint.len());
    assert_eq!(view.full_len, 3);
    assert_eq!(view.ohlc.len(), 2);
    // Column 0 aggregates klines 1+2 → buy volume 1+2.
    assert!((view.footprint[0].total_buy - 3.0).abs() < 1e-9);
    assert!((view.footprint[1].total_buy - 7.0).abs() < 1e-9);
}

#[test]
fn chart_shift_adds_empty_columns_that_never_scale_the_chart() {
    let data = test_candles();
    let plain = markets::build_view(&data, &[], &markets::SeriesFilters::NONE, 1, 0);
    let f = markets::SeriesFilters {
        right_pad: 5,
        ..markets::SeriesFilters::NONE
    };
    let shifted = markets::build_view(&data, &[], &f, 1, 0);
    assert_eq!(shifted.ohlc.len(), plain.ohlc.len() + 5);
    assert!(shifted.ohlc[plain.ohlc.len()..].iter().all(|o| o.is_gap));
    assert_eq!(
        dos::widgets::chart::visible_range(&shifted.ohlc),
        dos::widgets::chart::visible_range(&plain.ohlc)
    );
    // It renders (pane and full chart) at any size.
    for &(w, h) in TINY.iter().chain(&[(120, 40)]) {
        let mut t = term(w, h);
        t.draw(|fr| {
            markets::render_pane(
                fr,
                Rect::new(0, 0, w, h),
                "BTC",
                &data,
                &[],
                &f,
                0,
                1,
                1,
                Timeframe::H4,
                dos::widgets::chart::ChartType::Candle,
                true,
                &CLASSIC,
            );
        })
        .unwrap();
    }
}

#[test]
fn a_compact_pane_draws_the_crosshair_wherever_the_mouse_is() {
    let data = test_candles();
    for &(w, h) in TINY.iter().chain(&[(120, 40)]) {
        for mouse in [
            (0, 0),
            (w / 2, h / 2),
            (w.saturating_sub(1), h.saturating_sub(1)),
            (500, 500),
        ] {
            let f = markets::SeriesFilters {
                hover: Some(mouse),
                ..markets::SeriesFilters::NONE
            };
            let mut t = term(w, h);
            t.draw(|fr| {
                markets::render_pane(
                    fr,
                    Rect::new(0, 0, w, h),
                    "BTC",
                    &data,
                    &[],
                    &f,
                    0,
                    1,
                    1,
                    Timeframe::H4,
                    dos::widgets::chart::ChartType::Candle,
                    true,
                    &CLASSIC,
                );
            })
            .unwrap();
        }
    }
}

#[test]
fn the_crosshair_shows_price_time_and_the_candle_in_text_and_pixels() {
    let syms = test_symbols();
    let draw = |f: &mut ratatui::Frame, area: Rect, hover| {
        markets::render(
            f,
            area,
            syms[0].ticker,
            syms[0].name,
            &syms[0].data,
            &[],
            2,
            1,
            Timeframe::H4,
            dos::widgets::chart::ChartType::Candle,
            0,
            &markets::SeriesFilters::NONE,
            hover,
            None,
            None,
            &CLASSIC,
        );
    };
    // Any size, any mouse spot: no panic.
    for &(w, h) in TINY.iter().chain(&[(60, 20), (120, 40)]) {
        for hover in [
            (0, 0),
            (w / 2, h / 2),
            (w.saturating_sub(1), h.saturating_sub(1)),
        ] {
            let mut t = term(w, h);
            t.draw(|f| draw(f, Rect::new(0, 0, w, h), Some(hover)))
                .unwrap_or_else(|e| panic!("crosshair {w}x{h} at {hover:?}: {e}"));
        }
    }
    // Characters: the data window and both axis labels.
    let mut t = term(120, 40);
    t.draw(|f| draw(f, Rect::new(0, 0, 120, 40), Some((8, 15))))
        .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains(" O ") && text.contains(" H ") && text.contains(" V "),
        "{text}"
    );
    assert!(text.contains('%') && text.contains("range"), "{text}");
    // Pixels: the plot is an image, the data window stays in cells on top.
    let mut t = term(120, 40);
    let mut jobs = Vec::new();
    t.draw(|f| {
        dos::gfx::set_cell_px(Some((10, 20)));
        draw(f, Rect::new(0, 0, 120, 40), Some((8, 15)));
        jobs = dos::gfx::finish_frame(f.buffer_mut());
        dos::gfx::set_cell_px(None);
    })
    .unwrap();
    assert!(!jobs.is_empty(), "the plot went to pixels");
    let text = buffer_to_string(t.backend().buffer());
    assert!(text.contains(" O ") && text.contains("range"), "{text}");
}

#[test]
fn candle_times_read_as_dates_or_clock_times() {
    let mut c = test_candles()[0].clone();
    c.time_ms = 1_791_633_600_000; // 2026-10-10 12:00 UTC
    assert_eq!(markets::candle_time_label(&c, Timeframe::H1), "10-10 12:00");
    assert_eq!(markets::candle_time_label(&c, Timeframe::D1), "2026-10-10");
    assert_eq!(markets::candle_time_label(&c, Timeframe::W1), "2026-10-10");
}

#[test]
fn compare_page_draws_the_curve_and_the_heat_map_at_any_size() {
    use dos::compare::{corr_matrix, lead_lag, Basis};
    use dos::widgets::compare;
    let a: Vec<f64> = (0..300)
        .map(|i| 100.0 + (i as f64 * 0.37).sin() * 3.0 + i as f64 * 0.01)
        .collect();
    let mut b = vec![100.0; 2];
    b.extend_from_slice(&a[..298]);
    let ll = lead_lag(&a, &b, Basis::Changes, 12);
    let candles = |v: &[f64]| -> Vec<Ohlc> {
        v.iter()
            .enumerate()
            .map(|(i, &c)| Ohlc {
                open: c,
                high: c,
                low: c,
                close: c,
                time_ms: 1_700_000_000_000 + i as i64 * 60_000,
                ..Default::default()
            })
            .collect()
    };
    let m = corr_matrix(
        &[
            ("BTCUSDT".into(), candles(&a)),
            ("ETHUSDT".into(), candles(&b)),
            ("SOLUSDT".into(), candles(&a[..200])),
        ],
        Basis::Changes,
    );
    // Characters and pixels, every size: nothing panics.
    for px in [None, Some((10, 20))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(60, 16), (120, 40)]) {
            let mut t = term(w, h);
            t.draw(|f| {
                let r = Rect::new(0, 0, w, h);
                compare::render_leadlag(f, r, &ll, "BTC", "ETH", Some((&a, &b)), false, &CLASSIC);
                compare::render_leadlag(f, r, &ll, "BTC", "ETH", Some((&a, &b)), true, &CLASSIC);
                compare::render_leadlag(f, r, &ll, "BTC", "ETH", None, false, &CLASSIC);
                compare::render_heatmap(f, r, &m, &CLASSIC);
                compare::render_heat_page(f, r, &m, "Majors", &CLASSIC);
            })
            .unwrap_or_else(|e| panic!("compare {w}x{h} {px:?}: {e}"));
        }
    }
    dos::gfx::set_cell_px(None);
    let mut t = term(120, 30);
    t.draw(|f| {
        compare::render_leadlag(
            f,
            Rect::new(0, 0, 120, 30),
            &ll,
            "BTC",
            "ETH",
            Some((&a, &b)),
            false,
            &CLASSIC,
        )
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("BTC leads: ETH follows 2 candle(s) later"),
        "{text}"
    );
    assert!(text.contains("-12") && text.contains("+12"), "{text}");
    assert!(text.contains("Who moves first, BTC or ETH?"), "{text}");
    let mut t = term(80, 12);
    t.draw(|f| compare::render_heatmap(f, Rect::new(0, 0, 80, 12), &m, &CLASSIC))
        .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("BTC") && text.contains("+1.00") && text.contains("cyan"),
        "{text}"
    );
}

#[test]
fn the_judge_report_renders_at_any_size_and_says_its_verdict() {
    use dos::widgets::compare;
    let target: Vec<f64> = (0..1500)
        .map(|i| 100.0 + (i as f64 * 0.21).sin() * 4.0)
        .collect();
    let cond: Vec<f64> = (0..1500)
        .map(|i| if i % 37 == 0 { 1.0 } else { 0.0 })
        .collect();
    let v = dos::judge::judge(&cond, &target, 4, 0.7, 100, 3).unwrap();
    for px in [None, Some((10, 20))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(70, 20), (140, 40)]) {
            let mut t = term(w, h);
            t.draw(|f| {
                let r = Rect::new(0, 0, w, h);
                compare::render_verdict(f, r, &v, "X > 1", "BTC", "1h", &CLASSIC);
                compare::render_judge(f, r, &v, "X > 1", "BTC", "1h", &CLASSIC);
            })
            .unwrap_or_else(|e| panic!("verdict {w}x{h} {px:?}: {e}"));
        }
    }
    dos::gfx::set_cell_px(None);
    assert!(
        !v.moves.is_empty() && v.usual.len() > 1000,
        "distributions kept"
    );
    let mut t = term(140, 40);
    t.draw(|f| {
        compare::render_judge(
            f,
            Rect::new(0, 0, 140, 40),
            &v,
            "X > 1",
            "BTC",
            "1h",
            &CLASSIC,
        )
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("After X > 1")
            && text.contains("after the event")
            && text.contains("P(higher)"),
        "{text}"
    );
    let mut t = term(140, 40);
    t.draw(|f| {
        compare::render_verdict(
            f,
            Rect::new(0, 0, 140, 40),
            &v,
            "X > 1",
            "BTC",
            "1h",
            &CLASSIC,
        )
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    let head = if v.survives { "HOLDS" } else { "NO EVIDENCE" };
    assert!(
        text.contains(head) && text.contains("P(higher)") && text.contains("/ 3 ="),
        "{text}"
    );
    assert!(
        text.contains("newer 30%") && text.contains("chance"),
        "{text}"
    );
}

#[test]
fn search_results_render_at_any_size_with_the_selection() {
    use dos::search::{Hit, Question, Report};
    use dos::widgets::compare;
    let target: Vec<f64> = (0..1500)
        .map(|i| 100.0 + (i as f64 * 0.21).sin() * 4.0)
        .collect();
    let cond: Vec<f64> = (0..1500)
        .map(|i| if i % 37 == 0 { 1.0 } else { 0.0 })
        .collect();
    let v = dos::judge::judge(&cond, &target, 4, 0.7, 50, 900).unwrap();
    let hit = |c: &str| Hit {
        q: Question {
            cond: c.into(),
            target: "BTCUSDT".into(),
            h: 4,
        },
        older_events: 30,
        screen_edge: 0.1,
        verdict: Some(v.clone()),
    };
    let r = Report {
        symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
        tested: 900,
        rankable: 120,
        hits: (0..30)
            .map(|i| hit(&format!("CHG(ETHUSDT,1) > {i}")))
            .collect(),
        survivors: 0,
        cores: 10,
        elapsed_ms: 321,
    };
    for &(w, h) in TINY.iter().chain(&[(80, 12), (150, 40)]) {
        for sel in [0, 29, 99] {
            let mut t = term(w, h);
            t.draw(|f| {
                compare::render_search(f, Rect::new(0, 0, w, h), &r, sel, &CLASSIC);
                compare::render_search_page(f, Rect::new(0, 0, w, h), &r, sel, "Majors", &CLASSIC)
            })
            .unwrap_or_else(|e| panic!("search {w}x{h} sel {sel}: {e}"));
        }
    }
    let mut t = term(150, 12);
    t.draw(|f| compare::render_search(f, Rect::new(0, 0, 150, 12), &r, 29, &CLASSIC))
        .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("of 900 questions") && text.contains("CHG(ETH,1) > 29"),
        "{text}"
    );
}

#[test]
fn the_landscape_renders_at_any_size_with_its_maps() {
    use dos::formula::{align, builtin_templates, Source};
    use dos::landscape::{run, Grid};
    use dos::markets::{Ohlc, Timeframe};
    use dos::widgets::compare;
    let candles: Vec<Ohlc> = (0..1500)
        .map(|i| {
            let c = 100.0 + (i as f64 * 0.21).sin() * 4.0 + (i as f64 * 0.013).cos();
            Ohlc {
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 10.0,
                time_ms: 1_699_999_200_000 + i as i64 * 3_600_000,
                ..Default::default()
            }
        })
        .collect();
    let s = Source::Spot("BTCUSDT".into());
    let data = std::collections::HashMap::from([(s.clone(), candles)]);
    let al = align(&[s], &data, Timeframe::H1).unwrap();
    let t = builtin_templates();
    let two = run(
        &al,
        &Grid::parse("Z(BTCUSDT,{10..200}) < -{1.0..2.0}").unwrap(),
        "BTCUSDT",
        4,
        &t,
        30,
        5,
    )
    .unwrap();
    let one = run(
        &al,
        &Grid::parse("CHG(BTCUSDT,1) < -{0.5..3.0}").unwrap(),
        "BTCUSDT",
        4,
        &t,
        30,
        5,
    )
    .unwrap();
    for l in [&two, &one] {
        for &(w, h) in TINY.iter().chain(&[(80, 20), (180, 30)]) {
            for cur in [(0, 0), (8, 8), (99, 99)] {
                let mut term_ = term(w, h);
                term_
                    .draw(|f| {
                        compare::render_landscape(f, Rect::new(0, 0, w, h), l, cur, "1h", &CLASSIC);
                        compare::render_landscape_page(
                            f,
                            Rect::new(0, 0, w, h),
                            l,
                            cur,
                            "1h",
                            &CLASSIC,
                        )
                    })
                    .unwrap_or_else(|e| panic!("landscape {w}x{h}: {e}"));
            }
        }
    }
    let mut term_ = term(180, 30);
    term_
        .draw(|f| {
            compare::render_landscape(f, Rect::new(0, 0, 180, 30), &two, (0, 0), "1h", &CLASSIC)
        })
        .unwrap();
    let text = buffer_to_string(term_.backend().buffer());
    assert!(
        text.contains("older 70%") && text.contains("newer 30%") && text.contains("chance: 95%"),
        "{text}"
    );
    assert!(
        text.contains("Z(BTCUSDT,{10..200})") && text.contains("Shape:"),
        "{text}"
    );
}

#[test]
fn the_scanner_page_renders_at_any_size() {
    use dos::scanner::{Knife, PairIdea, Scan, Stretch};
    use dos::widgets::compare;
    let s = Scan {
        pairs: vec![PairIdea {
            a: "ETHUSDT".into(),
            b: "BTCUSDT".into(),
            r: 0.86,
            beta: 1.1,
            z: 2.6,
            half_life: 14.0,
            t_older: -4.1,
            t_newer: -3.0,
            long: "BTCUSDT".into(),
            short: "ETHUSDT".into(),
            z_path: (0..500).map(|i| (i as f64 * 0.05).sin() * 2.0).collect(),
            history: dos::scanner::GapHistory {
                times: 5,
                closed: 4,
                median_candles: 9.0,
                horizon: 28,
            },
        }],
        up: vec![Stretch {
            symbol: "SOLUSDT".into(),
            z: 3.1,
            change_pct: 9.0,
        }],
        down: vec![],
        knives: vec![Knife {
            symbol: "XUSDT".into(),
            day_pct: -14.0,
            recent_pct: -3.0,
            volume_x: 2.4,
        }],
        coins: 150,
        pairs_tried: 11175,
        candles: 500,
        at_ms: 1_000_000,
        tf: "1h".into(),
        pumps: vec![dos::movers::Mover {
            symbol: "PEPEUSDT".into(),
            up: true,
            change_pct: 4.2,
            window_min: 5,
            volume_x: 3.1,
            price: 1.0,
            first_ms: 280_000,
            last_ms: 390_000,
        }],
        dumps: vec![],
    };
    for px in [None, Some((10, 20))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(80, 24), (160, 50)]) {
            for sel in [0, 5] {
                let mut t = term(w, h);
                t.draw(|f| {
                    compare::render_scanner_page(
                        f,
                        Rect::new(0, 0, w, h),
                        &s,
                        sel,
                        400_000,
                        "",
                        "t trades it",
                        &CLASSIC,
                    )
                })
                .unwrap_or_else(|e| panic!("scanner {w}x{h}: {e}"));
            }
        }
    }
    dos::gfx::set_cell_px(None);
    let mut t = term(160, 50);
    t.draw(|f| {
        compare::render_scanner_page(
            f,
            Rect::new(0, 0, 160, 50),
            &s,
            0,
            400_000,
            "",
            "t trades it",
            &CLASSIC,
        )
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("buy BTC")
            && text.contains("sell ETH")
            && text.contains("before: closed 4 of 5 in 28 candles (median 9)")
            && text.contains("Falling knives: X")
            && text.contains("Pumps: PEPE +4.2% in 5m vol x3.1 (2 min ago)")
            && text.contains("Dumps: none"),
        "{text}"
    );
}

#[test]
fn fmt_volume_keeps_coin_fractions() {
    use dos::markets::fmt_volume;
    assert_eq!(fmt_volume(523.0), "523");
    assert_eq!(fmt_volume(12.7), "12.70");
    assert_eq!(fmt_volume(1_500.0), "1.50K");
    assert_eq!(fmt_volume(2_500_000.0), "2.50M");
}

#[test]
fn reset_paper_wipes_the_ledger_but_keeps_slots() {
    use dos::engine::{Broker, Runtime};
    use dos::strategies::actions::Action;
    let mut rt = Runtime::new(10_000.0);
    rt.broker.submit(
        &Action::BuyMarket {
            symbol: "BTCUSDT".into(),
            qty: 0.1,
        },
        60_000.0,
        1,
    );
    assert!(rt.broker.all_positions().values().any(|p| !p.is_flat()));
    rt.reset_paper().unwrap();
    assert!(
        rt.broker.all_positions().values().all(|p| p.is_flat()),
        "positions gone after reset"
    );
    assert!(rt.fill_log.is_empty());
}

#[test]
fn view_cache_reuses_unchanged_series_and_misses_on_any_change() {
    use std::rc::Rc;
    let mut data = test_candles();
    let f = markets::SeriesFilters::NONE;
    let a = markets::build_view(&data, &[], &f, 1, 0);
    let b = markets::build_view(&data, &[], &f, 1, 0);
    assert!(Rc::ptr_eq(&a, &b), "unchanged input must hit the cache");
    // Different scroll, aggregation, or a revised candle in the MIDDLE: miss.
    assert!(!Rc::ptr_eq(&a, &markets::build_view(&data, &[], &f, 1, 2)));
    assert!(!Rc::ptr_eq(&a, &markets::build_view(&data, &[], &f, 2, 0)));
    data[3].close += 1.0;
    let c = markets::build_view(&data, &[], &f, 1, 0);
    assert!(
        !Rc::ptr_eq(&a, &c),
        "an edited middle candle must not be served stale"
    );
    assert_eq!(c.ohlc[3].close, data[3].close);
}

#[test]
fn rule_form_renders_every_field_and_survives_tiny_terminals() {
    use dos::widgets::rule_form::{self, RuleForm};
    let form = RuleForm::new(None);
    for &(w, h) in TINY.iter().chain(&[(60, 16), (80, 24)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            rule_form::render(
                f,
                Rect::new(0, 0, w, h),
                &form,
                Some((w / 2, h / 2)),
                &CLASSIC,
            )
        })
        .unwrap_or_else(|e| panic!("rule form {w}x{h}: {e}"));
    }
    let mut t = term(80, 24);
    t.draw(|f| rule_form::render(f, Rect::new(0, 0, 80, 24), &form, None, &CLASSIC))
        .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    for label in [
        "Name",
        "Window, min",
        "Move, %",
        "Direction",
        "Hold, min",
        "Min volume",
        "Side",
    ] {
        assert!(text.contains(label), "missing field label {label}:\n{text}");
    }
    assert!(text.contains("Create") && text.contains("Cancel"), "{text}");
    assert!(
        form.layout.get().ok.width > 0,
        "layout recorded for hit-testing"
    );
}

#[test]
fn link_chip_and_picker_render_and_survive_tiny_terminals() {
    use dos::widgets::link_chip;
    for &(w, h) in TINY.iter().chain(&[(60, 16), (120, 40)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            let area = Rect::new(0, 0, w, h);
            let bar = Rect::new(0, 0, w, 1.min(h));
            link_chip::render_chip(f, bar, 2, 2, Some((w / 2, 0)), &CLASSIC);
            let l = link_chip::render_picker(f, area, 2, 3, 2, Some((w / 2, h / 2)), &CLASSIC);
            let _ = l.cell_at(w / 2, h / 2);
        })
        .unwrap_or_else(|e| panic!("link chip {w}x{h}: {e}"));
    }
    let mut t = term(120, 40);
    let mut layout = None;
    t.draw(|f| {
        layout = Some(link_chip::render_picker(
            f,
            Rect::new(0, 0, 120, 40),
            3,
            3,
            1,
            None,
            &CLASSIC,
        ));
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("Link group") && text.contains("Off") && text.contains("other window"),
        "{text}"
    );
    let l = layout.unwrap();
    for g in 0..link_chip::CELLS {
        let r = l.cells[g];
        assert!(r.width > 0, "cell {g} drawn");
        assert_eq!(
            l.cell_at(r.x, r.y),
            Some(g),
            "click on cell {g} resolves to it"
        );
    }
}

#[test]
fn window_picker_renders_survives_tiny_terminals_and_hits_its_rows() {
    use dos::widgets::win_picker;
    for &(w, h) in TINY.iter().chain(&[(40, 12), (120, 40)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            let area = Rect::new(0, 0, w, h);
            let bar = Rect::new(0, 0, w, 1.min(h));
            let chip = win_picker::chip_rect(bar, Rect::default());
            win_picker::render_chip(f, chip, Some((w / 2, 0)), &CLASSIC);
            let l =
                win_picker::render_picker(f, area, 1, Some("Day"), Some((w / 2, h / 2)), &CLASSIC);
            let _ = l.row_at(w / 2, h / 2);
        })
        .unwrap_or_else(|e| panic!("win picker {w}x{h}: {e}"));
    }
    let mut t = term(120, 40);
    let mut layout = None;
    t.draw(|f| {
        layout = Some(win_picker::render_picker(
            f,
            Rect::new(0, 0, 120, 40),
            0,
            Some("Day"),
            None,
            &CLASSIC,
        ));
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("Desk Day")
            && text.contains("+ book")
            && text.contains("Workspace: Arbitrage")
            && text.contains("Close this window"),
        "{text}"
    );
    let l = layout.unwrap();
    for i in 0..win_picker::ROWS {
        let r = l.rows[i];
        assert!(r.width > 0, "row {i} drawn");
        assert_eq!(l.row_at(r.x + 2, r.y), Some(i), "click on row {i}");
    }
    // The chip sits left of the link chip, or at the edge without one.
    let bar = Rect::new(0, 0, 100, 1);
    let link = Rect::new(80, 0, 10, 1);
    assert_eq!(win_picker::chip_rect(bar, link).x + 3, 79);
    assert_eq!(win_picker::chip_rect(bar, Rect::default()).x + 3, 99);
    assert_eq!(win_picker::chip_rect(Rect::new(0, 0, 10, 1), link).width, 0);
}

#[test]
fn charts_draw_in_pixels_when_the_terminal_shows_images_and_keep_text_labels() {
    let data: Vec<Ohlc> = (0..120)
        .map(|i| {
            let c = 100.0 + (i as f64 * 0.3).sin() * 5.0 + i as f64 * 0.1;
            Ohlc {
                open: c - 0.5,
                high: c + 1.0,
                low: c - 1.5,
                close: c,
                volume: 1000.0 + i as f64,
                date: String::new(),
                time_ms: 1_700_000_000_000 + i * 86_400_000,
                is_gap: false,
            }
        })
        .collect();
    for kind in [
        chart::ChartType::Candle,
        chart::ChartType::Line,
        chart::ChartType::Bar,
    ] {
        let mut t = term(90, 30);
        let mut jobs = Vec::new();
        let mut skipped = 0;
        t.draw(|f| {
            dos::gfx::set_cell_px(Some((10, 20)));
            markets::render_pane(
                f,
                Rect::new(0, 0, 90, 30),
                "BTCUSDT",
                &data,
                &[],
                &markets::SeriesFilters::NONE,
                0,
                1,
                1,
                Timeframe::D1,
                kind,
                true,
                &CLASSIC,
            );
            jobs = dos::gfx::finish_frame(f.buffer_mut());
            skipped = f.buffer_mut().content().iter().filter(|c| c.skip).count();
            dos::gfx::set_cell_px(None);
        })
        .unwrap();
        assert!(jobs.len() >= 2, "{kind:?}: plot and volume images");
        for j in &jobs {
            assert_eq!(
                j.canvas.w,
                j.rect.width as usize * 10,
                "pixel width = cells x 10"
            );
            assert_eq!(j.canvas.h, j.rect.height as usize * 20);
        }
        // Skipped cells never reach the backend: the image covers them.
        let buf = t.backend().buffer().clone();
        assert!(skipped > 500, "{kind:?}: {skipped} cells left to the image");
        let text = buffer_to_string(&buf);
        assert!(
            text.contains("BTCUSDT") && text.contains("Volume"),
            "labels stay text"
        );
        assert!(
            !text.contains('\u{10FFFD}'),
            "no sentinel reaches the screen"
        );
    }
    // Off again: the same pane is characters only.
    let mut t = term(90, 30);
    t.draw(|f| {
        markets::render_pane(
            f,
            Rect::new(0, 0, 90, 30),
            "BTCUSDT",
            &data,
            &[],
            &markets::SeriesFilters::NONE,
            0,
            1,
            1,
            Timeframe::D1,
            chart::ChartType::Candle,
            true,
            &CLASSIC,
        );
        assert!(dos::gfx::finish_frame(f.buffer_mut()).is_empty());
        assert!(f.buffer_mut().content().iter().all(|c| !c.skip));
    })
    .unwrap();
}

#[test]
fn the_chart_says_live_only_while_ticks_arrive() {
    let syms = market_symbols(MarketType::Crypto);
    let mut data = test_candles();
    for _ in 0..3 {
        data.extend(test_candles());
    }
    for (i, c) in data.iter_mut().enumerate() {
        c.time_ms = 1_704_067_200_000 + i as i64 * 86_400_000;
    }
    let header = |age: Option<f64>, offset: usize| {
        let mut t = term(140, 30);
        t.draw(|f| {
            markets::render(
                f,
                Rect::new(0, 0, 140, 30),
                syms[0].ticker,
                syms[0].name,
                &data,
                &[],
                1,
                1,
                Timeframe::D1,
                chart::ChartType::Candle,
                offset,
                &markets::SeriesFilters::NONE,
                None,
                age,
                None,
                &CLASSIC,
            );
        })
        .unwrap();
        buffer_to_string(t.backend().buffer())
    };
    assert!(header(Some(1.0), 0).contains("LIVE"));
    let stale = header(Some(42.0), 0);
    assert!(
        stale.contains("STALE 42s") && !stale.contains("|  LIVE"),
        "{stale}"
    );
    assert!(header(None, 0).contains("NO TICKS"));
    assert!(header(Some(f64::INFINITY), 0).contains("NO TICKS"));
    assert!(header(Some(1.0), 2).contains("scrolled -2/"));
}

#[test]
fn tape_and_watch_pages_survive_tiny_areas_and_show_their_data() {
    use dos::{
        data::binance::{AggTrade, TickerSummary},
        watchlists::{PriceTracker, Watchlists},
        widgets::{lists, orderbook},
    };
    use std::collections::VecDeque;
    let trades: VecDeque<AggTrade> = (0..40)
        .map(|i| AggTrade {
            price: 65_000.0 + i as f64,
            qty: 0.01 * (i % 7 + 1) as f64,
            time_ms: 1_700_000_000_000 + i * 500,
            is_buyer_maker: i % 3 == 0,
        })
        .collect();
    let empty = VecDeque::new();
    let tracker = PriceTracker::default();
    let mut wl = Watchlists::default();
    wl.add_symbol("BTCUSDT");
    let pairs = vec![TickerSummary {
        symbol: "BTCUSDT".into(),
        last_price: 65_000.0,
        change_pct: 2.5,
        high_24h: 66_000.0,
        low_24h: 63_000.0,
        volume_base: 100.0,
        volume_quote: 6_500_000.0,
    }];
    for &(w, h) in TINY {
        let mut t = term(w, h);
        t.draw(|f| {
            let a = Rect::new(0, 0, w, h);
            orderbook::render_tape(f, a, "BTCUSDT", &trades, &CLASSIC);
            orderbook::render_tape(f, a, "BTCUSDT", &empty, &CLASSIC);
            lists::render_watch(f, a, &wl, &pairs, &tracker, 0, &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("tape / watch {w}x{h}: {e}"));
    }
    let mut t = term(100, 30);
    t.draw(|f| {
        orderbook::render_tape(f, Rect::new(0, 0, 100, 30), "BTCUSDT", &trades, &CLASSIC);
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("Time & Sales - BTCUSDT") && s.contains("60s  buys"),
        "{s}"
    );
    assert!(s.contains("65039"), "newest trade on top");
    let mut t = term(60, 8);
    t.draw(|f| {
        lists::render_watch(
            f,
            Rect::new(0, 0, 60, 8),
            &wl,
            &pairs,
            &tracker,
            0,
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("BTCUSDT") && !s.contains(" Lists "),
        "symbols only: {s}"
    );
}

#[test]
fn liquidations_draw_on_the_chart_and_the_page_at_any_size() {
    use dos::data::liquidations::{LiqStore, Liquidation};
    let data = test_candles();
    let mut store = LiqStore::default();
    for (i, c) in data.iter().enumerate() {
        store.push(
            Liquidation {
                symbol: "BTCUSDT".into(),
                long: i % 2 == 0,
                price: (c.high + c.low) / 2.0,
                qty: 10.0 * (i + 1) as f64,
                time_ms: c.time_ms + 1_000,
            },
            false,
        );
    }
    let f = markets::SeriesFilters {
        liqs: store.of("BTCUSDT"),
        ..markets::SeriesFilters::NONE
    };
    for px in [None, Some((8, 16))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(120, 40)]) {
            let mut t = term(w, h);
            t.draw(|fr| {
                markets::render_pane(
                    fr,
                    Rect::new(0, 0, w, h),
                    "BTC",
                    &data,
                    &[],
                    &f,
                    0,
                    3,
                    1,
                    Timeframe::D1,
                    chart::ChartType::Candle,
                    true,
                    &CLASSIC,
                );
            })
            .unwrap_or_else(|e| panic!("chart {w}x{h} {px:?}: {e}"));
        }
    }
    dos::gfx::set_cell_px(None);
    // In characters: the circles and the hour's legend are there.
    let mut t = term(120, 40);
    t.draw(|fr| {
        markets::render_pane(
            fr,
            Rect::new(0, 0, 120, 40),
            "BTC",
            &data,
            &[],
            &f,
            0,
            3,
            1,
            Timeframe::D1,
            chart::ChartType::Candle,
            true,
            &CLASSIC,
        );
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(s.contains('●') && s.contains("Liq 1h"), "{s}");

    let now = data.last().unwrap().time_ms + 60_000;
    for (only, min) in [(false, 0.0), (true, 0.0), (false, 1e9)] {
        for &(w, h) in TINY.iter().chain(&[(130, 40)]) {
            let mut t = term(w, h);
            let page = dos::widgets::liquidations::LiqPage {
                feed: store.feed(),
                symbol: "BTCUSDT",
                only_symbol: only,
                min_usd: min,
                connected: true,
                now_ms: now,
            };
            t.draw(|fr| {
                let l =
                    dos::widgets::liquidations::render(fr, Rect::new(0, 0, w, h), &page, &CLASSIC);
                assert!(l.rows.iter().all(|(r, _)| r.x + r.width <= w && r.y < h));
            })
            .unwrap_or_else(|e| panic!("liq page {w}x{h}: {e}"));
        }
    }
    let mut t = term(130, 40);
    let page = dos::widgets::liquidations::LiqPage {
        feed: store.feed(),
        symbol: "BTCUSDT",
        only_symbol: false,
        min_usd: 0.0,
        connected: true,
        now_ms: now,
    };
    t.draw(|fr| {
        let l = dos::widgets::liquidations::render(fr, Rect::new(0, 0, 130, 40), &page, &CLASSIC);
        assert_eq!(l.rows.len(), data.len() + 1, "every row and one coin");
    })
    .unwrap();
    let s = buffer_to_string(t.backend().buffer());
    assert!(
        s.contains("MOST LIQUIDATED") && s.contains("LONG") && s.contains("SHORT"),
        "{s}"
    );
}

#[test]
fn estimated_liquidation_levels_draw_behind_the_candles_at_any_size() {
    use dos::liqmap::{Level, LiqMap};
    let data = test_candles();
    let (hi, lo) = chart::visible_range(&data).unwrap();
    let mid = (hi + lo) / 2.0;
    let map = LiqMap {
        symbol: "BTCUSDT".into(),
        step: (hi - lo) / 50.0,
        levels: vec![
            Level {
                price: lo + (hi - lo) * 0.1,
                long_usd: 5e6,
                short_usd: 0.0,
            },
            Level {
                price: hi - (hi - lo) * 0.1,
                long_usd: 0.0,
                short_usd: 2e6,
            },
        ],
        price: mid,
        from_ms: 0,
        to_ms: 0,
    };
    let f = markets::SeriesFilters {
        liq_levels: Some(&map),
        ..markets::SeriesFilters::NONE
    };
    let draw = |w: u16, h: u16| {
        let mut t = term(w, h);
        t.draw(|fr| {
            markets::render_pane(
                fr,
                Rect::new(0, 0, w, h),
                "BTC",
                &data,
                &[],
                &f,
                0,
                3,
                1,
                Timeframe::D1,
                chart::ChartType::Candle,
                true,
                &CLASSIC,
            );
        })
        .unwrap_or_else(|e| panic!("levels {w}x{h}: {e}"));
        t
    };
    for px in [None, Some((8, 16))] {
        dos::gfx::set_cell_px(px);
        for &(w, h) in TINY.iter().chain(&[(120, 40)]) {
            draw(w, h);
        }
    }
    dos::gfx::set_cell_px(None);
    let t = draw(120, 40);
    let buf = t.backend().buffer();
    let bg_count =
        |c: ratatui::style::Color| buf.content().iter().filter(|cell| cell.bg == c).count();
    assert!(bg_count(chart::LEVEL_LONG) > 0, "long levels drawn");
    assert!(bg_count(chart::LEVEL_SHORT) > 0, "short levels drawn");
    let s = buffer_to_string(buf);
    assert!(s.contains("est. levels in view"), "{s}");
}

#[test]
fn the_live_ideas_page_draws_every_row_kind_at_any_size() {
    use dos::ideas::{from_live, plan, What};
    use dos::live_ideas::{Group, Laggard, Link, Live, Move};
    use dos::widgets::ideas::{render, Preview};
    let mv = |s: &str, pct: f64| Move {
        symbol: s.into(),
        window_min: 15,
        pct,
        volume_x: 3.2,
        price: 1.5,
        pct_5: Some(pct / 3.0),
        pct_15: Some(pct),
        pct_60: None,
    };
    let live = Live {
        at_ms: 1_700_000_000_000,
        coins: 300,
        corr_coins: 280,
        minutes: 45,
        gainers: vec![mv("AAAUSDT", 7.2), mv("BBBUSDT", 4.0)],
        losers: vec![mv("CCCUSDT", -5.5)],
        links: vec![Link {
            a: "AAAUSDT".into(),
            b: "BBBUSDT".into(),
            r: 0.91,
            minutes: 45,
            pct_a: 7.2,
            pct_b: 4.0,
        }],
        laggards: vec![Laggard {
            leader: "AAAUSDT".into(),
            follower: "DDDUSDT".into(),
            r: 0.8,
            minutes: 45,
            window_min: 15,
            lead_pct: 7.2,
            follow_pct: 1.1,
        }],
        groups: vec![Group {
            members: (0..12).map(|i| format!("G{i:02}USDT")).collect(),
            r_mean: 0.86,
            minutes: 45,
            pct: 2.4,
            z_hedge: Some(-2.1),
        }],
        ..Live::default()
    };
    let mut ideas = from_live(&live);
    assert_eq!(ideas.len(), 6);
    assert!(matches!(ideas[0].what, What::Laggard(_)), "laggards first");
    // A stretched pair and the options picture make rows too.
    let live_more = Live {
        spreads: vec![dos::live_ideas::Spread {
            a: "AAAUSDT".into(),
            b: "BBBUSDT".into(),
            r: 0.9,
            minutes: 45,
            z: 2.3,
            sd_pct: 0.3,
            pct_a: 7.2,
            pct_b: 4.0,
        }],
        ..live.clone()
    };
    let with_spread = from_live(&live_more);
    assert_eq!(with_spread.len(), 7);
    assert!(with_spread
        .iter()
        .any(|i| matches!(i.what, What::Spread(_)) && i.title() == "buy BBB / sell AAA"));
    ideas.push(
        with_spread
            .into_iter()
            .find(|i| matches!(i.what, What::Spread(_)))
            .unwrap(),
    );
    let chain = dos::data::options::demo_chain(1_792_300_000_000);
    let o = dos::options::findings(&chain.contracts, &chain.index, 1_792_300_000_000);
    let option_rows = dos::ideas::from_options(&o);
    assert!(option_rows.len() >= 2, "a parity and a box at least");
    assert!(option_rows
        .iter()
        .any(|i| matches!(i.what, What::Parity(_))));
    assert!(option_rows.iter().any(|i| matches!(i.what, What::Box(_))));
    for i in &option_rows {
        let p = plan(i, &[], 100.0);
        assert!(
            !p.headline.is_empty() && p.steps.len() >= 3,
            "{}",
            p.headline
        );
        assert!(i.chart().0.starts_with('='));
    }
    ideas.extend(option_rows.into_iter().take(2));
    // Every row has a title, facts, a chart formula and a plan on candles.
    let candles = test_candles();
    for idea in &ideas {
        assert!(!idea.title().is_empty() && !idea.facts().is_empty());
        let (f, tf) = idea.chart();
        assert!(f.starts_with('=') && (tf == "1m" || tf == "1h"), "{f} {tf}");
        let p = plan(idea, &candles, 100.0);
        assert!(!p.headline.is_empty() && !p.steps.is_empty());
    }
    let group_chart = ideas
        .iter()
        .find(|i| matches!(i.what, What::Group(_)))
        .unwrap()
        .chart()
        .0;
    assert_eq!(
        group_chart.matches("IDX(").count(),
        8,
        "eight sources at most: {group_chart}"
    );
    assert!(
        ideas.iter().any(|i| i.test().is_some()),
        "laggards and moves can be judged"
    );
    let p = plan(&ideas[0], &candles, 100.0);
    for &(w, h) in TINY.iter().chain(&[(80, 20), (200, 50)]) {
        for sel in [0, 3, 5, 99] {
            let mut t = term(w, h);
            t.draw(|f| {
                let preview = Preview {
                    candles: Some(&candles),
                    note: "",
                    plan: Some(&p),
                };
                let layout = render(
                    f,
                    Rect::new(0, 0, w, h),
                    "What is the market doing?",
                    "AAA leads",
                    "Nothing yet",
                    "How it was chosen",
                    &ideas,
                    sel,
                    &preview,
                    Some((1, 1)),
                    &CLASSIC,
                );
                assert!(layout.rows.len() <= ideas.len());
            })
            .unwrap_or_else(|e| panic!("live ideas {w}x{h} sel {sel}: {e}"));
        }
    }
}

#[test]
fn the_baskets_block_draws_every_state_at_any_size() {
    use dos::basket::{Basket, Entry, State, Why, DEFAULT_EXIT};
    use dos::engine::Runtime;
    use dos::widgets::baskets;
    let mut rt = Runtime::new(10_000.0);
    rt.on_price("AUSDT", 10.0, 1);
    rt.on_price("BUSDT", 20.0, 1);
    let legs = vec![
        ("AUSDT".to_string(), true, 100.0),
        ("BUSDT".to_string(), false, 100.0),
    ];
    // One basket per state, straight into the book.
    let states = [
        State::Entering {
            since_ms: 0,
            market_after_ms: Some(90_000),
        },
        State::Open { since_ms: 0 },
        State::Exiting {
            since_ms: 0,
            why: Why::Target,
            market_after_ms: None,
        },
        State::Closed {
            at_ms: 0,
            why: Why::Stop,
        },
    ];
    for (i, st) in states.iter().enumerate() {
        let mut b = Basket::new(
            i as u64 + 1,
            "buy A / sell B",
            "test",
            &legs,
            Entry::default(),
            DEFAULT_EXIT,
            0,
        )
        .unwrap();
        b.size(&rt.last_marks).unwrap();
        b.state = *st;
        if i == 1 {
            b.blocked = Some("kill switch".into());
        }
        rt.baskets.baskets.push(b);
    }
    assert_eq!(baskets::rows_wanted(&rt.baskets, 8), 5);
    assert_eq!(baskets::rows_wanted(&rt.baskets, 3), 3);
    for &(w, h) in TINY.iter().chain(&[(100, 12), (200, 60)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            let area = Rect::new(0, 0, w, h);
            let hits = baskets::render(f, area, &rt.baskets, &rt.last_marks, 60_000, &CLASSIC);
            assert!(hits.len() <= 4);
            assert!(hits.iter().all(|(_, id)| (1..=4).contains(id)));
            dashboard::render_engine(f, area, &rt, &CLASSIC);
        })
        .unwrap_or_else(|e| panic!("baskets {w}x{h}: {e}"));
    }
    // Wide enough: the header names the open count and every row shows.
    let mut t = term(140, 12);
    t.draw(|f| {
        let hits = baskets::render(
            f,
            Rect::new(0, 0, 140, 12),
            &rt.baskets,
            &rt.last_marks,
            60_000,
            &CLASSIC,
        );
        assert_eq!(hits.len(), 4);
        // Open ones first (newest first), the closed one last.
        assert_eq!(hits.iter().map(|h| h.1).collect::<Vec<_>>(), [3, 2, 1, 4]);
    })
    .unwrap();
    let text: String = t
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("3 open"), "{text}");
    assert!(
        text.contains("BLOCKED") && text.contains("CLOSED stop"),
        "{text}"
    );
}

#[test]
fn the_book_ladder_reports_its_rows_and_marks_our_orders() {
    use dos::data::orderbook::OrderBookSnapshot;
    use dos::widgets::orderbook::{self, LoadDiag};
    let snap = OrderBookSnapshot {
        bids: (1..=15)
            .map(|i| (100.0 - i as f64 * 0.1, 1.0 + i as f64))
            .collect(),
        asks: (1..=15)
            .map(|i| (100.0 + i as f64 * 0.1, 1.0 + i as f64))
            .collect(),
    };
    let trades = std::collections::VecDeque::new();
    let orders = [(true, 99.5, 0.5), (false, 100.3, 0.2)];
    for &(w, h) in TINY.iter().chain(&[(60, 20), (120, 40)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            let a = Rect::new(0, 0, w, h);
            let l = orderbook::render(
                f,
                a,
                "BTCUSDT",
                Some(&snap),
                &trades,
                1,
                None,
                0,
                false,
                LoadDiag::default(),
                &[],
                &orders,
                &CLASSIC,
            );
            assert!(l
                .rows
                .iter()
                .all(|(r, _, p)| r.width > 0 && r.height == 1 && *p > 0.0));
            let l2 = orderbook::render(
                f,
                a,
                "BTCUSDT",
                None,
                &trades,
                1,
                None,
                0,
                true,
                LoadDiag::default(),
                &[],
                &[],
                &CLASSIC,
            );
            assert!(l2.rows.is_empty(), "no snapshot: no ladder");
        })
        .unwrap_or_else(|e| panic!("book {w}x{h}: {e}"));
    }
    let mut t = term(120, 40);
    let mut layout = None;
    t.draw(|f| {
        layout = Some(orderbook::render(
            f,
            Rect::new(0, 0, 120, 40),
            "BTCUSDT",
            Some(&snap),
            &trades,
            1,
            None,
            0,
            false,
            LoadDiag::default(),
            &[],
            &orders,
            &CLASSIC,
        ));
    })
    .unwrap();
    let l = layout.unwrap();
    assert!(l.bucket > 0.0);
    // Asks above the spread, bids below; every ladder price is a level.
    let asks: Vec<_> = l.rows.iter().filter(|(_, bid, _)| !bid).collect();
    let bids: Vec<_> = l.rows.iter().filter(|(_, bid, _)| *bid).collect();
    assert!(!asks.is_empty() && !bids.is_empty());
    assert!(asks.iter().all(|(r, _, _)| r.y < bids[0].0.y));
    assert!(bids.iter().all(|(_, _, p)| *p < 100.0) && asks.iter().all(|(_, _, p)| *p > 100.0));
    let s = buffer_to_string(t.backend().buffer());
    assert_eq!(s.matches('◆').count(), 2, "one mark per resting order: {s}");
}

#[test]
fn instrument_finder_survives_tiny_terminals_and_hits_rows_by_id() {
    use dos::data::marketdb::Found;
    use dos::widgets::instruments::{self, FindState};
    let row = |id: i64, sym: &str, active: bool| Found {
        id,
        venue: "BINANCE".into(),
        kind: "spot".into(),
        symbol: sym.into(),
        asset: "stock".into(),
        active,
        vol_usd: 1.5e6,
    };
    let st = FindState {
        rows: vec![row(7, "BTCUSDT", true), row(9, "TSLABUSDT", false)],
        total: 2,
        sel: 1,
        ..Default::default()
    };
    for &(w, h) in TINY.iter().chain(&[(40, 12)]) {
        let mut t = term(w, h);
        t.draw(|f| {
            let l = instruments::render(
                f,
                Rect::new(0, 0, w, h),
                &st,
                Some((w / 2, h / 2)),
                &CLASSIC,
            );
            let _ = l.id_at(w / 2, h / 2);
        })
        .unwrap_or_else(|e| panic!("finder {w}x{h}: {e}"));
    }
    let mut t = term(120, 40);
    let mut layout = None;
    t.draw(|f| {
        layout = Some(instruments::render(
            f,
            Rect::new(0, 0, 120, 40),
            &st,
            None,
            &CLASSIC,
        ));
    })
    .unwrap();
    let text = buffer_to_string(t.backend().buffer());
    assert!(
        text.contains("TSLABUSDT") && text.contains("1-2 of 2") && text.contains("not trading"),
        "{text}"
    );
    let l = layout.unwrap();
    assert_eq!(l.page, instruments::page_rows(Rect::new(0, 0, 120, 40)));
    let (r, id) = l.rows[1];
    assert_eq!(l.id_at(r.x, r.y), Some(id));
    assert_eq!(id, 9, "rows carry identity, not an index");
}
