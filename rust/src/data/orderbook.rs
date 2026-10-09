//! Order-book (depth-of-market) data.
//!
//! A REST snapshot seeds a [`LocalBook`]; the `<symbol>@depth@100ms` diff
//! stream is then applied with [`LocalBook::apply_checked`], which enforces
//! Binance's per-venue sequencing rules (they differ between Spot and
//! USDⓈ-M Futures).

use crate::data::binance::Venue;

#[derive(Clone, Debug, Default)]
pub struct OrderBookSnapshot {
    /// (price, qty), DESCENDING price (best bid first).
    pub bids: Vec<(f64, f64)>,
    /// (price, qty), ASCENDING price (best ask first).
    pub asks: Vec<(f64, f64)>,
}

impl OrderBookSnapshot {
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|(p, _)| *p)
    }
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|(p, _)| *p)
    }
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / 2.0),
            _ => None,
        }
    }
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        }
    }
    pub fn max_size(&self) -> f64 {
        self.bids
            .iter()
            .map(|(_, q)| *q)
            .chain(self.asks.iter().map(|(_, q)| *q))
            .fold(0.0_f64, f64::max)
    }
    pub fn total_bid_size(&self) -> f64 {
        self.bids.iter().map(|(_, q)| *q).sum()
    }
    pub fn total_ask_size(&self) -> f64 {
        self.asks.iter().map(|(_, q)| *q).sum()
    }
    /// Estimate the price tick size from adjacent ask levels' median diff.
    pub fn estimate_tick(&self) -> f64 {
        let mut diffs: Vec<f64> = Vec::new();
        if self.asks.len() >= 2 {
            for w in self.asks.windows(2) {
                let d = (w[1].0 - w[0].0).abs();
                if d > 0.0 {
                    diffs.push(d);
                }
            }
        }
        if self.bids.len() >= 2 {
            for w in self.bids.windows(2) {
                let d = (w[1].0 - w[0].0).abs();
                if d > 0.0 {
                    diffs.push(d);
                }
            }
        }
        if diffs.is_empty() {
            return 0.01;
        }
        diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        diffs[diffs.len() / 2]
    }
    /// Aggregate book levels into wider buckets of `bucket_size` price width.
    /// Returns (bucket_price, total_qty) per side, sorted same as input.
    /// `bucket_size` should be a multiple of the native tick.
    pub fn aggregated(&self, bucket_size: f64) -> AggregatedBook {
        if bucket_size <= 0.0 {
            return AggregatedBook {
                bids: self.bids.clone(),
                asks: self.asks.clone(),
            };
        }
        // Bids: bucket DOWN (lower edge), so a bid at 60100.05 in 1.0 bucket
        // groups with 60100.20, 60100.55 → bucket_price = 60100.0.
        let mut bid_map: std::collections::BTreeMap<i64, f64> = Default::default();
        let scale = (1.0 / bucket_size).max(1.0);
        for (p, q) in &self.bids {
            let key = (p * scale).floor() as i64;
            *bid_map.entry(key).or_insert(0.0) += q;
        }
        let mut bids: Vec<(f64, f64)> = bid_map
            .into_iter()
            .map(|(k, q)| (k as f64 / scale, q))
            .collect();
        bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        // Asks: bucket UP — ask at 60100.05 in 1.0 bucket → 60101.0.
        let mut ask_map: std::collections::BTreeMap<i64, f64> = Default::default();
        for (p, q) in &self.asks {
            let key = (p * scale).ceil() as i64;
            *ask_map.entry(key).or_insert(0.0) += q;
        }
        let mut asks: Vec<(f64, f64)> = ask_map
            .into_iter()
            .map(|(k, q)| (k as f64 / scale, q))
            .collect();
        asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        AggregatedBook { bids, asks }
    }
}

