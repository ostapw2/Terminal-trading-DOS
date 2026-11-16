//! Technical analysis for agents: indicators, trend, support / resistance,
//! candlestick and chart patterns.  Pure functions from candles to compact
//! JSON; nothing here touches the network or the app state.
//!
//! The patterns are **heuristics**, not facts: each one carries a rough
//! confidence and the reply says so.  They are meant to point an agent at
//! what to look at (and the chart image shows it), not to replace judgement.

use serde_json::{json, Value};

use crate::markets::{Ohlc, Timeframe};

/// Fewest candles `analyze` accepts.
pub const MIN_CANDLES: usize = 30;
/// Most candles it looks at (older ones are ignored).
pub const MAX_CANDLES: usize = 500;

// ─── Indicators ─────────────────────────────────────────────────────────

pub fn sma(v: &[f64], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; v.len()];
    if n == 0 || v.len() < n {
        return out;
    }
    let mut sum: f64 = v[..n].iter().sum();
    out[n - 1] = Some(sum / n as f64);
    for i in n..v.len() {
        sum += v[i] - v[i - n];
        out[i] = Some(sum / n as f64);
    }
    out
}

/// Exponential average seeded with the first `n` values' mean.
pub fn ema(v: &[f64], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; v.len()];
    if n == 0 || v.len() < n {
        return out;
    }
    let k = 2.0 / (n as f64 + 1.0);
    let mut prev = v[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = Some(prev);
    for i in n..v.len() {
        prev = v[i] * k + prev * (1.0 - k);
        out[i] = Some(prev);
    }
    out
}

/// Wilder's RSI.
pub fn rsi(v: &[f64], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; v.len()];
    if n == 0 || v.len() <= n {
        return out;
    }
    let (mut gain, mut loss) = (0.0, 0.0);
    for i in 1..=n {
        let d = v[i] - v[i - 1];
        if d >= 0.0 {
            gain += d;
        } else {
            loss -= d;
        }
    }
    let (mut ag, mut al) = (gain / n as f64, loss / n as f64);
    let value = |ag: f64, al: f64| {
        if al == 0.0 {
            if ag == 0.0 {
                50.0
            } else {
                100.0
            }
        } else {
            100.0 - 100.0 / (1.0 + ag / al)
        }
    };
    out[n] = Some(value(ag, al));
    for i in (n + 1)..v.len() {
        let d = v[i] - v[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        ag = (ag * (n as f64 - 1.0) + g) / n as f64;
        al = (al * (n as f64 - 1.0) + l) / n as f64;
        out[i] = Some(value(ag, al));
    }
    out
}

/// Wilder's average true range.
pub fn atr(c: &[Ohlc], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; c.len()];
    if n == 0 || c.len() <= n {
        return out;
    }
    let tr = |i: usize| {
        let hl = c[i].high - c[i].low;
        if i == 0 {
            hl
        } else {
            hl.max((c[i].high - c[i - 1].close).abs())
                .max((c[i].low - c[i - 1].close).abs())
        }
    };
    let mut prev = (1..=n).map(tr).sum::<f64>() / n as f64;
    out[n] = Some(prev);
    for (i, slot) in out.iter_mut().enumerate().skip(n + 1) {
        prev = (prev * (n as f64 - 1.0) + tr(i)) / n as f64;
        *slot = Some(prev);
    }
    out
}

/// (macd, signal, histogram) with 12 / 26 / 9.
/// One value per candle, `None` while undefined.
pub type Series = Vec<Option<f64>>;

pub fn macd(v: &[f64]) -> (Series, Series, Series) {
    let (fast, slow) = (ema(v, 12), ema(v, 26));
    let line: Vec<Option<f64>> = fast
        .iter()
        .zip(&slow)
        .map(|(f, s)| f.zip(*s).map(|(f, s)| f - s))
        .collect();
    // Signal = EMA(9) of the defined part of the line.
    let first = line.iter().position(Option::is_some).unwrap_or(line.len());
    let defined: Vec<f64> = line[first..].iter().flatten().copied().collect();
    let sig_tail = ema(&defined, 9);
    let mut signal = vec![None; v.len()];
    for (k, s) in sig_tail.into_iter().enumerate() {
        signal[first + k] = s;
    }
    let hist = line
        .iter()
        .zip(&signal)
        .map(|(m, s)| m.zip(*s).map(|(m, s)| m - s))
        .collect();
    (line, signal, hist)
}

/// (lower, middle, upper) bands.
pub fn bollinger(v: &[f64], n: usize, k: f64) -> Vec<Option<(f64, f64, f64)>> {
    let mid = sma(v, n);
    (0..v.len())
        .map(|i| {
            let m = mid[i]?;
            let w = &v[i + 1 - n..=i];
            let var = w.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n as f64;
            let sd = var.sqrt();
            Some((m - k * sd, m, m + k * sd))
        })
        .collect()
}

fn last(v: &[Option<f64>]) -> Option<f64> {
    v.last().copied().flatten()
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

/// Rounds to ~5 significant digits so tiny and huge prices both read well.
fn sig(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let mag = x.abs().log10().floor() as i32;
    round(x, (4 - mag).clamp(0, 10))
}

// ─── Pivots and levels ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pivot {
    pub idx: usize,
    pub price: f64,
}

/// Swing highs and lows: a bar whose high (low) is the extreme of the `k`
/// bars on each side.  The last `k` bars cannot be confirmed yet.
pub fn pivots(c: &[Ohlc], k: usize) -> (Vec<Pivot>, Vec<Pivot>) {
    let (mut highs, mut lows) = (Vec::new(), Vec::new());
    if c.len() < 2 * k + 1 {
        return (highs, lows);
    }
    for i in k..c.len() - k {
        let (h, l) = (c[i].high, c[i].low);
        let hi = (i - k..i).all(|j| c[j].high < h) && (i + 1..=i + k).all(|j| c[j].high <= h);
        let lo = (i - k..i).all(|j| c[j].low > l) && (i + 1..=i + k).all(|j| c[j].low >= l);
        if hi {
            highs.push(Pivot { idx: i, price: h });
        }
        if lo {
            lows.push(Pivot { idx: i, price: l });
        }
    }
    (highs, lows)
}

