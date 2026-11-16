//! Live ideas: what the whole market is doing right now, from the ticker
//! stream alone (`crate::movers`, no REST): the biggest moves over the last
//! minutes, which coins have moved as one this hour, which coin lags the
//! one it usually follows, and groups that trade like a single coin.
//!
//! Everything here is **today's** picture: the correlations are over at most
//! sixty one-minute changes, so they say what is moving together now, not
//! what is linked by history.  The rows say so, and `e` on a row hands the
//! question to the judge, which has the history.

use std::collections::HashMap;

use crate::movers::{Movers, Return};

/// The kv key the scanning window saves the picture under (JSON [`Live`]).
pub const KV_KEY: &str = "live_report";
/// The windows of the rankings (minutes); the longest the history has is
/// the one the rows rank by.
pub const WINDOWS: [u32; 3] = [5, 15, 60];
/// Most rows per ranking.
pub const TOP: usize = 10;
/// Minutes of one-minute changes the correlations want at most / at least.
pub const CORR_MINUTES: usize = 60;
pub const CORR_MIN_MINUTES: usize = 15;
/// A link is listed from this |r| on; a group forms from this r on.
pub const LINK_MIN_R: f64 = 0.7;
pub const GROUP_MIN_R: f64 = 0.8;
/// A laggard's link with its leader must be at least this strong.
pub const LAG_MIN_R: f64 = 0.6;
pub const MAX_LINKS: usize = 20;
pub const MAX_LAGGARDS: usize = 10;
pub const MAX_GROUPS: usize = 10;
pub const MIN_GROUP: usize = 3;
pub const MAX_GROUP: usize = 20;
/// A spread is kept from this r on and this |z| on; this many at most.
pub const SPREAD_MIN_R: f64 = 0.8;
pub const SPREAD_MIN_Z: f64 = 1.5;
pub const MAX_SPREADS: usize = 200;
/// The coin a group is measured against.
pub const HEDGE: &str = "BTCUSDT";
/// Synthetics are built tier by tier at these r's; this many at most.
pub const SYN_TIERS: [f64; 3] = [0.85, 0.75, 0.65];
pub const MAX_SYNTHETICS: usize = 40;
/// A synthetic pair is kept from this r on; this many at most.
pub const SYN_PAIR_MIN_R: f64 = 0.6;
pub const MAX_SYN_PAIRS: usize = 100;

/// A coin in the gainers or losers ranking.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Move {
    pub symbol: String,
    /// The window the ranking uses and the move over it.
    pub window_min: u32,
    pub pct: f64,
    pub volume_x: f64,
    pub price: f64,
    /// The move over each of [`WINDOWS`] that the history reaches.
    pub pct_5: Option<f64>,
    pub pct_15: Option<f64>,
    pub pct_60: Option<f64>,
}

/// Two coins whose one-minute changes have moved as one (or opposite).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub a: String,
    pub b: String,
    pub r: f64,
    /// Minutes of changes behind `r`.
    pub minutes: usize,
    /// Each one's move over those minutes, %.
    pub pct_a: f64,
    pub pct_b: f64,
}

/// A coin that usually moves with `leader` (this hour) but has not followed
/// its latest move.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Laggard {
    pub leader: String,
    pub follower: String,
    pub r: f64,
    pub minutes: usize,
    pub window_min: u32,
    pub lead_pct: f64,
    pub follow_pct: f64,
}

impl Laggard {
    /// How far the follower is behind, in the leader's direction (%).
    pub fn gap_pct(&self) -> f64 {
        self.lead_pct - self.follow_pct
    }
}

/// Two coins that moved as one this hour whose price ratio is now far
/// from its hour's mean: a stat-arb candidate among today's movers.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Spread {
    pub a: String,
    pub b: String,
    pub r: f64,
    pub minutes: usize,
    /// The log ratio ln(a/b) now, in sd of the hour's ratio (positive: a
    /// rich against b).
    pub z: f64,
    /// The sd of the log ratio over the hour, %.
    pub sd_pct: f64,
    pub pct_a: f64,
    pub pct_b: f64,
}

impl Spread {
    /// The cheap leg to buy and the rich one to sell.
    pub fn long_short(&self) -> (&str, &str) {
        if self.z > 0.0 {
            (&self.b, &self.a)
        } else {
            (&self.a, &self.b)
        }
    }
}

/// Coins whose one-minute changes have moved as one this hour.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Group {
    pub members: Vec<String>,
    /// Mean r between the members.
    pub r_mean: f64,
    pub minutes: usize,
    /// The mean move of the members over those minutes, %.
    pub pct: f64,
    /// The group's mean (each coin from its hour's start) against
    /// [`HEDGE`] now, in sd of the hour: the synthetic against BTC.
    #[serde(default)]
    pub z_hedge: Option<f64>,
}

