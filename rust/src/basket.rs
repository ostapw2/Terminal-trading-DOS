//! Baskets: a synthetic instrument of several legs traded as ONE unit on
//! the paper ledger — a pair, a group of coins that move as one, the
//! laggard against its leader.  Entered leg by leg with resting limit
//! orders a little inside the price (maker fees), finished at market when
//! a leg has not filled in time; left the same way when the basket's own
//! PnL reaches its target or stop, or its time is up.
//!
//! This module is pure: a [`Basket`] is a state machine that, given the
//! current prices and time, says what to do ([`Intent`]) and takes the
//! fills back ([`Basket::on_fill`]).  The runtime executes the intents on
//! the broker and owns the order ids.  The broker's positions stay netted
//! per symbol (risk and equity read them); a basket keeps its own `held`,
//! `avg` and fees per leg from its own orders, so two baskets in the same
//! coin each show their own PnL.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::strategies::context::{Fill, Side};

/// Forty: a synthetic of twenty coins against another.
pub const MAX_LEGS: usize = 40;
/// How far inside the price a leg's limit rests (basis points).
pub const DEFAULT_OFFSET_BPS: f64 = 5.0;
/// How long a leg may rest before the rest is done at market.
pub const DEFAULT_WAIT_SECS: u32 = 90;
/// The exit a new basket gets: +2 % closes it, -1 % stops it, 4 hours at most.
pub const DEFAULT_EXIT: ExitRule = ExitRule {
    target_pct: Some(2.0),
    stop_pct: Some(1.0),
    max_minutes: Some(240),
};
/// Closed baskets kept in the book (the oldest go first).
pub const CLOSED_KEPT: usize = 50;
const EPS: f64 = 1e-9;

/// One leg: a symbol, a direction, and what the basket holds in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    pub symbol: String,
    pub long: bool,
    /// Dollars the leg was sized with.
    pub usd: f64,
    /// Coins to hold once entered (set from the price at open).
    pub target_qty: f64,
    /// Coins this basket holds in the leg now (direction is `long`).
    pub held: f64,
    pub avg: f64,
    pub realized: f64,
    pub fees: f64,
    /// The resting order of the current phase, if any.
    pub order: Option<u64>,
    /// Coins to hold regardless of price (set with [`Basket::fix_qty`]).
    #[serde(default)]
    pub fixed_qty: Option<f64>,
}

impl Leg {
    fn dir(&self) -> f64 {
        if self.long {
            1.0
        } else {
            -1.0
        }
    }
    /// Coins still to buy (or sell short) to reach the target.
    pub fn remaining(&self) -> f64 {
        (self.target_qty - self.held).max(0.0)
    }
    pub fn entered(&self) -> bool {
        self.held + EPS >= self.target_qty
    }
    pub fn flat(&self) -> bool {
        self.held <= EPS
    }
    /// The side that opens the leg / that closes it.
    fn open_side(&self) -> Side {
        if self.long {
            Side::Buy
        } else {
            Side::Sell
        }
    }
    fn close_side(&self) -> Side {
        if self.long {
            Side::Sell
        } else {
            Side::Buy
        }
    }
    /// A fill of one of this leg's orders: opening fills average in,
    /// closing fills realize; the fee is the fill's.
    fn apply(&mut self, f: &Fill) {
        if f.qty <= 0.0 {
            return;
        }
        self.fees += f.fee;
        if f.side == self.open_side() {
            let total = self.held + f.qty;
            self.avg = (self.avg * self.held + f.price * f.qty) / total;
            self.held = total;
        } else {
            let qty = f.qty.min(self.held);
            self.realized += (f.price - self.avg) * qty * self.dir();
            self.held = (self.held - qty).max(0.0);
            if self.held <= EPS {
                self.held = 0.0;
            }
        }
    }
}

/// How the legs go in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// Rest `offset_bps` inside the price (a buy below it, a sell above);
    /// after `wait_secs` what has not filled is done at market.
    Limit {
        offset_bps: f64,
        wait_secs: u32,
    },
    Market,
}

impl Default for Entry {
    fn default() -> Self {
        Entry::Limit {
            offset_bps: DEFAULT_OFFSET_BPS,
            wait_secs: DEFAULT_WAIT_SECS,
        }
    }
}

/// When the basket leaves: net PnL as % of its dollars, or time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExitRule {
    pub target_pct: Option<f64>,
    pub stop_pct: Option<f64>,
    pub max_minutes: Option<u32>,
}

