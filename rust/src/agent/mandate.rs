//! Mandates: the only source of authority for the scalper role.
//!
//! A human issues a mandate on the TUI (list, size per order, orders per
//! hour, loss limit, duration).  Inside it the scalper trades on its own;
//! outside it the engine refuses.  **Paper only, always.**  Everything here
//! is pure: the terminal feeds it prices and positions and asks it to decide.
//!
//! Each mandate trades its own paper ledger (`Runtime::scalper`, started with
//! the mandate's balance): the scalper never sees or touches the human's
//! positions, and its PnL is simply that ledger's.  The global risk limits
//! still cover both ledgers together.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde_json::{json, Value};

use crate::engine::broker::Broker;
use crate::engine::PaperBroker;

pub const MAX_ORDER_USD: f64 = 10_000.0;
pub const MAX_ORDERS_PER_HOUR: u32 = 600;
pub const MAX_LOSS_USD: f64 = 10_000.0;
pub const MAX_MINUTES: u32 = 480;
pub const DEFAULT_BALANCE: f64 = 1_000.0;
pub const MAX_BALANCE: f64 = 1_000_000.0;
const HOUR_MS: i64 = 3_600_000;

#[derive(Clone, Debug, PartialEq)]
pub struct MandateSpec {
    pub list: String,
    pub max_order_usd: f64,
    pub max_orders_per_hour: u32,
    pub max_daily_loss: f64,
    pub minutes: u32,
    /// When the loss limit is reached, cancel the scalper's resting orders and
    /// close the positions it opened (default).  `keep` in the text turns it off.
    pub auto_close: bool,
    /// Starting cash of the scalper's own paper ledger (`bal=N`, default
    /// [`DEFAULT_BALANCE`]).  No leverage: it never holds more than its equity.
    pub balance: f64,
}

/// `"Scalp; 50 30 100 120"` or `"Scalp 50 30 100 120"`: list name, then USD
/// per order, orders per hour, loss limit in USD, minutes.  Trailing flags, in
/// any order: `keep` leaves positions open when the loss limit is hit
/// (default: they are closed), `bal=N` sets the scalper's balance.
pub fn parse_spec(text: &str) -> Result<MandateSpec, String> {
    const USAGE: &str = "usage: <list>; <USD per order> <orders per hour> <loss limit USD> \
                         <minutes> [bal=<USD>] [keep]";
    let mut text = text.trim();
    let mut auto_close = true;
    let mut balance = None;
    while let Some((head, last)) = text.rsplit_once(char::is_whitespace) {
        let lower = last.to_ascii_lowercase();
        if lower == "keep" && auto_close {
            auto_close = false;
        } else if let Some(v) = lower
            .strip_prefix("bal=")
            .or_else(|| lower.strip_prefix("balance="))
            .filter(|_| balance.is_none())
        {
            match v.parse::<f64>().ok().filter(|b| b.is_finite()) {
                Some(b) => balance = Some(b),
                None => return Err(format!("bad balance '{last}': {USAGE}")),
            }
        } else {
            break;
        }
        text = head.trim_end();
    }
    let (name, nums) = match text.split_once(';') {
        Some((n, r)) => (
            n.trim().to_string(),
            r.split_whitespace().collect::<Vec<_>>(),
        ),
        None => {
            let toks: Vec<&str> = text.split_whitespace().collect();
            if toks.len() < 5 {
                return Err(USAGE.into());
            }
            let cut = toks.len() - 4;
            (toks[..cut].join(" "), toks[cut..].to_vec())
        }
    };
    if name.is_empty() || nums.len() != 4 {
        return Err(USAGE.into());
    }
    let f = |i: usize| nums[i].parse::<f64>().ok().filter(|v| v.is_finite());
    let (Some(usd), Some(per_h), Some(loss), Some(min)) = (f(0), f(1), f(2), f(3)) else {
        return Err(USAGE.into());
    };
    if per_h.fract() != 0.0 || min.fract() != 0.0 {
        return Err("orders per hour and minutes must be whole numbers".into());
    }
    let spec = MandateSpec {
        list: name,
        max_order_usd: usd,
        max_orders_per_hour: per_h as u32,
        max_daily_loss: loss,
        minutes: min as u32,
        auto_close,
        balance: balance.unwrap_or(DEFAULT_BALANCE),
    };
    validate(&spec)?;
    Ok(spec)
}