/// A synthetic instrument: up to [`MAX_GROUP`] coins that moved as one
/// this hour, traded as one (equal dollars a member).  Built every pass
/// from the correlations, so its members can change between passes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Synthetic {
    pub id: usize,
    /// "SOL+7": the centre and the count of the others.
    pub name: String,
    pub members: Vec<String>,
    /// The r every member pair clears (the tier it was built at).
    pub tier_r: f64,
    pub r_mean: f64,
    pub minutes: usize,
    /// The mean move of the members over the hour, %.
    pub pct: f64,
    /// Against [`HEDGE`] now, in sd of the hour.
    pub z_hedge: Option<f64>,
}

/// Two synthetics that moved as one this hour whose ratio is stretched:
/// one synthetic against another, up to forty legs.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SynPair {
    pub a: String,
    pub b: String,
    pub a_members: Vec<String>,
    pub b_members: Vec<String>,
    pub r: f64,
    /// ln(A/B) now in sd of the hour (positive: A rich against B).
    pub z: f64,
    pub sd_pct: f64,
    pub minutes: usize,
    pub pct_a: f64,
    pub pct_b: f64,
}

/// A synthetic's name and members, borrowed.
pub type SynSide<'a> = (&'a str, &'a [String]);

impl SynPair {
    /// The cheap synthetic to buy and the rich one to sell: (name, members) each.
    pub fn long_short(&self) -> (SynSide<'_>, SynSide<'_>) {
        if self.z > 0.0 {
            ((&self.b, &self.b_members), (&self.a, &self.a_members))
        } else {
            ((&self.a, &self.a_members), (&self.b, &self.b_members))
        }
    }
}

/// The live picture at `at_ms`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Live {
    pub at_ms: i64,
    /// Coins with enough history for the rankings / the correlations.
    pub coins: usize,
    pub corr_coins: usize,
    /// Minutes of one-minute changes the correlations use (0: too early).
    pub minutes: usize,
    pub gainers: Vec<Move>,
    pub losers: Vec<Move>,
    pub links: Vec<Link>,
    pub laggards: Vec<Laggard>,
    pub groups: Vec<Group>,
    /// Stretched ratios among the linked pairs, the widest first.
    #[serde(default)]
    pub spreads: Vec<Spread>,
    /// Pairs the correlations covered (every two coins with history).
    #[serde(default)]
    pub pairs_tried: usize,
    /// Synthetic instruments built this pass, the biggest first, and the
    /// stretched pairs among them, the widest first.
    #[serde(default)]
    pub synthetics: Vec<Synthetic>,
    #[serde(default)]
    pub syn_pairs: Vec<SynPair>,
}

impl Live {
    pub fn is_empty(&self) -> bool {
        self.gainers.is_empty()
            && self.losers.is_empty()
            && self.links.is_empty()
            && self.laggards.is_empty()
            && self.groups.is_empty()
            && self.spreads.is_empty()
            && self.synthetics.is_empty()
    }
}

/// The live picture from the movers' history.
pub fn build(m: &Movers, now_ms: i64) -> Live {
    let by_window: Vec<(u32, Vec<Return>)> =
        WINDOWS.iter().map(|&w| (w, m.returns(w, now_ms))).collect();
    let (_, span_ms) = m.coverage();
    let minutes = ((span_ms / 60_000) as usize).min(CORR_MINUTES);
    let closes = if minutes >= CORR_MIN_MINUTES {
        m.minute_closes(minutes, now_ms)
    } else {
        Vec::new()
    };
    from_parts(&by_window, &closes, minutes, now_ms)
}

/// The pure part: rankings from the per-window returns, links / laggards /
/// groups from the per-minute closes (`minutes` long).
pub fn from_parts(
    by_window: &[(u32, Vec<Return>)],
    closes: &[(String, Vec<f64>)],
    minutes: usize,
    now_ms: i64,
) -> Live {
    let (gainers, losers, coins) = rankings(by_window);
    let (links, laggards, groups, spreads, synthetics, syn_pairs, corr_coins) =
        if closes.len() >= 2 && minutes >= 2 {
            let corr = correlations(closes);
            let pct: HashMap<&str, f64> = closes
                .iter()
                .filter_map(|(s, c)| {
                    let (f, l) = (*c.first()?, *c.last()?);
                    (f > 0.0).then(|| (s.as_str(), (l / f - 1.0) * 100.0))
                })
                .collect();
            let links = top_links(&corr, &pct, minutes);
            let laggards = laggards(&corr, &gainers, &losers, by_window, minutes);
            let mut groups = groups(&corr, &pct, minutes);
            let hedge = closes
                .iter()
                .find(|(s, _)| s == HEDGE)
                .map(|(_, c)| c.as_slice());
            for g in &mut groups {
                g.z_hedge = hedge.and_then(|h| group_vs_hedge(&g.members, closes, h));
            }
            let spreads = spreads(&corr, closes, &pct, minutes);
            let (mut synthetics, series) = synthetics(&corr, closes, &pct, minutes);
            for syn in &mut synthetics {
                syn.z_hedge = hedge.and_then(|h| group_vs_hedge(&syn.members, closes, h));
            }
            let syn_pairs = syn_pairs(&synthetics, &series, minutes);
            (
                links,
                laggards,
                groups,
                spreads,
                synthetics,
                syn_pairs,
                closes.len(),
            )
        } else {
            (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
            )
        };
    Live {
        at_ms: now_ms,
        coins,
        corr_coins,
        minutes: if corr_coins > 0 { minutes } else { 0 },
        gainers,
        losers,
        links,
        laggards,
        groups,
        spreads,
        pairs_tried: corr_coins * corr_coins.saturating_sub(1) / 2,
        synthetics,
        syn_pairs,
    }
}

