//! A small in-memory ring of events for agents (risk-gate rejections, kill
//! switch changes, limit edits, denied tool calls).  Ids only grow, so an
//! agent keeps a cursor (`since_id`) and never misses or repeats an event
//! unless it falls more than [`CAP`] events behind.

use std::collections::VecDeque;

use serde_json::{json, Value};

pub const CAP: usize = 200;
/// Longest text kept per event (chars).
const TEXT_MAX: usize = 300;

#[derive(Clone, Debug, PartialEq)]
pub struct AgentEvent {
    pub id: u64,
    pub ts: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Default)]
pub struct EventLog {
    events: VecDeque<AgentEvent>,
    next_id: u64,
}

impl EventLog {
    pub fn push(&mut self, ts: i64, kind: &str, text: &str) -> u64 {
        self.next_id += 1;
        self.events.push_back(AgentEvent {
            id: self.next_id,
            ts,
            kind: kind.to_string(),
            text: text.chars().take(TEXT_MAX).collect(),
        });
        while self.events.len() > CAP {
            self.events.pop_front();
        }
        self.next_id
    }

    /// Id of the newest event (0 = none yet).
    pub fn last_id(&self) -> u64 {
        self.next_id
    }

    /// Events with `id > since`, oldest first, at most `limit`.
    pub fn since(&self, since: u64, limit: usize) -> Vec<&AgentEvent> {
        self.events
            .iter()
            .filter(|e| e.id > since)
            .take(limit)
            .collect()
    }

    /// The newest `limit` events, oldest first (to start a cursor).
    pub fn recent(&self, limit: usize) -> Vec<&AgentEvent> {
        let skip = self.events.len().saturating_sub(limit);
        self.events.iter().skip(skip).collect()
    }
}

pub fn events_json(events: &[&AgentEvent], last_id: u64, timed_out: bool) -> Value {
    json!({
        "last_id": last_id,
        "timed_out": timed_out,
        "events": events.iter().map(|e| json!({
            "id": e.id, "time_ms": e.ts, "kind": e.kind, "text": e.text,
        })).collect::<Vec<_>>(),
        "note": "pass last_id back as since_id to receive only newer events",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_grow_and_since_returns_only_newer_events_in_order() {
        let mut l = EventLog::default();
        assert_eq!(l.last_id(), 0);
        let a = l.push(1, "blocked", "x");
        let b = l.push(2, "kill_switch", "ON");
        let c = l.push(3, "limit_changed", "y");
        assert_eq!((a, b, c), (1, 2, 3));
        let ids: Vec<u64> = l.since(1, 10).iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![2, 3]);
        assert!(l.since(3, 10).is_empty());
        assert_eq!(l.since(0, 2).len(), 2, "limit applies");
    }

    #[test]
    fn the_ring_drops_the_oldest_but_ids_keep_counting() {
        let mut l = EventLog::default();
        for i in 0..(CAP + 25) {
            l.push(i as i64, "k", "t");
        }
        assert_eq!(l.last_id(), (CAP + 25) as u64);
        assert_eq!(l.since(0, 1000).len(), CAP);
        assert_eq!(l.since(0, 1)[0].id, 26, "oldest 25 are gone");
        assert_eq!(
            l.recent(3).iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![(CAP + 23) as u64, (CAP + 24) as u64, (CAP + 25) as u64]
        );
    }

    #[test]
    fn long_text_is_cut_and_json_carries_the_cursor() {
        let mut l = EventLog::default();
        l.push(5, "blocked", &"z".repeat(1000));
        let e = l.since(0, 10);
        assert_eq!(e[0].text.chars().count(), TEXT_MAX);
        let j = events_json(&e, l.last_id(), false);
        assert_eq!(j["last_id"], 1);
        assert_eq!(j["events"][0]["kind"], "blocked");
        assert_eq!(events_json(&[], 9, true)["timed_out"], true);
    }
}
