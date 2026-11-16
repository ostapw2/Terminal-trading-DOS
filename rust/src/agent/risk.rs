//! Risk-manager tools: an exposure report, **tighten-only** limit changes, a
//! kill switch that can only be switched ON, and flattening.
//!
//! The asymmetry is the point (`docs/AGENT-CONNECTOR.md`): this role can make
//! the terminal safer, never riskier.  Loosening a limit, switching the kill
//! switch off and opening positions stay with the human.

use serde_json::{json, Value};

use crate::engine::broker::Broker;
use crate::engine::{PaperBroker, RiskLimits, Runtime};
use crate::strategies::Action;
use crate::watchlists::{Side, Watchlists};

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

/// Lower one limit.  `value` must be strictly below the current one.
pub fn tighten_limit(limits: &mut RiskLimits, name: &str, value: f64) -> Result<Value, String> {
    if !value.is_finite() || value < 0.0 {
        return Err("value must be a non-negative number".into());
    }
    let old = match name {
        "max_abs_position" => limits.max_abs_position,
        "max_open_orders" => limits.max_open_orders as f64,
        "max_daily_loss" => limits.max_daily_loss,
        "max_drawdown_pct" => limits.max_drawdown_pct,
        "max_basket_usd" => limits.max_basket_usd,
        "max_baskets_usd" => limits.max_baskets_usd,
        other => {
            return Err(format!(
                "unknown limit '{other}' (max_abs_position, max_open_orders, max_daily_loss, max_drawdown_pct, max_basket_usd, max_baskets_usd)"
            ))
        }
    };
    if name == "max_drawdown_pct" && value > 100.0 {
        return Err("max_drawdown_pct is a percent: 0-100".into());
    }
    // Whole orders only: 5.7 would silently become 5 and read as "tighter".
    if name == "max_open_orders" && value.fract() != 0.0 {
        return Err("max_open_orders must be a whole number".into());
    }
    if value >= old {
        return Err(format!(
            "not tighter: {name} is {old}, requested {value}; the risk role can only lower limits"
        ));
    }
    match name {
        "max_abs_position" => limits.max_abs_position = value,
        "max_open_orders" => limits.max_open_orders = value as usize,
        "max_daily_loss" => limits.max_daily_loss = value,
        "max_basket_usd" => limits.max_basket_usd = value,
        "max_baskets_usd" => limits.max_baskets_usd = value,
        _ => limits.max_drawdown_pct = value,
    }
    Ok(json!({"limit": name, "old": old, "new": value}))
}

/// Switch the kill switch ON.  `false` when it already was.  There is no
/// "off" here by design.
pub fn engage_kill_switch(rt: &mut Runtime, reason: &str) -> bool {
    if rt.limits.kill_switch {
        return false;
    }
    rt.limits.kill_switch = true;
    rt.risk_state.last_violation = Some(format!("risk agent: {reason}"));
    true
}

/// Closing market orders for the open positions of the human's ledger.
/// `symbols = None` closes everything; `Some(list)` only those symbols
/// (unknown ones are ignored).
pub fn flatten_actions(rt: &Runtime, symbols: Option<&[String]>) -> Vec<Action> {
    closing_actions(&rt.broker, symbols)
}

/// Closing market orders for the open positions of one ledger (the human's
/// or the scalper's).
pub fn closing_actions(book: &PaperBroker, symbols: Option<&[String]>) -> Vec<Action> {
    let mut out: Vec<Action> = book
        .all_positions()
        .iter()
        .filter(|(sym, p)| {
            !p.is_flat() && symbols.is_none_or(|want| want.iter().any(|w| w == *sym))
        })
        .map(|(sym, p)| {
            if p.qty > 0.0 {
                Action::SellMarket {
                    symbol: sym.clone(),
                    qty: p.qty.abs(),
                }
            } else {
                Action::BuyMarket {
                    symbol: sym.clone(),
                    qty: p.qty.abs(),
                }
            }
        })
        .collect();
    out.sort_by(|a, b| a.symbol().cmp(&b.symbol()));
    out
}