/// Mean and sd of a series (sd 0 when flat or short).
fn mean_sd(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    if n < 2.0 {
        return (v.first().copied().unwrap_or(0.0), 0.0);
    }
    let m = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    (m, var.sqrt())
}

/// The last value of a series in sd of the series: `None` when flat.
fn z_last(v: &[f64]) -> Option<f64> {
    let (m, sd) = mean_sd(v);
    let last = *v.last()?;
    (sd > 0.0 && sd.is_finite()).then(|| (last - m) / sd)
}

/// Every linked pair's log ratio over the hour, kept when it is stretched.
fn spreads(
    corr: &Matrix,
    closes: &[(String, Vec<f64>)],
    pct: &HashMap<&str, f64>,
    minutes: usize,
) -> Vec<Spread> {
    let n = corr.names.len();
    let mut out = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            let Some(r) = corr.get(i, j) else { continue };
            if r < SPREAD_MIN_R {
                continue;
            }
            let (a, b) = (&closes[i].1, &closes[j].1);
            let ratio: Vec<f64> = a
                .iter()
                .zip(b)
                .filter(|(x, y)| **x > 0.0 && **y > 0.0)
                .map(|(x, y)| (x / y).ln())
                .collect();
            if ratio.len() < CORR_MIN_MINUTES {
                continue;
            }
            let Some(z) = z_last(&ratio) else { continue };
            if z.abs() < SPREAD_MIN_Z {
                continue;
            }
            let (_, sd) = mean_sd(&ratio);
            out.push(Spread {
                a: corr.names[i].clone(),
                b: corr.names[j].clone(),
                r,
                minutes,
                z,
                sd_pct: sd * 100.0,
                pct_a: pct.get(corr.names[i].as_str()).copied().unwrap_or(0.0),
                pct_b: pct.get(corr.names[j].as_str()).copied().unwrap_or(0.0),
            });
        }
    }
    out.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
    out.truncate(MAX_SPREADS);
    out
}

/// The group's mean log move (from the hour's first close) less the
/// hedge's, now, in sd of that difference over the hour.
fn group_vs_hedge(members: &[String], closes: &[(String, Vec<f64>)], hedge: &[f64]) -> Option<f64> {
    let series: Vec<&[f64]> = members
        .iter()
        .filter_map(|m| {
            closes
                .iter()
                .find(|(s, _)| s == m)
                .map(|(_, c)| c.as_slice())
        })
        .collect();
    if series.is_empty() {
        return None;
    }
    let len = series
        .iter()
        .map(|c| c.len())
        .chain(std::iter::once(hedge.len()))
        .min()?;
    if len < CORR_MIN_MINUTES {
        return None;
    }
    let h0 = *hedge.first()?;
    let diff: Vec<f64> = (0..len)
        .map(|t| {
            let g = series.iter().map(|c| (c[t] / c[0]).ln()).sum::<f64>() / series.len() as f64;
            g - (hedge[t] / h0).ln()
        })
        .collect();
    z_last(&diff)
}

/// The rankings over the longest window the history reaches.
fn rankings(by_window: &[(u32, Vec<Return>)]) -> (Vec<Move>, Vec<Move>, usize) {
    let Some((w, rets)) = by_window.iter().rev().find(|(_, r)| !r.is_empty()) else {
        return (Vec::new(), Vec::new(), 0);
    };
    let lookup = |win: u32, sym: &str| -> Option<f64> {
        by_window
            .iter()
            .find(|(x, _)| *x == win)?
            .1
            .iter()
            .find(|r| r.symbol == sym)
            .map(|r| r.pct)
    };
    let mv = |r: &Return| Move {
        symbol: r.symbol.clone(),
        window_min: *w,
        pct: r.pct,
        volume_x: r.volume_x,
        price: r.price,
        pct_5: lookup(5, &r.symbol),
        pct_15: lookup(15, &r.symbol),
        pct_60: lookup(60, &r.symbol),
    };
    let gainers: Vec<Move> = rets
        .iter()
        .filter(|r| r.pct > 0.0)
        .take(TOP)
        .map(mv)
        .collect();
    let losers: Vec<Move> = rets
        .iter()
        .rev()
        .filter(|r| r.pct < 0.0)
        .take(TOP)
        .map(mv)
        .collect();
    (gainers, losers, rets.len())
}