/// Guard rails against typos (an extra zero is a 10x bigger mandate).
pub fn validate(s: &MandateSpec) -> Result<(), String> {
    if !(1.0..=MAX_ORDER_USD).contains(&s.max_order_usd) {
        return Err(format!("USD per order must be 1..{MAX_ORDER_USD}"));
    }
    if !(1..=MAX_ORDERS_PER_HOUR).contains(&s.max_orders_per_hour) {
        return Err(format!("orders per hour must be 1..{MAX_ORDERS_PER_HOUR}"));
    }
    if !(1.0..=MAX_LOSS_USD).contains(&s.max_daily_loss) {
        return Err(format!("loss limit must be 1..{MAX_LOSS_USD} USD"));
    }
    if !(1..=MAX_MINUTES).contains(&s.minutes) {
        return Err(format!("duration must be 1..{MAX_MINUTES} minutes"));
    }
    if !(10.0..=MAX_BALANCE).contains(&s.balance) {
        return Err(format!("balance must be 10..{MAX_BALANCE} USD"));
    }
    if s.max_order_usd > s.balance {
        return Err(format!(
            "USD per order must be within the balance ({}); raise it with bal=<USD>",
            s.balance
        ));
    }
    if s.max_daily_loss > s.balance {
        return Err(format!(
            "loss limit must be within the balance ({}); raise it with bal=<USD>",
            s.balance
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Active,
    /// Time ran out.
    Expired,
    /// Withdrawn by the human or the risk manager.
    Revoked(String),
    /// Loss limit reached: only reducing orders are accepted.
    Exhausted,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Self::Active => "active".into(),
            Self::Expired => "expired".into(),
            Self::Revoked(by) => format!("revoked by {by}"),
            Self::Exhausted => "exhausted (loss limit)".into(),
        }
    }
}

/// Fat-finger guard for limit orders: within 20% of the market.  A limit far
/// from the price is either a typo or a way to move size at a bad price.
pub fn limit_is_sane(limit: f64, market: f64) -> bool {
    limit.is_finite()
        && market.is_finite()
        && limit > 0.0
        && market > 0.0
        && (limit / market - 1.0).abs() <= 0.2
}

/// What the terminal knows about one order request.
pub struct OrderCheck {
    pub now_ms: i64,
    /// The symbol is a member of the mandate's list right now.
    pub in_list: bool,
    /// The scalper already traded this symbol under this mandate.
    pub touched: bool,
    /// The list's long/short tag allows this direction.
    pub side_ok: bool,
    pub notional: f64,
    /// Opens or grows a position (a flip counts as opening).
    pub opening: bool,
    pub live: bool,
    /// Gross notional the scalper's ledger would hold after this order
    /// (resting orders included).
    pub exposure_after: f64,
    /// The ledger's equity now.
    pub equity: f64,
}

#[derive(Clone, Debug)]
pub struct Mandate {
    pub id: u32,
    pub spec: MandateSpec,
    pub issued_ms: i64,
    pub expires_ms: i64,
    pub status: Status,
    order_times: VecDeque<i64>,
    pub touched: BTreeSet<String>,
    pub orders_total: u32,
    pub loss_now: f64,
}

/// Loss so far (>= 0) of a scalper ledger: what its equity is below its
/// starting balance (realized + unrealized).
pub fn ledger_loss(book: &PaperBroker, marks: &HashMap<String, f64>) -> f64 {
    (book.starting_cash - book.equity(marks)).max(0.0)
}

/// Gross notional a ledger holds plus what its resting orders would add.
pub fn ledger_exposure(book: &PaperBroker, marks: &HashMap<String, f64>) -> f64 {
    let held: f64 = book
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(s, p)| (p.qty * marks.get(s).copied().unwrap_or(p.avg)).abs())
        .sum();
    let resting: f64 = book
        .all_open_orders()
        .iter()
        .map(|o| (o.qty * o.limit).abs())
        .sum();
    // An empty f64 sum is -0.0; `+ 0.0` keeps "$-0.00" off the screen.
    held + resting + 0.0
}

/// What to trade to unwind a scalper ledger: `(symbol, signed qty)`, sorted.
pub fn close_plan(book: &PaperBroker) -> Vec<(String, f64)> {
    let mut plan: Vec<(String, f64)> = book
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(s, p)| (s.clone(), -p.qty))
        .collect();
    plan.sort_by(|a, b| a.0.cmp(&b.0));
    plan
}

