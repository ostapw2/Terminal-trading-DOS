//! Pumps and dumps over the whole market, from the all-symbols ticker
//! stream (`!miniTicker@arr`, once a second; no REST requests).
//!
//! Each coin's price and 24h quote volume are sampled every
//! [`SAMPLE_MS`]; a coin is a pump (dump) when its price rose (fell) by more
//! than a window's threshold ([`WINDOWS`]) AND the dollars traded in that
//! window are well above its usual pace ([`VOL_X_MIN`]).  The stream gives
//! only the rolling 24h volume, so the window's volume is estimated:
//! `Δ(24h volume) + the window's share of the day` — the part that left the
//! 24h window is assumed average.  A hit stays listed for [`HOLD_MS`] after
//! the last time it matched, so the lists do not flicker.
//!
//! Needs as much history as a window is long: after a start the 5-minute
//! window works first, the hour last.

use std::collections::{HashMap, VecDeque};

/// One ticker of the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub symbol: String,
    pub price: f64,
    /// Rolling 24h quote volume (USDT).
    pub quote_volume: f64,
    pub time_ms: i64,
}

pub const SAMPLE_MS: i64 = 10_000;
/// One-minute candles fetched to fill the past ([`Movers::backfill`]).
pub const BACKFILL_MINUTES: u32 = 65;
/// History kept per coin (the longest window plus a margin).
pub const KEEP_MS: i64 = 65 * 60_000;
/// (minutes, % move) a coin must make within the window.
pub const WINDOWS: [(u32, f64); 3] = [(5, 3.0), (15, 5.0), (60, 8.0)];
/// The window's dollars over its usual share of the day.
pub const VOL_X_MIN: f64 = 2.0;
/// Coins trading less than this a day are left out (thin books move alone).
pub const MIN_QUOTE_24H: f64 = 2_000_000.0;
/// A hit stays listed this long after it last matched.
pub const HOLD_MS: i64 = 30 * 60_000;
/// Most rows per list.
pub const MAX_LISTED: usize = 15;

/// A coin that pumped or dumped.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mover {
    pub symbol: String,
    pub up: bool,
    /// The move, %, over `window_min` (the largest relative to its threshold).
    pub change_pct: f64,
    pub window_min: u32,
    /// The window's dollars over its usual share (estimated).
    pub volume_x: f64,
    pub price: f64,
    /// First and last time it matched (ms).
    pub first_ms: i64,
    pub last_ms: i64,
}

/// A coin's move over a window, for everyone ([`Movers::returns`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Return {
    pub symbol: String,
    /// The move, %.
    pub pct: f64,
    /// The window's dollars over its usual share of the day (estimated).
    pub volume_x: f64,
    pub price: f64,
    pub quote_24h: f64,
}

#[derive(Default)]
pub struct Movers {
    /// Per coin: (time, price, 24h quote volume), oldest first.
    hist: HashMap<String, VecDeque<(i64, f64, f64)>>,
    hits: HashMap<String, Mover>,
}

