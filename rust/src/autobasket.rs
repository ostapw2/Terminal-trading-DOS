//! Rules that open baskets by themselves (`crate::basket`): a rule names a
//! kind of finding (a scanner pair far apart, a coin behind the leader it
//! moves with, a fade of the biggest movers, a stretched ratio among this
//! hour's linked pairs, a group against BTC, an options parity or box),
//! the dollars per leg, how many baskets it may hold at once, and a cooling
//! time per finding.  Every ten seconds the trading window reads the live
//! picture, the scan report and the options picture, asks each rule for
//! its candidates, and opens the ones that are not already open or
//! cooling.  The baskets then live their own life: limits in, the exit
//! rule, limits out.
//!
//! Pure: candidates and the "is it due" decision take data and return
//! data; the runtime opens the baskets.  Paper only, like the baskets.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::basket::{Entry, ExitRule, DEFAULT_EXIT, MAX_LEGS};
use crate::live_ideas::Live;
use crate::options::Options;
use crate::scanner::Scan;

/// Rules per ledger at most.
pub const MAX_RULES: usize = 64;
/// Baskets one rule may hold at most.
pub const MAX_OPEN: usize = 500;
pub const DEFAULT_USD_PER_LEG: f64 = 5.0;
pub const DEFAULT_MAX_OPEN: usize = 5;
pub const DEFAULT_COOLDOWN_MIN: u32 = 120;
/// An options basket is held to expiry: its exit is the clock alone, and
/// the paper ledger settles the contracts at their intrinsic value then.
pub const OPTIONS_EXIT: ExitRule = ExitRule {
    target_pct: None,
    stop_pct: None,
    max_minutes: None,
};
/// A carry basket earns slowly: no target, the replay's stop and a week
/// (`crate::carry::STOP_PCT`, `HOLD_H`).
pub const CARRY_EXIT: ExitRule = ExitRule {
    target_pct: None,
    stop_pct: Some(crate::carry::STOP_PCT),
    max_minutes: Some(crate::carry::HOLD_H as u32 * 60),
};

/// What a rule looks for.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// A scanner pair whose gap is at least `min_z` sd: buy the cheap leg,
    /// sell the rich one (beta-weighted when `beta_weighted`).
    Pairs { min_z: f64, beta_weighted: bool },
    /// A coin at least `min_gap_pct` points behind the leader it moves
    /// with (r at least `min_r`): the follower against the leader.
    Laggards { min_gap_pct: f64, min_r: f64 },
    /// The biggest movers of the last minutes, faded: the top `legs`
    /// gainers of at least `min_pct` sold (or losers bought when `up` is
    /// false), one basket per pass.
    Fade { legs: usize, min_pct: f64, up: bool },
    /// A pair that moved as one this hour (r at least `min_r`) whose price
    /// ratio is at least `min_z` sd from its hour's mean: buy the cheap
    /// one, sell the rich one, equal dollars.  Hundreds of pairs an hour.
    Spreads { min_r: f64, min_z: f64 },
    /// A group that trades as one coin, at least `min_z` sd from BTC this
    /// hour: the group (up to `legs` coins, equal dollars) against BTC for
    /// the same total.  The synthetic against the market.
    Groups { min_z: f64, legs: usize },
    /// Two synthetics that moved as one this hour (r at least `min_r`)
    /// whose ratio is at least `min_z` sd from its hour's mean: buy the
    /// cheap synthetic's coins, sell the rich one's, equal dollars a side.
    /// Up to forty legs.
    SynPairs { min_r: f64, min_z: f64 },
    /// Put-call parity with a net edge of at least `min_bps` of spot: spot
    /// with the put against the call (a conversion or a reversal), held
    /// to expiry.
    Parity { min_bps: f64 },
    /// A box spread (four options, no spot) with a net edge of at least
    /// `min_bps` of spot, held to expiry.
    Boxes { min_bps: f64 },
    /// The same coin on two exchanges: buy where it is cheapest, sell where
    /// it is dearest, when the gap less both takers is at least
    /// `min_net_bps` (`crate::crossx`); `perps`: also against perpetuals.
    Cross { min_net_bps: f64, perps: bool },
    /// Funding carry: a perpetual whose funding is at least `min_apr` % a
    /// year: buy the coin on spot, sell the perpetual for the same dollars,
    /// collect the funding (`crate::carry` replays it on history).
    Carry { min_apr: f64 },
    /// Pullback (`crate::pullback`): a coin up fast, bought on the break of
    /// a red candle; stop under the pullback, a target in R; spot or its
    /// perpetual.
    Pullback(crate::pullback::Params),
    /// Statistical arbitrage (`crate::statarb`): a cointegrated pair of
    /// the busiest perpetuals, a grid of limit orders as its spread
    /// widens (one basket per level), all out when it comes back; a loss
    /// cap and a time cap of its own.
    StatArb(crate::statarb::Spec),
}

impl Kind {
    /// A pullback rule's settings.
    pub fn pullback(&self) -> Option<crate::pullback::Params> {
        match *self {
            Kind::Pullback(p) => Some(p),
            _ => None,
        }
    }
    /// A stat-arb rule's settings.
    pub fn statarb(&self) -> Option<crate::statarb::Spec> {
        match *self {
            Kind::StatArb(s) => Some(s),
            _ => None,
        }
    }
}