impl ExitRule {
    pub fn words(&self) -> String {
        let mut v = Vec::new();
        if let Some(t) = self.target_pct {
            v.push(crate::i18n::pick(
                format!("target +{t:.1}%"),
                format!("ціль +{t:.1}%"),
            ));
        }
        if let Some(s) = self.stop_pct {
            v.push(crate::i18n::pick(
                format!("stop -{s:.1}%"),
                format!("стоп -{s:.1}%"),
            ));
        }
        if let Some(m) = self.max_minutes {
            v.push(crate::i18n::pick(
                format!("max {m} min"),
                format!("макс. {m} хв"),
            ));
        }
        if v.is_empty() {
            crate::i18n::pick(
                "no exit rule (close it yourself)",
                "правила виходу немає (закрий сам)",
            )
            .to_string()
        } else {
            v.join("  ")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Target,
    Stop,
    Time,
    User,
    /// Given up while entering.
    Abandoned,
}

impl Why {
    pub fn label(self) -> &'static str {
        match self {
            Why::Target => "target",
            Why::Stop => "stop",
            Why::Time => "time",
            Why::User => "closed",
            Why::Abandoned => "dropped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Entering {
        since_ms: i64,
        /// When what has not filled goes to market (`None`: never).
        market_after_ms: Option<i64>,
    },
    Open {
        since_ms: i64,
    },
    Exiting {
        since_ms: i64,
        why: Why,
        market_after_ms: Option<i64>,
    },
    Closed {
        at_ms: i64,
        why: Why,
    },
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Entering { .. } => "ENTERING",
            State::Open { .. } => "OPEN",
            State::Exiting { .. } => "EXITING",
            State::Closed { .. } => "CLOSED",
        }
    }
    pub fn is_closed(&self) -> bool {
        matches!(self, State::Closed { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Basket {
    pub id: u64,
    /// "buy SOL / sell AVAX", "group of 6".
    pub name: String,
    /// Which finding it came from, for the row.
    pub origin: String,
    pub created_ms: i64,
    pub legs: Vec<Leg>,
    pub entry: Entry,
    pub exit: ExitRule,
    pub state: State,
    /// An order the runtime could not place (the risk gate, no price): the
    /// basket waits for the user (`:basket drop N` / `:basket retry N`).
    #[serde(default)]
    pub blocked: Option<String>,
    /// Who opened it and for which finding: "auto:<rule>:<key>" from a
    /// rule (`crate::autobasket`), empty from the user.
    #[serde(default)]
    pub tag: String,
    /// Leverage of its perpetual legs (`crate::futures`); 1 for spot ones.
    #[serde(default = "one_lev")]
    pub leverage: f64,
}

fn one_lev() -> f64 {
    1.0
}

/// What the runtime should do now for a basket, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Limit {
        leg: usize,
        side: Side,
        qty: f64,
        limit: f64,
    },
    Cancel {
        leg: usize,
        order: u64,
    },
    Market {
        leg: usize,
        side: Side,
        qty: f64,
    },
    Note(String),
}

/// A basket's PnL at the current prices.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pnl {
    pub unrealized: f64,
    pub realized: f64,
    pub fees: f64,
    /// The dollars the basket was sized with.
    pub notional: f64,
}

impl Pnl {
    pub fn net(&self) -> f64 {
        self.unrealized + self.realized - self.fees
    }
    pub fn pct(&self) -> f64 {
        if self.notional > 0.0 {
            self.net() / self.notional * 100.0
        } else {
            0.0
        }
    }
}

/// A limit that rests `offset_bps` inside `last`: a buy below, a sell above.
pub fn limit_price(last: f64, buy: bool, offset_bps: f64) -> f64 {
    let o = offset_bps.max(0.0) / 10_000.0;
    if buy {
        last * (1.0 - o)
    } else {
        last * (1.0 + o)
    }
}

/// Fees in and out of `total_usd`: (all legs as maker, all as taker).
pub fn est_fees(total_usd: f64, maker_bps: f64, taker_bps: f64) -> (f64, f64) {
    (
        2.0 * total_usd * maker_bps / 10_000.0,
        2.0 * total_usd * taker_bps / 10_000.0,
    )
}

impl Basket {
    /// `legs`: (symbol, long, dollars).  Sized later with [`Basket::size`].
    pub fn new(
        id: u64,
        name: impl Into<String>,
        origin: impl Into<String>,
        legs: &[(String, bool, f64)],
        entry: Entry,
        exit: ExitRule,
        now_ms: i64,
    ) -> Result<Basket, String> {
        if legs.is_empty() {
            return Err("a basket needs at least one leg".into());
        }
        if legs.len() > MAX_LEGS {
            return Err(format!("a basket has {MAX_LEGS} legs at most"));
        }
        let mut seen = std::collections::HashSet::new();
        for (s, _, usd) in legs {
            if s.is_empty() || !seen.insert(s.as_str()) {
                return Err(format!("leg {s}: empty or twice in the basket"));
            }
            if !(usd.is_finite() && *usd > 0.0) {
                return Err(format!("leg {s}: dollars must be a positive number"));
            }
        }
        if let Entry::Limit {
            offset_bps,
            wait_secs,
        } = entry
        {
            if !(offset_bps.is_finite() && (0.0..=1_000.0).contains(&offset_bps)) {
                return Err("offset must be 0-1000 bps".into());
            }
            if wait_secs == 0 {
                return Err("wait must be at least a second".into());
            }
        }
        for v in [exit.target_pct, exit.stop_pct] {
            if v.is_some_and(|x| !(x.is_finite() && x > 0.0)) {
                return Err("target and stop are positive percents".into());
            }
        }
        if exit.max_minutes == Some(0) {
            return Err("max minutes must be at least one".into());
        }
        Ok(Basket {
            id,
            name: name.into(),
            origin: origin.into(),
            created_ms: now_ms,
            legs: legs
                .iter()
                .map(|(symbol, long, usd)| Leg {
                    symbol: symbol.clone(),
                    long: *long,
                    usd: *usd,
                    target_qty: 0.0,
                    held: 0.0,
                    avg: 0.0,
                    realized: 0.0,
                    fees: 0.0,
                    order: None,
                    fixed_qty: None,
                })
                .collect(),
            entry,
            exit,
            state: State::Entering {
                since_ms: now_ms,
                market_after_ms: None,
            },
            blocked: None,
            tag: String::new(),
            leverage: 1.0,
        })
    }

