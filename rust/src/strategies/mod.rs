//! Pluggable strategy framework.
//!
//! ## Adding a new strategy
//!
//! 1. Create a new file `rust/src/strategies/<name>.rs` implementing the
//!    [`Strategy`] trait.  Copy `dca_timer.rs` as a starting point.
//! 2. Register it in [`registry::instantiate`] and [`registry::catalog`]
//!    (one line each).
//!
//! That's the entire surface area.  The UI auto-renders parameter editors
//! from [`Strategy::schema`], the runtime drives the `on_*` callbacks,
//! and the broker resolves emitted [`Action`]s.
//!
//! ## Mapping from common scripting concepts
//!
//! Scripting concept       → Rust equivalent
//! ──────────────────────  ────────────────────────────────────────
//! `input.int(..)`         → [`ParamSpec`] with [`ParamKind::Int`]
//! `input.float(..)`       → [`ParamSpec`] with [`ParamKind::Float`]
//! `input.bool(..)`        → [`ParamKind::Bool`]
//! `input.string(opts=..)` → [`ParamKind::Choice`]
//! recalc on each bar      → [`Strategy::on_bar`]
//! recalc on each tick     → [`Strategy::on_tick`]
//! periodic timer          → [`Strategy::on_timer`] (set `meta().timer_secs`)
//! `strategy.entry(..)`    → [`Ctx::buy_market`] / [`Ctx::sell_market`]
//! `strategy.exit limit=`  → [`Ctx::buy_limit`] / [`Ctx::sell_limit`]
//! `strategy.cancel_all`   → [`Ctx::cancel_all`]
//! `plot(..)` debug print  → [`Ctx::note`] (renders in UI log)

pub mod actions;
pub mod context;
pub mod params;
pub mod registry;

// Built-in strategies.  Add your new module to this list.
pub mod dca_timer;
pub mod hedge_grid;
pub mod limit_entry;
pub mod manual_market;

pub use actions::Action;
pub use context::{Bar, Ctx, Fill, Position, Side, Tick};
pub use params::{ParamKind, ParamSpec, ParamValue};

#[derive(Clone, Copy, Debug)]
pub struct StrategyMeta {
    /// Stable identifier; used for registry lookup + persistence.
    pub id: &'static str,
    /// Short name shown in the UI dropdown.
    pub name: &'static str,
    /// One-line description shown next to the dropdown.
    pub description: &'static str,
    /// Timer interval in seconds.  0 disables `on_timer`.  The runtime
    /// guarantees at most one `on_timer` call per `timer_secs` per slot,
    /// but strategies may further self-gate.
    pub timer_secs: u64,
}

pub trait Strategy: Send {
    fn meta(&self) -> StrategyMeta;
    fn schema(&self) -> &[ParamSpec];

    /// Read a parameter's current value, or `None` if the key is unknown.
    fn get_param(&self, key: &str) -> Option<ParamValue>;

    /// Apply a value coming from the UI/persistence.  Implementations
    /// should clamp/coerce; return `Err(msg)` only for unrecoverable
    /// type mismatches.
    fn set_param(&mut self, key: &str, value: ParamValue) -> Result<(), String>;

    /// [`set_param`](Self::set_param) after forcing the value into the schema's
    /// range.  Use this for anything that did not come from the stepper.
    fn set_param_checked(&mut self, key: &str, value: ParamValue) -> Result<(), String> {
        let value = match self.schema().iter().find(|s| s.key == key) {
            Some(spec) => spec.clamp(value),
            None => value,
        };
        self.set_param(key, value)
    }

    fn on_start(&mut self, _ctx: &mut Ctx) {}
    fn on_stop(&mut self, _ctx: &mut Ctx) {}
    fn on_tick(&mut self, _ctx: &mut Ctx, _t: &Tick) {}
    fn on_bar(&mut self, _ctx: &mut Ctx, _b: &Bar) {}
    fn on_fill(&mut self, _ctx: &mut Ctx, _f: &Fill) {}
    fn on_timer(&mut self, _ctx: &mut Ctx) {}
}
