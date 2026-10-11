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
//! * Devices are registered with the account (no zeroconf), so they are only visible to the
//!   account RMS is signed in with.

use super::Spotify;
use crate::control::request::Source;
use crate::server::Server;
use dashmap::DashMap;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig};
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::config::PlayerConfig;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot_playback::player::{Player, PlayerEvent};
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

static DEVICES: OnceLock<DashMap<String, Arc<Device>>> = OnceLock::new();

fn devices() -> &'static DashMap<String, Arc<Device>> {
    DEVICES.get_or_init(DashMap::new)
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
}

impl PipeSink {
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
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        let mut samples = converter.f64_to_s16(&samples);
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
        tokio::spawn(async move {
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
    tokio::spawn(async move {
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
    let signed_in = sp
        .credentials_cache()
        .and_then(|c| c.credentials())
        .is_some();
    let players: Vec<_> = server
        .players()
        .into_iter()
        .filter(|p| signed_in && p.is_connected())
        .collect();
    // remove devices of players that left (or of everyone, when signed out)
    let keep: Vec<String> = players.iter().map(|p| key(&p.id)).collect();
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
                if let Err(e) = add_device(server, sp, &p).await {
                    warn!("Spotify Connect for {}: {e}", p.name());
                }
            }
            Some(dev) if !dev.alive.load(Ordering::Relaxed) => {
                warn!(
                    "Spotify Connect: device \"{}\" lost its session; restarting",
                    dev.name
                );
                if dev.set_driving(false) {
                    run(server, dev.mac, &["stop"]).await;
                }
                devices().remove(&k);
            }
            Some(dev) if dev.name != p.name() => {
                // renamed: re-register under the new name
                let _ = dev.spirc.shutdown();
                devices().remove(&k);
            }
            Some(dev) => follow_player(&dev, &p).await,
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
    let credentials = sp
        .credentials_cache()
        .and_then(|c| c.credentials())
        .ok_or("not signed in")?;
    let name = p.name();
    let k = key(&p.id);
    // one session per device: Connect state is per session
    let session = Session::new(
        SessionConfig {
            device_id: format!("{:0>40}", k),
            ..Default::default()
        },
        sp.credentials_cache(),
    );
    let bitrate = server.prefs.server.get_i64("spotifyBitrate").max(96) as u32;
    let normalise = server.prefs.server.get_bool("spotifyNormalisation");
    let config = PlayerConfig {
        // one endless stream: librespot does the gapless part
        gapless: true,
        ..super::stream::player_config(bitrate, normalise)
    };
    let pipe = Arc::new(Pipe::default());
    let sink_pipe = pipe.clone();
    let sink_player = Arc::downgrade(p);
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || {
        Box::new(PipeSink {
            pipe: sink_pipe,
            player: sink_player,
            stream: None,
        })
    });
    let initial = ((p.volume().clamp(0, 100) as u32 * 65535) / 100) as u16;
    let mixer = Arc::new(PlayerMixer {
        server: Arc::downgrade(server),
        mac: p.id,
        volume: AtomicU16::new(initial),
        forward: AtomicBool::new(false),
    });
    let mut events = player.get_player_event_channel();
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
    tokio::spawn(async move {
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
    });
    devices().insert(k, dev.clone());
    info!("Spotify Connect: device \"{name}\" ready");

    // App → player
    let server = Arc::downgrade(server);
    tokio::spawn(async move {
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
            // show it when it is heard: decoding runs ahead of the player by the lead
            let lead = dev.pipe.lead();
            let server = Arc::downgrade(server);
            let title = audio_item.name.clone();
            tokio::spawn(async move {
                tokio::time::sleep(lead).await;
                if let Some(server) = server.upgrade() {
                    now_playing(&server, mac, &title, &artist, &album, cover);
                }
            });
        }
        _ => {}
    }
}

/// Show the Connect track on the player (like a radio stream's title).
fn now_playing(
    server: &Arc<Server>,
    mac: MacAddress,
    title: &str,
    artist: &str,
    album: &str,
    cover: Option<String>,
) {
    let Some(p) = server.player(&mac) else { return };
    let c = p.controller();
    let mut c = c.lock();
    let master = c.master_id;
    let ours = url(&mac);
    let Some(song) = c.queue.iter_mut().find(|s| s.url() == ours) else {
        return;
    };
    song.remote_meta.title = Some(title.to_string());
    song.remote_meta.artist = Some(artist.to_string()).filter(|a| !a.is_empty());
    song.remote_meta.album = Some(album.to_string()).filter(|a| !a.is_empty());
    song.remote_meta.cover = cover;
    server.notifier.notify_owned(
        Some(master),
        vec!["playlist".into(), "newsong".into(), title.to_string()],
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
        };
        let t = Instant::now();
        sink.write(second(), &mut Converter::new(None)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(pipe.tx.lock().is_none(), "the closed stream is forgotten");
    }
}
