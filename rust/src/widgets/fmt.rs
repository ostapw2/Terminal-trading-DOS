//! Shared text / number / rect helpers for widgets.
//!
//! Single source for what used to be copied per widget: char-safe
//! truncation, price / quantity / money formatting, clock, hit-testing
//! and the bull/bear colours (named ANSI colours only).

use ratatui::{layout::Rect, style::Color, widgets::Widget, Frame};

pub const BULL: Color = Color::LightGreen;
pub const BEAR: Color = Color::LightRed;

pub const SPARKLINE_BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// First `w` characters of `s` (never splits a UTF-8 sequence).
pub fn truncate(s: &str, w: usize) -> &str {
    match s.char_indices().nth(w) {
        Some((end, _)) => &s[..end],
        None => s,
    }
}

/// Adaptive price formatter — precision follows magnitude so sub-cent
/// moves on alts stay visible without wasting columns on big caps.
pub fn fmt_price(v: f64) -> String {
    if v <= 0.0 {
        return "----".into();
    }
    if v >= 10_000.0 {
        format!("{:.2}", v)
    } else if v >= 100.0 {
        format!("{:.3}", v)
    } else if v >= 1.0 {
        format!("{:.4}", v)
    } else if v >= 0.01 {
        format!("{:.5}", v)
    } else if v >= 0.0001 {
        format!("{:.6}", v)
    } else {
        format!("{:.8}", v)
    }
}

/// Fixed 10-column price for table cells (screener, order book).
pub fn fmt_price_col(v: f64) -> String {
    if v >= 100.0 {
        format!("{:>10.2}", v)
    } else if v >= 1.0 {
        format!("{:>10.4}", v)
    } else {
        format!("{:>10.6}", v)
    }
}

/// Signed, compact quantity (`+12.30K`, `-0.0420`).
pub fn fmt_qty(q: f64) -> String {
    let abs = q.abs();
    let sign = if q >= 0.0 { "+" } else { "-" };
    if abs >= 1_000_000.0 {
        format!("{}{:.2}M", sign, abs / 1_000_000.0)
    } else if abs >= 10_000.0 {
        format!("{}{:.2}K", sign, abs / 1_000.0)
    } else if abs >= 1.0 {
        format!("{}{:.2}", sign, abs)
    } else {
        format!("{}{:.4}", sign, abs)
    }
}

/// Plain quantity for input fields: up to 6 decimals, trailing zeros trimmed.
pub fn fmt_qty_plain(q: f64) -> String {
    let s = format!("{:.6}", q);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Fixed 7-column quantity for the order book.
pub fn fmt_qty_col(v: f64) -> String {
    if v >= 1e6 {
        format!("{:>6.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:>6.2}K", v / 1e3)
    } else if v >= 100.0 {
        format!("{:>6.1} ", v)
    } else if v >= 1.0 {
        format!("{:>6.3} ", v)
    } else {
        format!("{:>6.4} ", v)
    }
}

/// `+$12.34` / `-$0.50`.
pub fn fmt_money_signed(v: f64) -> String {
    let sign = if v >= 0.0 { "+" } else { "-" };
    format!("{sign}${:.2}", v.abs())
}

/// Colour for a PnL-like value: bull / bear / neutral gray inside ±1e-9.
pub fn pnl_color(v: f64) -> Color {
    if v > 1e-9 {
        BULL
    } else if v < -1e-9 {
        BEAR
    } else {
        Color::Gray
    }
}

/// `HH:MM:SS` (UTC) from Unix milliseconds.
pub fn fmt_clock_ms(ms: i64) -> String {
    let total = (ms / 1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

/// Point-in-rect test that cannot overflow `u16`.
pub fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x
        && y >= r.y
        && (x as u32) < r.x as u32 + r.width as u32
        && (y as u32) < r.y as u32 + r.height as u32
}

/// `frame.render_widget` that clips `r` to the frame, so fixed-offset
/// layouts built for a roomy terminal cannot index outside the buffer on
/// a small one (audit W5).
pub fn render_clipped<W: Widget>(frame: &mut Frame, w: W, r: Rect) {
    let r = r.intersection(frame.area());
    if r.width > 0 && r.height > 0 {
        frame.render_widget(w, r);
    }
}

/// `w` × `h` rect centred in `screen`, clamped so it never exceeds it.
pub fn centered(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(screen.width);
    let h = h.min(screen.height);
    Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("→Δ…x", 3), "→Δ…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn price_precision_follows_magnitude() {
        assert_eq!(fmt_price(0.0), "----");
        assert_eq!(fmt_price(65_000.123), "65000.12");
        assert_eq!(fmt_price(0.00001234), "0.00001234");
    }

    #[test]
    fn qty_formats() {
        assert_eq!(fmt_qty(1_500_000.0), "+1.50M");
        assert_eq!(fmt_qty(-0.5), "-0.5000");
        assert_eq!(fmt_qty_plain(0.5), "0.5");
        assert_eq!(fmt_qty_plain(0.0), "0");
    }

    #[test]
    fn centered_never_exceeds_screen() {
        let screen = Rect::new(0, 0, 30, 8);
        let r = centered(screen, 72, 30);
        assert_eq!((r.width, r.height), (30, 8));
        let r = centered(screen, 10, 4);
        assert_eq!((r.x, r.y), (10, 2));
    }

    #[test]
    fn money_clock_and_rect() {
        assert_eq!(fmt_money_signed(-3.456), "-$3.46");
        assert_eq!(fmt_clock_ms(3_661_000), "01:01:01");
        let r = Rect::new(u16::MAX - 10, 0, 10, 1);
        assert!(rect_contains(r, u16::MAX - 1, 0));
        assert!(!rect_contains(r, u16::MAX, 0));
        assert!(!rect_contains(r, 0, 0));
    }
}
