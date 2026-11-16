//! The Compare page: a lead-lag curve (who moves first), a correlation
//! heat map (how a list moves together), the judge, search and landscape.
//!
//! Every mode starts the same way ([`render_head`]): the question it asks,
//! the answer in one sentence, how to read what is below.  Charts are pixels
//! when the terminal shows images, characters otherwise.
//!
//! Colours say "together" (cyan) and "opposite" (magenta), never bull / bear
//! green and red: a correlation is not a price move.  Bars and cells are
//! solid backgrounds (DESIGN rule 3).

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::Paragraph,
    Frame,
};

use crate::compare::{LeadLag, Matrix};
use crate::gfx::{self, Canvas};
use crate::i18n::pick;
use crate::tokens::Palette;
use crate::widgets::fmt::{render_clipped, truncate, BEAR, BULL};

/// How the answer line is coloured.
#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    /// Evidence found.
    Yes,
    /// No evidence (or a warning).
    No,
    /// A plain statement.
    Plain,
}

/// Words of `text` in lines of at most `w` characters.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let n = cur.chars().count();
        if n > 0 && n + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The top of a Compare mode: the question, the answer, how to read it (up
/// to four wrapped lines), a blank line.  Fills `area` with the page's
/// background first; returns what is left below.
pub fn render_head(
    frame: &mut Frame,
    area: Rect,
    question: &str,
    answer: &str,
    tone: Tone,
    how: &str,
    palette: &Palette,
) -> Rect {
    let bg = Style::default().fg(palette.text).bg(palette.bg);
    render_clipped(frame, ratatui::widgets::Block::default().style(bg), area);
    if area.height == 0 || area.width == 0 {
        return area;
    }
    let w = area.width as usize;
    let mut y = 0u16;
    line(
        frame,
        area,
        y,
        question,
        Style::default().fg(palette.muted).bg(palette.bg),
    );
    y += 1;
    let ink = match tone {
        Tone::Yes => palette.ok,
        Tone::No => palette.warn,
        Tone::Plain => palette.accent,
    };
    for l in wrap(answer, w).into_iter().take(2) {
        line(
            frame,
            area,
            y,
            &l,
            Style::default()
                .fg(ink)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
    }
    for l in wrap(how, w).into_iter().take(4) {
        line(
            frame,
            area,
            y,
            &l,
            Style::default().fg(palette.text).bg(palette.bg),
        );
        y += 1;
    }
    y += 1;
    Rect {
        y: area.y + y.min(area.height),
        height: area.height.saturating_sub(y),
        ..area
    }
}

/// How strong a correlation is, in a word.
pub fn strength(r: f64) -> &'static str {
    match r.abs() {
        a if a >= 0.7 => pick("strong", "сильний"),
        a if a >= 0.4 => pick("moderate", "помірний"),
        a if a >= 0.2 => pick("weak", "слабкий"),
        _ => pick("almost none", "майже немає"),
    }
}

/// The lead-lag answer in plain words, and its tone.
pub fn leadlag_answer(ll: &LeadLag, a: &str, b: &str) -> (String, Tone) {
    let same = |r: f64| {
        if r >= 0.0 {
            pick("same way", "в один бік")
        } else {
            pick("opposite ways", "у різні боки")
        }
    };
    match ll.peak {
        None => (
            pick(
                format!("No link: {a} and {b} move on their own."),
                format!("Зв'язку немає: {a} і {b} рухаються самі по собі."),
            ),
            Tone::No,
        ),
        Some((0, r)) => (
            pick(
                format!(
                    "Together: same candle, {} (r = {r:+.2}, {}). Neither leads.",
                    same(r),
                    strength(r)
                ),
                format!(
                    "Разом: у тій самій свічці, {} (r = {r:+.2}, {}). Ніхто не веде.",
                    same(r),
                    strength(r)
                ),
            ),
            Tone::Plain,
        ),
        Some((k, r)) if k > 0 => (
            pick(
                format!(
                    "{a} leads: {b} follows {k} candle(s) later, {} (r = {r:+.2}, {}).",
                    same(r),
                    strength(r)
                ),
                format!(
                    "{a} веде: {b} іде за ним через {k} св., {} (r = {r:+.2}, {}).",
                    same(r),
                    strength(r)
                ),
            ),
            Tone::Yes,
        ),
        Some((k, r)) => (
            pick(
                format!(
                    "{b} leads: {a} follows {} candle(s) later, {} (r = {r:+.2}, {}).",
                    -k,
                    same(r),
                    strength(r)
                ),
                format!(
                    "{b} веде: {a} іде за ним через {} св., {} (r = {r:+.2}, {}).",
                    -k,
                    same(r),
                    strength(r)
                ),
            ),
            Tone::Yes,
        ),
    }
}

/// Each candle as a point: `a`'s move (x) against `b`'s move in the same
/// candle (y), in %, the least-squares line through them.  Cells only, shaded
/// by how many candles fall in a cell; the line is white, the axes `─ │`.
fn render_scatter(
    frame: &mut Frame,
    area: Rect,
    (an, a): (&str, &[f64]),
    (bn, b): (&str, &[f64]),
    palette: &Palette,
) {
    use crate::compare::{changes, regress};
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let (ca, cb) = (changes(a), changes(b));
    let Some(g) = regress(&ca, &cb) else {
        line(
            frame,
            area,
            0,
            pick("Too few candles.", "Замало свічок."),
            muted,
        );
        return;
    };
    line(
        frame,
        area,
        0,
        &pick(
            format!(
                "Dot = a candle: {an} across, {bn} up, %. Line: {bn} = {:+.2} x {an} {:+.3}; r = {:+.2}, {} candles.",
                g.slope, g.intercept, g.r, g.n
            ),
            format!(
                "Точка = свічка: {an} вздовж, {bn} вгору, %. Лінія: {bn} = {:+.2} x {an} {:+.3}; r = {:+.2}, {} св.",
                g.slope, g.intercept, g.r, g.n
            ),
        ),
        muted,
    );
    let plot = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    if plot.width < 12 || plot.height < 3 {
        return;
    }
    let pts: Vec<(f64, f64)> = ca
        .iter()
        .zip(&cb)
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .map(|(&x, &y)| (x, y))
        .collect();
    // The scale holds all but the wildest 1 % of the moves, so one crash
    // does not squeeze the rest into the middle.
    let reach = |f: fn(&(f64, f64)) -> f64| -> f64 {
        let mut v: Vec<f64> = pts.iter().map(|p| f(p).abs()).collect();
        v.sort_by(|p, q| p.total_cmp(q));
        v.get(v.len().saturating_sub(1) * 99 / 100)
            .copied()
            .unwrap_or(1.0)
            .max(1e-9)
    };
    let (rx, ry) = (reach(|p| p.0), reach(|p| p.1));
    let (w, h) = (plot.width as usize, plot.height as usize);
    let col = |x: f64| ((x / rx + 1.0) / 2.0 * (w as f64 - 1.0)).round();
    let row = |y: f64| ((1.0 - (y / ry + 1.0) / 2.0) * (h as f64 - 1.0)).round();
    let mut count = vec![0u32; w * h];
    for &(x, y) in &pts {
        let (c, r) = (col(x), row(y));
        if c >= 0.0 && r >= 0.0 && (c as usize) < w && (r as usize) < h {
            count[r as usize * w + c as usize] += 1;
        }
    }
    let top = count.iter().copied().max().unwrap_or(1).max(1) as f64;
    let buf = frame.buffer_mut();
    let mut put = |c: usize, r: usize, ch: char, style: Style| {
        if let Some(cell) = buf.cell_mut((plot.x + c as u16, plot.y + r as u16)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    };
    // Axes through zero, under the points.
    let (zc, zr) = (col(0.0).max(0.0) as usize, row(0.0).max(0.0) as usize);
    for c in 0..w {
        put(c, zr.min(h - 1), '─', muted);
    }
    for r in 0..h {
        put(zc.min(w - 1), r, '│', muted);
    }
    for r in 0..h {
        for c in 0..w {
            let n = count[r * w + c];
            if n == 0 {
                continue;
            }
            let shade = match n as f64 / top {
                f if f > 0.5 => Color::LightCyan,
                f if f > 0.15 => Color::Cyan,
                _ => Color::DarkGray,
            };
            put(c, r, ' ', Style::default().bg(shade));
        }
    }
    // The line, a white cell per column where it is inside the plot.
    for c in 0..w {
        let x = (c as f64 / (w as f64 - 1.0) * 2.0 - 1.0) * rx;
        let r = row(g.intercept + g.slope * x);
        if r >= 0.0 && (r as usize) < h {
            put(c, r as usize, ' ', Style::default().bg(Color::White));
        }
    }
}

/// Two series as lines, each in % from its first value, on one scale; the
/// legend and the scale's ends in cells.  `false` when pixels are off.
fn render_price_lines(
    frame: &mut Frame,
    area: Rect,
    (an, a): (&str, &[f64]),
    (bn, b): (&str, &[f64]),
    palette: &Palette,
) -> bool {
    if area.height < 4 || area.width < 20 {
        return false;
    }
    let pct = |v: &[f64]| -> Vec<f64> {
        let first = v.iter().copied().find(|x| x.is_finite() && *x != 0.0);
        match first {
            Some(f) => v.iter().map(|x| (x / f - 1.0) * 100.0).collect(),
            None => Vec::new(),
        }
    };
    let (pa, pb) = (pct(a), pct(b));
    let n = pa.len().min(pb.len());
    let last = |v: &[f64]| {
        v.iter()
            .rev()
            .copied()
            .find(|x| x.is_finite())
            .unwrap_or(0.0)
    };
    let legend = format!(
        "{} ── {an} {:+.1}%   ── {bn} {:+.1}%",
        pick("Price, % from the start:", "Ціна, % від початку:"),
        last(&pa),
        last(&pb)
    );
    let plot = Rect {
        y: area.y + 1,
        height: area.height - 1,
        width: area.width.saturating_sub(8),
        ..area
    };
    let Some((pw, ph)) = gfx::plot_px(plot).filter(|_| n >= 2) else {
        line(
            frame,
            area,
            0,
            &legend,
            Style::default().fg(palette.muted).bg(palette.bg),
        );
        return false;
    };
    let vals = pa[..n]
        .iter()
        .chain(&pb[..n])
        .copied()
        .filter(|v| v.is_finite());
    let (lo, hi) = vals.fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(v), h.max(v)));
    let (lo, hi) = if hi - lo < 1e-9 {
        (lo - 1.0, hi + 1.0)
    } else {
        (lo, hi)
    };
    let mut cv = Canvas::new(pw, ph, palette.bg);
    let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
    let grid = cv.ink(palette.muted);
    let y_of = |v: f64| ((hi - v) / (hi - lo) * (ph as f64 - 1.0)).round() as i64;
    if lo < 0.0 && hi > 0.0 {
        cv.dashed_hline(y_of(0.0), pt, pt as usize, 3 * pt as usize, grid);
    }
    for (series, color) in [(&pa, Color::LightCyan), (&pb, Color::LightMagenta)] {
        let ink = cv.ink(color);
        let mut prev: Option<(i64, i64)> = None;
        for (i, v) in series[..n].iter().enumerate() {
            if !v.is_finite() {
                prev = None;
                continue;
            }
            let p = (
                (i as f64 / (n - 1) as f64 * (pw as f64 - 1.0)).round() as i64,
                y_of(*v),
            );
            if let Some(q) = prev {
                cv.line(q.0, q.1, p.0, p.1, pt, ink);
            }
            prev = Some(p);
        }
    }
    gfx::submit(frame.buffer_mut(), plot, cv);
    // Legend with the lines' own colours, the scale on the right.
    let buf = frame.buffer_mut();
    let mut x = area.x;
    let mut put = |s: &str, st: Style, x: &mut u16, y: u16| {
        for ch in s.chars() {
            if *x >= area.x + area.width {
                break;
            }
            if let Some(c) = buf.cell_mut((*x, y)) {
                c.set_char(ch);
                c.set_style(st);
            }
            *x += 1;
        }
    };
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    put(
        &format!(
            "{}  ",
            pick("Price, % from the start:", "Ціна, % від початку:")
        ),
        muted,
        &mut x,
        area.y,
    );
    put(
        &format!("── {an} {:+.1}%   ", last(&pa)),
        Style::default()
            .fg(Color::LightCyan)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
        &mut x,
        area.y,
    );
    put(
        &format!("── {bn} {:+.1}%", last(&pb)),
        Style::default()
            .fg(Color::LightMagenta)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
        &mut x,
        area.y,
    );
    let ax = plot.x + plot.width + 1;
    let mut x0 = ax;
    put(&format!("{hi:+.1}%"), muted, &mut x0, plot.y);
    let mut x1 = ax;
    put(
        &format!("{lo:+.1}%"),
        muted,
        &mut x1,
        plot.y + plot.height - 1,
    );
    true
}