/// Price levels where several swings met: (price, touches), strongest first.
pub fn levels(highs: &[Pivot], lows: &[Pivot], tol: f64) -> Vec<(f64, usize)> {
    let mut pts: Vec<f64> = highs.iter().chain(lows).map(|p| p.price).collect();
    pts.sort_by(|a, b| a.total_cmp(b));
    let mut clusters: Vec<(f64, usize)> = Vec::new();
    for p in pts {
        match clusters.last_mut() {
            Some((mean, n)) if (p - *mean).abs() <= tol => {
                *mean = (*mean * *n as f64 + p) / (*n as f64 + 1.0);
                *n += 1;
            }
            _ => clusters.push((p, 1)),
        }
    }
    clusters.retain(|&(_, n)| n >= 2);
    clusters.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.total_cmp(&b.0)));
    clusters
}

// ─── Candlestick patterns ───────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct CandlePattern {
    pub bars_ago: usize,
    pub name: &'static str,
    /// "bullish", "bearish" or "neutral".
    pub bias: &'static str,
    pub note: &'static str,
}

fn body(c: &Ohlc) -> f64 {
    (c.close - c.open).abs()
}
fn range(c: &Ohlc) -> f64 {
    c.high - c.low
}
fn upper(c: &Ohlc) -> f64 {
    c.high - c.open.max(c.close)
}
fn lower(c: &Ohlc) -> f64 {
    c.open.min(c.close) - c.low
}
fn bull(c: &Ohlc) -> bool {
    c.close > c.open
}
fn bear(c: &Ohlc) -> bool {
    c.close < c.open
}

/// Patterns that end on one of the last `recent` bars.  `trend_before` for a
/// bar is the sign of the close change over the 4 bars before the pattern.
pub fn candle_patterns(c: &[Ohlc], recent: usize) -> Vec<CandlePattern> {
    let n = c.len();
    let mut out = Vec::new();
    if n < 6 {
        return out;
    }
    let atr14 = atr(c, 14);
    for ago in 0..recent.min(n - 5) {
        let i = n - 1 - ago;
        let a = atr14[i].unwrap_or_else(|| range(&c[i]).max(1e-12));
        let trend = c[i - 1].close - c[i - 4].close; // before the bar
        let down = trend < 0.0;
        let up = trend > 0.0;
        let cur = &c[i];
        let prev = &c[i - 1];
        let r = range(cur);
        let mut push = |name: &'static str, bias: &'static str, note: &'static str| {
            out.push(CandlePattern {
                bars_ago: ago,
                name,
                bias,
                note,
            });
        };
        if r <= 0.0 {
            continue;
        }
        if body(cur) <= 0.1 * r {
            push("doji", "neutral", "indecision: open ~ close");
        }
        if lower(cur) >= 2.0 * body(cur).max(r * 0.05)
            && upper(cur) <= 0.35 * lower(cur)
            && body(cur) > 0.0
        {
            if down {
                push("hammer", "bullish", "long lower shadow after a decline");
            } else if up {
                push("hanging_man", "bearish", "long lower shadow after a rise");
            }
        }
        if upper(cur) >= 2.0 * body(cur).max(r * 0.05)
            && lower(cur) <= 0.35 * upper(cur)
            && body(cur) > 0.0
        {
            if up {
                push("shooting_star", "bearish", "long upper shadow after a rise");
            } else if down {
                push(
                    "inverted_hammer",
                    "bullish",
                    "long upper shadow after a decline",
                );
            }
        }
        if bear(prev)
            && bull(cur)
            && cur.open <= prev.close
            && cur.close >= prev.open
            && body(cur) > body(prev)
        {
            push(
                "bullish_engulfing",
                "bullish",
                "a green body swallows the red one",
            );
        }
        if bull(prev)
            && bear(cur)
            && cur.open >= prev.close
            && cur.close <= prev.open
            && body(cur) > body(prev)
        {
            push(
                "bearish_engulfing",
                "bearish",
                "a red body swallows the green one",
            );
        }
        if cur.high < prev.high && cur.low > prev.low {
            push(
                "inside_bar",
                "neutral",
                "range inside the previous bar: compression",
            );
        }
        if body(cur) >= 0.9 * r && r >= 1.2 * a {
            if bull(cur) {
                push(
                    "bullish_marubozu",
                    "bullish",
                    "strong bar, almost no shadows",
                );
            } else {
                push(
                    "bearish_marubozu",
                    "bearish",
                    "strong bar, almost no shadows",
                );
            }
        }
        if i >= 2 {
            let (p2, p1) = (&c[i - 2], &c[i - 1]);
            let mid1 = (p2.open + p2.close) / 2.0;
            if bear(p2)
                && body(p2) >= 0.5 * range(p2)
                && body(p1) <= 0.35 * body(p2)
                && bull(cur)
                && cur.close > mid1
            {
                push("morning_star", "bullish", "big red, small star, big green");
            }
            if bull(p2)
                && body(p2) >= 0.5 * range(p2)
                && body(p1) <= 0.35 * body(p2)
                && bear(cur)
                && cur.close < mid1
            {
                push("evening_star", "bearish", "big green, small star, big red");
            }
            let strong = |x: &Ohlc| body(x) >= 0.5 * range(x);
            if bull(p2)
                && bull(p1)
                && bull(cur)
                && strong(p2)
                && strong(p1)
                && strong(cur)
                && p1.close > p2.close
                && cur.close > p1.close
            {
                push(
                    "three_white_soldiers",
                    "bullish",
                    "three strong rising green bars",
                );
            }
            if bear(p2)
                && bear(p1)
                && bear(cur)
                && strong(p2)
                && strong(p1)
                && strong(cur)
                && p1.close < p2.close
                && cur.close < p1.close
            {
                push(
                    "three_black_crows",
                    "bearish",
                    "three strong falling red bars",
                );
            }
        }
    }
    out
}

// ─── Chart patterns ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct ChartPattern {
    pub name: &'static str,
    pub bias: &'static str,
    /// "low" / "medium" / "high": how clean the match is, not a probability.
    pub confidence: &'static str,
    pub from_idx: usize,
    pub to_idx: usize,
    /// Key prices (neckline, range edges, breakout level ...).
    pub levels: Vec<(&'static str, f64)>,
    pub note: String,
}