impl Kind {
    pub fn label(&self) -> String {
        match self {
            Kind::Pairs {
                min_z,
                beta_weighted,
            } => format!(
                "pairs: gap >= {min_z:.1} sd, {}",
                if *beta_weighted {
                    "beta-weighted"
                } else {
                    "equal dollars"
                }
            ),
            Kind::Laggards { min_gap_pct, min_r } => {
                format!("laggards: >= {min_gap_pct:.1} points behind, r >= {min_r:.2}")
            }
            Kind::Fade { legs, min_pct, up } => format!(
                "fade {}: top {legs} of >= {min_pct:.1}%",
                if *up {
                    "gainers (sell)"
                } else {
                    "losers (buy)"
                }
            ),
            Kind::Spreads { min_r, min_z } => {
                format!("spreads: this hour's pairs r >= {min_r:.2}, ratio >= {min_z:.1} sd away")
            }
            Kind::Groups { min_z, legs } => {
                format!("groups: a group of up to {legs} >= {min_z:.1} sd from BTC, against BTC")
            }
            Kind::SynPairs { min_r, min_z } => format!(
                "synthetic pairs: two synthetics r >= {min_r:.2}, ratio >= {min_z:.1} sd away"
            ),
            Kind::Parity { min_bps } => {
                format!("options parity: net edge >= {min_bps:.0} bps, held to expiry")
            }
            Kind::Boxes { min_bps } => {
                format!("options boxes: net edge >= {min_bps:.0} bps, held to expiry")
            }
            Kind::Cross { min_net_bps, perps } => format!(
                "cross-exchange: net gap >= {min_net_bps:.0} bps{}",
                if *perps {
                    ", with perpetuals"
                } else {
                    ", spot"
                }
            ),
            Kind::Carry { min_apr } => {
                format!(
                    "funding carry: funding >= {min_apr:.0}% a year, spot long / perpetual short"
                )
            }
            Kind::Pullback { .. } => {
                format!("pullback: {}", self.pullback().unwrap_or_default().words())
            }
            Kind::StatArb(s) => format!("stat-arb: {}", s.words()),
        }
    }
    pub fn short(&self) -> &'static str {
        match self {
            Kind::Pairs { .. } => "pairs",
            Kind::Laggards { .. } => "laggards",
            Kind::Fade { up: true, .. } => "fade gainers",
            Kind::Fade { up: false, .. } => "fade losers",
            Kind::Spreads { .. } => "spreads",
            Kind::Groups { .. } => "groups",
            Kind::SynPairs { .. } => "synthetic pairs",
            Kind::Parity { .. } => "parity",
            Kind::Boxes { .. } => "boxes",
            Kind::Cross { .. } => "cross",
            Kind::Carry { .. } => "carry",
            Kind::Pullback { .. } => "pullback",
            Kind::StatArb { .. } => "statarb",
        }
    }
    /// The kind's threshold (sd, points, %, bps) and its unit.
    pub fn threshold(&self) -> (f64, &'static str) {
        match self {
            Kind::Pairs { min_z, .. } => (*min_z, "sd"),
            Kind::Laggards { min_gap_pct, .. } => (*min_gap_pct, "points"),
            Kind::Fade { min_pct, .. } => (*min_pct, "%"),
            Kind::Spreads { min_z, .. } => (*min_z, "sd"),
            Kind::Groups { min_z, .. } | Kind::SynPairs { min_z, .. } => (*min_z, "sd"),
            Kind::Parity { min_bps } | Kind::Boxes { min_bps } => (*min_bps, "bps"),
            Kind::Cross { min_net_bps, .. } => (*min_net_bps, "bps"),
            Kind::Carry { min_apr } => (*min_apr, "% a year"),
            Kind::Pullback(p) => (p.min_rise_pct, "% rise"),
            Kind::StatArb(s) => (s.z_in, "sd"),
        }
    }
    /// The same kind with its threshold set to `v`.
    pub fn with_threshold(self, v: f64) -> Kind {
        match self {
            Kind::Pairs { beta_weighted, .. } => Kind::Pairs {
                min_z: v,
                beta_weighted,
            },
            Kind::Laggards { min_r, .. } => Kind::Laggards {
                min_gap_pct: v,
                min_r,
            },
            Kind::Fade { legs, up, .. } => Kind::Fade {
                legs,
                min_pct: v,
                up,
            },
            Kind::Spreads { min_r, .. } => Kind::Spreads { min_r, min_z: v },
            Kind::Groups { legs, .. } => Kind::Groups { min_z: v, legs },
            Kind::SynPairs { min_r, .. } => Kind::SynPairs { min_r, min_z: v },
            Kind::Parity { .. } => Kind::Parity { min_bps: v },
            Kind::Boxes { .. } => Kind::Boxes { min_bps: v },
            Kind::Cross { perps, .. } => Kind::Cross {
                min_net_bps: v,
                perps,
            },
            Kind::Carry { .. } => Kind::Carry { min_apr: v },
            Kind::Pullback(p) => Kind::Pullback(crate::pullback::Params {
                min_rise_pct: v,
                ..p
            }),
            Kind::StatArb(s) => Kind::StatArb(crate::statarb::Spec { z_in: v, ..s }),
        }
    }
    /// Options kinds hold to expiry and settle there.
    pub fn is_options(&self) -> bool {
        matches!(self, Kind::Parity { .. } | Kind::Boxes { .. })
    }
}

