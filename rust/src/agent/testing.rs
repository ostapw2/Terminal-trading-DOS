//! Strategy testing for the tester role: backtests, segment stability and
//! forward tests, all in **isolated** [`Runtime`]s — the main paper ledger is
//! never touched.
//!
//! Honesty about what the numbers mean lives in the replies (`caveats`) and in
//! the fill model documented on [`crate::engine::backtest`].

use serde_json::{json, Value};

use crate::engine::backtest::{count_round_trips, run, BacktestConfig, BacktestResult};
use crate::engine::bars::BarBuilder;
use crate::engine::broker::Broker;
use crate::engine::{Runtime, SlotStatus};
use crate::markets::{Ohlc, Timeframe};
use crate::strategies::{registry, Fill, ParamKind, ParamValue, Strategy};

pub const MAX_BARS: usize = 1000;
pub const MIN_BARS: usize = 50;
pub const MAX_SEGMENTS: usize = 8;
pub const MAX_FORWARD_TESTS: usize = 5;
const MAX_CURVE_POINTS: usize = 120;
const FORWARD_CURVE_CAP: usize = 600;
const FORWARD_FILL_CAP: usize = 2_000;

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

fn downsample(v: &[f64], n: usize) -> Vec<f64> {
    super::analytics::downsample(v, n)
        .into_iter()
        .map(|x| round(x, 2))
        .collect()
}

fn param_json(p: &crate::strategies::ParamSpec) -> Value {
    let mut v = json!({"key": p.key, "label": p.label, "help": p.help});
    match p.kind {
        ParamKind::Int {
            min, max, default, ..
        } => {
            v["type"] = json!("int");
            v["min"] = json!(min);
            v["max"] = json!(max);
            v["default"] = json!(default);
        }
        ParamKind::Float {
            min, max, default, ..
        } => {
            v["type"] = json!("float");
            v["min"] = json!(min);
            v["max"] = json!(max);
            v["default"] = json!(default);
        }
        ParamKind::Bool { default } => {
            v["type"] = json!("bool");
            v["default"] = json!(default);
        }
        ParamKind::Choice { options, default } => {
            v["type"] = json!("choice");
            v["options"] = json!(options);
            v["default"] = json!(default);
        }
        ParamKind::Duration { default_secs } => {
            v["type"] = json!("seconds");
            v["default"] = json!(default_secs);
        }
    }
    v
}

pub fn strategies_json() -> Value {
    let items: Vec<Value> = registry::catalog()
        .into_iter()
        .map(|m| {
            let params: Vec<Value> = registry::instantiate(m.id)
                .map(|s| s.schema().iter().map(param_json).collect())
                .unwrap_or_default();
            json!({
                "id": m.id, "name": m.name, "description": m.description,
                "timer_secs": m.timer_secs, "params": params,
            })
        })
        .collect();
    json!({
        "strategies": items,
        "note": "values outside a parameter's range are clamped, not rejected",
    })
}