/// Least-squares line through (idx, price): (slope per bar, value at `at`).
fn fit(points: &[Pivot], at: usize) -> Option<(f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.idx as f64).sum::<f64>() / n;
    let my = points.iter().map(|p| p.price).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.idx as f64 - mx).powi(2)).sum();
    if sxx == 0.0 {
        return None;
    }
    let sxy: f64 = points
        .iter()
        .map(|p| (p.idx as f64 - mx) * (p.price - my))
        .sum();
    let slope = sxy / sxx;
    Some((slope, my + slope * (at as f64 - mx)))
}

pub fn chart_patterns(c: &[Ohlc]) -> Vec<ChartPattern> {
    let n = c.len();
    let mut out = Vec::new();
    if n < MIN_CANDLES {
        return out;
    }
    let closes: Vec<f64> = c.iter().map(|k| k.close).collect();
    let a = last(&atr(c, 14)).unwrap_or(0.0);
    if a <= 0.0 {
        return out;
    }
    let price = c[n - 1].close;
    let (highs, lows) = pivots(c, 3);
    let rsi14 = rsi(&closes, 14);

    // Trend structure from the last three swings of each kind.
    if highs.len() >= 3 && lows.len() >= 3 {
        let h = &highs[highs.len() - 3..];
        let l = &lows[lows.len() - 3..];
        let hh = h[2].price > h[1].price && h[1].price > h[0].price;
        let hl = l[2].price > l[1].price && l[1].price > l[0].price;
        let lh = h[2].price < h[1].price && h[1].price < h[0].price;
        let ll = l[2].price < l[1].price && l[1].price < l[0].price;
        let from = h[0].idx.min(l[0].idx);
        if hh && hl {
            out.push(ChartPattern {
                name: "uptrend_structure",
                bias: "bullish",
                confidence: "medium",
                from_idx: from,
                to_idx: n - 1,
                levels: vec![("last_higher_low", l[2].price)],
                note:
                    "higher highs and higher lows; below the last higher low the structure breaks"
                        .into(),
            });
        } else if lh && ll {
            out.push(ChartPattern {
                name: "downtrend_structure",
                bias: "bearish",
                confidence: "medium",
                from_idx: from,
                to_idx: n - 1,
                levels: vec![("last_lower_high", h[2].price)],
                note: "lower highs and lower lows; above the last lower high the structure breaks"
                    .into(),
            });
        }
    }

    // Double top / bottom.
    if highs.len() >= 2 {
        let (p, q) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        let trough = c[p.idx..=q.idx]
            .iter()
            .map(|k| k.low)
            .fold(f64::INFINITY, f64::min);
        if q.idx - p.idx >= 5
            && (p.price - q.price).abs() <= 0.8 * a.max(p.price * 0.004)
            && p.price.min(q.price) - trough >= 1.5 * a
            && n - 1 - q.idx <= 25
        {
            let broken = price < trough;
            out.push(ChartPattern {
                name: "double_top",
                bias: "bearish",
                confidence: if broken { "high" } else { "medium" },
                from_idx: p.idx,
                to_idx: q.idx,
                levels: vec![("top", p.price.max(q.price)), ("neckline", trough)],
                note: format!(
                    "two highs at about the same level; {}",
                    if broken {
                        "price closed below the neckline: confirmed"
                    } else {
                        "confirmed only by a close below the neckline"
                    }
                ),
            });
        }
    }
    if lows.len() >= 2 {
        let (p, q) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        let peak = c[p.idx..=q.idx]
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max);
        if q.idx - p.idx >= 5
            && (p.price - q.price).abs() <= 0.8 * a.max(p.price * 0.004)
            && peak - p.price.max(q.price) >= 1.5 * a
            && n - 1 - q.idx <= 25
        {
            let broken = price > peak;
            out.push(ChartPattern {
                name: "double_bottom",
                bias: "bullish",
                confidence: if broken { "high" } else { "medium" },
                from_idx: p.idx,
                to_idx: q.idx,
                levels: vec![("bottom", p.price.min(q.price)), ("neckline", peak)],
                note: format!(
                    "two lows at about the same level; {}",
                    if broken {
                        "price closed above the neckline: confirmed"
                    } else {
                        "confirmed only by a close above the neckline"
                    }
                ),
            });
        }
    }

    // Head and shoulders (and inverse): three swings, the middle one extreme.
    if highs.len() >= 3 {
        let h = &highs[highs.len() - 3..];
        let (l, m, r) = (h[0], h[1], h[2]);
        let neck = c[l.idx..=r.idx]
            .iter()
            .map(|k| k.low)
            .fold(f64::INFINITY, f64::min);
        if m.price - l.price.max(r.price) >= 1.0 * a
            && (l.price - r.price).abs() <= 1.2 * a.max(l.price * 0.006)
            && r.idx - l.idx >= 10
            && n - 1 - r.idx <= 25
        {
            let broken = price < neck;
            out.push(ChartPattern {
                name: "head_and_shoulders",
                bias: "bearish",
                confidence: if broken { "high" } else { "low" },
                from_idx: l.idx,
                to_idx: r.idx,
                levels: vec![("head", m.price), ("neckline", neck)],
                note: "a higher middle peak between two lower, similar ones".into(),
            });
        }
    }
    if lows.len() >= 3 {
        let h = &lows[lows.len() - 3..];
        let (l, m, r) = (h[0], h[1], h[2]);
        let neck = c[l.idx..=r.idx]
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max);
        if l.price.min(r.price) - m.price >= 1.0 * a
            && (l.price - r.price).abs() <= 1.2 * a.max(l.price * 0.006)
            && r.idx - l.idx >= 10
            && n - 1 - r.idx <= 25
        {
            let broken = price > neck;
            out.push(ChartPattern {
                name: "inverse_head_and_shoulders",
                bias: "bullish",
                confidence: if broken { "high" } else { "low" },
                from_idx: l.idx,
                to_idx: r.idx,
                levels: vec![("head", m.price), ("neckline", neck)],
                note: "a lower middle trough between two higher, similar ones".into(),
            });
        }
    }

    // Triangles / wedges from the swings of the last 60 bars.
    let from = n.saturating_sub(60);
    let rh: Vec<Pivot> = highs.iter().filter(|p| p.idx >= from).copied().collect();
    let rl: Vec<Pivot> = lows.iter().filter(|p| p.idx >= from).copied().collect();
    if rh.len() >= 2 && rl.len() >= 2 {
        if let (Some((sh, _)), Some((sl, _))) = (fit(&rh, n - 1), fit(&rl, n - 1)) {
            let span = (n - 1 - rh[0].idx.min(rl[0].idx)) as f64;
            let (dh, dl) = (sh * span, sl * span); // change over the pattern
            let flat = 0.5 * a;
            let name = if dh.abs() <= flat && dl > 1.0 * a {
                Some(("ascending_triangle", "bullish", "flat top, rising lows"))
            } else if dl.abs() <= flat && dh < -a {
                Some((
                    "descending_triangle",
                    "bearish",
                    "flat bottom, falling highs",
                ))
            } else if dh < -0.7 * a && dl > 0.7 * a {
                Some((
                    "symmetrical_triangle",
                    "neutral",
                    "lower highs and higher lows converge: a breakout is coming",
                ))
            } else if dh > 0.7 * a && dl > 0.7 * a && dl > dh * 1.3 {
                Some((
                    "rising_wedge",
                    "bearish",
                    "both lines rise, the lower one faster",
                ))
            } else if dh < -0.7 * a && dl < -0.7 * a && dh < dl * 1.3 {
                Some((
                    "falling_wedge",
                    "bullish",
                    "both lines fall, the upper one faster",
                ))
            } else {
                None
            };
            if let Some((name, bias, note)) = name {
                let (top, _) = fit(&rh, n - 1).map(|(_, v)| (v, 0)).unwrap_or((0.0, 0));
                let (bot, _) = fit(&rl, n - 1).map(|(_, v)| (v, 0)).unwrap_or((0.0, 0));
                if top > bot {
                    out.push(ChartPattern {
                        name,
                        bias,
                        confidence: if rh.len() >= 3 && rl.len() >= 3 {
                            "medium"
                        } else {
                            "low"
                        },
                        from_idx: rh[0].idx.min(rl[0].idx),
                        to_idx: n - 1,
                        levels: vec![("upper_line_now", top), ("lower_line_now", bot)],
                        note: note.into(),
                    });
                }
            }
        }
    }

    // Range and breakout over the last 20 bars.
    if n >= 22 {
        let prior = &c[n - 21..n - 1];
        let hh = prior
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let ll = prior.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
        let avg_vol = prior.iter().map(|k| k.volume).sum::<f64>() / prior.len() as f64;
        let vol_ratio = if avg_vol > 0.0 {
            c[n - 1].volume / avg_vol
        } else {
            1.0
        };
        let vol_note = format!("volume {:.1}x the 20-bar average", vol_ratio);
        if price > hh {
            out.push(ChartPattern {
                name: "breakout_up",
                bias: "bullish",
                confidence: if vol_ratio >= 1.5 { "high" } else { "medium" },
                from_idx: n - 21,
                to_idx: n - 1,
                levels: vec![("broken_level", hh)],
                note: format!("close above the 20-bar high; {vol_note}"),
            });
        } else if price < ll {
            out.push(ChartPattern {
                name: "breakdown",
                bias: "bearish",
                confidence: if vol_ratio >= 1.5 { "high" } else { "medium" },
                from_idx: n - 21,
                to_idx: n - 1,
                levels: vec![("broken_level", ll)],
                note: format!("close below the 20-bar low; {vol_note}"),
            });
        } else if hh - ll <= 3.5 * a {
            out.push(ChartPattern {
                name: "range",
                bias: "neutral",
                confidence: "medium",
                from_idx: n - 21,
                to_idx: n - 1,
                levels: vec![("top", hh), ("bottom", ll)],
                note: "20 bars inside a narrow band: wait for the break".into(),
            });
        }
    }

    // RSI divergence on the last two swings.
    let rsi_at = |i: usize| rsi14[i];
    if highs.len() >= 2 {
        let (p, q) = (highs[highs.len() - 2], highs[highs.len() - 1]);
        if let (Some(rp), Some(rq)) = (rsi_at(p.idx), rsi_at(q.idx)) {
            if q.price > p.price && rq + 3.0 < rp && n - 1 - q.idx <= 20 {
                out.push(ChartPattern {
                    name: "bearish_rsi_divergence",
                    bias: "bearish",
                    confidence: "medium",
                    from_idx: p.idx,
                    to_idx: q.idx,
                    levels: vec![("rsi_first", rp), ("rsi_second", rq)],
                    note: "higher price high, lower RSI high: momentum fades".into(),
                });
            }
        }
    }
    if lows.len() >= 2 {
        let (p, q) = (lows[lows.len() - 2], lows[lows.len() - 1]);
        if let (Some(rp), Some(rq)) = (rsi_at(p.idx), rsi_at(q.idx)) {
            if q.price < p.price && rq > rp + 3.0 && n - 1 - q.idx <= 20 {
                out.push(ChartPattern {
                    name: "bullish_rsi_divergence",
                    bias: "bullish",
                    confidence: "medium",
                    from_idx: p.idx,
                    to_idx: q.idx,
                    levels: vec![("rsi_first", rp), ("rsi_second", rq)],
                    note: "lower price low, higher RSI low: selling momentum fades".into(),
                });
            }
        }
    }
    out
}

