//! Spotify Connect (what Spotty's per-player helper daemon does for Lyrion): every connected
//! player is a Connect device of the signed-in account, named after the player.  Picking it
//! in a Spotify app makes librespot (`Spirc`) play the app's queue; its audio goes through
//! the same encoder as other Spotify audio into one endless `spotify://connect:<player>`
//! stream that the player plays like any other.
//!
//! * App → player: play/pause drive the player; a seek or a track picked in the app restarts
//!   the stream so the player's buffer (many seconds of audio) does not play the old
//!   position first, while the next track of the queue plays on gaplessly.  The player's
//!   volume follows the app's volume while Connect drives the player.
//! * Player → app: stopping, pausing or playing something else on the player (remote, web
//!   UI, other controllers) pauses the Connect device, and resuming resumes it.
//! * Next and previous on the player (remote, web UI, other controllers) move through the
//!   app's queue: the player's own queue holds only the stream.  What the app's queue plays
//!   next is what Spotify last told the device ([`up_next`]).
//! * Cast and UPnP players show a track's title and artwork from when its stream loads, so
//!   for them the stream starts afresh for each track (after the last one's buffered audio
//!   has played out), not gaplessly, and the bridges take what plays from the device
//!   ([`audible`]).
//! * Devices are registered with the account (no zeroconf), so they are only visible to the
//!   account RMS is signed in with.  Credentials Spotify hands a session only work for its
//!   device id, and only with the client id they were issued for: each device keeps its own
//!   (`spotify/connect/<player>/`), got the first time with the sign-in's token.  A device
//!   that can't be set up is tried again later and later, and the Spotify settings say why.

use super::Spotify;
use crate::control::request::Source;
use crate::server::Server;
use dashmap::DashMap;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::dealer::protocol::Message;
use librespot_core::{Session, SessionConfig};
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::config::PlayerConfig;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot_playback::player::{Player, PlayerEvent};
use librespot_protocol::connect::ClusterUpdate;
use parking_lot::Mutex;
use slimproto::MacAddress;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// How often players are matched with Connect devices and their states compared.
const TICK: Duration = Duration::from_secs(1);
/// How long the decoder waits for the player to open the stream before dropping audio.
const WAIT_FOR_STREAM: Duration = Duration::from_secs(10);
/// After (re)starting the player's stream, its state is in flux: don't read it as the
/// user stopping or pausing.
const RESTART_GRACE: Duration = Duration::from_secs(4);
/// How far decoding may run ahead of what the player has actually played.  Without a
/// limit it fills the player's buffers (a Squeezebox holds about 40s of FLAC and PCM), so
/// the app's position, its "now playing" and the player's display would run that far ahead
/// of what is heard.  Must exceed the stream's start threshold (255 KB, about 2.5s of FLAC).
const LEAD: Duration = Duration::from_secs(5);
/// A track that loads this soon after the previous one ended is the next track playing on
/// (gapless), not one picked in the app.
const NATURAL_NEXT: Duration = Duration::from_secs(3);
/// How long a new track waits for its own stream on a player that reloads the stream per
/// track: the last track's buffered audio (up to the lead) plus the device opening it.
const RELOAD_WAIT: Duration = Duration::from_secs(20);

/// First wait before setting up a device that failed again; doubles up to `MAX_RETRY`.
/// A device that lost its session counts as failed, so one that keeps losing it isn't
/// set up again every second.
const FIRST_RETRY: Duration = Duration::from_secs(10);
/// A device that stays up this long is fine again: its next failure waits `FIRST_RETRY`.
const STABLE: Duration = Duration::from_secs(120);
const MAX_RETRY: Duration = Duration::from_secs(30 * 60);

static DEVICES: OnceLock<DashMap<String, Arc<Device>>> = OnceLock::new();

fn devices() -> &'static DashMap<String, Arc<Device>> {
    DEVICES.get_or_init(DashMap::new)
}

/// A player whose device could not be set up: why, and when to try again.
struct Failure {
    player: String,
    error: String,
    count: u32,
    retry_at: Instant,
}

static FAILURES: OnceLock<DashMap<String, Failure>> = OnceLock::new();

fn failures() -> &'static DashMap<String, Failure> {
    FAILURES.get_or_init(DashMap::new)
}

/// Why a player has no Connect device right now, if one hasn't: for the Spotify settings.
pub fn error() -> Option<String> {
    failures()
        .iter()
        .filter(|f| !devices().contains_key(f.key()))
        .map(|f| format!("Spotify Connect for {}: {}", f.player, f.error))
        .min()
}

/// Remember a failure and when to try again: logged when it first happens or changes.
fn failed(k: &str, player: &str, error: String) {
    let mut f = failures().entry(k.to_string()).or_insert(Failure {
        player: player.to_string(),
        error: String::new(),
        count: 0,
        retry_at: Instant::now(),
    });
    if f.error != error {
        warn!("Spotify Connect for {player}: {error}");
    } else {
        debug!("Spotify Connect for {player}: {error} (again)");
    }
    let wait = FIRST_RETRY
        .saturating_mul(1 << f.count.min(10))
        .min(MAX_RETRY);
    f.player = player.to_string();
    f.error = error;
    f.count += 1;
    f.retry_at = Instant::now() + wait;
}