/// Pearson r of the one-minute log changes of every pair, as a matrix over
/// `closes`' order (`None`: a flat series).
pub fn correlations(closes: &[(String, Vec<f64>)]) -> Matrix {
    let n = closes.len();
    let changes: Vec<Vec<f64>> = closes
        .iter()
        .map(|(_, c)| c.windows(2).map(|w| (w[1] / w[0]).ln()).collect())
        .collect();
    // Standardise once: r is then a dot product.
    let z: Vec<Option<Vec<f64>>> = changes
        .iter()
        .map(|x| {
            let k = x.len() as f64;
            if k < 2.0 {
                return None;
            }
            let m = x.iter().sum::<f64>() / k;
            let ss = x.iter().map(|v| (v - m).powi(2)).sum::<f64>();
            let sd = ss.sqrt();
            (ss > 0.0).then(|| x.iter().map(|v| (v - m) / sd).collect())
        })
        .collect();
    let mut r = vec![vec![None; n]; n];
    for i in 0..n {
        r[i][i] = Some(1.0);
        let Some(zi) = &z[i] else { continue };
        for j in i + 1..n {
            let Some(zj) = &z[j] else { continue };
            let v: f64 = zi.iter().zip(zj).map(|(a, b)| a * b).sum();
            r[i][j] = Some(v.clamp(-1.0, 1.0));
            r[j][i] = r[i][j];
        }
    }
    Matrix {
        names: closes.iter().map(|(s, _)| s.clone()).collect(),
        r,
    }
}

/// A correlation matrix with its coin names.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    pub names: Vec<String>,
    pub r: Vec<Vec<Option<f64>>>,
}

impl Matrix {
    fn index(&self, sym: &str) -> Option<usize> {
        self.names.iter().position(|s| s == sym)
    }
    pub fn get(&self, i: usize, j: usize) -> Option<f64> {
        self.r.get(i)?.get(j).copied().flatten()
    }
}

fn top_links(corr: &Matrix, pct: &HashMap<&str, f64>, minutes: usize) -> Vec<Link> {
    let n = corr.names.len();
    let mut v: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            if let Some(r) = corr.get(i, j) {
                if r.abs() >= LINK_MIN_R {
                    v.push((r, i, j));
                }
            }
        }
    }
    v.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
    v.into_iter()
        .take(MAX_LINKS)
        .map(|(r, i, j)| Link {
            a: corr.names[i].clone(),
            b: corr.names[j].clone(),
            r,
            minutes,
            pct_a: pct.get(corr.names[i].as_str()).copied().unwrap_or(0.0),
            pct_b: pct.get(corr.names[j].as_str()).copied().unwrap_or(0.0),
        })
        .collect()
}

/// For each ranked mover: the coin it has moved with most this hour that
/// has made less than half its move (in the same direction).
fn laggards(
    corr: &Matrix,
    gainers: &[Move],
    losers: &[Move],
    by_window: &[(u32, Vec<Return>)],
    minutes: usize,
) -> Vec<Laggard> {
    let mut out = Vec::new();
    for lead in gainers.iter().chain(losers) {
        let Some(li) = corr.index(&lead.symbol) else {
            continue;
        };
        let Some((_, rets)) = by_window.iter().find(|(w, _)| *w == lead.window_min) else {
            continue;
        };
        let mut best: Option<(f64, &Return)> = None;
        for (j, name) in corr.names.iter().enumerate() {
            if j == li {
                continue;
            }
            let Some(r) = corr.get(li, j) else { continue };
            if r < LAG_MIN_R {
                continue;
            }
            let Some(f) = rets.iter().find(|x| &x.symbol == name) else {
                continue;
            };
            // Behind: less than half the leader's move in its direction.
            let behind = if lead.pct > 0.0 {
                f.pct < lead.pct / 2.0
            } else {
                f.pct > lead.pct / 2.0
            };
            if behind && best.is_none_or(|b| r > b.0) {
                best = Some((r, f));
            }
        }
        if let Some((r, f)) = best {
            out.push(Laggard {
                leader: lead.symbol.clone(),
                follower: f.symbol.clone(),
                r,
                minutes,
                window_min: lead.window_min,
                lead_pct: lead.pct,
                follow_pct: f.pct,
            });
        }
    }
    out.sort_by(|a, b| b.gap_pct().abs().total_cmp(&a.gap_pct().abs()));
    out.truncate(MAX_LAGGARDS);
    out
}

