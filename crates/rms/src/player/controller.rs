//! Playback controller (`Slim::Player::StreamingController`).
//!
//! One controller exists per sync group (an unsynced player has its own).  It owns the
//! playlist shared by the group and runs the two-dimensional state machine
//! (playing state × streaming state) whose transition table is reproduced verbatim from the
//! reference server in [`action_for`].

use super::playlist::Playlist;
use super::song::{RetryData, Song, SongStatus, TrackInfo};
use super::Player;
use crate::server::Server;
use crate::stream::{self, PlayParams};
use parking_lot::Mutex;
use serde_json::json;
use slimproto::server::TransitionType;
use slimproto::MacAddress;
use std::collections::VecDeque;
use std::sync::{Arc, Weak};
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayingState {
    Stopped = 0,
    Buffering = 1,
    WaitingToSync = 2,
    Playing = 3,
    Paused = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingState {
    Idle = 0,
    Streaming = 1,
    Streamout = 2,
    TrackWait = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Stop,
    Play,
    ContinuePlay,
    Pause,
    Resume,
    Flush,
    Skip,
    JumpToTime,
    NextTrackReady,
    NextTrackError,
    LocalEndOfStream,
    BufferReady,
    Started,
    StreamingFailed,
    EndOfStream,
    ReadyToStream,
    Stopped,
    OutputUnderrun,
    StatusHeartbeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    NoOp,
    BadState,
    Invalid,
    Stop,
    StopGetNext,
    Continue,
    PlayIfReady,
    Pause,
    JumpOrResume,
    Resume,
    FlushGetNext,
    Skip,
    JumpToTime,
    JumpPaused,
    Stream,
    StreamIfReady,
    NextIfMore,
    Streamout,
    WaitToSync,
    StartIfReady,
    Playing,
    PlayAndNext,
    PlayAndStream,
    StopNextIfMore,
    SyncStopNext,
    StartStreamout,
    Start,
    AutoStart,
    RetryOrNext,
    Stopped,
    Buffering,
    Rebuffer,
    CheckSync,
    CheckPaused,
}

/// `%stateTable{event}[playingState][streamingState]`.
fn action_for(ev: Event, ps: PlayingState, ss: StreamingState) -> Action {
    use Action::*;
    let row: [[Action; 4]; 5] = match ev {
        Event::Stop => [
            [NoOp, BadState, BadState, Stop],
            [BadState, Stop, Stop, BadState],
            [BadState, Stop, Stop, BadState],
            [Stop, Stop, Stop, Stop],
            [Stop, Stop, Stop, Stop],
        ],
        Event::Play => [
            [StopGetNext, BadState, BadState, StopGetNext],
            [BadState, StopGetNext, StopGetNext, BadState],
            [BadState, StopGetNext, StopGetNext, BadState],
            [StopGetNext, StopGetNext, StopGetNext, StopGetNext],
            [StopGetNext, StopGetNext, StopGetNext, StopGetNext],
        ],
        Event::ContinuePlay => [
            [Stop, BadState, BadState, StopGetNext],
            [BadState, StopGetNext, StopGetNext, BadState],
            [BadState, StopGetNext, StopGetNext, BadState],
            [Continue, Continue, Continue, PlayIfReady],
            [Stop, Stop, Stop, Stop],
        ],
        Event::Pause => [
            [Invalid, BadState, BadState, NoOp],
            [BadState, NoOp, NoOp, BadState],
            [BadState, NoOp, NoOp, BadState],
            [Pause, Pause, Pause, Pause],
            [JumpOrResume, Resume, Resume, Resume],
        ],
        Event::Resume => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, Invalid, Invalid, BadState],
            [BadState, Invalid, Invalid, BadState],
            [Invalid, Invalid, Invalid, Invalid],
            [JumpOrResume, Resume, Resume, Resume],
        ],
        Event::Flush => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, Invalid, Invalid, BadState],
            [BadState, Invalid, Invalid, BadState],
            [FlushGetNext, FlushGetNext, FlushGetNext, FlushGetNext],
            [FlushGetNext, FlushGetNext, FlushGetNext, FlushGetNext],
        ],
        Event::Skip => [
            [StopGetNext, BadState, BadState, NoOp],
            [BadState, Skip, Skip, BadState],
            [BadState, Skip, Skip, BadState],
            [StopGetNext, Skip, Skip, Skip],
            [StopGetNext, Skip, Skip, Skip],
        ],
        Event::JumpToTime => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, JumpToTime, JumpToTime, BadState],
            [BadState, JumpToTime, JumpToTime, BadState],
            [JumpToTime, JumpToTime, JumpToTime, JumpToTime],
            [JumpPaused, JumpPaused, JumpPaused, JumpPaused],
        ],
        Event::NextTrackReady => [
            [NoOp, BadState, BadState, Stream],
            [BadState, Invalid, Invalid, BadState],
            [BadState, Invalid, Invalid, BadState],
            [Invalid, Invalid, Invalid, StreamIfReady],
            [Invalid, Invalid, Invalid, StreamIfReady],
        ],
        Event::NextTrackError => [
            [Invalid, BadState, BadState, NextIfMore],
            [BadState, Invalid, Invalid, BadState],
            [BadState, Invalid, Invalid, BadState],
            [Invalid, Invalid, Invalid, NextIfMore],
            [Invalid, Invalid, Invalid, NextIfMore],
        ],
        Event::LocalEndOfStream => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, Streamout, Invalid, BadState],
            [BadState, Streamout, Invalid, BadState],
            [Invalid, Streamout, Invalid, Invalid],
            [Invalid, Streamout, Invalid, Invalid],
        ],
        Event::BufferReady => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, WaitToSync, WaitToSync, BadState],
            [BadState, StartIfReady, StartIfReady, BadState],
            [Invalid, Invalid, Invalid, Invalid],
            [Invalid, Invalid, Invalid, Invalid],
        ],
        Event::Started => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, Playing, Playing, BadState],
            [BadState, Invalid, Invalid, BadState],
            [PlayAndNext, Playing, Playing, PlayAndStream],
            [PlayAndNext, Playing, Playing, PlayAndStream],
        ],
        Event::StreamingFailed => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, StopNextIfMore, StopNextIfMore, BadState],
            [BadState, StopNextIfMore, StopNextIfMore, BadState],
            [Invalid, SyncStopNext, SyncStopNext, Invalid],
            [Invalid, Stop, Stop, Invalid],
        ],
        Event::EndOfStream => [
            [NoOp, BadState, BadState, NoOp],
            [BadState, StartStreamout, Start, BadState],
            [BadState, StartStreamout, Start, BadState],
            [Invalid, AutoStart, AutoStart, Invalid],
            [Invalid, Streamout, NoOp, Invalid],
        ],
        Event::ReadyToStream => [
            [Invalid, BadState, BadState, Invalid],
            [BadState, NoOp, Invalid, BadState],
            [BadState, Invalid, Invalid, BadState],
            [NoOp, NextIfMore, RetryOrNext, StreamIfReady],
            [NoOp, NextIfMore, NextIfMore, StreamIfReady],
        ],
        Event::Stopped => [
            [Invalid, BadState, BadState, NoOp],
            [BadState, NoOp, NoOp, BadState],
            [BadState, Invalid, Invalid, BadState],
            [Stopped, Buffering, Buffering, PlayIfReady],
            [Stopped, Buffering, Buffering, Stopped],
        ],
        Event::OutputUnderrun => [
            [NoOp, BadState, BadState, NoOp],
            [BadState, NoOp, NoOp, BadState],
            [BadState, Invalid, Invalid, BadState],
            [Invalid, Rebuffer, Rebuffer, Invalid],
            [NoOp, NoOp, NoOp, NoOp],
        ],
        Event::StatusHeartbeat => [
            [NoOp, BadState, BadState, NoOp],
            [BadState, NoOp, NoOp, BadState],
            [BadState, StartIfReady, StartIfReady, BadState],
            [CheckSync, CheckSync, CheckSync, CheckSync],
            [NoOp, CheckPaused, CheckPaused, NoOp],
        ],
    };
    row[ps as usize][ss as usize]
}

