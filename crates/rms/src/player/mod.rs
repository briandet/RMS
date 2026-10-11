//! Players: identity, connection, capabilities and audio settings (`Slim::Player::Client`
//! / `Slim::Player::Squeezebox2` / `Slim::Player::SqueezePlay`).

pub mod connection;
pub mod controller;
pub mod playlist;
pub mod song;

use crate::prefs::PrefStore;
use crate::server::Server;
use parking_lot::{Mutex, RwLock};
use serde_json::json;
use slimproto::client::{Helo, Stat};
use slimproto::server::{setd_id, AudioGain, ServerFrame, StrmCommand, StrmHeader};
use slimproto::MacAddress;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub use controller::Controller;
pub use song::Song;

/// How long a disconnected player is remembered before `client forget` (seconds).
pub const FORGET_DISCONNECTED_SECS: u64 = 300;

#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub model: String,
    pub model_name: String,
    pub firmware: String,
    /// Codecs the player can decode, in preference order (`flc`, `pcm`, `mp3`, …).
    pub formats: Vec<String>,
    pub max_sample_rate: u32,
    pub accurate_play_points: bool,
    pub has_digital_out: bool,
    pub has_preamp: bool,
    pub has_disable_dac: bool,
    pub has_polarity_inversion: bool,
    pub balance: bool,
    pub can_https: bool,
    pub has_ir: bool,
    pub has_display: bool,
    pub max_transition_duration: u8,
}

