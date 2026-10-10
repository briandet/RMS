//! `INPUT.Time`: entering a time of day with the remote.  The cursor sits on the hours,
//! the minutes, AM/PM (12-hour formats) and a final arrow; up/down change the field under
//! the cursor, left/right move it (past either end leaves, saving), and number keys type
//! the digits.

use super::{menu, Mode, Transition, Ui};
use crate::display::Parts;
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub(super) struct TimeInput {
    pub header: String,
    /// Seconds after midnight.
    pub value: u32,
    pub cursor: usize,
    ampm: bool,
    /// Digit typed for the first half of a field (`lastLetterDigit`).
    last_digit: Option<u32>,
    /// When to give up on that digit; `true` = and move to the next field.
    pending: Option<(Instant, bool)>,
}

/// `splitTime` + `timeDigits`: (h0, h1, m0, m1, pm).
fn digits(value: u32, ampm: bool) -> (u32, u32, u32, u32, Option<bool>) {
    let mut h = value / 3600 % 24;
    let m = value / 60 % 60;
    let mut p = None;
    if ampm {
        p = Some(h > 11);
        if h > 11 {
            h -= 12;
        }
        if h == 0 {
            h = 12;
        }
    }
    (h / 10, h % 10, m / 10, m % 10, p)
}

/// `timeDigitsToTime`.
fn from_digits(h0: u32, h1: u32, m0: u32, m1: u32, p: Option<bool>) -> u32 {
    let mut h = h0 * 10 + h1;
    let mut pm = 0;
    if let Some(is_pm) = p {
        pm = is_pm as u32;
        if h == 12 {
            if is_pm {
                pm = 0;
            } else {
                h = 0;
            }
        }
    }
    ((pm * 12 + h) * 60 + m0 * 10 + m1) * 60
}

impl TimeInput {
    pub fn new(header: String, value: u32, time_format: &str) -> TimeInput {
        TimeInput {
            header,
            value,
            cursor: 0,
            ampm: time_format.contains("%p") || time_format.contains("%I"),
            last_digit: None,
            pending: None,
        }
    }

    fn last_pos(&self) -> usize {
        if self.ampm {
            3
        } else {
            2
        }
    }

    /// `timeString` with the cursor before the character it marks.
    pub fn lines(&self) -> Parts {
        let (h0, h1, m0, m1, p) = digits(self.value, self.ampm);
        let c = self.cursor;
        let cs = menu::CURSORPOS;
        let mut t = String::new();
        if !(h0 == 0 && self.ampm) {
            t.push_str(&h0.to_string());
        }
        if c == 0 {
            t.push_str(cs);
        }
        t.push_str(&format!("{h1}:{m0}"));
        if c == 1 {
            t.push_str(cs);
        }
        t.push_str(&m1.to_string());
        if let Some(pm) = p {
            let ap = if pm { "PM" } else { "AM" };
            if c == 2 {
                t.push_str(&format!(" {}{}{}", &ap[..1], cs, &ap[1..]));
            } else {
                t.push_str(&format!(" {ap}"));
            }
        }
        if (c == 2 && !self.ampm) || c == 3 {
            t.push_str(menu::RIGHTARROW);
        }
        Parts {
            line: [self.header.clone(), t],
            ..Default::default()
        }
    }
}

/// Result of a key in the time editor.
pub(super) enum TimeExit {
    Stay,
    /// Left past the first field or right past the last: save and leave.
    Leave,
}

impl Ui {
    fn time_input(&mut self) -> Option<&mut TimeInput> {
        match self.stack.last_mut() {
            Some(Mode::Time(t)) => Some(t),
            _ => None,
        }
    }

    /// `moveCursor`.
    fn time_move(&mut self, inc: i64) -> TimeExit {
        let Some(t) = self.time_input() else {
            return TimeExit::Stay;
        };
        let pos = t.cursor as i64 + inc;
        if pos < 0 {
            return if t.cursor == 0 {
                TimeExit::Leave
            } else {
                TimeExit::Stay
            };
        }
        if pos as usize > t.last_pos() {
            return TimeExit::Leave;
        }
        t.cursor = pos as usize;
        TimeExit::Stay
    }

    /// `scroll` / `scrollTime`: change the field under the cursor.
    fn time_scroll(&mut self, dir: i64) {
        let hold = self.ir.hold_time();
        let Some(t) = self.time_input() else { return };
        let (c, ampm) = (t.cursor, t.ampm);
        if c == t.last_pos() {
            return;
        }
        let mut h = (t.value / 3600 % 24) as usize;
        let mut m = (t.value / 60 % 60) as usize;
        match c {
            0 => h = self.scroll.scroll(dir, 24, h, hold),
            1 => m = self.scroll.scroll(dir, 60, m, hold),
            2 if ampm => h = (h + 12) % 24,
            _ => {}
        }
        if let Some(t) = self.time_input() {
            t.value = ((h * 60 + m) * 60) as u32;
        }
    }