const VALID_STATES: [[bool; 4]; 5] = [
    [true, false, false, true],
    [false, true, true, false],
    [false, true, true, false],
    [true, true, true, true],
    [true, true, true, true],
];

const FADEVOLUME: f64 = 0.3125;
const CHECK_SYNC_INTERVAL: f64 = 0.950;
const MIN_DEVIATION_ADJUST: f64 = 0.010;
const MAX_DEVIATION_ADJUST: f64 = 10.0;
const PLAYPOINT_RECENT_THRESHOLD: f64 = 3.0;

/// Parameters carried with an event.
#[derive(Debug, Default, Clone)]
pub struct EvParams {
    pub index: Option<usize>,
    /// Seek time offset (seconds) for the song to stream.
    pub seek: Option<f64>,
    /// Song id to stream (instead of `nextTrack`).
    pub song_id: Option<u64>,
    pub reconnect: bool,
    /// New time for JumpToTime: absolute, or relative when prefixed with `+`/`-`.
    pub newtime: Option<String>,
    pub restart_if_no_seek: bool,
    pub error_disconnect: bool,
    pub error_index: Option<usize>,
}

pub struct Controller {
    pub master_id: MacAddress,
    server: Weak<Server>,
    me: Weak<Mutex<Controller>>,
    /// Active (powered, connected) players; the first is the master.
    players: Vec<Weak<Player>>,
    all_players: Vec<Weak<Player>>,
    pub playlist: Playlist,
    pub playing_state: PlayingState,
    pub streaming_state: StreamingState,
    rebuffering: bool,
    /// Front = streaming song (`songqueue->[0]`), back = playing song (`songqueue->[-1]`).
    pub queue: VecDeque<Song>,
    next_track: Option<u64>,
    next_track_cb_id: u64,
    pub resume_time: Option<f64>,
    fade_in: Option<f64>,
    fade_active: bool,
    consecutive_errors: usize,
    next_check_sync: f64,
    pub sync_group_id: Option<u64>,
    pub track_start_time: f64,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Controller({} {:?}-{:?})",
            self.master_id, self.playing_state, self.streaming_state
        )
    }
}

impl Controller {
    /// Inert controller used only during player construction.
    pub fn placeholder(id: MacAddress) -> Controller {
        Controller {
            master_id: id,
            server: Weak::new(),
            me: Weak::new(),
            players: Vec::new(),
            all_players: Vec::new(),
            playlist: Playlist::default(),
            playing_state: PlayingState::Stopped,
            streaming_state: StreamingState::Idle,
            rebuffering: false,
            queue: VecDeque::new(),
            next_track: None,
            next_track_cb_id: 0,
            resume_time: None,
            fade_in: None,
            fade_active: false,
            consecutive_errors: 0,
            next_check_sync: 0.0,
            sync_group_id: None,
            track_start_time: 0.0,
        }
    }

    pub fn new_shared(server: &Arc<Server>, player: &Arc<Player>) -> Arc<Mutex<Controller>> {
        Arc::new_cyclic(|me| {
            let mut c = Controller::placeholder(player.id);
            c.server = Arc::downgrade(server);
            c.me = me.clone();
            c.all_players.push(Arc::downgrade(player));
            if player.power() {
                c.players.push(Arc::downgrade(player));
            }
            Mutex::new(c)
        })
    }

    fn server(&self) -> Option<Arc<Server>> {
        self.server.upgrade()
    }

    pub fn active_players(&self) -> Vec<Arc<Player>> {
        self.players.iter().filter_map(Weak::upgrade).collect()
    }

    pub fn all_players(&self) -> Vec<Arc<Player>> {
        self.all_players.iter().filter_map(Weak::upgrade).collect()
    }

    pub fn master(&self) -> Option<Arc<Player>> {
        self.server().and_then(|s| s.player(&self.master_id))
    }

    pub fn is_master(&self, id: MacAddress) -> bool {
        self.master_id == id
    }

    pub fn is_playing(&self, strict: bool) -> bool {
        if strict {
            self.playing_state == PlayingState::Playing
        } else {
            matches!(
                self.playing_state,
                PlayingState::Playing | PlayingState::Buffering | PlayingState::WaitingToSync
            )
        }
    }

    pub fn is_paused(&self) -> bool {
        self.playing_state == PlayingState::Paused
    }

    pub fn is_stopped(&self) -> bool {
        self.playing_state == PlayingState::Stopped
    }

    pub fn is_streamout(&self) -> bool {
        self.streaming_state == StreamingState::Streamout
    }

