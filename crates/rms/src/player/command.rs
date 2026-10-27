//! What can be done to a player, as typed commands.  This is the one implementation: the LMS
//! commands (`pause`, `mixer volume`, `playlist index`, `playlistcontrol`, …) parse their
//! tokens into a [`PlayerCommand`] and run it here, and so do the native API and the
//! Subsonic jukebox.
//!
//! [`run`] changes the player and sends the notifications that go with the change itself
//! (`prefset … volume`, `playlist jump`, …).  The notification of the command (what LMS
//! clients listening with `listen` / `subscribe` see) is the caller's: the LMS dispatcher
//! echoes the request as sent, other callers send [`PlayerCommand::lms_words`].

use super::controller::Controller;
use super::playlist::Entry;
use super::Player;
use crate::server::Server;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Shuffle {
    Off,
    Songs,
    Albums,
}

impl Shuffle {
    pub fn from_lms(v: u8) -> Shuffle {
        match v {
            1 => Shuffle::Songs,
            2 => Shuffle::Albums,
            _ => Shuffle::Off,
        }
    }

    pub fn to_lms(self) -> u8 {
        match self {
            Shuffle::Off => 0,
            Shuffle::Songs => 1,
            Shuffle::Albums => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    Off,
    One,
    All,
}

impl Repeat {
    pub fn from_lms(v: u8) -> Repeat {
        match v {
            1 => Repeat::One,
            2 => Repeat::All,
            _ => Repeat::Off,
        }
    }

    pub fn to_lms(self) -> u8 {
        match self {
            Repeat::Off => 0,
            Repeat::One => 1,
            Repeat::All => 2,
        }
    }
}

/// A new value, or a change of the current one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change<T> {
    To(T),
    By(T),
}

/// A queue position to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Index(usize),
    /// Relative to the current track: +1 is the next one, -1 the previous one (or the start
    /// of the current one when it is the only one), 0 the start of the current one.
    By(i64),
}

/// How tracks join the queue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Enqueue {
    /// Replace the queue and play from `start`.
    Play {
        start: Option<usize>,
        fade: Option<f64>,
    },
    /// Append.
    Add,
    /// After the current track.
    Next,
}

#[derive(Debug, Clone)]
pub enum PlayerCommand {
    /// Play from the current track, or resume when paused.
    Play {
        fade: Option<f64>,
    },
    /// Pause when playing.
    Pause,
    /// Pause when playing, play otherwise.
    TogglePause {
        fade: Option<f64>,
    },
    Stop,
    /// Seconds into the current track.
    Seek(Change<f64>),
    /// 0–100.
    Volume(Change<i32>),
    /// Without a value, toggles.
    Mute(Option<bool>),
    /// Without a value, toggles.  `noplay`: don't resume what played before.
    Power {
        on: Option<bool>,
        noplay: bool,
    },
    /// Turn off after this many seconds; 0 cancels.
    Sleep(f64),
    /// Without a value, the next mode in turn.
    Shuffle(Option<Shuffle>),
    Repeat(Option<Repeat>),
    Jump {
        to: Position,
        fade: Option<f64>,
        /// When stopped, only make it the current track.
        noplay: bool,
        /// Start this many seconds in.
        seek: Option<f64>,
    },
    Move {
        from: usize,
        to: usize,
    },
    Remove {
        index: usize,
    },
    Clear,
    Rename(String),
    /// Join that player's sync group.
    Sync {
        with: Arc<Player>,
        no_restart: bool,
    },
    Unsync,
    /// Tracks resolved with [`entries_for`], [`library_entries`] or [`url_entries`].
    Enqueue {
        entries: Vec<Entry>,
        how: Enqueue,
    },
}

/// What a command did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// False when there was nothing to do (LMS then echoes without notifying).
    pub changed: bool,
    /// Tracks added by `Enqueue`.
    pub count: Option<usize>,
    /// Queue index of the first added track.
    pub index: Option<usize>,
}

impl Outcome {
    fn done() -> Outcome {
        Outcome {
            changed: true,
            ..Default::default()
        }
    }

