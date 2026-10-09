//! UI-editable parameter system for strategies.
//!
//! Each strategy publishes a static `&[ParamSpec]` (its schema) that the
//! UI walks to render editable widgets. Values are typed via `ParamValue`
//! and round-trip through JSON for persistence.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl ParamValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(v) => Some(*v as i64),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ParamKind {
    Int {
        min: i64,
        max: i64,
        default: i64,
        step: i64,
    },
    Float {
        min: f64,
        max: f64,
        default: f64,
        step: f64,
    },
    Bool {
        default: bool,
    },
    Choice {
        options: &'static [&'static str],
        default: &'static str,
    },
    /// Time duration stored as seconds (UI may render "30s" / "5m" / "1h").
    Duration {
        default_secs: u64,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub help: &'static str,
}

impl ParamSpec {
    /// Force `value` into this spec's declared range.  Non-finite floats fall
    /// back to the default; text / bool pass through.  Values come from the
    /// database or the terminal, so the schema's `min` / `max` must hold here
    /// and not only in the UI stepper (audit D8).
    pub fn clamp(&self, value: ParamValue) -> ParamValue {
        match (self.kind, &value) {
            (ParamKind::Int { min, max, .. }, v) => {
                ParamValue::Int(v.as_int().unwrap_or(0).clamp(min, max))
            }
            (
                ParamKind::Float {
                    min, max, default, ..
                },
                v,
            ) => {
                let f = v.as_float().filter(|f| f.is_finite()).unwrap_or(default);
                ParamValue::Float(f.clamp(min, max))
            }
            (ParamKind::Duration { default_secs }, v) => {
                ParamValue::Int(v.as_int().unwrap_or(default_secs as i64).max(1))
            }
            _ => value,
        }
    }

    /// Bump a numeric param up/down by its step (or 1) within bounds.
    /// Returns the new value, clamped. No-op for Bool/Choice/Text.
    pub fn nudge(&self, current: &ParamValue, dir: i32) -> ParamValue {
        let step_sign = if dir >= 0 { 1.0 } else { -1.0 };
        match self.kind {
            ParamKind::Int { min, max, step, .. } => {
                let base = current.as_int().unwrap_or(0);
                let s = step.max(1) as f64 * step_sign;
                ParamValue::Int(((base as f64 + s) as i64).clamp(min, max))
            }
            ParamKind::Float { min, max, step, .. } => {
                let base = current.as_float().unwrap_or(0.0);
                let s = (if step <= 0.0 { 0.1 } else { step }) * step_sign;
                ParamValue::Float((base + s).clamp(min, max))
            }
            ParamKind::Duration { .. } => {
                let base = current.as_int().unwrap_or(0);
                let s = if base >= 3600 {
                    600
                } else if base >= 300 {
                    60
                } else if base >= 60 {
                    15
                } else {
                    5
                };
                ParamValue::Int((base + s as i64 * dir.signum() as i64).max(1))
            }
            ParamKind::Bool { .. } => ParamValue::Bool(!current.as_bool().unwrap_or(false)),
            ParamKind::Choice { options, .. } => {
                let cur = current.as_text().unwrap_or("");
                let i = options.iter().position(|o| *o == cur).unwrap_or(0) as i32;
                let n = options.len() as i32;
                let next = ((i + dir).rem_euclid(n.max(1))) as usize;
                ParamValue::Text(options[next].into())
            }
        }
    }
}