impl Movers {
    /// Take a batch of ticks (only tradable USDT coins are kept).
    pub fn feed<'a>(&mut self, ticks: impl IntoIterator<Item = &'a Tick>) {
        for t in ticks {
            if !(t.price > 0.0 && t.price.is_finite()) || !crate::scanner::tradable(&t.symbol) {
                continue;
            }
            let q = self.hist.entry(t.symbol.clone()).or_default();
            if q.back().is_some_and(|b| t.time_ms - b.0 < SAMPLE_MS) {
                // Keep the newest price within the sample, so "now" is now.
                if let Some(b) = q.back_mut() {
                    if t.time_ms >= b.0 {
                        b.1 = t.price;
                        b.2 = t.quote_volume;
                    }
                }
                continue;
            }
            q.push_back((t.time_ms, t.price, t.quote_volume));
            while q.front().is_some_and(|f| t.time_ms - f.0 > KEEP_MS) {
                q.pop_front();
            }
        }
    }

    /// Put one-minute closes of the past in front of what the stream has
    /// (`closes`: (time of the close in ms, price), oldest first; `quote_24h`:
    /// the coin's dollars a day, taken as constant: the history carries no
    /// volume).  Only samples older than the stream's first are used, so the
    /// correlations and the returns reach back an hour at once instead of
    /// waiting for it.  Pumps are not found in backfilled time: with the
    /// day's dollars flat there is no volume surge to see.
    pub fn backfill(&mut self, symbol: &str, closes: &[(i64, f64)], quote_24h: f64) {
        if !crate::scanner::tradable(symbol)
            || quote_24h.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        {
            return;
        }
        let q = self.hist.entry(symbol.to_string()).or_default();
        let limit = q.front().map_or(i64::MAX, |f| f.0 - SAMPLE_MS);
        let old: Vec<&(i64, f64)> = closes
            .iter()
            .filter(|(t, p)| *t < limit && *p > 0.0 && p.is_finite())
            .collect();
        for (t, p) in old.into_iter().rev() {
            q.push_front((*t, *p, quote_24h));
        }
    }

    /// Judge every coin at `now_ms`; drop hits older than [`HOLD_MS`].
    /// `true` when the lists changed.
    pub fn judge(&mut self, now_ms: i64) -> bool {
        let before = self.members();
        for (sym, q) in &self.hist {
            let Some(&(t_now, p_now, v_now)) = q.back() else {
                continue;
            };
            if now_ms - t_now > 2 * SAMPLE_MS || v_now < MIN_QUOTE_24H {
                continue;
            }
            // The strongest window: the largest move over its threshold.
            let mut best: Option<(f64, u32, f64, f64)> = None; // (ratio, min, pct, vol_x)
            for (min, thr) in WINDOWS {
                let w = min as i64 * 60_000;
                // The sample nearest to `now - w`, if the history reaches it.
                let Some(&(t0, p0, v0)) = q.iter().rev().find(|s| t_now - s.0 >= w) else {
                    continue;
                };
                if t_now - t0 > w + 3 * SAMPLE_MS || p0 <= 0.0 {
                    continue;
                }
                let pct = (p_now / p0 - 1.0) * 100.0;
                let usual = v_now * min as f64 / 1440.0;
                let vol_x = if usual > 0.0 {
                    ((v_now - v0) + usual) / usual
                } else {
                    0.0
                };
                if pct.abs() < thr || vol_x < VOL_X_MIN {
                    continue;
                }
                let ratio = pct.abs() / thr;
                if best.is_none_or(|b| ratio > b.0) {
                    best = Some((ratio, min, pct, vol_x));
                }
            }
            let Some((_, min, pct, vol_x)) = best else {
                continue;
            };
            let up = pct > 0.0;
            let first_ms = self
                .hits
                .get(sym)
                .filter(|h| h.up == up)
                .map_or(now_ms, |h| h.first_ms);
            self.hits.insert(
                sym.clone(),
                Mover {
                    symbol: sym.clone(),
                    up,
                    change_pct: pct,
                    window_min: min,
                    volume_x: vol_x,
                    price: p_now,
                    first_ms,
                    last_ms: now_ms,
                },
            );
        }
        self.hits.retain(|_, h| now_ms - h.last_ms <= HOLD_MS);
        self.members() != before
    }

    /// Pumps (or dumps), the strongest first.
    pub fn list(&self, up: bool) -> Vec<&Mover> {
        let mut v: Vec<&Mover> = self.hits.values().filter(|h| h.up == up).collect();
        v.sort_by(|a, b| {
            b.change_pct
                .abs()
                .total_cmp(&a.change_pct.abs())
                .then(a.symbol.cmp(&b.symbol))
        });
        v.truncate(MAX_LISTED);
        v
    }

    /// Symbols of each list.
    pub fn members(&self) -> (Vec<String>, Vec<String>) {
        let names = |up| self.list(up).iter().map(|m| m.symbol.clone()).collect();
        (names(true), names(false))
    }

    /// Every coin's move over the last `window_min` minutes, whatever its
    /// size (the lists above keep only the ones past a threshold): for the
    /// live rankings and correlations (`crate::live_ideas`).  Coins whose
    /// history does not reach the window, thin ones and stale ones are left out.
    pub fn returns(&self, window_min: u32, now_ms: i64) -> Vec<Return> {
        let w = window_min as i64 * 60_000;
        let mut out = Vec::new();
        for (sym, q) in &self.hist {
            let Some(&(t_now, p_now, v_now)) = q.back() else {
                continue;
            };
            if now_ms - t_now > 2 * SAMPLE_MS || v_now < MIN_QUOTE_24H {
                continue;
            }
            let Some(&(t0, p0, v0)) = q.iter().rev().find(|s| t_now - s.0 >= w) else {
                continue;
            };
            // Backfilled history has one sample a minute: its nearest sample
            // may lie up to a minute before the window starts.
            if t_now - t0 > w + 6 * SAMPLE_MS + 10_000 || p0 <= 0.0 {
                continue;
            }
            let usual = v_now * window_min as f64 / 1440.0;
            let volume_x = if usual > 0.0 {
                ((v_now - v0) + usual) / usual
            } else {
                0.0
            };
            out.push(Return {
                symbol: sym.clone(),
                pct: (p_now / p0 - 1.0) * 100.0,
                volume_x,
                price: p_now,
                quote_24h: v_now,
            });
        }
        out.sort_by(|a, b| b.pct.total_cmp(&a.pct).then(a.symbol.cmp(&b.symbol)));
        out
    }

    /// One close per minute for the last `minutes` minutes (oldest first,
    /// the last one is now), per coin that has every minute: the series the
    /// live correlations use.  A minute's close is the newest sample in it.
    pub fn minute_closes(&self, minutes: usize, now_ms: i64) -> Vec<(String, Vec<f64>)> {
        const M: i64 = 60_000;
        let mut out = Vec::new();
        for (sym, q) in &self.hist {
            let Some(&(t_now, _, v_now)) = q.back() else {
                continue;
            };
            if now_ms - t_now > 2 * SAMPLE_MS || v_now < MIN_QUOTE_24H {
                continue;
            }
            let mut closes = Vec::with_capacity(minutes);
            // Walk the minutes from the oldest; samples are in time order.
            let mut i = 0;
            let mut ok = true;
            for k in 0..minutes {
                let end = now_ms - (minutes - 1 - k) as i64 * M;
                let start = end - M;
                while i < q.len() && q[i].0 <= end {
                    i += 1;
                }
                // The newest sample at or before `end`, if it is in the minute.
                match i.checked_sub(1).map(|j| q[j]) {
                    Some((t, p, _)) if t > start - SAMPLE_MS && p > 0.0 => closes.push(p),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                out.push((sym.clone(), closes));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Each coin's newest price and 24h dollars (for the price tracker of
    /// the dynamic groups: fresher and wider than the REST summary).
    pub fn latest(&self) -> impl Iterator<Item = (&str, f64, f64)> {
        self.hist
            .iter()
            .filter_map(|(s, q)| q.back().map(|b| (s.as_str(), b.1, b.2)))
    }

    /// How many coins have history, and the longest span of it (ms).
    pub fn coverage(&self) -> (usize, i64) {
        let span = self
            .hist
            .values()
            .filter_map(|q| Some(q.back()?.0 - q.front()?.0))
            .max()
            .unwrap_or(0);
        (self.hist.len(), span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 60_000;
    const DAY_VOL: f64 = 14_400_000.0; // $10K a minute on average

    fn tick(sym: &str, t: i64, price: f64, vol: f64) -> Tick {
        Tick {
            symbol: sym.into(),
            price,
            quote_volume: vol,
            time_ms: t,
        }
    }

    /// `minutes` of samples every 10 s: price from `p0` to `p1` over the last
    /// `move_min` minutes, trading `per_min` dollars a minute during the move.
    fn feed_path(
        m: &mut Movers,
        sym: &str,
        minutes: i64,
        move_min: i64,
        p0: f64,
        p1: f64,
        per_min: f64,
    ) {
        let mut vol = DAY_VOL;
        for k in 0..=(minutes * 6) {
            let t = k * SAMPLE_MS;
            let into_move = t - (minutes - move_min) * M;
            let price = if into_move <= 0 {
                p0
            } else {
                p0 + (p1 - p0) * into_move as f64 / (move_min * M) as f64
            };
            if into_move > 0 {
                // New dollars minus the average that leaves the 24h window.
                vol += (per_min - DAY_VOL / 1440.0) / 6.0;
            }
            m.feed([&tick(sym, t, price, vol)]);
        }
    }

    #[test]
    fn a_fast_move_on_heavy_volume_is_a_pump_and_quiet_drift_is_not() {
        let mut m = Movers::default();
        // +4% in 5 minutes on 5x the usual dollars: a pump.
        feed_path(&mut m, "AAAUSDT", 20, 5, 100.0, 104.0, 50_000.0);
        // +4% in 5 minutes on usual volume: not confirmed.
        feed_path(&mut m, "BBBUSDT", 20, 5, 100.0, 104.0, 10_000.0);
        // -6% in 15 minutes on 4x: a dump.
        feed_path(&mut m, "CCCUSDT", 20, 15, 100.0, 94.0, 40_000.0);
        // A stablecoin never counts.
        feed_path(&mut m, "USDCUSDT", 20, 5, 1.0, 1.1, 90_000.0);
        let now = 20 * M;
        assert!(m.judge(now));
        let (pumps, dumps) = m.members();
        assert_eq!(pumps, vec!["AAAUSDT".to_string()]);
        assert_eq!(dumps, vec!["CCCUSDT".to_string()]);
        let p = m.list(true)[0];
        assert_eq!(p.window_min, 5);
        assert!((p.change_pct - 4.0).abs() < 0.2, "{p:?}");
        assert!(p.volume_x > 3.0 && p.volume_x < 7.0, "{p:?}");
        assert!(!m.judge(now), "nothing changed");
    }

    #[test]
    fn returns_rank_every_coin_and_minute_closes_need_every_minute() {
        let mut m = Movers::default();
        feed_path(&mut m, "AAAUSDT", 20, 5, 100.0, 104.0, 50_000.0);
        feed_path(&mut m, "BBBUSDT", 20, 5, 100.0, 101.0, 10_000.0);
        feed_path(&mut m, "CCCUSDT", 20, 15, 100.0, 94.0, 40_000.0);
        // Too short for a 15-minute window, fine for 5.
        feed_path(&mut m, "DDDUSDT", 8, 5, 100.0, 102.0, 10_000.0);
        let now = 20 * M;
        let r5 = m.returns(5, now);
        let names: Vec<&str> = r5.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(
            names,
            ["AAAUSDT", "BBBUSDT", "CCCUSDT"],
            "DDD's last sample is 12 min old"
        );
        assert!(
            (r5[0].pct - 4.0).abs() < 0.2 && r5[0].volume_x > 3.0,
            "{:?}",
            r5[0]
        );
        assert!((r5[1].pct - 1.0).abs() < 0.2, "{:?}", r5[1]);
        let r15 = m.returns(15, now);
        assert_eq!(r15.len(), 3);
        assert!(r15
            .iter()
            .all(|r| r.price > 0.0 && r.quote_24h >= MIN_QUOTE_24H));
        // Fresh coins only: DDD at its own "now" is listed.
        let r_d = m.returns(5, 8 * M);
        assert!(r_d.iter().any(|r| r.symbol == "DDDUSDT"));
        // Minute closes: 15 minutes for the three coins with 20 minutes of
        // history, none for the one with 8 (the first minutes are missing).
        let c = m.minute_closes(15, now);
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|(_, v)| v.len() == 15));
        let aaa = &c.iter().find(|(s, _)| s == "AAAUSDT").unwrap().1;
        assert!(
            (aaa[14] - 104.0).abs() < 1e-9 && (aaa[0] - 100.0).abs() < 1e-9,
            "{aaa:?}"
        );
        assert!(
            aaa.windows(2).all(|w| w[1] >= w[0]),
            "rising in the move: {aaa:?}"
        );
        assert!(m.minute_closes(25, now).is_empty(), "nobody has 25 minutes");
        assert_eq!(m.latest().count(), 4);
    }

    #[test]
    fn hits_hold_then_expire_and_short_history_says_nothing() {
        let mut m = Movers::default();
        feed_path(&mut m, "AAAUSDT", 4, 3, 100.0, 110.0, 90_000.0);
        assert!(
            !m.judge(4 * M),
            "four minutes of history: no 5-minute window yet"
        );
        feed_path(&mut m, "DDDUSDT", 20, 5, 100.0, 105.0, 80_000.0);
        m.judge(20 * M);
        assert_eq!(m.members().0, vec!["DDDUSDT".to_string()]);
        // The coin goes quiet: the move stays inside the 15-minute window
        // for 15 more minutes, then the hold runs; then it is dropped.
        let last = m.hist["DDDUSDT"].back().copied().unwrap();
        let mut t = 20 * M;
        while t < 20 * M + 15 * M + HOLD_MS + 2 * M {
            t += SAMPLE_MS;
            m.feed([&tick("DDDUSDT", t, last.1, last.2)]);
            m.judge(t);
            if t < 20 * M + HOLD_MS {
                assert_eq!(m.members().0.len(), 1, "held at {t}");
            }
        }
        assert!(m.members().0.is_empty(), "expired");
        // A thin coin is ignored.
        let mut thin = Movers::default();
        for k in 0..=120 {
            let t = k * SAMPLE_MS;
            let p = if k > 90 { 120.0 } else { 100.0 };
            thin.feed([&tick("EEEUSDT", t, p, 100_000.0 + k as f64 * 1_000.0)]);
        }
        assert!(!thin.judge(20 * M));
    }

    #[test]
    fn backfilled_minutes_make_the_hour_available_at_once() {
        let mut m = Movers::default();
        let now = 100 * M;
        // The stream has only the last minute; the past hour comes from klines.
        for k in 0..=6 {
            m.feed([&tick("AAAUSDT", now - M + k * SAMPLE_MS, 10.0, DAY_VOL)]);
        }
        let closes: Vec<(i64, f64)> = (0..70)
            .map(|k| (now - (70 - k) * M + 59_000, 9.0 + k as f64 * 0.01))
            .collect();
        m.backfill("AAAUSDT", &closes, DAY_VOL);
        let (_, span) = m.coverage();
        assert!(span >= 60 * M, "span {span}");
        // Only samples older than the stream's first were put in front.
        assert!(m.hist["AAAUSDT"].iter().all(|s| s.0 <= now));
        assert!(m.hist["AAAUSDT"]
            .iter()
            .zip(m.hist["AAAUSDT"].iter().skip(1))
            .all(|(a, b)| a.0 < b.0));
        let closes60 = m.minute_closes(60, now);
        assert_eq!(closes60.len(), 1);
        assert_eq!(closes60[0].1.len(), 60);
        // A stable coin or a bad price is ignored.
        m.backfill("USDCUSDT", &closes, DAY_VOL);
        assert!(!m.hist.contains_key("USDCUSDT"));
    }
}