/// A fresh strategy with `params` (a JSON object keyed by parameter name)
/// applied through the schema, so every value is range-checked.
pub fn build_strategy(id: &str, params: &Value) -> Result<Box<dyn Strategy>, String> {
    let mut s = registry::instantiate(id).ok_or_else(|| {
        format!(
            "unknown strategy '{id}' (known: {})",
            registry::catalog()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let Some(map) = params.as_object() else {
        return if params.is_null() {
            Ok(s)
        } else {
            Err("'params' must be an object".into())
        };
    };
    for (key, v) in map {
        let spec = s
            .schema()
            .iter()
            .find(|p| p.key == key)
            .copied()
            .ok_or_else(|| format!("strategy '{id}' has no parameter '{key}'"))?;
        let need = |what: &str| format!("{key}: need {what}");
        let value = match spec.kind {
            ParamKind::Int { .. } | ParamKind::Duration { .. } => ParamValue::Int(
                v.as_i64()
                    .or_else(|| v.as_f64().map(|f| f as i64))
                    .ok_or_else(|| need("a number"))?,
            ),
            ParamKind::Float { .. } => {
                ParamValue::Float(v.as_f64().ok_or_else(|| need("a number"))?)
            }
            ParamKind::Bool { .. } => {
                ParamValue::Bool(v.as_bool().ok_or_else(|| need("true or false"))?)
            }
            ParamKind::Choice { .. } => {
                ParamValue::Text(v.as_str().ok_or_else(|| need("a string"))?.to_string())
            }
        };
        s.set_param_checked(key, value)
            .map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(s)
}

pub fn config_from(
    starting_cash: Option<f64>,
    fee_bps: Option<f64>,
) -> Result<BacktestConfig, String> {
    let d = BacktestConfig::default();
    let cfg = BacktestConfig {
        starting_cash: starting_cash.unwrap_or(d.starting_cash),
        fee_bps: fee_bps.unwrap_or(d.fee_bps),
        funding_rate: d.funding_rate,
    };
    if !(cfg.starting_cash.is_finite() && (100.0..=1.0e9).contains(&cfg.starting_cash)) {
        return Err("starting_cash must be between 100 and 1e9".into());
    }
    if !(cfg.fee_bps.is_finite() && (0.0..=500.0).contains(&cfg.fee_bps)) {
        return Err("fee_bps must be between 0 and 500".into());
    }
    Ok(cfg)
}

/// Buy-and-hold return over the same candles: the benchmark a strategy has
/// to beat before its backtest means anything.
fn buy_hold_pct(bars: &[Ohlc]) -> Option<f64> {
    let first = bars.iter().find(|b| !b.is_gap && b.open > 0.0)?;
    let last = bars.iter().rev().find(|b| !b.is_gap && b.close > 0.0)?;
    Some((last.close / first.open - 1.0) * 100.0)
}

const CAVEATS: &str = "fills use the candle's close with no slippage (optimistic); \
     the intra-candle path is assumed; one backtest is one sample, not proof";

pub fn backtest_json(
    r: &BacktestResult,
    tf: Timeframe,
    bars: &[Ohlc],
    cfg: &BacktestConfig,
) -> Value {
    json!({
        "strategy": r.strategy_id, "symbol": r.symbol, "timeframe": tf.name(),
        "candles": r.bars, "first_ms": r.first_ms, "last_ms": r.last_ms,
        "starting_cash": r.starting_cash, "final_equity": round(r.final_equity, 2),
        "net_pnl": round(r.net_pnl, 2), "return_pct": round(r.return_pct, 3),
        "buy_and_hold_pct": buy_hold_pct(bars).map(|x| round(x, 3)),
        "fees": round(r.fees, 2), "fee_bps": cfg.fee_bps,
        "max_drawdown_pct": round(r.max_drawdown_pct, 3),
        "round_trips": r.trades, "win_rate_pct": round(r.win_rate_pct(), 1),
        "fills": r.fills.len(), "open_qty": r.open_qty,
        "halted_by_risk_gate": r.halted,
        "equity_curve": downsample(&r.equity_curve, MAX_CURVE_POINTS),
        "caveats": CAVEATS,
    })
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Split `bars` into `segments` contiguous windows and backtest each on a
/// fresh runtime: does the strategy work across time or in one lucky stretch?
/// This does **not** fit parameters (no in-sample / out-of-sample split).
pub fn walk_forward_json(
    strategy_id: &str,
    params: &Value,
    symbol: &str,
    tf: Timeframe,
    bars: &[Ohlc],
    segments: usize,
    cfg: &BacktestConfig,
) -> Result<Value, String> {
    let segments = segments.clamp(2, MAX_SEGMENTS);
    let usable: Vec<&Ohlc> = bars.iter().filter(|b| !b.is_gap).collect();
    if usable.len() < segments * MIN_BARS {
        return Err(format!(
            "{} candles is too few for {segments} segments (need at least {})",
            usable.len(),
            segments * MIN_BARS
        ));
    }
    let size = usable.len() / segments;
    let mut rows = Vec::new();
    let mut returns = Vec::new();
    let mut bench = Vec::new();
    for i in 0..segments {
        let from = i * size;
        let to = if i + 1 == segments {
            usable.len()
        } else {
            from + size
        };
        let slice: Vec<Ohlc> = usable[from..to].iter().map(|b| (*b).clone()).collect();
        let strategy = build_strategy(strategy_id, params)?;
        let r = run(strategy, symbol, &slice, cfg)?;
        let bh = buy_hold_pct(&slice);
        returns.push(r.return_pct);
        if let Some(b) = bh {
            bench.push(b);
        }
        rows.push(json!({
            "segment": i + 1, "candles": r.bars, "first_ms": r.first_ms, "last_ms": r.last_ms,
            "return_pct": round(r.return_pct, 3), "buy_and_hold_pct": bh.map(|x| round(x, 3)),
            "max_drawdown_pct": round(r.max_drawdown_pct, 3), "round_trips": r.trades,
            "halted_by_risk_gate": r.halted,
        }));
    }
    let m = mean(&returns);
    let sd = (returns.iter().map(|x| (x - m).powi(2)).sum::<f64>() / returns.len() as f64).sqrt();
    let positive = returns.iter().filter(|x| **x > 0.0).count();
    let beat = returns.iter().zip(&bench).filter(|(r, b)| r > b).count();
    Ok(json!({
        "strategy": strategy_id, "symbol": symbol, "timeframe": tf.name(),
        "segments": rows,
        "summary": {
            "mean_return_pct": round(m, 3), "std_return_pct": round(sd, 3),
            "worst_segment_pct": round(returns.iter().cloned().fold(f64::INFINITY, f64::min), 3),
            "best_segment_pct": round(returns.iter().cloned().fold(f64::NEG_INFINITY, f64::max), 3),
            "positive_segments": positive, "beat_buy_and_hold": beat, "of": segments,
        },
        "caveats": format!("{CAVEATS}; segments are independent windows with no parameter fitting"),
    }))
}

// ─── Parameter sweep with a held-out window ─────────────────────────────

/// Most parameter combinations one sweep may try.
pub const MAX_COMBOS: usize = 64;
/// Most values per swept parameter.
pub const MAX_GRID_VALUES: usize = 10;
/// Parameters that size positions: never swept (bigger size is not "better
/// parameters"), set from USD for comparisons.
pub const SIZING_KEYS: &[&str] = &["qty", "qty_per_level"];
/// How many parameters an automatic grid varies (the first numeric ones).
const AUTO_GRID_PARAMS: usize = 2;
/// Multipliers of the current value an automatic grid tries.
const AUTO_GRID_STEPS: [f64; 5] = [0.5, 0.75, 1.0, 1.5, 2.0];

/// The values to try, per parameter, in the order given.
pub type Grid = Vec<(String, Vec<Value>)>;

fn value_json(v: &ParamValue) -> Value {
    match v {
        ParamValue::Int(i) => json!(i),
        ParamValue::Float(f) => json!(f),
        ParamValue::Bool(b) => json!(b),
        ParamValue::Text(t) => json!(t),
    }
}

/// A grid around the current parameters when the caller gives none: the
/// first [`AUTO_GRID_PARAMS`] numeric, non-sizing parameters, each at
/// 0.5x..2x of its value (clamped to its range, duplicates dropped).
pub fn auto_grid(strategy_id: &str, base: &Value) -> Result<Grid, String> {
    let s = build_strategy(strategy_id, base)?;
    let mut grid = Grid::new();
    for spec in s.schema() {
        if grid.len() == AUTO_GRID_PARAMS {
            break;
        }
        if SIZING_KEYS.contains(&spec.key) {
            continue;
        }
        let cur = s.get_param(spec.key);
        let mut vals: Vec<Value> = Vec::new();
        for m in AUTO_GRID_STEPS {
            let v = match (spec.kind, cur.as_ref()) {
                (ParamKind::Int { .. }, Some(v)) => {
                    let base = v.as_int().unwrap_or(0) as f64;
                    spec.clamp(ParamValue::Int((base * m).round() as i64))
                }
                (ParamKind::Float { .. }, Some(v)) => {
                    spec.clamp(ParamValue::Float(v.as_float().unwrap_or(0.0) * m))
                }
                _ => break,
            };
            let j = value_json(&v);
            if !vals.contains(&j) {
                vals.push(j);
            }
        }
        if vals.len() > 1 {
            grid.push((spec.key.to_string(), vals));
        }
    }
    if grid.is_empty() {
        return Err(format!(
            "strategy '{strategy_id}' has no numeric parameter to sweep"
        ));
    }
    Ok(grid)
}

/// A grid from the caller: `{"fast": [5, 10, 20], "slow": [30, 50]}`.
/// Every value is checked against the strategy's schema.
pub fn parse_grid(strategy_id: &str, base: &Value, grid: &Value) -> Result<Grid, String> {
    let map = grid
        .as_object()
        .ok_or("'grid' must be an object: parameter -> array of values")?;
    let mut out = Grid::new();
    for (key, vals) in map {
        if SIZING_KEYS.contains(&key.as_str()) {
            return Err(format!("'{key}' sizes positions; it is not swept"));
        }
        let vals = vals
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| format!("grid '{key}': need a non-empty array"))?;
        if vals.len() > MAX_GRID_VALUES {
            return Err(format!("grid '{key}': at most {MAX_GRID_VALUES} values"));
        }
        for v in vals {
            let mut p = base.as_object().cloned().unwrap_or_default();
            p.insert(key.clone(), v.clone());
            build_strategy(strategy_id, &Value::Object(p))?;
        }
        out.push((key.clone(), vals.clone()));
    }
    if out.is_empty() {
        return Err("'grid' is empty".into());
    }
    Ok(out)
}

/// Every combination of the grid, each merged over `base`.
fn combinations(base: &Value, grid: &Grid) -> Vec<Value> {
    let mut out = vec![base.as_object().cloned().unwrap_or_default()];
    for (key, vals) in grid {
        out = out
            .into_iter()
            .flat_map(|m| {
                vals.iter().map(move |v| {
                    let mut m = m.clone();
                    m.insert(key.clone(), v.clone());
                    m
                })
            })
            .collect();
    }
    out.into_iter().map(Value::Object).collect()
}

/// Only the swept keys of a parameter set (what changed).
fn swept_part(params: &Value, grid: &Grid) -> Value {
    let mut m = serde_json::Map::new();
    for (k, _) in grid {
        if let Some(v) = params.get(k) {
            m.insert(k.clone(), v.clone());
        }
    }
    Value::Object(m)
}

fn window_json(bars: &[Ohlc]) -> Value {
    json!({
        "candles": bars.len(),
        "first_ms": bars.first().map(|b| b.time_ms),
        "last_ms": bars.last().map(|b| b.time_ms),
        "buy_and_hold_pct": buy_hold_pct(bars).map(|x| round(x, 3)),
    })
}

/// Fit on the older part of the history, judge on the newest part.
///
/// Every combination of `grid` is backtested on the training window (the
/// first `100 - holdout_pct` % of the candles) and ranked by return.  Only
/// the winner and the caller's own parameters (`base`) are then run on the
/// held-out window, which the fit never saw: that is the out-of-sample
/// number.  Test results of the other combinations are not reported, so the
/// held-out window cannot be used to pick parameters by hand either.
#[allow(clippy::too_many_arguments)]
pub fn sweep_json(
    strategy_id: &str,
    base: &Value,
    grid: &Grid,
    symbol: &str,
    tf: Timeframe,
    bars: &[Ohlc],
    holdout_pct: f64,
    cfg: &BacktestConfig,
) -> Result<Value, String> {
    if !(10.0..=50.0).contains(&holdout_pct) {
        return Err("holdout_pct must be 10-50".into());
    }
    let combos = combinations(base, grid);
    if combos.len() > MAX_COMBOS {
        return Err(format!(
            "{} combinations; at most {MAX_COMBOS} (fewer values or parameters)",
            combos.len()
        ));
    }
    let usable: Vec<Ohlc> = bars
        .iter()
        .filter(|b| !b.is_gap && b.close > 0.0 && b.open > 0.0)
        .cloned()
        .collect();
    let n_test = (usable.len() as f64 * holdout_pct / 100.0).round() as usize;
    let n_train = usable.len().saturating_sub(n_test);
    if n_train < MIN_BARS || n_test < MIN_BARS {
        return Err(format!(
            "{} candles is too few: training and held-out windows need {MIN_BARS} each",
            usable.len()
        ));
    }
    let (train, test) = usable.split_at(n_train);

    // A setting that never trades (e.g. fast = slow) "wins" a losing grid
    // with 0 %: doing nothing is not a fitted parameter, so it is not ranked.
    let tried = combos.len();
    let mut ranked: Vec<(Value, BacktestResult)> = Vec::with_capacity(tried);
    for p in combos {
        let r = run(build_strategy(strategy_id, &p)?, symbol, train, cfg)?;
        if !r.fills.is_empty() {
            ranked.push((p, r));
        }
    }
    if ranked.is_empty() {
        return Err(format!(
            "none of the {tried} settings traded on the training window: nothing to fit"
        ));
    }
    // Best return first; on a tie the shallower drawdown.
    ranked.sort_by(|a, b| {
        b.1.return_pct
            .total_cmp(&a.1.return_pct)
            .then(a.1.max_drawdown_pct.total_cmp(&b.1.max_drawdown_pct))
    });
    let (best_params, best_train) = &ranked[0];
    let best_test = run(build_strategy(strategy_id, best_params)?, symbol, test, cfg)?;
    let base_train = run(build_strategy(strategy_id, base)?, symbol, train, cfg)?;
    let base_test = run(build_strategy(strategy_id, base)?, symbol, test, cfg)?;
    let test_bh = buy_hold_pct(test).unwrap_or(0.0);

    let verdict = if best_test.trades == 0 && best_test.open_qty == 0.0 {
        "the best training setting did not trade on the held-out window: nothing learned"
    } else if best_test.return_pct <= 0.0 {
        "the best training setting LOST money on the held-out window: the fit does not carry over"
    } else if best_test.return_pct < base_test.return_pct {
        "the fitted setting did worse than your own on the held-out window: the fit is likely noise"
    } else if best_test.return_pct < test_bh {
        "made money on the held-out window but less than buy-and-hold"
    } else {
        "beat buy-and-hold on the held-out window; still one window, one sample"
    };
    let row = |p: &Value, r: &BacktestResult| {
        json!({
            "params": swept_part(p, grid),
            "return_pct": round(r.return_pct, 3),
            "max_drawdown_pct": round(r.max_drawdown_pct, 3),
            "round_trips": r.trades,
        })
    };
    Ok(json!({
        "strategy": strategy_id, "symbol": symbol, "timeframe": tf.name(),
        "grid": grid.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "combinations": tried,
        "did_not_trade": tried - ranked.len(),
        "holdout_pct": holdout_pct,
        "train": window_json(train),
        "test": window_json(test),
        "best": {
            "params": swept_part(best_params, grid),
            "train": row(best_params, best_train),
            "test": row(best_params, &best_test),
        },
        "yours": {
            "params": swept_part(base, grid),
            "train": row(base, &base_train),
            "test": row(base, &base_test),
        },
        "top_on_train": ranked.iter().take(5).map(|(p, r)| row(p, r)).collect::<Vec<_>>(),
        "verdict": verdict,
        "caveats": format!(
            "{CAVEATS}; the best of {} settings on the training window looks better than it is - \
             judge it by the held-out window only; settings that did not trade are not ranked",
            ranked.len()
        ),
    }))
}

// ─── Strategy comparison ────────────────────────────────────────────────

/// Strategies a comparison runs when none are named: every one that trades
/// on its own (the manual one only follows the user).
pub fn comparable_strategies() -> Vec<&'static str> {
    registry::catalog()
        .into_iter()
        .map(|m| m.id)
        .filter(|id| *id != "manual_market")
        .collect()
}

/// Several strategies, each with its default parameters and a position of
/// `usd` (at the first candle's price), on the same candles and fees.
/// Nothing is fitted, so the full history is a fair test for all of them.
#[allow(clippy::too_many_arguments)]
pub fn compare_json(
    ids: &[&str],
    usd: f64,
    symbol: &str,
    tf: Timeframe,
    bars: &[Ohlc],
    cfg: &BacktestConfig,
) -> Result<Value, String> {
    if !(usd.is_finite() && usd > 0.0) {
        return Err("usd must be positive".into());
    }
    let first = bars
        .iter()
        .find(|b| !b.is_gap && b.open > 0.0)
        .map(|b| b.open)
        .ok_or("no candles")?;
    let mut rows: Vec<(String, BacktestResult)> = Vec::new();
    for id in ids {
        let s = build_strategy(id, &Value::Null)?;
        let mut params = serde_json::Map::new();
        for spec in s.schema() {
            if SIZING_KEYS.contains(&spec.key) {
                params.insert(spec.key.to_string(), json!(usd / first));
            }
        }
        let r = run(
            build_strategy(id, &Value::Object(params))?,
            symbol,
            bars,
            cfg,
        )?;
        rows.push((id.to_string(), r));
    }
    rows.sort_by(|a, b| b.1.return_pct.total_cmp(&a.1.return_pct));
    let bh = buy_hold_pct(bars);
    Ok(json!({
        "symbol": symbol, "timeframe": tf.name(), "candles": bars.len(),
        "position_usd": usd, "fee_bps": cfg.fee_bps,
        "buy_and_hold_pct": bh.map(|x| round(x, 3)),
        "rows": rows.iter().map(|(id, r)| json!({
            "strategy": id,
            "return_pct": round(r.return_pct, 3),
            "beats_buy_and_hold": bh.map(|b| r.return_pct > b),
            "max_drawdown_pct": round(r.max_drawdown_pct, 3),
            "round_trips": r.trades, "win_rate_pct": round(r.win_rate_pct(), 1),
            "fees": round(r.fees, 2), "halted_by_risk_gate": r.halted,
        })).collect::<Vec<_>>(),
        "caveats": format!(
            "{CAVEATS}; default parameters, one position size for all; returns are on \
             starting_cash, so a strategy that sizes several levels (hedge_grid) holds more"
        ),
    }))
}

// ─── Forward test ───────────────────────────────────────────────────────

/// A strategy running on live prices in its own paper runtime.  Only ticks
/// and timers reach it — exactly what a live slot receives in the terminal.
pub struct ForwardTest {
    pub id: u32,
    pub strategy_id: String,
    pub symbol: String,
    pub fee_bps: f64,
    pub started_ms: i64,
    pub starting_cash: f64,
    rt: Runtime,
    fees: f64,
    fills: Vec<Fill>,
    curve: Vec<f64>,
    peak: f64,
    max_dd: f64,
    last_price: f64,
    last_sample_ms: i64,
    pub stopped: bool,
    bars: BarBuilder,
}

const CURVE_EVERY_MS: i64 = 5_000;

impl ForwardTest {
    /// Start at `price` (the live price right now).
    pub fn start(
        id: u32,
        strategy_id: &str,
        params: &Value,
        symbol: &str,
        cfg: &BacktestConfig,
        price: f64,
        now_ms: i64,
        bar_secs: i64,
    ) -> Result<Self, String> {
        if !(60..=3600).contains(&bar_secs) {
            return Err("bar_secs must be between 60 and 3600".into());
        }
        if !(price.is_finite() && price > 0.0) {
            return Err(format!("no live price for {symbol}"));
        }
        let strategy = build_strategy(strategy_id, params)?;
        let mut rt = Runtime::new(cfg.starting_cash);
        rt.broker.set_fee_bps(cfg.fee_bps);
        let idx = rt.add_slot(
            symbol.to_string(),
            "FORWARD".into(),
            strategy_id.to_string(),
            strategy,
        );
        rt.on_price(symbol, price, now_ms);
        rt.start(idx, now_ms)?;
        let mut t = Self {
            id,
            strategy_id: strategy_id.to_string(),
            symbol: symbol.to_string(),
            fee_bps: cfg.fee_bps,
            started_ms: now_ms,
            starting_cash: cfg.starting_cash,
            rt,
            fees: 0.0,
            fills: Vec::new(),
            curve: Vec::new(),
            peak: cfg.starting_cash,
            max_dd: 0.0,
            last_price: price,
            last_sample_ms: 0,
            stopped: false,
            bars: BarBuilder::new(bar_secs),
        };
        t.drain();
        Ok(t)
    }

    fn drain(&mut self) {
        for (_, _, f) in self.rt.fill_log.drain(..) {
            self.fees += f.fee;
            if self.fills.len() < FORWARD_FILL_CAP {
                self.fills.push(f);
            }
        }
    }

    fn equity(&self) -> f64 {
        // The broker's equity is net of fees already.
        self.rt.equity()
    }

    /// Feed one live price.  No-op once stopped.
    pub fn feed(&mut self, price: f64, now_ms: i64) {
        if self.stopped || !(price.is_finite() && price > 0.0) {
            return;
        }
        self.last_price = price;
        self.rt.on_price(&self.symbol, price, now_ms);
        self.rt.dispatch_timers(now_ms);
        // Closed-bar strategies (SMA Cross, RSI, Breakout) get their bars here.
        if let Some(bar) = self.bars.push(now_ms, price) {
            self.rt.on_bar(&self.symbol, bar, now_ms);
        }
        self.drain();
        let eq = self.equity();
        self.peak = self.peak.max(eq);
        if self.peak > 0.0 {
            self.max_dd = self.max_dd.max((self.peak - eq) / self.peak * 100.0);
        }
        if now_ms - self.last_sample_ms >= CURVE_EVERY_MS {
            self.last_sample_ms = now_ms;
            self.curve.push(eq);
            if self.curve.len() > FORWARD_CURVE_CAP {
                self.curve.remove(0);
            }
        }
    }

    /// Stop the strategy (its `on_stop` may act); the report stays readable.
    pub fn stop(&mut self, now_ms: i64) {
        if self.stopped {
            return;
        }
        let _ = self.rt.stop(0, now_ms);
        self.drain();
        self.stopped = true;
    }

    pub fn report(&self, now_ms: i64) -> Value {
        let (trades, wins) = count_round_trips(&self.fills);
        let pos = self.rt.broker.position(&self.symbol);
        let eq = self.equity();
        let running = self
            .rt
            .slots
            .first()
            .is_some_and(|s| s.status == SlotStatus::Running);
        let win_rate = if trades == 0 {
            0.0
        } else {
            wins as f64 * 100.0 / trades as f64
        };
        json!({
            "id": self.id, "strategy": self.strategy_id, "symbol": self.symbol,
            "state": if self.stopped { "stopped" } else if running { "running" } else { "halted" },
            "minutes_running": (now_ms - self.started_ms).max(0) / 60_000,
            "starting_cash": self.starting_cash, "equity": round(eq, 2),
            "net_pnl": round(eq - self.starting_cash, 2),
            "return_pct": round((eq / self.starting_cash - 1.0) * 100.0, 3),
            "fees": round(self.fees, 2), "max_drawdown_pct": round(self.max_dd, 3),
            "round_trips": trades, "win_rate_pct": round(win_rate, 1),
            "fills": self.fills.len(), "position_qty": pos.qty, "position_avg": pos.avg,
            "last_price": self.last_price,
            "bar_secs": self.bars.interval_secs(),
            "open_orders": self.rt.broker.open_order_count(),
            "halted_reason": self.rt.risk_state.last_violation,
            "equity_curve_5s": downsample(&self.curve, MAX_CURVE_POINTS),
            "caveats": "paper fills at the live price with no slippage; kept in memory only (lost when the terminal exits)",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize, f: impl Fn(usize) -> f64) -> Vec<Ohlc> {
        (0..n)
            .map(|i| {
                let c = f(i);
                Ohlc {
                    open: c,
                    high: c * 1.001,
                    low: c * 0.999,
                    close: c,
                    volume: 1.0,
                    date: String::new(),
                    time_ms: 1_700_000_000_000 + i as i64 * 3_600_000,
                    is_gap: false,
                }
            })
            .collect()
    }

    #[test]
    fn strategy_catalog_lists_params_with_types_and_ranges() {
        let j = strategies_json();
        let all = j["strategies"].as_array().unwrap();
        assert!(all.len() >= 4);
        let dca = all.iter().find(|s| s["id"] == "dca_timer").unwrap();
        let params = dca["params"].as_array().unwrap();
        assert!(params
            .iter()
            .any(|p| p["key"] == "qty" && p["type"] == "float" && p["max"].is_number()));
        assert!(params
            .iter()
            .any(|p| p["key"] == "side_buy" && p["type"] == "bool"));
        assert!(params
            .iter()
            .any(|p| p["key"] == "interval_secs" && p["type"] == "seconds"));
        for s in all {
            for p in s["params"].as_array().unwrap() {
                assert!(p["type"].is_string() && p["key"].is_string(), "{p}");
            }
        }
    }

    #[test]
    fn build_strategy_checks_ids_keys_and_types() {
        let err = |r: Result<Box<dyn Strategy>, String>| r.err().expect("must fail");
        assert!(err(build_strategy("nope", &Value::Null)).contains("known:"));
        assert!(build_strategy(
            "dca_timer",
            &json!({"qty": 0.5, "max_orders": 3, "side_buy": false})
        )
        .is_ok());
        assert!(
            err(build_strategy("dca_timer", &json!({"no_such_key": 1}))).contains("no parameter")
        );
        assert!(err(build_strategy("dca_timer", &json!({"qty": "lots"}))).contains("qty"));
        assert!(err(build_strategy("dca_timer", &json!({"side_buy": 1}))).contains("side_buy"));
        assert!(
            build_strategy("dca_timer", &json!([1])).is_err(),
            "params must be an object"
        );
        assert!(build_strategy("dca_timer", &Value::Null).is_ok());
        // Out-of-range values are clamped by the schema, not rejected.
        let s = build_strategy("dca_timer", &json!({"max_orders": 99_999_999})).unwrap();
        assert_eq!(
            s.get_param("max_orders").and_then(|v| v.as_int()),
            Some(10_000)
        );
    }

    #[test]
    fn config_bounds_are_enforced() {
        assert!(config_from(None, None).is_ok());
        assert!(config_from(Some(50.0), None).is_err());
        assert!(config_from(None, Some(-1.0)).is_err());
        assert!(config_from(None, Some(900.0)).is_err());
        assert!(config_from(Some(f64::NAN), None).is_err());
    }

    #[test]
    fn backtest_reply_carries_the_buy_and_hold_benchmark_and_caveats() {
        let b = bars(300, |i| 100.0 + i as f64 * 0.2);
        let cfg = BacktestConfig::default();
        let r = run(
            build_strategy("dca_timer", &Value::Null).unwrap(),
            "T",
            &b,
            &cfg,
        )
        .unwrap();
        let j = backtest_json(&r, Timeframe::H1, &b, &cfg);
        assert!(j["buy_and_hold_pct"].as_f64().unwrap() > 50.0, "{j}");
        assert!(j["caveats"].as_str().unwrap().contains("optimistic"));
        assert!(j["equity_curve"].as_array().unwrap().len() <= MAX_CURVE_POINTS);
        assert_eq!(j["candles"], 300);
    }

    #[test]
    fn walk_forward_runs_each_segment_and_summarises() {
        let b = bars(480, |i| {
            100.0 + (i as f64 / 7.0).sin() * 5.0 + i as f64 * 0.05
        });
        let cfg = BacktestConfig::default();
        let j =
            walk_forward_json("dca_timer", &Value::Null, "T", Timeframe::H1, &b, 4, &cfg).unwrap();
        assert_eq!(j["segments"].as_array().unwrap().len(), 4);
        assert_eq!(j["summary"]["of"], 4);
        let total: u64 = j["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["candles"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 480, "every candle lands in exactly one segment");
        assert!(j["caveats"]
            .as_str()
            .unwrap()
            .contains("no parameter fitting"));
        let few = bars(100, |i| 100.0 + i as f64);
        assert!(
            walk_forward_json("dca_timer", &Value::Null, "T", Timeframe::H1, &few, 4, &cfg)
                .is_err()
        );
        assert!(walk_forward_json("zzz", &Value::Null, "T", Timeframe::H1, &b, 2, &cfg).is_err());
    }

    #[test]
    fn forward_test_runs_on_live_ticks_in_its_own_ledger() {
        let cfg = BacktestConfig {
            starting_cash: 1_000.0,
            fee_bps: 10.0,
            ..Default::default()
        };
        assert!(
            ForwardTest::start(1, "manual_market", &Value::Null, "T", &cfg, 0.0, 0, 60).is_err(),
            "needs a live price"
        );
        let mut t = ForwardTest::start(
            1,
            "manual_market",
            &Value::Null,
            "T",
            &cfg,
            100.0,
            1_000,
            60,
        )
        .unwrap();
        for i in 0..10 {
            t.feed(100.0 + i as f64, 1_000 + i * 6_000);
        }
        let r = t.report(1_000 + 60_000);
        assert_eq!(r["state"], "running");
        assert_eq!(r["equity"], 1_000.0, "manual_market never trades by itself");
        assert!(r["equity_curve_5s"].as_array().unwrap().len() >= 5);
        assert!(r["caveats"].as_str().unwrap().contains("memory"));
        t.stop(2_000_000);
        assert_eq!(t.report(2_000_000)["state"], "stopped");
        t.feed(500.0, 2_100_000);
        assert_eq!(
            t.report(2_100_000)["last_price"],
            109.0,
            "stopped tests ignore new prices"
        );
    }

    #[test]
    fn forward_test_charges_fees_for_a_trading_strategy() {
        // dca_timer buys on its timer; 1% fees make the cost visible.
        let cfg = BacktestConfig {
            starting_cash: 10_000.0,
            fee_bps: 100.0,
            ..Default::default()
        };
        let params = json!({"interval_secs": 60, "qty": 1.0, "max_orders": 5});
        let mut t = ForwardTest::start(2, "dca_timer", &params, "T", &cfg, 100.0, 0, 60).unwrap();
        for i in 1..=400 {
            let px = 100.0 + (i as f64 / 5.0).sin() * 3.0;
            t.feed(px, i * 31_000);
        }
        let r = t.report(400 * 31_000);
        assert!(
            r["fills"].as_u64().unwrap() >= 1,
            "dca_timer buys on its timer: {r}"
        );
        assert!(r["fees"].as_f64().unwrap() > 0.0, "{r}");
        assert!(r["position_qty"].as_f64().unwrap() > 0.0);
        assert!(r["max_drawdown_pct"].as_f64().unwrap() >= 0.0);
    }

    /// Closed-candle series with real highs/lows around the close.
    fn wavy(n: usize, f: impl Fn(usize) -> f64) -> Vec<Ohlc> {
        (0..n)
            .map(|i| {
                let c = f(i);
                Ohlc {
                    open: if i == 0 { c } else { f(i - 1) },
                    high: c.max(if i == 0 { c } else { f(i - 1) }) * 1.002,
                    low: c.min(if i == 0 { c } else { f(i - 1) }) * 0.998,
                    close: c,
                    volume: 1.0,
                    date: String::new(),
                    time_ms: 1_700_000_000_000 + i as i64 * 3_600_000,
                    is_gap: false,
                }
            })
            .collect()
    }

    fn bt(id: &str, params: Value, data: &[Ohlc]) -> BacktestResult {
        let cfg = BacktestConfig {
            starting_cash: 100_000.0,
            fee_bps: 0.0,
            ..Default::default()
        };
        run(build_strategy(id, &params).unwrap(), "T", data, &cfg).unwrap()
    }

    #[test]
    fn signal_strategies_trade_in_a_backtest_and_suit_their_regime() {
        // A steady uptrend with pullbacks: trend followers should end positive.
        let trend = wavy(600, |i| {
            100.0 + i as f64 * 0.15 + (i as f64 / 6.0).sin() * 3.0
        });
        let sma = bt(
            "sma_cross",
            json!({"fast": 5, "slow": 20, "qty": 10}),
            &trend,
        );
        assert!(sma.trades >= 3, "sma_cross traded {} times", sma.trades);
        assert!(sma.net_pnl > 0.0, "sma_cross on a trend: {}", sma.net_pnl);
        let brk = bt(
            "breakout",
            json!({"entry_bars": 20, "exit_bars": 8, "qty": 10}),
            &trend,
        );
        assert!(brk.trades >= 1, "breakout traded {} times", brk.trades);
        assert!(brk.net_pnl > 0.0, "breakout on a trend: {}", brk.net_pnl);

        // A noisy mean-reverting series (fixed seed, so it is reproducible): mean
        // reversion profits, the trend followers bleed on false breakouts.
        let mut x = 100.0f64;
        let mut seed = 12345u64;
        let mut path = Vec::new();
        for _ in 0..800 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let u = ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            x += 0.15 * (100.0 - x) + u * 3.0;
            path.push(x);
        }
        let range = wavy(800, |i| path[i]);
        let rsi = bt(
            "rsi_reversion",
            json!({"period": 7, "oversold": 30, "exit_level": 50, "qty": 10}),
            &range,
        );
        assert!(rsi.trades >= 5, "rsi_reversion traded {} times", rsi.trades);
        assert!(
            rsi.net_pnl > 0.0,
            "rsi_reversion on a mean-reverting series: {}",
            rsi.net_pnl
        );
        let sma_range = bt(
            "sma_cross",
            json!({"fast": 5, "slow": 20, "qty": 10}),
            &range,
        );
        let brk_range = bt("breakout", json!({"qty": 10}), &range);
        assert!(
            rsi.net_pnl > sma_range.net_pnl && rsi.net_pnl > brk_range.net_pnl,
            "mean reversion beats trend-following here: rsi {} sma {} breakout {}",
            rsi.net_pnl,
            sma_range.net_pnl,
            brk_range.net_pnl
        );
    }

    #[test]
    fn signal_strategies_do_nothing_before_they_have_history_and_with_zero_qty() {
        let few = wavy(5, |i| 100.0 + i as f64);
        for id in ["sma_cross", "rsi_reversion", "breakout"] {
            let r = bt(id, Value::Null, &few);
            assert_eq!(r.fills.len(), 0, "{id} acted on 5 bars");
        }
        let trend = wavy(400, |i| 100.0 + i as f64 * 0.3);
        for id in ["sma_cross", "rsi_reversion", "breakout"] {
            let r = bt(id, json!({"qty": 0}), &trend);
            assert_eq!(r.fills.len(), 0, "{id} with qty 0");
        }
    }

    #[test]
    fn walk_forward_reports_a_regime_change_for_a_trend_strategy() {
        // First half trends, second half chops: segment returns must differ.
        let data = wavy(800, |i| {
            if i < 400 {
                100.0 + i as f64 * 0.2
            } else {
                180.0 + (i as f64 / 4.0).sin() * 4.0
            }
        });
        let cfg = BacktestConfig {
            starting_cash: 100_000.0,
            fee_bps: 10.0,
            ..Default::default()
        };
        let j = walk_forward_json(
            "sma_cross",
            &json!({"fast": 5, "slow": 20, "qty": 10}),
            "T",
            Timeframe::H1,
            &data,
            4,
            &cfg,
        )
        .unwrap();
        let rets: Vec<f64> = j["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["return_pct"].as_f64().unwrap())
            .collect();
        assert!(
            rets[0] > rets[3],
            "trending segment beats the choppy one: {rets:?}"
        );
    }

    #[test]
    fn forward_test_feeds_closed_bars_to_signal_strategies() {
        // 1-minute bars; a falling then recovering price drives RSI reversion in and out.
        let cfg = BacktestConfig {
            starting_cash: 100_000.0,
            fee_bps: 0.0,
            ..Default::default()
        };
        let params = json!({"period": 3, "oversold": 30, "exit_level": 50, "qty": 1});
        assert!(
            ForwardTest::start(9, "rsi_reversion", &params, "T", &cfg, 100.0, 0, 30).is_err(),
            "bar_secs below 60"
        );
        let mut t =
            ForwardTest::start(9, "rsi_reversion", &params, "T", &cfg, 100.0, 0, 60).unwrap();
        let mut ts = 0i64;
        let mut price = 100.0;
        // Ten falling minutes: RSI collapses, the strategy buys.
        for _ in 0..10 {
            for s in 0..4 {
                t.feed(price - s as f64 * 0.1, ts + s * 15_000);
            }
            ts += 60_000;
            price -= 1.5;
        }
        let r = t.report(ts);
        assert!(
            r["fills"].as_u64().unwrap() >= 1,
            "bought the dip on bar closes: {r}"
        );
        assert!(r["position_qty"].as_f64().unwrap() > 0.0);
        assert_eq!(r["bar_secs"], 60);
        // Ten rising minutes: RSI recovers past 50, it exits.
        for _ in 0..10 {
            for s in 0..4 {
                t.feed(price + s as f64 * 0.1, ts + s * 15_000);
            }
            ts += 60_000;
            price += 1.5;
        }
        let r = t.report(ts);
        assert_eq!(
            r["position_qty"].as_f64().unwrap(),
            0.0,
            "exited on the recovery: {r}"
        );
        assert!(r["round_trips"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn auto_grid_varies_numeric_params_but_never_the_size() {
        let g = auto_grid("sma_cross", &Value::Null).unwrap();
        let keys: Vec<&str> = g.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["fast", "slow"]);
        assert!(g.iter().all(|(_, v)| v.len() >= 3 && v.len() <= 5), "{g:?}");
        assert!(auto_grid("manual_market", &Value::Null).is_err());
        let g = auto_grid("hedge_grid", &Value::Null).unwrap();
        assert!(
            g.iter().all(|(k, _)| !SIZING_KEYS.contains(&k.as_str())),
            "{g:?}"
        );
    }

    #[test]
    fn a_grid_is_checked_and_capped() {
        let ok = parse_grid("sma_cross", &Value::Null, &json!({"fast": [5, 10]})).unwrap();
        assert_eq!(ok, vec![("fast".to_string(), vec![json!(5), json!(10)])]);
        assert!(parse_grid("sma_cross", &Value::Null, &json!({"qty": [1, 2]})).is_err());
        assert!(parse_grid("sma_cross", &Value::Null, &json!({"nope": [1]})).is_err());
        assert!(parse_grid("sma_cross", &Value::Null, &json!({"fast": []})).is_err());
        assert!(parse_grid("sma_cross", &Value::Null, &json!({"fast": "x"})).is_err());
        assert!(parse_grid(
            "sma_cross",
            &Value::Null,
            &json!({"fast": [1,2,3,4,5,6,7,8,9,10,11]})
        )
        .is_err());
        let combos = combinations(
            &json!({"qty": 1.0}),
            &vec![
                ("fast".into(), vec![json!(5), json!(10)]),
                ("slow".into(), vec![json!(30), json!(50), json!(70)]),
            ],
        );
        assert_eq!(combos.len(), 6);
        assert!(combos.iter().all(|c| c["qty"] == 1.0));
    }

    #[test]
    fn a_sweep_fits_on_the_old_part_and_judges_on_the_new_part() {
        // Trend up for 300 candles, then down for 150.
        let data = wavy(450, |i| {
            let t = if i < 300 {
                100.0 + i as f64 * 0.3
            } else {
                190.0 - (i - 300) as f64 * 0.3
            };
            t + (i as f64 * 0.7).sin() * 2.0
        });
        let cfg = BacktestConfig {
            starting_cash: 100_000.0,
            fee_bps: 0.0,
            ..Default::default()
        };
        let base = json!({"qty": 10.0});
        let grid = auto_grid("sma_cross", &base).unwrap();
        let j = sweep_json(
            "sma_cross",
            &base,
            &grid,
            "T",
            Timeframe::H1,
            &data,
            30.0,
            &cfg,
        )
        .unwrap();
        assert_eq!(j["train"]["candles"], 315);
        assert_eq!(j["test"]["candles"], 135);
        assert!(
            j["test"]["buy_and_hold_pct"].as_f64().unwrap() < 0.0,
            "the held-out part falls"
        );
        assert!(j["combinations"].as_u64().unwrap() >= 9);
        let best = j["best"]["train"]["return_pct"].as_f64().unwrap();
        assert!(j["best"]["train"]["round_trips"].as_u64().unwrap() > 0 || best != 0.0);
        let flat = data
            .iter()
            .map(|b| Ohlc {
                open: 100.0,
                high: 100.0,
                low: 100.0,
                close: 100.0,
                ..b.clone()
            })
            .collect::<Vec<_>>();
        assert!(
            sweep_json(
                "sma_cross",
                &base,
                &grid,
                "T",
                Timeframe::H1,
                &flat,
                30.0,
                &cfg
            )
            .unwrap_err()
            .contains("traded"),
            "a grid where nothing trades is an error, not a 0 % winner"
        );
        for r in j["top_on_train"].as_array().unwrap() {
            assert!(
                r["return_pct"].as_f64().unwrap() <= best + 1e-9,
                "ranked by training return"
            );
        }
        assert!(j["best"]["test"]["return_pct"].is_number());
        assert!(j["yours"]["test"]["return_pct"].is_number());
        assert!(j["verdict"].as_str().unwrap().len() > 10);
        assert!(
            j["best"]["params"].get("qty").is_none(),
            "only swept keys are shown"
        );
        // Bounds.
        assert!(sweep_json(
            "sma_cross",
            &base,
            &grid,
            "T",
            Timeframe::H1,
            &data,
            5.0,
            &cfg
        )
        .is_err());
        assert!(sweep_json(
            "sma_cross",
            &base,
            &grid,
            "T",
            Timeframe::H1,
            &data[..80],
            30.0,
            &cfg
        )
        .unwrap_err()
        .contains("too few"));
        let big: Grid = vec![
            ("fast".into(), (2..12).map(|v| json!(v)).collect()),
            ("slow".into(), (30..40).map(|v| json!(v)).collect()),
        ];
        assert!(sweep_json(
            "sma_cross",
            &base,
            &big,
            "T",
            Timeframe::H1,
            &data,
            30.0,
            &cfg
        )
        .unwrap_err()
        .contains("combinations"));
    }

    #[test]
    fn a_comparison_runs_every_trading_strategy_on_the_same_candles() {
        let data = wavy(300, |i| {
            100.0 + i as f64 * 0.2 + (i as f64 * 0.5).sin() * 3.0
        });
        let cfg = BacktestConfig {
            starting_cash: 100_000.0,
            fee_bps: 10.0,
            ..Default::default()
        };
        let ids = comparable_strategies();
        assert!(!ids.contains(&"manual_market") && ids.contains(&"sma_cross"));
        let j = compare_json(&ids, 1_000.0, "T", Timeframe::H1, &data, &cfg).unwrap();
        let rows = j["rows"].as_array().unwrap();
        assert_eq!(rows.len(), ids.len());
        let rets: Vec<f64> = rows
            .iter()
            .map(|r| r["return_pct"].as_f64().unwrap())
            .collect();
        assert!(
            rets.windows(2).all(|w| w[0] >= w[1]),
            "sorted by return: {rets:?}"
        );
        assert!(j["buy_and_hold_pct"].as_f64().unwrap() > 0.0);
        assert!(
            rows.iter().any(|r| r["round_trips"].as_u64().unwrap() > 0),
            "{j}"
        );
        assert!(compare_json(&["nope"], 1_000.0, "T", Timeframe::H1, &data, &cfg).is_err());
        assert!(compare_json(&ids, 0.0, "T", Timeframe::H1, &data, &cfg).is_err());
    }
}