/// The lead-lag bars in pixels: the grey band of chance, a bar per lag
/// (bright: the strongest one outside the band), the r scale on the right
/// and the lags underneath.  `false` when pixels are off.
fn render_lag_chart(frame: &mut Frame, area: Rect, ll: &LeadLag, palette: &Palette) -> bool {
    if area.height < 5 || area.width < 20 || ll.points.is_empty() {
        return false;
    }
    let plot = Rect {
        height: area.height - 1,
        width: area.width.saturating_sub(8),
        ..area
    };
    let Some((pw, ph)) = gfx::plot_px(plot) else {
        return false;
    };
    // The scale covers what is there (and the band), in tenths: a curve
    // that is all positive does not waste the lower half.
    let (min_r, max_r) = ll
        .points
        .iter()
        .filter_map(|p| p.1)
        .fold((0.0f64, 0.0f64), |(lo, hi), r| (lo.min(r), hi.max(r)));
    let top = ((max_r.max(ll.band * 2.0).max(0.1) * 10.0).ceil() / 10.0).min(1.0);
    let bot = ((min_r.min(-ll.band * 2.0).min(-0.1) * 10.0).floor() / 10.0).max(-1.0);
    let mut cv = Canvas::new(pw, ph, palette.bg);
    let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
    let y_of = |r: f64| ((top - r) / (top - bot) * (ph as f64 - 1.0)).round() as i64;
    let band = cv.ink(Color::DarkGray);
    cv.fill(0, y_of(ll.band), pw as i64, y_of(-ll.band) + 1, band);
    let grid = cv.ink(palette.muted);
    for g in [-1.0, -0.5, 0.5, 1.0] {
        if g > bot && g < top {
            cv.dashed_hline(y_of(g), pt, pt as usize, 3 * pt as usize, grid);
        }
    }
    cv.fill(0, y_of(0.0), pw as i64, y_of(0.0) + pt, grid);
    let np = ll.points.len();
    let slot = pw as f64 / np as f64;
    let bw = ((slot * 0.7).round() as i64).max(1);
    for (i, &(k, r, _)) in ll.points.iter().enumerate() {
        let Some(r) = r else {
            continue;
        };
        let peak = ll.peak.is_some_and(|p| p.0 == k);
        let color = match (r >= 0.0, peak) {
            (true, true) => Color::LightCyan,
            (true, false) => Color::Cyan,
            (false, true) => Color::LightMagenta,
            (false, false) => Color::Magenta,
        };
        let ink = cv.ink(color);
        let x0 = (i as f64 * slot + (slot - bw as f64) / 2.0).round() as i64;
        let (a, b) = (y_of(r), y_of(0.0));
        cv.fill(x0, a.min(b), x0 + bw, a.max(b) + 1, ink);
    }
    gfx::submit(frame.buffer_mut(), plot, cv);
    // Scale on the right, lags underneath (cells).
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let buf = frame.buffer_mut();
    let mut text = |x: u16, y: u16, s: &str, st: Style| {
        for (i, ch) in s.chars().enumerate() {
            let cx = x + i as u16;
            if cx >= area.x + area.width || y >= area.y + area.height {
                break;
            }
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(ch);
                c.set_style(st);
            }
        }
    };
    let ax = plot.x + plot.width + 1;
    let row_of =
        |r: f64| plot.y + ((top - r) / (top - bot) * (plot.height as f64 - 1.0)).round() as u16;
    text(ax, row_of(top), &format!("{top:+.1}"), muted);
    text(ax, row_of(0.0), " 0", muted);
    text(ax, row_of(bot), &format!("{bot:+.1}"), muted);
    let ly = plot.y + plot.height;
    let col_of = |i: usize| plot.x + ((i as f64 + 0.5) * plot.width as f64 / np as f64) as u16;
    let (k0, k1) = (ll.points[0].0, ll.points[np - 1].0);
    for (i, &(k, _, _)) in ll.points.iter().enumerate() {
        let peak = ll.peak.is_some_and(|p| p.0 == k);
        if k == k0 || k == k1 || k == 0 || peak {
            let s = if k == 0 {
                "0".to_string()
            } else {
                format!("{k:+}")
            };
            let st = if peak {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                muted
            };
            let x = col_of(i).saturating_sub(s.chars().count() as u16 / 2);
            text(x, ly, &s, st);
        }
    }
    let l = pick("<- B leads", "<- веде B");
    text(plot.x, ly, l, muted);
    let r = pick("A leads ->", "веде A ->");
    text(
        (plot.x + plot.width).saturating_sub(r.len() as u16),
        ly,
        r,
        muted,
    );
    true
}

/// Background of a correlation: weak ones stay plain.
pub fn heat_color(r: f64) -> Option<Color> {
    match r {
        r if r >= 0.5 => Some(Color::LightCyan),
        r if r >= 0.2 => Some(Color::Cyan),
        r if r <= -0.5 => Some(Color::LightMagenta),
        r if r <= -0.2 => Some(Color::Magenta),
        _ => None,
    }
}

fn line(frame: &mut Frame, area: Rect, dy: u16, text: &str, style: Style) {
    if dy >= area.height {
        return;
    }
    let r = Rect {
        x: area.x,
        y: area.y + dy,
        width: area.width,
        height: 1,
    };
    render_clipped(
        frame,
        Paragraph::new(Span::styled(
            truncate(text, area.width as usize).to_string(),
            style,
        )),
        r,
    );
}

/// "Who moves first?": the answer, the two prices, the lead-lag bars.
/// `series`: the aligned closes of A and B, for the price chart.
pub fn render_leadlag(
    frame: &mut Frame,
    area: Rect,
    ll: &LeadLag,
    a: &str,
    b: &str,
    series: Option<(&[f64], &[f64])>,
    scatter: bool,
    palette: &Palette,
) {
    let (answer, tone) = leadlag_answer(ll, a, b);
    let body = render_head(
        frame,
        area,
        &pick(
            format!("Who moves first, {a} or {b}?"),
            format!("Хто рухається першим, {a} чи {b}?"),
        ),
        &answer,
        tone,
        &pick(
            format!(
                "Bars: how alike the moves are when {b} is shifted k candles. Peak at 0: together; right: {a} leads; left: {b}. Inside the grey band: chance ({} candles).",
                ll.n
            ),
            format!(
                "Стовпці: наскільки схожі рухи, якщо зсунути {b} на k свічок. Пік на 0: разом; праворуч веде {a}; ліворуч {b}. У сірій смузі: випадковість ({} св.).",
                ll.n
            ),
        ),
        palette,
    );
    let (top_h, low_h) = match series {
        Some(_) if body.height >= 16 => (body.height * 2 / 5, body.height - body.height * 2 / 5),
        _ => (0, body.height),
    };
    if let Some((xa, xb)) = series.filter(|_| top_h > 0) {
        let top = Rect {
            height: top_h.saturating_sub(1),
            ..body
        };
        if scatter {
            render_scatter(frame, top, (a, xa), (b, xb), palette);
        } else {
            render_price_lines(frame, top, (a, xa), (b, xb), palette);
        }
    }
    let low = Rect {
        y: body.y + top_h,
        height: low_h,
        ..body
    };
    line(
        frame,
        low,
        0,
        &pick(
            format!("Likeness of moves by shift k ({a} now, {b} k candles later):"),
            format!("Схожість рухів за зсувом k ({a} зараз, {b} за k свічок):"),
        ),
        Style::default().fg(palette.muted).bg(palette.bg),
    );
    let chart = Rect {
        y: low.y + 1,
        height: low.height.saturating_sub(1),
        ..low
    };
    if !render_lag_chart(frame, chart, ll, palette) {
        render_leadlag_cells(frame, chart, ll, a, b, palette);
    }
}