/// The ledger as the agent and the archive see it.
pub fn ledger_json(book: &PaperBroker, marks: &HashMap<String, f64>) -> Value {
    let r2 = |x: f64| (x * 100.0).round() / 100.0 + 0.0;
    let eq = book.equity(marks);
    let mut positions: Vec<Value> = book
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat() || p.realized != 0.0)
        .map(|(s, p)| {
            let mark = marks.get(s).copied();
            json!({
                "symbol": s, "qty": p.qty, "avg": p.avg, "mark": mark,
                "unrealized": r2(mark.map_or(0.0, |m| p.unrealized(m))),
                "realized": r2(p.realized),
            })
        })
        .collect();
    positions.sort_by(|a, b| a["symbol"].as_str().cmp(&b["symbol"].as_str()));
    let orders: Vec<Value> = book
        .all_open_orders()
        .iter()
        .map(|o| json!({"id": o.id, "symbol": o.symbol, "side": format!("{:?}", o.side), "qty": o.qty, "limit": o.limit}))
        .collect();
    json!({
        "balance_start": r2(book.starting_cash),
        "equity": r2(eq),
        "pnl": r2(eq - book.starting_cash),
        "exposure_usd": r2(ledger_exposure(book, marks)),
        "positions": positions,
        "open_orders": orders,
    })
}

