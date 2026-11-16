//! Agent alerts: "wake me when ..." rules that fire once as an event.
//!
//! Price rules (above / below a level, a move within minutes) are checked on
//! every live price; a pattern rule watches a symbol's candles on one
//! timeframe and fires when a newly CLOSED candle completes a candlestick
//! pattern (`ta::candle_patterns`).  Each closed candle is judged once.
//!
//! The book is saved in the database (kv `agent_alerts`) by the trading
//! window, so alerts survive a restart.  Pure: the terminal fetches candles
//! and feeds them in.

use serde::{Deserialize, Serialize};

use crate::agent::ta;
use crate::markets::{Ohlc, Timeframe};
use crate::watchlists::MoveDir;

pub const MAX_ALERTS: usize = 50;
pub const MAX_NOTE_CHARS: usize = 80;
pub const KV_KEY: &str = "agent_alerts";

/// Patterns a pattern alert may name (`ta::candle_patterns`).
pub const PATTERN_NAMES: &[&str] = &[
    "bullish_engulfing",
    "bearish_engulfing",
    "bullish_marubozu",
    "bearish_marubozu",
    "doji",
    "hammer",
    "hanging_man",
    "inverted_hammer",
    "shooting_star",
    "inside_bar",
    "morning_star",
    "evening_star",
    "three_white_soldiers",
    "three_black_crows",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    Above {
        price: f64,
    },
    Below {
        price: f64,
    },
    Move {
        pct: f64,
        window_min: u32,
        dir: MoveDir,
    },
    /// A candlestick pattern completes on a closed `tf` candle: one of
    /// `names` (empty = any), with `bias` when given.
    Pattern {
        tf: String,
        names: Vec<String>,
        bias: Option<String>,
    },
    /// At least `usd` of the symbol's positions liquidated within
    /// `window_min` minutes (`side`: "long", "short", or none for both),
    /// counting only liquidations after the alert was set.
    Liquidations {
        usd: f64,
        window_min: u32,
        side: Option<String>,
    },
}

impl Rule {
    pub fn describe(&self) -> String {
        match self {
            Rule::Above { price } => format!("price >= {price}"),
            Rule::Below { price } => format!("price <= {price}"),
            Rule::Move {
                pct,
                window_min,
                dir,
            } => format!(
                "moves {}{pct}% within {window_min}m",
                match dir {
                    MoveDir::Up => "+",
                    MoveDir::Down => "-",
                    MoveDir::Either => "±",
                }
            ),
            Rule::Liquidations {
                usd,
                window_min,
                side,
            } => format!(
                "{} liquidated >= {} within {window_min}m",
                match side.as_deref() {
                    Some("long") => "longs",
                    Some("short") => "shorts",
                    _ => "positions",
                },
                crate::data::liquidations::fmt_usd(*usd)
            ),
            Rule::Pattern { tf, names, bias } => format!(
                "{} on a closed {tf} candle{}",
                if names.is_empty() {
                    "any candlestick pattern".to_string()
                } else {
                    names.join(" / ")
                },
                bias.as_deref()
                    .map(|b| format!(" ({b})"))
                    .unwrap_or_default()
            ),
        }
    }

