//! `dos` — terminal trading dashboard runtime.
//!
//! Public modules:
//!   * `tokens`     — generated design tokens (do not edit; see shared/tokens.toml)
//!   * `widgets`    — reusable UI widgets (FilePanel, FunctionBar, …)

// Render functions take explicit layout/style parameters instead of a
// parameter-object; the arity is deliberate.
#![allow(clippy::too_many_arguments)]

pub mod agent;
pub mod autobasket;
pub mod basket;
pub mod carry;
pub mod compare;
pub mod crossx;
pub mod data;
pub mod desk;
pub mod edges;
pub mod engine;
pub mod exchanges;
pub mod formula;
pub mod futures;
pub mod gfx;
pub mod i18n;
pub mod ideas;
pub mod judge;
pub mod landscape;
pub mod link;
pub mod liqmap;
pub mod live_ideas;
pub mod markets;
pub mod movers;
pub mod options;
pub mod pairs;
pub mod perf;
pub mod pullback;
pub mod replay;
pub mod scanner;
pub mod search;
pub mod spread_test;
pub mod statarb;
pub mod strategies;
pub mod synthetic;
pub mod tokens;
pub mod watchlists;
pub mod widgets;