/// A source's rule as the Auto tab starts it (by the kind's short name);
/// the replay on history (`crate::replay`) tests the same thresholds.
pub fn default_kind(short: &str, pair_beta: bool) -> Option<Kind> {
    Some(match short {
        "pairs" => Kind::Pairs {
            min_z: 2.0,
            beta_weighted: pair_beta,
        },
        "laggards" => Kind::Laggards {
            min_gap_pct: 3.0,
            min_r: 0.6,
        },
        "spreads" => Kind::Spreads {
            min_r: crate::live_ideas::SPREAD_MIN_R,
            min_z: 2.0,
        },
        "groups" => Kind::Groups {
            min_z: 2.0,
            legs: MAX_LEGS - 1,
        },
        "synthetic pairs" => Kind::SynPairs {
            min_r: crate::live_ideas::SYN_PAIR_MIN_R,
            min_z: 2.0,
        },
        "parity" => Kind::Parity { min_bps: 5.0 },
        "boxes" => Kind::Boxes { min_bps: 5.0 },
        "cross" => Kind::Cross {
            min_net_bps: 20.0,
            perps: false,
        },
        "carry" => Kind::Carry {
            min_apr: crate::carry::MIN_APR,
        },
        "pullback" => Kind::Pullback(crate::pullback::Params::default()),
        "statarb" => Kind::StatArb(crate::statarb::Spec::default()),
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u64,
    pub kind: Kind,
    pub usd_per_leg: f64,
    /// Baskets this rule may hold open at once.
    pub max_open: usize,
    /// After a basket of a finding closes, the same finding waits this long.
    pub cooldown_min: u32,
    pub entry: Entry,
    pub exit: ExitRule,
    pub on: bool,
    pub created_ms: i64,
    /// Open baskets only while the replay on history says these settings
    /// make money after fees (`crate::replay::verdict`, `crate::carry::verdict`).
    #[serde(default = "yes")]
    pub guard: bool,
}

fn yes() -> bool {
    true
}

impl Rule {
    pub fn new(id: u64, kind: Kind, now_ms: i64) -> Rule {
        Rule {
            id,
            kind,
            usd_per_leg: DEFAULT_USD_PER_LEG,
            max_open: match kind {
                // One basket per grid level of every pair.
                Kind::StatArb(s) => (s.max_pairs * s.levels).clamp(1, MAX_OPEN),
                _ => DEFAULT_MAX_OPEN,
            },
            cooldown_min: DEFAULT_COOLDOWN_MIN,
            entry: Entry::default(),
            exit: match kind {
                Kind::Parity { .. } | Kind::Boxes { .. } => OPTIONS_EXIT,
                Kind::Carry { .. } => CARRY_EXIT,
                Kind::StatArb(s) => ExitRule {
                    target_pct: None,
                    stop_pct: Some(s.loss_cap_pct),
                    max_minutes: Some(2880),
                },
                _ => DEFAULT_EXIT,
            },
            on: true,
            created_ms: now_ms,
            guard: true,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.usd_per_leg.is_finite() && self.usd_per_leg > 0.0) {
            return Err("dollars per leg must be positive".into());
        }
        if self.max_open == 0 || self.max_open > MAX_OPEN {
            return Err(format!("max open baskets: 1-{MAX_OPEN}"));
        }
        match self.kind {
            Kind::Pairs { min_z, .. } if !(min_z.is_finite() && min_z > 0.0) => {
                return Err("min z must be positive".into())
            }
            Kind::Laggards { min_gap_pct, min_r }
                if !(min_gap_pct.is_finite() && min_gap_pct > 0.0)
                    || !(0.0..=1.0).contains(&min_r) =>
            {
                return Err("min gap must be positive, min r within 0-1".into())
            }
            Kind::Fade { legs, min_pct, .. }
                if legs == 0 || legs > MAX_LEGS || !(min_pct.is_finite() && min_pct > 0.0) =>
            {
                return Err(format!("legs 1-{MAX_LEGS}, min % positive"))
            }
            Kind::Spreads { min_r, min_z } | Kind::SynPairs { min_r, min_z }
                if !(0.0..=1.0).contains(&min_r) || !(min_z.is_finite() && min_z > 0.0) =>
            {
                return Err("min r within 0-1, min z positive".into())
            }
            Kind::Groups { min_z, legs }
                if !(2..MAX_LEGS).contains(&legs) || !(min_z.is_finite() && min_z > 0.0) =>
            {
                return Err(format!("legs 2-{}, min z positive", MAX_LEGS - 1))
            }
            Kind::Parity { min_bps } | Kind::Boxes { min_bps }
                if !(min_bps.is_finite() && min_bps >= 0.0) =>
            {
                return Err("min bps must be zero or more".into())
            }
            Kind::Carry { min_apr } if !(min_apr.is_finite() && min_apr > 0.0) => {
                return Err("min % a year must be positive".into())
            }
            Kind::Pullback(p)
                if !(p.window_min == 15 || p.window_min == 60)
                    || !(p.min_rise_pct.is_finite() && p.min_rise_pct > 0.0)
                    || ![1, 3, 5, 15].contains(&p.tf_min)
                    || !(p.target_r.is_finite() && p.target_r > 0.0) =>
            {
                return Err("pullback: window 15 or 60 min, rise % positive, candles 1/3/5/15 min, target R positive".into())
            }
            Kind::Cross { min_net_bps, .. } if !(min_net_bps.is_finite() && min_net_bps >= 0.0) => {
                return Err("min net bps must be zero or more".into())
            }
            Kind::StatArb(s) => s.validate().map_err(|e| format!("stat-arb: {e}"))?,
            _ => {}
        }
        Ok(())
    }

    /// One line for the status line and the list.
    pub fn words(&self) -> String {
        format!(
            "#{} {} {}; ${:.2} a leg, {} open at most, {} min cooling; {}",
            self.id,
            if self.on { "ON " } else { "off" },
            self.kind.label(),
            self.usd_per_leg,
            self.max_open,
            self.cooldown_min,
            self.exit.words()
        )
    }

    /// The tag a basket of this rule carries: "auto:<rule>:<finding>".
    pub fn tag(&self, key: &str) -> String {
        format!("auto:{}:{key}", self.id)
    }
}

/// A basket a rule would open now.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The finding, stable across passes ("AUSDT/BUSDT", "fade:up").
    pub key: String,
    pub name: String,
    pub origin: String,
    pub legs: Vec<(String, bool, f64)>,
    /// Legs held in coins / contracts whatever the price (options against
    /// their spot): (symbol, qty).
    pub fixed: Vec<(String, f64)>,
    /// Minutes the basket may live (options: to expiry); `None` keeps the
    /// rule's exit.
    pub max_minutes: Option<u32>,
    /// An exit and an entry of the finding's own (cross-exchange: tight and
    /// at market), instead of the rule's.
    pub exit: Option<ExitRule>,
    pub entry: Option<Entry>,
    /// Prices to put in the ledger before it opens: (instrument, price).
    pub prices: Vec<(String, f64)>,
}

impl Candidate {
    fn plain(
        key: String,
        name: String,
        origin: String,
        legs: Vec<(String, bool, f64)>,
    ) -> Candidate {
        Candidate {
            key,
            name,
            origin,
            legs,
            fixed: Vec::new(),
            max_minutes: None,
            exit: None,
            entry: None,
            prices: Vec::new(),
        }
    }
}

/// What a cross-exchange rule would open: every row of the picture whose net
/// gap reaches the rule's minimum, the widest first.  Both legs at market
/// (the gap is gone in seconds), left again at a share of the gap, on a
/// stop of 0.3 % of the dollars, or after an hour.
pub fn cross_candidates(rule: &Rule, opps: &[crate::crossx::Opp]) -> Vec<Candidate> {
    let Kind::Cross { min_net_bps, perps } = rule.kind else {
        return Vec::new();
    };
    let usd = rule.usd_per_leg;
    opps.iter()
        .filter(|o| o.net_bps >= min_net_bps)
        .filter(|o| perps || o.kind == crate::crossx::Kind::Cross)
        .map(|o| {
            let (a, b) = (
                crate::exchanges::instrument(&o.symbol, o.buy),
                crate::exchanges::instrument(&o.symbol, o.sell),
            );
            // A basket's percent is of both legs' dollars: the gap is half of it.
            let target = (o.net_bps / 2.0 / 100.0 * 0.6).max(0.03);
            let mut c = Candidate::plain(
                format!("{}:{}>{}", o.symbol, o.buy.code(), o.sell.code()),
                format!("buy {a} / sell {b}"),
                format!(
                    "auto cross: {} buy {} at {}, sell {} at {}: gap {:.0} bps, net {:.0}",
                    o.symbol,
                    o.buy.code(),
                    o.ask,
                    o.sell.code(),
                    o.bid,
                    o.gap_bps,
                    o.net_bps
                ),
                vec![(a.clone(), true, usd), (b.clone(), false, usd)],
            );
            c.prices = vec![(a, o.ask), (b, o.bid)];
            c.entry = Some(Entry::Market);
            c.exit = Some(ExitRule {
                target_pct: Some(target),
                stop_pct: Some(0.3),
                max_minutes: Some(60),
            });
            c
        })
        .collect()
}

/// What a carry rule would open: every perpetual whose latest funding rate
/// (`rates`: symbol -> fraction per settlement, Binance's `premiumIndex`),
/// taken as every 8 h, is at least the rule's yearly %, the highest first.
/// Coins on a 4 h or 1 h interval pay more than this reads: the rule errs
/// on the side of fewer baskets.  Both legs at market (the replay's fees).
pub fn carry_candidates(rule: &Rule, rates: &HashMap<String, f64>) -> Vec<Candidate> {
    let Kind::Carry { min_apr } = rule.kind else {
        return Vec::new();
    };
    let per_year = 365.0 * 3.0;
    let mut v: Vec<(&String, f64)> = rates
        .iter()
        .map(|(s, r)| (s, crate::carry::apr(*r, per_year)))
        .filter(|(s, a)| a.is_finite() && *a >= min_apr && crate::scanner::tradable(s))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter()
        .map(|(sym, a)| {
            let perp = crate::futures::perp(sym);
            let mut c = Candidate::plain(
                sym.clone(),
                format!("carry {}", sym.trim_end_matches("USDT")),
                format!("auto carry: {sym} funding {a:.0}% a year: spot long, perpetual short"),
                vec![
                    (sym.clone(), true, rule.usd_per_leg),
                    (perp, false, rule.usd_per_leg),
                ],
            );
            c.entry = Some(Entry::Market);
            c
        })
        .collect()
}

/// What a pullback rule would buy now: every armed setup (`setups`: symbol
/// -> the setup and the price it was read at) whose break has come, the
/// biggest rise first.  One leg, at market, with the setup's own stop and
/// target.
pub fn pullback_candidates(
    rule: &Rule,
    setups: &HashMap<String, (crate::pullback::Setup, f64)>,
) -> Vec<Candidate> {
    let Some(p) = rule.kind.pullback() else {
        return Vec::new();
    };
    let mut v: Vec<(&String, &(crate::pullback::Setup, f64))> =
        setups.iter().filter(|(_, (s, _))| s.triggered).collect();
    v.sort_by(|a, b| {
        b.1 .0
            .rise_pct
            .total_cmp(&a.1 .0.rise_pct)
            .then_with(|| a.0.cmp(b.0))
    });
    v.into_iter()
        .map(|(coin, (s, px))| {
            let entry = px.max(s.trigger);
            // The instrument bought: the coin, or its perpetual.
            let sym = if p.perp {
                crate::futures::perp(coin)
            } else {
                coin.clone()
            };
            let sym = &sym;
            let mut c = Candidate::plain(
                format!("{coin}:{}", s.red_ms),
                format!("pullback {}", coin.trim_end_matches("USDT")),
                format!(
                    "auto pullback: {sym} up {:.1}%, broke the red candle at {:.6}; stop {:.6}, target {:.6}",
                    s.rise_pct, s.trigger, s.stop, s.target
                ),
                vec![(sym.clone(), true, rule.usd_per_leg)],
            );
            c.entry = Some(Entry::Market);
            c.exit = Some(s.exit(entry, &p));
            c.prices = vec![(sym.clone(), entry)];
            c
        })
        .collect()
}

/// The key of a stat-arb basket: pair, side of the spread, grid level.
pub fn statarb_key(x: &str, y: &str, side: f64, level: usize) -> String {
    format!(
        "{x}/{y}:{}:L{level}",
        if side > 0.0 { "long" } else { "short" }
    )
}

/// The pair and side a stat-arb key names.
pub fn statarb_key_parts(key: &str) -> Option<(&str, &str, f64)> {
    let mut it = key.split(':');
    let (x, y) = it.next()?.split_once('/')?;
    let side = match it.next()? {
        "long" => 1.0,
        "short" => -1.0,
        _ => return None,
    };
    Some((x, y, side))
}

/// What a stat-arb rule would open from the live picture (`pairs`, the
/// widest spread first): for every pair whose |z| reaches a grid level
/// not open yet, one basket of that level: the cheap leg bought, the rich
/// one sold, dollars by the hedge ratio, a whole grid at most 1 % of the
/// thinner leg's 5-minute dollars.  A pair already open on the other
/// side, more than `max_pairs` pairs, or a coin in more than
/// `max_per_coin` pairs: nothing.  Limits rest `ENTRY_WAIT_SECS`; the
/// exit is the loss cap and `max_hold_hl` half-lives (the spread coming
/// back is watched by the trading window).
pub fn statarb_candidates(
    rule: &Rule,
    pairs: &[crate::statarb::PairNow],
    open_tags: &[String],
) -> Vec<Candidate> {
    let Some(spec) = rule.kind.statarb() else {
        return Vec::new();
    };
    let prefix = rule.tag("");
    // Pairs this rule holds now, by side.
    let open: Vec<(String, String, f64)> = open_tags
        .iter()
        .filter_map(|t| t.strip_prefix(&prefix))
        .filter_map(|k| statarb_key_parts(k).map(|(x, y, s)| (x.to_string(), y.to_string(), s)))
        .collect();
    let mut open_pairs: Vec<(String, String)> = open
        .iter()
        .map(|(x, y, _)| (x.clone(), y.clone()))
        .collect();
    open_pairs.sort();
    open_pairs.dedup();
    let busy = |c: &str| open_pairs.iter().filter(|(x, y)| x == c || y == c).count();
    let grid = spec.grid();
    let short = |s: &str| s.trim_end_matches("USDT").to_string();
    let mut out = Vec::new();
    let mut new_pairs = 0usize;
    for pr in pairs {
        if !(pr.z.is_finite() && pr.z.abs() >= spec.z_in) {
            continue;
        }
        let side = if pr.z > 0.0 { -1.0 } else { 1.0 };
        let held = open_pairs.iter().any(|(x, y)| *x == pr.x && *y == pr.y);
        if open
            .iter()
            .any(|(x, y, s)| *x == pr.x && *y == pr.y && *s != side)
        {
            continue; // the other side is still leaving
        }
        if !held {
            if open_pairs.len() + new_pairs >= spec.max_pairs
                || busy(&pr.x) >= spec.max_per_coin
                || busy(&pr.y) >= spec.max_per_coin
            {
                continue;
            }
            new_pairs += 1;
        }
        // Dollars of one level: both legs, by the hedge ratio, under the
        // liquidity cap of the whole grid.
        let unit = (rule.usd_per_leg * 2.0).min(pr.thin_5m * 0.01 / grid.len() as f64);
        if unit <= 0.0 {
            continue;
        }
        let (ux, uy) = (unit / (1.0 + pr.beta), unit * pr.beta / (1.0 + pr.beta));
        let (xp, yp) = (crate::futures::perp(&pr.x), crate::futures::perp(&pr.y));
        for (level, z_level) in grid.iter().enumerate() {
            if pr.z.abs() < *z_level {
                break;
            }
            let legs = if side > 0.0 {
                vec![(xp.clone(), true, ux), (yp.clone(), false, uy)]
            } else {
                vec![(xp.clone(), false, ux), (yp.clone(), true, uy)]
            };
            let mut c = Candidate::plain(
                statarb_key(&pr.x, &pr.y, side, level),
                format!(
                    "{} {} / {} {} L{}",
                    if side > 0.0 { "buy" } else { "sell" },
                    short(&pr.x),
                    if side > 0.0 { "sell" } else { "buy" },
                    short(&pr.y),
                    level + 1
                ),
                format!(
                    "auto stat-arb: {}/{} spread {:+.1} sd, level {} of {}, half-life {:.0} min, beta {:.2}",
                    pr.x, pr.y, pr.z, level + 1, grid.len(), pr.hl_min, pr.beta
                ),
                legs,
            );
            c.entry = Some(Entry::Limit {
                offset_bps: crate::basket::DEFAULT_OFFSET_BPS,
                wait_secs: crate::statarb::ENTRY_WAIT_SECS,
            });
            c.exit = Some(ExitRule {
                target_pct: None,
                stop_pct: Some(spec.loss_cap_pct),
                max_minutes: Some(pr.hold_minutes(spec.max_hold_hl)),
            });
            out.push(c);
        }
    }
    out
}

/// Minutes from now to an expiry, one at least.
fn minutes_to(expiry_ms: i64, now_ms: i64) -> u32 {
    ((expiry_ms - now_ms) / 60_000).clamp(1, u32::MAX as i64) as u32
}

/// What a rule would open from the live picture, the scan report and the
/// options picture, in the order to open them (the strongest first).
pub fn candidates(
    rule: &Rule,
    live: Option<&Live>,
    scan: Option<&Scan>,
    options: Option<&Options>,
    now_ms: i64,
) -> Vec<Candidate> {
    let usd = rule.usd_per_leg;
    let short = |s: &str| s.trim_end_matches("USDT").to_string();
    match rule.kind {
        Kind::Pairs {
            min_z,
            beta_weighted,
        } => {
            let Some(scan) = scan else {
                return Vec::new();
            };
            let mut v: Vec<&crate::scanner::PairIdea> =
                scan.pairs.iter().filter(|p| p.z.abs() >= min_z).collect();
            v.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
            v.into_iter()
                .map(|p| {
                    Candidate::plain(
                        format!("{}/{}", p.long, p.short),
                        format!("buy {} / sell {}", short(&p.long), short(&p.short)),
                        format!(
                            "auto pairs: gap {:+.1} sd, half-life {:.0} ({})",
                            p.z, p.half_life, scan.tf
                        ),
                        p.legs(usd, beta_weighted).to_vec(),
                    )
                })
                .collect()
        }
        Kind::Laggards { min_gap_pct, min_r } => {
            let Some(live) = live else {
                return Vec::new();
            };
            let mut v: Vec<&crate::live_ideas::Laggard> = live
                .laggards
                .iter()
                .filter(|l| l.gap_pct().abs() >= min_gap_pct && l.r >= min_r)
                .collect();
            v.sort_by(|a, b| b.gap_pct().abs().total_cmp(&a.gap_pct().abs()));
            v.into_iter()
                .map(|l| {
                    let up = l.lead_pct > 0.0;
                    Candidate::plain(
                        format!("{}/{}", l.follower, l.leader),
                        format!(
                            "{} {} / {} {}",
                            if up { "buy" } else { "sell" },
                            short(&l.follower),
                            if up { "sell" } else { "buy" },
                            short(&l.leader)
                        ),
                        format!(
                            "auto laggards: {} behind {} by {:.1} points, r {:.2} over {} min",
                            short(&l.follower),
                            short(&l.leader),
                            l.gap_pct().abs(),
                            l.r,
                            l.minutes
                        ),
                        vec![(l.follower.clone(), up, usd), (l.leader.clone(), !up, usd)],
                    )
                })
                .collect()
        }
        Kind::Fade { legs, min_pct, up } => {
            let Some(live) = live else {
                return Vec::new();
            };
            let movers = if up { &live.gainers } else { &live.losers };
            let picked: Vec<(String, bool, f64)> = movers
                .iter()
                .filter(|m| m.pct.abs() >= min_pct)
                .take(legs)
                .map(|m| (m.symbol.clone(), !up, usd))
                .collect();
            if picked.is_empty() {
                return Vec::new();
            }
            let mut names: Vec<String> = picked.iter().map(|(s, _, _)| s.clone()).collect();
            names.sort();
            vec![Candidate::plain(
                format!(
                    "fade:{}:{}",
                    if up { "up" } else { "down" },
                    names.join(",")
                ),
                format!(
                    "{} {} x{}",
                    if up { "sell" } else { "buy" },
                    if up { "gainers" } else { "losers" },
                    picked.len()
                ),
                format!(
                    "auto fade: the biggest {} of the last {} min",
                    if up { "rises" } else { "falls" },
                    movers.first().map_or(0, |m| m.window_min)
                ),
                picked,
            )]
        }
        Kind::Spreads { min_r, min_z } => {
            let Some(live) = live else {
                return Vec::new();
            };
            // Already the widest first.
            live.spreads
                .iter()
                .filter(|s| s.r >= min_r && s.z.abs() >= min_z)
                .map(|s| {
                    let (long, sh) = s.long_short();
                    Candidate::plain(
                        format!("{long}/{sh}"),
                        format!("buy {} / sell {}", short(long), short(sh)),
                        format!(
                            "auto spreads: ratio {:+.1} sd from its {} min mean, r {:.2}, sd {:.2}%",
                            s.z, s.minutes, s.r, s.sd_pct
                        ),
                        vec![(long.to_string(), true, usd), (sh.to_string(), false, usd)],
                    )
                })
                .collect()
        }
        Kind::SynPairs { min_r, min_z } => {
            let Some(live) = live else {
                return Vec::new();
            };
            live.syn_pairs
                .iter()
                .filter(|p| p.r >= min_r && p.z.abs() >= min_z)
                .filter_map(|p| {
                    let ((long, lm), (sh, sm)) = p.long_short();
                    if lm.is_empty() || sm.is_empty() || lm.len() + sm.len() > MAX_LEGS {
                        return None;
                    }
                    // Equal dollars a side: the rule's dollars a leg on the
                    // long side, the short side scaled to the same total.
                    let short_usd = usd * lm.len() as f64 / sm.len() as f64;
                    let mut legs: Vec<(String, bool, f64)> =
                        lm.iter().map(|m| (m.clone(), true, usd)).collect();
                    legs.extend(sm.iter().map(|m| (m.clone(), false, short_usd)));
                    Some(Candidate::plain(
                        format!("syn:{long}/{sh}"),
                        format!("buy {long} / sell {sh}"),
                        format!(
                            "auto synthetic pairs: {long} ({}) against {sh} ({}), ratio {:+.1} sd from its {} min mean, r {:.2}",
                            lm.len(),
                            sm.len(),
                            p.z,
                            p.minutes,
                            p.r
                        ),
                        legs,
                    ))
                })
                .collect()
        }
        Kind::Groups { min_z, legs } => {
            let Some(live) = live else {
                return Vec::new();
            };
            let mut v: Vec<(&crate::live_ideas::Group, f64)> = live
                .groups
                .iter()
                .filter_map(|g| g.z_hedge.map(|z| (g, z)))
                .filter(|(g, z)| {
                    z.abs() >= min_z && !g.members.contains(&crate::live_ideas::HEDGE.to_string())
                })
                .collect();
            v.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
            v.into_iter()
                .map(|(g, z)| {
                    // The group ran ahead of BTC: sell it, buy BTC; behind: the other way.
                    let long_group = z < 0.0;
                    let members: Vec<&String> = g.members.iter().take(legs).collect();
                    let mut out: Vec<(String, bool, f64)> = members
                        .iter()
                        .map(|m| ((*m).clone(), long_group, usd))
                        .collect();
                    out.push((
                        crate::live_ideas::HEDGE.to_string(),
                        !long_group,
                        usd * members.len() as f64,
                    ));
                    let mut names: Vec<&str> = members.iter().map(|m| m.as_str()).collect();
                    names.sort_unstable();
                    Candidate::plain(
                        format!("group:{}", names.join(",")),
                        format!(
                            "{} group of {} / {} BTC",
                            if long_group { "buy" } else { "sell" },
                            members.len(),
                            if long_group { "sell" } else { "buy" }
                        ),
                        format!(
                            "auto groups: the group is {z:+.1} sd from BTC over {} min (r {:.2} inside)",
                            g.minutes, g.r_mean
                        ),
                        out,
                    )
                })
                .collect()
        }
        Kind::Parity { min_bps } => {
            let Some(o) = options else {
                return Vec::new();
            };
            o.parity
                .iter()
                .filter(|p| p.net_bps() >= min_bps && p.spot > 0.0)
                .map(|p| {
                    let coins = usd / p.spot;
                    let legs = p.legs(coins);
                    let fixed = legs.iter().map(|(s, _, q)| (s.clone(), *q)).collect();
                    Candidate {
                        key: p.key(),
                        name: p.words(),
                        origin: format!(
                            "auto parity: {:+.0} bps net ({:.1}% a year) over {:.0} days, spot {}",
                            p.net_bps(),
                            p.annual_pct(),
                            p.days,
                            p.spot
                        ),
                        legs: legs
                            .iter()
                            .map(|(s, l, q)| (s.clone(), *l, q * usd / coins))
                            .collect(),
                        fixed,
                        max_minutes: Some(minutes_to(p.expiry_ms, now_ms)),
                        exit: None,
                        entry: None,
                        prices: Vec::new(),
                    }
                })
                .collect()
        }
        // Their own pictures: `cross_candidates`, `carry_candidates`, ...
        Kind::Cross { .. } | Kind::Carry { .. } | Kind::Pullback { .. } | Kind::StatArb { .. } => {
            Vec::new()
        }
        Kind::Boxes { min_bps } => {
            let Some(o) = options else {
                return Vec::new();
            };
            o.boxes
                .iter()
                .filter(|b| b.net_bps() >= min_bps && b.spot > 0.0)
                .map(|b| {
                    let coins = usd / b.spot;
                    let legs = b.legs(coins);
                    let fixed = legs.iter().map(|(s, _, q)| (s.clone(), *q)).collect();
                    Candidate {
                        key: b.key(),
                        name: b.words(),
                        origin: format!(
                            "auto boxes: {:+.0} bps net ({:.1}% a year) over {:.0} days, pays {} a coin",
                            b.net_bps(),
                            b.annual_pct(),
                            b.days,
                            b.payout()
                        ),
                        legs: legs.iter().map(|(s, l, _)| (s.clone(), *l, usd)).collect(),
                        fixed,
                        max_minutes: Some(minutes_to(b.expiry_ms, now_ms)),
                        exit: None,
                        entry: None,
                        prices: Vec::new(),
                    }
                })
                .collect()
        }
    }
}

/// Which candidates to open now: not open already (`open_tags` holds the
/// tags of the baskets that are not closed), not sharing a coin with one
/// of this rule's open baskets (`busy`: their symbols) nor with a basket
/// opened earlier in this pass, not cooling (`fired` holds when a tag last
/// closed or opened), and no more than the rule's room.
pub fn due<'a>(
    rule: &Rule,
    cands: &'a [Candidate],
    open_tags: &[String],
    busy: &HashSet<String>,
    fired: &HashMap<String, i64>,
    now_ms: i64,
) -> Vec<&'a Candidate> {
    if !rule.on {
        return Vec::new();
    }
    let prefix = format!("auto:{}:", rule.id);
    let open_now = open_tags.iter().filter(|t| t.starts_with(&prefix)).count();
    let mut room = rule.max_open.saturating_sub(open_now);
    let mut taken: HashSet<String> = busy.clone();
    let mut out = Vec::new();
    for c in cands {
        if room == 0 {
            break;
        }
        let tag = rule.tag(&c.key);
        if open_tags.contains(&tag) {
            continue;
        }
        if c.legs.iter().any(|(s, _, _)| taken.contains(s)) {
            continue;
        }
        if fired
            .get(&tag)
            .is_some_and(|t| now_ms - t < rule.cooldown_min as i64 * 60_000)
        {
            continue;
        }
        taken.extend(c.legs.iter().map(|(s, _, _)| s.clone()));
        out.push(c);
        room -= 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_ideas::{Laggard, Move};
    use crate::scanner::{GapHistory, PairIdea};

    #[test]
    fn statarb_opens_one_basket_per_level_under_its_caps() {
        use crate::statarb::PairNow;
        let pair = |x: &str, y: &str, z: f64| PairNow {
            x: x.into(),
            y: y.into(),
            beta: 1.0,
            z,
            hl_min: 120.0,
            thin_5m: 100_000.0,
            bars: 2016,
        };
        let spec = crate::statarb::Spec {
            max_pairs: 2,
            max_per_coin: 1,
            ..Default::default()
        };
        let mut rule = Rule::new(7, Kind::StatArb(spec), 0);
        rule.usd_per_leg = 5.0;
        assert_eq!(rule.max_open, 8);
        let pairs = vec![
            pair("AUSDT", "BUSDT", -2.6),
            pair("CUSDT", "DUSDT", 3.1),
            pair("AUSDT", "EUSDT", -4.0),
            pair("FUSDT", "GUSDT", 1.5),
        ];
        let c = statarb_candidates(&rule, &pairs, &[]);
        // A/B: levels 2 and 2.5 reached (2 baskets); C/D: 3 levels; A/E: A busy; F/G: under z in.
        let keys: Vec<&str> = c.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "AUSDT/BUSDT:long:L0",
                "AUSDT/BUSDT:long:L1",
                "CUSDT/DUSDT:short:L0",
                "CUSDT/DUSDT:short:L1",
                "CUSDT/DUSDT:short:L2",
            ]
        );
        let ab = &c[0];
        assert_eq!(ab.legs[0], ("AUSDT_PERP".to_string(), true, 5.0));
        assert_eq!(ab.legs[1], ("BUSDT_PERP".to_string(), false, 5.0));
        assert_eq!(ab.exit.unwrap().max_minutes, Some(240));
        assert_eq!(ab.exit.unwrap().stop_pct, Some(4.0));
        assert!(matches!(
            ab.entry,
            Some(Entry::Limit { wait_secs: 600, .. })
        ));
        assert_eq!(statarb_key_parts(&ab.key), Some(("AUSDT", "BUSDT", 1.0)));
        // With A/B open long, a flip to short waits; the open pair counts for the caps.
        let open = vec![rule.tag("AUSDT/BUSDT:long:L0")];
        let flipped = vec![pair("AUSDT", "BUSDT", 2.2), pair("CUSDT", "DUSDT", 3.1)];
        let c = statarb_candidates(&rule, &flipped, &open);
        assert!(
            c.iter().all(|x| x.key.starts_with("CUSDT")),
            "{:?}",
            c.iter().map(|x| &x.key).collect::<Vec<_>>()
        );
        // The liquidity cap: a thin pair gets a smaller unit.
        let thin = vec![PairNow {
            thin_5m: 800.0,
            ..pair("HUSDT", "IUSDT", -2.0)
        }];
        let c = statarb_candidates(&rule, &thin, &[]);
        assert!((c[0].legs[0].2 - 1.0).abs() < 1e-9, "{:?}", c[0].legs);
        assert!(Rule::new(
            8,
            Kind::StatArb(crate::statarb::Spec { levels: 0, ..spec }),
            0
        )
        .validate()
        .is_err());
    }

    #[test]
    fn pullback_buys_only_triggered_setups_with_their_own_exit() {
        assert!(
            default_kind("pullback", false)
                .unwrap()
                .pullback()
                .unwrap()
                .perp,
            "perpetuals by default"
        );
        let kind = Kind::Pullback(crate::pullback::Params {
            target_r: 2.0,
            perp: false,
            ..Default::default()
        });
        let rule = Rule::new(9, kind, 0);
        assert!(rule.validate().is_ok());
        let mut bad = rule.clone();
        bad.kind = Kind::Pullback(crate::pullback::Params {
            window_min: 30,
            ..Default::default()
        });
        assert!(bad.validate().is_err(), "15 or 60 minutes");
        let s = |triggered: bool, rise: f64| crate::pullback::Setup {
            trigger: 10.0,
            stop: 9.8,
            target: 10.4,
            rise_pct: rise,
            red_ms: 7,
            triggered,
        };
        let setups: HashMap<String, (crate::pullback::Setup, f64)> = [
            ("AUSDT".to_string(), (s(true, 4.0), 10.01)),
            ("BUSDT".to_string(), (s(false, 9.0), 9.9)),
            ("CUSDT".to_string(), (s(true, 6.0), 10.0)),
        ]
        .into();
        let c = pullback_candidates(&rule, &setups);
        let keys: Vec<&str> = c.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            ["CUSDT:7", "AUSDT:7"],
            "triggered only, biggest rise first"
        );
        assert_eq!(
            c[0].legs,
            vec![("CUSDT".to_string(), true, rule.usd_per_leg)]
        );
        let x = c[0].exit.unwrap();
        assert!((x.stop_pct.unwrap() - 2.0).abs() < 1e-9, "{x:?}");
        assert!((x.target_pct.unwrap() - 4.0).abs() < 1e-9);
        assert_eq!(c[1].prices, vec![("AUSDT".to_string(), 10.01)]);
        // On perpetuals the same coin's perpetual is bought.
        let mut perp = rule.clone();
        perp.kind = Kind::Pullback(crate::pullback::Params {
            perp: true,
            ..Default::default()
        });
        let c = pullback_candidates(&perp, &setups);
        assert_eq!(c[0].key, "CUSDT:7");
        assert_eq!(c[0].legs[0].0, "CUSDT_PERP");
        assert_eq!(c[0].prices[0].0, "CUSDT_PERP");
    }

    #[test]
    fn carry_buys_spot_and_sells_the_perpetual_of_high_funding_only() {
        let rule = Rule::new(3, Kind::Carry { min_apr: 30.0 }, 0);
        assert_eq!(rule.exit, CARRY_EXIT);
        assert!(rule.validate().is_ok());
        assert!(Rule::new(4, Kind::Carry { min_apr: 0.0 }, 0)
            .validate()
            .is_err());
        let rates: HashMap<String, f64> = [
            ("SOLUSDT", 0.0005),  // 55 % a year
            ("BTCUSDT", 0.0001),  // 11 %: below
            ("DOGEUSDT", 0.0003), // 33 %
            ("USDCUSDT", 0.001),  // a stablecoin: never
            ("ETHUSDT", -0.0005), // negative: shorts pay
        ]
        .into_iter()
        .map(|(s, r)| (s.to_string(), r))
        .collect();
        let c = carry_candidates(&rule, &rates);
        let keys: Vec<&str> = c.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["SOLUSDT", "DOGEUSDT"], "the highest first");
        assert_eq!(
            c[0].legs,
            vec![
                ("SOLUSDT".to_string(), true, rule.usd_per_leg),
                ("SOLUSDT_PERP".to_string(), false, rule.usd_per_leg)
            ]
        );
        assert_eq!(c[0].entry, Some(Entry::Market));
        assert!(carry_candidates(
            &Rule::new(
                5,
                Kind::Cross {
                    min_net_bps: 1.0,
                    perps: false
                },
                0
            ),
            &rates
        )
        .is_empty());
        assert_eq!(
            Kind::Carry { min_apr: 30.0 }.with_threshold(50.0),
            Kind::Carry { min_apr: 50.0 }
        );
    }

    fn pair(a: &str, b: &str, z: f64) -> PairIdea {
        let (long, short) = if z < 0.0 { (a, b) } else { (b, a) };
        PairIdea {
            a: a.into(),
            b: b.into(),
            r: 0.9,
            beta: 1.5,
            z,
            half_life: 12.0,
            t_older: -4.0,
            t_newer: -3.0,
            long: long.into(),
            short: short.into(),
            z_path: Vec::new(),
            history: GapHistory::default(),
        }
    }

    fn scan(pairs: Vec<PairIdea>) -> Scan {
        Scan {
            pairs,
            tf: "1h".into(),
            ..Scan::default()
        }
    }

    fn mv(s: &str, pct: f64) -> Move {
        Move {
            symbol: s.into(),
            window_min: 15,
            pct,
            volume_x: 3.0,
            price: 1.0,
            pct_5: None,
            pct_15: Some(pct),
            pct_60: None,
        }
    }

    #[test]
    fn pairs_rule_takes_the_widest_gaps_with_the_hedge_ratio() {
        let s = scan(vec![
            pair("AUSDT", "BUSDT", -2.2),
            pair("CUSDT", "DUSDT", 3.1),
            pair("EUSDT", "FUSDT", -1.0),
        ]);
        let rule = Rule::new(
            1,
            Kind::Pairs {
                min_z: 2.0,
                beta_weighted: true,
            },
            0,
        );
        let c = candidates(&rule, None, Some(&s), None, 0);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].key, "DUSDT/CUSDT", "widest first; cheap leg first");
        assert_eq!(c[1].key, "AUSDT/BUSDT");
        // Beta-weighted: $5 of the a-leg, $7.50 of the b-leg (beta 1.5).
        let legs = &c[1].legs;
        assert_eq!(legs[0], ("AUSDT".to_string(), true, 5.0));
        assert_eq!(legs[1], ("BUSDT".to_string(), false, 7.5));
        assert!(c[0].origin.contains("1h"));
        assert!(
            candidates(&rule, None, None, None, 0).is_empty(),
            "no scan: nothing"
        );
    }

    #[test]
    fn laggards_and_fades_come_from_the_live_picture() {
        let live = Live {
            laggards: vec![
                Laggard {
                    leader: "AUSDT".into(),
                    follower: "BUSDT".into(),
                    r: 0.8,
                    minutes: 60,
                    window_min: 15,
                    lead_pct: 5.0,
                    follow_pct: 1.0,
                },
                Laggard {
                    leader: "CUSDT".into(),
                    follower: "DUSDT".into(),
                    r: 0.5,
                    minutes: 60,
                    window_min: 15,
                    lead_pct: -6.0,
                    follow_pct: -1.0,
                },
            ],
            gainers: vec![mv("GUSDT", 8.0), mv("HUSDT", 3.0), mv("IUSDT", 1.0)],
            losers: vec![mv("LUSDT", -4.0)],
            ..Live::default()
        };
        let lag = Rule::new(
            2,
            Kind::Laggards {
                min_gap_pct: 3.0,
                min_r: 0.7,
            },
            0,
        );
        let c = candidates(&lag, Some(&live), None, None, 0);
        assert_eq!(c.len(), 1, "the second has r 0.5: {c:?}");
        assert_eq!(c[0].key, "BUSDT/AUSDT");
        assert_eq!(c[0].legs[0], ("BUSDT".to_string(), true, 5.0));
        assert_eq!(c[0].legs[1], ("AUSDT".to_string(), false, 5.0));
        let lag_any = Rule::new(
            3,
            Kind::Laggards {
                min_gap_pct: 3.0,
                min_r: 0.0,
            },
            0,
        );
        let c = candidates(&lag_any, Some(&live), None, None, 0);
        assert_eq!(c.len(), 2);
        assert!(c[0].key.starts_with("DUSDT"), "the wider gap first: {c:?}");
        assert!(
            c[0].name.starts_with("sell DUSDT") || c[0].name.starts_with("sell D"),
            "{}",
            c[0].name
        );
        assert!(!c[0].legs[0].1, "a fall: sell the follower");
        let fade = Rule::new(
            4,
            Kind::Fade {
                legs: 5,
                min_pct: 2.0,
                up: true,
            },
            0,
        );
        let c = candidates(&fade, Some(&live), None, None, 0);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].legs.len(), 2, "two gainers of 2% or more");
        assert!(c[0].legs.iter().all(|(_, long, _)| !long), "faded: sold");
        assert_eq!(c[0].key, "fade:up:GUSDT,HUSDT");
        let fade_down = Rule::new(
            5,
            Kind::Fade {
                legs: 5,
                min_pct: 2.0,
                up: false,
            },
            0,
        );
        let c = candidates(&fade_down, Some(&live), None, None, 0);
        assert_eq!(c[0].legs, vec![("LUSDT".to_string(), true, 5.0)]);
        let none = Rule::new(
            6,
            Kind::Fade {
                legs: 5,
                min_pct: 50.0,
                up: true,
            },
            0,
        );
        assert!(candidates(&none, Some(&live), None, None, 0).is_empty());
    }

    #[test]
    fn due_skips_open_and_cooling_findings_and_keeps_the_room() {
        let mut rule = Rule::new(
            7,
            Kind::Pairs {
                min_z: 2.0,
                beta_weighted: false,
            },
            0,
        );
        rule.max_open = 2;
        rule.cooldown_min = 60;
        let c = |k: &str| {
            Candidate::plain(
                k.into(),
                k.into(),
                String::new(),
                k.split('/')
                    .map(|s| (format!("{s}USDT"), true, 1.0))
                    .collect(),
            )
        };
        let cands = vec![c("A/B"), c("C/D"), c("E/F"), c("G/H")];
        let none = HashSet::new();
        // A/B is open; C/D closed 10 min ago (cooling); E/F closed 2 h ago.
        let open = vec![
            rule.tag("A/B"),
            "auto:9:Z/Y".to_string(),
            "manual".to_string(),
        ];
        let mut fired = HashMap::new();
        fired.insert(rule.tag("C/D"), 50 * 60_000);
        fired.insert(rule.tag("E/F"), -60 * 60_000);
        let now = 60 * 60_000;
        let d = due(&rule, &cands, &open, &none, &fired, now);
        let keys: Vec<&str> = d.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            ["E/F"],
            "one open already, room for one more; C/D cools"
        );
        rule.max_open = 5;
        let d = due(&rule, &cands, &open, &none, &fired, now);
        let keys: Vec<&str> = d.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["E/F", "G/H"]);
        rule.on = false;
        assert!(due(&rule, &cands, &open, &none, &fired, now).is_empty());
        // A coin already in one of the rule's open baskets, or in a basket
        // picked earlier this pass, keeps the next candidate out.
        rule.on = true;
        let busy: HashSet<String> = ["GUSDT".to_string()].into_iter().collect();
        let d = due(&rule, &cands, &open, &busy, &fired, now);
        let keys: Vec<&str> = d.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["E/F"], "G/H shares G with an open basket");
        let overlapping = vec![c("E/F"), c("F/X"), c("Y/Z")];
        let d = due(&rule, &overlapping, &open, &none, &fired, now);
        let keys: Vec<&str> = d.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["E/F", "Y/Z"], "F/X shares F with E/F picked first");
        // Validation.
        rule.on = true;
        assert!(rule.validate().is_ok());
        rule.usd_per_leg = 0.0;
        assert!(rule.validate().is_err());
        rule.usd_per_leg = 5.0;
        rule.max_open = 0;
        assert!(rule.validate().is_err());
        rule.max_open = 3;
        rule.kind = Kind::Laggards {
            min_gap_pct: 1.0,
            min_r: 2.0,
        };
        assert!(rule.validate().is_err());
        let json = serde_json::to_string(&rule).unwrap();
        assert_eq!(serde_json::from_str::<Rule>(&json).unwrap(), rule);
        assert!(rule.words().contains("#7 ON"));
    }

    #[test]
    fn spreads_groups_and_options_make_candidates_with_their_own_sizes() {
        use crate::live_ideas::{Group, Spread, HEDGE};
        use crate::options::{BoxSpread, Parity, Side};
        let live = Live {
            spreads: vec![
                Spread {
                    a: "AUSDT".into(),
                    b: "BUSDT".into(),
                    r: 0.9,
                    minutes: 60,
                    z: 2.5,
                    sd_pct: 0.4,
                    pct_a: 3.0,
                    pct_b: 1.0,
                },
                Spread {
                    a: "CUSDT".into(),
                    b: "DUSDT".into(),
                    r: 0.82,
                    minutes: 60,
                    z: -1.6,
                    sd_pct: 0.2,
                    pct_a: 0.0,
                    pct_b: 1.0,
                },
            ],
            groups: vec![
                Group {
                    members: vec!["EUSDT".into(), "FUSDT".into(), "GUSDT".into()],
                    r_mean: 0.85,
                    minutes: 60,
                    pct: 2.0,
                    z_hedge: Some(2.2),
                },
                Group {
                    members: vec!["HUSDT".into(), HEDGE.into(), "IUSDT".into()],
                    r_mean: 0.85,
                    minutes: 60,
                    pct: 2.0,
                    z_hedge: Some(-3.0),
                },
            ],
            ..Live::default()
        };
        let sp = Rule::new(
            1,
            Kind::Spreads {
                min_r: 0.8,
                min_z: 2.0,
            },
            0,
        );
        let c = candidates(&sp, Some(&live), None, None, 0);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].key, "BUSDT/AUSDT", "A rich: buy B, sell A");
        assert_eq!(c[0].legs[0], ("BUSDT".to_string(), true, 5.0));
        assert_eq!(c[0].legs[1], ("AUSDT".to_string(), false, 5.0));
        let gr = Rule::new(
            2,
            Kind::Groups {
                min_z: 2.0,
                legs: 2,
            },
            0,
        );
        let c = candidates(&gr, Some(&live), None, None, 0);
        assert_eq!(c.len(), 1, "a group holding BTC is skipped: {c:?}");
        assert_eq!(c[0].legs.len(), 3, "two members and BTC");
        assert!(c[0].legs[..2].iter().all(|l| !l.1), "ran ahead: sold");
        assert_eq!(c[0].legs[2], (HEDGE.to_string(), true, 10.0));
        assert_eq!(c[0].key, "group:EUSDT,FUSDT");
        // Options: the sizes follow the spot leg's coins, not the premium.
        let now = 1_792_300_000_000;
        let o = Options {
            parity: vec![Parity {
                underlying: "BTCUSDT".into(),
                expiry_ms: now + 40 * 86_400_000,
                strike: 80_000.0,
                call: "BTC-261127-80000-C".into(),
                put: "BTC-261127-80000-P".into(),
                unit: 1.0,
                side: Side::Reversal,
                spot: 80_000.0,
                forward: 80_400.0,
                rate: 0.05,
                days: 40.0,
                gross_bps: 50.0,
                fees_bps: 16.0,
                implied_forward: 80_000.0,
            }],
            boxes: vec![BoxSpread {
                underlying: "BTCUSDT".into(),
                expiry_ms: now + 40 * 86_400_000,
                low: 70_000.0,
                high: 80_000.0,
                unit: 1.0,
                call_low: "BTC-261127-70000-C".into(),
                put_low: "BTC-261127-70000-P".into(),
                call_high: "BTC-261127-80000-C".into(),
                put_high: "BTC-261127-80000-P".into(),
                buy: true,
                cost: 9_800.0,
                rate: 0.05,
                days: 40.0,
                gross_bps: 30.0,
                fees_bps: 12.0,
                spot: 80_000.0,
            }],
            ..Options::default()
        };
        let mut par = Rule::new(3, Kind::Parity { min_bps: 20.0 }, 0);
        par.usd_per_leg = 800.0;
        assert_eq!(par.exit, OPTIONS_EXIT, "options rules hold to expiry");
        let c = candidates(&par, None, None, Some(&o), now);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].legs.len(), 3);
        assert_eq!(c[0].legs[0], ("BTCUSDT".to_string(), false, 800.0));
        assert_eq!(c[0].fixed.len(), 3);
        assert!(
            (c[0].fixed[0].1 - 0.01).abs() < 1e-12,
            "$800 of BTC at 80000"
        );
        assert!(
            (c[0].fixed[1].1 - 0.01).abs() < 1e-12,
            "one contract a coin"
        );
        assert_eq!(c[0].max_minutes, Some(40 * 24 * 60));
        let par_high = Rule::new(4, Kind::Parity { min_bps: 40.0 }, 0);
        assert!(candidates(&par_high, None, None, Some(&o), now).is_empty());
        let bx = Rule::new(5, Kind::Boxes { min_bps: 10.0 }, 0);
        let c = candidates(&bx, None, None, Some(&o), now);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].legs.len(), 4);
        assert_eq!(c[0].fixed.len(), 4);
        assert!(
            c[0].legs[0].1 && !c[0].legs[1].1,
            "long low call, short high call"
        );
        // Thresholds go through one door.
        assert_eq!(bx.kind.threshold(), (10.0, "bps"));
        assert_eq!(sp.kind.with_threshold(3.0).threshold(), (3.0, "sd"));
        assert_eq!(
            gr.kind.with_threshold(1.0),
            Kind::Groups {
                min_z: 1.0,
                legs: 2
            }
        );
        assert!(Rule::new(
            6,
            Kind::Groups {
                min_z: 1.0,
                legs: 1
            },
            0
        )
        .validate()
        .is_err());
        assert!(Rule::new(
            7,
            Kind::Spreads {
                min_r: 2.0,
                min_z: 1.0
            },
            0
        )
        .validate()
        .is_err());
        assert!(Rule::new(8, Kind::Parity { min_bps: -1.0 }, 0)
            .validate()
            .is_err());
        let mut big = Rule::new(9, Kind::Parity { min_bps: 0.0 }, 0);
        big.max_open = MAX_OPEN;
        assert!(big.validate().is_ok());
        big.max_open += 1;
        assert!(big.validate().is_err());
    }

    #[test]
    fn a_synthetic_pair_rule_sells_the_rich_synthetic_with_equal_dollars_a_side() {
        use crate::live_ideas::SynPair;
        let live = Live {
            syn_pairs: vec![
                SynPair {
                    a: "SOL+2".into(),
                    b: "ETH+5".into(),
                    a_members: vec!["SOLUSDT".into(), "APTUSDT".into(), "SUIUSDT".into()],
                    b_members: (0..6).map(|i| format!("E{i}USDT")).collect(),
                    r: 0.8,
                    z: 2.4,
                    sd_pct: 0.2,
                    minutes: 60,
                    pct_a: 2.0,
                    pct_b: 0.5,
                },
                SynPair {
                    a: "X+2".into(),
                    b: "Y+2".into(),
                    a_members: vec!["X0USDT".into(), "X1USDT".into(), "X2USDT".into()],
                    b_members: vec!["Y0USDT".into(), "Y1USDT".into(), "Y2USDT".into()],
                    r: 0.5,
                    z: 3.0,
                    sd_pct: 0.2,
                    minutes: 60,
                    pct_a: 0.0,
                    pct_b: 0.0,
                },
            ],
            ..Live::default()
        };
        let mut rule = Rule::new(
            1,
            Kind::SynPairs {
                min_r: 0.6,
                min_z: 2.0,
            },
            0,
        );
        rule.usd_per_leg = 6.0;
        let c = candidates(&rule, Some(&live), None, None, 0);
        assert_eq!(c.len(), 1, "the second has r 0.5: {c:?}");
        assert_eq!(c[0].key, "syn:ETH+5/SOL+2", "SOL+2 rich: buy ETH+5");
        assert_eq!(c[0].legs.len(), 9);
        let long_total: f64 = c[0].legs.iter().filter(|l| l.1).map(|l| l.2).sum();
        let short_total: f64 = c[0].legs.iter().filter(|l| !l.1).map(|l| l.2).sum();
        assert!((long_total - 36.0).abs() < 1e-9 && (short_total - 36.0).abs() < 1e-9);
        assert_eq!(c[0].legs[6], ("SOLUSDT".to_string(), false, 12.0));
        assert_eq!(rule.kind.threshold(), (2.0, "sd"));
        assert!(rule.validate().is_ok());
        assert!(rule.words().contains("synthetic pairs"));
    }
}