impl Capabilities {
    /// Build capabilities for a device id / HELO, following the per-class defaults of the
    /// reference server and the SqueezePlay capability string.
    pub fn from_helo(helo: &Helo) -> Capabilities {
        let model = helo.model().unwrap_or("unknown");
        let mut c = Capabilities {
            model: model.to_string(),
            model_name: String::new(),
            firmware: helo.revision.to_string(),
            formats: Vec::new(),
            max_sample_rate: 48000,
            accurate_play_points: false,
            has_digital_out: true,
            has_preamp: true,
            has_disable_dac: true,
            has_polarity_inversion: false,
            balance: false,
            can_https: false,
            has_ir: true,
            has_display: true,
            max_transition_duration: 10,
        };
        match model {
            "squeezebox2" => {
                // SB2 vs SB3 by MAC range
                let m = helo.mac.0;
                let is_sb3 = m[0] == 0x00
                    && m[1] == 0x04
                    && m[2] == 0x20
                    && (m[3] > 0x06 || (m[3] == 0x06 && (m[4] > 0 || m[5] > 0)));
                c.model = if is_sb3 {
                    "squeezebox3".into()
                } else {
                    "squeezebox2".into()
                };
                c.model_name = if is_sb3 {
                    "Squeezebox Classic".into()
                } else {
                    "Squeezebox2".into()
                };
                c.formats = ["wma", "ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
            "transporter" => {
                c.model_name = "Transporter".into();
                c.formats = ["wma", "ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                c.max_sample_rate = 96000;
                c.has_polarity_inversion = true;
            }
            "boom" => {
                c.model_name = "Squeezebox Boom".into();
                c.formats = ["wma", "ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                c.has_digital_out = false;
            }
            "receiver" => {
                c.model_name = "Squeezebox Receiver".into();
                c.formats = ["wma", "ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                c.has_display = false;
                c.has_ir = false;
            }
            "squeezeslave" => {
                c.model_name = "Squeezeslave".into();
                c.formats = ["ogg", "flc", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
            "softsqueeze" | "softsqueeze3" | "softboom" => {
                c.model_name = "Softsqueeze".into();
                c.formats = ["ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
            "squeezeplay" | "controller" => {
                c.model_name = "SqueezePlay".into();
                c.formats = ["ogg", "flc", "aif", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                c.has_ir = false;
                c.has_display = false;
                c.has_preamp = false;
                c.has_disable_dac = false;
            }
            _ => {
                c.model_name = model.to_string();
                c.formats = ["flc", "pcm", "mp3"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
        }
        c.apply_capabilities(helo);
        c
    }

    /// Apply a SqueezePlay-style capabilities string (`Model=…,ModelName=…,flc,pcm,…`).
    pub fn apply_capabilities(&mut self, helo: &Helo) {
        let Some(caps) = helo.capabilities.as_deref() else {
            return;
        };
        let declared = helo.declared_formats();
        if !declared.is_empty() {
            self.formats = declared;
        }
        for cap in caps.split(',') {
            let (k, v) = match cap.split_once('=') {
                Some((k, v)) => (k, v),
                None => (cap, "1"),
            };
            let flag = |v: &str| !(v.is_empty() || v == "0");
            match k {
                "Model" | "model" => self.model = v.to_string(),
                "ModelName" | "modelName" => self.model_name = v.to_string(),
                "MaxSampleRate" => self.max_sample_rate = v.parse().unwrap_or(48000),
                "AccuratePlayPoints" => self.accurate_play_points = flag(v),
                "Firmware" => self.firmware = v.to_string(),
                "HasDigitalOut" => self.has_digital_out = flag(v),
                "HasPreAmp" => self.has_preamp = flag(v),
                "HasDisableDac" => self.has_disable_dac = flag(v),
                "HasPolarityInversion" => self.has_polarity_inversion = flag(v),
                "Balance" => self.balance = flag(v),
                "CanHTTPS" => self.can_https = flag(v),
                _ => {}
            }
        }
        // piCorePlayer appends -pCP to its model name
        if self.model == "squeezelite"
            && self.firmware.to_lowercase().contains("-pcp")
            && !self.model_name.to_lowercase().contains("pcp")
        {
            self.model_name.push_str("-pCP");
        }
        if self.model_name.is_empty() {
            self.model_name = self.model.clone();
        }
    }

    /// Volume curve of the player class (`getVolumeParameters`).
    pub fn volume_curve(&self) -> slimproto::VolumeCurve {
        match self.model.as_str() {
            "squeezebox2" | "squeezebox3" | "transporter" | "receiver" | "squeezeslave"
            | "softsqueeze" => slimproto::VolumeCurve::SQUEEZEBOX2,
            _ => slimproto::VolumeCurve::SQUEEZEPLAY,
        }
    }

    /// WiiM firmware identifies itself with this model name.
    pub fn is_wiim(&self) -> bool {
        self.model_name.to_ascii_lowercase().contains("wiim")
    }
}

/// Mutable per-player state.
#[derive(Debug)]
pub struct PlayerState {
    pub name: String,
    pub caps: Capabilities,
    pub device_id: u8,
    pub revision: u8,
    pub uuid: Option<String>,
    pub addr: Option<SocketAddr>,
    pub connected: bool,
    pub disconnected_at: Option<f64>,
    pub connected_at: f64,

    pub power: bool,
    /// 0..=100
    pub volume: i32,
    pub muted: bool,
    pub bass: i32,
    pub treble: i32,
    pub pitch: i32,
    pub balance: i32,
    /// Temporary volume (e.g. alarm) — when set, `volume` is restored afterwards.
    pub temp_volume: Option<i32>,
    /// Absolute server time at which the player should go to sleep (0 = none).
    pub sleep_time: f64,

    // Slimproto status tracking
    pub last_stat: Option<Stat>,
    pub last_stat_at: f64,
    pub last_heard: f64,
    pub jiffies_epoch: Option<f64>,
    jiffies_diffs: Vec<f64>,
    pub stream_start_ts: Option<u32>,
    pub ready_to_stream: bool,
    pub buffer_ready: bool,
    pub connecting: bool,
    pub buffer_size: u32,
    pub buffer_fullness: u32,
    pub output_buffer_fullness: u32,
    pub bytes_received: u64,
    pub signal_strength: u16,
    pub sequence_number: u32,

    /// Name query (`setd 0`) outstanding.
    pub name_query_pending: bool,
    pub is_upgrading: bool,

    /// Remote stream start time (for elapsed estimation on live streams).
    pub remote_stream_start: Option<f64>,
    /// `playerpref`/display state used by `displaystatus`.
    pub show_briefly: Option<(Vec<String>, f64)>,
    /// Timestamp of the last user interaction (IR/button/command) for screensaver logic.
    pub last_activity: f64,
    /// Time of the last IR / front panel button (`Slim::Hardware::IR::lastIRTime`), 0 = none.
    pub last_ir: f64,
    /// The volume bar is shown on the display until this time (`mixerDisplay`).
    pub mixer_display_until: f64,
    /// Last `strm t` sent, so we can detect dead connections.
    pub last_status_request: f64,
    /// A push/bump animation is running on the display (until `ANIC` or this time).
    pub animating_until: f64,
}

/// A connected (or recently disconnected) player.
pub struct Player {
    pub id: MacAddress,
    pub prefs: Arc<PrefStore>,
    pub state: Mutex<PlayerState>,
    frames: RwLock<Option<mpsc::UnboundedSender<ServerFrame>>>,
    pub controller: RwLock<Arc<Mutex<Controller>>>,
    /// Cancelled whenever the player's current HTTP stream must stop.
    stream_cancel: Mutex<CancellationToken>,
    /// Incremented for every `strm s`; the `/stream.mp3` handler refuses stale generations.
    pub stream_generation: AtomicU64,
    /// Stream the player will fetch from `/stream.mp3` for the current generation.
    pending_stream: Mutex<Option<(u64, crate::stream::StreamSpec)>>,
    /// Per-stream password when `authorize` is on.
    stream_password: Mutex<Option<String>>,
    pub server: Weak<Server>,
    /// Screen published by the player UI (`crate::ui`).
    pub ui: Mutex<crate::ui::View>,
    /// Events to the player's UI task.
    pub ui_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<crate::ui::Event>>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Player({})", self.id)
    }
}

impl Player {
    pub fn new(server: &Arc<Server>, helo: &Helo, addr: SocketAddr) -> Arc<Player> {
        let id = helo.mac;
        let prefs = server.prefs.player(&id.to_string());
        let caps = Capabilities::from_helo(helo);
        let mut name = prefs.get_str("playername");
        if name.is_empty() {
            name = server.default_player_name(&caps.model_name);
            prefs.set("playername", json!(name));
        }
        let now = crate::util::now();
        let state = PlayerState {
            name,
            caps,
            device_id: helo.device_id,
            revision: helo.revision,
            uuid: helo.uuid_hex().filter(|u| u.chars().any(|c| c != '0')),
            addr: Some(addr),
            connected: false,
            disconnected_at: None,
            connected_at: now,
            power: prefs.get_i64("power") != 0,
            volume: prefs.get_i64("volume").clamp(0, 100) as i32,
            muted: prefs.get_i64("mute") != 0,
            bass: prefs.get_i64("bass") as i32,
            treble: prefs.get_i64("treble") as i32,
            pitch: 100,
            balance: prefs.get_i64("balance") as i32,
            temp_volume: None,
            sleep_time: 0.0,
            last_stat: None,
            last_stat_at: 0.0,
            last_heard: now,
            jiffies_epoch: None,
            jiffies_diffs: Vec::new(),
            stream_start_ts: None,
            ready_to_stream: true,
            buffer_ready: false,
            connecting: false,
            buffer_size: 0,
            buffer_fullness: 0,
            output_buffer_fullness: 0,
            bytes_received: 0,
            signal_strength: 0,
            sequence_number: 0,
            name_query_pending: false,
            is_upgrading: false,
            remote_stream_start: None,
            show_briefly: None,
            last_activity: now,
            last_ir: 0.0,
            mixer_display_until: 0.0,
            last_status_request: 0.0,
            animating_until: 0.0,
        };
        let player = Arc::new(Player {
            id,
            prefs,
            state: Mutex::new(state),
            frames: RwLock::new(None),
            controller: RwLock::new(Arc::new(Mutex::new(Controller::placeholder(id)))),
            stream_cancel: Mutex::new(CancellationToken::new()),
            stream_generation: AtomicU64::new(0),
            pending_stream: Mutex::new(None),
            stream_password: Mutex::new(None),
            server: Arc::downgrade(server),
            ui: Mutex::new(crate::ui::View::default()),
            ui_tx: Mutex::new(None),
        });
        // a controller of its own, until synced
        let ctrl = Controller::new_shared(server, &player);
        ctrl.lock().restore_playlist(&player, server);
        *player.controller.write() = ctrl;
        player
    }

    pub fn server(&self) -> Option<Arc<Server>> {
        self.server.upgrade()
    }

    pub fn name(&self) -> String {
        self.state.lock().name.clone()
    }

    pub fn is_connected(&self) -> bool {
        self.state.lock().connected
    }

    pub fn power(&self) -> bool {
        self.state.lock().power
    }

    pub fn set_frame_sender(&self, tx: Option<mpsc::UnboundedSender<ServerFrame>>) {
        *self.frames.write() = tx;
    }

    /// Queue a frame for the player.  Silently dropped when disconnected.
    pub fn send(&self, frame: ServerFrame) {
        if let Some(tx) = self.frames.read().as_ref() {
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!("{} <- {:?}", self.id, frame);
            } else {
                debug!("{} <- {}", self.id, String::from_utf8_lossy(&frame.op()));
            }
            let _ = tx.send(frame);
        }
    }

    pub fn controller(&self) -> Arc<Mutex<Controller>> {
        self.controller.read().clone()
    }

    pub fn set_controller(&self, c: Arc<Mutex<Controller>>) {
        *self.controller.write() = c;
    }

    // ---- streaming socket management -------------------------------------------------

    /// Start a new stream generation: cancels any HTTP stream still being served.
    pub fn new_stream_generation(&self) -> u64 {
        let old = std::mem::replace(&mut *self.stream_cancel.lock(), CancellationToken::new());
        old.cancel();
        self.stream_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn stream_cancel_token(&self) -> CancellationToken {
        self.stream_cancel.lock().clone()
    }

    /// Close the player's HTTP stream (server side) without starting a new one.
    pub fn close_stream(&self) {
        self.new_stream_generation();
    }

    // ---- audio settings ------------------------------------------------------------

    /// Send the `audg` frame for the current volume/mute/balance.
    pub fn send_volume(&self) {
        let (vol, dvc, preamp, balance, seq, curve) = {
            let s = self.state.lock();
            let effective = if s.muted {
                0
            } else {
                s.temp_volume.unwrap_or(s.volume)
            };
            // players without a digital output always use digital volume control (Player::init)
            let dvc = self.prefs.get_i64("digitalVolumeControl") != 0 || !s.caps.has_digital_out;
            let preamp = self.prefs.get_f64("preampVolumeControl");
            (
                effective,
                dvc,
                preamp,
                s.balance,
                Some(s.sequence_number),
                s.caps.volume_curve(),
            )
        };
        self.send(ServerFrame::Audg(AudioGain::for_volume_curve(
            vol, curve, dvc, preamp, balance, seq,
        )));
    }

    /// Set the volume (0..=100), persist it and send it to the player.  Returns the new volume.
    pub fn set_volume(&self, volume: i32, temporary: bool) -> i32 {
        let v = volume.clamp(0, 100);
        {
            let mut s = self.state.lock();
            if temporary {
                s.temp_volume = Some(v);
            } else {
                s.volume = v;
                s.temp_volume = None;
                if v > 0 {
                    s.muted = false;
                }
            }
        }
        if !temporary {
            self.prefs.set("volume", json!(v));
            self.prefs.set("mute", json!(0));
        }
        self.send_volume();
        v
    }

    pub fn volume(&self) -> i32 {
        self.state.lock().volume
    }

    pub fn set_muted(&self, muted: bool) {
        self.state.lock().muted = muted;
        self.prefs.set("mute", json!(muted as i32));
        self.send_volume();
    }

    pub fn is_muted(&self) -> bool {
        self.state.lock().muted
    }

    /// Enable/disable audio outputs (`aude`).
    pub fn audio_outputs_enable(&self, enabled: bool) {
        self.send(ServerFrame::Aude {
            spdif: enabled,
            dac: enabled,
        });
    }

    /// Ask the firmware for its stored player name (`setd 0`).
    pub fn query_player_name(&self) {
        self.state.lock().name_query_pending = true;
        self.send(ServerFrame::Setd {
            id: setd_id::PLAYER_NAME,
            value: None,
        });
    }

    /// Push the player name to the firmware (`setd 0 <name>\0`).
    pub fn push_player_name(&self, name: &str) {
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        self.send(ServerFrame::Setd {
            id: setd_id::PLAYER_NAME,
            value: Some(v.into()),
        });
    }

    pub fn set_name(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.state.lock().name = name.to_string();
        self.prefs.set("playername", json!(name));
        self.push_player_name(name);
        if let Some(server) = self.server() {
            server.notifier.notify(Some(self.id), &["name", name]);
        }
    }

    /// Request a status report (`strm t`).
    pub fn request_status(&self) {
        self.state.lock().last_status_request = crate::util::now();
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::Status,
            0,
            self.polarity_flags(),
        )));
    }

    fn polarity_flags(&self) -> u8 {
        (self.prefs.get_i64("polarityInversion") & 0x03) as u8
    }

    // ---- low-level transport commands used by the controller -------------------------

    pub fn strm_stop(&self) {
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::Stop,
            0,
            self.polarity_flags(),
        )));
        self.close_stream();
        let mut s = self.state.lock();
        s.ready_to_stream = true;
        s.stream_start_ts = None;
    }

    pub fn strm_flush(&self) {
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::Flush,
            0,
            self.polarity_flags(),
        )));
        self.close_stream();
        self.state.lock().ready_to_stream = true;
    }

    /// Pause; `interval_ms` > 0 pauses for that long (sync adjustment).
    pub fn strm_pause(&self, interval_ms: u32) {
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::Pause,
            interval_ms,
            self.polarity_flags(),
        )));
    }

    /// Unpause now (`at_jiffies == 0`) or at the given player clock value.
    pub fn strm_unpause(&self, at_jiffies: u32) {
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::Unpause,
            at_jiffies,
            self.polarity_flags(),
        )));
    }

    pub fn strm_skip_ahead(&self, ms: u32) {
        self.send(ServerFrame::strm(StrmHeader::simple(
            StrmCommand::SkipAhead,
            ms,
            self.polarity_flags(),
        )));
    }

    /// Convert a server wall-clock time into the player's jiffies clock (`startAt`).
    pub fn jiffies_for_time(&self, at: f64) -> u32 {
        let s = self.state.lock();
        let epoch = s.jiffies_epoch.unwrap_or(at);
        ((at - epoch) * 1000.0).max(0.0) as u32
    }

    // ---- STAT bookkeeping -----------------------------------------------------------

    /// Record a STAT; returns the previous `connecting` flag.
    pub fn record_stat(&self, stat: &Stat) {
        let now = crate::util::now();
        let mut s = self.state.lock();
        s.last_heard = now;
        s.last_stat_at = now;
        s.buffer_size = stat.buffer_size;
        s.buffer_fullness = stat.fullness;
        s.output_buffer_fullness = stat.output_buffer_fullness;
        s.bytes_received = stat.bytes_received;
        s.signal_strength = stat.signal_strength;
        s.track_jiffies_epoch(stat.jiffies, now);
        s.last_stat = Some(stat.clone());
    }

    /// Elapsed seconds reported by the player for the current stream, extrapolated to now
    /// (`Squeezebox2::songElapsedSeconds`).  `None` when no STAT has been seen.
    pub fn song_elapsed_seconds(&self, playing: bool) -> Option<f64> {
        let s = self.state.lock();
        let stat = s.last_stat.as_ref()?;
        let mut elapsed = stat.elapsed_milliseconds as f64 / 1000.0;
        if elapsed < stat.elapsed_seconds as f64 {
            elapsed =
                stat.elapsed_seconds as f64 + (stat.elapsed_milliseconds % 1000) as f64 / 1000.0;
        }
        if playing && s.connected {
            if let Some(ts) = s.jiffies_to_timestamp(stat.jiffies) {
                let delta = crate::util::now() - ts;
                if delta > 0.0 && delta < 10.0 {
                    elapsed += delta;
                }
            }
        }
        Some(elapsed)
    }

    /// `[status_time, apparent_stream_start_time, song_elapsed]` for sync (`playPoint`).
    pub fn play_point(&self) -> Option<(f64, f64, f64)> {
        let s = self.state.lock();
        let stat = s.last_stat.as_ref()?;
        if stat.elapsed_milliseconds == 0 {
            return None;
        }
        let status_time = s.jiffies_to_timestamp(stat.jiffies)?;
        let elapsed = stat.elapsed_milliseconds as f64 / 1000.0;
        let mut song_elapsed = elapsed;
        if song_elapsed < stat.elapsed_seconds as f64 {
            song_elapsed =
                stat.elapsed_seconds as f64 + (stat.elapsed_milliseconds % 1000) as f64 / 1000.0;
        }
        Some((status_time, status_time - elapsed, song_elapsed))
    }

    pub fn buffer_fullness(&self) -> u32 {
        self.state.lock().buffer_fullness
    }

    pub fn is_ready_to_stream(&self) -> bool {
        self.state.lock().ready_to_stream
    }

    pub fn is_buffer_ready(&self) -> bool {
        self.state.lock().buffer_ready
    }

    pub fn set_pending_stream(&self, generation: u64, spec: crate::stream::StreamSpec) {
        *self.pending_stream.lock() = Some((generation, spec));
    }

    pub fn clear_pending_stream(&self) {
        *self.pending_stream.lock() = None;
    }

    /// The stream spec for the current generation, if the player has not fetched it yet.
    pub fn pending_stream(&self) -> Option<(u64, crate::stream::StreamSpec)> {
        let g = self.stream_generation.load(Ordering::SeqCst);
        self.pending_stream
            .lock()
            .clone()
            .filter(|(gen, _)| *gen == g)
    }

    pub fn set_stream_password(&self, pw: Option<String>) {
        *self.stream_password.lock() = pw;
    }

    pub fn stream_password(&self) -> Option<String> {
        self.stream_password.lock().clone()
    }

    /// Set a temporary volume without touching prefs (used for fades).
    pub fn set_volume_temp_silent(&self, v: i32) {
        self.state.lock().temp_volume = Some(v);
        self.send_volume();
    }

    /// Clear the temporary volume and resend the real one.
    pub fn clear_temp_volume(&self) {
        self.state.lock().temp_volume = None;
        self.send_volume();
    }

    pub fn save_current_song(&self, index: usize) {
        self.prefs.set("currentSong", json!(index));
    }
}

impl PlayerState {
    /// `Slim::Player::Player::trackJiffiesEpoch`.
    fn track_jiffies_epoch(&mut self, jiffies: u32, now: f64) {
        let offset = now - jiffies as f64 / 1000.0;
        match self.jiffies_epoch {
            Some(epoch) if offset >= epoch && offset - epoch <= 50.0 => {
                let diff = offset - epoch;
                self.jiffies_diffs.push(diff);
                if self.jiffies_diffs.len() > 50 {
                    self.jiffies_diffs.remove(0);
                }
                if self.jiffies_diffs.len() >= 10 {
                    let min = self
                        .jiffies_diffs
                        .iter()
                        .cloned()
                        .fold(f64::INFINITY, f64::min);
                    if min > 0.001 && (min < 0.005 || self.jiffies_diffs.len() >= 50) {
                        self.jiffies_epoch = Some(epoch + min);
                        self.jiffies_diffs.clear();
                    }
                }
            }
            _ => {
                self.jiffies_epoch = Some(offset);
                self.jiffies_diffs.clear();
            }
        }
    }

    pub fn jiffies_to_timestamp(&self, jiffies: u32) -> Option<f64> {
        self.jiffies_epoch.map(|e| e + jiffies as f64 / 1000.0)
    }
}
