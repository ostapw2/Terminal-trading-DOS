//! The market scanner: hundreds of coins at once, sorted into lists you can
//! trade from.
//!
//! - **Pairs** for pair (stat-arb) trading: two coins whose prices are tied
//!   (their log prices form a spread that keeps coming back to its mean),
//!   whose spread is now far from that mean.  The cheap leg goes to the
//!   "long" list, the rich one to the "short" list.  A pair counts only if
//!   the spread comes back on the older part of the history *and* still does
//!   on the newer part (searching thousands of pairs finds tied-looking ones
//!   in pure noise; the newer part was not used to pick them).
//! - **Stretched up / down**: a coin far above / below its own recent normal
//!   (z of the price against its last `STRETCH_N` candles).
//! - **Falling knives**: a steep fall that has not stopped: down a lot over
//!   a day, still falling in the last hours, near its low, on heavy volume.
//!
//! Candidates, not signals: the lists say what stands out now; the judge
//! (Compare page) says whether such a setup ever paid.  Pure: aligned
//! candles in, a report out.

use crate::markets::Ohlc;

/// Candles the price z of "stretched" is measured over.
pub const STRETCH_N: usize = 50;
/// |z| at which a coin is stretched.
pub const STRETCH_Z: f64 = 2.5;
/// Correlation of hourly returns a pair needs.
pub const PAIR_MIN_R: f64 = 0.7;
/// |z| of the spread at which a pair is far from its mean.
pub const PAIR_Z: f64 = 2.0;
/// How strongly the spread must come back (t of the AR(1) coefficient):
/// the older part at the Engle-Granger 5 % level, the newer part looser.
pub const PAIR_T_OLDER: f64 = -3.4;
pub const PAIR_T_NEWER: f64 = -2.5;
/// Half-life of the spread, candles: too fast is noise, too slow never pays.
pub const HALF_LIFE: (f64, f64) = (2.0, 120.0);
/// The last candles left out of "does the spread come back" (they hold
/// the divergence itself).
pub const NOW_EXCLUDED: usize = 5;
/// How far (sd) the newer part's average gap may sit from the older
/// normal: further, and the pair has moved to a new level (a break), not
/// stretched away from its old one.
pub const PAIR_DRIFT_MAX: f64 = 1.0;
/// At most this many entries per list.
pub const MAX_PER_LIST: usize = 12;

/// One coin's candles (oldest first), on the same times as the others.
pub struct Series {
    pub symbol: String,
    pub candles: Vec<Ohlc>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PairIdea {
    pub a: String,
    pub b: String,
    /// Correlation of the returns.
    pub r: f64,
    /// Hedge ratio: spread = ln a - beta * ln b.
    pub beta: f64,
    /// Where the spread is now, in standard deviations from its mean.
    pub z: f64,
    /// Candles for the spread to close half its distance (older part).
    pub half_life: f64,
    /// t of the pull back to the mean: older part, newer part.
    pub t_older: f64,
    pub t_newer: f64,
    /// The leg to buy and the leg to sell.
    pub long: String,
    pub short: String,
    /// The spread's z through the history (for a chart).
    pub z_path: Vec<f64>,
    /// How earlier gaps this wide ended ([`gap_history`]).
    #[serde(default)]
    pub history: GapHistory,
}

/// Earlier times the gap was at least as wide as now, on the same side.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GapHistory {
    /// Episodes with a full horizon after them.
    pub times: usize,
    /// Of those, closed (back within 0.5 sd, or past zero) within the horizon.
    pub closed: usize,
    /// Median candles it took the closed ones.
    pub median_candles: f64,
    /// The horizon, candles (twice the half-life, at least 4).
    pub horizon: usize,
}