/// Greedy groups at `threshold`: the strongest pairs first, a pair joins
/// its two coins' groups when every member of each is linked to the other
/// (r >= threshold).  Coins in `taken` are skipped; the coins of every
/// group of `MIN_GROUP` or more are marked taken.
fn greedy(corr: &Matrix, threshold: f64, taken: &mut [bool]) -> Vec<Vec<usize>> {
    let n = corr.names.len();
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        if taken[i] {
            continue;
        }
        for (j, &gone) in taken.iter().enumerate().skip(i + 1) {
            if gone {
                continue;
            }
            if let Some(r) = corr.get(i, j) {
                if r >= threshold {
                    pairs.push((r, i, j));
                }
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut group_of: Vec<Option<usize>> = vec![None; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let linked = |a: usize, b: usize| corr.get(a, b).is_some_and(|r| r >= threshold);
    for (_, i, j) in pairs {
        match (group_of[i], group_of[j]) {
            (None, None) => {
                group_of[i] = Some(groups.len());
                group_of[j] = Some(groups.len());
                groups.push(vec![i, j]);
            }
            (Some(g), None) => {
                if groups[g].len() < MAX_GROUP && groups[g].iter().all(|&m| linked(m, j)) {
                    groups[g].push(j);
                    group_of[j] = Some(g);
                }
            }
            (None, Some(g)) => {
                if groups[g].len() < MAX_GROUP && groups[g].iter().all(|&m| linked(m, i)) {
                    groups[g].push(i);
                    group_of[i] = Some(g);
                }
            }
            (Some(_), Some(_)) => {}
        }
    }
    let out: Vec<Vec<usize>> = groups
        .into_iter()
        .filter(|g| g.len() >= MIN_GROUP)
        .collect();
    for g in &out {
        for &i in g {
            taken[i] = true;
        }
    }
    out
}

/// Mean r inside a group of coin indices.
fn mean_r(corr: &Matrix, g: &[usize]) -> f64 {
    let mut rs = Vec::new();
    for (a, &i) in g.iter().enumerate() {
        for &j in &g[a + 1..] {
            if let Some(r) = corr.get(i, j) {
                rs.push(r);
            }
        }
    }
    rs.iter().sum::<f64>() / rs.len().max(1) as f64
}

/// Groups of coins moving as one (r >= GROUP_MIN_R), the biggest first.
fn groups(corr: &Matrix, pct: &HashMap<&str, f64>, minutes: usize) -> Vec<Group> {
    let mut taken = vec![false; corr.names.len()];
    let mut out: Vec<Group> = greedy(corr, GROUP_MIN_R, &mut taken)
        .into_iter()
        .map(|g| {
            let members: Vec<String> = g.iter().map(|&i| corr.names[i].clone()).collect();
            let moves: Vec<f64> = members
                .iter()
                .filter_map(|m| pct.get(m.as_str()).copied())
                .collect();
            Group {
                r_mean: mean_r(corr, &g),
                pct: moves.iter().sum::<f64>() / moves.len().max(1) as f64,
                minutes,
                members,
                z_hedge: None,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.r_mean.total_cmp(&a.r_mean))
    });
    out.truncate(MAX_GROUPS);
    out
}

/// Synthetic instruments: every coin in at most one, built tier by tier
/// ([`SYN_TIERS`]: the tightest groups first, then looser ones from the
/// coins left).  Each is named after its centre (the member linked most
/// to the rest) and the count of the others: "SOL+7".  Returns them with
/// their series: the mean log move of the members from the hour's first
/// close, one value a minute.
fn synthetics(
    corr: &Matrix,
    closes: &[(String, Vec<f64>)],
    pct: &HashMap<&str, f64>,
    minutes: usize,
) -> (Vec<Synthetic>, Vec<Vec<f64>>) {
    let n = corr.names.len();
    let mut taken = vec![false; n];
    let mut found: Vec<(f64, Vec<usize>)> = Vec::new();
    for &tier in &SYN_TIERS {
        // Loose coins join an earlier synthetic first, when they clear this
        // tier with every member of it; then the rest group among themselves.
        for (i, gone) in taken.iter_mut().enumerate() {
            if *gone {
                continue;
            }
            let joins = found.iter().position(|(_, g)| {
                g.len() < MAX_GROUP && g.iter().all(|&m| corr.get(m, i).is_some_and(|r| r >= tier))
            });
            if let Some(k) = joins {
                found[k].1.push(i);
                found[k].0 = found[k].0.min(tier);
                *gone = true;
            }
        }
        for g in greedy(corr, tier, &mut taken) {
            found.push((tier, g));
        }
    }
    found.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(b.0.total_cmp(&a.0)));
    found.truncate(MAX_SYNTHETICS);
    let len = closes.iter().map(|(_, c)| c.len()).min().unwrap_or(0);
    let mut syns = Vec::new();
    let mut series = Vec::new();
    for (id, (tier, g)) in found.into_iter().enumerate() {
        // The centre: the member with the highest mean r to the others.
        let centre = g
            .iter()
            .copied()
            .max_by(|&a, &b| {
                let ra = g.iter().filter_map(|&j| corr.get(a, j)).sum::<f64>();
                let rb = g.iter().filter_map(|&j| corr.get(b, j)).sum::<f64>();
                ra.total_cmp(&rb)
            })
            .unwrap_or(g[0]);
        let members: Vec<String> = g.iter().map(|&i| corr.names[i].clone()).collect();
        let moves: Vec<f64> = members
            .iter()
            .filter_map(|m| pct.get(m.as_str()).copied())
            .collect();
        let s: Vec<f64> = (0..len)
            .map(|t| {
                g.iter()
                    .map(|&i| {
                        let c = &closes[i].1;
                        if c[0] > 0.0 && c[t] > 0.0 {
                            (c[t] / c[0]).ln()
                        } else {
                            0.0
                        }
                    })
                    .sum::<f64>()
                    / g.len() as f64
            })
            .collect();
        syns.push(Synthetic {
            id,
            name: format!(
                "{}+{}",
                corr.names[centre].trim_end_matches("USDT"),
                g.len() - 1
            ),
            members,
            tier_r: tier,
            r_mean: mean_r(corr, &g),
            minutes,
            pct: moves.iter().sum::<f64>() / moves.len().max(1) as f64,
            z_hedge: None,
        });
        series.push(s);
    }
    (syns, series)
}

/// Pairs of synthetics that moved as one this hour (r >= SYN_PAIR_MIN_R on
/// their one-minute changes) whose ratio is stretched (|z| >= SPREAD_MIN_Z).
fn syn_pairs(syns: &[Synthetic], series: &[Vec<f64>], minutes: usize) -> Vec<SynPair> {
    if syns.len() < 2 || series.iter().any(|s| s.len() < CORR_MIN_MINUTES) {
        return Vec::new();
    }
    // Their correlations: the log series as closes.
    let as_closes: Vec<(String, Vec<f64>)> = syns
        .iter()
        .zip(series)
        .map(|(s, v)| (s.name.clone(), v.iter().map(|x| x.exp()).collect()))
        .collect();
    let corr = correlations(&as_closes);
    let mut out = Vec::new();
    for i in 0..syns.len() {
        for j in i + 1..syns.len() {
            let Some(r) = corr.get(i, j) else { continue };
            if r < SYN_PAIR_MIN_R {
                continue;
            }
            let diff: Vec<f64> = series[i]
                .iter()
                .zip(&series[j])
                .map(|(a, b)| a - b)
                .collect();
            let Some(z) = z_last(&diff) else { continue };
            if z.abs() < SPREAD_MIN_Z {
                continue;
            }
            let (_, sd) = mean_sd(&diff);
            out.push(SynPair {
                a: syns[i].name.clone(),
                b: syns[j].name.clone(),
                a_members: syns[i].members.clone(),
                b_members: syns[j].members.clone(),
                r,
                z,
                sd_pct: sd * 100.0,
                minutes,
                pct_a: syns[i].pct,
                pct_b: syns[j].pct,
            });
        }
    }
    out.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
    out.truncate(MAX_SYN_PAIRS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic noise series (no crates): a linear congruential walk.
    fn noise(seed: u64, n: usize) -> Vec<f64> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((x >> 33) as f64 / (1u64 << 31) as f64) - 0.5
            })
            .collect()
    }

    /// Closes from one-minute changes: `common` shared, `own` private, each
    /// scaled; starts at 100.
    fn series(common: &[f64], own: &[f64], c_scale: f64, o_scale: f64) -> Vec<f64> {
        let mut p = 100.0;
        let mut v = vec![p];
        for (c, o) in common.iter().zip(own) {
            p *= 1.0 + (c * c_scale + o * o_scale) / 100.0;
            v.push(p);
        }
        v
    }

    fn ret(sym: &str, pct: f64) -> Return {
        Return {
            symbol: sym.into(),
            pct,
            volume_x: 2.0,
            price: 10.0,
            quote_24h: 5e6,
        }
    }

    #[test]
    fn shared_noise_links_and_private_noise_does_not() {
        let common = noise(1, 60);
        let a = series(&common, &noise(2, 60), 1.0, 0.1);
        let b = series(&common, &noise(3, 60), 1.0, 0.1);
        let c = series(&common, &noise(4, 60), 1.0, 0.1);
        let d = series(&noise(5, 60), &noise(6, 60), 1.0, 0.1);
        let opp = series(&common, &noise(7, 60), -1.0, 0.1);
        let flat = vec![5.0; 61];
        let closes = vec![
            ("AAAUSDT".to_string(), a),
            ("BBBUSDT".to_string(), b),
            ("CCCUSDT".to_string(), c),
            ("DDDUSDT".to_string(), d),
            ("FLTUSDT".to_string(), flat),
            ("OPPUSDT".to_string(), opp),
        ];
        let m = correlations(&closes);
        assert!(m.get(0, 1).unwrap() > 0.9, "{:?}", m.get(0, 1));
        assert!(m.get(0, 3).unwrap().abs() < 0.3, "{:?}", m.get(0, 3));
        assert!(m.get(0, 5).unwrap() < -0.9, "{:?}", m.get(0, 5));
        assert!(m.get(0, 4).is_none(), "a flat series has no r");
        let live = from_parts(&[], &closes, 60, 1);
        assert_eq!(live.minutes, 60);
        assert_eq!(live.corr_coins, 6);
        // Links: the three linked ones and the opposite one, the unlinked never.
        assert!(live.links.len() >= 4, "{:?}", live.links);
        assert!(live.links.iter().all(|l| l.r.abs() >= LINK_MIN_R));
        assert!(!live
            .links
            .iter()
            .any(|l| l.a == "DDDUSDT" || l.b == "DDDUSDT"));
        assert!(live.links.iter().any(|l| l.r < 0.0 && l.b == "OPPUSDT"));
        // One group of the three that share the noise.
        assert_eq!(live.groups.len(), 1, "{:?}", live.groups);
        let mut g = live.groups[0].members.clone();
        g.sort();
        assert_eq!(g, ["AAAUSDT", "BBBUSDT", "CCCUSDT"]);
        assert!(live.groups[0].r_mean > 0.9);
        assert!(
            live.gainers.is_empty() && live.laggards.is_empty(),
            "no returns given"
        );
    }

    #[test]
    fn rankings_use_the_longest_window_and_laggards_trail_their_leader() {
        let common = noise(11, 60);
        let a = series(&common, &noise(12, 60), 1.0, 0.1);
        let b = series(&common, &noise(13, 60), 1.0, 0.1);
        let closes = vec![("AAAUSDT".to_string(), a), ("BBBUSDT".to_string(), b)];
        let by_window = vec![
            (
                5,
                vec![
                    ret("AAAUSDT", 1.0),
                    ret("BBBUSDT", 0.9),
                    ret("ZZZUSDT", -0.2),
                ],
            ),
            (
                15,
                vec![
                    ret("AAAUSDT", 6.0),
                    ret("BBBUSDT", 1.0),
                    ret("ZZZUSDT", -4.0),
                ],
            ),
            (60, vec![]),
        ];
        let live = from_parts(&by_window, &closes, 60, 7);
        assert_eq!(live.at_ms, 7);
        assert_eq!(live.coins, 3);
        assert_eq!(live.gainers.len(), 2);
        assert_eq!(live.gainers[0].symbol, "AAAUSDT");
        assert_eq!(live.gainers[0].window_min, 15);
        assert_eq!(live.gainers[0].pct_5, Some(1.0));
        assert_eq!(live.gainers[0].pct_60, None);
        assert_eq!(live.losers.len(), 1);
        assert_eq!(live.losers[0].symbol, "ZZZUSDT");
        // BBB moves with AAA (r > 0.6) but made 1% to AAA's 6%: a laggard.
        assert_eq!(live.laggards.len(), 1, "{:?}", live.laggards);
        let l = &live.laggards[0];
        assert_eq!(
            (l.leader.as_str(), l.follower.as_str()),
            ("AAAUSDT", "BBBUSDT")
        );
        assert!((l.gap_pct() - 5.0).abs() < 1e-9);
        // Nothing is a laggard of itself; ZZZ has no closes, so no laggard.
        assert!(live.laggards.iter().all(|l| l.leader != l.follower));
        // Too little history for correlations: rankings only.
        let early = from_parts(&by_window, &closes, 0, 7);
        assert_eq!(early.minutes, 0);
        assert!(early.links.is_empty() && early.laggards.is_empty());
        assert_eq!(early.gainers.len(), 2);
        assert!(!early.is_empty() && Live::default().is_empty());
    }

    #[test]
    fn hundreds_of_coins_correlate_in_milliseconds() {
        let common = noise(21, 60);
        let closes: Vec<(String, Vec<f64>)> = (0..400)
            .map(|i| {
                (
                    format!("C{i:03}USDT"),
                    series(&common, &noise(100 + i as u64, 60), 0.5, 1.0),
                )
            })
            .collect();
        let t = std::time::Instant::now();
        let live = from_parts(&[], &closes, 60, 1);
        let took = t.elapsed();
        assert_eq!(live.corr_coins, 400);
        assert!(took.as_millis() < 2000, "{took:?}");
        let json = serde_json::to_string(&live).unwrap();
        let back: Live = serde_json::from_str(&json).unwrap();
        assert_eq!(back, live);
    }

    #[test]
    fn a_stretched_ratio_among_linked_coins_is_a_spread_and_a_group_is_measured_against_btc() {
        // A and B share one noise; B gets an extra drift in the last
        // minutes, so ln(A/B) ends far from its mean.
        let common = noise(5, 60);
        let flat = vec![0.0; 60];
        let a = series(&common, &flat, 1.0, 0.0);
        let mut b = series(&common, &flat, 1.0, 0.0);
        for (k, p) in b.iter_mut().enumerate().skip(50) {
            *p *= 1.0 + 0.004 * (k - 49) as f64;
        }
        let c = series(&noise(9, 60), &flat, 1.0, 0.0);
        let btc = series(&noise(11, 60), &flat, 1.0, 0.0);
        let closes = vec![
            ("AUSDT".to_string(), a),
            ("BUSDT".to_string(), b),
            ("CUSDT".to_string(), c),
            (HEDGE.to_string(), btc),
        ];
        let corr = correlations(&closes);
        let pct = HashMap::new();
        let sp = spreads(&corr, &closes, &pct, 60);
        assert_eq!(sp.len(), 1, "{sp:?}");
        assert_eq!((sp[0].a.as_str(), sp[0].b.as_str()), ("AUSDT", "BUSDT"));
        assert!(sp[0].z < -SPREAD_MIN_Z, "B ran ahead: A cheap, {}", sp[0].z);
        assert_eq!(sp[0].long_short(), ("AUSDT", "BUSDT"));
        assert!(sp[0].sd_pct > 0.0);
        // Group of A and B against BTC: a z, since both have 60 minutes.
        let members = vec!["AUSDT".to_string(), "BUSDT".to_string()];
        let z = group_vs_hedge(&members, &closes, &closes[3].1).unwrap();
        assert!(z.is_finite());
        assert!(group_vs_hedge(&["ZUSDT".to_string()], &closes, &closes[3].1).is_none());
        // Through from_parts: the picture carries the spread and the count of pairs.
        let by_window = vec![(15u32, vec![ret("AUSDT", 1.0), ret("BUSDT", 6.0)])];
        let live = from_parts(&by_window, &closes, 60, 0);
        assert_eq!(live.spreads.len(), 1);
        assert_eq!(live.pairs_tried, 6);
        let json = serde_json::to_string(&live).unwrap();
        assert_eq!(serde_json::from_str::<Live>(&json).unwrap(), live);
        // An old report without the new fields still loads.
        let old: Live = serde_json::from_str(
            r#"{"at_ms":0,"coins":0,"corr_coins":0,"minutes":0,"gainers":[],"losers":[],"links":[],"laggards":[],"groups":[{"members":["AUSDT","BUSDT","CUSDT"],"r_mean":0.9,"minutes":30,"pct":1.0}]}"#,
        )
        .unwrap();
        assert_eq!(old.groups[0].z_hedge, None);
        assert!(old.spreads.is_empty());
    }

    #[test]
    fn synthetics_take_every_coin_once_by_tiers_and_a_stretched_pair_of_them_shows() {
        // Two families of five coins each (tight inside, r ~ 0.9), loosely
        // linked to each other; family B drifts away in the last minutes.
        let fam_a = noise(21, 80);
        let fam_b = noise(22, 80);
        let shared = noise(23, 80);
        let flat = vec![0.0; 80];
        let mut closes: Vec<(String, Vec<f64>)> = Vec::new();
        for k in 0..5 {
            let own = noise(100 + k, 80);
            let mixed: Vec<f64> = fam_a
                .iter()
                .zip(&shared)
                .map(|(a, s)| 0.5 * a + s)
                .collect();
            closes.push((format!("A{k}USDT"), series(&mixed, &own, 1.0, 0.25)));
        }
        for k in 0..5 {
            let own = noise(200 + k, 80);
            let mixed: Vec<f64> = fam_b
                .iter()
                .zip(&shared)
                .map(|(b, s)| 0.5 * b + s)
                .collect();
            let mut v = series(&mixed, &own, 1.0, 0.25);
            for (t, p) in v.iter_mut().enumerate().skip(70) {
                *p *= 1.0 + 0.003 * (t - 69) as f64;
            }
            closes.push((format!("B{k}USDT"), v));
        }
        closes.push((
            "LONEUSDT".to_string(),
            series(&noise(300, 80), &flat, 1.0, 0.0),
        ));
        let corr = correlations(&closes);
        let pct = HashMap::new();
        let (syns, series_out) = synthetics(&corr, &closes, &pct, 80);
        assert_eq!(
            syns.len(),
            2,
            "{:?}",
            syns.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert_eq!(series_out[0].len(), 81);
        let all: Vec<&String> = syns.iter().flat_map(|s| s.members.iter()).collect();
        assert_eq!(
            all.len(),
            10,
            "every family coin once, the lone one in none"
        );
        assert!(syns
            .iter()
            .all(|s| s.name.ends_with("+4") && s.r_mean > 0.5));
        assert!(syns.iter().all(|s| SYN_TIERS.contains(&s.tier_r)));
        let pairs = syn_pairs(&syns, &series_out, 80);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        let p = &pairs[0];
        assert!(p.r >= SYN_PAIR_MIN_R && p.z.abs() >= SPREAD_MIN_Z, "{p:?}");
        let ((long, lm), (short, sm)) = p.long_short();
        assert_eq!(lm.len() + sm.len(), 10);
        assert_ne!(long, short);
        // B ran ahead: B rich, so B is sold.
        assert!(short.starts_with('B'), "{short} {}", p.z);
        // Through from_parts, with the hedge: a z against BTC for each.
        let mut with_btc = closes.clone();
        with_btc.push((HEDGE.to_string(), series(&noise(400, 80), &flat, 1.0, 0.0)));
        let live = from_parts(&[], &with_btc, 80, 0);
        assert_eq!(live.synthetics.len(), 2);
        assert!(live.synthetics.iter().all(|s| s.z_hedge.is_some()));
        assert_eq!(live.syn_pairs.len(), 1);
        // The report survives the trip through kv (floats to the last
        // digit differ, so the shape is what is checked).
        let json = serde_json::to_string(&live).unwrap();
        let back: Live = serde_json::from_str(&json).unwrap();
        assert_eq!(back.synthetics.len(), 2);
        assert_eq!(back.syn_pairs[0].a_members, live.syn_pairs[0].a_members);
        assert_eq!(back.synthetics[0].name, live.synthetics[0].name);
    }
}