pub fn risk_report_json(rt: &Runtime, lists: &Watchlists, mode: &str, now_ms: i64) -> Value {
    let eq = rt.equity();
    let l = &rt.limits;
    let mut rows: Vec<(String, f64, f64)> = rt
        .broker
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(s, p)| {
            let mark = rt.last_mark(s).unwrap_or(p.avg);
            (s.clone(), p.qty, mark)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let notional = |q: f64, m: f64| (q * m).abs();
    let long: f64 = rows
        .iter()
        .filter(|r| r.1 > 0.0)
        .map(|r| notional(r.1, r.2))
        .sum();
    let short: f64 = rows
        .iter()
        .filter(|r| r.1 < 0.0)
        .map(|r| notional(r.1, r.2))
        .sum();
    let gross = long + short;
    let positions: Vec<Value> = rows
        .iter()
        .map(|(s, q, m)| {
            json!({
                "symbol": s, "side": if *q > 0.0 { "long" } else { "short" },
                "qty": q, "notional": round(notional(*q, *m), 2),
                "share_of_gross_pct": round(if gross > 0.0 { notional(*q, *m) / gross * 100.0 } else { 0.0 }, 1),
            })
        })
        .collect();
    // The scalper's ledger: shown apart, but the limits cover both.
    let scalper = rt.scalper.as_ref().filter(|_| !rt.is_live());
    let qty_in = |b: &PaperBroker, s: &str| b.position(s).qty;
    let mut syms: Vec<&String> = rows.iter().map(|r| &r.0).collect();
    if let Some(b) = scalper {
        syms.extend(b.all_positions().keys());
    }
    let largest_pos = syms
        .iter()
        .map(|s| {
            let other = scalper.map_or(0.0, |b| qty_in(b, s));
            (rt.broker.position(s).qty + other).abs()
        })
        .fold(0.0, f64::max);
    let open_orders = rt.broker.open_order_count() + scalper.map_or(0, |b| b.open_order_count());
    let scalper_json = scalper.map(|b| {
        let mut j = crate::agent::mandate::ledger_json(b, &rt.last_marks);
        j["note"] =
            json!("the scalper's own paper ledger; risk limits count it together with this one");
        j
    });

    // Lists whose side tag the open positions contradict.
    let mut by_list = Vec::new();
    for list in lists.lists().iter().filter(|x| !x.symbols.is_empty()) {
        let held: Vec<&(String, f64, f64)> = rows
            .iter()
            .filter(|r| list.symbols.contains(&r.0))
            .collect();
        if held.is_empty() {
            continue;
        }
        let against: Vec<&str> = held
            .iter()
            .filter(|r| match list.side {
                Side::Long => r.1 < 0.0,
                Side::Short => r.1 > 0.0,
                Side::Any => false,
            })
            .map(|r| r.0.as_str())
            .collect();
        by_list.push(json!({
            "list": list.name,
            "side_tag": match list.side { Side::Any => "any", Side::Long => "long", Side::Short => "short" },
            "open_positions": held.len(),
            "gross_notional": round(held.iter().map(|r| notional(r.1, r.2)).sum(), 2),
            "against_side_tag": against,
        }));
    }

    // Drawdown and day loss are watched on both ledgers together.
    let risk_eq = rt.risk_equity();
    let dd = if rt.risk_state.equity_peak > 0.0 {
        (rt.risk_state.equity_peak - risk_eq) / rt.risk_state.equity_peak * 100.0
    } else {
        0.0
    };
    let day_loss = (-rt.day_pnl()).max(0.0);
    let util = |used: f64, limit: f64| {
        if limit > 0.0 {
            used / limit * 100.0
        } else {
            0.0
        }
    };
    let mut warnings: Vec<String> = Vec::new();
    if l.kill_switch {
        warnings.push(format!(
            "kill switch is ON ({})",
            rt.risk_state.last_violation.clone().unwrap_or_default()
        ));
    }
    if util(dd, l.max_drawdown_pct) >= 70.0 {
        warnings.push(format!(
            "drawdown {dd:.2}% is {:.0}% of its limit",
            util(dd, l.max_drawdown_pct)
        ));
    }
    if util(day_loss, l.max_daily_loss) >= 70.0 {
        warnings.push(format!(
            "daily loss {day_loss:.2} is {:.0}% of its limit",
            util(day_loss, l.max_daily_loss)
        ));
    }
    if util(largest_pos, l.max_abs_position) >= 80.0 {
        warnings.push("a position is above 80% of max_abs_position".into());
    }
    if gross > 0.0 && rows.iter().any(|r| notional(r.1, r.2) / gross > 0.5) && rows.len() > 1 {
        warnings.push("one position is over half of gross exposure".into());
    }
    for b in &by_list {
        if b["against_side_tag"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            warnings.push(format!(
                "positions against the side tag of list '{}'",
                b["list"].as_str().unwrap_or("")
            ));
        }
    }
    json!({
        "mode": mode, "time_ms": now_ms,
        "equity": round(eq, 2), "day_pnl": round(rt.day_pnl(), 2),
        "drawdown_pct": round(dd, 3), "equity_peak": round(rt.risk_state.equity_peak, 2),
        "exposure": {
            "gross": round(gross, 2), "net": round(long - short, 2),
            "long": round(long, 2), "short": round(short, 2),
            "gross_pct_of_equity": round(if eq > 0.0 { gross / eq * 100.0 } else { 0.0 }, 1),
            "open_positions": rows.len(),
        },
        "positions": positions,
        "by_list": by_list,
        "scalper_ledger": scalper_json,
        "limits": {
            "max_abs_position": l.max_abs_position, "max_open_orders": l.max_open_orders,
            "max_daily_loss": l.max_daily_loss, "max_drawdown_pct": l.max_drawdown_pct,
            "kill_switch": l.kill_switch,
        },
        "utilization_pct": {
            "largest_position": round(util(largest_pos, l.max_abs_position), 1),
            "open_orders": round(util(open_orders as f64, l.max_open_orders as f64), 1),
            "daily_loss": round(util(day_loss, l.max_daily_loss), 1),
            "drawdown": round(util(dd, l.max_drawdown_pct), 1),
        },
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::broker::{BrokerSnapshot, PaperBroker};
    use crate::strategies::context::Position;

    fn rt_with(positions: &[(&str, f64, f64, f64)]) -> Runtime {
        // (symbol, qty, avg, mark)
        let mut rt = Runtime::new(10_000.0);
        rt.broker = PaperBroker::restore(BrokerSnapshot {
            starting_cash: 10_000.0,
            positions: positions
                .iter()
                .map(|(s, q, avg, _)| {
                    (
                        s.to_string(),
                        Position {
                            qty: *q,
                            avg: *avg,
                            realized: 0.0,
                        },
                    )
                })
                .collect(),
            open_orders: Vec::new(),
            next_id: 1,
            brackets: Vec::new(),
            fee_maker_bps: crate::engine::broker::FEE_MAKER_BPS,
            fee_taker_bps: crate::engine::broker::FEE_TAKER_BPS,
            fees_paid: 0.0,
            leverage: Vec::new(),
            funding: 0.0,
            fut_maker_bps: crate::futures::FEE_MAKER_BPS,
            fut_taker_bps: crate::futures::FEE_TAKER_BPS,
        });
        for (s, _, _, mark) in positions {
            rt.on_price(s, *mark, 1);
        }
        rt
    }

    #[test]
    fn limits_can_only_be_lowered() {
        let mut l = RiskLimits {
            max_abs_position: 10.0,
            max_open_orders: 5,
            max_daily_loss: 100.0,
            max_drawdown_pct: 20.0,
            kill_switch: false,
            ..RiskLimits::default()
        };
        assert!(tighten_limit(&mut l, "max_abs_position", 5.0).is_ok());
        assert_eq!(l.max_abs_position, 5.0);
        for (name, v) in [
            ("max_abs_position", 5.0),
            ("max_abs_position", 50.0),
            ("max_open_orders", 5.0),
            ("max_daily_loss", 100.5),
            ("max_drawdown_pct", 20.0),
        ] {
            let e = tighten_limit(&mut l, name, v).unwrap_err();
            assert!(e.contains("not tighter"), "{name} {v}: {e}");
        }
        assert_eq!(l.max_open_orders, 5, "a refused change changes nothing");
        assert!(tighten_limit(&mut l, "max_open_orders", 2.0).is_ok() && l.max_open_orders == 2);
        assert!(tighten_limit(&mut l, "max_open_orders", 1.5)
            .unwrap_err()
            .contains("whole"));
        assert!(
            tighten_limit(&mut l, "max_daily_loss", 0.0).is_ok(),
            "zero blocks everything: allowed, it is tighter"
        );
        assert!(tighten_limit(&mut l, "max_drawdown_pct", f64::NAN).is_err());
        assert!(tighten_limit(&mut l, "max_drawdown_pct", -1.0).is_err());
        assert!(tighten_limit(&mut l, "bogus", 1.0)
            .unwrap_err()
            .contains("unknown limit"));
    }

    #[test]
    fn kill_switch_only_goes_on_and_records_why() {
        let mut rt = Runtime::new(1_000.0);
        assert!(engage_kill_switch(&mut rt, "spread blew out"));
        assert!(rt.limits.kill_switch);
        assert!(rt
            .risk_state
            .last_violation
            .as_deref()
            .unwrap()
            .contains("spread blew out"));
        assert!(
            !engage_kill_switch(&mut rt, "again"),
            "already on: no change"
        );
        assert!(
            rt.risk_state
                .last_violation
                .as_deref()
                .unwrap()
                .contains("spread"),
            "reason is kept"
        );
    }

    #[test]
    fn flatten_closes_each_position_the_right_way_and_respects_the_filter() {
        let rt = rt_with(&[
            ("LONGUSDT", 2.0, 100.0, 110.0),
            ("SHORTUSDT", -3.0, 50.0, 45.0),
            ("FLATUSDT", 0.0, 0.0, 10.0),
        ]);
        let all = flatten_actions(&rt, None);
        assert_eq!(all.len(), 2, "flat positions are skipped");
        assert!(
            matches!(&all[0], Action::SellMarket { symbol, qty } if symbol == "LONGUSDT" && *qty == 2.0)
        );
        assert!(
            matches!(&all[1], Action::BuyMarket { symbol, qty } if symbol == "SHORTUSDT" && *qty == 3.0)
        );
        let one = flatten_actions(
            &rt,
            Some(&["SHORTUSDT".to_string(), "GHOSTUSDT".to_string()]),
        );
        assert_eq!(one.len(), 1);
        assert!(flatten_actions(&rt, Some(&[])).is_empty());
    }

    #[test]
    fn report_shows_exposure_utilization_and_side_violations() {
        let rt = rt_with(&[("AUSDT", 10.0, 100.0, 100.0), ("BUSDT", -5.0, 100.0, 100.0)]);
        let mut lists = Watchlists::default();
        lists.add_list("Longs");
        lists.add_many_to(3, ["AUSDT", "BUSDT"].into_iter());
        lists.set_side_at(3, Side::Long);
        let j = risk_report_json(&rt, &lists, "paper", 1);
        assert_eq!(j["exposure"]["gross"], 1500.0);
        assert_eq!(j["exposure"]["net"], 500.0);
        assert_eq!(j["exposure"]["open_positions"], 2);
        assert_eq!(j["positions"][0]["share_of_gross_pct"], 66.7);
        let bl = j["by_list"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["list"] == "Longs")
            .unwrap();
        assert_eq!(
            bl["against_side_tag"],
            json!(["BUSDT"]),
            "a short inside a LONG list"
        );
        let w: Vec<&str> = j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(
            w.iter().any(|x| x.contains("against the side tag")),
            "{w:?}"
        );
        assert!(w.iter().any(|x| x.contains("over half")), "{w:?}");
    }

    #[test]
    fn report_flags_kill_switch_and_limit_pressure() {
        let mut rt = rt_with(&[("AUSDT", 90.0, 100.0, 100.0)]);
        rt.limits.max_abs_position = 100.0;
        engage_kill_switch(&mut rt, "test");
        let j = risk_report_json(&rt, &Watchlists::default(), "paper", 1);
        let w = j["warnings"].to_string();
        assert!(
            w.contains("kill switch is ON") && w.contains("80% of max_abs_position"),
            "{w}"
        );
        assert_eq!(j["utilization_pct"]["largest_position"], 90.0);
        assert_eq!(j["limits"]["kill_switch"], true);
    }

    #[test]
    fn report_counts_the_scalper_ledger_against_the_limits() {
        let mut rt = rt_with(&[("AUSDT", 1.0, 100.0, 100.0)]);
        rt.limits.max_abs_position = 4.0;
        rt.start_scalper_ledger(1_000.0).unwrap();
        rt.submit_scalper(
            Action::BuyMarket {
                symbol: "AUSDT".into(),
                qty: 2.0,
            },
            2,
        )
        .unwrap();
        let r = risk_report_json(&rt, &Watchlists::default(), "paper", 0);
        assert_eq!(r["utilization_pct"]["largest_position"], 75.0, "{r}");
        assert_eq!(r["scalper_ledger"]["positions"][0]["qty"], 2.0);
        assert_eq!(closing_actions(rt.scalper.as_ref().unwrap(), None).len(), 1);
    }
}