impl Mandate {
    pub fn new(id: u32, spec: MandateSpec, now_ms: i64) -> Self {
        let expires_ms = now_ms + spec.minutes as i64 * 60_000;
        Self {
            id,
            spec,
            issued_ms: now_ms,
            expires_ms,
            status: Status::Active,
            order_times: VecDeque::new(),
            touched: BTreeSet::new(),
            orders_total: 0,
            loss_now: 0.0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == Status::Active
    }

    pub fn revoke(&mut self, by: &str) -> bool {
        if matches!(self.status, Status::Revoked(_) | Status::Expired) {
            return false;
        }
        self.status = Status::Revoked(by.to_string());
        true
    }

    /// Advance the state: time and loss.  Returns the new status when it
    /// changed.
    pub fn update(&mut self, now_ms: i64, loss: f64) -> Option<Status> {
        self.loss_now = loss;
        if matches!(self.status, Status::Active | Status::Exhausted) && now_ms >= self.expires_ms {
            self.status = Status::Expired;
            return Some(self.status.clone());
        }
        if self.status == Status::Active && loss >= self.spec.max_daily_loss {
            self.status = Status::Exhausted;
            return Some(self.status.clone());
        }
        None
    }

    pub fn orders_last_hour(&mut self, now_ms: i64) -> usize {
        while self
            .order_times
            .front()
            .is_some_and(|t| now_ms - t >= HOUR_MS)
        {
            self.order_times.pop_front();
        }
        self.order_times.len()
    }

    /// May this order go through?  The reason is returned to the agent.
    pub fn check(&mut self, c: &OrderCheck) -> Result<(), String> {
        if c.live {
            return Err("mandates are paper-only: the terminal is in live mode".into());
        }
        match &self.status {
            Status::Active => {}
            Status::Exhausted if !c.opening => {} // reducing is still fine
            other => return Err(format!("mandate {}: no orders accepted", other.label())),
        }
        if c.now_ms >= self.expires_ms {
            return Err("mandate time is over".into());
        }
        if c.opening {
            if !c.in_list {
                return Err(format!(
                    "symbol is not in the mandate's list '{}'",
                    self.spec.list
                ));
            }
            if !c.side_ok {
                return Err("the list's long/short tag forbids this direction".into());
            }
            if c.notional > self.spec.max_order_usd + 1e-9 {
                return Err(format!(
                    "order is ${:.2}, the mandate allows ${:.2} per order",
                    c.notional, self.spec.max_order_usd
                ));
            }
            if c.exposure_after > c.equity + 1e-9 {
                return Err(format!(
                    "the scalper would hold ${:.2} on a ${:.2} balance (no leverage)",
                    c.exposure_after, c.equity
                ));
            }
            let used = self.orders_last_hour(c.now_ms);
            if used >= self.spec.max_orders_per_hour as usize {
                return Err(format!(
                    "rate limit: {used} orders in the last hour (max {})",
                    self.spec.max_orders_per_hour
                ));
            }
        } else if !c.in_list && !c.touched {
            return Err("can only reduce positions in the list or ones this mandate holds".into());
        }
        Ok(())
    }

    /// Remember an accepted order.
    pub fn record(&mut self, now_ms: i64, symbol: &str, opening: bool) {
        if opening {
            self.order_times.push_back(now_ms);
        }
        self.orders_total += 1;
        self.touched.insert(symbol.to_string());
    }

    pub fn to_json(&mut self, now_ms: i64) -> Value {
        let used = self.orders_last_hour(now_ms);
        json!({
            "id": self.id, "status": self.status.label(), "active": self.is_active(),
            "list": self.spec.list, "paper_only": true,
            "balance_start": self.spec.balance,
            "limits": {
                "max_order_usd": self.spec.max_order_usd,
                "max_orders_per_hour": self.spec.max_orders_per_hour,
                "max_loss_usd": self.spec.max_daily_loss,
            },
            "used": {
                "orders_last_hour": used,
                "orders_left_this_hour": (self.spec.max_orders_per_hour as usize).saturating_sub(used),
                "loss_usd": (self.loss_now * 100.0).round() / 100.0,
                "loss_left_usd": ((self.spec.max_daily_loss - self.loss_now).max(0.0) * 100.0).round() / 100.0,
                "orders_total": self.orders_total,
            },
            "minutes_left": ((self.expires_ms - now_ms).max(0)) / 60_000,
            "symbols_traded": self.touched,
            "auto_close_at_loss_limit": self.spec.auto_close,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::Action;

    fn spec() -> MandateSpec {
        MandateSpec {
            list: "Scalp".into(),
            max_order_usd: 50.0,
            max_orders_per_hour: 3,
            max_daily_loss: 20.0,
            minutes: 60,
            auto_close: true,
            balance: 1_000.0,
        }
    }

    fn open(now: i64) -> OrderCheck {
        OrderCheck {
            now_ms: now,
            in_list: true,
            touched: false,
            side_ok: true,
            notional: 40.0,
            opening: true,
            live: false,
            exposure_after: 40.0,
            equity: 1_000.0,
        }
    }

    #[test]
    fn spec_parsing_accepts_both_forms_and_rejects_typos() {
        let a = parse_spec("Scalp; 50 30 100 120").unwrap();
        assert_eq!(
            (
                a.list.as_str(),
                a.max_order_usd,
                a.max_orders_per_hour,
                a.max_daily_loss,
                a.minutes
            ),
            ("Scalp", 50.0, 30, 100.0, 120)
        );
        assert_eq!(
            parse_spec("My list 20 10 50 30").unwrap().list,
            "My list",
            "names may contain spaces"
        );
        assert_eq!(parse_spec("  Scalp ;50 30 100 120 ").unwrap().list, "Scalp");
        for bad in [
            "",
            "Scalp",
            "Scalp; 50 30 100",
            "Scalp; a b c d",
            "; 50 30 100 120",
            "Scalp; 50 1.5 100 120",
            "Scalp; 50 30 100 12.5",
        ] {
            assert!(parse_spec(bad).is_err(), "{bad:?}");
        }
        for bad in [
            "S; 0 30 100 120",
            "S; 99999 30 100 120",
            "S; 50 0 100 120",
            "S; 50 9999 100 120",
            "S; 50 30 0 120",
            "S; 50 30 99999 120",
            "S; 50 30 100 0",
            "S; 50 30 100 9999",
        ] {
            assert!(parse_spec(bad).unwrap_err().contains("must be"), "{bad:?}");
        }
    }

    #[test]
    fn an_order_inside_the_mandate_passes_and_each_limit_blocks_alone() {
        let mut m = Mandate::new(1, spec(), 0);
        assert!(m.check(&open(1_000)).is_ok());
        let mut c = open(1_000);
        c.live = true;
        assert!(m.check(&c).unwrap_err().contains("paper-only"));
        let mut c = open(1_000);
        c.in_list = false;
        assert!(m
            .check(&c)
            .unwrap_err()
            .contains("not in the mandate's list"));
        let mut c = open(1_000);
        c.side_ok = false;
        assert!(m.check(&c).unwrap_err().contains("tag"));
        let mut c = open(1_000);
        c.notional = 50.01;
        assert!(m.check(&c).unwrap_err().contains("per order"));
        let mut c = open(1_000);
        c.notional = 50.0;
        assert!(m.check(&c).is_ok(), "exactly the limit is fine");
        assert!(m
            .check(&open(60 * 60_000))
            .unwrap_err()
            .contains("time is over"));
    }

    #[test]
    fn the_hourly_rate_limit_slides() {
        let mut m = Mandate::new(1, spec(), 0);
        for i in 0..3 {
            assert!(m.check(&open(i * 1_000)).is_ok());
            m.record(i * 1_000, "AUSDT", true);
        }
        assert!(m.check(&open(10_000)).unwrap_err().contains("rate limit"));
        // Reducing orders are not rate limited and do not use the budget.
        let mut close = open(10_000);
        close.opening = false;
        assert!(m.check(&close).is_ok());
        m.record(10_000, "AUSDT", false);
        assert_eq!(m.orders_last_hour(10_000), 3);
        // After an hour the first orders fall out of the window.
        let mut m2 = m.clone();
        m2.expires_ms = i64::MAX;
        assert!(
            m2.check(&open(HOUR_MS + 500)).is_ok(),
            "first order is older than an hour"
        );
    }

    #[test]
    fn closing_needs_the_list_or_a_symbol_the_mandate_touched() {
        let mut m = Mandate::new(1, spec(), 0);
        let mut c = open(1_000);
        c.opening = false;
        c.in_list = false;
        assert!(m.check(&c).unwrap_err().contains("only reduce"));
        c.touched = true;
        assert!(
            m.check(&c).is_ok(),
            "a symbol that left the list can still be closed"
        );
        let mut c = open(1_000);
        c.opening = false;
        c.notional = 5_000.0;
        assert!(
            m.check(&c).is_ok(),
            "closing is exempt from the per-order cap"
        );
    }

    #[test]
    fn expiry_revoke_and_exhaustion_change_what_is_allowed() {
        let mut m = Mandate::new(1, spec(), 0);
        assert_eq!(m.update(1_000, 5.0), None);
        assert_eq!(m.update(1_000, 20.0), Some(Status::Exhausted));
        assert_eq!(m.update(2_000, 99.0), None, "reported once");
        assert!(
            m.check(&open(2_000)).unwrap_err().contains("exhausted"),
            "no new risk after the loss limit"
        );
        let mut close = open(2_000);
        close.opening = false;
        assert!(m.check(&close).is_ok(), "reducing is still allowed");

        let mut m = Mandate::new(2, spec(), 0);
        assert!(m.revoke("risk"));
        assert!(!m.revoke("human"), "already revoked: no second revoke");
        assert!(
            m.check(&close_of(1_000))
                .unwrap_err()
                .contains("revoked by risk"),
            "a revoked mandate accepts nothing"
        );

        let mut m = Mandate::new(3, spec(), 0);
        assert_eq!(m.update(60 * 60_000, 0.0), Some(Status::Expired));
        assert!(m.check(&close_of(1_000)).is_err());
        assert!(
            !m.revoke("human"),
            "an expired mandate cannot be revoked again"
        );
    }

    fn close_of(now: i64) -> OrderCheck {
        let mut c = open(now);
        c.opening = false;
        c
    }

    fn book_with(positions: &[(&str, f64, f64)]) -> (PaperBroker, HashMap<String, f64>) {
        // (symbol, qty bought at 100, mark now)
        let mut b = PaperBroker::new(1_000.0);
        b.set_fee_bps(0.0); // exact PnL below
        let mut marks = HashMap::new();
        for (s, q, m) in positions {
            let a = if *q > 0.0 {
                Action::BuyMarket {
                    symbol: s.to_string(),
                    qty: *q,
                }
            } else {
                Action::SellMarket {
                    symbol: s.to_string(),
                    qty: -q,
                }
            };
            b.submit(&a, 100.0, 1);
            marks.insert(s.to_string(), *m);
        }
        (b, marks)
    }

    #[test]
    fn loss_exposure_and_json_come_from_the_scalpers_own_ledger() {
        let (b, marks) = book_with(&[("AUSDT", 2.0, 95.0), ("BUSDT", -1.0, 90.0)]);
        // A: -10, B: +10 => flat PnL, no loss.
        assert_eq!(ledger_loss(&b, &marks), 0.0);
        assert!((ledger_exposure(&b, &marks) - (190.0 + 90.0)).abs() < 1e-9);
        let (b, marks) = book_with(&[("AUSDT", 2.0, 95.0)]);
        assert!((ledger_loss(&b, &marks) - 10.0).abs() < 1e-9);
        let j = ledger_json(&b, &marks);
        assert_eq!(j["balance_start"], 1_000.0);
        assert_eq!(j["pnl"], -10.0);
        assert_eq!(j["positions"][0]["symbol"], "AUSDT");
    }

    #[test]
    fn opening_beyond_the_balance_is_refused_closing_is_not() {
        let mut m = Mandate::new(1, spec(), 0);
        let mut c = open(1_000);
        c.exposure_after = 1_000.01;
        assert!(m.check(&c).unwrap_err().contains("no leverage"));
        c.opening = false;
        assert!(
            m.check(&c).is_ok(),
            "reducing is never blocked by the balance"
        );
    }

    #[test]
    fn the_balance_flag_is_parsed_and_checked() {
        assert_eq!(
            parse_spec("Scalp; 50 30 100 120").unwrap().balance,
            DEFAULT_BALANCE
        );
        let s = parse_spec("Scalp; 50 30 100 120 bal=2500 keep").unwrap();
        assert_eq!((s.balance, s.auto_close), (2_500.0, false));
        let s = parse_spec("My list 20 10 50 30 keep balance=300").unwrap();
        assert_eq!(
            (s.list.as_str(), s.balance, s.auto_close),
            ("My list", 300.0, false)
        );
        assert!(parse_spec("Scalp; 50 30 100 120 bal=x").is_err());
        assert!(parse_spec("Scalp; 50 30 100 120 bal=5")
            .unwrap_err()
            .contains("balance"));
        assert!(parse_spec("Scalp; 2000 30 100 120")
            .unwrap_err()
            .contains("bal="));
        assert!(parse_spec("Scalp; 50 30 2000 120 bal=1500")
            .unwrap_err()
            .contains("loss limit"));
        assert!(parse_spec("Scalp; 2000 30 100 120 bal=5000").is_ok());
    }

    #[test]
    fn the_keep_suffix_turns_auto_close_off() {
        assert!(parse_spec("Scalp; 50 30 100 120").unwrap().auto_close);
        assert!(!parse_spec("Scalp; 50 30 100 120 keep").unwrap().auto_close);
        assert!(!parse_spec("Scalp; 50 30 100 120 KEEP").unwrap().auto_close);
        let s = parse_spec("My list 20 10 50 30 keep").unwrap();
        assert!(
            !s.auto_close && s.list == "My list",
            "keep is not part of the name"
        );
        assert!(
            parse_spec("Scalp; 50 30 100 120 stay").is_err(),
            "only 'keep' is a flag"
        );
    }

    #[test]
    fn the_close_plan_unwinds_every_position_of_the_ledger() {
        let (b, _) = book_with(&[("BUSDT", -3.0, 100.0), ("AUSDT", 2.0, 100.0)]);
        assert_eq!(
            close_plan(&b),
            vec![("AUSDT".to_string(), -2.0), ("BUSDT".to_string(), 3.0)]
        );
        assert!(close_plan(&PaperBroker::new(1_000.0)).is_empty());
    }

    #[test]
    fn limit_prices_must_be_near_the_market() {
        assert!(
            limit_is_sane(100.0, 100.0)
                && limit_is_sane(119.9, 100.0)
                && limit_is_sane(80.1, 100.0)
        );
        for (l, m) in [
            (121.0, 100.0),
            (79.0, 100.0),
            (1e-9, 100.0),
            (1e9, 100.0),
            (0.0, 100.0),
            (-5.0, 100.0),
            (100.0, 0.0),
            (f64::NAN, 100.0),
            (100.0, f64::INFINITY),
        ] {
            assert!(!limit_is_sane(l, m), "{l} vs {m}");
        }
    }

    #[test]
    fn json_reports_usage_and_what_is_left() {
        let mut m = Mandate::new(7, spec(), 1_000);
        m.record(2_000, "AUSDT", true);
        m.update(3_000, 4.5);
        let j = m.to_json(3_000);
        assert_eq!(j["id"], 7);
        assert_eq!(j["paper_only"], true);
        assert_eq!(j["used"]["orders_last_hour"], 1);
        assert_eq!(j["used"]["orders_left_this_hour"], 2);
        assert_eq!(j["used"]["loss_left_usd"], 15.5);
        assert_eq!(j["minutes_left"], 59);
        assert_eq!(j["balance_start"], 1_000.0);
    }
}
