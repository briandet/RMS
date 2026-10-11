//! IR remote timing (`Slim::Hardware::IR::processIR` and friends): turns the stream of IR
//! codes a player sends while a button is down into button events with Lyrion's press
//! styles: the plain first press, `.repeat` while held, `.hold` after 0.9 s, and `.single`
//! / `.double` / `.hold_release` once released.  Front panel buttons report `.down` / `.up`.

use super::irmap::maps;
use tokio::time::{Duration, Instant};

/// Codes closer than this belong to the same press (seconds).
pub const IRMINTIME: f64 = 0.140;
/// Hold time for `.hold` events.
pub const IRHOLDTIME: f64 = 0.9;
/// A press released within this time is a `.single`.
pub const IRSINGLETIME: f64 = 0.256;

/// Player clock wrap (`0xffffffff / ticspersec`, 1 kHz ticks).
const WRAP: f64 = 0xffff_ffffu32 as f64 / 1000.0;

#[derive(Debug, Clone)]
enum TimerKind {
    CheckRelease {
        release_type: &'static str,
        start_ir_time: f64,
        start_code: u32,
        est_ir_time: f64,
        orig_mode: String,
    },
    FireHold {
        button: String,
        start_code: u32,
    },
}

#[derive(Debug, Clone)]
struct Timer {
    at: Instant,
    kind: TimerKind,
}

/// Per-player IR state.
#[derive(Debug, Default)]
pub struct IrState {
    /// Player clock (seconds) of the last code; 0 after a non-IR button.
    pub last_ir_time: f64,
    last_code: u32,
    start_ir_hold: f64,
    ir_repeat_time: f64,
    pub ir_time_diff: f64,
    /// Last button with press style (`lastirbutton`), for passback.
    pub last_button: String,
    timers: Vec<Timer>,
}

fn wrapped(mut d: f64) -> f64 {
    if d < 0.0 {
        d += WRAP;
    }
    d
}

impl IrState {
    /// `holdTime`: how long the current button has been held.
    pub fn hold_time(&self) -> f64 {
        if self.last_ir_time == 0.0 {
            return 0.0;
        }
        wrapped(self.last_ir_time - self.start_ir_hold)
    }

    /// `repeatCount`: repetitions due since the last code while holding, at `maxrate` per
    /// second after accelerating at `accel` per second².
    pub fn repeat_count(&self, maxrate: f64, accel: f64) -> i64 {
        let hold = self.hold_time();
        if hold == 0.0 {
            return 0;
        }
        let diff = self.ir_time_diff;
        let acc = |t: f64| 0.5 * accel * t * t;
        if accel > 0.0 {
            if maxrate > 0.0 && maxrate < hold * accel {
                let flat = hold - maxrate / accel;
                (maxrate * flat) as i64 - (maxrate * (flat - diff)) as i64
            } else {
                acc(hold) as i64 - acc(hold - diff) as i64
            }
        } else {
            (maxrate * hold) as i64 - (maxrate * (hold - diff)) as i64
        }
    }

