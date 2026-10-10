//! List scrolling with acceleration while a button is held
//! (`Slim::Buttons::Common::scroll_dynamic`, button part).

/// Items per second before acceleration.
const MINIMUM_VELOCITY: f64 = 2.0;
/// Hold time before automatic scrolling starts.
const HOLD_TIME_BEFORE_SCROLL: f64 = 0.300;
/// Seconds to traverse the estimated target range.
const TC: f64 = 5.0;

#[derive(Debug, Clone, Default)]
pub struct ScrollParams {
    estimate_start: f64,
    estimate_end: f64,
    v: f64,
    a: f64,
    last_hold_time: f64,
    last_position: f64,
    last_position_returned: usize,
    /// Length of the list the estimates were made for.
    len: usize,
}

impl ScrollParams {
    fn reset_range(&mut self, pos: f64, len: usize) {
        let delta = self.estimate_end - self.estimate_start + 1.0;
        if pos > self.estimate_end {
            self.estimate_end = (self.estimate_end + delta).min(len as f64 - 1.0);
        } else if pos < self.estimate_start {
            self.estimate_start = (self.estimate_start - delta).max(0.0);
        }
    }

    /// New position after moving `direction` (±n) from `current` in a list of `len`, given
    /// how long the button has been held.  Single presses wrap around the ends.
    pub fn scroll(&mut self, direction: i64, len: usize, current: usize, hold_time: f64) -> usize {
        if len == 0 {
            return 0;
        }
        if len != self.len {
            // `scroll_getInitialScrollParams`: the target is anywhere in the list
            *self = ScrollParams {
                estimate_end: len as f64,
                v: MINIMUM_VELOCITY * (direction as f64).signum(),
                last_hold_time: -1.0,
                len,
                ..Default::default()
            };
        }
        let dir = direction as f64;
        let result: i64;
        if hold_time == 0.0 {
            let mut cur = current as i64;
            if current == len - 1 && direction > 0 {
                cur = -1;
                self.estimate_start = 0.0;
                self.estimate_end = len as f64 - 1.0;
            } else if current == 0 && direction < 0 {
                cur = len as i64;
                self.estimate_start = 0.0;
                self.estimate_end = len as f64 - 1.0;
            }
            self.last_hold_time = 0.0;
            self.v = MINIMUM_VELOCITY * dir.signum();
            self.a = 0.0;
            result = cur + direction;
            if direction > 0 {
                self.estimate_start = result as f64;
                if self.estimate_end < self.estimate_start {
                    self.estimate_end = self.estimate_start + 1.0;
                }
            } else {
                self.estimate_end = result as f64;
                if self.estimate_start > self.estimate_end {
                    self.estimate_start = self.estimate_end - 1.0;
                }
            }
            self.reset_range(result as f64, len);
            self.last_position = result as f64;
        } else if hold_time < HOLD_TIME_BEFORE_SCROLL {
            result = current as i64;
        } else {
            self.a = 2.0 * (self.estimate_end - self.estimate_start) / (TC * TC) * dir.signum();
            let t = hold_time - self.last_hold_time;
            let velocity = self.a * t + self.v;
            let pos = if self.last_position_returned == current {
                self.last_position
            } else {
                current as f64
            };
            let x = 0.5 * self.a * t * t + self.v * t + pos;
            self.last_position = x;
            result = (0.5 + x).floor() as i64;
            self.reset_range(result as f64, len);
            self.v = velocity;
            self.last_hold_time = hold_time;
        }
        let r = result.clamp(0, len as i64 - 1) as usize;
        self.last_position_returned = r;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presses_step_and_wrap() {
        let mut s = ScrollParams::default();
        assert_eq!(s.scroll(1, 5, 0, 0.0), 1);
        assert_eq!(s.scroll(1, 5, 4, 0.0), 0);
        assert_eq!(s.scroll(-1, 5, 0, 0.0), 4);
        assert_eq!(s.scroll(-1, 5, 3, 0.0), 2);
    }

    #[test]
    fn holding_accelerates() {
        let mut s = ScrollParams::default();
        let mut pos = s.scroll(1, 1000, 0, 0.0);
        let mut t = 0.0;
        let mut steps = vec![];
        while t < 4.0 {
            t += 0.108;
            let n = s.scroll(1, 1000, pos, t);
            steps.push(n - pos);
            pos = n;
        }
        assert!(pos > 100, "moved {pos}");
        let early: usize = steps[3..8].iter().sum();
        let late: usize = steps[steps.len() - 5..].iter().sum();
        assert!(late > early);
        // holding doesn't wrap
        assert!(s.scroll(1, 1000, 999, 5.0) == 999);
    }
}