    /// LMS `mode` value.
    pub fn mode(&self) -> &'static str {
        match self.playing_state {
            PlayingState::Stopped => "stop",
            PlayingState::Paused => "pause",
            _ => "play",
        }
    }

    pub fn playing_song(&self) -> Option<&Song> {
        self.queue.back()
    }

    pub fn streaming_song(&self) -> Option<&Song> {
        self.queue.front()
    }

    /// `Slim::Player::Source::playingSongIndex`.
    pub fn playing_song_index(&self) -> usize {
        self.playing_song()
            .map(|s| s.index)
            .unwrap_or(self.playlist.current)
    }

    pub fn streaming_song_index(&self) -> usize {
        self.streaming_song()
            .map(|s| s.index)
            .unwrap_or(self.playlist.current)
    }

    pub fn playing_song_duration(&self) -> Option<f64> {
        self.playing_song().and_then(|s| s.duration())
    }

    // ---- elapsed time ---------------------------------------------------------------

    pub fn playing_song_elapsed(&self) -> f64 {
        if self.is_paused() {
            return self.resume_time.unwrap_or(0.0);
        }
        let Some(song) = self.playing_song() else {
            return 0.0;
        };
        let Some(master) = self.master() else {
            return 0.0;
        };
        let elapsed = if self.is_stopped() {
            Some(0.0)
        } else {
            master.song_elapsed_seconds(self.is_playing(true))
        };
        let mut t = match elapsed {
            Some(e) if e > 0.0 => e,
            _ => {
                if song.is_remote() {
                    master
                        .state
                        .lock()
                        .remote_stream_start
                        .map(|s| crate::util::now() - s)
                        .unwrap_or(0.0)
                } else {
                    0.0
                }
            }
        };
        t += song.start_offset;
        if t < 0.0 {
            t = 0.0;
        }
        if let Some(d) = song.duration() {
            if t > d {
                t = d;
            }
        }
        t
    }

    // ---- event entry point ----------------------------------------------------------

    pub fn event(&mut self, ev: Event, p: EvParams) {
        let action = action_for(ev, self.playing_state, self.streaming_state);
        debug!(
            "{}: {:?} in {:?}-{:?} -> {:?}",
            self.master_id, ev, self.playing_state, self.streaming_state, action
        );
        let before = (self.playing_state, self.streaming_state);
        self.run(action, ev, p);
        if !VALID_STATES[self.playing_state as usize][self.streaming_state as usize] {
            error!(
                "{}: {:?} with {:?} resulted in invalid state {:?}-{:?}",
                self.master_id, ev, action, self.playing_state, self.streaming_state
            );
        } else if before != (self.playing_state, self.streaming_state) {
            debug!(
                "{}: {:?} - new state {:?}-{:?}",
                self.master_id, ev, self.playing_state, self.streaming_state
            );
        }
    }

    fn run(&mut self, action: Action, ev: Event, p: EvParams) {
        use Action::*;
        match action {
            NoOp => {}
            BadState => error!(
                "{}: event {:?} received while in invalid state {:?}-{:?}",
                self.master_id, ev, self.playing_state, self.streaming_state
            ),
            Invalid => warn!(
                "{}: event {:?} received while in invalid state {:?}-{:?}",
                self.master_id, ev, self.playing_state, self.streaming_state
            ),
            Stop => self.a_stop(false),
            StopGetNext => {
                self.a_stop(false);
                self.get_next_track(&p, false);
            }
            Continue => self.a_continue(p),
            PlayIfReady => {
                self.set_playing_state(PlayingState::Stopped);
                if self.next_track.is_some() {
                    self.a_stream(p);
                } else {
                    self.notify_stopped(false);
                }
            }
            Pause => self.a_pause(),
            JumpOrResume => {
                if let Some(t) = self.resume_time {
                    self.fade_in = Some(FADEVOLUME);
                    self.a_jump_to_time(EvParams {
                        newtime: Some(t.to_string()),
                        restart_if_no_seek: true,
                        ..Default::default()
                    });
                    self.resume_time = None;
                    self.fade_in = None;
                } else {
                    self.a_resume();
                }
            }
            Resume => self.a_resume(),
            FlushGetNext => {
                self.queue.pop_front();
                for pl in self.active_players() {
                    pl.strm_flush();
                }
                self.set_streaming_state(StreamingState::Idle);
                self.get_next_track(&p, true);
            }
            Skip => {
                self.a_stop(false);
                self.get_next_track(&p, false);
            }
            JumpToTime => self.a_jump_to_time(p),
            JumpPaused => self.a_jump_paused(p),
            Stream => self.a_stream(p),
            StreamIfReady => self.a_stream_if_ready(p),
            NextIfMore => self.a_next_if_more(p),
            Streamout => self.set_streaming_state(StreamingState::Streamout),
            WaitToSync => {
                self.set_playing_state(PlayingState::WaitingToSync);
                self.a_start_if_ready();
            }
            StartIfReady => self.a_start_if_ready(),
            Playing => self.a_playing(ev == Event::Started),
            PlayAndNext => {
                self.a_playing(ev == Event::Started);
                self.get_next_track(&EvParams::default(), true);
            }
            PlayAndStream => {
                self.a_playing(ev == Event::Started);
                self.a_stream_if_ready(p);
            }
            StopNextIfMore => {
                self.a_stop(false);
                if self.will_retry() {
                    return;
                }
                self.get_next_track(&p, true);
            }
            SyncStopNext => {
                if self.players.len() > 1 {
                    self.a_stop(false);
                } else if p.error_disconnect {
                    // already playing: treat like end of stream, but give a retry a chance
                    self.set_streaming_state(StreamingState::Streamout);
                    if self.streaming_song().is_some_and(|s| s.retry.is_none()) {
                        return;
                    }
                } else {
                    self.set_streaming_state(StreamingState::Idle);
                }
                if self.will_retry() {
                    return;
                }
                self.get_next_track(&p, true);
            }
            StartStreamout => {
                self.set_streaming_state(StreamingState::Streamout);
                self.a_start();
            }
            Start => self.a_start(),
            AutoStart => {
                self.set_streaming_state(StreamingState::Streamout);
                if self
                    .streaming_song()
                    .map(|s| s.status != SongStatus::Playing)
                    .unwrap_or(false)
                {
                    info!("{}: autostart possibly short track", self.master_id);
                    for pl in self.active_players() {
                        pl.strm_unpause(0);
                    }
                }
            }
            RetryOrNext => self.a_retry_or_next(p),
            Stopped => {
                self.set_playing_state(PlayingState::Stopped);
                self.notify_stopped(false);
            }
            Buffering => self.set_playing_state(PlayingState::Buffering),
            Rebuffer => self.a_rebuffer(),
            CheckSync => self.a_check_sync(),
            CheckPaused => self.a_check_paused(),
        }
    }

    fn set_playing_state(&mut self, s: PlayingState) {
        self.playing_state = s;
    }

    fn set_streaming_state(&mut self, s: StreamingState) {
        self.streaming_state = s;
    }

    fn notify(&self, words: &[&str]) {
        if let Some(server) = self.server() {
            server.notifier.notify(Some(self.master_id), words);
        }
    }

    fn notify_stopped(&self, suppress: bool) {
        if !suppress {
            self.notify(&["playlist", "stop"]);
        }
    }

    // ---- actions --------------------------------------------------------------------

    fn stop_client(pl: &Arc<Player>) {
        pl.strm_stop();
    }

    fn a_stop(&mut self, suppress: bool) {
        let suppress = suppress || !(self.is_playing(false) || self.is_paused());
        for pl in self.active_players() {
            Self::stop_client(&pl);
        }
        while self.queue.len() > 1 {
            self.queue.pop_front();
        }
        if let Some(s) = self.queue.front_mut() {
            s.status = SongStatus::Finished;
        }
        self.set_playing_state(PlayingState::Stopped);
        self.set_streaming_state(StreamingState::Idle);
        self.notify_stopped(suppress);
    }

    fn a_playing(&mut self, player_reported: bool) {
        if player_reported {
            // A song the server already promoted to playing (via `_Start`) has now really
            // started on the player: record that, but do not retire it.
            if let Some(back) = self.queue.back_mut() {
                if back.status == SongStatus::Playing && !back.start_reported {
                    back.start_reported = true;
                    if !self.rebuffering {
                        self.set_playing_state(PlayingState::Playing);
                    }
                    return;
                }
            }
        }
        if self.fade_active {
            let dur = self
                .master()
                .map(|m| m.prefs.get_f64("fadeInDuration"))
                .unwrap_or(0.0);
            for pl in self.active_players() {
                if let Some(server) = self.server() {
                    server.fade_volume(&pl, dur, None);
                }
            }
        }
        self.fade_active = false;
        if !self.rebuffering {
            self.set_playing_state(PlayingState::Playing);
        }
        self.consecutive_errors = 0;
        while self.queue.len() > 1 {
            let st = self.queue.back().map(|s| s.status).unwrap();
            if matches!(
                st,
                SongStatus::Playing | SongStatus::Failed | SongStatus::Finished
            ) {
                self.queue.pop_back();
            } else {
                break;
            }
        }
        let now = crate::util::now();
        self.track_start_time = now;
        let mut newsong: Option<(String, usize, String)> = None;
        if let Some(song) = self.queue.back_mut() {
            song.status = SongStatus::Playing;
            song.start_reported = player_reported;
            // we are playing so we must be done retrying
            song.retry = None;
            song.started_at = Some(now);
            newsong = Some((standard_title(song), song.index, song.track.url.clone()));
        }
        if let Some((title, index, url)) = newsong {
            self.playlist.current = index;
            if let Some(server) = self.server() {
                if !crate::util::is_remote_url(&url) {
                    let lib = server.library.clone();
                    let u = url.clone();
                    tokio::task::spawn_blocking(move || lib.mark_played(&u));
                }
                if let Some(m) = self.master() {
                    m.prefs.set("currentSong", json!(index));
                }
                server.notifier.notify_owned(
                    Some(self.master_id),
                    vec![
                        "playlist".into(),
                        "newsong".into(),
                        title,
                        index.to_string(),
                    ],
                );
            }
        }
    }

    fn a_pause(&mut self) {
        self.resume_time = Some(self.playing_song_elapsed());
        self.set_playing_state(PlayingState::Paused);
        let me = self.me.clone();
        if let Some(server) = self.server() {
            let players = self.active_players();
            for pl in &players {
                if pl.id == self.master_id {
                    let me = me.clone();
                    server.fade_volume(
                        pl,
                        -FADEVOLUME,
                        Some(Box::new(move || {
                            if let Some(c) = me.upgrade() {
                                let c = c.lock();
                                if !c.is_paused() {
                                    return;
                                }
                                for p in c.active_players() {
                                    p.strm_pause(0);
                                }
                            }
                        })),
                    );
                } else {
                    server.fade_volume(pl, -FADEVOLUME, None);
                }
            }
            if players.iter().all(|p| p.id != self.master_id) {
                for p in &players {
                    p.strm_pause(0);
                }
            }
        }
        self.notify(&["playlist", "pause", "1"]);
    }

    fn a_resume(&mut self) {
        let paused_at = self.resume_time.unwrap_or(0.0)
            - self.playing_song().map(|s| s.start_offset).unwrap_or(0.0);
        let server = self.server();
        let sync_delay = server
            .as_ref()
            .map(|s| s.prefs.server.get_i64("syncStartDelay"))
            .filter(|d| *d > 0)
            .unwrap_or(200) as f64
            / 1000.0;
        let start_at_base = crate::util::now() + sync_delay;
        if self.fade_in.is_none() {
            let d = self
                .master()
                .map(|m| m.prefs.get_f64("fadeInDuration"))
                .unwrap_or(0.0);
            if d > 0.0 {
                self.fade_in = Some(d);
            }
        }
        self.set_playing_state(PlayingState::Playing);
        let players = self.active_players();
        let synced = players.len() > 1;
        for pl in &players {
            pl.set_volume_temp_silent(0);
            if synced {
                let mut start_at = start_at_base;
                if let Some((_, _, elapsed)) = pl.play_point() {
                    let delay = elapsed - paused_at;
                    if delay >= 0.0 {
                        start_at += delay;
                    }
                }
                pl.strm_unpause(pl.jiffies_for_time(start_at));
            } else {
                pl.strm_unpause(0);
            }
            if let Some(s) = &server {
                s.fade_volume(pl, self.fade_in.unwrap_or(FADEVOLUME), None);
            }
        }
        self.next_check_sync = start_at_base + 2.0;
        self.notify(&["playlist", "pause", "0"]);
        self.resume_time = None;
        self.fade_in = None;
    }

    fn a_start(&mut self) {
        if !self.rebuffering {
            if let Some(f) = self.fade_in {
                if let Some(server) = self.server() {
                    for pl in self.active_players() {
                        if f > pl.state.lock().caps.max_transition_duration as f64 {
                            server.fade_volume(&pl, f, None);
                        }
                    }
                }
            }
            self.fade_in = None;
            let players = self.active_players();
            if players.len() > 1 {
                self.sync_start(&players);
            } else if let Some(m) = self.master() {
                m.strm_unpause(0);
            }
            self.a_playing(false);
        } else {
            self.rebuffering = false;
            self.a_resume();
        }
    }

    fn sync_start(&self, players: &[Arc<Player>]) {
        let delay_of =
            |p: &Arc<Player>| (p.prefs.get_i64("startDelay") + p.prefs.get_i64("playDelay")) as f64;
        let max_delay = players.iter().map(delay_of).fold(0.0, f64::max);
        let sync_delay = self
            .server()
            .map(|s| s.prefs.server.get_i64("syncStartDelay"))
            .filter(|d| *d > 0)
            .unwrap_or(200) as f64;
        let start_at = crate::util::now() + (max_delay + sync_delay) / 1000.0;
        for p in players {
            let at = start_at - delay_of(p) / 1000.0;
            p.strm_unpause(p.jiffies_for_time(at));
        }
    }

    fn a_start_if_ready(&mut self) {
        if self.active_players().iter().all(|p| p.is_buffer_ready()) {
            self.a_start();
        }
    }

    fn a_stream_if_ready(&mut self, p: EvParams) {
        if self.next_track.is_none() {
            return;
        }
        if self
            .active_players()
            .iter()
            .all(|pl| pl.is_ready_to_stream())
        {
            self.a_stream(p);
        }
    }

    fn a_next_if_more(&mut self, p: EvParams) {
        self.set_streaming_state(StreamingState::Idle);
        if self.queue.len() < 2 {
            self.get_next_track(&p, true);
        } else {
            info!(
                "{}: streaming track not started yet, will wait until then to try next track",
                self.master_id
            );
        }
    }

    fn a_retry_or_next(&mut self, p: EvParams) {
        self.set_streaming_state(StreamingState::Idle);
        let elapsed = self.playing_song_elapsed();
        let same = match (self.streaming_song(), self.playing_song()) {
            (Some(a), Some(b)) => a.id == b.id,
            _ => false,
        };
        if same {
            if let Some(song) = self.streaming_song() {
                if song.is_remote() && elapsed > 10.0 && song.is_live() {
                    info!(
                        "{}: attempting to re-stream {} after {:.0}s",
                        self.master_id,
                        song.url(),
                        elapsed
                    );
                    let id = song.id;
                    if let Some(s) = self.queue.front_mut() {
                        s.retry = Some(RetryData {
                            count: 0,
                            start: crate::util::now(),
                        });
                    }
                    self.a_stream(EvParams {
                        song_id: Some(id),
                        ..Default::default()
                    });
                    return;
                }
            }
        }
        if self.queue.len() < 2 {
            self.get_next_track(&p, true);
        }
    }

    fn a_continue(&mut self, p: EvParams) {
        // Without byte-exact seek data we resume by time, as LMS does for most handlers.
        let t = self.playing_song_elapsed();
        info!(
            "{}: restarting playback at time offset {:.1}",
            self.master_id, t
        );
        let _ = p;
        self.a_jump_to_time(EvParams {
            newtime: Some(t.to_string()),
            restart_if_no_seek: true,
            ..Default::default()
        });
    }

    fn a_jump_to_time(&mut self, p: EvParams) {
        let Some(song) = self.playing_song().cloned() else {
            return;
        };
        let newtime_s = p.newtime.clone().unwrap_or_else(|| "0".into());
        let relative = newtime_s.starts_with('+') || newtime_s.starts_with('-');
        let mut newtime: f64 = newtime_s.parse().unwrap_or(0.0);
        if (!relative && newtime == 0.0) || song.duration().is_none() {
            self.a_stop(true);
            let mut s = song.clone();
            s.start_offset = 0.0;
            let id = self.requeue(s);
            self.a_stream(EvParams {
                song_id: Some(id),
                ..Default::default()
            });
            return;
        }
        if relative {
            newtime += self.playing_song_elapsed();
            if newtime < 0.0 {
                newtime = 0.0;
            }
        }
        if newtime > song.duration().unwrap_or(0.0) {
            self.a_stop(false);
            self.get_next_track(&EvParams::default(), false);
            return;
        }
        if !song.can_seek && !p.restart_if_no_seek {
            return;
        }
        self.a_stop(true);
        let id = self.requeue(song);
        self.a_stream(EvParams {
            song_id: Some(id),
            seek: if song_can_seek_now(&self.queue, id) {
                Some(newtime)
            } else {
                None
            },
            ..Default::default()
        });
    }

    /// Put `song` (a clone of a previous queue entry) back as the only queue entry.
    fn requeue(&mut self, mut song: Song) -> u64 {
        song.status = SongStatus::Ready;
        let id = song.id;
        self.queue.clear();
        self.queue.push_back(song);
        id
    }

    fn a_jump_paused(&mut self, p: EvParams) {
        let Some(song) = self.playing_song().cloned() else {
            return;
        };
        if !song.can_seek {
            if p.restart_if_no_seek {
                self.a_stop(false);
            }
            return;
        }
        let newtime_s = p.newtime.clone().unwrap_or_else(|| "0".into());
        let mut newtime: f64 = newtime_s.parse().unwrap_or(0.0);
        if newtime_s.starts_with('+') || newtime_s.starts_with('-') {
            newtime += self.resume_time.unwrap_or(0.0);
            if newtime < 0.0 {
                newtime = 0.0;
            }
        }
        self.resume_time = Some(newtime);
        self.pause_streaming();
    }

    fn pause_streaming(&mut self) {
        if self.streaming_state == StreamingState::Idle {
            return;
        }
        for pl in self.active_players() {
            Self::stop_client(&pl);
        }
        self.set_streaming_state(StreamingState::Idle);
        let playing_id = self.playing_song().map(|s| s.id);
        if let Some(s) = self.queue.back_mut() {
            s.status = SongStatus::Ready;
        }
        if self.queue.front().map(|s| s.id) != playing_id {
            self.queue.pop_front();
        }
    }

    fn a_rebuffer(&mut self) {
        self.set_playing_state(PlayingState::Buffering);
        self.rebuffering = true;
        for pl in self.active_players() {
            pl.strm_pause(0);
            pl.state.lock().buffer_ready = false;
        }
        self.resume_time = Some(self.playing_song_elapsed());
        // Poll buffer fullness like `Player::rebuffer` / `_buffering`.
        let me = self.me.clone();
        let threshold = {
            let bitrate = self
                .streaming_song()
                .and_then(|s| s.stream_bitrate)
                .unwrap_or(0);
            let t = if bitrate > 0 {
                bitrate as u64 / 8 * 5
            } else {
                80 * 1024
            };
            t.max(1)
        };
        tokio::spawn(async move {
            let start = crate::util::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(125)).await;
                let Some(c) = me.upgrade() else { return };
                let mut ctrl = c.lock();
                if !ctrl.rebuffering || ctrl.playing_state != PlayingState::Buffering {
                    return;
                }
                let players = ctrl.active_players();
                for p in &players {
                    p.request_status();
                }
                let ready = players.iter().all(|p| {
                    let s = p.state.lock();
                    let cap = (s.buffer_size as u64).saturating_sub(4000).max(1);
                    s.buffer_fullness as u64 >= threshold.min(cap)
                });
                if ready || crate::util::now() - start > 30.0 {
                    for p in &players {
                        p.state.lock().buffer_ready = true;
                    }
                    ctrl.event(Event::BufferReady, EvParams::default());
                    return;
                }
            }
        });
    }

    fn a_check_sync(&mut self) {
        let players = self.active_players();
        if players.len() < 2 {
            return;
        }
        let now = crate::util::now();
        if now < self.next_check_sync {
            return;
        }
        self.next_check_sync = now + CHECK_SYNC_INTERVAL;
        let recent = now - PLAYPOINT_RECENT_THRESHOLD;
        let mut points: Vec<(Arc<Player>, f64)> = Vec::new();
        for p in &players {
            if p.prefs.exists("maintainSync") && p.prefs.get_i64("maintainSync") == 0 {
                continue;
            }
            let Some(pp) = p.play_point() else { return };
            if pp.0 <= recent {
                return;
            }
            points.push((
                p.clone(),
                pp.1 + p.prefs.get_i64("playDelay") as f64 / 1000.0,
            ));
        }
        if points.is_empty() {
            return;
        }
        // decreasing apparent start time: most-behind first
        points.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // All slimproto players support skipAhead, so the reference is the last (most ahead).
        let reference = points.len() - 1;
        let ref_time = points[reference].1;
        for (i, (p, t)) in points.iter().enumerate() {
            if i == reference {
                continue;
            }
            let delta = (t - ref_time).abs();
            let min_adj = p.prefs.get_i64("minSyncAdjust") as f64 / 1000.0;
            if !(MIN_DEVIATION_ADJUST..=MAX_DEVIATION_ADJUST).contains(&delta) || delta < min_adj {
                continue;
            }
            if i < reference {
                info!("{} resync: skipAhead {}ms", p.id, (delta * 1000.0) as u32);
                p.strm_skip_ahead((delta * 1000.0) as u32);
                self.next_check_sync += 1.0;
            } else {
                info!("{} resync: pauseFor {}ms", p.id, (delta * 1000.0) as u32);
                p.strm_pause((delta * 1000.0) as u32);
                self.next_check_sync += delta;
            }
        }
    }

    fn a_check_paused(&mut self) {
        let Some(song) = self.playing_song() else {
            return;
        };
        if !song.is_remote() {
            return;
        }
        let Some(m) = self.master() else { return };
        let usage = {
            let s = m.state.lock();
            if s.buffer_size == 0 {
                0.0
            } else {
                s.buffer_fullness as f64 / s.buffer_size as f64
            }
        };
        if usage > 0.98 {
            if song.can_seek && self.resume_time.is_some() {
                self.pause_streaming();
            } else if song.duration().is_none() {
                info!(
                    "{}: stopping remote stream upon full buffer when paused",
                    self.master_id
                );
                self.a_stop(false);
            }
        }
    }

    /// `_willRetry`: only a remote stream that already played (it has [`RetryData`]) is
    /// retried.  The retry goes through TRACKWAIT and the next-track callback id, so the
    /// state table decides what the timer may do (a user `stop` in between cancels it).
    fn will_retry(&mut self) -> bool {
        const RETRY_INTERVALS: [f64; 4] = [5.0, 10.0, 15.0, 30.0];
        const RETRY_LIMIT: f64 = 5.0 * 60.0;
        const RETRY_LIMIT_PLAYLIST: f64 = 30.0;

        let Some((song_id, index, url, retry)) = self
            .streaming_song()
            .map(|s| (s.id, s.index, s.url().to_string(), s.retry))
        else {
            return false;
        };
        let Some(retry) = retry else {
            info!("{}: no retry data", self.master_id);
            return false;
        };
        let limit = match self.next_index(None) {
            Some(next) if next != index => RETRY_LIMIT_PLAYLIST,
            _ => RETRY_LIMIT,
        };
        let interval = RETRY_INTERVALS[retry.count.min(RETRY_INTERVALS.len() - 1)];
        let retry_time = crate::util::now() + interval;
        if retry.start + limit < retry_time {
            // too late, give up
            if let Some(s) = self.queue.front_mut() {
                s.retry = None;
            }
            warn!(
                "{}: cannot open {}: RETRY_LIMIT_EXCEEDED",
                self.master_id, url
            );
            if let Some(server) = self.server() {
                server.notifier.notify_owned(
                    Some(self.master_id),
                    vec![
                        "playlist".into(),
                        "cant_open".into(),
                        url,
                        "RETRY_LIMIT_EXCEEDED".into(),
                    ],
                );
            }
            self.a_stop(false);
            // the failed retry counts as one error
            self.consecutive_errors = 1;
            return false;
        }
        if let Some(s) = self.queue.front_mut() {
            if let Some(r) = s.retry.as_mut() {
                r.count += 1;
            }
        }
        self.next_track_cb_id += 1;
        let cb_id = self.next_track_cb_id;
        self.next_track = None;
        self.set_streaming_state(StreamingState::TrackWait);
        info!("{}: will retry {} in {}s", self.master_id, url, interval);
        let me = self.me.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64(interval)).await;
            let Some(c) = me.upgrade() else { return };
            let mut c = c.lock();
            if let Some(s) = c.queue.iter_mut().find(|s| s.id == song_id) {
                s.status = SongStatus::Ready;
            }
            c.consecutive_errors = 0;
            c.next_track_ready(cb_id, song_id);
        });
        true
    }

    // ---- next track -----------------------------------------------------------------

    /// `nextsong`: playlist index following `cur` according to repeat mode.
    fn next_index(&mut self, cur: Option<usize>) -> Option<usize> {
        let count = self.playlist.len();
        if count == 0 {
            return None;
        }
        let cur = cur.unwrap_or_else(|| self.streaming_song().map(|s| s.index).unwrap_or(0));
        let mut repeat = self.playlist.repeat;
        if self.consecutive_errors >= 2 {
            if count == 1 {
                warn!(
                    "{}: giving up because of too many consecutive errors",
                    self.master_id
                );
                return None;
            } else if repeat == 1 {
                repeat = 2;
            }
        }
        if repeat == 1 {
            return Some(cur);
        }
        if self.consecutive_errors > count {
            return None;
        }
        let mut next = cur + 1;
        if next >= count {
            if self.playlist.shuffle != 0 && repeat == 2 {
                if let Some(s) = self.server() {
                    if s.prefs.server.get_bool("reshuffleOnRepeat") {
                        self.playlist.reshuffle(false);
                    }
                }
            }
            next = 0;
        }
        if repeat == 0 && next == 0 {
            return None;
        }
        Some(next)
    }

    fn get_next_track(&mut self, p: &EvParams, if_more: bool) {
        if self.consecutive_errors > self.playlist.len() {
            warn!(
                "{}: giving up because of too many consecutive errors: {}",
                self.master_id, self.consecutive_errors
            );
            return;
        }
        self.next_track_cb_id += 1;
        let id = self.next_track_cb_id;
        self.next_track = None;
        let index = match p.index {
            Some(i) => Some(i),
            None => {
                let old = p.error_index;
                self.next_index(old)
            }
        };
        let index = match index {
            Some(i) => i,
            None => {
                if if_more {
                    return;
                }
                0
            }
        };
        let Some(entry) = self.playlist.song(index).cloned() else {
            self.set_streaming_state(StreamingState::TrackWait);
            self.next_track_error(id, index, "PROBLEM_OPENING");
            return;
        };
        let Some(server) = self.server() else { return };
        let mut song = Song::new(index, server.track_info_for(&entry));
        song.start_offset = p.seek.unwrap_or(0.0);
        self.set_streaming_state(StreamingState::TrackWait);
        let song_id = song.id;
        let needs_probe = song.track.remote;
        self.queue.push_front(song);
        while self.queue.len() > 1
            && matches!(
                self.queue.back().map(|s| s.status),
                Some(SongStatus::Failed | SongStatus::Finished)
            )
        {
            self.queue.pop_back();
        }
        if needs_probe && crate::plugins::spotify::is_spotify(&entry.url) {
            // Spotify: no stream to probe, just the track's metadata
            let me = self.me.clone();
            let url = entry.url.clone();
            tokio::spawn(async move {
                let meta = match crate::plugins::spotify::get() {
                    Some(sp) => sp.track_meta(&url).await,
                    None => None,
                };
                let Some(c) = me.upgrade() else { return };
                let mut c = c.lock();
                if c.next_track_cb_id != id {
                    return;
                }
                if let Some(s) = c.queue.iter_mut().find(|s| s.id == song_id) {
                    s.track = crate::server::spotify_track_info(&url);
                    if let Some(m) = meta {
                        s.remote_meta.title = Some(m.title);
                        s.remote_meta.artist = Some(m.artist).filter(|a| !a.is_empty());
                        s.remote_meta.album = Some(m.album).filter(|a| !a.is_empty());
                        s.remote_meta.cover = m.image;
                    }
                }
                c.next_track_ready(id, song_id);
            });
        } else if needs_probe {
            let me = self.me.clone();
            let url = entry.url.clone();
            tokio::spawn(async move {
                let resolved = server.resolve_remote(&url).await;
                let Some(c) = me.upgrade() else { return };
                let mut c = c.lock();
                if c.next_track_cb_id != id {
                    return;
                }
                match resolved {
                    Some(info) => {
                        if let Some(s) = c.queue.iter_mut().find(|s| s.id == song_id) {
                            s.track.content_type = info.content_type.clone();
                            s.track.bitrate = info.bitrate.or(s.track.bitrate);
                            s.remote_meta.station = info.title.clone();
                            s.remote_meta.content_type = if info.is_hls {
                                Some("hls".into())
                            } else {
                                None
                            };
                            if s.track.title.is_none() {
                                s.track.title = info.title.clone();
                            }
                            s.stream_url = Some(info.stream_url.clone());
                        }
                        c.next_track_ready(id, song_id);
                    }
                    None => c.next_track_error(id, index, "PROBLEM_CONNECTING"),
                }
            });
        } else {
            self.next_track_ready(id, song_id);
        }
    }

    fn next_track_ready(&mut self, cb_id: u64, song_id: u64) {
        if self.next_track_cb_id != cb_id {
            return;
        }
        self.next_track = Some(song_id);
        self.event(Event::NextTrackReady, EvParams::default());
    }

    fn next_track_error(&mut self, cb_id: u64, index: usize, err: &str) {
        if self.next_track_cb_id != cb_id {
            return;
        }
        if let Some(s) = self
            .queue
            .iter_mut()
            .find(|s| s.index == index && s.status == SongStatus::Ready)
        {
            s.status = SongStatus::Failed;
        }
        self.consecutive_errors += 1;
        let url = self
            .playlist
            .song(index)
            .map(|e| e.url.clone())
            .unwrap_or_default();
        warn!("{}: cannot open {}: {}", self.master_id, url, err);
        if let Some(server) = self.server() {
            server.notifier.notify_owned(
                Some(self.master_id),
                vec!["playlist".into(), "cant_open".into(), url, err.into()],
            );
        }
        self.event(
            Event::NextTrackError,
            EvParams {
                error_index: Some(index),
                ..Default::default()
            },
        );
    }

    // ---- streaming ------------------------------------------------------------------

    fn a_stream(&mut self, p: EvParams) {
        let Some(server) = self.server() else { return };
        let song_id = p.song_id.or(self.next_track);
        let Some(song_id) = song_id else {
            error!("{}: no song to stream", self.master_id);
            self.a_next_if_more(EvParams::default());
            return;
        };
        // queue maintenance (bug 10510 / 12653)
        let mut song = match self.queue.iter().position(|s| s.id == song_id) {
            Some(pos) => self.queue.remove(pos).unwrap(),
            None => {
                error!("{}: song {} not in queue", self.master_id, song_id);
                return;
            }
        };
        while matches!(
            self.queue.back().map(|s| s.status),
            Some(SongStatus::Failed | SongStatus::Finished | SongStatus::Ready)
        ) {
            self.queue.pop_back();
        }
        while matches!(
            self.queue.front().map(|s| s.status),
            Some(SongStatus::Failed | SongStatus::Finished | SongStatus::Ready)
        ) {
            self.queue.pop_front();
        }
        if self.queue.len() >= 2 {
            error!(
                "{}: aborting streaming because songqueue too long",
                self.master_id
            );
            while self.queue.len() > 1 {
                self.queue.pop_front();
            }
        }

        let players = self.active_players();
        if players.is_empty() {
            warn!("{}: no active players to stream to", self.master_id);
            self.queue.push_front(song);
            return;
        }
        let seek = p.seek.or(if song.start_offset > 0.0 {
            Some(song.start_offset)
        } else {
            None
        });
        let opened = match stream::open_song(&server, &mut song, &players, seek) {
            Ok(o) => o,
            Err(e) => {
                warn!("{}: problem opening {}: {}", self.master_id, song.url(), e);
                server.notifier.notify_owned(
                    Some(self.master_id),
                    vec![
                        "playlist".into(),
                        "cant_open".into(),
                        song.url().to_string(),
                        e,
                    ],
                );
                song.status = SongStatus::Failed;
                let idx = song.index;
                self.consecutive_errors += 1;
                self.queue.push_front(song);
                self.a_next_if_more(EvParams {
                    error_index: Some(idx),
                    ..Default::default()
                });
                return;
            }
        };
        song.stream_format = opened.stream_format;
        song.spec = opened.spec;
        song.direct_url = opened.direct_url;
        song.stream_bitrate = opened.stream_bitrate;
        song.can_seek = opened.can_seek && song.duration().is_some();
        song.status = SongStatus::Streaming;
        self.notify(&["playlist", "open", song.url()]);

        let paused = players.len() > 1
            && matches!(
                self.playing_state,
                PlayingState::Stopped | PlayingState::Buffering
            );
        let fade_in = self.fade_in.unwrap_or(0.0);
        let paused = paused || fade_in > 0.0;
        let master = players[0].clone();
        let master_prefs = master.prefs.clone();
        self.fade_active = fade_in < 1.0
            && self.playing_state == PlayingState::Stopped
            && master_prefs.get_f64("fadeInDuration") > 0.0;

        // transitions (stream_s)
        let mut transition = TransitionType::from_pref(master_prefs.get_i64("transitionType"));
        let mut tdur = master_prefs.get_f64("transitionDuration").max(0.0);
        if transition != TransitionType::None
            && master_prefs.get_bool("transitionSmart")
            && (self.playlist.len() < 2 || self.adjacent_same_album(song.index))
        {
            transition = TransitionType::None;
        }
        if !self.is_playing(true) {
            if transition == TransitionType::FadeIn {
                transition = TransitionType::None;
            } else if transition == TransitionType::FadeInOut {
                transition = TransitionType::FadeOut;
            }
        }
        if transition != TransitionType::None {
            if let Some(d) = song.duration() {
                if d < tdur * 2.0 {
                    tdur = d / 3.0;
                }
            }
        }
        let replay_gain = replay_gain_for(&master_prefs, &song, &self.playlist);
        song.replay_gain_db = replay_gain.unwrap_or(0.0);
        let buffer_threshold = if song.is_remote() {
            let secs = server.prefs.server.get_i64("bufferSecs").max(1) as u64;
            Some(match song.stream_bitrate {
                Some(b) if b > 0 => ((b as u64 / 8 * secs) / 1000).clamp(1, 255) as u8,
                _ => 20,
            })
        } else {
            None
        };

        if self.playing_state == PlayingState::Stopped {
            let master_vol = players
                .iter()
                .find(|p| p.prefs.get_bool("syncVolume"))
                .map(|p| p.prefs.get_i64("volume").abs() as i32);
            for pl in &players {
                let v = if pl.prefs.get_bool("syncVolume") {
                    master_vol.unwrap_or(pl.volume())
                } else {
                    pl.volume()
                };
                pl.set_volume(v, false);
                if self.fade_active {
                    pl.set_volume_temp_silent(0);
                }
            }
        }

        // Several players: read a generated stream once for all of them (files are just
        // read by each).
        let shared = match song.spec.as_ref().map(|s| &s.source) {
            Some(stream::SourceKind::File { .. }) | None => None,
            Some(_) if players.len() > 1 => Some(stream::shared::SharedSource::new(players.len())),
            Some(_) => None,
        };
        let mut started = 0;
        for pl in &players {
            if song.is_remote() {
                pl.state.lock().remote_stream_start = Some(crate::util::now());
            }
            let my_fade = if fade_in > pl.state.lock().caps.max_transition_duration as f64 {
                0.0
            } else {
                fade_in
            };
            let params = PlayParams {
                paused,
                reconnect: p.reconnect,
                fade_in: my_fade,
                replay_gain_db: replay_gain,
                buffer_threshold,
                transition,
                transition_duration: tdur.round() as u8,
                shared: shared.clone(),
            };
            if stream::send_stream_start(&server, pl, &song, &params) {
                started += 1;
            }
            pl.send_volume();
        }
        let idx = song.index;
        self.queue.push_front(song);
        if started != players.len() {
            warn!(
                "{}: only {} of {} players started streaming",
                self.master_id,
                started,
                players.len()
            );
            self.a_stop(false);
            self.consecutive_errors += 1;
            self.a_next_if_more(EvParams {
                error_index: Some(idx),
                ..Default::default()
            });
            return;
        }
        self.next_track = None;
        if self.playing_state == PlayingState::Stopped {
            self.set_playing_state(PlayingState::Buffering);
        }
        self.set_streaming_state(StreamingState::Streaming);
        if let Some(m) = self.master() {
            m.save_current_song(idx);
        }
    }

    fn adjacent_same_album(&self, index: usize) -> bool {
        let key = |i: usize| self.playlist.song(i).and_then(|e| e.album_key.clone());
        let cur = key(index);
        cur.is_some() && ((index > 0 && key(index - 1) == cur) || key(index + 1) == cur)
    }

    // ---- public command interface (PlayControl) ------------------------------------

    pub fn stop(&mut self) {
        self.event(Event::Stop, EvParams::default());
    }

    pub fn play(&mut self, index: Option<usize>, seek: Option<f64>, fade_in: Option<f64>) {
        self.consecutive_errors = 0;
        if let Some(f) = fade_in.filter(|f| *f > 0.0) {
            self.fade_in = Some(f);
        }
        self.event(
            Event::Play,
            EvParams {
                index,
                seek,
                ..Default::default()
            },
        );
    }

    pub fn skip(&mut self) {
        self.consecutive_errors = 0;
        self.event(Event::Skip, EvParams::default());
    }

    pub fn pause(&mut self) {
        self.event(Event::Pause, EvParams::default());
    }

    pub fn resume(&mut self, fade_in: Option<f64>) {
        if let Some(f) = fade_in.filter(|f| *f > 0.0) {
            self.fade_in = Some(f);
        }
        self.event(Event::Resume, EvParams::default());
    }

    pub fn flush(&mut self) {
        self.event(Event::Flush, EvParams::default());
    }

    pub fn jump_to_time(&mut self, newtime: &str, restart_if_no_seek: bool) {
        self.event(
            Event::JumpToTime,
            EvParams {
                newtime: Some(newtime.to_string()),
                restart_if_no_seek,
                ..Default::default()
            },
        );
    }

    /// Select a song without playing (`resetSongqueue`).
    pub fn reset_song_queue(&mut self, index: Option<usize>) {
        self.queue.clear();
        if let Some(i) = index {
            if let (Some(e), Some(server)) = (self.playlist.song(i).cloned(), self.server()) {
                self.queue
                    .push_back(Song::new(i, server.track_info_for(&e)));
            }
            self.playlist.current = i;
        }
    }

    /// Called after the playlist grew while idle: restart streaming if needed.
    pub fn next_if_streamed(&mut self) {
        if self.streaming_state == StreamingState::Idle
            && self.playing_state != PlayingState::Stopped
            && self.playing_state != PlayingState::Paused
        {
            self.get_next_track(&EvParams::default(), true);
        }
    }

    // ---- player notifications -------------------------------------------------------

    pub fn player_stopped(&mut self, client: MacAddress) {
        if !self.is_master(client) {
            return;
        }
        if self.queue.len() > 1 {
            self.queue.pop_back();
        } else {
            let at_end = self
                .playing_song()
                .map(|s| s.status == SongStatus::Playing && s.index + 1 == self.playlist.len())
                .unwrap_or(false);
            if let Some(s) = self.queue.back_mut() {
                if s.status == SongStatus::Playing {
                    s.status = SongStatus::Finished;
                }
            }
            if at_end && self.playlist.repeat == 0 {
                self.reset_song_queue(Some(0));
            }
        }
        self.event(Event::Stopped, EvParams::default());
    }

    pub fn player_track_started(&mut self, client: MacAddress) {
        if !self.is_master(client) {
            return;
        }
        if self.queue.len() == 1 && self.players.len() > 1 {
            return;
        }
        self.event(Event::Started, EvParams::default());
    }

    pub fn player_ready_to_stream(&mut self, _client: MacAddress) {
        self.event(Event::ReadyToStream, EvParams::default());
    }

    pub fn player_output_underrun(&mut self, _client: MacAddress) {
        self.event(Event::OutputUnderrun, EvParams::default());
    }

    pub fn player_streaming_failed(
        &mut self,
        _client: MacAddress,
        error: &str,
        error_disconnect: bool,
    ) {
        let mut url = String::new();
        if let Some(s) = self.queue.front_mut() {
            s.status = SongStatus::Failed;
            url = s.track.url.clone();
        }
        if self.queue.len() > 1 {
            self.queue.pop_front();
        }
        warn!(
            "{}: streaming failed for {}: {}",
            self.master_id, url, error
        );
        if !url.is_empty() {
            if let Some(server) = self.server() {
                server.notifier.notify_owned(
                    Some(self.master_id),
                    vec![
                        "playlist".into(),
                        "cant_open".into(),
                        url,
                        error.to_string(),
                    ],
                );
            }
        }
        self.consecutive_errors += 1;
        self.event(
            Event::StreamingFailed,
            EvParams {
                error_disconnect,
                ..Default::default()
            },
        );
    }

    pub fn player_buffer_ready(&mut self, _client: MacAddress) {
        self.event(Event::BufferReady, EvParams::default());
    }

    pub fn player_end_of_stream(&mut self, client: MacAddress) {
        if !self.is_master(client) {
            return;
        }
        self.event(Event::EndOfStream, EvParams::default());
    }

    pub fn player_status_heartbeat(&mut self, client: MacAddress) {
        if !self.is_master(client) {
            return;
        }
        self.event(Event::StatusHeartbeat, EvParams::default());
    }

    /// The server finished sending the current local stream.
    pub fn local_end_of_stream(&mut self, song_id: u64) {
        if self.streaming_song().map(|s| s.id) != Some(song_id) {
            return;
        }
        self.event(Event::LocalEndOfStream, EvParams::default());
    }

    pub fn player_reconnect(&mut self) {
        if self.streaming_song().is_some() {
            self.event(Event::ContinuePlay, EvParams::default());
        }
    }

    // ---- sync group membership ------------------------------------------------------

    pub fn is_active(&self, id: MacAddress) -> bool {
        self.players
            .iter()
            .filter_map(Weak::upgrade)
            .any(|p| p.id == id)
    }

    /// `playerActive`.
    pub fn player_active(&mut self, player: &Arc<Player>) {
        if self.is_active(player.id) {
            return;
        }
        if self.is_paused() && self.playing_song().is_some() {
            self.pause_streaming();
        }
        if let Some(m) = self.master() {
            if !m.power() && self.is_active(m.id) {
                if !self.is_stopped() && !self.is_paused() {
                    self.a_stop(false);
                }
                self.players.clear();
            }
        }
        self.players.push(Arc::downgrade(player));
        self.new_master();
        if self.is_playing(true) {
            let t = self.playing_song_elapsed();
            self.a_jump_to_time(EvParams {
                newtime: Some(t.to_string()),
                restart_if_no_seek: true,
                ..Default::default()
            });
        }
    }

    /// `playerInactive`.
    pub fn player_inactive(&mut self, player: &Arc<Player>) {
        if let Some(pos) = self
            .players
            .iter()
            .position(|w| w.upgrade().map(|p| p.id == player.id).unwrap_or(false))
        {
            if !self.is_stopped() {
                if self.players.len() == 1 {
                    self.a_stop(false);
                } else {
                    Self::stop_client(player);
                }
            }
            self.players.remove(pos);
        }
        if player.id == self.master_id && !self.players.is_empty() {
            self.new_master();
        }
    }

    fn new_master(&mut self) {
        self.players.retain(|w| w.upgrade().is_some());
        self.all_players.retain(|w| w.upgrade().is_some());
        let new = self
            .players
            .first()
            .and_then(Weak::upgrade)
            .or_else(|| self.all_players.first().and_then(Weak::upgrade));
        if let Some(p) = new {
            if p.id != self.master_id {
                info!("new sync-group master {} (was {})", p.id, self.master_id);
                self.master_id = p.id;
            }
        }
    }

    pub(crate) fn add_member(&mut self, player: &Arc<Player>) {
        self.all_players.push(Arc::downgrade(player));
        if player.power() && player.is_connected() {
            self.players.push(Arc::downgrade(player));
            if self.master().map(|m| !m.power()).unwrap_or(true) {
                self.new_master();
            }
        }
    }

    pub(crate) fn remove_member(&mut self, player: &Arc<Player>) {
        if let Some(pos) = self
            .players
            .iter()
            .position(|w| w.upgrade().map(|p| p.id == player.id).unwrap_or(false))
        {
            if !self.is_stopped() {
                if self.players.len() == 1 {
                    self.a_stop(false);
                } else {
                    Self::stop_client(player);
                }
            } else {
                Self::stop_client(player);
            }
            self.players.remove(pos);
        }
        self.all_players
            .retain(|w| w.upgrade().map(|p| p.id != player.id).unwrap_or(false));
        if !self.all_players.is_empty() {
            self.new_master();
        }
    }

    pub(crate) fn after_join(&mut self, player: &Arc<Player>, no_restart: bool) {
        if !self.is_active(player.id) {
            return;
        }
        if self.is_paused() {
            self.pause_streaming();
        } else if !no_restart && !self.is_stopped() {
            let t = self.playing_song_elapsed();
            self.a_jump_to_time(EvParams {
                newtime: Some(t.to_string()),
                restart_if_no_seek: true,
                ..Default::default()
            });
        }
    }

    pub fn member_count(&self) -> usize {
        self.all_players
            .iter()
            .filter(|w| w.upgrade().is_some())
            .count()
    }

    // ---- persistence ----------------------------------------------------------------

    pub fn save_playlist(&self) {
        let Some(m) = self.master() else { return };
        let urls: Vec<&str> = self
            .playlist
            .entries
            .iter()
            .map(|e| e.url.as_str())
            .collect();
        m.prefs.set("playlist", json!(urls));
        m.prefs
            .set("playlistShuffledList", json!(self.playlist.order));
        m.prefs.set("currentSong", json!(self.playlist.current));
        m.prefs.set("shuffle", json!(self.playlist.shuffle));
        m.prefs.set("repeat", json!(self.playlist.repeat));
    }

    pub fn restore_playlist(&mut self, player: &Player, server: &Server) {
        let p = &player.prefs;
        self.playlist.shuffle = p.get_i64("shuffle").clamp(0, 2) as u8;
        self.playlist.repeat = p.get_i64("repeat").clamp(0, 2) as u8;
        let urls = p.get_string_list("playlist");
        if urls.is_empty() {
            return;
        }
        let order: Vec<usize> = p
            .get("playlistShuffledList")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let n = urls.len();
        self.playlist.entries = urls.into_iter().map(|u| server.entry_for_url(&u)).collect();
        self.playlist.order = if order.len() == n && order.iter().all(|&i| i < n) {
            order
        } else {
            (0..n).collect()
        };
        self.playlist.current = (p.get_i64("currentSong").max(0) as usize).min(n.saturating_sub(1));
        self.playlist.timestamp = crate::util::now();
        self.reset_song_queue(Some(self.playlist.current));
        info!("{}: restored playlist with {} tracks", player.id, n);
    }
}