    /// Does this basket sell something it does not hold on spot?  (A short
    /// leg on a coin, not an option and not already a perpetual.)
    pub fn needs_perps(&self) -> bool {
        self.legs.iter().any(|l| {
            !l.long
                && !l.symbol.contains('-')
                && !l.symbol.contains('@')
                && !crate::futures::is_perp(&l.symbol)
        })
    }

    /// Put every plain coin leg on its perpetual, at `leverage`: the only
    /// way to hold a short.  Option legs stay as they are.
    pub fn move_to_perps(&mut self, leverage: f64) {
        for l in &mut self.legs {
            if !l.symbol.contains('-') && !l.symbol.contains('@') {
                l.symbol = crate::futures::perp(&l.symbol);
            }
        }
        self.leverage = crate::futures::clamp_leverage(leverage);
    }

    pub fn total_usd(&self) -> f64 {
        self.legs.iter().map(|l| l.usd).sum()
    }

    /// Set every leg's target from the marks; `Err` names a leg without a
    /// price.  A leg fixed in coins ([`Basket::fix_qty`]) keeps its coins
    /// and takes its dollars from the mark.
    pub fn size(&mut self, marks: &HashMap<String, f64>) -> Result<(), String> {
        for l in &mut self.legs {
            let p = marks.get(&l.symbol).copied().unwrap_or(0.0);
            if !(p.is_finite() && p >= 0.0) || (p == 0.0 && l.fixed_qty.is_none()) {
                return Err(format!("no price for {}", l.symbol));
            }
            match l.fixed_qty {
                Some(q) => {
                    l.target_qty = q;
                    l.usd = q * p;
                }
                None => l.target_qty = l.usd / p,
            }
        }
        Ok(())
    }

    /// Hold `qty` coins (contracts) of `symbol` whatever its price: for
    /// legs that must match another leg one to one (an option against its
    /// spot).  `Err` when the leg is not in the basket.
    pub fn fix_qty(&mut self, symbol: &str, qty: f64) -> Result<(), String> {
        if !(qty.is_finite() && qty > 0.0) {
            return Err(format!("leg {symbol}: coins must be a positive number"));
        }
        let leg = self
            .legs
            .iter_mut()
            .find(|l| l.symbol == symbol)
            .ok_or_else(|| format!("no leg {symbol}"))?;
        leg.fixed_qty = Some(qty);
        Ok(())
    }

    /// Start the clock: the entry orders go out now.
    pub fn start(&mut self, now_ms: i64) {
        self.state = State::Entering {
            since_ms: now_ms,
            market_after_ms: self.wait_until(now_ms),
        };
    }