    /// `numberButton`: typing a time with the number keys.
    fn time_number(&mut self, digit: u32) -> TimeExit {
        let Some(t) = self.time_input() else {
            return TimeExit::Stay;
        };
        let (mut h0, mut h1, mut m0, mut m1, mut p) = digits(t.value, t.ampm);
        let ampm = t.ampm;
        let c = t.cursor;
        if (c == 2 && !ampm) || c > 2 {
            return TimeExit::Stay;
        }
        let soon = Instant::now() + Duration::from_secs_f64(super::DISPLAY_TEXT_TIMEOUT);
        let mut advance = false;
        let mut pass_on = None;
        match c {
            0 => match t.last_digit.take() {
                Some(d0) => {
                    h0 = d0;
                    let max = match h0 {
                        0 => 10,
                        1 if ampm => 3,
                        1 => 10,
                        2 => 4,
                        _ => 0,
                    };
                    if digit < max {
                        h1 = digit;
                    } else {
                        h1 = h0;
                        h0 = 0;
                        pass_on = Some(digit);
                    }
                    t.pending = None;
                    advance = true;
                }
                None => {
                    if !ampm || digit != 0 {
                        h0 = 0;
                        h1 = digit;
                        if (ampm && digit > 1) || digit > 2 {
                            advance = true;
                        } else {
                            t.last_digit = Some(digit);
                            t.pending = Some((soon, true));
                        }
                    } else {
                        t.last_digit = Some(digit);
                        t.pending = Some((soon, false));
                    }
                }
            },
            1 => match t.last_digit.take() {
                Some(d0) => {
                    m0 = d0;
                    m1 = digit;
                    t.pending = None;
                    advance = true;
                }
                None => {
                    m0 = 0;
                    m1 = digit;
                    if digit > 5 {
                        advance = true;
                    } else {
                        t.last_digit = Some(digit);
                        t.pending = Some((soon, true));
                    }
                }
            },
            _ => {
                if digit == 2 || digit == 7 {
                    p = Some(digit == 7);
                    advance = true;
                }
            }
        }
        t.value = from_digits(h0, h1, m0, m1, p);
        if advance {
            if let TimeExit::Leave = self.time_move(1) {
                return TimeExit::Leave;
            }
        }
        match pass_on {
            Some(d) => self.time_number(d),
            None => TimeExit::Stay,
        }
    }

    /// The typed digit timed out: forget it, moving on if it was a complete field.
    pub(super) fn time_timer(&mut self) -> Option<Instant> {
        match self.stack.last() {
            Some(Mode::Time(t)) => t.pending.map(|p| p.0),
            _ => None,
        }
    }

    pub(super) async fn time_fire(&mut self) {
        let Some(t) = self.time_input() else { return };
        let Some((at, next)) = t.pending else { return };
        if at > Instant::now() {
            return;
        }
        t.pending = None;
        t.last_digit = None;
        if next {
            if let TimeExit::Leave = self.time_move(1) {
                self.time_leave().await;
            }
        }
    }

    /// Leave the editor, handing the time to whoever opened it (`timeExitHandler`).
    async fn time_leave(&mut self) {
        let Some(Mode::Time(t)) = self.stack.pop() else {
            return;
        };
        self.transition = Some(Transition::PushRight);
        self.alarm_time_saved(t.value).await;
    }

    pub(super) async fn time_function(&mut self, name: &str, arg: Option<&str>) -> bool {
        let exit = match name {
            "up" => {
                self.time_scroll(1);
                TimeExit::Stay
            }
            "down" => {
                self.time_scroll(-1);
                TimeExit::Stay
            }
            "left" => self.time_move(-1),
            "right" => self.time_move(1),
            "numberLetter" => self.time_number(arg.and_then(|a| a.parse().ok()).unwrap_or(0)),
            // play / add / search: nothing to do here
            "exit" => TimeExit::Stay,
            "passback" => {
                self.passback().await;
                TimeExit::Stay
            }
            _ => return false,
        };
        if let TimeExit::Leave = exit {
            self.time_leave().await;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_round_trip() {
        for v in [0, 7 * 3600 + 30 * 60, 12 * 3600, 23 * 3600 + 59 * 60] {
            for ampm in [false, true] {
                let (a, b, c, d, p) = digits(v, ampm);
                assert_eq!(from_digits(a, b, c, d, p), v, "{v} {ampm}");
            }
        }
        assert_eq!(digits(0, true), (1, 2, 0, 0, Some(false)));
        assert_eq!(digits(13 * 3600 + 5 * 60, false), (1, 3, 0, 5, None));
    }

    #[test]
    fn time_string_with_cursor() {
        let mut t = TimeInput::new("Set Time".into(), 7 * 3600 + 30 * 60, "|%I:%M %p");
        assert_eq!(t.lines().line[1], "\x0a7:30 AM");
        t.cursor = 1;
        assert_eq!(t.lines().line[1], "7:3\x0a0 AM");
        t.cursor = 2;
        assert_eq!(t.lines().line[1], "7:30 A\x0aM");
        t.cursor = 3;
        assert_eq!(t.lines().line[1], "7:30 AM\x02");
        let t = TimeInput::new("x".into(), 19 * 3600, "%H:%M");
        assert_eq!(t.lines().line[1], "1\x0a9:00");
    }
}