/// Aggregated bid + ask side of the book at a given bucket size.
#[derive(Clone, Debug)]
pub struct AggregatedBook {
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

/// One event from `<symbol>@depth@100ms` — a list of price-level changes
/// since the last event.  `qty == 0` means the level is REMOVED.
#[derive(Clone, Debug, Default)]
pub struct DepthDiffEvent {
    pub first_update_id: u64, // U
    pub final_update_id: u64, // u
    /// `pu` — final update id of the PREVIOUS event.  Futures only; Spot
    /// diffs do not carry it.
    pub prev_final_id: Option<u64>,
    pub bids: Vec<(f64, f64)>, // (price, qty)
    pub asks: Vec<(f64, f64)>,
}

/// Full local copy of the order book, maintained by applying `DepthDiffEvent`s
/// to an initial REST snapshot.  Keys are scaled price * 1e8 (i64) so we get
/// stable ordering and exact equality (avoiding f64 BTreeMap pain).
#[derive(Clone, Debug)]
pub struct LocalBook {
    pub bids: std::collections::BTreeMap<i64, f64>,
    pub asks: std::collections::BTreeMap<i64, f64>,
    pub last_update_id: u64,
    pub price_scale: f64,
    /// `false` until the first diff after the snapshot has been applied
    /// (the first event follows different rules than the rest).
    synced: bool,
}

/// A hole in the diff sequence: the book can no longer be trusted and must
/// be re-seeded from a fresh snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// `u` of the last event applied (or the snapshot's `lastUpdateId`).
    pub last_applied: u64,
    /// `U` of the offending event.
    pub event_first: u64,
    /// `pu` of the offending event (Futures).
    pub event_prev: Option<u64>,
}

impl LocalBook {
    pub fn from_snapshot(snap: &OrderBookSnapshot, last_update_id: u64) -> Self {
        let scale: f64 = 1e8;
        let mut bids = std::collections::BTreeMap::new();
        let mut asks = std::collections::BTreeMap::new();
        for (p, q) in &snap.bids {
            if *q > 0.0 {
                bids.insert((p * scale).round() as i64, *q);
            }
        }
        for (p, q) in &snap.asks {
            if *q > 0.0 {
                asks.insert((p * scale).round() as i64, *q);
            }
        }
        LocalBook {
            bids,
            asks,
            last_update_id,
            price_scale: scale,
            synced: false,
        }
    }

    /// Apply one diff only if it continues the sequence.
    ///
    /// Binance rules (see the "How to manage a local order book correctly"
    /// sections of the Spot and USDⓈ-M Futures docs), with `last` = snapshot
    /// `lastUpdateId` or the previous event's `u`:
    ///   * events with `u` below the snapshot are stale → dropped (`Ok`);
    ///   * Spot, first event: `U <= last+1 <= u`; afterwards `U == last+1`;
    ///   * Futures, first event: `U <= last <= u`; afterwards `pu == last`.
    ///
    /// Anything else is a [`Gap`].
    pub fn apply_checked(&mut self, diff: &DepthDiffEvent, venue: Venue) -> Result<(), Gap> {
        let last = self.last_update_id;
        let gap = Gap {
            last_applied: last,
            event_first: diff.first_update_id,
            event_prev: diff.prev_final_id,
        };
        let stale = match venue {
            Venue::Spot => diff.final_update_id < last + 1,
            Venue::Futures => diff.final_update_id < last,
        };
        if !self.synced && stale {
            return Ok(());
        }
        let continues = match (venue, self.synced) {
            (Venue::Spot, false) => diff.first_update_id <= last + 1,
            (Venue::Spot, true) => diff.first_update_id == last + 1,
            (Venue::Futures, false) => diff.first_update_id <= last,
            (Venue::Futures, true) => diff.prev_final_id == Some(last),
        };
        if !continues {
            return Err(gap);
        }
        self.apply(diff);
        self.synced = true;
        Ok(())
    }

    /// Apply one diff: insert / update levels, remove those with qty 0.
    pub fn apply(&mut self, diff: &DepthDiffEvent) {
        for (p, q) in &diff.bids {
            let key = (p * self.price_scale).round() as i64;
            if *q <= 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, *q);
            }
        }
        for (p, q) in &diff.asks {
            let key = (p * self.price_scale).round() as i64;
            if *q <= 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, *q);
            }
        }
        self.last_update_id = diff.final_update_id;
    }

    /// Materialise the top `n` levels each side as an `OrderBookSnapshot`
    /// for rendering.  Bids descending, asks ascending.
    pub fn snapshot_top(&self, n: usize) -> OrderBookSnapshot {
        let bids: Vec<(f64, f64)> = self
            .bids
            .iter()
            .rev()
            .take(n)
            .map(|(k, q)| (*k as f64 / self.price_scale, *q))
            .collect();
        let asks: Vec<(f64, f64)> = self
            .asks
            .iter()
            .take(n)
            .map(|(k, q)| (*k as f64 / self.price_scale, *q))
            .collect();
        OrderBookSnapshot { bids, asks }
    }
}

