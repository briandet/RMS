//! Where a Cast device's playback is, on the driver's own clock, from the positions its
//! `MEDIA_STATUS` messages report: what lets a Chromecast keep time with the players it is
//! synced with (see [`super::chromecast`]).
//!
//! A status says the media was at `currentTime` when the device answered, some time
//! between the request and the answer reaching us: its middle is the best guess, and
//! answers that took long are left out.  The position is a line (a start point and the
//! playback rate), and each status gives its offset from that line; the position is the
//! line moved by the median of the recent offsets, so a late answer now and then doesn't
//! move it.  A change of rate starts a new line from where the position is; a status far
//! off the line (a seek, a stall) starts one from that status.

use std::collections::VecDeque;
use std::time::Instant;

/// Statuses whose answer took longer than this (seconds) say too little to keep.
const MAX_ROUND_TRIP: f64 = 0.25;
/// A status this far (seconds) off the line is a jump: the line starts again from it.
const JUMP: f64 = 0.4;
/// Offsets kept for the median.
const WINDOW: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Line {
    /// Seconds of the clock.
    t: f64,
    /// Media position at `t`.
    pos: f64,
    /// Media seconds a second: 0 while paused.
    rate: f64,
}

impl Line {
    fn at(&self, t: f64) -> f64 {
        self.pos + self.rate * (t - self.t)
    }
}

pub struct CastClock {
    base: Instant,
    line: Option<Line>,
    offsets: VecDeque<f64>,
}

impl CastClock {
    pub fn new() -> CastClock {
        CastClock {
            base: Instant::now(),
            line: None,
            offsets: VecDeque::new(),
        }
    }

    /// Seconds of the clock at `t`.
    pub fn secs(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.base).as_secs_f64()
    }

    /// The clock in milliseconds (wrapping), as players report their jiffies.
    pub fn jiffies(&self, t: Instant) -> u32 {
        (self.secs(t) * 1000.0) as u64 as u32
    }

    /// The moment the clock reads `jiffies`, given that it reads `now_jiffies` at `now`
    /// (it wraps, so the nearest such moment).
    pub fn instant_of(jiffies: u32, now_jiffies: u32, now: Instant) -> Instant {
        let ahead = jiffies.wrapping_sub(now_jiffies) as i32 as i64;
        if ahead >= 0 {
            now + std::time::Duration::from_millis(ahead as u64)
        } else {
            now.checked_sub(std::time::Duration::from_millis((-ahead) as u64))
                .unwrap_or(now)
        }
    }

    /// Forget the position (a new track).
    pub fn reset(&mut self) {
        self.line = None;
        self.offsets.clear();
    }

    /// A status: the media at `pos`, playing at `rate` (0: paused), asked at `sent` and
    /// answered at `received`.
    pub fn observe(&mut self, pos: f64, rate: f64, sent: f64, received: f64) {
        let round_trip = (received - sent).max(0.0);
        let at = (sent + received) / 2.0;
        let fresh = Line { t: at, pos, rate };
        let Some(line) = self.line else {
            self.line = Some(fresh);
            self.offsets.clear();
            return;
        };
        if rate != line.rate {
            // paused, resumed, or a new rate: from the status
            self.line = Some(fresh);
            self.offsets.clear();
            return;
        }
        let offset = pos - line.at(at);
        if (offset - self.median()).abs() > JUMP {
            self.line = Some(fresh);
            self.offsets.clear();
            return;
        }
        if round_trip > MAX_ROUND_TRIP {
            return;
        }
        self.offsets.push_back(offset);
        if self.offsets.len() > WINDOW {
            self.offsets.pop_front();
        }
    }

    /// The device was told to play at `rate` from `t`: the position goes on from there at
    /// that rate.
    pub fn set_rate(&mut self, rate: f64, t: f64) {
        if let Some(pos) = self.position(t) {
            self.line = Some(Line { t, pos, rate });
            self.offsets.clear();
        }
    }

    fn median(&self) -> f64 {
        if self.offsets.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f64> = self.offsets.iter().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    }

    /// The media position at `t`, once a status has said.
    pub fn position(&self, t: f64) -> Option<f64> {
        self.line.map(|l| (l.at(t) + self.median()).max(0.0))
    }

    /// When (seconds of the clock) the media was at position 0, playing as it does now;
    /// `None` while paused or unknown.
    pub fn origin(&self) -> Option<f64> {
        let l = self.line?;
        (l.rate > 0.0).then(|| l.t - (l.pos + self.median()) / l.rate)
    }

    /// Statuses the position rests on (beyond the first of its line).
    pub fn samples(&self) -> usize {
        self.offsets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn late_answers_now_and_then_dont_move_the_position() {
        let mut c = CastClock::new();
        // the media started at clock 10.0; answers take 20 ms, every third one 200 ms more
        // on the way back (reported as if the status were taken in the middle: 100 ms off)
        for i in 0..12 {
            let sent = 11.0 + i as f64;
            let late = if i % 3 == 2 { 0.2 } else { 0.0 };
            let truth_at_answer = sent + 0.01 - 10.0;
            c.observe(truth_at_answer, 1.0, sent, sent + 0.02 + late);
        }
        let p = c.position(30.0).unwrap();
        assert!(close(p, 20.0, 0.005), "{p}");
        assert!(close(c.origin().unwrap(), 10.0, 0.005));
    }

    #[test]
    fn slow_answers_are_left_out() {
        let mut c = CastClock::new();
        c.observe(1.0, 1.0, 10.0, 10.0);
        // half a second there and back: it could have been taken any time in between
        c.observe(1.9, 1.0, 10.5, 11.0);
        assert_eq!(c.samples(), 0);
        assert!(close(c.position(11.0).unwrap(), 2.0, 1e-9));
    }

    #[test]
    fn a_new_rate_goes_on_from_where_it_was() {
        let mut c = CastClock::new();
        c.observe(5.0, 1.0, 20.0, 20.0);
        c.set_rate(1.05, 22.0);
        assert!(close(c.position(22.0).unwrap(), 7.0, 1e-9));
        assert!(close(c.position(26.0).unwrap(), 7.0 + 4.0 * 1.05, 1e-9));
        // the device's own statuses at that rate fit
        c.observe(7.0 + 6.0 * 1.05, 1.05, 28.0, 28.0);
        assert_eq!(c.samples(), 1);
    }

    #[test]
    fn a_seek_or_a_stall_starts_again_from_the_status() {
        let mut c = CastClock::new();
        for i in 0..5 {
            let t = 1.0 + i as f64;
            c.observe(t, 1.0, t, t);
        }
        // a second behind: it stalled
        c.observe(5.0, 1.0, 6.0, 6.0);
        assert!(close(c.position(6.0).unwrap(), 5.0, 1e-9));
        // paused: the position stays
        c.observe(5.5, 0.0, 6.5, 6.5);
        assert!(close(c.position(9.0).unwrap(), 5.5, 1e-9));
        assert!(c.origin().is_none());
    }

    #[test]
    fn jiffies_map_to_moments_both_ways() {
        let now = Instant::now();
        let at = CastClock::instant_of(1_500, 1_000, now);
        assert_eq!(at.duration_since(now).as_millis(), 500);
        // across the wrap
        let at = CastClock::instant_of(100, u32::MAX - 99, now);
        assert_eq!(at.duration_since(now).as_millis(), 200);
        let at = CastClock::instant_of(900, 1_000, now);
        assert!(at <= now);
    }
}
