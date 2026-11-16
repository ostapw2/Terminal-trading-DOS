//! Tick → bar aggregation.  Live there are only price ticks; strategies that
//! work on closed bars (SMA Cross, RSI Reversion, Breakout) get theirs from
//! here, so they behave the same live, in a forward test and in a backtest.

use crate::strategies::Bar;

/// Closes one bar per `interval` of wall-clock time, aligned to the epoch
/// (a 60 s builder closes bars on the minute).
pub struct BarBuilder {
    interval_ms: i64,
    bucket: i64,
    cur: Option<Bar>,
}

impl BarBuilder {
    pub fn new(interval_secs: i64) -> Self {
        Self {
            interval_ms: interval_secs.max(1) * 1000,
            bucket: i64::MIN,
            cur: None,
        }
    }

    pub fn interval_secs(&self) -> i64 {
        self.interval_ms / 1000
    }

    /// Feed one price.  Returns the bar that just CLOSED, if this tick opened
    /// a new interval.  Ticks that do not move time forward only extend the
    /// open bar; non-positive or non-finite prices are ignored.
    pub fn push(&mut self, ts_ms: i64, price: f64) -> Option<Bar> {
        if !(price.is_finite() && price > 0.0) {
            return None;
        }
        let bucket = ts_ms.div_euclid(self.interval_ms);
        match &mut self.cur {
            Some(bar) if bucket <= self.bucket => {
                bar.high = bar.high.max(price);
                bar.low = bar.low.min(price);
                bar.close = price;
                None
            }
            _ => {
                let closed = self.cur.take();
                self.bucket = bucket;
                self.cur = Some(Bar {
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: 0.0,
                    time_ms: bucket * self.interval_ms,
                });
                closed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_closes_when_the_next_interval_starts_and_tracks_ohlc() {
        let mut b = BarBuilder::new(60);
        assert!(b.push(1_000, 100.0).is_none());
        assert!(b.push(20_000, 105.0).is_none());
        assert!(b.push(40_000, 98.0).is_none());
        assert!(b.push(59_000, 101.0).is_none());
        let bar = b
            .push(61_000, 102.0)
            .expect("new minute closes the old bar");
        assert_eq!(
            (bar.open, bar.high, bar.low, bar.close),
            (100.0, 105.0, 98.0, 101.0)
        );
        assert_eq!(bar.time_ms, 0, "stamped with the interval start");
        let next = b.push(121_000, 90.0).unwrap();
        assert_eq!(
            (next.open, next.close, next.time_ms),
            (102.0, 102.0, 60_000)
        );
    }

    #[test]
    fn skipped_intervals_make_no_filler_bars_and_bad_prices_are_ignored() {
        let mut b = BarBuilder::new(60);
        b.push(0, 100.0);
        // Nothing for 10 minutes, then a tick: exactly one closed bar, not ten.
        let bar = b.push(600_000, 110.0).unwrap();
        assert_eq!(bar.close, 100.0);
        assert!(b.push(600_500, f64::NAN).is_none());
        assert!(b.push(600_600, -1.0).is_none());
        assert!(b.push(600_700, 0.0).is_none());
        let after = b.push(660_000, 111.0).unwrap();
        assert_eq!(
            (after.open, after.close),
            (110.0, 110.0),
            "bad ticks left no trace"
        );
    }

    #[test]
    fn out_of_order_ticks_extend_the_open_bar_instead_of_reopening() {
        let mut b = BarBuilder::new(60);
        b.push(70_000, 100.0);
        assert!(
            b.push(65_000, 120.0).is_none(),
            "older timestamp: same open bar"
        );
        let bar = b.push(130_000, 99.0).unwrap();
        assert_eq!((bar.high, bar.close), (120.0, 120.0));
        assert_eq!(
            BarBuilder::new(0).interval_secs(),
            1,
            "interval is at least a second"
        );
    }
}