/// A new sign-in: set up the devices that failed now, not after their wait.
pub fn signed_in_again() {
    failures().clear();
}

/// Key of a player in `spotify://connect:<key>` URLs (its MAC without colons).
pub fn key(mac: &MacAddress) -> String {
    mac.to_string().replace(':', "")
}

pub fn url(mac: &MacAddress) -> String {
    format!("spotify://connect:{}", key(mac))
}

/// The PCM handed from the device's player thread to the stream the player reads.
#[derive(Default)]
struct Pipe {
    /// The current stream's sender, with a generation number to tell streams apart.
    tx: Mutex<Option<(u64, SyncSender<Vec<i16>>)>>,
    /// How far (ms) decoding is ahead of what the player has played, as last measured.
    lead_ms: std::sync::atomic::AtomicU64,
}

impl Pipe {
    fn lead(&self) -> Duration {
        Duration::from_millis(self.lead_ms.load(Ordering::Relaxed))
    }
}

struct PipeSink {
    pipe: Arc<Pipe>,
    player: Weak<crate::player::Player>,
    /// The stream being written: its generation, when it started, frames written to it.
    stream: Option<(u64, Instant, u64)>,
    learner: super::learn::Learner,
    /// The player reloads the stream for each track (see [`Device::reload`]).
    reload: bool,
    /// The device's player events, to tell a new track from a resume when the sink starts.
    events: super::learn::EventSlot,
}

impl PipeSink {
    fn generation(&self) -> Option<u64> {
        self.pipe.tx.lock().as_ref().map(|(g, _)| *g)
    }

    /// Whether a track loaded since this was last asked.
    fn new_track(&mut self) -> bool {
        let mut slot = self.events.lock();
        let Some(ch) = slot.as_mut() else {
            return false;
        };
        let mut changed = false;
        while let Ok(ev) = ch.try_recv() {
            changed |= matches!(
                ev,
                PlayerEvent::Loading { .. } | PlayerEvent::TrackChanged { .. }
            );
        }
        changed
    }

    /// Seconds of the current stream the player has played.  Never more than the time since
    /// the stream started: right after a restart the player still reports the old stream.
    fn played(&self, started: Instant) -> f64 {
        let reported = self
            .player
            .upgrade()
            .and_then(|p| p.song_elapsed_seconds(true))
            .unwrap_or(0.0);
        reported.min(started.elapsed().as_secs_f64())
    }
}

