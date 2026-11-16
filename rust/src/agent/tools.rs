//! State tools for agents: read the terminal's lists, screener, positions,
//! equity and fills, and (researcher only) edit watchlists.
//!
//! These work on plain borrowed state, so they are testable without the TUI.
//! Replies are compact and size-capped: an agent pays for every token.

use serde_json::{json, Value};

use crate::data::binance::{TickerSummary, Venue};
use crate::engine::Runtime;
use crate::watchlists::{MoveRule, PriceTracker, Side, Watchlist, Watchlists};
use crate::widgets::screener::{rank_live, SortMode};

pub const MAX_LIST_SYMBOLS: usize = 500;
pub const MAX_SCREENER_ROWS: usize = 200;
pub const MAX_FILLS: usize = 100;
pub const MAX_HISTORY: usize = 300;

pub struct State<'a> {
    pub runtime: &'a Runtime,
    pub lists: &'a Watchlists,
    pub pairs: &'a [TickerSummary],
    pub tracker: &'a PriceTracker,
    pub now_ms: i64,
    pub mode: &'static str,
    pub venue: Venue,
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

/// A string argument, trimmed and non-empty.  Never echo it into the UI as is:
/// use [`arg_clean`] / [`arg_text`] for text that is stored or displayed.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Like [`arg_str`] but safe to store and show: unsafe chars removed, capped.
pub fn arg_clean(args: &Value, key: &str) -> Option<String> {
    arg_str(args, key)
        .map(|s| super::clean_text(s, false).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Multi-line text argument (note bodies): newlines and tabs survive.
pub fn arg_text(args: &Value, key: &str) -> Option<String> {
    arg_str(args, key)
        .map(|s| super::clean_text(s, true).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A Binance symbol argument: 1-20 ASCII letters/digits, upper-cased.  The
/// value ends up inside a request URL, so nothing else may pass.
pub fn valid_symbol(raw: &str) -> Result<String, String> {
    let s = raw.trim().to_ascii_uppercase();
    if (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(s)
    } else {
        Err("symbol must be 1-20 letters or digits (e.g. BTCUSDT)".into())
    }
}

/// Most symbols / tags accepted in one array argument.
pub const MAX_ARRAY_ARG: usize = 500;

pub fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

pub fn arg_usize(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default)
        .clamp(1, max)
}

/// An array of symbols; invalid entries are dropped, at most [`MAX_ARRAY_ARG`].
pub fn arg_symbols(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| valid_symbol(s).ok())
                .take(MAX_ARRAY_ARG)
                .collect()
        })
        .unwrap_or_default()
}

/// Up to 10 tags: cleaned, lower-cased, 1-32 chars each.
pub fn arg_tags(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| {
                    super::clean_text(s, false)
                        .trim()
                        .to_ascii_lowercase()
                        .chars()
                        .take(32)
                        .collect::<String>()
                })
                .filter(|s| !s.is_empty())
                .take(10)
                .collect()
        })
        .unwrap_or_default()
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::Any => "any",
        Side::Long => "long",
        Side::Short => "short",
    }
}

pub fn parse_side(s: &str) -> Option<Side> {
    match s.trim().to_ascii_lowercase().as_str() {
        "any" | "" => Some(Side::Any),
        "long" => Some(Side::Long),
        "short" => Some(Side::Short),
        _ => None,
    }
}

pub fn find_list(lists: &Watchlists, name: &str) -> Option<usize> {
    lists
        .lists()
        .iter()
        .position(|l| l.name.eq_ignore_ascii_case(name.trim()))
}

fn list_json(l: &Watchlist, with_symbols: bool) -> Value {
    let mut v = json!({
        "name": l.name,
        "kind": if l.rule().is_some() { "dynamic" } else { "manual" },
        "rule": l.rule().map(|r| r.label()),
        "side": side_name(l.side),
        "count": l.symbols.len(),
    });
    if with_symbols {
        v["symbols"] = json!(l.symbols.iter().take(MAX_LIST_SYMBOLS).collect::<Vec<_>>());
        v["truncated"] = json!(l.symbols.len() > MAX_LIST_SYMBOLS);
    }
    v
}

