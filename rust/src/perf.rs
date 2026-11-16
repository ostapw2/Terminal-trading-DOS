//! Frame timing: how long a frame takes to build and send, and how long an
//! input or a price tick waits before it is on screen.
//!
//! `:perf` shows a summary in the status line; `DOS_PERF=<file>` also writes
//! one CSV line per frame (`ms_since_start,draw_us,flush_us,input_us,tick_us`)
//! for offline study.  Waits are measured from the moment the app sees the
//! event (an input read from the terminal, a tick taken off its channel) to
//! the end of the frame that shows it; the terminal's own drawing comes on
//! top and is not visible from here.

use std::collections::VecDeque;
use std::io::Write;
use std::time::{Duration, Instant};

/// Frames kept for the summary.
const KEEP: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub draw: Duration,
    pub flush: Duration,
    /// Oldest input this frame answered: how long it waited.
    pub input: Option<Duration>,
    /// Oldest price tick this frame showed: how long it waited.
    pub tick: Option<Duration>,
}

pub struct Perf {
    started: Instant,
    frames: VecDeque<Frame>,
    input_at: Option<Instant>,
    tick_at: Option<Instant>,
    log: Option<std::fs::File>,
}

impl Default for Perf {
    fn default() -> Self {
        Self::new()
    }
}

impl Perf {
    pub fn new() -> Self {
        let log = std::env::var_os("DOS_PERF")
            .filter(|p| !p.is_empty())
            .and_then(|p| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
            });
        Self {
            started: Instant::now(),
            frames: VecDeque::new(),
            input_at: None,
            tick_at: None,
            log,
        }
    }

    /// An input was read; the next finished frame answers it.
    pub fn input(&mut self) {
        self.input_at.get_or_insert_with(Instant::now);
    }

    /// A price tick changed what is shown.
    pub fn tick(&mut self) {
        self.tick_at.get_or_insert_with(Instant::now);
    }

    /// Is a tick waiting to be drawn?
    pub fn tick_waiting(&self) -> bool {
        self.tick_at.is_some()
    }

    /// A frame was built (`draw`) and sent (`flush`), ending now.
    pub fn frame(&mut self, draw: Duration, flush: Duration) {
        let now = Instant::now();
        let f = Frame {
            draw,
            flush,
            input: self.input_at.take().map(|t| now - t),
            tick: self.tick_at.take().map(|t| now - t),
        };
        if let Some(log) = self.log.as_mut() {
            let us = |d: Option<Duration>| d.map_or(String::new(), |d| d.as_micros().to_string());
            let _ = writeln!(
                log,
                "{},{},{},{},{}",
                self.started.elapsed().as_millis(),
                f.draw.as_micros(),
                f.flush.as_micros(),
                us(f.input),
                us(f.tick)
            );
        }
        if self.frames.len() == KEEP {
            self.frames.pop_front();
        }
        self.frames.push_back(f);
    }

    /// `frame 9.1/14 ms (p50/p95) | input 10/16 | tick 12/49 | 512 frames`.
    pub fn summary(&self) -> String {
        let pick = |get: fn(&Frame) -> Option<Duration>| {
            let mut v: Vec<Duration> = self.frames.iter().filter_map(get).collect();
            v.sort();
            if v.is_empty() {
                return "-".to_string();
            }
            let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1000.0;
            format!("{:.1}/{:.1}", at(0.5), at(0.95))
        };
        format!(
            "frame {} ms (p50/p95) | input {} | tick {} | flush {} | {} frames",
            pick(|f| Some(f.draw + f.flush)),
            pick(|f| f.input),
            pick(|f| f.tick),
            pick(|f| Some(f.flush)),
            self.frames.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_are_measured_once_per_frame_and_summarised() {
        let mut p = Perf::new();
        assert!(p.summary().contains("input -"));
        p.input();
        p.tick();
        assert!(p.tick_waiting());
        std::thread::sleep(Duration::from_millis(2));
        p.input(); // a second input keeps the oldest time
        p.frame(Duration::from_millis(5), Duration::from_millis(1));
        let f = p.frames[0];
        assert!(f.input.unwrap() >= Duration::from_millis(2));
        assert!(f.tick.is_some() && !p.tick_waiting());
        p.frame(Duration::from_millis(7), Duration::ZERO);
        assert_eq!(
            p.frames[1].input, None,
            "nothing waited for the second frame"
        );
        let s = p.summary();
        assert!(s.contains("frame 7.0/7.0") && s.contains("2 frames"), "{s}");
    }
}