/// The curve in characters (no pixels): one bar per lag k (r of `a` now
/// with `b` k candles later), the ±band dashed, the peak bright.
fn render_leadlag_cells(
    frame: &mut Frame,
    area: Rect,
    ll: &LeadLag,
    a: &str,
    b: &str,
    palette: &Palette,
) {
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let _ = (a, b);
    if area.height < 4 || area.width < 8 || ll.points.is_empty() {
        return;
    }
    let plot = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height - 1,
    };
    let np = ll.points.len() as u16;
    let cw = (plot.width / np).max(1);
    let shown = (plot.width / cw).min(np) as usize;
    // Centre the shown lags on k = 0 when the window is too narrow.
    let skip = (ll.points.len() - shown) / 2;
    let pts = &ll.points[skip..skip + shown];
    let left = plot.x + (plot.width - cw * shown as u16) / 2;
    let max_r = pts
        .iter()
        .filter_map(|p| p.1)
        .fold(0.0f64, |m, r| m.max(r.abs()));
    let scale = ((max_r.max(ll.band * 1.5).max(0.2) * 10.0).ceil() / 10.0).min(1.0);
    let half = (plot.height.saturating_sub(1) / 2).max(1);
    let zero = plot.y + half;
    let row_of = |v: f64| -> i32 { zero as i32 - (v / scale * half as f64).round() as i32 };
    let buf = frame.buffer_mut();
    let mut put = |x: u16, y: i32, ch: char, style: Style| {
        if y < plot.y as i32 || y >= (plot.y + plot.height) as i32 {
            return;
        }
        if let Some(c) = buf.cell_mut((x, y as u16)) {
            c.set_char(ch);
            c.set_style(style);
        }
    };
    // Zero line and the band, under the bars.
    for x in plot.x..plot.x + plot.width {
        put(x, zero as i32, '─', muted);
        put(x, row_of(ll.band), '┄', muted);
        put(x, row_of(-ll.band), '┄', muted);
    }
    for (i, &(k, r, _)) in pts.iter().enumerate() {
        let x0 = left + i as u16 * cw;
        let Some(r) = r else {
            continue;
        };
        let peak = ll.peak.is_some_and(|p| p.0 == k);
        let color = match (r >= 0.0, peak) {
            (true, true) => Color::LightCyan,
            (true, false) => Color::Cyan,
            (false, true) => Color::LightMagenta,
            (false, false) => Color::Magenta,
        };
        let (top, bot) = if r >= 0.0 {
            (row_of(r), zero as i32)
        } else {
            (zero as i32, row_of(r))
        };
        let bar_w = if cw >= 3 { cw - 1 } else { cw };
        for y in top..=bot {
            for x in x0..x0 + bar_w {
                put(x, y, ' ', Style::default().bg(color));
            }
        }
    }
    // Lag labels: both ends, zero, the peak.
    let ly = (plot.y + plot.height) as i32;
    // The label row is under the plot: written without `put`'s plot check.
    let mut put = |x: u16, y: i32, ch: char, style: Style| {
        if let Some(c) = buf.cell_mut((x, y as u16)) {
            c.set_char(ch);
            c.set_style(style);
        }
    };
    if ly < (area.y + area.height) as i32 {
        let mut label = |k: i32, style: Style| {
            if let Some(i) = pts.iter().position(|p| p.0 == k) {
                let x = left + i as u16 * cw;
                for (j, ch) in format!("{k:+}").replace("+0", "0").chars().enumerate() {
                    let cx = x + j as u16;
                    if cx < area.x + area.width {
                        put(cx, ly, ch, style);
                    }
                }
            }
        };
        let (k0, k1) = (pts[0].0, pts[pts.len() - 1].0);
        label(k0, text);
        label(0, text);
        label(k1, text);
        if let Some((k, _)) = ll.peak {
            label(
                k,
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
}

/// "Which symbols move together?": the answer (the most and least alike
/// pair, the average), then the map.
pub fn render_heat_page(frame: &mut Frame, area: Rect, m: &Matrix, list: &str, palette: &Palette) {
    let n = m.names.len();
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            if let Some(r) = m.r.get(i).and_then(|row| row.get(j)).copied().flatten() {
                pairs.push((r, i, j));
            }
        }
    }
    let short = |s: &str| s.trim_end_matches("USDT").to_string();
    // Groups that move as one, shown as blocks along the diagonal.
    let (groups, order) = m.groups(GROUP_R);
    let shown = m.reordered(&order);
    let group_words: Vec<String> = groups
        .iter()
        .map(|g| {
            let names: Vec<String> = g.iter().map(|&i| short(&m.names[i])).collect();
            let r = m.reordered(g);
            let mut v = Vec::new();
            for i in 0..g.len() {
                for j in i + 1..g.len() {
                    if let Some(x) = r.r[i][j] {
                        v.push(x);
                    }
                }
            }
            let avg = v.iter().sum::<f64>() / v.len().max(1) as f64;
            format!("{} (r {avg:+.2})", names.join(" "))
        })
        .collect();
    let answer = if pairs.is_empty() {
        pick(
            format!("Not enough data: {list} needs 2+ symbols with candles."),
            format!("Замало даних: у {list} треба 2+ символи зі свічками."),
        )
    } else {
        let avg = pairs.iter().map(|p| p.0).sum::<f64>() / pairs.len() as f64;
        let hi = pairs.iter().max_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
        let lo = pairs.iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
        let groups_text = if group_words.is_empty() {
            pick(
                format!("No group moves as one (r >= {GROUP_R:.1}). "),
                format!("Груп, що рухаються разом, немає (r >= {GROUP_R:.1}). "),
            )
        } else {
            pick(
                format!("As one: {}. ", group_words.join("; ")),
                format!("Разом: {}. ", group_words.join("; ")),
            )
        };
        format!(
            "{groups_text}{} {} (r = {avg:+.2}); {} {}-{} ({:+.2}), {} {}-{} ({:+.2}).",
            pick("On average", "У середньому"),
            strength(avg),
            pick("closest", "найближчі"),
            short(&m.names[hi.1]),
            short(&m.names[hi.2]),
            hi.0,
            pick("furthest", "найдальші"),
            short(&m.names[lo.1]),
            short(&m.names[lo.2]),
            lo.0
        )
    };
    let rest = render_head(
        frame,
        area,
        &pick(
            format!("Which coins of {list} move together?"),
            format!("Які монети з {list} рухаються разом?"),
        ),
        &answer,
        Tone::Plain,
        pick(
            "Cell: how alike two coins' moves are (-1..+1). Cyan: together; magenta: opposite; plain: unrelated. A group is one risk, not several.",
            "Клітинка: наскільки схожі рухи двох монет (-1..+1). Блакитний: разом; пурпуровий: навпаки; без кольору: не пов'язані. Група це один ризик, а не кілька.",
        ),
        palette,
    );
    render_heatmap(frame, rest, &shown, palette);
}

/// Average r at which symbols count as one group.
pub const GROUP_R: f64 = 0.6;

/// "Which simple rules change the odds?": the answer, then the judged list.
pub fn render_search_page(
    frame: &mut Frame,
    area: Rect,
    r: &crate::search::Report,
    sel: usize,
    list: &str,
    palette: &Palette,
) {
    let (answer, tone) = if r.survivors > 0 {
        (
            pick(
                format!(
                    "{} of {} ideas hold on data they never saw (green).",
                    r.survivors, r.tested
                ),
                format!(
                    "{} з {} ідей тримаються на даних, яких не бачили (зелені).",
                    r.survivors, r.tested
                ),
            ),
            Tone::Yes,
        )
    } else {
        (
            pick(
                format!(
                    "None of {} ideas holds: the best look is what chance gives among so many.",
                    r.tested
                ),
                format!(
                    "Жодна з {} ідей не тримається: найкращі це те, що дає випадок серед такої кількості.",
                    r.tested
                ),
            ),
            Tone::No,
        )
    };
    let rest = render_head(
        frame,
        area,
        &pick(
            format!("Which rules on {list} ({} coins) predict a rise?", r.symbols.len()),
            format!("Які правила на {list} ({} монет) передбачають зростання?", r.symbols.len()),
        ),
        &answer,
        tone,
        pick(
            "Row = idea \"after X, is Y higher h candles later?\". Picked on the older 70%; columns show the newer 30% and chance. Up/Down: choose, Enter: full test.",
            "Рядок = ідея «після X, чи Y вище через h свічок?». Знайдено на старших 70%; колонки: новіші 30% і випадок. Вгору/вниз: вибір, Enter: повна перевірка.",
        ),
        palette,
    );
    // The chosen idea's moves under the list, when there is room.
    let sel = sel.min(r.hits.len().saturating_sub(1));
    let chosen = r
        .hits
        .get(sel)
        .and_then(|h| h.verdict.as_ref().map(|v| (h, v)));
    match chosen {
        Some((h, v)) if rest.height >= 26 => {
            let chart_h = (rest.height / 3).clamp(10, 16);
            let list = Rect {
                height: rest.height - chart_h - 1,
                ..rest
            };
            render_search(frame, list, r, sel, palette);
            let chart = Rect {
                y: rest.y + list.height + 1,
                height: chart_h,
                ..rest
            };
            line(
                frame,
                chart,
                0,
                &pick(
                    format!(
                        "Chosen: after {} -> {} in {} candle(s)",
                        h.q.cond.replace("USDT", ""),
                        h.q.target.replace("USDT", ""),
                        h.q.h
                    ),
                    format!(
                        "Вибрано: після {} -> {} через {} св.",
                        h.q.cond.replace("USDT", ""),
                        h.q.target.replace("USDT", ""),
                        h.q.h
                    ),
                ),
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            );
            render_moves_chart(
                frame,
                Rect {
                    y: chart.y + 1,
                    height: chart.height - 1,
                    ..chart
                },
                v,
                palette,
            );
        }
        _ => render_search(frame, rest, r, sel, palette),
    }
}