pub fn get_lists(st: &State, args: &Value) -> Result<Value, String> {
    let with = args
        .get("include_symbols")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "lists": st.lists.lists().iter().map(|l| list_json(l, with)).collect::<Vec<_>>(),
        "note": "dynamic lists are computed from live prices and are empty while the app warms up",
    }))
}

pub fn get_list(st: &State, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name").ok_or("argument 'name' is required")?;
    let idx = find_list(st.lists, name).ok_or_else(|| format!("no list named '{name}'"))?;
    let l = &st.lists.lists()[idx];
    let rows: Vec<Value> = l
        .symbols
        .iter()
        .take(MAX_LIST_SYMBOLS)
        .map(|sym| {
            let p = st.pairs.iter().find(|p| p.symbol == *sym);
            let mv = l
                .rule()
                .and_then(|r| st.tracker.change_pct(sym, r.window_min, st.now_ms));
            json!({
                "symbol": sym,
                "last": p.map(|p| p.last_price),
                "change_24h_pct": p.map(|p| round(p.change_pct, 2)),
                "quote_volume_24h": p.map(|p| p.volume_quote.round()),
                "move_pct": mv.map(|m| round(m, 2)),
            })
        })
        .collect();
    let mut out = list_json(l, false);
    out["rows"] = json!(rows);
    Ok(out)
}

pub fn get_screener(st: &State, args: &Value) -> Result<Value, String> {
    let sort = match arg_str(args, "sort")
        .unwrap_or("change")
        .to_ascii_lowercase()
        .as_str()
    {
        "range" | "gap" => SortMode::Gap,
        "change" => SortMode::Change,
        "volume" => SortMode::Volume,
        "price" | "last" => SortMode::Last,
        other => {
            return Err(format!(
                "unknown sort '{other}' (range, change, volume, price)"
            ))
        }
    };
    let asc = args
        .get("ascending")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let min_qv = arg_f64(args, "min_quote_volume").unwrap_or(0.0).max(0.0);
    let limit = arg_usize(args, "limit", 30, MAX_SCREENER_ROWS);
    let ranked = rank_live(st.pairs, limit, sort, min_qv, asc);
    let rows: Vec<Value> = ranked
        .iter()
        .filter_map(|(i, _)| st.pairs.get(*i))
        .map(|p| {
            let lo = p.low_24h.max(1e-12);
            json!({
                "symbol": p.symbol,
                "last": p.last_price,
                "change_24h_pct": round(p.change_pct, 2),
                "range_24h_pct": round((p.high_24h - p.low_24h) / lo * 100.0, 2),
                "quote_volume_24h": p.volume_quote.round(),
            })
        })
        .collect();
    Ok(json!({
        "venue": match st.venue { Venue::Spot => "spot", Venue::Futures => "futures" },
        "pairs_known": st.pairs.len(),
        "returned": rows.len(),
        "rows": rows,
    }))
}

pub fn get_positions(st: &State) -> Result<Value, String> {
    let mut rows: Vec<Value> = st
        .runtime
        .broker
        .all_positions()
        .iter()
        .filter(|(_, p)| !p.is_flat())
        .map(|(sym, p)| {
            let mark = st.runtime.last_mark(sym);
            json!({
                "symbol": sym,
                "side": if p.qty > 0.0 { "long" } else { "short" },
                "qty": p.qty,
                "avg": p.avg,
                "mark": mark,
                "unrealized": mark.map(|m| round(p.unrealized(m), 4)),
                "realized": round(p.realized, 4),
            })
        })
        .collect();
    rows.sort_by(|a, b| a["symbol"].as_str().cmp(&b["symbol"].as_str()));
    let orders: Vec<Value> = st
        .runtime
        .broker
        .all_open_orders()
        .iter()
        .map(|o| json!({"id": o.id, "symbol": o.symbol, "side": format!("{:?}", o.side), "qty": o.qty, "limit": o.limit}))
        .collect();
    // The scalper's own paper ledger (a mandate's), shown apart.
    let scalper = st
        .runtime
        .scalper
        .as_ref()
        .map(|b| crate::agent::mandate::ledger_json(b, &st.runtime.last_marks));
    Ok(
        json!({"mode": st.mode, "positions": rows, "open_orders": orders, "scalper_ledger": scalper}),
    )
}