/// How often a gap this wide closed before: every earlier time `z_path`
/// crossed out to `|z_now|` on the side of `z_now` (before the last
/// `exclude` candles), did it come back within twice the half-life?  An
/// episode ends when it closes or its horizon runs out; one too close to
/// the end to have a full horizon is not counted.
pub fn gap_history(z_path: &[f64], z_now: f64, half_life: f64, exclude: usize) -> GapHistory {
    let thr = z_now.abs().max(PAIR_Z);
    let side = z_now.signum();
    let horizon = ((2.0 * half_life).ceil() as usize).max(4);
    let end = z_path.len().saturating_sub(exclude);
    let wide = |z: f64| z * side >= thr;
    let closed_at = |z: f64| z * side <= 0.5;
    let mut out = GapHistory {
        horizon,
        ..Default::default()
    };
    let mut took: Vec<f64> = Vec::new();
    let mut t = 1;
    while t < end {
        if !(wide(z_path[t]) && !wide(z_path[t - 1])) {
            t += 1;
            continue;
        }
        if t + horizon >= end {
            break;
        }
        out.times += 1;
        match (1..=horizon).find(|k| closed_at(z_path[t + k])) {
            Some(k) => {
                out.closed += 1;
                took.push(k as f64);
                t += k;
            }
            None => t += horizon,
        }
    }
    if !took.is_empty() {
        took.sort_by(f64::total_cmp);
        out.median_candles = took[took.len() / 2];
    }
    out
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stretch {
    pub symbol: String,
    pub z: f64,
    /// Change over the z window, %.
    pub change_pct: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Knife {
    pub symbol: String,
    /// Change over the last 24 candles and the last 4, %.
    pub day_pct: f64,
    pub recent_pct: f64,
    /// Volume of the last 4 candles over the usual 4.
    pub volume_x: f64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scan {
    pub pairs: Vec<PairIdea>,
    pub up: Vec<Stretch>,
    pub down: Vec<Stretch>,
    pub knives: Vec<Knife>,
    /// Coins scanned, pairs tried.
    pub coins: usize,
    pub pairs_tried: usize,
    pub candles: usize,
    /// When it ran (ms), and the candles' timeframe.
    #[serde(default)]
    pub at_ms: i64,
    #[serde(default)]
    pub tf: String,
    /// Live pumps and dumps (`crate::movers`), updated between scans.
    #[serde(default)]
    pub pumps: Vec<crate::movers::Mover>,
    #[serde(default)]
    pub dumps: Vec<crate::movers::Mover>,
}

/// The kv key the last scan is saved under (every window reads it).
pub const KV_KEY: &str = "scan_report";
/// ... and the 15-minute scan beside it.
pub const KV_KEY_15M: &str = "scan_report_15m";

/// The kv key of the scan on `tf` ("1h" or "15m").
pub fn kv_key(tf: &str) -> &'static str {
    if tf == "15m" {
        KV_KEY_15M
    } else {
        KV_KEY
    }
}

fn mean_sd(x: &[f64]) -> (f64, f64) {
    let n = x.len().max(1) as f64;
    let m = x.iter().sum::<f64>() / n;
    let v = x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (m, v.sqrt())
}

fn corr(x: &[f64], y: &[f64]) -> f64 {
    let (mx, sx) = mean_sd(x);
    let (my, sy) = mean_sd(y);
    if sx == 0.0 || sy == 0.0 {
        return 0.0;
    }
    let n = x.len().min(y.len());
    let c = (0..n).map(|i| (x[i] - mx) * (y[i] - my)).sum::<f64>() / (n as f64 - 1.0).max(1.0);
    c / (sx * sy)
}

/// OLS slope of y on x.
fn slope(y: &[f64], x: &[f64]) -> f64 {
    let (mx, sx) = mean_sd(x);
    let (my, _) = mean_sd(y);
    if sx == 0.0 {
        return 0.0;
    }
    let n = x.len().min(y.len());
    let c = (0..n).map(|i| (x[i] - mx) * (y[i] - my)).sum::<f64>() / (n as f64 - 1.0).max(1.0);
    c / (sx * sx)
}

/// How strongly `s` is pulled back to its mean: Δs_t = φ·(s_{t-1} - mean) + e;
/// returns (t of φ, half-life in candles).  A t well below zero means
/// the series keeps returning (a crude Dickey-Fuller test).
pub fn pull_back(s: &[f64]) -> (f64, f64) {
    if s.len() < 20 {
        return (0.0, f64::INFINITY);
    }
    let (m, _) = mean_sd(s);
    let x: Vec<f64> = s[..s.len() - 1].iter().map(|v| v - m).collect();
    let dy: Vec<f64> = s.windows(2).map(|w| w[1] - w[0]).collect();
    let sxx: f64 = x.iter().map(|v| v * v).sum();
    if sxx == 0.0 {
        return (0.0, f64::INFINITY);
    }
    let phi = x.iter().zip(&dy).map(|(a, b)| a * b).sum::<f64>() / sxx;
    let resid: f64 = x.iter().zip(&dy).map(|(a, b)| (b - phi * a).powi(2)).sum();
    let se = (resid / (x.len() as f64 - 1.0).max(1.0) / sxx).sqrt();
    let t = if se > 0.0 { phi / se } else { 0.0 };
    let hl = if phi < 0.0 && phi > -1.0 {
        -(2f64.ln()) / (1.0 + phi).ln()
    } else {
        f64::INFINITY
    };
    (t, hl)
}

fn returns(c: &[f64]) -> Vec<f64> {
    c.windows(2)
        .map(|w| {
            if w[0] > 0.0 && w[1] > 0.0 {
                (w[1] / w[0]).ln()
            } else {
                0.0
            }
        })
        .collect()
}

/// [`scan_tf`] on hourly candles.
pub fn scan(series: &[Series], older_share: f64) -> Scan {
    scan_tf(series, older_share, 24)
}

/// Scan `series` (same length, same times; shorter ones are skipped).
/// `older_share` of the history picks the pairs; the rest must agree.
/// `per_day` candles make a day (24 on 1h, 96 on 15m): the knives' "day"
/// and "last four hours" are measured in real time, not in candles.
pub fn scan_tf(series: &[Series], older_share: f64, per_day: usize) -> Scan {
    let n = series.iter().map(|s| s.candles.len()).max().unwrap_or(0);
    let usable: Vec<&Series> = series
        .iter()
        .filter(|s| n >= 60 && s.candles.len() == n && s.candles.iter().all(|c| c.close > 0.0))
        .collect();
    let mut out = Scan {
        coins: usable.len(),
        candles: n,
        ..Default::default()
    };
    if usable.is_empty() {
        return out;
    }
    let closes: Vec<Vec<f64>> = usable
        .iter()
        .map(|s| s.candles.iter().map(|c| c.close).collect())
        .collect();
    let logs: Vec<Vec<f64>> = closes
        .iter()
        .map(|c| c.iter().map(|v| v.ln()).collect())
        .collect();
    let rets: Vec<Vec<f64>> = closes.iter().map(|c| returns(c)).collect();

    // Stretched: the last close against its recent normal.
    for (i, s) in usable.iter().enumerate() {
        let c = &closes[i];
        let w = &c[n.saturating_sub(STRETCH_N)..];
        let (m, sd) = mean_sd(w);
        if sd <= 0.0 {
            continue;
        }
        let z = (c[n - 1] - m) / sd;
        let st = Stretch {
            symbol: s.symbol.clone(),
            z,
            change_pct: (c[n - 1] / w[0] - 1.0) * 100.0,
        };
        if z >= STRETCH_Z {
            out.up.push(st);
        } else if z <= -STRETCH_Z {
            out.down.push(st);
        }
    }
    out.up.sort_by(|a, b| b.z.total_cmp(&a.z));
    out.down.sort_by(|a, b| a.z.total_cmp(&b.z));
    out.up.truncate(MAX_PER_LIST);
    out.down.truncate(MAX_PER_LIST);

    // Falling knives: a day down hard, still falling, near the low, heavy volume.
    let day_n = per_day.max(6);
    let rec_n = day_n / 6; // four hours
    if n >= 2 * day_n {
        for s in &usable {
            let c = &s.candles;
            let last = c[n - 1].close;
            let day = (last / c[n - 1 - day_n].close - 1.0) * 100.0;
            let recent = (last / c[n - 1 - rec_n].close - 1.0) * 100.0;
            let low = c[n - day_n..]
                .iter()
                .map(|k| k.low)
                .fold(f64::MAX, f64::min);
            let v4: f64 = c[n - rec_n..].iter().map(|k| k.volume).sum();
            let usual: f64 = c[..n - rec_n].iter().map(|k| k.volume).sum::<f64>()
                / (n - rec_n) as f64
                * rec_n as f64;
            let vx = if usual > 0.0 { v4 / usual } else { 0.0 };
            if day <= -8.0 && recent <= -2.0 && last <= low * 1.01 && vx >= 1.5 {
                out.knives.push(Knife {
                    symbol: s.symbol.clone(),
                    day_pct: day,
                    recent_pct: recent,
                    volume_x: vx,
                });
            }
        }
        out.knives.sort_by(|a, b| a.day_pct.total_cmp(&b.day_pct));
        out.knives.truncate(MAX_PER_LIST);
    }

    // Pairs: tied prices, spread far from its mean now.
    let cut = ((n as f64) * older_share.clamp(0.5, 0.9)) as usize;
    let mut ideas: Vec<PairIdea> = Vec::new();
    for i in 0..usable.len() {
        for j in i + 1..usable.len() {
            out.pairs_tried += 1;
            let r = corr(&rets[i], &rets[j]);
            if r < PAIR_MIN_R {
                continue;
            }
            // Fitted on the older part only.
            let beta = slope(&logs[i][..cut], &logs[j][..cut]);
            if !(0.2..=5.0).contains(&beta) {
                continue;
            }
            let spread: Vec<f64> = (0..n).map(|t| logs[i][t] - beta * logs[j][t]).collect();
            let (t_older, half_life) = pull_back(&spread[..cut]);
            if t_older > PAIR_T_OLDER || !(HALF_LIFE.0..=HALF_LIFE.1).contains(&half_life) {
                continue;
            }
            // The newer part up to just before now: the divergence being
            // traded is not part of the test that the spread comes back.
            let (t_newer, _) = pull_back(&spread[cut..n - NOW_EXCLUDED]);
            if t_newer > PAIR_T_NEWER {
                continue;
            }
            let (m, sd) = mean_sd(&spread[..cut]);
            if sd <= 0.0 {
                continue;
            }
            let z_path: Vec<f64> = spread.iter().map(|v| (v - m) / sd).collect();
            let z = z_path[n - 1];
            if z.abs() < PAIR_Z {
                continue;
            }
            // A gap that has sat away from the old normal for the whole newer
            // part is a new level, not a stretch that will close.
            let newer = &z_path[cut..n - NOW_EXCLUDED];
            let drift = newer.iter().sum::<f64>() / newer.len().max(1) as f64;
            if drift.abs() > PAIR_DRIFT_MAX {
                continue;
            }
            let (a, b) = (usable[i].symbol.clone(), usable[j].symbol.clone());
            // Spread high: a is rich against b — sell a, buy b.
            let (long, short) = if z > 0.0 {
                (b.clone(), a.clone())
            } else {
                (a.clone(), b.clone())
            };
            let history = gap_history(&z_path, z, half_life, NOW_EXCLUDED);
            ideas.push(PairIdea {
                a,
                b,
                r,
                beta,
                z,
                half_life,
                t_older,
                t_newer,
                long,
                short,
                z_path,
                history,
            });
        }
    }
    ideas.sort_by(|x, y| y.z.abs().total_cmp(&x.z.abs()));
    // A coin is long in one pair or short in one, never both.
    let mut longs: Vec<String> = Vec::new();
    let mut shorts: Vec<String> = Vec::new();
    for p in ideas {
        if out.pairs.len() >= MAX_PER_LIST || shorts.contains(&p.long) || longs.contains(&p.short) {
            continue;
        }
        longs.push(p.long.clone());
        shorts.push(p.short.clone());
        out.pairs.push(p);
    }
    out
}

impl PairIdea {
    /// The two orders that trade this pair for `usd`: `(symbol, buy, usd)`,
    /// the leg to buy first.  Beta-weighted (the hedge the spread was fitted
    /// with: `$1` of `a` against `$beta` of `b`) or equal dollars.
    pub fn legs(&self, usd: f64, beta_weighted: bool) -> [(String, bool, f64); 2] {
        let w = |sym: &str| {
            if beta_weighted && sym == self.b {
                usd * self.beta.abs()
            } else {
                usd
            }
        };
        [
            (self.long.clone(), true, w(&self.long)),
            (self.short.clone(), false, w(&self.short)),
        ]
    }
}

impl Scan {
    /// Members of each scanner list, in order.
    pub fn members(&self, kind: crate::watchlists::ScanKind) -> Vec<String> {
        use crate::watchlists::ScanKind as K;
        let mut v: Vec<String> = match kind {
            K::PairsLong => self.pairs.iter().map(|p| p.long.clone()).collect(),
            K::PairsShort => self.pairs.iter().map(|p| p.short.clone()).collect(),
            K::StretchedUp => self.up.iter().map(|s| s.symbol.clone()).collect(),
            K::StretchedDown => self.down.iter().map(|s| s.symbol.clone()).collect(),
            K::Knives => self.knives.iter().map(|k| k.symbol.clone()).collect(),
            K::Pumps => self.pumps.iter().map(|m| m.symbol.clone()).collect(),
            K::Dumps => self.dumps.iter().map(|m| m.symbol.clone()).collect(),
        };
        let mut seen = std::collections::HashSet::new();
        v.retain(|s| seen.insert(s.clone()));
        v
    }
}

/// Symbols the venues' listings call something other than a coin: set from
/// `market.db` at start and after each listing refresh.
fn non_crypto() -> &'static std::sync::RwLock<Option<std::collections::HashSet<String>>> {
    static S: std::sync::OnceLock<std::sync::RwLock<Option<std::collections::HashSet<String>>>> =
        std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// What the listings say is not a coin (tokenized stocks, commodities, FX,
/// indices); [`tradable`] leaves these out from now on.
pub fn set_non_crypto(symbols: std::collections::HashSet<String>) {
    if let Ok(mut s) = non_crypto().write() {
        *s = Some(symbols);
    }
}

/// A USDT pair worth scanning: not a stablecoin, wrapped coin, leveraged
/// token or tokenized stock / commodity (those move with another market,
/// not the crypto one, and form a tight group of their own).
pub fn tradable(symbol: &str) -> bool {
    const STABLE: &[&str] = &[
        "USDC", "FDUSD", "TUSD", "BUSD", "USDP", "DAI", "EUR", "AEUR", "EURI", "USD1", "XUSD",
        "USDE", "PAX", "PAXG", "WBTC", "WBETH", "BFUSD",
    ];
    // Until the listings are loaded (first start, demo): the tokenized stocks
    // seen live 19.10.  After that the venues' own asset type decides.
    const STOCK_TOKENS: &[&str] = &[
        "KORUB", "SKHYB", "EWYB", "MUB", "SOXLB", "TSLAB", "PLTRB", "BNCB", "AXTIB", "SNXXB",
        "SNDKB", "LITEB", "AAOIB",
    ];
    let Some(base) = symbol.strip_suffix("USDT") else {
        return false;
    };
    let not_coin = match non_crypto().read().ok().as_deref() {
        Some(Some(set)) => set.contains(symbol),
        _ => STOCK_TOKENS.contains(&base),
    };
    !base.is_empty()
        && !STABLE.contains(&base)
        && !not_coin
        && !["UP", "DOWN", "BULL", "BEAR"]
            .iter()
            .any(|t| base.ends_with(t) && base.len() > t.len())
}

/// The coins worth scanning: [`tradable`] USDT pairs by 24h quote volume,
/// at most `max`.
pub fn universe(pairs: &[(String, f64)], max: usize) -> Vec<String> {
    let mut v: Vec<&(String, f64)> = pairs
        .iter()
        .filter(|(s, vol)| *vol > 0.0 && tradable(s))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.into_iter().take(max).map(|(s, _)| s.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 1_000_000) as f64 / 1_000_000.0 - 0.5
        }
    }

    fn series(name: &str, closes: &[f64], volume: f64) -> Series {
        Series {
            symbol: name.into(),
            candles: closes
                .iter()
                .enumerate()
                .map(|(i, &c)| Ohlc {
                    open: c,
                    high: c * 1.001,
                    low: c * 0.999,
                    close: c,
                    volume,
                    time_ms: i as i64 * 3_600_000,
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn a_tied_pair_far_apart_is_found_and_noise_is_not() {
        let n = 500;
        let mut r = Rng(42);
        // A common walk; B follows it with a mean-reverting gap; C and D wander alone.
        let mut base = 100.0;
        let mut gap = 0.0;
        let (mut a, mut b, mut c, mut d) = (vec![], vec![], vec![], vec![]);
        let (mut pc, mut pd) = (50.0, 20.0);
        for t in 0..n {
            base *= 1.0 + r.next() * 0.02;
            gap = gap * 0.9 + r.next() * 0.01;
            if t == n - 1 {
                gap = 0.06; // pulled far apart right now
            }
            a.push(base * (1.0 + gap));
            b.push(base * 0.5);
            pc *= 1.0 + r.next() * 0.02;
            pd *= 1.0 + r.next() * 0.02;
            c.push(pc);
            d.push(pd);
        }
        let s = scan(
            &[
                series("AUSDT", &a, 10.0),
                series("BUSDT", &b, 10.0),
                series("CUSDT", &c, 10.0),
                series("DUSDT", &d, 10.0),
            ],
            0.7,
        );
        assert_eq!(s.coins, 4);
        assert_eq!(s.pairs_tried, 6);
        assert_eq!(
            s.pairs.len(),
            1,
            "{:?}",
            s.pairs
                .iter()
                .map(|p| (&p.a, &p.b, p.z))
                .collect::<Vec<_>>()
        );
        let p = &s.pairs[0];
        assert_eq!((p.a.as_str(), p.b.as_str()), ("AUSDT", "BUSDT"));
        assert!(p.z > PAIR_Z, "{}", p.z);
        // A is rich: sell A, buy B.
        assert_eq!((p.long.as_str(), p.short.as_str()), ("BUSDT", "AUSDT"));
        assert!(p.half_life < 20.0 && p.t_newer < PAIR_T_NEWER);
        use crate::watchlists::ScanKind;
        assert_eq!(s.members(ScanKind::PairsLong), vec!["BUSDT".to_string()]);
        assert_eq!(s.members(ScanKind::PairsShort), vec!["AUSDT".to_string()]);
    }

    #[test]
    fn a_pair_that_moved_to_a_new_level_is_not_a_stretch() {
        let n = 500;
        let mut r = Rng(42);
        let mut base = 100.0;
        let mut gap = 0.0;
        let (mut a, mut b) = (vec![], vec![]);
        for t in 0..n {
            base *= 1.0 + r.next() * 0.02;
            gap = gap * 0.9 + r.next() * 0.01;
            // From 80 % of the history on, the gap lives at a new level.
            let level = if t >= n * 8 / 10 { -0.03 } else { 0.0 };
            a.push(base * (1.0 + gap + level));
            b.push(base * 0.5);
        }
        let s = scan(&[series("AUSDT", &a, 10.0), series("BUSDT", &b, 10.0)], 0.7);
        assert!(
            s.pairs.is_empty(),
            "{:?}",
            s.pairs.iter().map(|p| p.z).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stretched_coins_and_falling_knives() {
        let n = 200;
        let flat: Vec<f64> = (0..n).map(|i| 100.0 + (i as f64 * 0.7).sin()).collect();
        let mut up = flat.clone();
        *up.last_mut().unwrap() = 108.0;
        let mut knife = flat.clone();
        for (k, v) in knife.iter_mut().enumerate().skip(n - 24) {
            *v = 100.0 * (1.0 - 0.006 * (k + 24 - n) as f64);
        }
        let mut ks = series("KUSDT", &knife, 10.0);
        for c in ks.candles.iter_mut().skip(n - 4) {
            c.volume = 40.0;
        }
        let s = scan(
            &[series("FUSDT", &flat, 10.0), series("UUSDT", &up, 10.0), ks],
            0.7,
        );
        assert_eq!(
            s.up.iter().map(|x| x.symbol.as_str()).collect::<Vec<_>>(),
            ["UUSDT"]
        );
        assert_eq!(s.knives.len(), 1, "{:?}", s.knives);
        assert!(s.knives[0].day_pct < -8.0 && s.knives[0].volume_x > 1.5);
    }

    #[test]
    fn on_15m_a_knife_is_measured_over_a_real_day() {
        // A steady 15% fall over the last 96 candles: a day on 15m candles.
        // Measured as 24 candles (the hourly rule) it is only 3.75% "a day".
        let n = 400;
        let flat: Vec<f64> = (0..n).map(|i| 100.0 + (i as f64 * 0.7).sin()).collect();
        let mut v = flat.clone();
        for (k, x) in v.iter_mut().enumerate().skip(n - 96) {
            *x = 100.0 * (1.0 - 0.15 * (k + 96 - n) as f64 / 96.0);
        }
        let mut k = series("KUSDT", &v, 10.0);
        for c in k.candles.iter_mut().skip(n - 16) {
            c.volume = 40.0;
        }
        let both = [series("FUSDT", &flat, 10.0), k];
        let on_15m = scan_tf(&both, 0.7, 96);
        assert_eq!(on_15m.knives.len(), 1, "{:?}", on_15m.knives);
        assert!(on_15m.knives[0].day_pct < -12.0, "{:?}", on_15m.knives);
        assert!(
            scan(&both, 0.7).knives.is_empty(),
            "24 candles are six hours here"
        );
    }

    #[test]
    fn gap_history_counts_earlier_wide_gaps_and_how_they_ended() {
        // Two earlier excursions to +3: one closes in 3 candles, one sits
        // there; one on the other side does not count; now +2.8.
        let mut z = vec![0.0; 60];
        z[10] = 3.0;
        z[11] = 2.0;
        z[12] = 1.0;
        z[13] = 0.2;
        for v in z.iter_mut().take(36).skip(25) {
            *v = 3.1;
        }
        z[40] = -3.5;
        let h = gap_history(&z, 2.8, 2.0, 5);
        assert_eq!(h.horizon, 4);
        assert_eq!((h.times, h.closed), (2, 1), "{h:?}");
        assert_eq!(h.median_candles, 3.0);
        // Too close to the end for a horizon: not counted.
        let mut late = vec![0.0; 20];
        late[13] = 3.0;
        assert_eq!(gap_history(&late, 2.5, 2.0, 5).times, 0);
        // The other side.
        assert_eq!(gap_history(&z, -2.5, 2.0, 5).times, 1);
    }

    #[test]
    fn pair_legs_hedge_with_beta_or_equal_dollars() {
        let p = PairIdea {
            a: "AUSDT".into(),
            b: "BUSDT".into(),
            r: 0.9,
            beta: 1.5,
            z: 2.5,
            half_life: 10.0,
            t_older: -4.0,
            t_newer: -3.0,
            // Spread high: a is rich, so buy b and sell a.
            long: "BUSDT".into(),
            short: "AUSDT".into(),
            z_path: vec![],
            history: GapHistory::default(),
        };
        let [buy, sell] = p.legs(100.0, true);
        assert_eq!(buy, ("BUSDT".to_string(), true, 150.0));
        assert_eq!(sell, ("AUSDT".to_string(), false, 100.0));
        let [buy, sell] = p.legs(100.0, false);
        assert_eq!((buy.2, sell.2), (100.0, 100.0));
    }

    #[test]
    fn the_universe_is_liquid_usdt_coins_without_stables() {
        let p = |s: &str, v: f64| (s.to_string(), v);
        let u = universe(
            &[
                p("BTCUSDT", 9.0),
                p("USDCUSDT", 99.0),
                p("ETHUSDT", 8.0),
                p("BTCUPUSDT", 50.0),
                p("ETHBTC", 70.0),
                p("SOLUSDT", 7.0),
                p("DEADUSDT", 0.0),
            ],
            2,
        );
        assert_eq!(u, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    }

    #[test]
    fn the_listings_decide_what_is_not_a_coin() {
        // A set that still holds the fallback's names keeps other tests true
        // whichever runs first (the set is process-wide).
        set_non_crypto(
            ["KORUBUSDT", "SOXLBUSDT", "TSLAUSDT", "NEWSTKBUSDT"]
                .map(String::from)
                .into(),
        );
        assert!(!tradable("NEWSTKBUSDT"), "a stock token no list named");
        assert!(!tradable("TSLAUSDT"), "a stock perpetual");
        assert!(tradable("BTCUSDT") && tradable("MUBARAKUSDT"));
    }

    #[test]
    fn pull_back_tells_a_returning_series_from_a_walk() {
        let mut r = Rng(7);
        let mut ar = vec![0.0];
        let mut walk = vec![0.0];
        for _ in 0..400 {
            let last = *ar.last().unwrap();
            ar.push(last * 0.8 + r.next());
            let w = *walk.last().unwrap();
            walk.push(w + r.next());
        }
        let (t_ar, hl) = pull_back(&ar);
        let (t_walk, _) = pull_back(&walk);
        assert!(t_ar < -5.0 && (2.0..5.0).contains(&hl), "{t_ar} {hl}");
        assert!(t_walk > -3.0, "{t_walk}");
    }

    #[test]
    fn tradable_skips_stablecoins_and_stock_tokens() {
        assert!(tradable("BTCUSDT") && tradable("SUIUSDT"));
        assert!(!tradable("USDCUSDT") && !tradable("BTCUPUSDT"));
        assert!(!tradable("KORUBUSDT") && !tradable("SOXLBUSDT"));
        assert!(!tradable("BTCUSDC"));
    }
}
