//! Every live rule replayed on history with the code that trades it.
//!
//! One close a minute per coin (the scanning window's long backfill, kept
//! in `market.db`) is walked forward.  Every `step` minutes the live
//! picture is rebuilt from the last hour exactly as the market listener
//! builds it (`live_ideas::from_parts`), each rule is asked for its
//! candidates (`autobasket::candidates`) and for the ones due now
//! (`autobasket::due`: room, cooling, one position per coin), and each
//! basket opened is followed minute by minute to its exit (the rule's own
//! target / stop / clock, or the finding's).  Fees as the paper ledger
//! charges these baskets (a short leg puts them on perpetuals): a maker
//! fill in (2 bps a leg) unless the finding enters at market (taker, 5), a
//! maker fill out on the target and the clock (the basket rests limits
//! first) and a taker fill on the stop.  Entries fill at the signal
//! minute's close: optimistic, said wherever it is shown.
//!
//! The guard (`verdict`): a rule opens baskets only with settings whose
//! replay made money after fees on the newest third of the history, and
//! [`full_report`] looks for such settings on the older two thirds only.
//! Volumes are not replayed (the picture's filters do not use them).
//! Pure: series in, numbers out.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::autobasket::{self, Rule};
use crate::basket::Entry;
use crate::i18n::pick;
use crate::live_ideas::{self, CORR_MINUTES, WINDOWS};
use crate::movers::Return;

pub const KV_KEY: &str = "replay_rules";
pub const STEP: usize = 5;
/// The sources replayed from minutes (the others have their own:
/// `spread_test`, `carry`; options and cross-exchange need quotes).
pub const SOURCES: [&str; 4] = ["laggards", "spreads", "groups", "synthetic pairs"];
/// What a fill costs, bps of its dollars: the maker and taker rates, and
/// whether a basket leaving on its clock rests limits first (the live
/// basket does: market only after the wait) or crosses at once.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Costs {
    pub maker_bps: f64,
    pub taker_bps: f64,
    pub clock_as_maker: bool,
}

impl Default for Costs {
    fn default() -> Self {
        Costs {
            maker_bps: crate::futures::FEE_MAKER_BPS,
            taker_bps: crate::futures::FEE_TAKER_BPS,
            clock_as_maker: true,
        }
    }
}

/// Baskets the older part must hold before a setting can be chosen there,
/// and the newest third before its verdict counts.
pub const MIN_TUNE_TRADES: usize = 10;
pub const MIN_VERDICT_TRADES: usize = 10;
/// What a basket must keep on the newest third after fees, %: a margin for
/// the replay's optimistic fills (at the signal's close) and for picking the
/// best of twelve settings on the older part.
pub const MIN_EDGE_PCT: f64 = 0.05;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleTest {
    /// The source's short name and its rule in words.
    pub short: String,
    pub label: String,
    /// The rule replayed (its id: 0 for a default or a tried setting) and
    /// its settings.
    #[serde(default)]
    pub rule_id: u64,
    #[serde(default)]
    pub kind: Option<autobasket::Kind>,
    #[serde(default)]
    pub exit: Option<crate::basket::ExitRule>,
    pub trades: usize,
    pub wins: usize,
    pub by_target: usize,
    pub by_stop: usize,
    pub by_clock: usize,
    /// Mean and worst net of a basket, % of its dollars.
    pub mean_pct: f64,
    /// The mean before fees, and the fees: is there an edge to pay them from?
    #[serde(default)]
    pub mean_gross_pct: f64,
    #[serde(default)]
    pub mean_fees_pct: f64,
    pub worst_pct: f64,
    /// The same for baskets opened in the older two thirds and the newest third.
    pub older_pct: f64,
    pub older_trades: usize,
    pub newer_pct: f64,
    pub newer_trades: usize,
}