pub fn get_equity(st: &State, args: &Value) -> Result<Value, String> {
    let n = arg_usize(args, "history", 60, MAX_HISTORY);
    let rt = st.runtime;
    let hist: Vec<f64> = rt
        .equity_history
        .iter()
        .rev()
        .take(n)
        .rev()
        .map(|v| round(*v, 2))
        .collect();
    Ok(json!({
        "mode": st.mode,
        "equity": round(rt.equity(), 2),
        "day_start": round(rt.risk_state.day_start_equity, 2),
        "day_pnl": round(rt.day_pnl(), 2),
        "day_pnl_note": "day_pnl and the loss/drawdown limits count the scalper's ledger too",
        "scalper_pnl": round(rt.scalper_pnl(), 2),
        "unrealized": round(rt.unrealized_total(), 2),
        "realized": round(rt.realized_total(), 2),
        "kill_switch": rt.limits.kill_switch,
        "limits": {
            "max_abs_position": rt.limits.max_abs_position,
            "max_open_orders": rt.limits.max_open_orders,
            "max_daily_loss": rt.limits.max_daily_loss,
            "max_drawdown_pct": rt.limits.max_drawdown_pct,
        },
        "last_violation": rt.risk_state.last_violation,
        "history_5s_samples": hist,
    }))
}