/// The heat map: every pair's correlation, coloured by strength.
pub fn render_heatmap(frame: &mut Frame, area: Rect, m: &Matrix, palette: &Palette) {
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    if m.names.is_empty() {
        line(
            frame,
            area,
            0,
            pick("The active list is empty.", "Активний список порожній."),
            muted,
        );
        return;
    }
    const CELL: u16 = 6;
    let label_w = m
        .names
        .iter()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 10) as u16
        + 1;
    let fit_cols = (area.width.saturating_sub(label_w) / CELL) as usize;
    let fit_rows = area.height.saturating_sub(3) as usize;
    let n = m.names.len().min(fit_cols).min(fit_rows);
    let short = |s: &str, w: usize| truncate(s.trim_end_matches("USDT"), w).to_string();
    let mut head = " ".repeat(label_w as usize);
    for name in m.names.iter().take(n) {
        head.push_str(&format!("{:>6}", short(name, 5)));
    }
    line(frame, area, 0, &head, muted);
    let buf = frame.buffer_mut();
    for i in 0..n {
        let y = area.y + 1 + i as u16;
        let label = format!(
            "{:<w$}",
            short(&m.names[i], label_w as usize - 1),
            w = label_w as usize
        );
        for (j, ch) in label.chars().enumerate() {
            if let Some(c) = buf.cell_mut((area.x + j as u16, y)) {
                c.set_char(ch);
                c.set_style(text);
            }
        }
        for j in 0..n {
            let x = area.x + label_w + j as u16 * CELL;
            let (cell, style) = match m.r[i][j] {
                _ if i == j => ("    . ".to_string(), muted),
                Some(r) => {
                    let s = format!("{r:>+5.2} ");
                    match heat_color(r) {
                        Some(bg) => (s, Style::default().fg(Color::Black).bg(bg)),
                        None => (s, text),
                    }
                }
                None => ("    - ".to_string(), muted),
            };
            for (k, ch) in cell.chars().enumerate() {
                if let Some(c) = buf.cell_mut((x + k as u16, y)) {
                    c.set_char(ch);
                    c.set_style(style);
                }
            }
        }
    }
    let more = if n < m.names.len() {
        pick(
            format!(" | {} of {} shown (widen the window)", n, m.names.len()),
            format!(" | показано {} з {} (збільш вікно)", n, m.names.len()),
        )
    } else {
        String::new()
    };
    line(
        frame,
        area,
        n as u16 + 2,
        &pick(
            format!(
                "cyan: together, magenta: opposite | fewest shared candles {}{more}",
                m.min_pairs
            ),
            format!(
                "блакитний: разом, пурпуровий: навпаки | мінімум спільних свічок {}{more}",
                m.min_pairs
            ),
        ),
        muted,
    );
}

/// The judge's report: the verdict in words, then the older and the newer
/// part side by side, the chaos baseline and the number of questions.
pub fn render_verdict(
    frame: &mut Frame,
    area: Rect,
    v: &crate::judge::Verdict,
    cond: &str,
    target: &str,
    tf: &str,
    palette: &Palette,
) {
    render_verdict_table(frame, area, v, cond, target, tf, false, palette)
}

/// "Does this condition change the odds?": the answer, the two
/// distributions of moves, then the numbers.
pub fn render_judge(
    frame: &mut Frame,
    area: Rect,
    v: &crate::judge::Verdict,
    cond: &str,
    target: &str,
    tf: &str,
    palette: &Palette,
) {
    let pctf = |x: f64| format!("{:.0}%", x * 100.0);
    let (answer, tone) = if v.survives {
        (
            pick(
                format!(
                    "Holds: after {cond}, {target} was higher {} candle(s) later in {} of cases on unseen data (usual {}).",
                    v.horizon,
                    pctf(v.newer.hit),
                    pctf(v.newer.base_hit)
                ),
                format!(
                    "Тримається: після {cond} {target} було вище через {} св. у {} випадків на небачених даних (звичайно {}).",
                    v.horizon,
                    pctf(v.newer.hit),
                    pctf(v.newer.base_hit)
                ),
            ),
            Tone::Yes,
        )
    } else {
        (
            pick(
                format!("No evidence: {}.", v.reason),
                format!("Доказів немає: {}.", v.reason),
            ),
            Tone::No,
        )
    };
    let body = render_head(
        frame,
        area,
        &pick(
            format!("After {cond}, is {target} higher in {} candle(s) ({tf}) more often than usual?", v.horizon),
            format!("Після {cond} чи {target} вище через {} св. ({tf}) частіше, ніж звичайно?", v.horizon),
        ),
        &answer,
        tone,
        &pick(
            format!(
                "Chart: {target}'s move {} candle(s) later. Grey: usual. Colour: after the event; right of grey means a rise is likelier. Numbers: older 70%, newer 30%, chance.",
                v.horizon
            ),
            format!(
                "Графік: рух {target} через {} св. Сірий: звичайно. Колір: після події; праворуч від сірого зростання ймовірніше. Цифри: старші 70%, новіші 30%, випадок.",
                v.horizon
            ),
        ),
        palette,
    );
    let chart_h = if body.height >= 24 {
        body.height - 14
    } else {
        body.height / 2
    };
    let chart = Rect {
        height: chart_h,
        ..body
    };
    render_moves_chart(frame, chart, v, palette);
    let rest = Rect {
        y: body.y + chart_h + 1,
        height: body.height.saturating_sub(chart_h + 1),
        ..body
    };
    render_verdict_table(frame, rest, v, cond, target, tf, true, palette);
}