/// Parse a Binance `<symbol>@depth@100ms` payload.
pub fn parse_depth_diff(json: &serde_json::Value) -> Option<DepthDiffEvent> {
    let first_update_id = json.get("U").and_then(|v| v.as_u64())?;
    let final_update_id = json.get("u").and_then(|v| v.as_u64())?;
    let parse_levels = |arr: &Vec<serde_json::Value>| -> Vec<(f64, f64)> {
        arr.iter()
            .filter_map(|row| {
                let row = row.as_array()?;
                let p = row.first()?.as_str()?.parse::<f64>().ok()?;
                let q = row.get(1)?.as_str()?.parse::<f64>().ok()?;
                Some((p, q))
            })
            .collect()
    };
    let bids = json
        .get("b")
        .and_then(|v| v.as_array())
        .map(parse_levels)
        .unwrap_or_default();
    let asks = json
        .get("a")
        .and_then(|v| v.as_array())
        .map(parse_levels)
        .unwrap_or_default();
    Some(DepthDiffEvent {
        first_update_id,
        final_update_id,
        prev_final_id: json.get("pu").and_then(|v| v.as_u64()),
        bids,
        asks,
    })
}

/// Parse a depth-snapshot REST payload into `(snapshot, lastUpdateId)`.
/// `lastUpdateId` is needed to sync incoming WS diff events.
pub fn parse_depth_with_id(json: &serde_json::Value) -> Option<(OrderBookSnapshot, u64)> {
    let last = json.get("lastUpdateId").and_then(|v| v.as_u64())?;
    let snap = parse_depth(json)?;
    Some((snap, last))
}

#[cfg(test)]
mod local_book_tests {
    use super::*;

    #[test]
    fn apply_diff_inserts_and_removes() {
        let snap = OrderBookSnapshot {
            bids: vec![(100.0, 5.0), (99.0, 4.0)],
            asks: vec![(101.0, 5.0), (102.0, 4.0)],
        };
        let mut book = LocalBook::from_snapshot(&snap, 1);
        let diff = DepthDiffEvent {
            first_update_id: 2,
            final_update_id: 3,
            prev_final_id: None,
            bids: vec![(99.0, 0.0), (98.5, 7.0)], // remove 99, add 98.5
            asks: vec![(101.0, 9.9)],             // update 101
        };
        book.apply(&diff);
        assert_eq!(book.last_update_id, 3);
        let top = book.snapshot_top(10);
        assert!((top.bids[0].0 - 100.0).abs() < 1e-9);
        assert!((top.bids[1].0 - 98.5).abs() < 1e-9);
        assert!((top.bids[1].1 - 7.0).abs() < 1e-9);
        assert!((top.asks[0].1 - 9.9).abs() < 1e-9);
    }
}