    fn nothing() -> Outcome {
        Outcome::default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandError {
    /// The request does not make sense for this player (400).
    Invalid(String),
}

fn ctrl(p: &Player) -> Arc<Mutex<Controller>> {
    p.controller()
}

/// The playlist changed: timestamp, persistence.
pub fn playlist_changed(c: &mut Controller) {
    c.playlist.timestamp = crate::util::now();
    c.save_playlist();
}

fn num(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn signed<T: std::fmt::Display + PartialOrd + Default>(v: T) -> String {
    if v >= T::default() {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

impl PlayerCommand {
    /// The LMS command that does the same, as listeners are notified of it.
    pub fn lms_words(&self) -> Vec<String> {
        let w: Vec<String> = match self {
            PlayerCommand::Play { .. } => vec!["play".into()],
            PlayerCommand::Pause => vec!["pause".into(), "1".into()],
            PlayerCommand::TogglePause { .. } => vec!["pause".into()],
            PlayerCommand::Stop => vec!["stop".into()],
            PlayerCommand::Seek(Change::To(t)) => vec!["time".into(), num(*t)],
            PlayerCommand::Seek(Change::By(t)) => vec!["time".into(), signed(*t)],
            PlayerCommand::Volume(Change::To(v)) => {
                vec!["mixer".into(), "volume".into(), v.to_string()]
            }
            PlayerCommand::Volume(Change::By(v)) => {
                vec!["mixer".into(), "volume".into(), signed(*v)]
            }
            PlayerCommand::Mute(m) => {
                let mut w = vec!["mixer".to_string(), "muting".into()];
                w.extend(m.map(|m| (m as i32).to_string()));
                w
            }
            PlayerCommand::Power { on, .. } => {
                let mut w = vec!["power".to_string()];
                w.extend(on.map(|o| (o as i32).to_string()));
                w
            }
            PlayerCommand::Sleep(s) => vec!["sleep".into(), num(*s)],
            PlayerCommand::Shuffle(m) => {
                let mut w = vec!["playlist".to_string(), "shuffle".into()];
                w.extend(m.map(|m| m.to_lms().to_string()));
                w
            }
            PlayerCommand::Repeat(m) => {
                let mut w = vec!["playlist".to_string(), "repeat".into()];
                w.extend(m.map(|m| m.to_lms().to_string()));
                w
            }
            PlayerCommand::Jump { to, .. } => {
                let i = match to {
                    Position::Index(i) => i.to_string(),
                    Position::By(n) => signed(*n),
                };
                vec!["playlist".into(), "index".into(), i]
            }
            PlayerCommand::Move { from, to } => vec![
                "playlist".into(),
                "move".into(),
                from.to_string(),
                to.to_string(),
            ],
            PlayerCommand::Remove { index } => {
                vec!["playlist".into(), "delete".into(), index.to_string()]
            }
            PlayerCommand::Clear => vec!["playlist".into(), "clear".into()],
            PlayerCommand::Rename(n) => vec!["name".into(), n.clone()],
            PlayerCommand::Sync { with, .. } => vec!["sync".into(), with.id.to_string()],
            PlayerCommand::Unsync => vec!["sync".into(), "-".into()],
            PlayerCommand::Enqueue { how, entries } => {
                let cmd = match how {
                    Enqueue::Play { .. } => "loadtracks",
                    Enqueue::Add => "addtracks",
                    Enqueue::Next => "inserttracks",
                };
                vec![
                    "playlist".into(),
                    cmd.into(),
                    "listRef".into(),
                    entries.len().to_string(),
                ]
            }
        };
        w
    }
}

/// Run `cmd` on `p` and send the notifications of what changed (not of the command; see
/// the module documentation).
pub fn run(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: PlayerCommand,
) -> Result<Outcome, CommandError> {
    p.state.lock().last_activity = crate::util::now();
    Ok(match cmd {
        PlayerCommand::Play { fade } => play(server, p, fade),
        PlayerCommand::Pause => {
            let c = ctrl(p);
            let mut c = c.lock();
            if c.mode() == "play" {
                c.pause();
                Outcome::done()
            } else {
                Outcome::nothing()
            }
        }
        PlayerCommand::TogglePause { fade } => {
            let mode = ctrl(p).lock().mode();
            if mode == "pause" || mode == "stop" {
                play(server, p, fade)
            } else {
                ctrl(p).lock().pause();
                Outcome::done()
            }
        }
        PlayerCommand::Stop => {
            let c = ctrl(p);
            let mut c = c.lock();
            if c.mode() != "stop" {
                c.stop();
            }
            Outcome::done()
        }
        PlayerCommand::Seek(t) => {
            let t = match t {
                Change::To(t) if t.is_finite() && t >= 0.0 => num(t),
                Change::By(d) if d.is_finite() => signed(d),
                _ => return Err(CommandError::Invalid("not a time".into())),
            };
            ctrl(p).lock().jump_to_time(&t, false);
            Outcome::done()
        }
        PlayerCommand::Volume(v) => volume(server, p, v),
        PlayerCommand::Mute(m) => {
            let muted = m.unwrap_or(!p.is_muted());
            p.set_muted(muted);
            crate::display::show_volume(p);
            for o in volume_group(p) {
                o.set_muted(muted);
            }
            Outcome::done()
        }
        PlayerCommand::Power { on, noplay } => {
            let new = on.unwrap_or(!p.power());
            if new == p.power() {
                return Ok(Outcome::nothing());
            }
            server.set_power(p, new, noplay);
            server.notifier.notify(
                Some(p.id),
                &["prefset", "server", "power", if new { "1" } else { "0" }],
            );
            Outcome::done()
        }
        PlayerCommand::Sleep(secs) => {
            p.state.lock().sleep_time = if secs > 0.0 {
                crate::util::now() + secs
            } else {
                0.0
            };
            Outcome::done()
        }
        PlayerCommand::Shuffle(m) => shuffle(server, p, m),
        PlayerCommand::Repeat(m) => {
            let c = ctrl(p);
            let mut c = c.lock();
            let new = match m {
                Some(m) => m.to_lms(),
                None => [1u8, 2, 0][c.playlist.repeat as usize % 3],
            };
            c.playlist.repeat = new;
            c.save_playlist();
            drop(c);
            server.notifier.notify(
                Some(p.id),
                &["prefset", "server", "repeat", &new.to_string()],
            );
            Outcome::done()
        }
        PlayerCommand::Jump {
            to,
            fade,
            noplay,
            seek,
        } => jump(server, p, to, fade, noplay, seek),
        PlayerCommand::Move { from, to } => {
            let c = ctrl(p);
            let mut c = c.lock();
            let len = c.playlist.len();
            if from >= len || to >= len {
                return Err(out_of_range(from.max(to), len));
            }
            c.playlist.move_song(from, to);
            // keep queue indexes consistent with the moved positions
            for s in c.queue.iter_mut() {
                if s.index == from {
                    s.index = to;
                } else if from < s.index && to >= s.index {
                    s.index -= 1;
                } else if from > s.index && to <= s.index {
                    s.index += 1;
                }
            }
            playlist_changed(&mut c);
            Outcome::done()
        }
        PlayerCommand::Remove { index } => {
            let c = ctrl(p);
            let mut c = c.lock();
            let len = c.playlist.len();
            if index >= len {
                return Err(out_of_range(index, len));
            }
            remove_track(&mut c, index);
            playlist_changed(&mut c);
            Outcome::done()
        }
        PlayerCommand::Clear => {
            let c = ctrl(p);
            let mut c = c.lock();
            c.stop();
            c.playlist.clear();
            c.queue.clear();
            playlist_changed(&mut c);
            Outcome::done()
        }
        PlayerCommand::Rename(name) => {
            if name.trim().is_empty() {
                return Err(CommandError::Invalid("name must not be empty".into()));
            }
            p.set_name(&name);
            Outcome::done()
        }
        PlayerCommand::Sync { with, no_restart } => {
            if with.id == p.id {
                return Err(CommandError::Invalid(
                    "a player can't sync with itself".into(),
                ));
            }
            server.sync(&with, p, no_restart);
            Outcome::done()
        }
        PlayerCommand::Unsync => {
            server.unsync(p);
            Outcome::done()
        }
        PlayerCommand::Enqueue { entries, how } => enqueue(server, p, entries, how),
    })
}

fn out_of_range(index: usize, len: usize) -> CommandError {
    CommandError::Invalid(format!(
        "index {index} is out of range (queue has {len} tracks)"
    ))
}

fn play(server: &Arc<Server>, p: &Arc<Player>, fade: Option<f64>) -> Outcome {
    let c = ctrl(p);
    let mode = c.lock().mode();
    if mode == "play" {
        return Outcome::nothing();
    }
    if !p.power() {
        server.set_power(p, true, true);
    }
    let mut c = c.lock();
    if mode == "pause" {
        c.resume(fade);
    } else if !c.playlist.is_empty() {
        let idx = c.playing_song_index().min(c.playlist.len() - 1);
        c.play(Some(idx), None, fade);
        drop(c);
        server
            .notifier
            .notify(Some(p.id), &["playlist", "jump", &idx.to_string()]);
    }
    Outcome::done()
}

/// The other players of `p`'s sync group that share its volume.
fn volume_group(p: &Arc<Player>) -> Vec<Arc<Player>> {
    if !p.prefs.get_bool("syncVolume") {
        return Vec::new();
    }
    ctrl(p)
        .lock()
        .all_players()
        .into_iter()
        .filter(|o| o.id != p.id && o.prefs.get_bool("syncVolume"))
        .collect()
}

fn volume(server: &Arc<Server>, p: &Arc<Player>, v: Change<i32>) -> Outcome {
    let old = if p.is_muted() { 0 } else { p.volume() };
    let new = match v {
        Change::To(n) => n,
        Change::By(d) => old + d,
    };
    let new = p.set_volume(new, false);
    crate::display::show_volume(p);
    for o in volume_group(p) {
        o.set_volume(new, false);
        crate::display::show_volume(&o);
    }
    server.notifier.notify(
        Some(p.id),
        &["prefset", "server", "volume", &new.to_string()],
    );
    Outcome::done()
}

fn shuffle(server: &Arc<Server>, p: &Arc<Player>, m: Option<Shuffle>) -> Outcome {
    let c = ctrl(p);
    let mut c = c.lock();
    let new = match m {
        Some(m) => m.to_lms(),
        None => [1u8, 2, 0][c.playlist.shuffle as usize % 3],
    };
    let playing = !c.is_stopped();
    let cur_entry = c.playlist.order.get(c.playing_song_index()).copied();
    c.playlist.current = c
        .playing_song_index()
        .min(c.playlist.len().saturating_sub(1));
    c.playlist.set_shuffle(new, playing);
    // re-point queue songs at their new positions
    let order = c.playlist.order.clone();
    if let Some(ce) = cur_entry {
        let newpos = order.iter().position(|&i| i == ce).unwrap_or(0);
        for s in c.queue.iter_mut() {
            s.index = newpos;
        }
        c.playlist.current = newpos;
        if playing {
            // drop any already-streamed next song so the new order is followed
            while c.queue.len() > 1 {
                c.queue.pop_front();
            }
            if c.streaming_state != super::controller::StreamingState::Streaming {
                c.flush();
            }
        }
    }
    playlist_changed(&mut c);
    drop(c);
    server.notifier.notify(
        Some(p.id),
        &["prefset", "server", "shuffle", &new.to_string()],
    );
    Outcome::done()
}

fn jump(
    server: &Arc<Server>,
    p: &Arc<Player>,
    to: Position,
    fade: Option<f64>,
    noplay: bool,
    seek: Option<f64>,
) -> Outcome {
    // the queue holds only a Spotify Connect stream: the app's queue moves instead
    if let Position::By(n) = to {
        if crate::plugins::spotify::connect::skip(p, n) {
            return Outcome::done();
        }
    }
    let c = ctrl(p);
    let count = c.lock().playlist.len();
    if count == 0 {
        return Outcome::nothing();
    }
    if !p.power() {
        server.set_power(p, true, true);
    }
    let mut c = c.lock();
    let new_index = match to {
        Position::By(n) => {
            if !c.is_stopped() {
                if n == 0 || (n == -1 && count == 1) {
                    c.jump_to_time("0", true);
                    return Outcome::done();
                }
                if n == 1 {
                    c.skip();
                    return Outcome::done();
                }
            }
            let i = c.playing_song_index() as i64 + n;
            if i >= count as i64
                && c.playlist.shuffle != 0
                && c.playlist.repeat == 2
                && server.prefs.server.get_bool("reshuffleOnRepeat")
            {
                c.playlist.reshuffle(false);
            }
            i
        }
        Position::Index(i) => i as i64,
    };
    let new_index = new_index.rem_euclid(count as i64) as usize;
    if noplay && c.is_stopped() {
        c.reset_song_queue(Some(new_index));
    } else {
        c.play(Some(new_index), seek, fade);
    }
    c.save_playlist();
    Outcome::done()
}

/// `Slim::Player::Playlist::removeTrack`: stop/skip appropriately when the playing song goes.
pub fn remove_track(c: &mut Controller, idx: usize) {
    if idx >= c.playlist.len() {
        return;
    }
    let playing = c.playing_song_index();
    let streaming = c.streaming_song_index();
    let was_stopped = c.is_stopped();
    if idx == playing && !was_stopped {
        if c.playlist.len() == 1 {
            c.stop();
        } else {
            c.playlist.delete(idx);
            // the song that took its place plays next
            let next = idx.min(c.playlist.len() - 1);
            let paused = c.is_paused();
            c.play(Some(next), None, None);
            if paused {
                c.pause();
            }
            return;
        }
    } else if idx == streaming && idx != playing && !was_stopped {
        c.flush();
    }
    c.playlist.delete(idx);
    for s in c.queue.iter_mut() {
        if s.index > idx {
            s.index -= 1;
        }
    }
    if c.playlist.is_empty() {
        c.queue.clear();
        c.stop();
    }
}

fn enqueue(server: &Arc<Server>, p: &Arc<Player>, entries: Vec<Entry>, how: Enqueue) -> Outcome {
    let count = entries.len();
    let c = ctrl(p);
    let mut c = c.lock();
    let index = match how {
        Enqueue::Play { start, fade } => {
            c.stop();
            c.playlist.clear();
            c.queue.clear();
            c.playlist.append(entries);
            c.playlist.modified = false;
            if !c.playlist.is_empty() {
                let mut j = start.unwrap_or(0).min(c.playlist.len() - 1);
                if c.playlist.shuffle != 0 && start.is_some() {
                    c.playlist.jump_to_entry(j);
                    j = c.playlist.current;
                }
                drop(c);
                if !p.power() {
                    server.set_power(p, true, true);
                }
                let cc = p.controller();
                let mut c = cc.lock();
                c.play(Some(j), None, fade);
                playlist_changed(&mut c);
                drop(c);
                server
                    .notifier
                    .notify(Some(p.id), &["playlist", "jump", &j.to_string()]);
                return Outcome {
                    changed: true,
                    count: Some(count),
                    index: Some(0),
                };
            }
            0
        }
        Enqueue::Add => {
            c.playlist.append(entries);
            c.playlist.modified = true;
            c.next_if_streamed();
            c.playlist.len() - count
        }
        Enqueue::Next => {
            let index = c.streaming_song_index() + 1;
            c.playlist.insert_after_current(entries);
            c.playlist.modified = true;
            c.next_if_streamed();
            drop(c);
            server
                .notifier
                .notify(Some(p.id), &["playlist", "load_done"]);
            playlist_changed(&mut ctrl(p).lock());
            return Outcome {
                changed: true,
                count: Some(count),
                index: Some(index),
            };
        }
    };
    playlist_changed(&mut c);
    Outcome {
        changed: true,
        count: Some(count),
        index: Some(index),
    }
}

// ---- what to enqueue ---------------------------------------------------------------------

/// Library tracks as queue entries.
pub async fn library_entries(server: &Arc<Server>, f: rms_library::TrackFilter) -> Vec<Entry> {
    let lib = server.library.clone();
    let tracks = crate::util::spawn_blocking(move || lib.tracks_for(&f))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    tracks.iter().map(|t| server.entry_for_track(t)).collect()
}

/// A URL (a file, a folder, a playlist file, a stream, a Spotify link, a `db:` favorite) as
/// queue entries.
pub async fn url_entries(server: &Arc<Server>, url: &str) -> Vec<Entry> {
    crate::control::commands::expand_item(server, url).await
}

/// What a library item stands for, as queue entries.
pub async fn entries_for(server: &Arc<Server>, item: &Item) -> Vec<Entry> {
    use rms_library::TrackFilter;
    let f = match item {
        Item::Track(id) => TrackFilter {
            track_ids: vec![*id],
            ..Default::default()
        },
        Item::Album(id) => TrackFilter {
            album_id: Some(id.to_string()),
            ..Default::default()
        },
        Item::Artist(id) => TrackFilter {
            artist_id: Some(*id),
            ..Default::default()
        },
        Item::Genre(id) => TrackFilter {
            genre_id: Some(id.to_string()),
            ..Default::default()
        },
        Item::Year(y) => TrackFilter {
            year: Some(*y),
            ..Default::default()
        },
        Item::Playlist(id) => TrackFilter {
            playlist_id: Some(*id),
            ..Default::default()
        },
        Item::Folder(id) => TrackFilter {
            folder_id: Some(*id),
            ..Default::default()
        },
        Item::Url(u) => return url_entries(server, u).await,
        Item::Favorite(id) => return crate::plugins::favorites::entries(server, id).await,
        Item::Tracks(ids) if ids.iter().any(|id| rms_library::remote::is_remote(*id)) => {
            return mixed_entries(server, ids).await
        }
        Item::Tracks(ids) => TrackFilter {
            track_ids: ids.clone(),
            ..Default::default()
        },
    };
    library_entries(server, f).await
}

/// Library tracks and learned streamed tracks (negative ids, `rms_library::remote`), in
/// order.  A streamed track's title and artwork come from what was learned, so showing it
/// needs no lookup.
async fn mixed_entries(server: &Arc<Server>, ids: &[i64]) -> Vec<Entry> {
    use rms_library::remote::is_remote;
    let mut out = Vec::new();
    let mut i = 0;
    while i < ids.len() {
        let run = ids[i..]
            .iter()
            .take_while(|id| is_remote(**id) == is_remote(ids[i]))
            .count();
        let part = &ids[i..i + run];
        if is_remote(ids[i]) {
            for id in part {
                let Some(t) = server.library.remote_track(*id) else {
                    continue;
                };
                if let Some(sp) = crate::plugins::spotify::get() {
                    sp.meta.entry(t.url.clone()).or_insert_with(|| {
                        crate::plugins::spotify::TrackMeta {
                            title: t.title.clone(),
                            artist: t.artists.join(", "),
                            album: t.album.clone().unwrap_or_default(),
                            secs: t.duration.unwrap_or(0.0),
                            image: t.cover.clone(),
                            ..Default::default()
                        }
                    });
                }
                out.push(Entry::new(t.url));
            }
        } else {
            let f = rms_library::TrackFilter {
                track_ids: part.to_vec(),
                ..Default::default()
            };
            out.extend(library_entries(server, f).await);
        }
        i += run;
    }
    out
}

/// Something to queue.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Track(i64),
    Tracks(Vec<i64>),
    Album(i64),
    Artist(i64),
    Genre(i64),
    Year(i64),
    Playlist(i64),
    Folder(i64),
    Url(String),
    /// A favorite (or a folder of them) by its id (`0.1`).
    Favorite(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(c: PlayerCommand) -> String {
        c.lms_words().join(" ")
    }

    /// Listeners and subscriptions match on these words, as LMS sends them.
    #[test]
    fn commands_are_notified_as_lms_commands() {
        assert_eq!(words(PlayerCommand::Pause), "pause 1");
        assert_eq!(words(PlayerCommand::TogglePause { fade: None }), "pause");
        assert_eq!(words(PlayerCommand::Seek(Change::To(61.5))), "time 61.5");
        assert_eq!(words(PlayerCommand::Seek(Change::By(-10.0))), "time -10");
        assert_eq!(
            words(PlayerCommand::Volume(Change::To(30))),
            "mixer volume 30"
        );
        assert_eq!(
            words(PlayerCommand::Volume(Change::By(5))),
            "mixer volume +5"
        );
        assert_eq!(words(PlayerCommand::Mute(None)), "mixer muting");
        assert_eq!(words(PlayerCommand::Mute(Some(true))), "mixer muting 1");
        assert_eq!(
            words(PlayerCommand::Power {
                on: Some(false),
                noplay: false
            }),
            "power 0"
        );
        assert_eq!(
            words(PlayerCommand::Shuffle(Some(Shuffle::Albums))),
            "playlist shuffle 2"
        );
        assert_eq!(
            words(PlayerCommand::Jump {
                to: Position::By(1),
                fade: None,
                noplay: false,
                seek: None
            }),
            "playlist index +1"
        );
        assert_eq!(
            words(PlayerCommand::Move { from: 5, to: 0 }),
            "playlist move 5 0"
        );
        assert_eq!(words(PlayerCommand::Unsync), "sync -");
    }
}