/// Two histograms of the move h candles later: every candle (grey) and
/// after the event (green right of 0, red left), each as a share of its own
/// count, on one axis.  Characters show the two means instead.
fn render_moves_chart(frame: &mut Frame, area: Rect, v: &crate::judge::Verdict, palette: &Palette) {
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let mean = |x: &[f64]| x.iter().sum::<f64>() / x.len().max(1) as f64;
    let head = pick(
        format!(
            "Move in {} candle(s): after the event {} times, mean {:+.2}% | usual {} candles, mean {:+.2}%",
            v.horizon,
            v.moves.len(),
            mean(&v.moves),
            v.usual.len(),
            mean(&v.usual)
        ),
        format!(
            "Рух через {} св.: після події {} разів, середнє {:+.2}% | звичайно {} свічок, середнє {:+.2}%",
            v.horizon,
            v.moves.len(),
            mean(&v.moves),
            v.usual.len(),
            mean(&v.usual)
        ),
    );
    line(frame, area, 0, &head, muted);
    if area.height < 5 || area.width < 20 || v.usual.len() < 10 {
        return;
    }
    let plot = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    let Some((pw, ph)) = gfx::plot_px(plot) else {
        return;
    };
    let mut sorted = v.usual.clone();
    sorted.sort_by(f64::total_cmp);
    let q = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let lim = q(0.02).abs().max(q(0.98).abs()).max(1e-6);
    let (lo, hi) = (-lim, lim);
    // Fewer events, wider bins: 73 events in 41 bins is a comb, not a shape.
    let bins = match v.moves.len() {
        0..=99 => 21,
        100..=399 => 31,
        _ => 41,
    };
    let bin_of =
        |x: f64| (((x - lo) / (hi - lo) * bins as f64) as i64).clamp(0, bins as i64 - 1) as usize;
    let hist = |xs: &[f64]| {
        let mut h = vec![0f64; bins];
        for &x in xs {
            h[bin_of(x)] += 1.0;
        }
        let n = xs.len().max(1) as f64;
        h.into_iter().map(|c| c / n).collect::<Vec<f64>>()
    };
    let (hu, he) = (hist(&v.usual), hist(&v.moves));
    let top = hu.iter().chain(he.iter()).fold(1e-9f64, |m, x| m.max(*x));
    let mut cv = Canvas::new(pw, ph, palette.bg);
    let grey = cv.ink(Color::DarkGray);
    let (up, down) = (cv.ink(BULL), cv.ink(BEAR));
    let zero = cv.ink(palette.text);
    let slot = pw as f64 / bins as f64;
    let h_of = |share: f64| (share / top * (ph as f64 - 2.0)).round() as i64;
    for i in 0..bins {
        let x0 = (i as f64 * slot).round() as i64;
        let x1 = ((i + 1) as f64 * slot).round() as i64 - 1;
        cv.fill(x0, ph as i64 - h_of(hu[i]), x1.max(x0 + 1), ph as i64, grey);
        if he[i] > 0.0 {
            let mid = lo + (i as f64 + 0.5) * (hi - lo) / bins as f64;
            let w = ((x1 - x0) / 4).max(1);
            cv.fill(
                x0 + w,
                ph as i64 - h_of(he[i]),
                (x1 - w).max(x0 + w + 1),
                ph as i64,
                if mid >= 0.0 { up } else { down },
            );
        }
    }
    let x0 = ((0.0 - lo) / (hi - lo) * pw as f64).round() as i64;
    let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
    cv.fill(x0, 0, x0 + pt, ph as i64, zero);
    gfx::submit(frame.buffer_mut(), plot, cv);
    let ly = plot.y + plot.height;
    line(
        frame,
        Rect {
            y: ly,
            height: 1,
            ..area
        },
        0,
        &format!("{lo:+.1}%"),
        muted,
    );
    let mid = pick(
        format!("0  (beyond +-{lim:.1}% at the edges)"),
        format!("0  (понад +-{lim:.1}% на краях)"),
    );
    let mx = (area.x + area.width / 2).saturating_sub(0);
    line(
        frame,
        Rect {
            x: mx,
            y: ly,
            width: area.width / 2,
            height: 1,
        },
        0,
        &mid,
        muted,
    );
    let r = format!("{hi:+.1}%");
    let rx = (area.x + area.width).saturating_sub(r.len() as u16);
    line(
        frame,
        Rect {
            x: rx,
            y: ly,
            width: r.len() as u16,
            height: 1,
        },
        0,
        &r,
        muted,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_verdict_table(
    frame: &mut Frame,
    area: Rect,
    v: &crate::judge::Verdict,
    cond: &str,
    target: &str,
    tf: &str,
    brief: bool,
    palette: &Palette,
) {
    // Brief: the answer is already above; only the table and the chaos line.
    let top: u16 = if brief { 0 } else { 3 };
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let (head, head_style) = if v.survives {
        (
            pick("HOLDS", "ТРИМАЄТЬСЯ"),
            Style::default()
                .fg(palette.black)
                .bg(palette.ok)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            pick("NO EVIDENCE", "ДОКАЗІВ НЕМАЄ"),
            Style::default()
                .fg(palette.black)
                .bg(palette.warn)
                .add_modifier(Modifier::BOLD),
        )
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    let w = head.chars().count() as u16 + 2;
    if !brief {
        render_clipped(
            frame,
            Paragraph::new(Span::styled(format!(" {head} "), head_style)),
            Rect {
                width: w.min(area.width),
                height: 1,
                ..area
            },
        );
        render_clipped(
            frame,
            Paragraph::new(Span::styled(format!(" {}", v.reason), text)),
            Rect {
                x: area.x + w.min(area.width),
                width: area.width.saturating_sub(w),
                height: 1,
                ..area
            },
        );
        line(
            frame,
            area,
            1,
            &pick(
                format!(
                    "After {cond}: is {target} higher in {} candle(s) ({tf})?",
                    v.horizon
                ),
                format!(
                    "Після {cond}: чи {target} вище через {} св. ({tf})?",
                    v.horizon
                ),
            ),
            muted,
        );
    }
    let pct = |x: f64| format!("{:.0}%", x * 100.0);
    let rows: Vec<(&str, String, String)> = vec![
        (
            "",
            pick("older 70% (found here)", "старші 70% (знайдено тут)").into(),
            pick("newer 30% (unseen)", "новіші 30% (небачені)").into(),
        ),
        (
            pick("events", "подій"),
            v.older.events.to_string(),
            v.newer.events.to_string(),
        ),
        (
            pick("P(higher)", "P(вище)"),
            format!(
                "{} [{}-{}]",
                pct(v.older.hit),
                pct(v.older.hit_ci.0),
                pct(v.older.hit_ci.1)
            ),
            format!(
                "{} [{}-{}]",
                pct(v.newer.hit),
                pct(v.newer.hit_ci.0),
                pct(v.newer.hit_ci.1)
            ),
        ),
        (
            pick("usual P(higher)", "звичайна P(вище)"),
            pct(v.older.base_hit),
            pct(v.newer.base_hit),
        ),
        ("", String::new(), String::new()),
        (
            pick("mean move", "середній рух"),
            format!(
                "{:+.2}% [{:+.2}..{:+.2}]",
                v.older.mean, v.older.mean_ci.0, v.older.mean_ci.1
            ),
            format!(
                "{:+.2}% [{:+.2}..{:+.2}]",
                v.newer.mean, v.newer.mean_ci.0, v.newer.mean_ci.1
            ),
        ),
        (
            pick("usual move", "звичайний рух"),
            format!("{:+.2}%", v.older.base_mean),
            format!("{:+.2}%", v.newer.base_mean),
        ),
        (
            pick("candles", "свічок"),
            v.older.candles.to_string(),
            v.newer.candles.to_string(),
        ),
    ];
    const LABEL: usize = 17;
    let col = ((area.width as usize).saturating_sub(LABEL) / 2).max(1);
    for (i, (label, a, b)) in rows.iter().enumerate() {
        let y = top + i as u16;
        line(
            frame,
            area,
            y,
            &format!(
                "{label:<LABEL$}{:<col$}{}",
                truncate(a, col.saturating_sub(1)),
                b
            ),
            if i == 0 { muted } else { text },
        );
    }
    // The odds as bars: interval, point, the usual rate.
    let bar_y = top + 4;
    if bar_y < area.height {
        let y = area.y + bar_y;
        let bw = col.saturating_sub(4).min(40) as u16;
        let buf = frame.buffer_mut();
        for (k, s) in [&v.older, &v.newer].iter().enumerate() {
            let x0 = area.x + LABEL as u16 + (k * col) as u16;
            if bw < 5 || x0 + bw > area.x + area.width || s.events == 0 {
                continue;
            }
            let at = |p: f64| x0 + ((p.clamp(0.0, 1.0)) * (bw - 1) as f64).round() as u16;
            for x in x0..x0 + bw {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_char('┄');
                    c.set_style(muted);
                }
            }
            let ink = if s.clear_of_base() {
                palette.ok
            } else {
                palette.warn
            };
            for x in at(s.hit_ci.0)..=at(s.hit_ci.1) {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_char('─');
                    c.set_style(Style::default().fg(ink).bg(palette.bg));
                }
            }
            if let Some(c) = buf.cell_mut((at(s.base_hit), y)) {
                c.set_char('│');
                c.set_style(text);
            }
            if let Some(c) = buf.cell_mut((at(s.hit), y)) {
                c.set_char('●');
                c.set_style(
                    Style::default()
                        .fg(ink)
                        .bg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
        let label = pick("odds 0-100%", "шанс 0-100%");
        for (j, ch) in label.chars().enumerate() {
            if let Some(c) = buf.cell_mut((area.x + j as u16, y)) {
                c.set_char(ch);
                c.set_style(muted);
            }
        }
    }
    let n = rows.len() as u16 + top + 1;
    line(
        frame,
        area,
        n,
        &pick(
            format!(
                "chance: a shifted condition did as well in {} of {} tries | threshold 5% / {} = {}",
                crate::judge::pct(v.p_chaos),
                v.shifts,
                v.tries,
                crate::judge::pct(v.alpha)
            ),
            format!(
                "випадок: зсунута умова так само добра в {} з {} спроб | поріг 5% / {} = {}",
                crate::judge::pct(v.p_chaos),
                v.shifts,
                v.tries,
                crate::judge::pct(v.alpha)
            ),
        ),
        text,
    );
    line(
        frame,
        area,
        n + 1,
        pick(
            "| usual, ● after the event, ── 95% range | no fees; events are at least h apart",
            "| │ звичайно, ● після події, ── 95% діапазон | без комісій; події не ближче h свічок",
        ),
        muted,
    );
}

/// Search results: what was asked and how strictly, then the judged best
/// (survivors first), one row each; `sel` is highlighted.
pub fn render_search(
    frame: &mut Frame,
    area: Rect,
    r: &crate::search::Report,
    sel: usize,
    palette: &Palette,
) {
    use crate::judge::pct;
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let alpha = 0.05 / r.tested.max(1) as f64;
    line(
        frame,
        area,
        0,
        &pick(
            format!(
                "{} hold of {} questions on {} coins ({} ms) | {} had >= {} events | top {} judged at 5% / {} = {}",
                r.survivors,
                r.tested,
                r.symbols.len(),
                r.elapsed_ms,
                r.rankable,
                crate::judge::MIN_EVENTS,
                r.hits.len(),
                r.tested,
                pct(alpha)
            ),
            format!(
                "{} тримаються з {} питань на {} монетах ({} мс) | {} мали >= {} подій | топ {} за порогом 5% / {} = {}",
                r.survivors,
                r.tested,
                r.symbols.len(),
                r.elapsed_ms,
                r.rankable,
                crate::judge::MIN_EVENTS,
                r.hits.len(),
                r.tested,
                pct(alpha)
            ),
        ),
        Style::default()
            .fg(palette.accent)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD),
    );
    line(
        frame,
        area,
        1,
        &format!(
            "{:<7}{:<34}{:<10}{:>4}  {:>9}  {:<26}{}",
            "",
            pick("when", "коли"),
            pick("target", "ціль"),
            "h",
            pick("events", "подій"),
            pick("newer: odds vs usual", "нове: шанс і звичайний"),
            pick("chance", "випадок")
        ),
        muted,
    );
    let rows = area.height.saturating_sub(3) as usize;
    let sel = sel.min(r.hits.len().saturating_sub(1));
    let start = sel.saturating_sub(rows.saturating_sub(1));
    for (i, h) in r.hits.iter().enumerate().skip(start).take(rows) {
        let short = |s: &str| s.replace("USDT", "");
        let (mark, events, odds, chance) = match &h.verdict {
            Some(v) => (
                if v.survives {
                    pick("HOLDS", "ТРИМ.")
                } else {
                    pick("no", "ні")
                },
                format!("{}/{}", v.older.events, v.newer.events),
                // The bar is painted over these 16 spaces below.
                format!(
                    "{:16}{:.0}%/{:.0}%",
                    "",
                    v.newer.hit * 100.0,
                    v.newer.base_hit * 100.0
                ),
                pct(v.p_chaos),
            ),
            None => (
                "?",
                h.older_events.to_string(),
                String::new(),
                String::new(),
            ),
        };
        let row = format!(
            " {:<6}{:<34}{:<10}{:>4}  {:>9}  {:<26}{}",
            mark,
            truncate(&short(&h.q.cond), 33),
            short(&h.q.target),
            h.q.h,
            events,
            odds,
            chance
        );
        let style = if i == sel {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else if h.verdict.as_ref().is_some_and(|v| v.survives) {
            Style::default()
                .fg(palette.ok)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            text
        };
        let ry = 2 + (i - start) as u16;
        line(frame, area, ry, &row, style);
        // The odds of the newer part as a bar: ┄ 0-100%, │ the usual odds,
        // ── the 95% interval, ● the odds after the event.
        if let Some(v) = h.verdict.as_ref().filter(|v| v.newer.events > 0) {
            let bx = area.x + 1 + 6 + 34 + 10 + 4 + 2 + 9 + 2;
            const BW: u16 = 14;
            if bx + BW < area.x + area.width && ry < area.height {
                let y = area.y + ry;
                let at = |p: f64| bx + (p.clamp(0.0, 1.0) * (BW - 1) as f64).round() as u16;
                let ink = if v.newer.clear_of_base() {
                    palette.ok
                } else {
                    palette.warn
                };
                let bg = if i == sel {
                    palette.cursor_bg
                } else {
                    palette.bg
                };
                let buf = frame.buffer_mut();
                let mut put = |x: u16, ch: char, st: Style| {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.set_char(ch);
                        c.set_style(st);
                    }
                };
                for x in bx..bx + BW {
                    put(x, '┄', Style::default().fg(palette.muted).bg(bg));
                }
                for x in at(v.newer.hit_ci.0)..=at(v.newer.hit_ci.1) {
                    put(x, '─', Style::default().fg(ink).bg(bg));
                }
                put(
                    at(v.newer.base_hit),
                    '│',
                    Style::default().fg(palette.text).bg(bg),
                );
                put(
                    at(v.newer.hit),
                    '●',
                    Style::default().fg(ink).bg(bg).add_modifier(Modifier::BOLD),
                );
            }
        }
    }
    line(
        frame,
        area,
        area.height.saturating_sub(1),
        pick(
            "Up/Down: choose, Enter: full test | bar: │ usual, ● after event, ── 95% range (green: clear of usual)",
            "Вгору/вниз: вибір, Enter: повна перевірка | смуга: │ звичайно, ● після події, ── 95% діапазон (зелений: поза звичайним)",
        ),
        muted,
    );
}

/// The search landscape: what the best setting is, how its neighbourhood
/// looks, then the older, newer and chaos maps side by side (as many as
/// fit).  `cursor` is the inspected cell `(x, y)`.
#[allow(clippy::too_many_arguments)]
pub fn render_landscape(
    frame: &mut Frame,
    area: Rect,
    l: &crate::landscape::Landscape,
    cursor: (usize, usize),
    tf: &str,
    palette: &Palette,
) {
    render_landscape_maps(frame, area, l, cursor, tf, false, palette)
}

/// "Where does the rule work?": the answer, then the best setting, its
/// shape and the three maps.
pub fn render_landscape_page(
    frame: &mut Frame,
    area: Rect,
    l: &crate::landscape::Landscape,
    cursor: (usize, usize),
    tf: &str,
    palette: &Palette,
) {
    let (answer, tone) = match &l.verdict {
        Some(v) if v.survives => (
            pick(
                format!("Holds at the best setting; the map shows {}.", l.shape()),
                format!(
                    "Тримається на найкращому налаштуванні; карта: {}.",
                    l.shape()
                ),
            ),
            Tone::Yes,
        ),
        Some(v) => (
            pick(
                format!("No evidence: {}. Map: {}.", v.reason, l.shape()),
                format!("Доказів немає: {}. Карта: {}.", v.reason, l.shape()),
            ),
            Tone::No,
        ),
        None => (
            pick(
                "No setting has enough events: widen the ranges or the history.",
                "Жодне налаштування не має досить подій: розшир діапазони або історію.",
            )
            .to_string(),
            Tone::No,
        ),
    };
    let rest = render_head(
        frame,
        area,
        &pick(
            format!(
                "Does '{}' work for {} ({} candle(s) later, {tf}) on many settings, or one lucky one?",
                l.grid.text(),
                l.target,
                l.horizon
            ),
            format!(
                "Чи працює '{}' для {} (через {} св., {tf}) на багатьох налаштуваннях чи лише на одному щасливому?",
                l.grid.text(),
                l.target,
                l.horizon
            ),
        ),
        &answer,
        tone,
        pick(
            "Cell = a setting: how much likelier a rise is after the event, in points. Left: older 70%. Middle: newer 30%. Right: chance. A real rule is a patch of colour that returns in the middle map; one bright cell is luck.",
            "Клітинка = налаштування: наскільки зростання ймовірніше після події, у пунктах. Ліворуч: старші 70%. Посередині: новіші 30%. Праворуч: випадок. Справжнє правило це пляма кольору, що повторюється посередині; одна яскрава клітинка це удача.",
        ),
        palette,
    );
    render_landscape_maps(frame, rest, l, cursor, tf, true, palette)
}

#[allow(clippy::too_many_arguments)]
fn render_landscape_maps(
    frame: &mut Frame,
    area: Rect,
    l: &crate::landscape::Landscape,
    cursor: (usize, usize),
    tf: &str,
    brief: bool,
    palette: &Palette,
) {
    // Brief: the answer is above; skip the badge and the question rows.
    let o: u16 = if brief { 2 } else { 0 };
    use crate::judge::{pct, MIN_EVENTS};
    use crate::landscape::num;
    use crate::widgets::fmt::{BEAR, BULL};
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    if area.height == 0 || area.width == 0 {
        return;
    }
    let (survives, reason) = match &l.verdict {
        Some(v) => (v.survives, v.reason.clone()),
        None => (
            false,
            pick(
                "no setting has enough events",
                "жодне налаштування не має досить подій",
            )
            .to_string(),
        ),
    };
    let (head, ink) = if survives {
        (pick("HOLDS", "ТРИМАЄТЬСЯ"), palette.ok)
    } else {
        (pick("NO EVIDENCE", "ДОКАЗІВ НЕМАЄ"), palette.warn)
    };
    let w = head.chars().count() as u16 + 2;
    let (gw, gh) = (l.grid.width(), l.grid.height());
    let cells = l.cells.iter().flatten().flatten().count();
    if !brief {
        render_clipped(
            frame,
            Paragraph::new(Span::styled(
                format!(" {head} "),
                Style::default()
                    .fg(palette.black)
                    .bg(ink)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect {
                width: w.min(area.width),
                height: 1,
                ..area
            },
        );
        render_clipped(
            frame,
            Paragraph::new(Span::styled(
                format!(" {}: {reason}", pick("best setting", "найкраще")),
                text,
            )),
            Rect {
                x: area.x + w.min(area.width),
                width: area.width.saturating_sub(w),
                height: 1,
                ..area
            },
        );
        line(
            frame,
            area,
            1,
            &pick(
                format!(
                    "After {}: is {} higher in {} candle(s) ({tf})? | {cells} settings, {} ms",
                    l.grid.text(),
                    l.target,
                    l.horizon,
                    l.elapsed_ms
                ),
                format!(
                    "Після {}: чи {} вище через {} св. ({tf})? | {cells} налаштувань, {} мс",
                    l.grid.text(),
                    l.target,
                    l.horizon,
                    l.elapsed_ms
                ),
            ),
            muted,
        );
    }
    let pp = |e: f64| format!("{:+.0} pp", e * 100.0);
    let best_line = match l.best.and_then(|(x, y)| Some((x, y, l.cell(x, y)?))) {
        Some((x, y, c)) => pick(
            format!(
                "Best on older: {}  {} ({} ev.) -> newer {} ({} ev.) | a shifted grid does as well {} of {}",
                l.grid.at(x, y),
                pp(c.older.1),
                c.older.0,
                pp(c.newer.1),
                c.newer.0,
                pct(l.p_best),
                l.shifts
            ),
            format!(
                "Найкраще на старій: {}  {} ({} под.) -> нова {} ({} под.) | зсунута сітка так само добра в {} з {}",
                l.grid.at(x, y),
                pp(c.older.1),
                c.older.0,
                pp(c.newer.1),
                c.newer.0,
                pct(l.p_best),
                l.shifts
            ),
        ),
        None => pick(
            format!("No setting has {MIN_EVENTS} events on the older part: widen the ranges"),
            format!("Жодне налаштування не має {MIN_EVENTS} подій на старій частині: розшир діапазони"),
        ),
    };
    line(frame, area, 2 - o, &best_line, text);
    line(
        frame,
        area,
        3 - o,
        &pick(
            format!(
                "Shape: {} | older and newer agree in {} of {}",
                l.shape(),
                l.agree.0,
                l.agree.1
            ),
            format!(
                "Форма: {} | стара й нова збігаються в {} з {}",
                l.shape(),
                l.agree.0,
                l.agree.1
            ),
        ),
        text,
    );
    let (cx, cy) = (cursor.0.min(gw - 1), cursor.1.min(gh - 1));
    let cur = match l.cell(cx, cy) {
        Some(c) => {
            let noise = if c.noise.is_finite() {
                format!("+-{:.0} pp", c.noise * 100.0)
            } else {
                "-".into()
            };
            pick(
                format!(
                    "Cursor: {}  older {} ({}), newer {} ({}), chance {noise} | Enter: judge",
                    l.grid.at(cx, cy),
                    pp(c.older.1),
                    c.older.0,
                    pp(c.newer.1),
                    c.newer.0
                ),
                format!(
                    "Курсор: {}  стара {} ({}), нова {} ({}), випадок {noise} | Enter: перевірити",
                    l.grid.at(cx, cy),
                    pp(c.older.1),
                    c.older.0,
                    pp(c.newer.1),
                    c.newer.0
                ),
            )
        }
        None => pick(
            format!("Cursor: {}  not computed", l.grid.at(cx, cy)),
            format!("Курсор: {}  не пораховано", l.grid.at(cx, cy)),
        ),
    };
    line(frame, area, 4 - o, &cur, muted);
    // The maps.
    const CELL: u16 = 5;
    const GAP: u16 = 3;
    let ylabels: Vec<String> = match l.grid.axes.get(1) {
        Some(a) => a.values.iter().map(|v| num(*v)).collect(),
        None => vec![String::new()],
    };
    let label_w = ylabels
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0)
        .max(3) as u16
        + 1;
    let map_w = label_w + gw as u16 * CELL;
    let fit = ((area.width + GAP) / (map_w + GAP)).clamp(1, 3) as usize;
    let top = 6u16 - o;
    let titles = [
        pick(
            "older 70%: odds - usual, pp",
            "старі 70%: шанс - звичайний, пп",
        ),
        pick("newer 30% (unseen)", "нові 30% (небачені)"),
        pick(
            "chance: 95% of shifted |edge|",
            "випадок: 95% зсунутих |ефекту|",
        ),
    ];
    let buf = frame.buffer_mut();
    let mut put = |x: u16, y: u16, s: &str, style: Style| {
        if y >= area.y + area.height {
            return;
        }
        for (i, ch) in s.chars().enumerate() {
            let cx = x + i as u16;
            if cx >= area.x + area.width {
                break;
            }
            if let Some(c) = buf.cell_mut((cx, y)) {
                c.set_char(ch);
                c.set_style(style);
            }
        }
    };
    let xname = |i: usize| -> String {
        let a = &l.grid.axes[0];
        format!("{:>w$}", num(a.values[i]), w = CELL as usize)
    };
    for (m, title) in titles.iter().enumerate().take(fit) {
        let x0 = area.x + m as u16 * (map_w + GAP);
        put(
            x0,
            area.y + top,
            truncate(title, map_w as usize),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        );
        for i in 0..gw {
            put(
                x0 + label_w + i as u16 * CELL,
                area.y + top + 1,
                &xname(i),
                muted,
            );
        }
        for (y, label) in ylabels.iter().enumerate().take(gh) {
            let ry = area.y + top + 2 + y as u16;
            put(
                x0,
                ry,
                &format!("{label:>w$} ", w = label_w as usize - 1),
                muted,
            );
            for x in 0..gw {
                let at = x0 + label_w + x as u16 * CELL;
                let best = l.best == Some((x, y));
                let mark = if best { "●" } else { " " };
                let (s, mut style) = match (l.cell(x, y), m) {
                    (None, _) => ("    -".to_string(), muted),
                    (Some(c), 2) => (
                        if c.noise.is_finite() && c.rankable() {
                            format!("{:>5.0}", c.noise * 100.0)
                        } else {
                            "    ·".into()
                        },
                        muted,
                    ),
                    (Some(c), _) => {
                        let (ev, e) = if m == 0 { c.older } else { c.newer };
                        if ev < MIN_EVENTS {
                            ("    ·".to_string(), muted)
                        } else {
                            let s = format!("{:>5}", format!("{}{:+.0}", mark.trim(), e * 100.0));
                            let style = if m == 0 && c.above_noise() {
                                Style::default().fg(Color::Black).bg(if e > 0.0 {
                                    BULL
                                } else {
                                    BEAR
                                })
                            } else {
                                Style::default()
                                    .fg(if e > 0.0 { Color::Green } else { Color::Red })
                                    .bg(palette.bg)
                            };
                            (s, style)
                        }
                    }
                };
                if (x, y) == (cx, cy) {
                    style = Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg);
                }
                put(at, ry, &s, style);
            }
        }
    }
    // With pixels, each map becomes patches of colour (the numbers stay on
    // the cursor line): brighter = a larger edge; green = higher more often,
    // red = less often; a white frame = beyond its chaos baseline; the
    // chaos map in greys; a white dot = the best; a yellow frame = cursor.
    for m in 0..fit {
        let x0 = area.x + m as u16 * (map_w + GAP) + label_w;
        let rect = Rect {
            x: x0,
            y: area.y + top + 2,
            width: gw as u16 * CELL,
            height: gh as u16,
        }
        .intersection(area);
        if rect.width < gw as u16 * CELL || rect.height < gh as u16 {
            continue;
        }
        let Some((pw, ph)) = gfx::plot_px(rect) else {
            continue;
        };
        let mut cv = Canvas::new(pw, ph, palette.bg);
        // From a neutral dark grey: mixing with the page's blue reads as purple.
        let bg = [48u8, 48, 56];
        let mix = |c: [u8; 3], t: f64| -> Color {
            let t = (t * 4.0).round() / 4.0;
            let f = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
            Color::Rgb(f(bg[0], c[0]), f(bg[1], c[1]), f(bg[2], c[2]))
        };
        let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
        let (cw, ch) = (pw as f64 / gw as f64, ph as f64 / gh as f64);
        let white = cv.ink(Color::White);
        let yellow = cv.ink(Color::LightYellow);
        let dim = cv.ink(palette.muted);
        for y in 0..gh {
            for x in 0..gw {
                let (x1, y1) = ((x as f64 * cw) as i64, (y as f64 * ch) as i64);
                let (x2, y2) = (((x + 1) as f64 * cw) as i64, ((y + 1) as f64 * ch) as i64);
                let (ix1, iy1, ix2, iy2) = (x1 + pt, y1 + pt, x2 - pt, y2 - pt);
                let Some(c) = l.cell(x, y) else {
                    continue;
                };
                let (fill, frame_it) = match m {
                    2 => (
                        (c.rankable() && c.noise.is_finite())
                            .then(|| mix([200, 200, 200], (c.noise / 0.2).clamp(0.25, 1.0))),
                        false,
                    ),
                    _ => {
                        let (ev, e) = if m == 0 { c.older } else { c.newer };
                        if ev < MIN_EVENTS {
                            (None, false)
                        } else {
                            let base = if e > 0.0 {
                                [60, 230, 110]
                            } else {
                                [245, 80, 80]
                            };
                            (
                                Some(mix(base, (e.abs() / 0.15).clamp(0.25, 1.0))),
                                m == 0 && c.above_noise(),
                            )
                        }
                    }
                };
                match fill {
                    Some(col) => {
                        let ink = cv.ink(col);
                        cv.fill(ix1, iy1, ix2, iy2, ink);
                    }
                    None => {
                        let (mx, my) = ((ix1 + ix2) / 2, (iy1 + iy2) / 2);
                        cv.fill(mx - pt, my - pt, mx + pt, my + pt, dim);
                    }
                }
                let ring = |cv: &mut Canvas, ink: u8, w: i64| {
                    cv.fill(ix1, iy1, ix2, iy1 + w, ink);
                    cv.fill(ix1, iy2 - w, ix2, iy2, ink);
                    cv.fill(ix1, iy1, ix1 + w, iy2, ink);
                    cv.fill(ix2 - w, iy1, ix2, iy2, ink);
                };
                if frame_it {
                    ring(&mut cv, white, pt);
                }
                if l.best == Some((x, y)) && m < 2 {
                    let (mx, my) = ((ix1 + ix2) / 2, (iy1 + iy2) / 2);
                    let r = (2 * pt).min((iy2 - iy1) / 3).max(1);
                    cv.fill(mx - r, my - r, mx + r, my + r, white);
                }
                if (x, y) == (cx, cy) {
                    ring(&mut cv, yellow, 2 * pt);
                }
            }
        }
        gfx::submit(frame.buffer_mut(), rect, cv);
    }
    let foot = top + 3 + gh as u16;
    let more = if fit < 3 {
        pick(
            format!(" | {fit} of 3 maps shown (widen the window)"),
            format!(" | показано {fit} з 3 карт (збільш вікно)"),
        )
    } else {
        String::new()
    };
    line(
        frame,
        area,
        foot,
        &format!(
            "x: {}{}{more}",
            l.grid.axes[0].range,
            l.grid
                .axes
                .get(1)
                .map(|a| format!(", y: {}", a.range))
                .unwrap_or_default()
        ),
        muted,
    );
    line(
        frame,
        area,
        foot + 1,
        &if gfx::cell_px().is_some() {
            pick(
                format!("brighter = bigger; green: rise likelier, red: less | white frame: beyond chance | white dot: best | dot: < {MIN_EVENTS} events | arrows, Enter: test"),
                format!("яскравіше = більше; зелений: зростання ймовірніше, червоний: менше | біла рамка: поза випадком | біла крапка: найкраще | крапка: < {MIN_EVENTS} подій | стрілки, Enter: перевірка"),
            )
        } else {
            pick(
                format!("pp: P(higher) after event minus usual | solid: beyond chance | ●: best on older | ·: < {MIN_EVENTS} events | arrows, Enter: judge"),
                format!("пп: P(вище) після події мінус звичайна | суцільний: поза випадком | ●: найкраще на старій | ·: < {MIN_EVENTS} подій | стрілки, Enter: перевірка"),
            )
        },
        muted,
    );
}

/// "What stands out on the market now?": the scanner's answer, its pairs
/// (the chosen one's spread as a chart), the stretched coins and the knives.
/// `status`: the scanning window's progress or a hint, may be empty.
/// A pair's earlier wide gaps, in words: "before: closed 4 of 5 within 28
/// candles (median 9)".
pub fn history_words(h: &crate::scanner::GapHistory) -> String {
    if h.times == 0 {
        return pick("before: never this wide", "раніше так широко не було").to_string();
    }
    let mut s = pick(
        format!(
            "before: closed {} of {} in {} candles",
            h.closed, h.times, h.horizon
        ),
        format!(
            "раніше: закрився {} з {} за {} св.",
            h.closed, h.times, h.horizon
        ),
    );
    if h.closed > 0 {
        s.push_str(&pick(
            format!(" (median {:.0})", h.median_candles),
            format!(" (медіана {:.0})", h.median_candles),
        ));
    }
    s
}

pub fn render_scanner_page(
    frame: &mut Frame,
    area: Rect,
    s: &crate::scanner::Scan,
    sel: usize,
    now_ms: i64,
    status: &str,
    trade_hint: &str,
    palette: &Palette,
) {
    let age = ((now_ms - s.at_ms) / 60_000).max(0);
    let short = |x: &str| x.trim_end_matches("USDT").to_string();
    let st_tail = if status.is_empty() {
        String::new()
    } else {
        format!(" ({status})")
    };
    let answer = pick(
        format!(
            "{} pair(s) off their norm, {} up, {} down, {} knife(s), {} pump(s), {} dump(s); scanned {} min ago{st_tail}.",
            s.pairs.len(),
            s.up.len(),
            s.down.len(),
            s.knives.len(),
            s.pumps.len(),
            s.dumps.len(),
            age
        ),
        format!(
            "{} пар(и) поза нормою, {} угору, {} униз, {} ножів, {} пампів, {} дампів; скан {} хв тому{st_tail}.",
            s.pairs.len(),
            s.up.len(),
            s.down.len(),
            s.knives.len(),
            s.pumps.len(),
            s.dumps.len(),
            age
        ),
    );
    let rest = render_head(
        frame,
        area,
        &pick(
            format!(
                "What stands out among the {} most traded coins ({} candles, {} pairs tried)?",
                s.coins, s.tf, s.pairs_tried
            ),
            format!(
                "Що виділяється серед {} найторгованіших монет ({} свічок, перевірено {} пар)?",
                s.coins, s.tf, s.pairs_tried
            ),
        ),
        &answer,
        Tone::Plain,
        pick(
            "Pair: two coins tied in price; when the gap is off, buy the cheap, sell the rich, bet it closes. Stretched: far from its 50-candle norm. Knife: -8% a day, still falling, heavy volume. Pump/dump: live fast move, listed 30 min. Candidates, not signals: test in mode 3.",
            "Пара: дві монети зі зв'язаною ціною; коли розрив відхилився, купи дешеву, продай дорогу, ставка на закриття розриву. Розтягнуті: далеко від норми за 50 свічок. Ніж: -8% за день, ще падає, великі обсяги. Памп/дамп: швидкий рух, у списку 30 хв. Кандидати, а не сигнали: перевір у режимі 3.",
        ),
        palette,
    );
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let bold = Style::default()
        .fg(palette.accent)
        .bg(palette.bg)
        .add_modifier(Modifier::BOLD);
    let mut y = 0u16;
    let head = if s.pairs.is_empty() || trade_hint.is_empty() {
        pick("Pairs (Up/Down: choose):", "Пари (вгору/вниз: вибір):").to_string()
    } else {
        pick(
            format!("Pairs (Up/Down: choose; {trade_hint}):"),
            format!("Пари (вгору/вниз: вибір; {trade_hint}):"),
        )
    };
    line(frame, rest, y, &head, bold);
    y += 1;
    let sel = sel.min(s.pairs.len().saturating_sub(1));
    if s.pairs.is_empty() {
        line(
            frame,
            rest,
            y,
            pick(
                "  none: no tied pair is off its norm",
                "  немає: жодна пара не відхилилась від норми",
            ),
            muted,
        );
        y += 1;
    }
    for (i, p) in s.pairs.iter().enumerate() {
        let row = format!(
            "  {} {:<9} {} {:<9}  {} {:+.1} sd   r {:.2}   {} {:.0}   {} {:.2}   {}",
            pick("buy", "купити"),
            short(&p.long),
            pick("sell", "продати"),
            short(&p.short),
            pick("gap", "розрив"),
            p.z,
            p.r,
            pick("half-life", "піврозпад"),
            p.half_life,
            pick("ratio", "коеф."),
            p.beta,
            history_words(&p.history)
        );
        let st = if i == sel {
            Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg)
        } else {
            text
        };
        line(frame, rest, y, &row, st);
        y += 1;
    }
    // The chosen pair's gap through the history.
    let lists_h = 11u16;
    let chart_h = rest.height.saturating_sub(y + 1 + lists_h);
    if let Some(p) = s.pairs.get(sel).filter(|_| chart_h >= 6) {
        let area_c = Rect {
            y: rest.y + y + 1,
            height: chart_h,
            ..rest
        };
        line(
            frame,
            area_c,
            0,
            &pick(
                format!(
                    "Gap {} vs {}, in sd (dashed: +-2); now {:+.1}",
                    short(&p.a),
                    short(&p.b),
                    p.z
                ),
                format!(
                    "Розрив {} проти {}, у sd (штрих: +-2); зараз {:+.1}",
                    short(&p.a),
                    short(&p.b),
                    p.z
                ),
            ),
            muted,
        );
        let plot = Rect {
            y: area_c.y + 1,
            height: area_c.height - 1,
            width: area_c.width.saturating_sub(6),
            ..area_c
        };
        if let Some((pw, ph)) = gfx::plot_px(plot) {
            let zs = &p.z_path;
            let lim = zs.iter().fold(2.5f64, |m, v| m.max(v.abs())) * 1.05;
            let mut cv = Canvas::new(pw, ph, palette.bg);
            let pt = (gfx::cell_px().map_or(8, |c| c.0) as i64 / 7).max(1);
            let y_of = |v: f64| ((lim - v) / (2.0 * lim) * (ph as f64 - 1.0)).round() as i64;
            let grid = cv.ink(palette.muted);
            for g in [-2.0, 2.0] {
                cv.dashed_hline(y_of(g), pt, 2 * pt as usize, 2 * pt as usize, grid);
            }
            cv.fill(0, y_of(0.0), pw as i64, y_of(0.0) + pt, grid);
            let ink = cv.ink(Color::LightCyan);
            let n = zs.len().max(2);
            for i in 1..zs.len() {
                let x0 = ((i - 1) as f64 / (n - 1) as f64 * (pw as f64 - 1.0)) as i64;
                let x1 = (i as f64 / (n - 1) as f64 * (pw as f64 - 1.0)) as i64;
                cv.line(x0, y_of(zs[i - 1]), x1, y_of(zs[i]), pt, ink);
            }
            gfx::submit(frame.buffer_mut(), plot, cv);
            let ax = plot.x + plot.width + 1;
            line(
                frame,
                Rect {
                    x: ax,
                    y: plot.y,
                    width: 6,
                    height: 1,
                },
                0,
                &format!("{:+.0}", lim),
                muted,
            );
            line(
                frame,
                Rect {
                    x: ax,
                    y: plot.y + plot.height - 1,
                    width: 6,
                    height: 1,
                },
                0,
                &format!("{:+.0}", -lim),
                muted,
            );
        }
        y += chart_h + 1;
    }
    // The other lists, one wrapped paragraph each.
    let w = rest.width as usize;
    let mut para = |title: &str, items: Vec<String>, y: &mut u16| {
        let body = if items.is_empty() {
            format!("{title}: {}", pick("none", "немає"))
        } else {
            format!("{title}: {}", items.join("  "))
        };
        for (k, l) in wrap(&body, w).into_iter().take(2).enumerate() {
            line(frame, rest, *y, &l, if k == 0 { text } else { muted });
            *y += 1;
        }
    };
    y += 1;
    para(
        pick("Stretched up", "Розтягнуті вгору"),
        s.up.iter()
            .map(|x| format!("{} {:+.1}sd", short(&x.symbol), x.z))
            .collect(),
        &mut y,
    );
    para(
        pick("Stretched down", "Розтягнуті вниз"),
        s.down
            .iter()
            .map(|x| format!("{} {:+.1}sd", short(&x.symbol), x.z))
            .collect(),
        &mut y,
    );
    para(
        pick("Falling knives", "Падаючі ножі"),
        s.knives
            .iter()
            .map(|k| {
                format!(
                    "{} {:+.0}% {}, {} x{:.1}",
                    short(&k.symbol),
                    k.day_pct,
                    pick("day", "доба"),
                    pick("vol", "обсяг"),
                    k.volume_x
                )
            })
            .collect(),
        &mut y,
    );
    let mover = |m: &crate::movers::Mover| {
        format!(
            "{} {:+.1}% {} {}{} {} x{:.1} ({} {})",
            short(&m.symbol),
            m.change_pct,
            pick("in", "за"),
            m.window_min,
            pick("m", "хв"),
            pick("vol", "обсяг"),
            m.volume_x,
            ((now_ms - m.first_ms) / 60_000).max(0),
            pick("min ago", "хв тому")
        )
    };
    para(
        pick("Pumps", "Пампи"),
        s.pumps.iter().map(mover).collect(),
        &mut y,
    );
    para(
        pick("Dumps", "Дампи"),
        s.dumps.iter().map(mover).collect(),
        &mut y,
    );
}
