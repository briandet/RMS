//! Song scanner (`Slim::Plugin::SongScanner`): hold FWD or REW to get a bar of the playing
//! song; up/down (and FWD/REW held) move the target position, quick FWD/REW presses jump
//! 10 s, and the player seeks there once the target rests for a moment.  The mode leaves
//! itself after 15 s without buttons or when the song changes.

use super::{split_function, Mode, Transition, Ui};
use crate::display::Parts;
use crate::strings::s;

const JUMP_INTERVAL: f64 = 10.0;
const LOCAL_UPDATE_INTERVAL: f64 = 0.4;
const REMOTE_UPDATE_INTERVAL: f64 = 2.0;
const EXITMODE_INTERVAL: f64 = 15.0;

#[derive(Debug, Clone)]
pub(super) struct Scanner {
    /// Target position (seconds); follows playback while nothing is pending.
    offset: f64,
    duration: f64,
    /// Seconds per up/down step.
    increment: f64,
    /// When the target last changed and still has to be applied (0 = nothing pending).
    last_update: f64,
    update_interval: f64,
    exit_at: f64,
    song_id: u64,
    /// Timer ticks since entering (`activeFfwRew`).
    ticks: u32,
}

/// `_formatTime`.
fn format_time(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t % 3600 / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

impl Ui {
    fn playing(&self) -> Option<(u64, Option<f64>, f64, bool, bool)> {
        let c = self.p.controller();
        let c = c.lock();
        let song = c.playing_song()?;
        Some((
            song.id,
            c.playing_song_duration().filter(|d| *d > 0.0),
            c.playing_song_elapsed(),
            song.is_remote(),
            c.mode() == "pause",
        ))
    }

    /// The `song_scanner` function (`_jumptoscanner`).
    pub(super) async fn song_scanner(&mut self) {
        let error = match self.playing() {
            None => Some("PLUGIN_SONGSCANNER_ERR_NOTRACK"),
            Some((_, None, ..)) => Some("PLUGIN_SONGSCANNER_ERR_UNKNOWNSIZE"),
            Some(_) => None,
        };
        if let Some(e) = error {
            self.show_briefly(vec![s(e), String::new()], 1.5);
            return;
        }
        let Some((song_id, Some(duration), elapsed, remote, _)) = self.playing() else {
            return;
        };
        let now = Self::now();
        self.push(Mode::Scanner(Scanner {
            offset: elapsed,
            duration,
            increment: (duration / 100.0).clamp(1.0, 5.0),
            last_update: 0.0,
            update_interval: if remote {
                REMOTE_UPDATE_INTERVAL
            } else {
                LOCAL_UPDATE_INTERVAL
            },
            exit_at: now + EXITMODE_INTERVAL,
            song_id,
            ticks: 0,
        }));
    }

    fn scanner(&mut self) -> Option<&mut Scanner> {
        match self.stack.last_mut() {
            Some(Mode::Scanner(sc)) => Some(sc),
            _ => None,
        }
    }

    /// Seek to a pending target.
    async fn scanner_apply(&mut self) {
        let Some(sc) = self.scanner() else { return };
        if sc.last_update > 0.0 {
            sc.last_update = 0.0;
            let to = format!("{:.1}", sc.offset);
            self.cmd(&["time", &to]).await;
        }
    }

    /// `_scannerExitHandler`: apply what's pending, resume on play, leave.
    async fn scanner_exit(&mut self, play: bool) {
        self.scanner_apply().await;
        if play && self.playing().is_some_and(|p| p.4) {
            self.cmd(&["pause", "0"]).await;
        }
        if matches!(self.stack.last(), Some(Mode::Scanner(_))) {
            self.stack.pop();
            self.transition = Some(Transition::PushRight);
        }
    }

    /// `INPUT.Bar` `changePos` for the scanner: steps of `increment`, accelerating while held.
    fn scanner_move(&mut self, dir: i64) {
        let mut steps = dir as f64;
        if self.ir.hold_time() > 0.0 {
            steps *= self.ir.repeat_count(50.0, 8.0) as f64;
        }
        let now = Self::now();
        let Some(sc) = self.scanner() else { return };
        let new = (sc.offset + steps * sc.increment).clamp(0.0, sc.duration);
        if (dir < 0 && sc.offset <= 0.0)
            || (dir > 0 && sc.offset >= sc.duration)
            || new == sc.offset
        {
            return;
        }
        sc.offset = new;
        sc.last_update = now;
        sc.exit_at = now + EXITMODE_INTERVAL;
    }

    /// `_jump`: ±10 s, not past the end.
    fn scanner_jump(&mut self, interval: f64) {
        let now = Self::now();
        let Some(sc) = self.scanner() else { return };
        if interval > 0.0 && sc.offset + 2.0 * interval < sc.duration {
            sc.offset += interval;
        } else if interval < 0.0 {
            sc.offset = (sc.offset + interval).max(0.0);
        } else {
            return;
        }
        sc.last_update = now;
        sc.exit_at = now + EXITMODE_INTERVAL;
    }

    /// Functions while the scanner is on top (its `INPUT.Bar` and the plugin's own).
    pub(super) async fn scanner_function(&mut self, name: &str, arg: Option<&str>) -> bool {
        match name {
            "up" | "down" => self.scanner_move(if name == "up" { 1 } else { -1 }),
            "exit" | "left" | "right" => self.scanner_exit(false).await,
            "passback" => {
                // the button as the scanner mode itself maps it
                let b = self.ir.last_button.clone();
                let Some(f) = super::irmap::maps()
                    .function(&["songscanner"], &b)
                    .map(str::to_string)
                else {
                    return true;
                };
                let (n, a) = split_function(&f);
                let (n, a) = (n.to_string(), a.map(str::to_string));
                if !Box::pin(self.scanner_own(&n, a.as_deref())).await {
                    self.common_function(&n, a.as_deref(), &b).await;
                }
            }
            _ => return Box::pin(self.scanner_own(name, arg)).await,
        }
        true
    }

    /// `%functions` of the song scanner mode.
    async fn scanner_own(&mut self, name: &str, arg: Option<&str>) -> bool {
        match (name, arg) {
            ("play", _) => self.scanner_exit(true).await,
            ("pause", _) => {
                self.scanner_apply().await;
                let paused = self.playing().is_some_and(|p| p.4);
                self.cmd(&["pause", if paused { "0" } else { "1" }]).await;
                let now = Self::now();
                if let Some(sc) = self.scanner() {
                    sc.exit_at = now + EXITMODE_INTERVAL;
                }
            }
            ("jump", Some("fwd")) => self.scanner_jump(JUMP_INTERVAL),
            ("jump", Some("rew")) => self.scanner_jump(-JUMP_INTERVAL),
            ("song", Some(dir @ ("scanner_fwd" | "scanner_rew"))) => {
                if self.scanner().is_some_and(|sc| sc.ticks > 1) {
                    self.scanner_move(if dir == "scanner_fwd" { 1 } else { -1 });
                }
            }
            _ => return false,
        }
        true
    }

    /// `_timerHandler`, every 0.1 s.
    pub(super) async fn scanner_tick(&mut self) {
        let now = Self::now();
        let playing = self.playing();
        let Some(sc) = self.scanner() else { return };
        sc.ticks += 1;
        let gone = playing.as_ref().is_none_or(|p| p.0 != sc.song_id);
        if gone || now > sc.exit_at {
            self.stack.pop();
            self.transition = Some(Transition::PushRight);
            return;
        }
        if sc.last_update == 0.0 {
            // nothing pending: the target follows playback
            sc.offset = playing.map(|p| p.2).unwrap_or(sc.offset).min(sc.duration);
        } else if now - sc.last_update >= sc.update_interval {
            self.scanner_apply().await;
        }
    }

    /// The scanner screen (`INPUT.Bar` with header value, time overlay and a cursor at the
    /// playing position while a new target is pending).
    pub(super) fn scanner_lines(&self, sc: &Scanner) -> Parts {
        let (elapsed, paused) = self.playing().map(|p| (p.2, p.4)).unwrap_or((0.0, false));
        let rate = if paused {
            format!(" ({})", s("PAUSED"))
        } else {
            String::new()
        };
        let pct = |t: f64| (t / sc.duration * 100.0).clamp(0.0, 100.0);
        Parts {
            line: [
                format!("{} {} ", s("PLUGIN_SONGSCANNER_SET"), rate),
                String::new(),
            ],
            overlay: [
                format!(
                    "{} / -{}",
                    format_time(sc.offset),
                    format_time(sc.duration - sc.offset)
                ),
                String::new(),
            ],
            slider: Some(pct(sc.offset)),
            slider_cursor: (sc.last_update > 0.0).then(|| pct(elapsed)),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn times() {
        assert_eq!(super::format_time(83.7), "01:23");
        assert_eq!(super::format_time(3723.0), "1:02:03");
    }
}
