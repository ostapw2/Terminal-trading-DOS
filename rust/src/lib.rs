//! `dos` — terminal trading dashboard runtime.
//!
//! Public modules:
//!   * `tokens`     — generated design tokens (do not edit; see shared/tokens.toml)
//!   * `widgets`    — reusable UI widgets (FilePanel, FunctionBar, …)

// Render functions take explicit layout/style parameters instead of a
// parameter-object; the arity is deliberate.
#![allow(clippy::too_many_arguments)]

pub mod data;
pub mod engine;
pub mod markets;
pub mod strategies;
pub mod tokens;
pub mod widgets;