    fn wait_until(&self, now_ms: i64) -> Option<i64> {
        match self.entry {
            Entry::Limit { wait_secs, .. } => Some(now_ms + wait_secs as i64 * 1000),
            Entry::Market => None,
        }
    }

    fn offset(&self) -> Option<f64> {
        match self.entry {
            Entry::Limit { offset_bps, .. } => Some(offset_bps),
            Entry::Market => None,
        }
    }

    pub fn entered_legs(&self) -> usize {
        self.legs.iter().filter(|l| l.entered()).count()
    }

    pub fn pnl(&self, marks: &HashMap<String, f64>) -> Pnl {
        let mut p = Pnl {
            notional: self.total_usd(),
            ..Pnl::default()
        };
        for l in &self.legs {
            p.realized += l.realized;
            p.fees += l.fees;
            if l.held > 0.0 {
                let mark = marks.get(&l.symbol).copied().unwrap_or(l.avg);
                p.unrealized += (mark - l.avg) * l.held * l.dir();
            }
        }
        p
    }

    /// A fill of one of this basket's resting orders (by its order id).
    /// Returns the leg it went to.
    pub fn on_fill(&mut self, order_id: u64, f: &Fill) -> Option<usize> {
        let i = self.legs.iter().position(|l| l.order == Some(order_id))?;
        self.legs[i].apply(f);
        // The broker fills a resting order whole.
        self.legs[i].order = None;
        Some(i)
    }

    /// A fill the runtime got straight from a market order for `leg`.
    pub fn on_fill_leg(&mut self, leg: usize, f: &Fill) {
        if let Some(l) = self.legs.get_mut(leg) {
            l.apply(f);
        }
    }

    /// The runtime placed a resting order for `leg`.
    pub fn set_order(&mut self, leg: usize, order: u64) {
        if let Some(l) = self.legs.get_mut(leg) {
            l.order = Some(order);
        }
    }

    /// The runtime cancelled (or lost) the leg's resting order.
    pub fn clear_order(&mut self, leg: usize) {
        if let Some(l) = self.legs.get_mut(leg) {
            l.order = None;
        }
    }

    /// Does the exit rule fire at these prices and this time?
    fn exit_due(&self, now_ms: i64, marks: &HashMap<String, f64>) -> Option<Why> {
        let State::Open { since_ms } = self.state else {
            return None;
        };
        let pct = self.pnl(marks).pct();
        if self.exit.target_pct.is_some_and(|t| pct >= t) {
            return Some(Why::Target);
        }
        if self.exit.stop_pct.is_some_and(|s| pct <= -s) {
            return Some(Why::Stop);
        }
        if self
            .exit
            .max_minutes
            .is_some_and(|m| now_ms - since_ms >= m as i64 * 60_000)
        {
            return Some(Why::Time);
        }
        None
    }

    /// Ask the basket to leave (`User`), or give it up while entering
    /// (`Abandoned`); the exit rule's own reasons come through `step`.
    pub fn request_exit(
        &mut self,
        why: Why,
        now_ms: i64,
        marks: &HashMap<String, f64>,
    ) -> Vec<Intent> {
        if self.state.is_closed() || matches!(self.state, State::Exiting { .. }) {
            return Vec::new();
        }
        let mut out = Vec::new();
        // Nothing of the old phase may rest.
        for (i, l) in self.legs.iter_mut().enumerate() {
            if let Some(o) = l.order.take() {
                out.push(Intent::Cancel { leg: i, order: o });
            }
        }
        // A stop is a loss cut: at market, now.  The others may rest first.
        let market_after_ms = if why == Why::Stop {
            Some(now_ms)
        } else {
            self.wait_until(now_ms)
        };
        self.state = State::Exiting {
            since_ms: now_ms,
            why,
            market_after_ms,
        };
        self.blocked = None;
        out.extend(self.step(now_ms, marks));
        out
    }