fn song_can_seek_now(queue: &VecDeque<Song>, id: u64) -> bool {
    queue
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.can_seek || s.spec.is_none())
        .unwrap_or(true)
}

/// `Slim::Music::Info::standardTitle` (simplified: track title, or URL for unknown items).
pub fn standard_title(song: &Song) -> String {
    song.remote_meta
        .title
        .clone()
        .or_else(|| song.track.title.clone())
        .or_else(|| song.remote_meta.station.clone())
        .unwrap_or_else(|| title_from_url(&song.track.url))
}

pub fn title_from_url(url: &str) -> String {
    if let Some(p) = crate::util::path_from_file_url(url) {
        return p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| url.to_string());
    }
    url.to_string()
}

/// `Slim::Player::ReplayGain::fetchGainMode`: 0 none, 1 track, 2 album, 3 smart.
fn replay_gain_for(
    prefs: &crate::prefs::PrefStore,
    song: &Song,
    playlist: &Playlist,
) -> Option<f64> {
    let mode = prefs.get_i64("replayGainMode");
    if song.is_remote() {
        let rg = prefs.get_f64("remoteReplayGain");
        return if mode > 0 && rg != 0.0 {
            Some(rg)
        } else {
            None
        };
    }
    let t = &song.track;
    let gain = match mode {
        1 => t.replay_gain,
        2 => t.album_replay_gain.or(t.replay_gain),
        3 => {
            let key = playlist.song(song.index).and_then(|e| e.album_key.clone());
            let same_album = key.is_some()
                && ((song.index > 0
                    && playlist
                        .song(song.index - 1)
                        .and_then(|e| e.album_key.clone())
                        == key)
                    || playlist
                        .song(song.index + 1)
                        .and_then(|e| e.album_key.clone())
                        == key);
            if same_album {
                t.album_replay_gain.or(t.replay_gain)
            } else {
                t.replay_gain.or(t.album_replay_gain)
            }
        }
        _ => None,
    }?;
    // Prevent clipping with peak values (LMS preventClipping).
    let peak = t.replay_peak.unwrap_or(0.0);
    let mut g = gain;
    if peak > 0.0 {
        let max = -20.0 * peak.log10();
        if g > max {
            g = max;
        }
    }
    Some(g)
}