impl RuleTest {
    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }

    /// Can the rule be trusted on this history?  Enough trades, a positive
    /// mean, and both parts positive.
    pub fn trusted(&self) -> bool {
        self.trades >= 30 && self.mean_pct > 0.0 && self.older_pct > 0.0 && self.newer_pct > 0.0
    }

    /// Did it make money after fees on the newest third, on enough baskets?
    pub fn passes(&self) -> bool {
        self.newer_trades >= MIN_VERDICT_TRADES
            && self.newer_pct >= MIN_EDGE_PCT
            && self.mean_pct > 0.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub coins: usize,
    pub minutes: usize,
    pub rules: Vec<RuleTest>,
    /// Per source, the best setting chosen on the older two thirds (its
    /// newest-third result is the honest one).
    #[serde(default)]
    pub tuned: Vec<RuleTest>,
    /// The user's rules as they are set (by id).
    #[serde(default)]
    pub own: Vec<RuleTest>,
    /// [`rules_sig`] of the rules it replayed.
    #[serde(default)]
    pub rules_sig: u64,
    /// Unix ms the test ran.
    pub at_ms: i64,
}

impl ReplayReport {
    pub fn get(&self, short: &str) -> Option<&RuleTest> {
        self.rules.iter().find(|r| r.short == short)
    }
    pub fn tuned(&self, short: &str) -> Option<&RuleTest> {
        self.tuned.iter().find(|r| r.short == short)
    }
    pub fn own(&self, rule_id: u64) -> Option<&RuleTest> {
        self.own.iter().find(|r| r.rule_id == rule_id)
    }
}

/// Kinds this replay can test (from minute closes alone).
pub fn replayable(kind: &autobasket::Kind) -> bool {
    use autobasket::Kind::*;
    matches!(
        kind,
        Laggards { .. } | Spreads { .. } | Groups { .. } | SynPairs { .. } | Fade { .. }
    )
}