    /// The state machine: what to do now.  Intents for a leg come in the
    /// order to run them (a cancel before a market order).
    pub fn step(&mut self, now_ms: i64, marks: &HashMap<String, f64>) -> Vec<Intent> {
        if self.blocked.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        match self.state {
            State::Entering {
                market_after_ms, ..
            } => {
                if self.legs.iter().all(|l| l.entered()) {
                    self.state = State::Open { since_ms: now_ms };
                    out.push(Intent::Note(format!(
                        "basket #{} {}: open, {} legs, ${:.2}",
                        self.id,
                        self.name,
                        self.legs.len(),
                        self.total_usd()
                    )));
                    return out;
                }
                let at_market = market_after_ms.is_none_or(|t| now_ms >= t);
                for i in 0..self.legs.len() {
                    let l = &self.legs[i];
                    if l.entered() {
                        continue;
                    }
                    let side = l.open_side();
                    let qty = l.remaining();
                    if at_market {
                        if let Some(o) = l.order {
                            out.push(Intent::Cancel { leg: i, order: o });
                        }
                        out.push(Intent::Market { leg: i, side, qty });
                    } else if l.order.is_none() {
                        let Some(&last) = marks.get(&l.symbol) else {
                            continue;
                        };
                        let offset = self.offset().unwrap_or(0.0);
                        out.push(Intent::Limit {
                            leg: i,
                            side,
                            qty,
                            limit: limit_price(last, l.long, offset),
                        });
                    }
                }
            }
            State::Open { .. } => {
                if let Some(why) = self.exit_due(now_ms, marks) {
                    return self.request_exit(why, now_ms, marks);
                }
            }
            State::Exiting {
                why,
                market_after_ms,
                ..
            } => {
                if self.legs.iter().all(|l| l.flat()) {
                    let p = self.pnl(marks);
                    self.state = State::Closed { at_ms: now_ms, why };
                    for l in &mut self.legs {
                        l.order = None;
                    }
                    out.push(Intent::Note(format!(
                        "basket #{} {}: {} at {}{:.2} net (fees ${:.2})",
                        self.id,
                        self.name,
                        why.label(),
                        if p.net() >= 0.0 { "+$" } else { "-$" },
                        p.net().abs(),
                        p.fees
                    )));
                    return out;
                }
                let at_market = market_after_ms.is_none_or(|t| now_ms >= t);
                for i in 0..self.legs.len() {
                    let l = &self.legs[i];
                    if l.flat() {
                        continue;
                    }
                    let side = l.close_side();
                    let qty = l.held;
                    if at_market {
                        if let Some(o) = l.order {
                            out.push(Intent::Cancel { leg: i, order: o });
                        }
                        out.push(Intent::Market { leg: i, side, qty });
                    } else if l.order.is_none() {
                        let Some(&last) = marks.get(&l.symbol) else {
                            continue;
                        };
                        let offset = self.offset().unwrap_or(0.0);
                        out.push(Intent::Limit {
                            leg: i,
                            side,
                            qty,
                            limit: limit_price(last, !l.long, offset),
                        });
                    }
                }
            }
            State::Closed { .. } => {}
        }
        out
    }
}

/// The book: open and recently closed baskets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub baskets: Vec<Basket>,
    #[serde(default = "one")]
    pub next_id: u64,
}

fn one() -> u64 {
    1
}