// ─── The report ─────────────────────────────────────────────────────────

/// Is `last` still being built at `now_ms`?  (Its period has not ended.)
pub fn is_forming(last: &Ohlc, tf: Timeframe, now_ms: i64) -> bool {
    last.time_ms > 0 && tf.next_open_ms(last.time_ms) > now_ms
}

/// Everything an agent needs about one chart, as compact JSON.  Indicators
/// and patterns use closed candles only: a candle still being built (the
/// exchange sends it as the last one) is reported separately.
pub fn analyze(
    symbol: &str,
    tf: Timeframe,
    candles: &[Ohlc],
    now_ms: i64,
) -> Result<Value, String> {
    let mut real: Vec<Ohlc> = candles.iter().filter(|k| !k.is_gap).cloned().collect();
    let forming = match real.last() {
        Some(l) if is_forming(l, tf, now_ms) => real.pop(),
        _ => None,
    };
    let from = real.len().saturating_sub(MAX_CANDLES);
    let c = &real[from..];
    if c.len() < MIN_CANDLES {
        return Err(format!(
            "need at least {MIN_CANDLES} candles for {symbol}, got {}",
            c.len()
        ));
    }
    let n = c.len();
    let closes: Vec<f64> = c.iter().map(|k| k.close).collect();
    let price = closes[n - 1];
    let s20 = last(&sma(&closes, 20));
    let s50 = last(&sma(&closes, 50));
    let s200 = last(&sma(&closes, 200));
    let e12 = last(&ema(&closes, 12));
    let e26 = last(&ema(&closes, 26));
    let rsi14 = last(&rsi(&closes, 14));
    let a14 = last(&atr(c, 14));
    let (m, ms, mh) = macd(&closes);
    let bb = bollinger(&closes, 20, 2.0).last().copied().flatten();
    let vol20 = if n >= 21 {
        c[n - 21..n - 1].iter().map(|k| k.volume).sum::<f64>() / 20.0
    } else {
        0.0
    };

    // Votes: each one is a plain statement with a sign, so the total is
    // explainable.  Not advice.
    let mut votes: Vec<(i32, String)> = Vec::new();
    if let Some(s) = s20 {
        votes.push(if price > s {
            (1, format!("price above SMA20 ({})", sig(s)))
        } else {
            (-1, format!("price below SMA20 ({})", sig(s)))
        });
    }
    if let (Some(a), Some(b)) = (s20, s50) {
        votes.push(if a > b {
            (1, "SMA20 above SMA50".to_string())
        } else {
            (-1, "SMA20 below SMA50".to_string())
        });
    }
    if let Some(s) = s200 {
        votes.push(if price > s {
            (1, "price above SMA200".to_string())
        } else {
            (-1, "price below SMA200".to_string())
        });
    }
    if let Some(r) = rsi14 {
        if r >= 70.0 {
            votes.push((-1, format!("RSI {r:.0}: overbought")));
        } else if r <= 30.0 {
            votes.push((1, format!("RSI {r:.0}: oversold")));
        } else if r >= 55.0 {
            votes.push((1, format!("RSI {r:.0}: bullish momentum")));
        } else if r <= 45.0 {
            votes.push((-1, format!("RSI {r:.0}: bearish momentum")));
        }
    }
    if let (Some(h), Some(hp)) = (last(&mh), mh.get(n.wrapping_sub(2)).copied().flatten()) {
        votes.push(if h > 0.0 {
            (
                1,
                format!(
                    "MACD histogram positive{}",
                    if h > hp { " and rising" } else { "" }
                ),
            )
        } else {
            (
                -1,
                format!(
                    "MACD histogram negative{}",
                    if h < hp { " and falling" } else { "" }
                ),
            )
        });
    }
    let candle_pats = candle_patterns(c, 5);
    let chart_pats = chart_patterns(c);
    for p in &chart_pats {
        match p.bias {
            "bullish" => votes.push((1, format!("pattern: {}", p.name))),
            "bearish" => votes.push((-1, format!("pattern: {}", p.name))),
            _ => {}
        }
    }
    let score: i32 = votes.iter().map(|v| v.0).sum();
    let label = match score {
        s if s >= 4 => "bullish",
        s if s >= 2 => "mildly bullish",
        s if s <= -4 => "bearish",
        s if s <= -2 => "mildly bearish",
        _ => "mixed",
    };

    let (highs, lows) = pivots(c, 3);
    let tol = a14.map_or(price * 0.004, |a| a * 0.6);
    let lv = levels(&highs, &lows, tol);
    let mut support: Vec<&(f64, usize)> = lv.iter().filter(|l| l.0 < price).collect();
    let mut resistance: Vec<&(f64, usize)> = lv.iter().filter(|l| l.0 > price).collect();
    support.sort_by(|a, b| b.0.total_cmp(&a.0)); // nearest first
    resistance.sort_by(|a, b| a.0.total_cmp(&b.0));
    let lvl = |l: &&(f64, usize)| {
        json!({"price": sig(l.0), "touches": l.1,
        "distance_pct": round((l.0 - price) / price * 100.0, 2)})
    };

    let t_of = |i: usize| c[i.min(n - 1)].time_ms;
    let mut summary: Vec<String> = Vec::new();
    summary.push(format!(
        "{symbol} {} at {}: {label} (score {score:+})",
        tf.name(),
        sig(price)
    ));
    for p in chart_pats.iter().filter(|p| p.confidence != "low") {
        summary.push(format!("{} ({}): {}", p.name, p.bias, p.note));
    }
    for p in candle_pats.iter().filter(|p| p.bars_ago <= 1) {
        summary.push(format!(
            "candle {}: {} ({}, {} bar(s) ago)",
            p.name, p.note, p.bias, p.bars_ago
        ));
    }
    if let Some(l) = support.first() {
        summary.push(format!("nearest support {} ({} touches)", sig(l.0), l.1));
    }
    if let Some(l) = resistance.first() {
        summary.push(format!("nearest resistance {} ({} touches)", sig(l.0), l.1));
    }

    Ok(json!({
        "symbol": symbol,
        "timeframe": tf.name(),
        "candles_used": n,
        "last_close": sig(price),
        "last_candle_time_ms": c[n - 1].time_ms,
        "forming_candle": forming.as_ref().map(|f| json!({
            "time_ms": f.time_ms, "open": sig(f.open), "high": sig(f.high),
            "low": sig(f.low), "close": sig(f.close), "volume": f.volume,
            "elapsed_pct": round(((now_ms - f.time_ms) as f64 / ((tf.next_open_ms(f.time_ms) - f.time_ms).max(1) as f64) * 100.0).clamp(0.0, 100.0), 0),
        })),
        "price_now": forming.as_ref().map(|f| sig(f.close)),
        "note_on_candles": "indicators, levels and patterns use CLOSED candles; forming_candle (if present) is the one still being built",
        "bias": {"label": label, "score": score,
                 "votes": votes.iter().map(|(s, t)| json!({"sign": s, "why": t})).collect::<Vec<_>>()},
        "indicators": {
            "sma20": s20.map(sig), "sma50": s50.map(sig), "sma200": s200.map(sig),
            "ema12": e12.map(sig), "ema26": e26.map(sig),
            "rsi14": rsi14.map(|v| round(v, 1)),
            "atr14": a14.map(sig),
            "atr14_pct": a14.map(|a| round(a / price * 100.0, 2)),
            "macd": last(&m).map(sig), "macd_signal": last(&ms).map(sig),
            "macd_hist": last(&mh).map(sig),
            "bollinger": bb.map(|(l, mid, u)| json!({
                "lower": sig(l), "middle": sig(mid), "upper": sig(u),
                "percent_b": if u > l { json!(round((price - l) / (u - l), 2)) } else { Value::Null },
                "bandwidth_pct": round((u - l) / mid * 100.0, 2),
            })),
            "volume_vs_avg20": if vol20 > 0.0 { json!(round(c[n - 1].volume / vol20, 2)) } else { Value::Null },
        },
        "levels": {
            "support": support.iter().take(3).map(lvl).collect::<Vec<_>>(),
            "resistance": resistance.iter().take(3).map(lvl).collect::<Vec<_>>(),
            "recent_swing_highs": highs.iter().rev().take(3).map(|p| json!({"price": sig(p.price), "time_ms": t_of(p.idx)})).collect::<Vec<_>>(),
            "recent_swing_lows": lows.iter().rev().take(3).map(|p| json!({"price": sig(p.price), "time_ms": t_of(p.idx)})).collect::<Vec<_>>(),
        },
        "chart_patterns": chart_pats.iter().map(|p| json!({
            "pattern": p.name, "bias": p.bias, "confidence": p.confidence,
            "from_time_ms": t_of(p.from_idx), "to_time_ms": t_of(p.to_idx),
            "levels": p.levels.iter().map(|(k, v)| json!({"name": k, "price": sig(*v)})).collect::<Vec<_>>(),
            "note": p.note,
        })).collect::<Vec<_>>(),
        "candle_patterns": candle_pats.iter().map(|p| json!({
            "pattern": p.name, "bias": p.bias, "bars_ago": p.bars_ago, "note": p.note,
        })).collect::<Vec<_>>(),
        "summary": summary,
        "caveat": "heuristic pattern detection on closed candles: a hint about where to look, not a signal; confirm on the chart and with volume",
    }))
}