#[allow(unused)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Controller>();
    let _ = TrackInfo::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_spot_checks() {
        use PlayingState as P;
        use StreamingState as S;
        assert_eq!(
            action_for(Event::Play, P::Stopped, S::Idle),
            Action::StopGetNext
        );
        assert_eq!(
            action_for(Event::NextTrackReady, P::Stopped, S::TrackWait),
            Action::Stream
        );
        assert_eq!(
            action_for(Event::Started, P::Buffering, S::Streaming),
            Action::Playing
        );
        assert_eq!(
            action_for(Event::EndOfStream, P::Playing, S::Streaming),
            Action::AutoStart
        );
        assert_eq!(
            action_for(Event::ReadyToStream, P::Playing, S::Streamout),
            Action::RetryOrNext
        );
        assert_eq!(
            action_for(Event::Stopped, P::Playing, S::Idle),
            Action::Stopped
        );
        assert_eq!(
            action_for(Event::Stopped, P::Playing, S::TrackWait),
            Action::PlayIfReady
        );
        assert_eq!(
            action_for(Event::Pause, P::Paused, S::Idle),
            Action::JumpOrResume
        );
        assert_eq!(
            action_for(Event::BufferReady, P::WaitingToSync, S::Streaming),
            Action::StartIfReady
        );
        assert_eq!(
            action_for(Event::StatusHeartbeat, P::Playing, S::Idle),
            Action::CheckSync
        );
    }
}