pub fn parse_depth(json: &serde_json::Value) -> Option<OrderBookSnapshot> {
    let bids_arr = json.get("bids").and_then(|v| v.as_array())?;
    let asks_arr = json.get("asks").and_then(|v| v.as_array())?;
    let parse_levels = |arr: &Vec<serde_json::Value>| -> Vec<(f64, f64)> {
        arr.iter()
            .filter_map(|row| {
                let row = row.as_array()?;
                let p = row.first()?.as_str()?.parse::<f64>().ok()?;
                let q = row.get(1)?.as_str()?.parse::<f64>().ok()?;
                if p > 0.0 && q >= 0.0 {
                    Some((p, q))
                } else {
                    None
                }
            })
            .collect()
    };
    let mut bids = parse_levels(bids_arr);
    let mut asks = parse_levels(asks_arr);
    bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Some(OrderBookSnapshot { bids, asks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_depth_payload() {
        let raw = r#"{"lastUpdateId":1,"bids":[["100.5","2.0"],["100.4","1.0"]],"asks":[["100.6","0.5"],["100.7","3.0"]]}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let s = parse_depth(&v).unwrap();
        assert_eq!(s.bids.len(), 2);
        assert_eq!(s.asks.len(), 2);
        assert!((s.best_bid().unwrap() - 100.5).abs() < 1e-9);
        assert!((s.best_ask().unwrap() - 100.6).abs() < 1e-9);
        assert!((s.spread().unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn empty_payload_returns_empty() {
        let raw = r#"{"bids":[],"asks":[]}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let s = parse_depth(&v).unwrap();
        assert!(s.bids.is_empty());
        assert!(s.asks.is_empty());
        assert!(s.best_bid().is_none());
    }
}

#[cfg(test)]
mod sequencing_tests {
    use super::*;

    fn book(last: u64) -> LocalBook {
        let snap = OrderBookSnapshot {
            bids: vec![(100.0, 1.0)],
            asks: vec![(101.0, 1.0)],
        };
        LocalBook::from_snapshot(&snap, last)
    }

    fn ev(u_first: u64, u_final: u64, pu: Option<u64>) -> DepthDiffEvent {
        DepthDiffEvent {
            first_update_id: u_first,
            final_update_id: u_final,
            prev_final_id: pu,
            bids: vec![(100.0, 2.0)],
            asks: vec![],
        }
    }

    /// Binance Spot docs: snapshot lastUpdateId = 1027024.
    #[test]
    fn spot_first_event_must_straddle_last_plus_one_then_be_contiguous() {
        let mut b = book(1_027_024);
        // Entirely before the snapshot → dropped, book untouched.
        assert!(b
            .apply_checked(&ev(1_027_000, 1_027_024, None), Venue::Spot)
            .is_ok());
        assert_eq!(b.last_update_id, 1_027_024);
        // U <= last+1 <= u → accepted.
        assert!(b
            .apply_checked(&ev(1_027_023, 1_027_026, None), Venue::Spot)
            .is_ok());
        assert_eq!(b.last_update_id, 1_027_026);
        // Contiguous: U == last+1.
        assert!(b
            .apply_checked(&ev(1_027_027, 1_027_030, None), Venue::Spot)
            .is_ok());
        // Hole: U > last+1.
        let gap = b
            .apply_checked(&ev(1_027_040, 1_027_045, None), Venue::Spot)
            .unwrap_err();
        assert_eq!(gap.last_applied, 1_027_030);
        assert_eq!(gap.event_first, 1_027_040);
        // Overlap after sync is also a violation on Spot (U must equal last+1).
        assert!(b
            .apply_checked(&ev(1_027_029, 1_027_035, None), Venue::Spot)
            .is_err());
    }

    #[test]
    fn spot_snapshot_too_old_is_a_gap() {
        let mut b = book(100);
        assert!(b.apply_checked(&ev(150, 160, None), Venue::Spot).is_err());
    }

    /// Binance USDⓈ-M Futures docs: first event U <= last <= u, then pu == last.
    #[test]
    fn futures_chain_on_pu_not_on_first_update_id() {
        let mut b = book(1_027_024);
        assert!(b
            .apply_checked(&ev(1_027_020, 1_027_024, Some(1_027_019)), Venue::Futures)
            .is_ok());
        assert_eq!(b.last_update_id, 1_027_024);
        // Futures batches routinely have U != last+1; pu == last is what counts.
        assert!(b
            .apply_checked(&ev(1_027_030, 1_027_040, Some(1_027_024)), Venue::Futures)
            .is_ok());
        assert_eq!(b.last_update_id, 1_027_040);
        assert!(b
            .apply_checked(&ev(1_027_041, 1_027_050, Some(1_027_040)), Venue::Futures)
            .is_ok());
        // pu does not match → gap.
        let gap = b
            .apply_checked(&ev(1_027_051, 1_027_060, Some(1_027_049)), Venue::Futures)
            .unwrap_err();
        assert_eq!(gap.event_prev, Some(1_027_049));
        // Missing pu on Futures is a gap too (never silently accepted).
        assert!(b
            .apply_checked(&ev(1_027_051, 1_027_060, None), Venue::Futures)
            .is_err());
    }

    #[test]
    fn futures_event_before_snapshot_is_dropped() {
        let mut b = book(500);
        assert!(b
            .apply_checked(&ev(400, 499, Some(399)), Venue::Futures)
            .is_ok());
        assert_eq!(b.last_update_id, 500);
    }

    #[test]
    fn parse_depth_diff_reads_pu() {
        let v =
            serde_json::json!({"e":"depthUpdate","U":10,"u":12,"pu":9,"b":[["1.0","2"]],"a":[]});
        let d = parse_depth_diff(&v).unwrap();
        assert_eq!(
            (d.first_update_id, d.final_update_id, d.prev_final_id),
            (10, 12, Some(9))
        );
        let spot = serde_json::json!({"U":10,"u":12,"b":[],"a":[]});
        assert_eq!(parse_depth_diff(&spot).unwrap().prev_final_id, None);
    }
}