/// Several single-timeframe reports (from [`analyze`]) side by side: per
/// timeframe the bias, price, nearest levels and the patterns that matter,
/// and whether the timeframes agree (the usual top-down read: the higher
/// timeframe sets the direction, the lower one the timing).
pub fn multi_json(symbol: &str, reports: &[(Timeframe, Value)]) -> Value {
    let mut sorted: Vec<&(Timeframe, Value)> = reports.iter().collect();
    sorted.sort_by_key(|(tf, _)| std::cmp::Reverse(tf.duration_secs())); // highest first
    let names = |v: &Value, key: &str, skip_low: bool| -> Vec<Value> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|p| !(skip_low && p["confidence"] == "low"))
                    .map(|p| json!({"pattern": p["pattern"], "bias": p["bias"]}))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut per = serde_json::Map::new();
    let mut scores: Vec<(&str, i64)> = Vec::new();
    for (tf, r) in &sorted {
        let score = r["bias"]["score"].as_i64().unwrap_or(0);
        scores.push((tf.name(), score));
        per.insert(
            tf.name().to_string(),
            json!({
                "bias": r["bias"]["label"], "score": score,
                "price": r["price_now"].as_f64().or(r["last_close"].as_f64()),
                "rsi14": r["indicators"]["rsi14"],
                "sma50": r["indicators"]["sma50"],
                "nearest_support": r["levels"]["support"][0],
                "nearest_resistance": r["levels"]["resistance"][0],
                "chart_patterns": names(r, "chart_patterns", true),
                "candle_patterns": r["candle_patterns"].as_array().map(|a| a.iter()
                    .filter(|p| p["bars_ago"].as_u64().unwrap_or(9) <= 1 && p["bias"] != "neutral")
                    .map(|p| json!({"pattern": p["pattern"], "bias": p["bias"]})).collect::<Vec<_>>()),
                "summary": r["summary"],
            }),
        );
    }
    let bullish = scores.iter().filter(|s| s.1 >= 2).count();
    let bearish = scores.iter().filter(|s| s.1 <= -2).count();
    let alignment = if scores.len() > 1 && bullish == scores.len() {
        "aligned bullish: every timeframe leans up".to_string()
    } else if scores.len() > 1 && bearish == scores.len() {
        "aligned bearish: every timeframe leans down".to_string()
    } else if bullish > 0 && bearish > 0 {
        let up: Vec<&str> = scores.iter().filter(|s| s.1 >= 2).map(|s| s.0).collect();
        let down: Vec<&str> = scores.iter().filter(|s| s.1 <= -2).map(|s| s.0).collect();
        format!(
            "conflict: {} lean up while {} lean down",
            up.join(", "),
            down.join(", ")
        )
    } else {
        "mixed: no clear agreement between timeframes".to_string()
    };
    json!({
        "symbol": symbol,
        "timeframes": per,
        "order": sorted.iter().map(|(tf, _)| tf.name()).collect::<Vec<_>>(),
        "alignment": alignment,
        "how_to_read": "highest timeframe first: it sets the direction, lower ones give the timing; each entry is the same analysis as get_ta for one timeframe",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 100.0,
            date: String::new(),
            time_ms: 0,
            is_gap: false,
        }
    }

    /// Candles from closes, with a small constant range around each.
    fn series(closes: &[f64]) -> Vec<Ohlc> {
        let mut prev = closes[0];
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let o = prev;
                prev = c;
                let mut x = k(o, o.max(c) + 0.5, o.min(c) - 0.5, c);
                x.time_ms = i as i64 * 60_000;
                x
            })
            .collect()
    }

    #[test]
    fn indicators_match_hand_computed_values() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(sma(&v, 3), [None, None, Some(2.0), Some(3.0), Some(4.0)]);
        let e = ema(&v, 3);
        assert_eq!(e[2], Some(2.0));
        assert_eq!(e[3], Some(3.0)); // k = 0.5: 4*0.5 + 2*0.5
        assert_eq!(e[4], Some(4.0));
        // A strictly rising series has RSI 100, a falling one 0, a flat one 50.
        let up: Vec<f64> = (0..30).map(f64::from).collect();
        assert_eq!(last(&rsi(&up, 14)), Some(100.0));
        let down: Vec<f64> = (0..30).rev().map(f64::from).collect();
        assert_eq!(last(&rsi(&down, 14)), Some(0.0));
        assert_eq!(last(&rsi(&[5.0; 30], 14)), Some(50.0));
        // Bollinger on a flat series collapses to the mean.
        let b = bollinger(&[2.0; 25], 20, 2.0)[24].unwrap();
        assert_eq!(b, (2.0, 2.0, 2.0));
        // ATR of unit-range candles is 1.
        let flat: Vec<Ohlc> = (0..20).map(|_| k(10.0, 10.5, 9.5, 10.0)).collect();
        assert!((last(&atr(&flat, 14)).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn macd_signal_and_histogram_line_up() {
        let v: Vec<f64> = (0..80)
            .map(|i| 100.0 + (i as f64 * 0.3).sin() * 5.0 + i as f64 * 0.1)
            .collect();
        let (m, s, h) = macd(&v);
        assert!(m[24].is_none() && m[25].is_some());
        assert!(s[32].is_none() && s[33].is_some());
        for i in 0..v.len() {
            if let (Some(m), Some(s), Some(h)) = (m[i], s[i], h[i]) {
                assert!((m - s - h).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn pivots_find_swings_and_levels_cluster_touches() {
        // Two equal peaks at 110 and an equal trough pair at 100.
        let closes = [
            100.0, 102.0, 105.0, 110.0, 105.0, 102.0, 100.0, 102.0, 105.0, 110.0, 105.0, 102.0,
            100.0, 102.0, 104.0,
        ];
        let c = series(&closes);
        let (h, l) = pivots(&c, 2);
        assert_eq!(h.iter().map(|p| p.idx).collect::<Vec<_>>(), [3, 9]);
        assert_eq!(l.iter().map(|p| p.idx).collect::<Vec<_>>(), [6, 12]);
        let lv = levels(&h, &l, 0.6);
        assert_eq!(lv.len(), 2);
        assert!(lv.iter().all(|&(_, n)| n == 2));
        assert!((lv.iter().map(|x| x.0).fold(0.0, f64::max) - 110.5).abs() < 1e-9);
    }

    #[test]
    fn candlestick_patterns_are_recognised() {
        // A decline, then a bullish engulfing bar.
        let mut c = series(&[110.0, 108.0, 106.0, 104.0, 102.0, 100.0]);
        c.push(k(99.0, 106.0, 98.5, 105.0)); // opens below the last close, closes above its open
        let p = candle_patterns(&c, 1);
        assert!(
            p.iter()
                .any(|x| x.name == "bullish_engulfing" && x.bias == "bullish"),
            "{p:?}"
        );
        // Hammer after a decline.
        let mut c = series(&[110.0, 108.0, 106.0, 104.0, 102.0, 100.0]);
        c.push(k(99.0, 99.4, 95.0, 99.2));
        let p = candle_patterns(&c, 1);
        assert!(p.iter().any(|x| x.name == "hammer"), "{p:?}");
        // The same shape after a rise is a hanging man.
        let mut c = series(&[90.0, 92.0, 94.0, 96.0, 98.0, 100.0]);
        c.push(k(101.0, 101.4, 97.0, 101.2));
        let p = candle_patterns(&c, 1);
        assert!(
            p.iter()
                .any(|x| x.name == "hanging_man" && x.bias == "bearish"),
            "{p:?}"
        );
        // Doji and inside bar.
        let mut c = series(&[100.0, 101.0, 102.0, 103.0, 104.0, 105.0]);
        c.push(k(104.0, 104.8, 103.7, 104.05));
        let p = candle_patterns(&c, 1);
        assert!(p.iter().any(|x| x.name == "doji"), "{p:?}");
        assert!(p.iter().any(|x| x.name == "inside_bar"), "{p:?}");
        // Morning star: big red, small body, big green above the red's middle.
        let mut c = series(&[110.0, 108.0, 106.0, 104.0, 102.0]);
        c.push(k(102.0, 102.3, 95.0, 96.0)); // big red
        c.push(k(95.5, 96.2, 94.8, 95.7)); // small star
        c.push(k(96.0, 103.0, 95.8, 102.5)); // big green past the red's middle (99)
        let p = candle_patterns(&c, 1);
        assert!(p.iter().any(|x| x.name == "morning_star"), "{p:?}");
        // Nothing to say about a flat market.
        let flat: Vec<Ohlc> = (0..10).map(|_| k(100.0, 100.5, 99.5, 100.2)).collect();
        assert!(candle_patterns(&flat, 3)
            .iter()
            .all(|x| x.bias == "neutral" || x.name == "inside_bar"));
    }

    /// A wave: closes that follow `pts` (anchors) with straight lines.
    fn wave(pts: &[(usize, f64)]) -> Vec<f64> {
        let end = pts.last().unwrap().0;
        (0..=end)
            .map(|i| {
                let j = pts.iter().rposition(|p| p.0 <= i).unwrap();
                if j + 1 == pts.len() {
                    return pts[j].1;
                }
                let (x0, y0) = pts[j];
                let (x1, y1) = pts[j + 1];
                y0 + (y1 - y0) * (i - x0) as f64 / (x1 - x0) as f64
            })
            .collect()
    }

    #[test]
    fn double_top_and_double_bottom_are_found_and_confirmed() {
        // Rise to 120, dip to 100, back to 120, then a close under the neckline.
        let top = wave(&[
            (0, 90.0),
            (12, 120.0),
            (24, 100.0),
            (36, 120.0),
            (46, 95.0),
            (50, 94.0),
        ]);
        let p = chart_patterns(&series(&top));
        let dt = p
            .iter()
            .find(|x| x.name == "double_top")
            .expect("double top");
        assert_eq!(dt.bias, "bearish");
        assert_eq!(dt.confidence, "high", "closed below the neckline");
        let bottom: Vec<f64> = top.iter().map(|x| 210.0 - x).collect();
        let p = chart_patterns(&series(&bottom));
        let db = p
            .iter()
            .find(|x| x.name == "double_bottom")
            .expect("double bottom");
        assert_eq!(db.bias, "bullish");
    }

    #[test]
    fn structure_triangles_and_breakouts() {
        // Higher highs and higher lows.
        let up = wave(&[
            (0, 100.0),
            (8, 110.0),
            (14, 104.0),
            (22, 118.0),
            (28, 111.0),
            (36, 126.0),
            (40, 120.0),
            (44, 124.0),
        ]);
        let p = chart_patterns(&series(&up));
        assert!(p.iter().any(|x| x.name == "uptrend_structure"), "{p:?}");
        // Converging swings: lower highs, higher lows, apex still ahead.
        let tri = wave(&[
            (0, 100.0),
            (6, 120.0),
            (12, 100.0),
            (18, 117.0),
            (24, 102.0),
            (30, 114.0),
            (36, 104.0),
            (42, 111.0),
            (46, 105.0),
            (50, 108.0),
        ]);
        let p = chart_patterns(&series(&tri));
        assert!(
            p.iter()
                .any(|x| x.name.contains("triangle") || x.name.contains("wedge")),
            "{p:?}"
        );
        // A tight base, then a close above its high on heavy volume.
        let mut c = series(
            &[100.0; 35]
                .iter()
                .enumerate()
                .map(|(i, v)| v + (i % 3) as f64 * 0.2)
                .collect::<Vec<_>>(),
        );
        let mut last = k(100.4, 106.0, 100.3, 105.5);
        last.volume = 400.0;
        c.push(last);
        let p = chart_patterns(&c);
        let b = p
            .iter()
            .find(|x| x.name == "breakout_up")
            .expect("breakout");
        assert_eq!(b.confidence, "high");
        // A flat base alone is a range.
        let flat = series(
            &[100.0; 35]
                .iter()
                .enumerate()
                .map(|(i, v)| v + (i % 3) as f64 * 0.2)
                .collect::<Vec<_>>(),
        );
        assert!(chart_patterns(&flat).iter().any(|x| x.name == "range"));
    }

    #[test]
    fn analyze_reports_everything_and_refuses_too_little_data() {
        let closes: Vec<f64> = (0..120)
            .map(|i| 100.0 + i as f64 * 0.5 + (i as f64 * 0.5).sin() * 3.0)
            .collect();
        let r = analyze("TESTUSDT", Timeframe::H1, &series(&closes), i64::MAX / 2).unwrap();
        assert_eq!(r["symbol"], "TESTUSDT");
        assert_eq!(r["timeframe"], "1h");
        assert!(r["indicators"]["sma50"].is_number() && r["indicators"]["rsi14"].is_number());
        assert!(r["indicators"]["sma200"].is_null(), "only 120 candles");
        assert!(
            r["bias"]["score"].as_i64().unwrap() >= 2,
            "a steady climb reads bullish: {}",
            r["bias"]
        );
        assert!(r["summary"].as_array().unwrap().len() >= 2);
        assert!(r["caveat"].as_str().unwrap().contains("heuristic"));
        let e = analyze("X", Timeframe::H1, &series(&closes[..10]), i64::MAX / 2).unwrap_err();
        assert!(e.contains("at least"), "{e}");
    }

    #[test]
    fn a_forming_candle_is_kept_out_of_the_analysis_and_reported_apart() {
        let closes: Vec<f64> = (0..80).map(|i| 100.0 + i as f64 * 0.3).collect();
        let mut c = series(&closes); // candle i opens at i minutes
        let n = c.len();
        // Hourly candles here: the last one (opened at minute 79) is still open
        // 10 "minutes" later; the one before it is long closed.
        let tf = Timeframe::M1;
        let now = c[n - 1].time_ms + 30_000; // 30 s into the last 1m candle
        c[n - 1].close += 50.0; // a wild forming candle must not move the indicators
        let r = analyze("X", tf, &c, now).unwrap();
        assert_eq!(r["candles_used"], n - 1);
        assert!(r["forming_candle"].is_object());
        assert_eq!(r["forming_candle"]["elapsed_pct"], 50.0);
        assert!(r["price_now"].as_f64().unwrap() > 150.0);
        assert!(r["last_close"].as_f64().unwrap() < 130.0);
        // With the clock past the candle's end it is a normal closed candle.
        let r = analyze("X", tf, &c, c[n - 1].time_ms + 61_000).unwrap();
        assert_eq!(r["candles_used"], n);
        assert!(r["forming_candle"].is_null());
    }

    #[test]
    fn multi_timeframe_report_orders_high_to_low_and_judges_agreement() {
        let up: Vec<f64> = (0..120)
            .map(|i| 100.0 + i as f64 * 0.5 + (i as f64 * 0.5).sin() * 3.0)
            .collect();
        let down: Vec<f64> = up.iter().map(|x| 300.0 - x).collect();
        let now = i64::MAX / 2;
        let r = |c: &[f64], tf| (tf, analyze("X", tf, &series(c), now).unwrap());
        let aligned = multi_json(
            "X",
            &[
                r(&up, Timeframe::M5),
                r(&up, Timeframe::D1),
                r(&up, Timeframe::H1),
            ],
        );
        assert_eq!(aligned["order"], json!(["1d", "1h", "5m"]));
        assert!(
            aligned["alignment"]
                .as_str()
                .unwrap()
                .starts_with("aligned bullish"),
            "{}",
            aligned["alignment"]
        );
        assert!(aligned["timeframes"]["1h"]["rsi14"].is_number());
        let mixed = multi_json("X", &[r(&up, Timeframe::D1), r(&down, Timeframe::H1)]);
        let a = mixed["alignment"].as_str().unwrap();
        assert!(
            a.starts_with("conflict") && a.contains("1d") && a.contains("1h"),
            "{a}"
        );
        let one = multi_json("X", &[r(&up, Timeframe::H1)]);
        assert!(
            one["alignment"].as_str().unwrap().starts_with("mixed"),
            "a single timeframe cannot agree"
        );
    }
}