    /// The timeframe a pattern rule watches.
    pub fn timeframe(&self) -> Option<Timeframe> {
        match self {
            Rule::Pattern { tf, .. } => crate::agent::analytics::parse_timeframe(tf),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: u32,
    pub symbol: String,
    pub rule: Rule,
    pub created_ms: i64,
    #[serde(default)]
    pub note: String,
    /// Pattern rules: open time of the newest closed candle already judged.
    #[serde(default)]
    pub last_bar_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AlertBook {
    #[serde(default)]
    pub alerts: Vec<Alert>,
    #[serde(default)]
    pub next_id: u32,
}

impl AlertBook {
    pub fn from_json(s: &str) -> Self {
        let mut b: AlertBook = serde_json::from_str(s).unwrap_or_default();
        b.alerts.truncate(MAX_ALERTS);
        let max_id = b.alerts.iter().map(|a| a.id).max().unwrap_or(0);
        b.next_id = b.next_id.max(max_id + 1).max(1);
        b
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Add an alert; pattern rules start judging at the next candle to
    /// close after `now_ms`.
    pub fn add(
        &mut self,
        symbol: String,
        rule: Rule,
        note: &str,
        now_ms: i64,
    ) -> Result<&Alert, String> {
        if self.alerts.len() >= MAX_ALERTS {
            return Err(format!("at most {MAX_ALERTS} alerts: clear some first"));
        }
        let last_bar_ms = match rule.timeframe() {
            Some(tf) => last_closed_open_ms(tf, now_ms),
            None => 0,
        };
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.alerts.push(Alert {
            id,
            symbol,
            rule,
            created_ms: now_ms,
            note: note.chars().take(MAX_NOTE_CHARS).collect(),
            last_bar_ms,
        });
        Ok(self.alerts.last().expect("just pushed"))
    }
}

/// Check a pattern name / bias from an agent.
pub fn parse_pattern(names: &[String], bias: Option<&str>) -> Result<(), String> {
    for n in names {
        if !PATTERN_NAMES.contains(&n.as_str()) {
            return Err(format!(
                "unknown pattern '{n}' (one of: {})",
                PATTERN_NAMES.join(", ")
            ));
        }
    }
    match bias {
        None | Some("bullish") | Some("bearish") | Some("neutral") => Ok(()),
        Some(b) => Err(format!("bias '{b}': bullish, bearish or neutral")),
    }
}

/// Open time of the newest candle of `tf` that is closed at `now_ms`: the
/// one before the candle now forming (calendar weeks and months).
pub fn last_closed_open_ms(tf: Timeframe, now_ms: i64) -> i64 {
    let forming = tf.open_ms(now_ms);
    tf.open_ms(forming - 1)
}

/// May a candle newer than the last judged one have closed?  (Fetch only
/// then.)
pub fn pattern_due(a: &Alert, now_ms: i64) -> bool {
    a.rule
        .timeframe()
        .is_some_and(|tf| last_closed_open_ms(tf, now_ms) > a.last_bar_ms)
}

/// Judge the candles fetched for a pattern alert: `(fired text, new
/// last_bar_ms)`.  Only closed candles count; one already judged is never
/// judged again.
pub fn judge_pattern(a: &Alert, bars: &[Ohlc], now_ms: i64) -> (Option<String>, i64) {
    let Rule::Pattern { tf, names, bias } = &a.rule else {
        return (None, a.last_bar_ms);
    };
    let Some(t) = a.rule.timeframe() else {
        return (None, a.last_bar_ms);
    };
    let closed: Vec<Ohlc> = bars
        .iter()
        .filter(|b| !b.is_gap && b.time_ms > 0 && t.next_open_ms(b.time_ms) <= now_ms)
        .cloned()
        .collect();
    let Some(newest) = closed.last().map(|b| b.time_ms) else {
        return (None, a.last_bar_ms);
    };
    if newest <= a.last_bar_ms {
        return (None, a.last_bar_ms);
    }
    let hit = ta::candle_patterns(&closed, 1).into_iter().find(|p| {
        p.bars_ago == 0
            && (names.is_empty() || names.iter().any(|n| n == p.name))
            && bias.as_deref().is_none_or(|b| b == p.bias)
    });
    let text = hit.map(|p| {
        let close = closed.last().map_or(0.0, |b| b.close);
        format!(
            "{} {} on the {tf} candle just closed ({}, close {close})",
            a.symbol, p.name, p.bias
        )
    });
    (text, newest)
}

/// Longest window of a liquidation alert, minutes.
pub const MAX_LIQ_WINDOW_MIN: u32 = 240;

/// A liquidation alert against the symbol's liquidations (oldest first):
/// the text when it fires.
pub fn judge_liquidations<'a>(
    a: &Alert,
    liqs: impl DoubleEndedIterator<Item = &'a crate::data::liquidations::Liquidation>,
    now_ms: i64,
) -> Option<String> {
    let Rule::Liquidations {
        usd,
        window_min,
        side,
    } = &a.rule
    else {
        return None;
    };
    let since = (now_ms - *window_min as i64 * 60_000).max(a.created_ms);
    let (mut long, mut short) = (0.0, 0.0);
    for l in liqs.rev().take_while(|l| l.time_ms >= since) {
        if l.long {
            long += l.usd();
        } else {
            short += l.usd();
        }
    }
    let hit = match side.as_deref() {
        Some("long") => long,
        Some("short") => short,
        _ => long + short,
    };
    use crate::data::liquidations::fmt_usd;
    (hit >= *usd).then(|| {
        format!(
            "{}: {} liquidated within {window_min}m (longs {}, shorts {})",
            a.symbol,
            fmt_usd(hit),
            fmt_usd(long),
            fmt_usd(short)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: i64, o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            date: String::new(),
            time_ms: t,
            is_gap: false,
        }
    }

    const H: i64 = 3_600_000;

    /// A falling series ending in a bullish engulfing candle at `last`.
    fn engulfing(last: i64) -> Vec<Ohlc> {
        let mut v = Vec::new();
        let mut p = 120.0;
        for i in 0..20 {
            let t = last - (20 - i) * H;
            v.push(bar(t, p, p + 0.5, p - 1.5, p - 1.0));
            p -= 1.0;
        }
        // Small bearish, then a big bullish candle that engulfs it.
        v.push(bar(last - H, p, p + 0.2, p - 0.8, p - 0.6));
        v.push(bar(last, p - 0.8, p + 1.5, p - 0.9, p + 1.2));
        v
    }

    fn pattern_rule(names: &[&str]) -> Rule {
        Rule::Pattern {
            tf: "1h".into(),
            names: names.iter().map(|s| s.to_string()).collect(),
            bias: None,
        }
    }

    #[test]
    fn a_pattern_fires_once_on_a_closed_candle_only() {
        let last = 100 * H;
        let mut book = AlertBook::default();
        book.add(
            "BTCUSDT".into(),
            pattern_rule(&["bullish_engulfing"]),
            "n",
            last - 10,
        )
        .unwrap();
        let a = book.alerts[0].clone();
        assert_eq!(a.last_bar_ms, last - 2 * H, "starts at the last closed bar");
        // The engulfing candle is still open: nothing.
        let (t, _) = judge_pattern(&a, &engulfing(last), last + H / 2);
        assert!(t.is_none());
        assert!(!pattern_due(&a, last + H / 2) || a.last_bar_ms < last);
        // Closed now: fires and remembers the bar.
        let (t, seen) = judge_pattern(&a, &engulfing(last), last + H + 1);
        assert!(
            t.as_deref()
                .is_some_and(|s| s.contains("bullish_engulfing")),
            "{t:?}"
        );
        assert_eq!(seen, last);
        let judged = Alert {
            last_bar_ms: seen,
            ..a.clone()
        };
        assert!(
            judge_pattern(&judged, &engulfing(last), last + H + 2)
                .0
                .is_none(),
            "once"
        );
        // Another name does not match.
        let other = Alert {
            rule: pattern_rule(&["hammer"]),
            ..a.clone()
        };
        assert!(judge_pattern(&other, &engulfing(last), last + H + 1)
            .0
            .is_none());
        // Any pattern with the right bias does.
        let any = Alert {
            rule: Rule::Pattern {
                tf: "1h".into(),
                names: vec![],
                bias: Some("bullish".into()),
            },
            ..a
        };
        assert!(judge_pattern(&any, &engulfing(last), last + H + 1)
            .0
            .is_some());
    }

    #[test]
    fn the_book_round_trips_caps_and_keeps_ids_unique() {
        let mut b = AlertBook::default();
        b.add("A".into(), Rule::Above { price: 1.0 }, &"x".repeat(200), 0)
            .unwrap();
        assert_eq!(b.alerts[0].note.chars().count(), MAX_NOTE_CHARS);
        b.add(
            "B".into(),
            Rule::Move {
                pct: 2.0,
                window_min: 15,
                dir: MoveDir::Up,
            },
            "",
            0,
        )
        .unwrap();
        let back = AlertBook::from_json(&b.to_json());
        assert_eq!(back, b);
        let mut broken = AlertBook::from_json(
            r#"{"alerts":[{"id":9,"symbol":"A","rule":{"kind":"below","price":2.0},"created_ms":0}]}"#,
        );
        assert_eq!(broken.next_id, 10, "ids continue after the highest");
        assert_eq!(
            broken
                .add("C".into(), Rule::Above { price: 1.0 }, "", 0)
                .unwrap()
                .id,
            10
        );
        assert!(AlertBook::from_json("garbage").alerts.is_empty());
        for _ in 0..MAX_ALERTS {
            let _ = b.add("X".into(), Rule::Above { price: 1.0 }, "", 0);
        }
        assert_eq!(b.alerts.len(), MAX_ALERTS);
        assert!(b
            .add("X".into(), Rule::Above { price: 1.0 }, "", 0)
            .is_err());
    }

    #[test]
    fn pattern_names_and_bias_are_checked() {
        assert!(parse_pattern(&["hammer".into()], Some("bullish")).is_ok());
        assert!(parse_pattern(&["hamster".into()], None).is_err());
        assert!(parse_pattern(&[], Some("up")).is_err());
        assert_eq!(
            pattern_rule(&["doji"]).describe(),
            "doji on a closed 1h candle"
        );
        // Every listed name is one the detector can report.
        let src = include_str!("ta.rs");
        for n in PATTERN_NAMES {
            assert!(src.contains(&format!("\"{n}\"")), "{n}");
        }
    }

    #[test]
    fn liquidation_alert_counts_only_after_it_was_set_and_its_side() {
        use crate::data::liquidations::Liquidation;
        let l = |long: bool, usd: f64, t: i64| Liquidation {
            symbol: "BTCUSDT".into(),
            long,
            price: 100.0,
            qty: usd / 100.0,
            time_ms: t,
        };
        let mut book = AlertBook::default();
        let rule = Rule::Liquidations {
            usd: 1_000.0,
            window_min: 5,
            side: Some("long".into()),
        };
        let a = book
            .add("BTCUSDT".into(), rule, "", 1_000_000)
            .unwrap()
            .clone();
        let now = 1_100_000;
        // Before the alert: does not count.  Shorts: not this side.
        let mut liqs = vec![l(true, 5_000.0, 900_000), l(false, 5_000.0, 1_050_000)];
        assert_eq!(judge_liquidations(&a, liqs.iter(), now), None);
        liqs.push(l(true, 600.0, 1_060_000));
        assert_eq!(judge_liquidations(&a, liqs.iter(), now), None);
        liqs.push(l(true, 400.0, 1_090_000));
        let fired = judge_liquidations(&a, liqs.iter(), now).unwrap();
        assert!(fired.contains("$1K liquidated within 5m"), "{fired}");
        // Out of the window again.
        assert_eq!(
            judge_liquidations(&a, liqs.iter(), 1_090_000 + 6 * 60_000),
            None
        );
        assert!(a
            .rule
            .describe()
            .contains("longs liquidated >= $1K within 5m"));
    }
}