    /// When the next timer is due.
    pub fn next_timer(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.at).min()
    }

    fn button_with(code: u32, style: &str) -> Option<String> {
        maps().button(code).map(|b| {
            if style.is_empty() {
                b.to_string()
            } else {
                format!("{b}.{style}")
            }
        })
    }

    /// Process an IR code received at player time `ir_time` (1 kHz ticks) in `mode`.
    /// Returns the button events to execute, in order; `Err(())` for unknown codes.
    pub fn process(&mut self, code: u32, ir_ticks: u32, mode: &str) -> Result<Vec<String>, ()> {
        let ir_time = ir_ticks as f64 / 1000.0;
        if code == 0 {
            return Ok(vec![]);
        }
        let Some(button) = maps().button(code).map(str::to_string) else {
            return Err(());
        };
        let mut out = Vec::new();
        let timediff = wrapped(ir_time - self.last_ir_time);
        let front_panel = button.ends_with(".up") || button.ends_with(".down");
        if !front_panel && timediff < IRMINTIME && code != self.last_code {
            // stray code in the middle of a repeat sequence
            return Ok(out);
        }
        if timediff == 0.0 {
            return Ok(out);
        }
        self.ir_time_diff = timediff;
        self.last_ir_time = ir_time;

        if front_panel {
            let (base, dir) = button.rsplit_once('.').unwrap();
            let dir = if dir == "down" && code == self.last_code {
                "repeat"
            } else {
                dir
            };
            self.last_code = code;
            self.ir_repeat_time = 0.0;
            self.front_panel(base, dir, ir_time, &mut out);
        } else if button.starts_with("knob") {
            // Boom knob
            if code == self.last_code && timediff < 0.5 {
                out.push(format!("{button}.repeat"));
            } else {
                self.last_code = code;
                self.ir_repeat_time = 0.0;
                out.push(button.clone());
            }
            self.start_ir_hold = ir_time;
            self.last_button = out.last().cloned().unwrap_or_default();
        } else if code == self.last_code
            && (self.ir_time_diff < IRMINTIME
                || (self.ir_time_diff < self.ir_repeat_time * 2.02
                    && self.ir_time_diff > self.ir_repeat_time * 1.98))
        {
            // holdCode + repeatCode
            if self.hold_time() >= IRHOLDTIME && self.hold_time() - IRHOLDTIME < self.ir_time_diff {
                out.extend(Self::button_with(code, "hold"));
            }
            out.extend(Self::button_with(code, "repeat"));
            if self.ir_repeat_time == 0.0
                || (self.ir_time_diff > 0.0 && self.ir_time_diff < self.ir_repeat_time)
            {
                self.ir_repeat_time = self.ir_time_diff;
            }
        } else {
            self.start_ir_hold = ir_time;
            self.last_code = code;
            self.ir_repeat_time = 0.0;
            // `firePendingTimer` for the previous press, then time this one
            let pending = self
                .timers
                .iter()
                .position(|t| matches!(t.kind, TimerKind::CheckRelease { .. }));
            let rescheduled = match pending {
                Some(i) => {
                    let t = self.timers.remove(i);
                    self.check_release(t.kind, mode, &mut out)
                }
                None => false,
            };
            if !rescheduled {
                self.timers.push(Timer {
                    at: Instant::now() + Duration::from_secs_f64(IRSINGLETIME),
                    kind: TimerKind::CheckRelease {
                        release_type: "single",
                        start_ir_time: ir_time,
                        start_code: code,
                        est_ir_time: ir_time + IRSINGLETIME,
                        orig_mode: mode.to_string(),
                    },
                });
            }
            out.push(button);
        }
        if let Some(b) = out.last() {
            self.last_button = b.clone();
        }
        Ok(out)
    }

    /// `processFrontPanel`.
    fn front_panel(&mut self, base: &str, dir: &str, ir_time: f64, out: &mut Vec<String>) {
        match dir {
            "repeat" => out.push(format!("{base}.repeat")),
            "down" => {
                self.timers
                    .retain(|t| !matches!(t.kind, TimerKind::FireHold { .. }));
                out.push(base.to_string());
                self.start_ir_hold = ir_time;
                self.timers.push(Timer {
                    at: Instant::now() + Duration::from_secs_f64(IRHOLDTIME),
                    kind: TimerKind::FireHold {
                        button: format!("{base}.hold"),
                        start_code: self.last_code,
                    },
                });
            }
            _ => {
                self.timers
                    .retain(|t| !matches!(t.kind, TimerKind::FireHold { .. }));
                if wrapped(ir_time - self.start_ir_hold) > IRHOLDTIME {
                    out.push(format!("{base}.hold_release"));
                } else {
                    out.push(format!("{base}.single"));
                }
            }
        }
    }

    /// Run due timers; returns the button events they produce.
    pub fn fire_timers(&mut self, mode: &str) -> Vec<String> {
        let now = Instant::now();
        let mut out = Vec::new();
        let (due, rest): (Vec<Timer>, Vec<Timer>) =
            self.timers.drain(..).partition(|t| t.at <= now);
        self.timers = rest;
        for t in due {
            match t.kind {
                TimerKind::FireHold { button, start_code } => {
                    if start_code == self.last_code {
                        out.push(button);
                    }
                }
                kind @ TimerKind::CheckRelease { .. } => {
                    self.check_release(kind, mode, &mut out);
                }
            }
        }
        if let Some(b) = out.last() {
            self.last_button = b.clone();
        }
        out
    }

    /// `checkRelease`: decide whether the button is still down; returns true when it
    /// rescheduled itself (or detected a double press).
    fn check_release(&mut self, kind: TimerKind, mode: &str, out: &mut Vec<String>) -> bool {
        let TimerKind::CheckRelease {
            mut release_type,
            start_ir_time,
            start_code,
            est_ir_time,
            orig_mode,
        } = kind
        else {
            return false;
        };
        if mode != orig_mode {
            return false;
        }
        let release =
            |rt: &str, out: &mut Vec<String>| out.extend(Self::button_with(start_code, rt));
        if start_code != self.last_code {
            if release_type != "hold_check" {
                release(release_type, out);
            }
            return false;
        }
        if start_ir_time != self.start_ir_hold {
            match release_type {
                "hold_check" => return false,
                "hold_release" => {
                    release(release_type, out);
                    return false;
                }
                _ => {
                    release("double", out);
                    self.timers.push(Timer {
                        at: Instant::now() + Duration::from_secs_f64(IRHOLDTIME),
                        kind: TimerKind::CheckRelease {
                            release_type: "hold_check",
                            start_ir_time: self.start_ir_hold,
                            start_code,
                            est_ir_time: self.start_ir_hold + IRHOLDTIME,
                            orig_mode,
                        },
                    });
                    return true;
                }
            }
        }
        if est_ir_time - self.last_ir_time < IRMINTIME {
            // still held
            let next = if est_ir_time >= start_ir_time + IRHOLDTIME {
                release_type = "hold_release";
                IRHOLDTIME / 2.0
            } else if est_ir_time + IRSINGLETIME > start_ir_time + IRHOLDTIME {
                start_ir_time + IRHOLDTIME - est_ir_time
            } else {
                IRSINGLETIME
            };
            self.timers.push(Timer {
                at: Instant::now() + Duration::from_secs_f64(next.max(0.01)),
                kind: TimerKind::CheckRelease {
                    release_type,
                    start_ir_time,
                    start_code,
                    est_ir_time: est_ir_time + next,
                    orig_mode,
                },
            });
            return true;
        }
        if release_type != "hold_check" {
            release(release_type, out);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: u32 = 0x7689e01f;
    const PLAY: u32 = 0x768910ef;

    #[tokio::test(start_paused = true)]
    async fn single_press_then_release() {
        let mut ir = IrState::default();
        assert_eq!(ir.process(PLAY, 1000, "home").unwrap(), vec!["play"]);
        tokio::time::advance(Duration::from_millis(300)).await;
        assert_eq!(ir.fire_timers("home"), vec!["play.single"]);
        assert!(ir.next_timer().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn held_button_repeats_and_holds() {
        let mut ir = IrState::default();
        let mut events = ir.process(UP, 1000, "home").unwrap();
        // the remote repeats every ~108 ms while held
        for i in 1..=12u32 {
            tokio::time::advance(Duration::from_millis(108)).await;
            events.extend(ir.fire_timers("home"));
            events.extend(ir.process(UP, 1000 + i * 108, "home").unwrap());
        }
        assert_eq!(events[0], "arrow_up");
        assert!(events.iter().filter(|e| *e == "arrow_up.repeat").count() >= 10);
        assert_eq!(events.iter().filter(|e| *e == "arrow_up.hold").count(), 1);
        assert!(!events.iter().any(|e| e.ends_with(".single")));
        assert!(ir.hold_time() > 1.0);
        // released: a hold_release follows
        tokio::time::advance(Duration::from_millis(1000)).await;
        let mut after = ir.fire_timers("home");
        tokio::time::advance(Duration::from_millis(1000)).await;
        after.extend(ir.fire_timers("home"));
        assert_eq!(after, vec!["arrow_up.hold_release"]);
    }

    #[test]
    fn front_panel_down_up() {
        let mut ir = IrState::default();
        assert_eq!(ir.process(0x00010012, 1000, "home").unwrap(), vec!["play"]);
        assert_eq!(
            ir.process(0x00020012, 1100, "home").unwrap(),
            vec!["play.single"]
        );
        assert!(ir.process(0x0bad0bad, 1200, "home").is_err());
    }

    #[test]
    fn repeat_count_accelerates() {
        let mut ir = IrState {
            start_ir_hold: 1.0,
            last_ir_time: 1.0 + 3.0,
            ir_time_diff: 0.108,
            ..Default::default()
        };
        let late = ir.repeat_count(50.0, 8.0);
        ir.last_ir_time = 1.0 + 0.5;
        let early = ir.repeat_count(50.0, 8.0);
        assert!(late > early);
    }
}