/// A fingerprint of the replayable rules' settings (FNV): a report made
/// for other settings is redone.
pub fn rules_sig(rules: &[Rule]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for r in rules.iter().filter(|r| replayable(&r.kind)) {
        let text = format!(
            "{}|{}|{}|{}",
            r.id,
            serde_json::to_string(&r.kind).unwrap_or_default(),
            serde_json::to_string(&r.exit).unwrap_or_default(),
            r.max_open
        );
        for b in text.bytes().chain([0]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The exits tried for every source.
pub const EXITS: [crate::basket::ExitRule; 4] = [
    crate::basket::DEFAULT_EXIT,
    crate::basket::ExitRule {
        target_pct: Some(0.5),
        stop_pct: Some(1.0),
        max_minutes: Some(240),
    },
    crate::basket::ExitRule {
        target_pct: Some(0.3),
        stop_pct: Some(0.6),
        max_minutes: Some(60),
    },
    crate::basket::ExitRule {
        target_pct: Some(1.0),
        stop_pct: Some(2.0),
        max_minutes: Some(720),
    },
];

/// The settings tried for a source: its threshold x1, x1.5, x2 with each of
/// [`EXITS`].
pub fn variants(short: &str) -> Vec<Rule> {
    let Some(k0) = autobasket::default_kind(short, false) else {
        return Vec::new();
    };
    let th = k0.threshold().0;
    let mut v = Vec::new();
    for mult in [1.0, 1.5, 2.0] {
        for exit in EXITS {
            let mut r = Rule::new(0, k0.with_threshold(th * mult), 0);
            r.exit = exit;
            v.push(r);
        }
    }
    v
}

/// The report: the sources at their defaults, every tried setting (the
/// best on the older two thirds kept per source), and the user's own
/// replayable rules, in one walk (one picture a step for all of them).
pub fn full_report(series: &[(String, Vec<f64>)], own: &[Rule], at_ms: i64) -> ReplayReport {
    let defaults = default_rules();
    let tried: Vec<(String, Rule)> = SOURCES
        .iter()
        .flat_map(|s| variants(s).into_iter().map(move |r| (s.to_string(), r)))
        .collect();
    let mine: Vec<Rule> = own
        .iter()
        .filter(|r| replayable(&r.kind))
        .cloned()
        .collect();
    let mut all: Vec<Rule> = defaults.clone();
    all.extend(tried.iter().map(|(_, r)| r.clone()));
    all.extend(mine.iter().cloned());
    // Every rule walks alone: ids only tag the baskets.
    for (i, r) in all.iter_mut().enumerate() {
        r.id = i as u64 + 1;
        r.on = true;
    }
    let mut out = run(series, &all, STEP, at_ms);
    let results = std::mem::take(&mut out.rules);
    let (d, rest) = results.split_at(defaults.len());
    let (t, m) = rest.split_at(tried.len());
    out.rules = d.to_vec();
    for s in SOURCES {
        let best = t
            .iter()
            .filter(|x| x.short == s && x.older_trades >= MIN_TUNE_TRADES)
            .max_by(|a, b| a.older_pct.total_cmp(&b.older_pct));
        if let Some(b) = best {
            out.tuned.push(b.clone());
        }
    }
    out.own = m
        .iter()
        .zip(&mine)
        .map(|(x, r)| RuleTest {
            rule_id: r.id,
            ..x.clone()
        })
        .collect();
    out.rules_sig = rules_sig(own);
    out
}

/// May `rule` open baskets now?  `Ok(why)` or `Err(why)`.  Kinds this replay
/// cannot test are not judged here (`Ok`).  With no report, or a report
/// made for other settings, the rule waits.
pub fn verdict(report: Option<&ReplayReport>, rule: &Rule) -> Result<String, String> {
    if !replayable(&rule.kind) {
        return Ok(String::new());
    }
    let Some(r) = report else {
        return Err(pick(
            "waiting for the replay on history",
            "чекаю прогону на історії",
        )
        .into());
    };
    let Some(t) = r.own(rule.id) else {
        return Err(pick(
            "its settings are not replayed yet",
            "ці налаштування ще не прогнано",
        )
        .into());
    };
    if t.passes() {
        Ok(pick(
            format!(
                "newest third {:+.2}% on {} baskets after fees",
                t.newer_pct, t.newer_trades
            ),
            format!(
                "нова третина {:+.2}% на {} кошиках після комісій",
                t.newer_pct, t.newer_trades
            ),
        ))
    } else if t.newer_trades < MIN_VERDICT_TRADES {
        Err(pick(
            format!(
                "too few baskets on the newest third ({}) to know",
                t.newer_trades
            ),
            format!(
                "замало кошиків на новій третині ({}), щоб знати",
                t.newer_trades
            ),
        ))
    } else {
        Err(pick(
            format!("under +{MIN_EDGE_PCT:.2}% a basket after fees on unseen history: {:+.2}% (newest third {:+.2}%)", t.mean_pct, t.newer_pct),
            format!("менше +{MIN_EDGE_PCT:.2}% на кошик після комісій на непобаченій історії: {:+.2}% (нова третина {:+.2}%)", t.mean_pct, t.newer_pct),
        ))
    }
}

struct Open {
    tag: String,
    /// (coin index, long, dollars, entry price)
    legs: Vec<(usize, bool, f64, f64)>,
    opened: usize,
    entry_bps: f64,
    target: Option<f64>,
    stop: Option<f64>,
    until: Option<usize>,
}

/// The rules of [`SOURCES`] at their Auto defaults.
pub fn default_rules() -> Vec<Rule> {
    SOURCES
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            autobasket::default_kind(s, false).map(|k| Rule::new(i as u64 + 1, k, 0))
        })
        .collect()
}

/// Replay `rules` on `series` (per coin, one close a minute, oldest first;
/// every series the same minutes, the newest ones aligned at the end).
pub fn run(series: &[(String, Vec<f64>)], rules: &[Rule], step: usize, at_ms: i64) -> ReplayReport {
    run_costs(series, rules, step, Costs::default(), at_ms)
}

/// [`run`] with other fill costs.
pub fn run_costs(
    series: &[(String, Vec<f64>)],
    rules: &[Rule],
    step: usize,
    costs: Costs,
    at_ms: i64,
) -> ReplayReport {
    let n = series.iter().map(|s| s.1.len()).min().unwrap_or(0);
    let mut report = ReplayReport {
        coins: series.len(),
        minutes: n,
        at_ms,
        ..Default::default()
    };
    let px: Vec<&[f64]> = series.iter().map(|s| &s.1[s.1.len() - n..]).collect();
    let index: HashMap<&str, usize> = series
        .iter()
        .enumerate()
        .map(|(i, s)| (s.0.as_str(), i))
        .collect();
    let longest = *WINDOWS.iter().max().unwrap_or(&60) as usize;
    let start = CORR_MINUTES.max(longest);
    // The older two thirds and the newest third (baskets by the minute they opened).
    let half = start + n.saturating_sub(start) * 2 / 3;
    // Per rule: open baskets, cooling, results (net, opened minute, how).
    let mut open: Vec<Vec<Open>> = rules.iter().map(|_| Vec::new()).collect();
    let mut fired: Vec<HashMap<String, i64>> = rules.iter().map(|_| HashMap::new()).collect();
    let mut done: Vec<Vec<(f64, usize, u8, f64)>> = rules.iter().map(|_| Vec::new()).collect();
    if series.len() >= 2 && n > start + 1 {
        for t in start..n {
            // Follow what is open to this minute.
            for (ri, baskets) in open.iter_mut().enumerate() {
                baskets.retain(|b| {
                    let dollars: f64 = b.legs.iter().map(|l| l.2).sum();
                    if dollars <= 0.0 {
                        return false;
                    }
                    let pct = b
                        .legs
                        .iter()
                        .map(|&(c, long, usd, p0)| {
                            let r = px[c][t] / p0 - 1.0;
                            usd * if long { r } else { -r }
                        })
                        .sum::<f64>()
                        / dollars
                        * 100.0;
                    let how = if b.target.is_some_and(|x| pct >= x) {
                        0
                    } else if b.stop.is_some_and(|x| pct <= -x) {
                        1
                    } else if b.until.is_some_and(|u| t >= u) {
                        2
                    } else {
                        return true;
                    };
                    let maker_out = how == 0 || (how == 2 && costs.clock_as_maker);
                    let out_bps = if maker_out {
                        costs.maker_bps
                    } else {
                        costs.taker_bps
                    };
                    let net = pct - (b.entry_bps + out_bps) / 100.0;
                    done[ri].push((net, b.opened, how, pct));
                    fired[ri].insert(b.tag.clone(), t as i64 * 60_000);
                    false
                });
            }
            if !(t - start).is_multiple_of(step.max(1)) || t + 1 >= n {
                continue;
            }
            let live = picture(series, &px, t);
            let now = t as i64 * 60_000;
            for (ri, rule) in rules.iter().enumerate() {
                let cands = autobasket::candidates(rule, Some(&live), None, None, now);
                let tags: Vec<String> = open[ri].iter().map(|b| b.tag.clone()).collect();
                let busy: HashSet<String> = open[ri]
                    .iter()
                    .flat_map(|b| b.legs.iter().map(|l| series[l.0].0.clone()))
                    .collect();
                let due: Vec<autobasket::Candidate> =
                    autobasket::due(rule, &cands, &tags, &busy, &fired[ri], now)
                        .into_iter()
                        .cloned()
                        .collect();
                for c in due {
                    let tag = rule.tag(&c.key);
                    fired[ri].insert(tag.clone(), now);
                    let legs: Option<Vec<(usize, bool, f64, f64)>> = c
                        .legs
                        .iter()
                        .map(|(s, long, usd)| {
                            let i = *index.get(s.as_str())?;
                            (px[i][t] > 0.0).then_some((i, *long, *usd, px[i][t]))
                        })
                        .collect();
                    let Some(legs) = legs else { continue };
                    let mut exit = c.exit.unwrap_or(rule.exit);
                    if let Some(m) = c.max_minutes {
                        exit.max_minutes = Some(m);
                    }
                    let entry = c.entry.unwrap_or(rule.entry);
                    open[ri].push(Open {
                        tag,
                        legs,
                        opened: t,
                        entry_bps: if entry == Entry::Market {
                            costs.taker_bps
                        } else {
                            costs.maker_bps
                        },
                        target: exit.target_pct,
                        stop: exit.stop_pct,
                        until: exit.max_minutes.map(|m| t + m as usize),
                    });
                }
            }
        }
    }
    // Baskets still open when the history ends are not counted.
    for (ri, rule) in rules.iter().enumerate() {
        report.rules.push(summarize(rule, &done[ri], half));
    }
    report
}

/// The live picture at minute `t`: the last hour of closes, the returns over
/// each window.
fn picture(series: &[(String, Vec<f64>)], px: &[&[f64]], t: usize) -> live_ideas::Live {
    let from = t + 1 - CORR_MINUTES;
    let closes: Vec<(String, Vec<f64>)> = series
        .iter()
        .zip(px)
        .filter(|(_, p)| p[from..=t].iter().all(|x| *x > 0.0))
        .map(|((s, _), p)| (s.clone(), p[from..=t].to_vec()))
        .collect();
    let by_window: Vec<(u32, Vec<Return>)> = WINDOWS
        .iter()
        .map(|&w| {
            let mut rets: Vec<Return> = series
                .iter()
                .zip(px)
                .filter_map(|((s, _), p)| {
                    let (p0, p1) = (p[t - w as usize], p[t]);
                    (p0 > 0.0 && p1 > 0.0).then(|| Return {
                        symbol: s.clone(),
                        pct: (p1 / p0 - 1.0) * 100.0,
                        volume_x: 1.0,
                        price: p1,
                        quote_24h: 0.0,
                    })
                })
                .collect();
            rets.sort_by(|a, b| b.pct.total_cmp(&a.pct).then(a.symbol.cmp(&b.symbol)));
            (w, rets)
        })
        .collect();
    live_ideas::from_parts(&by_window, &closes, CORR_MINUTES, t as i64 * 60_000)
}

fn summarize(rule: &Rule, done: &[(f64, usize, u8, f64)], half: usize) -> RuleTest {
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let nets: Vec<f64> = done.iter().map(|d| d.0).collect();
    let older: Vec<f64> = done.iter().filter(|d| d.1 < half).map(|d| d.0).collect();
    let newer: Vec<f64> = done.iter().filter(|d| d.1 >= half).map(|d| d.0).collect();
    RuleTest {
        short: rule.kind.short().to_string(),
        label: rule.kind.label(),
        rule_id: 0,
        kind: Some(rule.kind),
        exit: Some(rule.exit),
        trades: done.len(),
        wins: nets.iter().filter(|x| **x > 0.0).count(),
        by_target: done.iter().filter(|d| d.2 == 0).count(),
        by_stop: done.iter().filter(|d| d.2 == 1).count(),
        by_clock: done.iter().filter(|d| d.2 == 2).count(),
        mean_pct: mean(&nets),
        mean_gross_pct: mean(&done.iter().map(|d| d.3).collect::<Vec<_>>()),
        mean_fees_pct: mean(&done.iter().map(|d| d.3 - d.0).collect::<Vec<_>>()),
        worst_pct: if nets.is_empty() {
            0.0
        } else {
            nets.iter().copied().fold(f64::INFINITY, f64::min)
        },
        older_pct: mean(&older),
        older_trades: older.len(),
        newer_pct: mean(&newer),
        newer_trades: newer.len(),
    }
}

/// The report the scanning window keeps: [`SOURCES`] at their defaults.
pub fn report(series: &[(String, Vec<f64>)], at_ms: i64) -> ReplayReport {
    run(series, &default_rules(), STEP, at_ms)
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
            (self.0 % 100_000) as f64 / 100_000.0 - 0.5
        }
    }

    /// `k` coins on one common walk, each with its own mean-reverting gap.
    fn family(k: usize, n: usize, gap_amp: f64, seed: u64) -> Vec<(String, Vec<f64>)> {
        let mut r = Rng(seed);
        let mut common = 100.0f64;
        let mut gaps = vec![0.0f64; k];
        let mut out: Vec<(String, Vec<f64>)> =
            (0..k).map(|i| (format!("C{i}USDT"), Vec::new())).collect();
        for _ in 0..n {
            common *= 1.0 + r.next() * 0.004;
            for (i, g) in gaps.iter_mut().enumerate() {
                *g = *g * 0.97 + r.next() * gap_amp;
                out[i].1.push(common * (1.0 + i as f64 * 0.1) * (1.0 + *g));
            }
        }
        out
    }

    #[test]
    fn the_rules_trade_their_own_findings_and_follow_them_to_an_exit() {
        let series = family(8, 1500, 0.001, 11);
        let rules = default_rules();
        let r = run(&series, &rules, 10, 5);
        assert_eq!(r.rules.len(), SOURCES.len());
        assert_eq!((r.coins, r.minutes, r.at_ms), (8, 1500, 5));
        let spreads = r.get("spreads").unwrap();
        assert!(spreads.trades > 0, "{r:?}");
        for t in &r.rules {
            assert_eq!(t.trades, t.by_target + t.by_stop + t.by_clock, "{t:?}");
            assert_eq!(t.trades, t.older_trades + t.newer_trades);
            assert!(t.worst_pct <= t.mean_pct || t.trades == 0);
        }
    }

    #[test]
    fn short_history_or_a_lone_coin_replays_nothing() {
        let rules = default_rules();
        let lone = vec![("AUSDT".to_string(), vec![1.0; 500])];
        assert!(run(&lone, &rules, 5, 0).rules.iter().all(|t| t.trades == 0));
        let short = family(5, 50, 0.004, 3);
        assert!(run(&short, &rules, 5, 0)
            .rules
            .iter()
            .all(|t| t.trades == 0));
    }

    #[test]
    fn the_guard_lets_only_settings_that_paid_on_the_newest_third() {
        let series = family(8, 3000, 0.001, 11);
        let mut mine = Rule::new(42, autobasket::default_kind("spreads", false).unwrap(), 0);
        mine.exit = EXITS[1];
        let fade = Rule::new(
            43,
            autobasket::Kind::Cross {
                min_net_bps: 1.0,
                perps: false,
            },
            0,
        );
        let r = full_report(&series, &[mine.clone(), fade.clone()], 9);
        assert_eq!(r.rules.len(), SOURCES.len(), "the defaults");
        assert_eq!(r.own.len(), 1, "only replayable rules");
        assert_eq!(r.own[0].rule_id, 42);
        assert_eq!(r.rules_sig, rules_sig(&[mine.clone(), fade.clone()]));
        for t in &r.tuned {
            assert!(t.older_trades >= MIN_TUNE_TRADES, "{t:?}");
            assert!(t.kind.is_some() && t.exit.is_some());
        }
        let v = verdict(Some(&r), &mine);
        assert_eq!(v.is_ok(), r.own[0].passes(), "{v:?}");
        assert!(verdict(Some(&r), &fade).is_ok(), "not judged here");
        assert!(verdict(None, &mine).is_err(), "no proof, no trade");
        let mut other = mine.clone();
        other.id = 7;
        assert!(verdict(Some(&r), &other).is_err(), "settings not replayed");
        assert_eq!(variants("groups").len(), 12);
        assert!(variants("cross").len() == 12 && variants("nope").is_empty());
    }

    #[test]
    fn trust_needs_trades_and_both_halves() {
        let mut t = RuleTest {
            trades: 40,
            mean_pct: 0.1,
            older_pct: 0.2,
            newer_pct: -0.05,
            ..Default::default()
        };
        assert!(!t.trusted(), "the newer half lost");
        t.newer_pct = 0.01;
        assert!(t.trusted());
        t.trades = 10;
        assert!(!t.trusted(), "too few");
    }
}