/// The baskets of the paper ledger, open ones first.
pub fn get_baskets(st: &State) -> Result<Value, String> {
    let rt = &st.runtime;
    let (_, _, book) = rt.paper_ledger();
    let mut rows: Vec<&crate::basket::Basket> = book.baskets.iter().collect();
    rows.sort_by_key(|b| (b.state.is_closed(), std::cmp::Reverse(b.id)));
    let baskets: Vec<Value> = rows
        .iter()
        .map(|b| {
            let p = b.pnl(&rt.last_marks);
            let (state, why) = match b.state {
                crate::basket::State::Closed { why, .. } => ("closed", Some(why.label())),
                crate::basket::State::Exiting { why, .. } => ("exiting", Some(why.label())),
                crate::basket::State::Open { .. } => ("open", None),
                crate::basket::State::Entering { .. } => ("entering", None),
            };
            json!({
                "id": b.id, "name": b.name, "origin": b.origin, "state": state, "why": why,
                "blocked": b.blocked, "created_ms": b.created_ms,
                "usd": round(b.total_usd(), 2),
                "net_pnl": round(p.net(), 4), "net_pct": round(p.pct(), 3),
                "unrealized": round(p.unrealized, 4), "realized": round(p.realized, 4),
                "fees": round(p.fees, 4),
                "exit": {"target_pct": b.exit.target_pct, "stop_pct": b.exit.stop_pct, "max_minutes": b.exit.max_minutes},
                "legs": b.legs.iter().map(|l| json!({
                    "symbol": l.symbol, "side": if l.long { "long" } else { "short" },
                    "usd": round(l.usd, 2), "target_qty": l.target_qty, "held": l.held,
                    "avg": l.avg, "realized": round(l.realized, 4), "fees": round(l.fees, 4),
                    "resting_order": l.order,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({
        "open": book.open_count(), "open_usd": round(book.open_notional(), 2),
        "note": "paper only; exits are held by the trading window, not by an exchange",
        "baskets": baskets,
    }))
}

pub fn get_fills(st: &State, args: &Value) -> Result<Value, String> {
    let n = arg_usize(args, "limit", 20, MAX_FILLS);
    let rows: Vec<Value> = st
        .runtime
        .fill_log
        .iter()
        .rev()
        .take(n)
        .map(|(ts, sym, f)| {
            json!({
                "time_ms": ts, "symbol": sym, "side": format!("{:?}", f.side),
                "qty": f.qty, "price": f.price,
            })
        })
        .collect();
    Ok(json!({"newest_first": true, "fills": rows}))
}

// ─── Watchlist writes (researcher) ──────────────────────────────────────

/// Outcome of a write: the reply plus whether the lists changed (so the
/// caller persists them).
#[derive(Debug)]
pub struct Write {
    pub reply: Value,
    pub changed: bool,
}

/// Run `f` and put the active list back afterwards: an agent editing lists
/// must not move the user's selection under their hands.
fn keep_active<T>(lists: &mut Watchlists, f: impl FnOnce(&mut Watchlists) -> T) -> T {
    let prev = lists.active_index();
    let out = f(lists);
    lists.select_list(prev);
    out
}

pub fn create_list(lists: &mut Watchlists, args: &Value) -> Result<Write, String> {
    let name = arg_clean(args, "name").ok_or("argument 'name' is required")?;
    let name = name.as_str();
    let side = match arg_str(args, "side") {
        Some(s) => parse_side(s).ok_or("side must be any, long or short")?,
        None => Side::Any,
    };
    let symbols = arg_symbols(args, "symbols");
    let (created, idx, added) = keep_active(lists, |l| {
        if !l.add_list(name) {
            return (false, 0, 0);
        }
        let idx = l.active_index();
        l.set_side_at(idx, side);
        let added = l
            .add_many_to(idx, symbols.iter().map(String::as_str))
            .unwrap_or(0);
        (true, idx, added)
    });
    if !created {
        return Err(format!("cannot create '{name}': empty or already used"));
    }
    Ok(Write {
        reply: json!({"created": lists.lists()[idx].name, "symbols_added": added, "side": side_name(side)}),
        changed: true,
    })
}

pub fn add_to_list(lists: &mut Watchlists, args: &Value) -> Result<Write, String> {
    let name = arg_str(args, "list").ok_or("argument 'list' is required")?;
    let symbols = arg_symbols(args, "symbols");
    if symbols.is_empty() {
        return Err("argument 'symbols' must be a non-empty array of strings".into());
    }
    let idx = find_list(lists, name).ok_or_else(|| format!("no list named '{name}'"))?;
    let added = keep_active(lists, |l| {
        l.add_many_to(idx, symbols.iter().map(String::as_str))
    })
    .ok_or_else(|| format!("'{name}' is a dynamic group: it fills itself"))?;
    Ok(Write {
        reply: json!({"list": name, "added": added, "already_present": symbols.len() - added}),
        changed: added > 0,
    })
}

pub fn create_dynamic_group(lists: &mut Watchlists, args: &Value) -> Result<Write, String> {
    let text = arg_str(args, "rule").ok_or("argument 'rule' is required, e.g. \"15 10 up 5\"")?;
    let spec = MoveRule::parse_spec(text).ok_or(
        "rule must be '<minutes 1-30> <percent 0.1-100> [up|down|both] [hold minutes] \
         [vol=<min 24h USDT volume, e.g. 5M>] [name=<name>] [side=any|long|short]'",
    )?;
    let rule = spec.rule;
    let side = match arg_str(args, "side") {
        Some(s) => parse_side(s).ok_or("side must be any, long or short")?,
        None => spec.side.unwrap_or(Side::Any),
    };
    let created = keep_active(lists, |l| {
        if !l.add_dynamic_named(rule, spec.name.as_deref(), Some(side)) {
            return None;
        }
        let idx = l.active_index();
        Some(l.lists()[idx].name.clone())
    });
    let name = created.ok_or("a group with this rule already exists")?;
    Ok(Write {
        reply: json!({"created": name, "rule": rule.label(), "side": side_name(side)}),
        changed: true,
    })
}

pub fn set_side(lists: &mut Watchlists, args: &Value) -> Result<Write, String> {
    let name = arg_str(args, "list").ok_or("argument 'list' is required")?;
    let side = parse_side(arg_str(args, "side").ok_or("argument 'side' is required")?)
        .ok_or("side must be any, long or short")?;
    let idx = find_list(lists, name).ok_or_else(|| format!("no list named '{name}'"))?;
    lists.set_side_at(idx, side);
    Ok(Write {
        reply: json!({"list": name, "side": side_name(side)}),
        changed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::context::Side as OrderSide;

    fn pair(sym: &str, last: f64, chg: f64, qv: f64) -> TickerSummary {
        TickerSummary {
            symbol: sym.into(),
            last_price: last,
            change_pct: chg,
            high_24h: last * 1.05,
            low_24h: last * 0.95,
            volume_base: 1.0,
            volume_quote: qv,
        }
    }

    struct Fixture {
        runtime: Runtime,
        lists: Watchlists,
        pairs: Vec<TickerSummary>,
        tracker: PriceTracker,
    }

    fn fixture() -> Fixture {
        let mut lists = Watchlists::default();
        lists.add_symbol("BTCUSDT");
        lists.add_symbol("ETHUSDT");
        lists.add_symbol("MISSINGUSDT");
        Fixture {
            runtime: Runtime::new(10_000.0),
            lists,
            pairs: vec![
                pair("BTCUSDT", 60_000.0, 2.0, 9e9),
                pair("ETHUSDT", 3_000.0, -1.0, 4e9),
                pair("SOLUSDT", 150.0, 8.0, 1e9),
                pair("DOGEUSDT", 0.1, -6.0, 5e5),
            ],
            tracker: PriceTracker::default(),
        }
    }

    fn st(f: &Fixture) -> State<'_> {
        State {
            runtime: &f.runtime,
            lists: &f.lists,
            pairs: &f.pairs,
            tracker: &f.tracker,
            now_ms: 1_000_000,
            mode: "paper",
            venue: Venue::Spot,
        }
    }

    #[test]
    fn lists_show_kind_rule_side_and_count() {
        let f = fixture();
        let j = get_lists(&st(&f), &json!({})).unwrap();
        let ls = j["lists"].as_array().unwrap();
        assert_eq!(ls[0]["name"], "Main");
        assert_eq!(ls[0]["count"], 3);
        assert!(ls[0].get("symbols").is_none(), "symbols only on request");
        assert_eq!(ls[1]["kind"], "dynamic");
        assert!(ls[1]["rule"].as_str().unwrap().contains("15m"));
        let j = get_lists(&st(&f), &json!({"include_symbols": true})).unwrap();
        assert_eq!(j["lists"][0]["symbols"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn get_list_joins_live_prices_and_marks_unknown_symbols() {
        let f = fixture();
        let j = get_list(&st(&f), &json!({"name": "main"})).unwrap();
        let rows = j["rows"].as_array().unwrap();
        assert_eq!(rows[0]["last"], 60_000.0);
        assert_eq!(rows[2]["last"], Value::Null, "no price for an unknown pair");
        assert!(get_list(&st(&f), &json!({"name": "nope"})).is_err());
        assert!(get_list(&st(&f), &json!({})).is_err());
    }

    #[test]
    fn screener_sorts_filters_and_caps() {
        let f = fixture();
        let j = get_screener(&st(&f), &json!({"sort": "change", "limit": 2})).unwrap();
        let rows = j["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["symbol"], "SOLUSDT", "biggest 24h change first");
        let j = get_screener(&st(&f), &json!({"sort": "volume", "min_quote_volume": 2e9})).unwrap();
        assert_eq!(j["returned"], 2, "volume filter drops SOL and DOGE");
        assert!(get_screener(&st(&f), &json!({"sort": "vibes"})).is_err());
        let j = get_screener(&st(&f), &json!({"limit": 99_999})).unwrap();
        assert!(j["returned"].as_u64().unwrap() as usize <= MAX_SCREENER_ROWS);
    }

    #[test]
    fn positions_equity_and_fills_report_runtime_state() {
        let mut f = fixture();
        let j = get_positions(&st(&f)).unwrap();
        assert_eq!(j["positions"].as_array().unwrap().len(), 0);
        let e = get_equity(&st(&f), &json!({})).unwrap();
        assert_eq!(e["equity"], 10_000.0);
        assert_eq!(e["kill_switch"], false);
        assert_eq!(e["mode"], "paper");
        // A fill in the log shows up newest first.
        for i in 0..3 {
            f.runtime.fill_log.push_back((
                i,
                "BTCUSDT".into(),
                crate::strategies::context::Fill {
                    side: OrderSide::Buy,
                    qty: 1.0,
                    price: 100.0 + i as f64,
                    time_ms: i,
                    order_id: 0,
                    fee: 0.0,
                },
            ));
        }
        let j = get_fills(&st(&f), &json!({"limit": 2})).unwrap();
        let rows = j["fills"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["price"], 102.0);
    }

    #[test]
    fn writes_create_edit_and_keep_the_users_active_list() {
        let mut lists = Watchlists::default();
        lists.select_list(1); // the user is looking at the first dynamic group
        let before = lists.active_index();

        let w = create_list(
            &mut lists,
            &json!({"name": "Scalp", "symbols": ["btcusdt", "ethusdt", "btcusdt"], "side": "long"}),
        )
        .unwrap();
        assert!(w.changed);
        assert_eq!(w.reply["symbols_added"], 2, "normalised and de-duplicated");
        assert_eq!(lists.active_index(), before, "selection untouched");
        let idx = find_list(&lists, "scalp").unwrap();
        assert_eq!(lists.lists()[idx].side, Side::Long);

        let w = add_to_list(
            &mut lists,
            &json!({"list": "Scalp", "symbols": ["SOLUSDT", "ETHUSDT"]}),
        )
        .unwrap();
        assert_eq!(
            (
                w.reply["added"].as_u64(),
                w.reply["already_present"].as_u64()
            ),
            (Some(1), Some(1))
        );
        assert_eq!(lists.active_index(), before);

        let e =
            add_to_list(&mut lists, &json!({"list": "Pump 15m", "symbols": ["X"]})).unwrap_err();
        assert!(e.contains("dynamic"), "{e}");
        assert!(add_to_list(&mut lists, &json!({"list": "Scalp", "symbols": []})).is_err());
        assert!(
            create_list(&mut lists, &json!({"name": "scalp"})).is_err(),
            "duplicate name"
        );
        assert!(create_list(&mut lists, &json!({"name": "X", "side": "sideways"})).is_err());

        let w = create_dynamic_group(&mut lists, &json!({"rule": "5 3 down 2", "side": "long"}))
            .unwrap();
        assert_eq!(lists.active_index(), before);
        assert!(w.reply["created"].as_str().unwrap().starts_with("Move 5m"));
        assert!(
            create_dynamic_group(&mut lists, &json!({"rule": "5 3 down 2"})).is_err(),
            "same rule twice"
        );
        assert!(create_dynamic_group(&mut lists, &json!({"rule": "0 0"})).is_err());

        set_side(&mut lists, &json!({"list": "Scalp", "side": "short"})).unwrap();
        assert_eq!(
            lists.lists()[find_list(&lists, "Scalp").unwrap()].side,
            Side::Short
        );
        assert!(set_side(&mut lists, &json!({"list": "ghost", "side": "long"})).is_err());
    }

    #[test]
    fn argument_helpers_are_forgiving_but_bounded() {
        let a = json!({"n": 1_000_000, "s": "  x ", "e": "", "arr": [" a ", 3, "b"]});
        assert_eq!(arg_usize(&a, "n", 5, 50), 50);
        assert_eq!(arg_usize(&a, "missing", 5, 50), 5);
        assert_eq!(arg_str(&a, "s"), Some("x"));
        assert_eq!(arg_str(&a, "e"), None);
        assert_eq!(
            arg_symbols(&a, "arr"),
            vec!["A", "B"],
            "numbers dropped, symbols cleaned"
        );
        let hostile = json!({"arr": ["BTC&limit=99999", "ETH/../x", "ok1", "with space", "", "x".repeat(21), "bnb\u{1b}"]});
        assert_eq!(
            arg_symbols(&hostile, "arr"),
            vec!["OK1"],
            "only plain alphanumerics survive"
        );
        assert!(valid_symbol("BTCUSDT").is_ok() && valid_symbol("btc").unwrap() == "BTC");
        assert_eq!(
            valid_symbol(" btc\n").unwrap(),
            "BTC",
            "only surrounding whitespace is trimmed"
        );
        for bad in [
            "",
            "A B",
            "BTC-USDT",
            "B\nTC",
            "BTC%20",
            "../etc",
            &"A".repeat(21),
        ] {
            assert!(valid_symbol(bad).is_err(), "{bad:?}");
        }
        let many = json!({"s": (0..2000).map(|i| format!("S{i}")).collect::<Vec<_>>()});
        assert_eq!(
            arg_symbols(&many, "s").len(),
            MAX_ARRAY_ARG,
            "array arguments are capped"
        );
        let t = json!({"tags": ["Spread\u{1b}[2J", "  ", "A".repeat(100), "a","b","c","d","e","f","g","h","i","j"]});
        let tags = arg_tags(&t, "tags");
        assert_eq!(tags.len(), 10);
        assert_eq!(tags[0], "spread [2j", "escape defused, lower-cased");
        assert_eq!(tags[1].len(), 32, "length cap");
        assert_eq!(
            arg_clean(&json!({"n": "ab\u{1b}]0;x\u{7}c"}), "n").unwrap(),
            "ab ]0;x c"
        );
        assert_eq!(arg_text(&json!({"n": "l1\nl2"}), "n").unwrap(), "l1\nl2");
        assert_eq!(parse_side("LONG"), Some(Side::Long));
        assert_eq!(parse_side("x"), None);
    }

    /// Hostile argument values for every key, in every tool that takes JSON.
    /// A panic is an `abort` in release builds (takes the whole TUI down), so
    /// "returns an error" is the only acceptable way to refuse.
    #[test]
    fn no_tool_panics_on_hostile_arguments() {
        let mut nasty: Vec<Value> = vec![
            Value::Null,
            json!(true),
            json!(0),
            json!(-1),
            json!(1),
            json!(1e308),
            json!(-1e308),
            json!(f64::MIN_POSITIVE),
            json!(u64::MAX),
            json!(i64::MIN),
            json!(""),
            json!(" "),
            json!("x".repeat(100_000)),
            json!("\u{1b}]0;PWNED\u{7}"),
            json!("😀\u{0}"),
            json!([]),
            json!({}),
            json!([null]),
            json!([[[[]]]]),
            json!({"a": {"b": [1, "c"]}}),
            json!(["BTCUSDT", 1, null, "x".repeat(500)]),
            json!("1e999"),
            json!("NaN"),
        ];
        nasty.push(json!((0..3000)
            .map(|i| format!("S{i}"))
            .collect::<Vec<_>>()));
        let keys = [
            "name",
            "list",
            "symbols",
            "symbol",
            "side",
            "sort",
            "limit",
            "value",
            "history",
            "min_quote_volume",
            "ascending",
            "include_symbols",
            "rule",
            "tags",
            "title",
            "body",
            "since_id",
            "depth",
            "usd",
            "qty",
            "type",
            "limit_price",
            "order_id",
            "timeframe",
            "mode",
            "a",
            "b",
            "segments",
            "params",
            "fee_bps",
            "starting_cash",
            "strategy",
            "reason",
            "all",
            "id",
            "bar_secs",
            "top",
            "min_corr",
        ];
        let f = fixture();
        for v in &nasty {
            // The same hostile value under every key at once, and under each key alone.
            let mut all = serde_json::Map::new();
            for k in keys {
                all.insert(k.to_string(), v.clone());
            }
            let mut variants: Vec<Value> = vec![Value::Object(all), v.clone()];
            for k in keys {
                variants.push(json!({ k: v.clone() }));
            }
            for args in &variants {
                let st = st(&f);
                let _ = get_lists(&st, args);
                let _ = get_list(&st, args);
                let _ = get_screener(&st, args);
                let _ = get_positions(&st);
                let _ = get_equity(&st, args);
                let _ = get_fills(&st, args);
                let mut lists = Watchlists::default();
                let _ = create_list(&mut lists, args);
                let _ = add_to_list(&mut lists, args);
                let _ = create_dynamic_group(&mut lists, args);
                let _ = set_side(&mut lists, args);
                // Whatever got in must still be a well-formed, bounded model.
                assert!(lists.lists().len() <= crate::watchlists::MAX_LISTS);
                for l in lists.lists() {
                    assert!(l.symbols.len() <= crate::watchlists::MAX_SYMBOLS_PER_LIST);
                    assert!(l.name.chars().count() <= crate::watchlists::MAX_NAME_CHARS);
                    assert!(
                        !l.name.chars().any(crate::widgets::fmt::is_unsafe_char),
                        "{:?}",
                        l.name
                    );
                    assert!(
                        l.symbols.iter().all(|s| valid_symbol(s).is_ok()),
                        "{:?}",
                        l.symbols
                    );
                }
                let _ = crate::agent::testing::build_strategy("dca_timer", args);
                let _ = crate::agent::testing::build_strategy(
                    "sma_cross",
                    args.get("params").unwrap_or(args),
                );
                let _ = crate::agent::testing::config_from(
                    args.get("starting_cash").and_then(Value::as_f64),
                    args.get("fee_bps").and_then(Value::as_f64),
                );
                let _ = crate::agent::analytics::parse_timeframe(
                    args.get("timeframe").and_then(Value::as_str).unwrap_or(""),
                );
            }
        }
        // Numeric edge cases of the risk and mandate code.
        let mut limits = crate::engine::RiskLimits::default();
        for name in [
            "max_abs_position",
            "max_open_orders",
            "max_daily_loss",
            "max_drawdown_pct",
            "x",
            "",
        ] {
            for x in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                -1.0,
                0.0,
                1e308,
                1e-308,
                0.5,
                5.5,
            ] {
                let _ = crate::agent::risk::tighten_limit(&mut limits, name, x);
            }
        }
        for text in [
            "",
            ";",
            ";;;;",
            "a;1 1 1 1",
            "x 1e999 1 1 1",
            "x NaN 1 1 1",
            "x -1 -1 -1 -1",
            "é; 1 1 1 1",
            "\u{1b}; 50 30 100 120",
            &"a".repeat(50_000),
        ] {
            let _ = crate::agent::mandate::parse_spec(text);
        }
        for depth in [0usize, 1, 20, usize::MAX] {
            let book = crate::data::orderbook::OrderBookSnapshot {
                bids: vec![(1.0, 1.0)],
                asks: vec![(2.0, 1.0)],
            };
            let _ = crate::agent::analytics::orderbook_json("X", depth, &book, &[]);
        }
    }
}