impl Sink for PipeSink {
    fn start(&mut self) -> SinkResult<()> {
        // A new track on a player that reloads the stream per track (librespot isn't gapless
        // for it, so it stops and starts the sink between tracks): hold it until the stream
        // restarts, so the track begins at the beginning of its own stream.  A resume after
        // a pause goes on in the same stream.
        if self.reload && self.new_track() {
            let before = self.generation();
            let deadline = Instant::now() + RELOAD_WAIT;
            while Instant::now() < deadline {
                let now = self.generation();
                if now.is_some() && now != before {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.learner.stopped();
        if self.reload {
            // (the events up to here belong to the track that ended)
            self.new_track();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        let mut samples = converter.f64_to_s16(&samples);
        self.learner.samples(&samples);
        let frames = (samples.len() / super::stream::CHANNELS as usize) as u64;
        let deadline = std::time::Instant::now() + WAIT_FOR_STREAM;
        // the player's position while we wait for it, and since when it has not moved
        let mut stalled: Option<(f64, Instant)> = None;
        loop {
            let tx = self.pipe.tx.lock().clone();
            match tx {
                Some((generation, tx)) => {
                    if self.stream.is_none_or(|(g, _, _)| g != generation) {
                        self.stream = Some((generation, Instant::now(), 0));
                    }
                    let (_, started, written) = self.stream.expect("set above");
                    let ahead =
                        written as f64 / super::stream::SAMPLE_RATE as f64 - self.played(started);
                    self.pipe
                        .lead_ms
                        .store((ahead.max(0.0) * 1000.0) as u64, Ordering::Relaxed);
                    if ahead > LEAD.as_secs_f64() {
                        let played = self.played(started);
                        match stalled {
                            Some((at, since)) if (played - at).abs() < 0.001 => {
                                if since.elapsed() > Duration::from_secs(1) {
                                    // the player is paused or stuck: drop this packet so the
                                    // decoder thread can take commands (a pause from the app)
                                    return Ok(());
                                }
                            }
                            _ => stalled = Some((played, Instant::now())),
                        }
                        // gone? (nothing reads the stream any more)
                        if let Err(TrySendError::Disconnected(_)) = tx.try_send(Vec::new()) {
                            return Ok(());
                        }
                        // like a sound card: wait for the player to catch up
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    match tx.try_send(samples) {
                        Ok(()) => {
                            if let Some(st) = &mut self.stream {
                                st.2 += frames;
                            }
                            return Ok(());
                        }
                        Err(TrySendError::Full(s)) => samples = s,
                        Err(TrySendError::Disconnected(_)) => {
                            // the player stopped reading: forget that stream
                            let mut g = self.pipe.tx.lock();
                            if g.as_ref().is_some_and(|(cur, _)| *cur == generation) {
                                *g = None;
                            }
                            return Ok(());
                        }
                    }
                }
                None if std::time::Instant::now() > deadline => return Ok(()),
                None => {}
            }
            // blocks the decoder like a sound card: the player sets the pace
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The Connect volume follows the player's volume (0..65535 ↔ 0..100).
struct PlayerMixer {
    server: Weak<Server>,
    mac: MacAddress,
    volume: AtomicU16,
    /// Forward volume changes to the player only while Connect drives it (librespot sets
    /// its initial volume when the device starts, which must not change the player's).
    forward: AtomicBool,
}

impl Mixer for PlayerMixer {
    fn open(_: MixerConfig) -> Result<Self, librespot_core::Error> {
        Err(librespot_core::Error::unimplemented("opened by RMS"))
    }

    fn volume(&self) -> u16 {
        self.volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u16) {
        self.volume.store(volume, Ordering::Relaxed);
        if !self.forward.load(Ordering::Relaxed) {
            return;
        }
        let Some(server) = self.server.upgrade() else {
            return;
        };
        let percent = (volume as u32 * 100 + 32767) / 65535;
        let mac = self.mac;
        crate::util::spawn(async move {
            run(&server, mac, &["mixer", "volume", &percent.to_string()]).await;
        });
    }
}

struct Device {
    mac: MacAddress,
    name: String,
    spirc: Spirc,
    pipe: Arc<Pipe>,
    mixer: Arc<PlayerMixer>,
    /// The Spotify side is playing (from the player events).
    playing: AtomicBool,
    /// We started the Connect stream on the player and still drive it.
    driving: AtomicBool,
    /// The Spirc task is running (it ends when the session is lost).
    alive: Arc<AtomicBool>,
    /// Until when the player's state is ignored after restarting its stream.
    grace_until: Mutex<Option<Instant>>,
    /// When the last track ended (to tell gapless track changes from picked ones).
    last_end: Mutex<Option<Instant>>,
    /// The position changed while paused: flush the player's buffer on resume.
    flush_on_resume: AtomicBool,
    /// When it was set up.
    started: Instant,
    /// The player shows a track's title and artwork from when its stream loads (Cast,
    /// UPnP): the stream restarts for each track, with what plays then.
    reload: bool,
    /// (reload) The track loading follows the last one by itself.
    natural: AtomicBool,
    /// (reload) Connect started driving the player with the track loading.
    starting: AtomicBool,
    /// What the player plays now.
    audible: Mutex<Option<Audible>>,
    /// The tracks Spotify plays next on the device (URIs), while it is the account's active
    /// device.
    next: Arc<Mutex<Vec<String>>>,
}

/// A track of the app's queue, as the player shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audible {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Image URL.
    pub cover: Option<String>,
    /// The track's `spotify://track:<id>` URL (not for an episode).
    pub url: Option<String>,
}

/// What the Connect stream `url` (`spotify://connect:<key>`) plays now, when known.
pub fn audible(url: &str) -> Option<Audible> {
    let k = url.strip_prefix("spotify://connect:")?;
    devices().get(k)?.audible.lock().clone()
}

/// What plays next on the Connect device of a player (`key`) while it plays there: the
/// track URIs of the app's queue, then of what it plays from.  Empty when it plays elsewhere.
pub fn up_next(key: &str) -> Vec<String> {
    devices()
        .get(key)
        .map(|d| d.next.lock().clone())
        .unwrap_or_default()
}

/// The tracks a cluster update says play next on device `id`: none when another device
/// plays.
fn next_tracks(update: &ClusterUpdate, id: &str) -> Vec<String> {
    let Some(cluster) = update.cluster.as_ref() else {
        return Vec::new();
    };
    if cluster.active_device_id != id {
        return Vec::new();
    }
    cluster
        .player_state
        .next_tracks
        .iter()
        .map(|t| t.uri.clone())
        .filter(|u| u.starts_with("spotify:track:"))
        .collect()
}

/// Next (`by` > 0) or previous (the start of the track, or the one before) on a player
/// that plays its Connect stream: the app's queue moves.  False when it doesn't play one.
pub fn skip(p: &crate::player::Player, by: i64) -> bool {
    let playing = {
        let c = p.controller();
        let c = c.lock();
        c.playing_song().map(|s| s.url().to_string())
    };
    let Some(k) = playing
        .as_deref()
        .and_then(|u| u.strip_prefix("spotify://connect:"))
    else {
        return false;
    };
    let Some(dev) = devices().get(k).map(|d| d.clone()) else {
        return false;
    };
    if !dev.driving.load(Ordering::Relaxed) {
        return false;
    }
    let r = if by > 0 {
        dev.spirc.next()
    } else {
        dev.spirc.prev()
    };
    if let Err(e) = &r {
        debug!("Spotify Connect: {}: skip: {e}", dev.name);
    }
    r.is_ok()
}

impl Device {
    fn in_grace(&self) -> bool {
        self.grace_until.lock().is_some_and(|t| t > Instant::now())
    }

    fn set_driving(&self, on: bool) -> bool {
        self.mixer.forward.store(on, Ordering::Relaxed);
        self.driving.swap(on, Ordering::Relaxed)
    }
}

async fn run(server: &Arc<Server>, mac: MacAddress, words: &[&str]) {
    let tokens = words.iter().map(|w| w.to_string()).collect();
    crate::control::dispatch::execute_tokens(server, Some(mac), tokens, Source::Internal).await;
}

/// The stream of a player's Connect device (`spotify://connect:<key>`), FLAC or PCM.
pub fn open_stream(key: &str, flac: bool) -> std::io::Result<crate::stream::source::ByteStream> {
    use futures::StreamExt;
    let dev = devices()
        .get(key)
        .map(|d| d.clone())
        .ok_or_else(|| std::io::Error::other("no Spotify Connect device for this player"))?;
    let (tx, rx) = sync_channel::<Vec<i16>>(super::stream::QUEUE);
    // a new stream replaces the previous one (its sender is dropped, so it ends)
    {
        let mut g = dev.pipe.tx.lock();
        let generation = g.as_ref().map(|(n, _)| n + 1).unwrap_or(0);
        *g = Some((generation, tx));
    }
    let rx = super::stream::encode(rx, flac)?;
    Ok(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|c| (Ok(bytes::Bytes::from(c)), rx))
    })
    .boxed())
}

/// Keep a Connect device per connected player while Spotify is signed in.
pub fn start(server: &Arc<Server>) {
    let server = Arc::downgrade(server);
    crate::util::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            let Some(server) = server.upgrade() else {
                return;
            };
            let Some(sp) = super::get() else { continue };
            supervise(&server, sp).await;
        }
    });
}

async fn supervise(server: &Arc<Server>, sp: &Arc<Spotify>) {
    let signed_in = sp.signed_in();
    let players: Vec<_> = server
        .players()
        .into_iter()
        .filter(|p| signed_in && p.is_connected())
        .collect();
    // remove devices of players that left (or of everyone, when signed out)
    let keep: Vec<String> = players.iter().map(|p| key(&p.id)).collect();
    failures().retain(|k, _| keep.contains(k));
    devices().retain(|k, d| {
        let ok = keep.contains(k);
        if !ok {
            info!("Spotify Connect: removing device \"{}\"", d.name);
            let _ = d.spirc.shutdown();
        }
        ok
    });
    for p in players {
        let k = key(&p.id);
        let dev = devices().get(&k).map(|d| d.clone());
        match dev {
            None => {
                if failures()
                    .get(&k)
                    .is_some_and(|f| f.retry_at > Instant::now())
                {
                    continue;
                }
                // (a failure before stays until the device has been up a while)
                if let Err(e) = add_device(server, sp, &p).await {
                    failed(&k, &p.name(), e);
                }
            }
            Some(dev) if !dev.alive.load(Ordering::Relaxed) => {
                if dev.set_driving(false) {
                    run(server, dev.mac, &["stop"]).await;
                }
                devices().remove(&k);
                failed(&k, &p.name(), "lost its Spotify session".into());
            }
            Some(dev) if dev.name != p.name() => {
                // renamed: re-register under the new name
                let _ = dev.spirc.shutdown();
                devices().remove(&k);
            }
            Some(dev) => {
                if dev.started.elapsed() > STABLE {
                    failures().remove(&k);
                }
                follow_player(&dev, &p).await
            }
        }
    }
}

/// Player → app: pause the Connect device when the player stops playing its stream.
async fn follow_player(dev: &Arc<Device>, p: &Arc<crate::player::Player>) {
    let (mode, current) = {
        let c = p.controller();
        let c = c.lock();
        (c.mode(), c.playing_song().map(|s| s.url().to_string()))
    };
    // keep the app's volume in step with the player
    let vol = ((p.volume().clamp(0, 100) as u32 * 65535) / 100) as u16;
    if vol.abs_diff(dev.mixer.volume()) > 700 {
        dev.mixer.volume.store(vol, Ordering::Relaxed);
        let _ = dev.spirc.set_volume(vol);
    }
    if !dev.driving.load(Ordering::Relaxed) || dev.in_grace() {
        return;
    }
    let ours = current.as_deref() == Some(url(&dev.mac).as_str());
    let playing = dev.playing.load(Ordering::Relaxed);
    if !ours || mode == "stop" {
        debug!("Spotify Connect: {} left the Connect stream", dev.name);
        dev.set_driving(false);
        if playing {
            let _ = dev.spirc.pause();
        }
    } else if mode == "pause" && playing {
        let _ = dev.spirc.pause();
    } else if mode == "play" && !playing {
        let _ = dev.spirc.play();
    }
}

async fn add_device(
    server: &Arc<Server>,
    sp: &Arc<Spotify>,
    p: &Arc<crate::player::Player>,
) -> Result<(), String> {
    let name = p.name();
    let k = key(&p.id);
    let dir = sp.dir.join("connect").join(&k);
    let cache = Cache::new(Some(dir.clone()), None, None, None).ok();
    // A device logs in with the sign-in's token the first time and keeps the
    // credentials Spotify gives it (they only work for its device id), of Spotify's own
    // client id, kept with them (`client_id`): ones of another id (from before the two
    // sign-ins) are dropped, as are ones Spotify refuses.
    let client_id = super::CLIENT_ID.to_string();
    let id_file = dir.join("client_id");
    let stored = cache
        .as_ref()
        .and_then(|c| c.credentials())
        .filter(|_| std::fs::read_to_string(&id_file).is_ok_and(|id| id.trim() == client_id));
    let from_stored = stored.is_some();
    let credentials = match stored {
        Some(c) => c,
        None => {
            let _ = std::fs::remove_file(dir.join("credentials.json"));
            let token = sp.token().await?;
            Credentials::with_access_token(token)
        }
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&id_file, &client_id);
    let started = start_device(server, p, &name, &k, &client_id, credentials, cache).await;
    if started.is_err() && from_stored {
        // refused: log in afresh next time
        let _ = std::fs::remove_file(dir.join("credentials.json"));
    }
    started
}

async fn start_device(
    server: &Arc<Server>,
    p: &Arc<crate::player::Player>,
    name: &str,
    k: &str,
    client_id: &str,
    credentials: Credentials,
    cache: Option<Cache>,
) -> Result<(), String> {
    let name = name.to_string();
    let k = k.to_string();
    // one session per device: Connect state is per session.  Its own cache: the credentials
    // Spotify returns are bound to this device id.
    let session = Session::new(
        SessionConfig {
            device_id: format!("{:0>40}", k),
            client_id: client_id.to_string(),
            ..Default::default()
        },
        cache,
    );
    let bitrate = server.prefs.server.get_i64("spotifyBitrate").max(96) as u32;
    let normalise = server.prefs.server.get_bool("spotifyNormalisation");
    let reload = matches!(p.state.lock().caps.model.as_str(), "chromecast" | "upnp");
    let config = PlayerConfig {
        // one endless stream: librespot does the gapless part (except where the stream
        // restarts for each track: the sink stops between tracks then)
        gapless: !reload,
        ..super::stream::player_config(bitrate, normalise)
    };
    let pipe = Arc::new(Pipe::default());
    let sink_pipe = pipe.clone();
    let sink_player = Arc::downgrade(p);
    let learn_events = super::learn::EventSlot::default();
    let learner = super::learn::Learner::new(Arc::downgrade(server), learn_events.clone());
    let sink_events = super::learn::EventSlot::default();
    let sink_events_slot = sink_events.clone();
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || {
        Box::new(PipeSink {
            pipe: sink_pipe,
            player: sink_player,
            stream: None,
            learner,
            reload,
            events: sink_events_slot,
        })
    });
    *learn_events.lock() = Some(player.get_player_event_channel());
    *sink_events.lock() = Some(player.get_player_event_channel());
    let initial = ((p.volume().clamp(0, 100) as u32 * 65535) / 100) as u16;
    let mixer = Arc::new(PlayerMixer {
        server: Arc::downgrade(server),
        mac: p.id,
        volume: AtomicU16::new(initial),
        forward: AtomicBool::new(false),
    });
    let mut events = player.get_player_event_channel();
    let state_session = session.clone();
    let (spirc, task) = Spirc::new(
        ConnectConfig {
            name: name.clone(),
            initial_volume: initial,
            ..Default::default()
        },
        session,
        credentials,
        player,
        mixer.clone(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let alive = Arc::new(AtomicBool::new(true));
    let alive_task = alive.clone();
    let task_name = name.clone();
    crate::util::spawn(async move {
        task.await;
        alive_task.store(false, Ordering::Relaxed);
        info!("Spotify Connect: device \"{task_name}\" stopped");
    });
    let dev = Arc::new(Device {
        mac: p.id,
        name: name.clone(),
        spirc,
        pipe,
        mixer,
        playing: AtomicBool::new(false),
        driving: AtomicBool::new(false),
        alive,
        grace_until: Mutex::new(None),
        last_end: Mutex::new(None),
        flush_on_resume: AtomicBool::new(false),
        started: Instant::now(),
        reload,
        natural: AtomicBool::new(false),
        starting: AtomicBool::new(false),
        audible: Mutex::new(None),
        next: Arc::new(Mutex::new(Vec::new())),
    });
    devices().insert(k, dev.clone());
    // What Spotify plays next, from the cluster updates the device gets (as Spirc does).
    // They end with the device: Spirc closes its session's dealer.
    let updates = state_session.dealer().listen_for(
        "hm://connect-state/v1/cluster",
        Message::from_raw::<ClusterUpdate>,
    );
    let id = state_session.device_id().to_string();
    drop(state_session);
    match updates {
        Ok(mut updates) => {
            let next = dev.next.clone();
            crate::util::spawn(async move {
                use futures::StreamExt;
                while let Some(update) = updates.next().await {
                    match update {
                        Ok(u) => *next.lock() = next_tracks(&u, &id),
                        Err(e) => debug!("Spotify Connect: a cluster update: {e}"),
                    }
                }
            });
        }
        Err(e) => warn!("Spotify Connect: \"{name}\" won't know what plays next: {e}"),
    }
    info!("Spotify Connect: device \"{name}\" ready");

    // App → player
    let server = Arc::downgrade(server);
    crate::util::spawn(async move {
        while let Some(ev) = events.recv().await {
            let Some(server) = server.upgrade() else {
                return;
            };
            on_event(&server, &dev, ev).await;
        }
    });
    Ok(())
}

/// (Re)start the player's Connect stream: it then plays what the decoder produces from now
/// on, without the old audio in its buffer.
async fn restart(server: &Arc<Server>, dev: &Arc<Device>) {
    *dev.grace_until.lock() = Some(Instant::now() + RESTART_GRACE);
    dev.flush_on_resume.store(false, Ordering::Relaxed);
    // Stop first: `playlist play` of the current single URL only resumes (LMS bug 14760),
    // and `time 0` while paused only moves the resume point.
    run(server, dev.mac, &["stop"]).await;
    run(server, dev.mac, &["playlist", "play", &url(&dev.mac)]).await;
}

async fn on_event(server: &Arc<Server>, dev: &Arc<Device>, ev: PlayerEvent) {
    let mac = dev.mac;
    match ev {
        // Start the player's stream as soon as a track loads: the decoder waits for it
        // (PipeSink), so waiting for `Playing` would stall the device.
        PlayerEvent::Loading { .. } => {
            let natural = dev
                .last_end
                .lock()
                .take()
                .is_some_and(|t| t.elapsed() < NATURAL_NEXT);
            if dev.reload {
                // the stream (re)starts once the track is known (`TrackChanged`), to load
                // with its title
                if !dev.driving.load(Ordering::Relaxed) {
                    info!("Spotify Connect: \"{}\" starts playing", dev.name);
                    *dev.grace_until.lock() = Some(Instant::now() + RESTART_GRACE);
                    dev.set_driving(true);
                    dev.starting.store(true, Ordering::Relaxed);
                } else {
                    dev.natural.store(natural, Ordering::Relaxed);
                }
                return;
            }
            if !dev.driving.load(Ordering::Relaxed) {
                info!("Spotify Connect: \"{}\" starts playing", dev.name);
                dev.set_driving(true);
                restart(server, dev).await;
            } else if !natural {
                // a track picked in the app: drop the old one's buffered audio
                if dev.playing.load(Ordering::Relaxed) {
                    restart(server, dev).await;
                } else {
                    dev.flush_on_resume.store(true, Ordering::Relaxed);
                }
            }
        }
        PlayerEvent::EndOfTrack { .. } => {
            *dev.last_end.lock() = Some(Instant::now());
        }
        PlayerEvent::Playing { .. } => {
            dev.playing.store(true, Ordering::Relaxed);
            if dev.in_grace() {
                return;
            }
            let Some(p) = server.player(&mac) else { return };
            let (mode, ours) = {
                let c = p.controller();
                let c = c.lock();
                (
                    c.mode(),
                    c.playing_song().map(|s| s.url().to_string()) == Some(url(&mac)),
                )
            };
            if !dev.driving.load(Ordering::Relaxed) || !ours || mode == "stop" {
                info!("Spotify Connect: \"{}\" starts playing", dev.name);
                dev.set_driving(true);
                restart(server, dev).await;
            } else if dev.flush_on_resume.load(Ordering::Relaxed) {
                restart(server, dev).await;
            } else if mode == "pause" {
                run(server, mac, &["pause", "0"]).await;
            }
        }
        PlayerEvent::Paused { .. } => {
            dev.playing.store(false, Ordering::Relaxed);
            if dev.driving.load(Ordering::Relaxed) {
                run(server, mac, &["pause", "1"]).await;
            }
        }
        PlayerEvent::Seeked { .. } => {
            // the player's buffer still holds audio from the old position
            if dev.driving.load(Ordering::Relaxed) {
                if dev.playing.load(Ordering::Relaxed) {
                    restart(server, dev).await;
                } else {
                    dev.flush_on_resume.store(true, Ordering::Relaxed);
                }
            }
        }
        PlayerEvent::Stopped { .. } | PlayerEvent::SessionDisconnected { .. } => {
            dev.playing.store(false, Ordering::Relaxed);
            if dev.set_driving(false) {
                run(server, mac, &["stop"]).await;
            }
        }
        PlayerEvent::TrackChanged { audio_item } => {
            let artist = match &audio_item.unique_fields {
                librespot_metadata::audio::UniqueFields::Track { artists, .. } => artists
                    .0
                    .iter()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            let album = match &audio_item.unique_fields {
                librespot_metadata::audio::UniqueFields::Track { album, .. } => album.clone(),
                _ => String::new(),
            };
            let cover = audio_item
                .covers
                .iter()
                .max_by_key(|c| c.width)
                .map(|c| c.url.clone());
            let url = match &audio_item.unique_fields {
                librespot_metadata::audio::UniqueFields::Track { .. } => audio_item
                    .track_id
                    .to_id()
                    .ok()
                    .map(|id| super::url_for("track", &id)),
                _ => None,
            };
            // its album and artist, to browse to from what plays (Spotify's metadata, unlike
            // the title, doesn't name the album's id)
            if let (Some(url), Some(sp)) = (url.clone(), super::get()) {
                crate::util::spawn(async move {
                    sp.track_meta(&url).await;
                });
            }
            let track = Audible {
                title: audio_item.name.clone(),
                artist,
                album,
                cover,
                url,
            };
            if dev.reload && dev.driving.load(Ordering::Relaxed) {
                return reload_for(server, dev, track).await;
            }
            // show it when it is heard: decoding runs ahead of the player by the lead
            let lead = dev.pipe.lead();
            let server = Arc::downgrade(server);
            let dev = dev.clone();
            crate::util::spawn(async move {
                tokio::time::sleep(lead).await;
                if let Some(server) = server.upgrade() {
                    now_playing(&server, &dev, &track);
                }
            });
        }
        _ => {}
    }
}

/// A new track on a player that reloads the stream per track: restart the stream with it,
/// right away when it was picked in the app, or once the last track's buffered audio has
/// played out when it follows by itself.  (The new track waits for its stream: see
/// [`PipeSink::start`].)
async fn reload_for(server: &Arc<Server>, dev: &Arc<Device>, track: Audible) {
    let natural = dev.natural.swap(false, Ordering::Relaxed);
    let starting = dev.starting.swap(false, Ordering::Relaxed);
    if natural {
        let lead = dev.pipe.lead();
        let server = Arc::downgrade(server);
        let dev = dev.clone();
        crate::util::spawn(async move {
            tokio::time::sleep(lead).await;
            let Some(server) = server.upgrade() else {
                return;
            };
            if !dev.driving.load(Ordering::Relaxed) {
                return;
            }
            now_playing(&server, &dev, &track);
            if dev.playing.load(Ordering::Relaxed) {
                restart(&server, &dev).await;
                now_playing(&server, &dev, &track);
            } else {
                dev.flush_on_resume.store(true, Ordering::Relaxed);
            }
        });
    } else if starting || dev.playing.load(Ordering::Relaxed) {
        now_playing(server, dev, &track);
        restart(server, dev).await;
        // (the new stream is a new queue entry)
        now_playing(server, dev, &track);
    } else {
        // picked while paused: the stream restarts on resume
        now_playing(server, dev, &track);
        dev.flush_on_resume.store(true, Ordering::Relaxed);
    }
}

/// Show the Connect track on the player (like a radio stream's title).
fn now_playing(server: &Arc<Server>, dev: &Device, track: &Audible) {
    *dev.audible.lock() = Some(track.clone());
    let Some(p) = server.player(&dev.mac) else {
        return;
    };
    let c = p.controller();
    let mut c = c.lock();
    let master = c.master_id;
    let ours = url(&dev.mac);
    let Some(song) = c.queue.iter_mut().find(|s| s.url() == ours) else {
        return;
    };
    song.remote_meta.title = Some(track.title.clone());
    song.remote_meta.artist = Some(track.artist.clone()).filter(|a| !a.is_empty());
    song.remote_meta.album = Some(track.album.clone()).filter(|a| !a.is_empty());
    song.remote_meta.cover = track.cover.clone();
    server.notifier.notify_owned(
        Some(master),
        vec!["playlist".into(), "newsong".into(), track.title.clone()],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::spotify::stream;

    /// One second of silence as librespot hands it to a sink.
    fn second() -> AudioPacket {
        AudioPacket::Samples(vec![0.0; (stream::SAMPLE_RATE * stream::CHANNELS) as usize])
    }

    fn frames(rx: &std::sync::mpsc::Receiver<Vec<i16>>) -> usize {
        rx.try_iter()
            .map(|c| c.len() / stream::CHANNELS as usize)
            .sum()
    }

    #[test]
    fn decoding_stays_a_lead_ahead_of_a_player_that_does_not_play() {
        let pipe = Arc::new(Pipe::default());
        let (tx, rx) = sync_channel::<Vec<i16>>(64);
        *pipe.tx.lock() = Some((0, tx));
        // no player: nothing is ever played
        let mut sink = PipeSink {
            pipe: pipe.clone(),
            player: Weak::new(),
            stream: None,
            learner: crate::plugins::spotify::learn::Learner::new(Weak::new(), Default::default()),
            reload: false,
            events: Default::default(),
        };
        let mut conv = Converter::new(None);
        let t = Instant::now();
        let mut delivered = 0;
        for _ in 0..8 {
            sink.write(second(), &mut conv).unwrap();
            delivered += frames(&rx);
        }
        let lead_frames = (LEAD.as_secs() * stream::SAMPLE_RATE as u64) as usize;
        let one = stream::SAMPLE_RATE as usize;
        // up to the lead (plus the packet that crossed it), never all 8 seconds
        assert!(
            delivered <= lead_frames + one,
            "delivered {delivered} frames"
        );
        assert!(
            delivered >= lead_frames - one,
            "delivered {delivered} frames"
        );
        // past the lead it waits ~1s for the stalled player, then drops instead of blocking
        assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
        assert!(pipe.lead() >= LEAD);
    }

    #[test]
    fn a_closed_stream_does_not_block_the_decoder() {
        let pipe = Arc::new(Pipe::default());
        let (tx, rx) = sync_channel::<Vec<i16>>(1);
        *pipe.tx.lock() = Some((0, tx));
        drop(rx);
        let mut sink = PipeSink {
            pipe: pipe.clone(),
            player: Weak::new(),
            stream: None,
            learner: crate::plugins::spotify::learn::Learner::new(Weak::new(), Default::default()),
            reload: false,
            events: Default::default(),
        };
        let t = Instant::now();
        sink.write(second(), &mut Converter::new(None)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(pipe.tx.lock().is_none(), "the closed stream is forgotten");
    }

    fn reload_sink(
        pipe: &Arc<Pipe>,
    ) -> (PipeSink, tokio::sync::mpsc::UnboundedSender<PlayerEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let events = crate::plugins::spotify::learn::EventSlot::default();
        *events.lock() = Some(rx);
        let sink = PipeSink {
            pipe: pipe.clone(),
            player: Weak::new(),
            stream: None,
            learner: crate::plugins::spotify::learn::Learner::new(Weak::new(), Default::default()),
            reload: true,
            events,
        };
        (sink, tx)
    }

    #[test]
    fn a_new_track_waits_for_its_own_stream_where_streams_reload_per_track() {
        let pipe = Arc::new(Pipe::default());
        let (old_tx, _old_rx) = sync_channel::<Vec<i16>>(4);
        *pipe.tx.lock() = Some((3, old_tx));
        let (mut sink, events) = reload_sink(&pipe);
        // a resume (no new track) goes on in the same stream at once
        let t = Instant::now();
        sink.start().unwrap();
        assert!(t.elapsed() < Duration::from_millis(100));
        // a new track: held until the stream restarts
        sink.stop().unwrap();
        events
            .send(PlayerEvent::Loading {
                play_request_id: 2,
                track_id: librespot_core::SpotifyUri::from_uri(
                    "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
                )
                .unwrap(),
                position_ms: 0,
            })
            .unwrap();
        let p = pipe.clone();
        let opener = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let (tx, rx) = sync_channel::<Vec<i16>>(4);
            *p.tx.lock() = Some((4, tx));
            rx
        });
        let t = Instant::now();
        sink.start().unwrap();
        let waited = t.elapsed();
        let _rx = opener.join().unwrap();
        assert!(waited >= Duration::from_millis(250), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        assert_eq!(sink.generation(), Some(4));
    }

    #[test]
    fn what_plays_next_is_the_tracks_of_the_cluster_while_the_device_plays() {
        use librespot_protocol::connect::Cluster;
        use librespot_protocol::player::{PlayerState, ProvidedTrack};
        let track = |uri: &str| ProvidedTrack {
            uri: uri.into(),
            ..Default::default()
        };
        let update = |active: &str| ClusterUpdate {
            cluster: Some(Cluster {
                active_device_id: active.into(),
                player_state: Some(PlayerState {
                    next_tracks: vec![
                        track("spotify:track:q1"),
                        track("spotify:delimiter"),
                        track("spotify:episode:e1"),
                        track("spotify:track:c1"),
                    ],
                    ..Default::default()
                })
                .into(),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };
        assert_eq!(
            next_tracks(&update("me"), "me"),
            ["spotify:track:q1", "spotify:track:c1"]
        );
        assert!(next_tracks(&update("phone"), "me").is_empty());
        assert!(next_tracks(&ClusterUpdate::default(), "me").is_empty());
    }

    #[test]
    fn failed_devices_are_tried_again_later_and_later() {
        let k = "0004209999aa";
        let denied = "Login request was denied: INVALID_CREDENTIALS".to_string();
        let wait = || failures().get(k).unwrap().retry_at - Instant::now();
        failed(k, "Kitchen", denied.clone());
        let first = wait();
        assert!(
            first > Duration::from_secs(8) && first <= FIRST_RETRY,
            "{first:?}"
        );
        failed(k, "Kitchen", denied.clone());
        assert!(wait() > first * 3 / 2, "doubles");
        for _ in 0..20 {
            failed(k, "Kitchen", denied.clone());
        }
        assert!(wait() <= MAX_RETRY);
        assert!(error().is_some_and(|e| e.contains("for Kitchen: Login request was denied")));
        failures().remove(k);
    }
}