impl Book {
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        id
    }
    pub fn get(&self, id: u64) -> Option<&Basket> {
        self.baskets.iter().find(|b| b.id == id)
    }
    pub fn get_mut(&mut self, id: u64) -> Option<&mut Basket> {
        self.baskets.iter_mut().find(|b| b.id == id)
    }
    /// Dollars of the baskets that are not closed.
    pub fn open_notional(&self) -> f64 {
        self.baskets
            .iter()
            .filter(|b| !b.state.is_closed())
            .map(Basket::total_usd)
            .sum()
    }
    pub fn open_count(&self) -> usize {
        self.baskets.iter().filter(|b| !b.state.is_closed()).count()
    }
    /// Drop the oldest closed baskets beyond [`CLOSED_KEPT`].
    pub fn retire(&mut self) {
        let mut closed = self.baskets.iter().filter(|b| b.state.is_closed()).count();
        if closed <= CLOSED_KEPT {
            return;
        }
        self.baskets.retain(|b| {
            if b.state.is_closed() && closed > CLOSED_KEPT {
                closed -= 1;
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(v: &[(&str, f64)]) -> HashMap<String, f64> {
        v.iter().map(|(s, p)| (s.to_string(), *p)).collect()
    }

    fn fill(side: Side, qty: f64, price: f64, order_id: u64, fee: f64) -> Fill {
        Fill {
            side,
            qty,
            price,
            time_ms: 0,
            order_id,
            fee,
        }
    }

    fn pair() -> Basket {
        let legs = vec![
            ("AUSDT".to_string(), true, 100.0),
            ("BUSDT".to_string(), false, 100.0),
        ];
        let mut b = Basket::new(
            1,
            "buy A / sell B",
            "test",
            &legs,
            Entry::default(),
            DEFAULT_EXIT,
            0,
        )
        .unwrap();
        b.size(&marks(&[("AUSDT", 10.0), ("BUSDT", 20.0)])).unwrap();
        b.start(0);
        b
    }

    #[test]
    fn a_basket_is_checked_when_made() {
        let leg = |s: &str, usd: f64| (s.to_string(), true, usd);
        let e = Entry::default();
        assert!(Basket::new(1, "n", "o", &[], e, DEFAULT_EXIT, 0).is_err());
        let many: Vec<_> = (0..=MAX_LEGS).map(|i| leg(&format!("S{i}"), 1.0)).collect();
        assert!(Basket::new(1, "n", "o", &many, e, DEFAULT_EXIT, 0).is_err());
        assert!(Basket::new(
            1,
            "n",
            "o",
            &[leg("A", 1.0), leg("A", 1.0)],
            e,
            DEFAULT_EXIT,
            0
        )
        .is_err());
        assert!(Basket::new(1, "n", "o", &[leg("A", 0.0)], e, DEFAULT_EXIT, 0).is_err());
        assert!(Basket::new(
            1,
            "n",
            "o",
            &[leg("A", 1.0)],
            Entry::Limit {
                offset_bps: 5.0,
                wait_secs: 0
            },
            DEFAULT_EXIT,
            0
        )
        .is_err());
        let bad_exit = ExitRule {
            stop_pct: Some(-1.0),
            ..DEFAULT_EXIT
        };
        assert!(Basket::new(1, "n", "o", &[leg("A", 1.0)], e, bad_exit, 0).is_err());
        let mut ok = Basket::new(1, "n", "o", &[leg("A", 50.0)], e, DEFAULT_EXIT, 0).unwrap();
        assert!(ok.size(&marks(&[])).is_err(), "no price");
        ok.size(&marks(&[("A", 25.0)])).unwrap();
        assert!((ok.legs[0].target_qty - 2.0).abs() < 1e-12);
        assert!((ok.total_usd() - 50.0).abs() < 1e-12);
        assert!((limit_price(100.0, true, 5.0) - 99.95).abs() < 1e-9);
        assert!((limit_price(100.0, false, 5.0) - 100.05).abs() < 1e-9);
        assert_eq!(est_fees(1_000.0, 10.0, 20.0), (2.0, 4.0));
    }

    #[test]
    fn legs_rest_inside_the_price_then_the_basket_opens() {
        let mut b = pair();
        let m = marks(&[("AUSDT", 10.0), ("BUSDT", 20.0)]);
        let intents = b.step(1, &m);
        assert_eq!(
            intents,
            vec![
                Intent::Limit {
                    leg: 0,
                    side: Side::Buy,
                    qty: 10.0,
                    limit: limit_price(10.0, true, 5.0)
                },
                Intent::Limit {
                    leg: 1,
                    side: Side::Sell,
                    qty: 5.0,
                    limit: limit_price(20.0, false, 5.0)
                },
            ]
        );
        b.set_order(0, 11);
        b.set_order(1, 12);
        assert!(b.step(2, &m).is_empty(), "both rest: nothing to do");
        // Leg 0 fills whole at its limit, as a maker.
        assert_eq!(
            b.on_fill(11, &fill(Side::Buy, 10.0, 9.995, 11, 0.01)),
            Some(0)
        );
        assert!(b.legs[0].entered() && b.legs[0].order.is_none());
        assert_eq!(b.entered_legs(), 1);
        assert!(
            b.on_fill(99, &fill(Side::Buy, 1.0, 1.0, 99, 0.0)).is_none(),
            "not ours"
        );
        b.on_fill(12, &fill(Side::Sell, 5.0, 20.01, 12, 0.01));
        let intents = b.step(3, &m);
        assert!(
            matches!(b.state, State::Open { since_ms: 3 }),
            "{:?}",
            b.state
        );
        assert!(matches!(&intents[..], [Intent::Note(n)] if n.contains("open")));
        // PnL at the marks: A 10 @ 9.995 -> +0.05; B short 5 @ 20.01 -> +0.05; fees 0.02.
        let p = b.pnl(&m);
        assert!((p.unrealized - 0.1).abs() < 1e-9, "{p:?}");
        assert!((p.fees - 0.02).abs() < 1e-12);
        assert!((p.net() - 0.08).abs() < 1e-9);
        assert!((p.notional - 200.0).abs() < 1e-12);
    }

    #[test]
    fn after_the_wait_the_rest_goes_to_market() {
        let mut b = pair();
        let m = marks(&[("AUSDT", 10.0), ("BUSDT", 20.0)]);
        b.step(0, &m);
        b.set_order(0, 11);
        b.set_order(1, 12);
        b.on_fill(11, &fill(Side::Buy, 10.0, 9.995, 11, 0.0));
        // 89 s: still waiting.  91 s: cancel the unfilled leg, market it.
        assert!(b.step(89_000, &m).is_empty());
        let intents = b.step(91_000, &m);
        assert_eq!(
            intents,
            vec![
                Intent::Cancel { leg: 1, order: 12 },
                Intent::Market {
                    leg: 1,
                    side: Side::Sell,
                    qty: 5.0
                },
            ]
        );
        b.clear_order(1);
        b.on_fill_leg(1, &fill(Side::Sell, 5.0, 20.0, 0, 0.1));
        b.step(91_001, &m);
        assert!(matches!(b.state, State::Open { .. }));
        // A market entry rests nothing.
        let legs = vec![("AUSDT".to_string(), true, 10.0)];
        let mut mk = Basket::new(2, "m", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
        mk.size(&m).unwrap();
        mk.start(0);
        assert_eq!(
            mk.step(0, &m),
            vec![Intent::Market {
                leg: 0,
                side: Side::Buy,
                qty: 1.0
            }]
        );
    }

    fn open_pair() -> (Basket, HashMap<String, f64>) {
        let mut b = pair();
        let m = marks(&[("AUSDT", 10.0), ("BUSDT", 20.0)]);
        b.step(0, &m);
        b.set_order(0, 11);
        b.set_order(1, 12);
        b.on_fill(11, &fill(Side::Buy, 10.0, 10.0, 11, 0.0));
        b.on_fill(12, &fill(Side::Sell, 5.0, 20.0, 12, 0.0));
        b.step(1, &m);
        assert!(matches!(b.state, State::Open { .. }));
        (b, m)
    }

    #[test]
    fn the_exit_rule_leaves_with_limits_and_a_stop_at_market() {
        // Target: A up 3 % -> +3 on 200 = +1.5 %; not yet.  A up 5 % -> +2.5 %.
        let (mut b, _) = open_pair();
        let m = marks(&[("AUSDT", 10.3), ("BUSDT", 20.0)]);
        assert!(b.step(2, &m).is_empty());
        let m = marks(&[("AUSDT", 10.5), ("BUSDT", 20.0)]);
        let intents = b.step(3, &m);
        assert!(
            matches!(
                b.state,
                State::Exiting {
                    why: Why::Target,
                    ..
                }
            ),
            "{:?}",
            b.state
        );
        assert_eq!(
            intents,
            vec![
                Intent::Limit {
                    leg: 0,
                    side: Side::Sell,
                    qty: 10.0,
                    limit: limit_price(10.5, false, 5.0)
                },
                Intent::Limit {
                    leg: 1,
                    side: Side::Buy,
                    qty: 5.0,
                    limit: limit_price(20.0, true, 5.0)
                },
            ]
        );
        b.set_order(0, 21);
        b.set_order(1, 22);
        b.on_fill(21, &fill(Side::Sell, 10.0, 10.505, 21, 0.1));
        b.on_fill(22, &fill(Side::Buy, 5.0, 19.99, 22, 0.1));
        let intents = b.step(4, &m);
        assert!(matches!(
            b.state,
            State::Closed {
                why: Why::Target,
                ..
            }
        ));
        assert!(matches!(&intents[..], [Intent::Note(n)] if n.contains("target")));
        let p = b.pnl(&m);
        assert!((p.realized - (5.05 + 0.05)).abs() < 1e-9, "{p:?}");
        assert!((p.fees - 0.2).abs() < 1e-12);
        assert_eq!(p.unrealized, 0.0);
        assert!(b.step(5, &m).is_empty(), "closed: nothing more");

        // Stop: A down 3 % -> -1.5 %: market at once, no resting.
        let (mut b, _) = open_pair();
        let m = marks(&[("AUSDT", 9.7), ("BUSDT", 20.0)]);
        let intents = b.step(3, &m);
        assert!(matches!(b.state, State::Exiting { why: Why::Stop, .. }));
        assert!(
            intents.iter().all(|i| matches!(i, Intent::Market { .. })),
            "{intents:?}"
        );
        assert_eq!(intents.len(), 2);

        // Time: 240 minutes after it opened.
        let (mut b, m) = open_pair();
        assert!(b.step(1 + 239 * 60_000, &m).is_empty());
        b.step(1 + 240 * 60_000, &m);
        assert!(matches!(b.state, State::Exiting { why: Why::Time, .. }));

        // The user closes it; a second request changes nothing.
        let (mut b, m) = open_pair();
        let intents = b.request_exit(Why::User, 5, &m);
        assert_eq!(intents.len(), 2);
        assert!(b.request_exit(Why::User, 6, &m).is_empty());
        // Exit limits that do not fill go to market after the wait.
        b.set_order(0, 31);
        b.set_order(1, 32);
        let intents = b.step(5 + 91_000, &m);
        assert_eq!(intents.len(), 4, "{intents:?}");
        assert!(matches!(intents[0], Intent::Cancel { leg: 0, order: 31 }));
        assert!(matches!(intents[1], Intent::Market { leg: 0, .. }));
    }

    #[test]
    fn dropping_while_entering_cancels_and_closes_what_was_held() {
        let mut b = pair();
        let m = marks(&[("AUSDT", 10.0), ("BUSDT", 20.0)]);
        b.step(0, &m);
        b.set_order(0, 11);
        b.set_order(1, 12);
        b.on_fill(11, &fill(Side::Buy, 10.0, 10.0, 11, 0.0));
        let intents = b.request_exit(Why::Abandoned, 1, &m);
        // Cancel the resting leg, then a sell limit for the held one.
        assert_eq!(intents[0], Intent::Cancel { leg: 1, order: 12 });
        assert!(matches!(
            intents[1],
            Intent::Limit {
                leg: 0,
                side: Side::Sell,
                ..
            }
        ));
        assert_eq!(intents.len(), 2);
        b.set_order(0, 21);
        b.on_fill(21, &fill(Side::Sell, 10.0, 10.005, 21, 0.0));
        b.step(2, &m);
        assert!(matches!(
            b.state,
            State::Closed {
                why: Why::Abandoned,
                ..
            }
        ));
        // A blocked basket waits.
        let mut w = pair();
        w.blocked = Some("kill switch".into());
        assert!(w.step(0, &m).is_empty());
    }

    #[test]
    fn two_baskets_in_one_coin_keep_their_own_pnl() {
        let m = marks(&[("AUSDT", 10.0)]);
        let legs = vec![("AUSDT".to_string(), true, 100.0)];
        let mut b1 = Basket::new(1, "one", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
        let mut b2 = Basket::new(2, "two", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
        b1.size(&m).unwrap();
        b2.size(&m).unwrap();
        b1.on_fill_leg(0, &fill(Side::Buy, 10.0, 9.0, 0, 0.0));
        b2.on_fill_leg(0, &fill(Side::Buy, 10.0, 11.0, 0, 0.0));
        let m = marks(&[("AUSDT", 10.0)]);
        assert!((b1.pnl(&m).net() - 10.0).abs() < 1e-9);
        assert!((b2.pnl(&m).net() + 10.0).abs() < 1e-9);
        // Partial close realizes on the leg's own average.
        b1.on_fill_leg(0, &fill(Side::Sell, 4.0, 10.0, 0, 0.0));
        assert!((b1.legs[0].realized - 4.0).abs() < 1e-9);
        assert!((b1.legs[0].held - 6.0).abs() < 1e-9);
    }

    #[test]
    fn the_book_numbers_baskets_and_keeps_fifty_closed() {
        let mut book = Book::default();
        assert_eq!(book.next_id(), 1);
        assert_eq!(book.next_id(), 2);
        let legs = vec![("AUSDT".to_string(), true, 10.0)];
        for i in 0..60 {
            let mut b = Basket::new(i, "b", "o", &legs, Entry::Market, DEFAULT_EXIT, 0).unwrap();
            if i < 57 {
                b.state = State::Closed {
                    at_ms: i as i64,
                    why: Why::User,
                };
            }
            book.baskets.push(b);
        }
        assert_eq!(book.open_count(), 3);
        assert!((book.open_notional() - 30.0).abs() < 1e-12);
        book.retire();
        assert_eq!(book.baskets.len(), CLOSED_KEPT + 3);
        assert_eq!(book.baskets[0].id, 7, "the oldest closed went first");
        assert!(book.get(59).is_some() && book.get(3).is_none());
        let json = serde_json::to_string(&book).unwrap();
        let back: Book = serde_json::from_str(&json).unwrap();
        assert_eq!(back, book);
        let old: Book = serde_json::from_str(r#"{"baskets":[]}"#).unwrap();
        assert_eq!(old.next_id, 1);
    }
}
