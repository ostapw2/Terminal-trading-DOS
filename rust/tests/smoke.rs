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
                1.0,
                100.0,
                trade_ticket::SizeMode::Qty,
                1.0,
                Some((1, 1)),
                None,
                &CLASSIC,
            );
            strategy_panel::render(f, area, &runtime, None, &CLASSIC);
            dashboard::render_engine(f, area, &runtime, &CLASSIC);
            screener::render(f, area, &syms, 0, screener::SortMode::Gap, &CLASSIC, false);
        })
        .unwrap_or_else(|e| panic!("engine panels {w}x{h}: {e}"));
    }
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
