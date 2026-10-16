//! Players that are not Squeezeboxes: a web browser, UPnP renderers, Chromecasts, AirPlay
//! speakers.  Each one gets a *bridge*: a slimproto client inside RMS that connects to RMS
//! like a Squeezebox would, so the server treats it as one (queue, sync groups, alarms,
//! Don't Stop The Music, every client's controls), and drives a [`Renderer`] — the device —
//! with plain commands: play this URL, pause, resume, stop, volume.
//!
//! The renderer fetches the audio itself from the URL it is given (`/stream.mp3?player=…`,
//! relative to the server's HTTP root), in a format it declared it can play.  It reports
//! what it is doing back, and the bridge turns that into the player's status messages:
//! a track started (`STMs`), the position (`STMt`), ready for the next track (`STMd`, when
//! the track has been fetched, or near its end, so the next one can be preloaded), out of
//! music (`STMu`).
//!
//! An *accurate* renderer (the browser's Web Audio player) keeps time like a Squeezebox:
//! it reports its own clock and exactly where playback is on it, and it can start at a
//! given moment, wait for some milliseconds or skip some.  The server then syncs it with
//! other players as it syncs Squeezeboxes.

pub mod airplay;
pub mod browser;
pub mod chromecast;
pub mod devices;
pub mod upnp;

use crate::server::Server;
use bytes::{Buf, BytesMut};
use serde::{Deserialize, Serialize};
use slimproto::client::{Helo, Stat};
use slimproto::server::{StrmCommand, StrmHeader};
use slimproto::MacAddress;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use ts_rs::TS;

/// Who the bridged player is.
#[derive(Debug, Clone)]
pub struct Identity {
    pub mac: MacAddress,
    pub name: String,
    /// Model id (`browser`, `upnp`, `chromecast`, `airplay`).
    pub model: String,
    pub model_name: String,
    /// LMS format codes the renderer plays, best first (`flc`, `mp3`, `ogg`, `ops`, `aac`,
    /// `pcm`: raw samples, described by [`Pcm`]).
    pub formats: Vec<String>,
    /// The highest sample rate it plays; the server resamples above it.
    pub max_sample_rate: u32,
    /// Reports exact play points on its own clock and follows sync adjustments
    /// ([`FromRenderer::Clock`], `jiffies` in progress, [`ToRenderer::Hold`] / `Skip`).
    pub accurate: bool,
}

/// What raw (`pcm`) samples are like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
pub struct Pcm {
    pub bits: u32,
    pub rate: u32,
    pub channels: u32,
    pub little_endian: bool,
}

/// A stable, locally administered MAC address for a device id.
pub fn mac_for(id: &str) -> MacAddress {
    let d = md5::compute(id.as_bytes());
    MacAddress([0x02, d[0], d[1], d[2], d[3], d[4]])
}

/// What the server is about to stream to a bridged player, for renderers that show it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NowStreaming {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    /// Absolute image URL.
    pub artwork: Option<String>,
    /// Its length from where the stream starts, when known.
    pub duration: Option<f64>,
}

/// What the player `mac` is about to be streamed (its pending stream).  A Spotify Connect
/// stream is the track its device plays.
pub fn now_streaming(server: &Server, mac: &MacAddress, base_url: &str) -> Option<NowStreaming> {
    let p = server.player(mac)?;
    let (_, spec) = p.pending_stream()?;
    let (entry, song) = {
        let c = p.controller();
        let c = c.lock();
        let song = c.queue.iter().find(|s| s.id == spec.song_id)?.clone();
        (c.playlist.song(song.index)?.clone(), song)
    };
    if let Some(a) = crate::plugins::spotify::connect::audible(&entry.url) {
        let some = |s: String| Some(s).filter(|s| !s.is_empty());
        return Some(NowStreaming {
            title: a.title,
            artist: some(a.artist),
            album: some(a.album),
            track_number: None,
            artwork: a.cover,
            duration: None,
        });
    }
    let t = crate::api::state::track(server, &entry, Some(&song));
    let artwork = t.artwork.map(|art| {
        if art.starts_with("http://") || art.starts_with("https://") {
            art
        } else {
            format!("{base_url}/{art}_500x500_o")
        }
    });
    Some(NowStreaming {
        title: t.title,
        artist: t.artist,
        album: t.album,
        track_number: t.track_number,
        artwork,
        duration: t
            .duration
            .map(|d| d - song.start_offset)
            .filter(|d| *d > 0.0),
    })
}

/// What the bridge asks of the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToRenderer {
    /// Play `url` (server-relative); with `autostart` false, get ready but wait for `Resume`.
    Load {
        url: String,
        format: String,
        /// For `pcm`.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pcm: Option<Pcm>,
        autostart: bool,
    },
    /// The track after the current one: start it when the current one ends.
    Preload {
        url: String,
        format: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pcm: Option<Pcm>,
    },
    Pause,
    /// Resume, or start what was loaded without autostart; for an accurate renderer
    /// possibly `at` a moment of its clock (sync).
    Resume {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        at: Option<u32>,
    },
    Stop,
    /// 0–100.
    Volume {
        volume: u8,
        muted: bool,
    },
    /// Play silence for `ms`, then go on (a sync adjustment: the others are behind).
    Hold {
        ms: u32,
    },
    /// Skip `ms` of audio (a sync adjustment: this player is behind).
    Skip {
        ms: u32,
    },
}

/// What the renderer tells the bridge.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromRenderer {
    /// Loaded enough to start (for `Load` without autostart).
    Ready,
    /// A track (the loaded or the preloaded one) started playing.
    Started,
    /// Seconds into the current track, and its length when known.  An accurate renderer
    /// gives the moment of its clock this is true at (`jiffies`, now).
    Progress {
        elapsed: f64,
        #[serde(default)]
        #[ts(optional)]
        duration: Option<f64>,
        #[serde(default)]
        #[ts(optional)]
        jiffies: Option<u32>,
    },
    /// The renderer's clock (milliseconds, wrapping), now.
    Clock {
        jiffies: u32,
    },
    /// The current track's audio has all been fetched: the next one can come.
    Fetched,
    Paused,
    Resumed,
    /// The current track ended and nothing preloaded followed.
    Ended,
    Error {
        message: String,
    },
}

/// A renderer's position, kept smooth from what it reports.  Renderers that aren't
/// accurate report positions that are rounded down (UPnP's `RelTime` is mostly in whole
/// seconds) or stale (some only refresh it every few seconds); taken as they come, the
/// player's time would jump back at every report.
///
/// Each report puts the moment the track started (its *origin*) in a window: within the
/// second before for a whole-second position, exactly for one with a fraction.  Reports
/// that fit narrow the window, and the position is reckoned from its middle.  A report up
/// to [`STALE_REPORT`] behind it is stale and changes nothing; one ahead of it means the
/// position was behind, and one further behind is a seek or a stall: either starts a new
/// window.
pub(crate) struct PositionClock {
    /// Times are seconds since this.
    base: Instant,
    /// Earliest and latest possible origin.
    origin: Option<(f64, f64)>,
}

/// Leeway for the time a request takes to reach the renderer and come back.
const POSITION_LEEWAY: f64 = 0.05;
/// How far behind a report may be and still be taken as stale.
const STALE_REPORT: f64 = 5.0;

impl PositionClock {
    pub(crate) fn new() -> PositionClock {
        PositionClock {
            base: Instant::now(),
            origin: None,
        }
    }

    /// Start over (a new track, a resume).
    pub(crate) fn reset(&mut self) {
        self.origin = None;
    }

    fn secs(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.base).as_secs_f64()
    }

    /// A report of `pos` seconds (`whole`: rounded down to a second) true at `at`; returns
    /// the position at `now`.
    pub(crate) fn report(&mut self, pos: f64, whole: bool, at: Instant, now: Instant) -> f64 {
        let t = self.secs(at);
        let (lo, hi) = (t - pos - if whole { 1.0 } else { 0.0 }, t - pos);
        self.origin = match self.origin {
            // fits: narrow the window
            Some((a, b)) if lo.max(a) <= hi.min(b) + POSITION_LEEWAY => {
                let (lo, hi) = (lo.max(a), hi.min(b));
                if lo <= hi {
                    Some((lo, hi))
                } else {
                    let mid = (lo + hi) / 2.0;
                    Some((mid, mid))
                }
            }
            // a little behind: stale
            Some((a, b)) if lo > b && lo - b <= STALE_REPORT => Some((a, b)),
            _ => Some((lo, hi)),
        };
        let (lo, hi) = self.origin.unwrap_or((lo, hi));
        (self.secs(now) - (lo + hi) / 2.0).max(0.0)
    }
}

/// Seconds before the end of a track at which the next one is asked for.
const PRELOAD_BEFORE_END: f64 = 10.0;

#[derive(Default)]
struct Playback {
    loaded: bool,
    playing: bool,
    /// Seconds into the track at `elapsed_at`.
    elapsed: f64,
    elapsed_at: Option<Instant>,
    duration: Option<f64>,
    /// Asked for the next track (`STMd`) since the current one started.
    asked_next: bool,
    /// The next track is preloaded.
    next: bool,
}

impl Playback {
    /// Seconds into the track now: the last report, moved on by the time since while
    /// playing (a status message must not carry a stale position with a current clock).
    fn elapsed_now(&self) -> f64 {
        match (self.playing, self.elapsed_at) {
            (true, Some(at)) => self.elapsed + at.elapsed().as_secs_f64(),
            _ => self.elapsed,
        }
    }
}

/// Connect `identity` to the server and relay between it and the renderer until the
/// renderer goes away (its sender closes) or the server drops the connection.
pub async fn run(
    server: Arc<Server>,
    identity: Identity,
    to_renderer: mpsc::Sender<ToRenderer>,
    mut from_renderer: mpsc::Receiver<FromRenderer>,
) -> anyhow::Result<()> {
    let port = server.config.slimproto_port;
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await?;
    sock.set_nodelay(true)?;
    let started = Instant::now();
    // the player's clock: ours, or the renderer's when it reports one
    let mut offset: u32 = 0;
    if identity.accurate {
        // its clock first, so the server knows it from the first status message
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, from_renderer.recv()).await {
                Ok(Some(FromRenderer::Clock { jiffies })) => {
                    offset = jiffies.wrapping_sub(started.elapsed().as_millis() as u32);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(_) => anyhow::bail!("{}: no clock from the renderer", identity.name),
            }
        }
    }
    let local = move || started.elapsed().as_millis() as u32;
    info!(
        "{} ({}) connected as {}",
        identity.name, identity.model_name, identity.mac
    );

    let caps = format!(
        "Model={},ModelName={},Firmware=rms-bridge,MaxSampleRate={},{}{}",
        identity.model,
        identity.model_name,
        identity.max_sample_rate,
        if identity.accurate {
            "AccuratePlayPoints=1,"
        } else {
            ""
        },
        identity.formats.join(",")
    );
    let helo = Helo {
        device_id: 12, // squeezelite: a software player
        revision: 0,
        mac: identity.mac,
        uuid: None,
        wlan_channel_list: 0,
        bitmapped: false,
        reconnect: false,
        bytes_received: Some(0),
        language: Some(*b"EN"),
        capabilities: Some(caps),
    };
    let mut out = BytesMut::new();
    helo.encode(&mut out);
    send(&mut sock, b"HELO", &out).await?;

    let mut pb = Playback::default();
    let mut buf = BytesMut::with_capacity(4096);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            n = sock.read_buf(&mut buf) => {
                if n? == 0 {
                    info!("{}: the server closed the connection", identity.name);
                    return Ok(());
                }
                while let Some((op, body)) = next_frame(&mut buf) {
                    if &op == b"setd" && body.first() == Some(&0) && body.len() == 1 {
                        // the server asks for the player's name
                        let mut name = vec![0u8];
                        name.extend_from_slice(identity.name.as_bytes());
                        name.push(0);
                        send(&mut sock, b"SETD", &name).await?;
                        continue;
                    }
                    if &op == b"strm" {
                        if let Some((h, _)) = StrmHeader::parse(&body) {
                            debug!("{}: strm {:?}", identity.name, h.command);
                        }
                    }
                    let ev = on_frame(&server, &identity, &mut pb, &to_renderer, &op, &body).await;
                    let jiffies = local().wrapping_add(offset);
                    for e in ev {
                        stat(&mut sock, e, &pb, jiffies, 0).await?;
                    }
                    if &op == b"strm" && body.first() == Some(&b't') {
                        let ts = StrmHeader::parse(&body).map(|(h, _)| h.replay_gain).unwrap_or(0);
                        stat(&mut sock, *b"STMt", &pb, jiffies, ts).await?;
                    }
                }
            }
            msg = from_renderer.recv() => {
                let Some(msg) = msg else {
                    info!("{}: gone", identity.name);
                    let _ = sock.shutdown().await;
                    return Ok(());
                };
                debug!("{}: the device reports {msg:?}", identity.name);
                // an accurate renderer's report is true at its clock's `jiffies`: keep ours
                // in step, so the status message carries exactly that moment
                if let FromRenderer::Clock { jiffies }
                | FromRenderer::Progress { jiffies: Some(jiffies), .. } = &msg
                {
                    if identity.accurate {
                        offset = jiffies.wrapping_sub(local());
                    }
                }
                let progress = matches!(msg, FromRenderer::Progress { .. });
                let ev = on_renderer(&mut pb, msg);
                let jiffies = local().wrapping_add(offset);
                for e in &ev {
                    stat(&mut sock, *e, &pb, jiffies, 0).await?;
                }
                // a play point for the server's sync, as squeezelite sends them
                if progress && identity.accurate && ev.is_empty() && pb.playing {
                    stat(&mut sock, *b"STMt", &pb, jiffies, 0).await?;
                }
            }
            _ = tick.tick() => {
                // accurate renderers send their play points themselves
                if pb.playing && !identity.accurate {
                    stat(&mut sock, *b"STMt", &pb, local().wrapping_add(offset), 0).await?;
                }
            }
        }
    }
}

/// Split one server frame (`u16` length, 4-byte op, payload) off `buf`.
fn next_frame(buf: &mut BytesMut) -> Option<([u8; 4], Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if buf.len() < 2 + len || len < 4 {
        return None;
    }
    buf.advance(2);
    let frame = buf.split_to(len);
    let mut op = [0u8; 4];
    op.copy_from_slice(&frame[..4]);
    Some((op, frame[4..].to_vec()))
}

/// The status events a server frame calls for.
async fn on_frame(
    server: &Arc<Server>,
    identity: &Identity,
    pb: &mut Playback,
    to: &mpsc::Sender<ToRenderer>,
    op: &[u8; 4],
    body: &[u8],
) -> Vec<[u8; 4]> {
    match op {
        b"strm" => {
            let Some((h, rest)) = StrmHeader::parse(body) else {
                return Vec::new();
            };
            match h.command {
                StrmCommand::Start => {
                    let Some(url) = request_path(rest) else {
                        warn!("{}: strm without a request", identity.name);
                        return vec![*b"STMn"];
                    };
                    let format = format_code(h.format.byte());
                    let pcm = (format == "pcm").then(|| pcm_of(&h));
                    // a start while a track plays is the next one (we asked with STMd)
                    if pb.playing && pb.asked_next {
                        pb.next = true;
                        let _ = to.send(ToRenderer::Preload { url, format, pcm }).await;
                        return vec![*b"STMc"];
                    }
                    let autostart = matches!(h.autostart, b'1' | b'3');
                    *pb = Playback {
                        loaded: true,
                        ..Default::default()
                    };
                    debug!("{}: load {url} ({format})", identity.name);
                    let _ = to
                        .send(ToRenderer::Load {
                            url,
                            format,
                            pcm,
                            autostart,
                        })
                        .await;
                    vec![*b"STMc"]
                }
                StrmCommand::Pause => {
                    // with an interval: silence for that long (a sync adjustment)
                    if h.replay_gain != 0 {
                        if identity.accurate {
                            let _ = to.send(ToRenderer::Hold { ms: h.replay_gain }).await;
                        }
                        return Vec::new();
                    }
                    let _ = to.send(ToRenderer::Pause).await;
                    Vec::new()
                }
                StrmCommand::Unpause => {
                    // a non-zero interval is the moment (player clock) to start at
                    let at = (identity.accurate && h.replay_gain != 0).then_some(h.replay_gain);
                    let _ = to.send(ToRenderer::Resume { at }).await;
                    Vec::new()
                }
                StrmCommand::SkipAhead => {
                    if identity.accurate && h.replay_gain != 0 {
                        let _ = to.send(ToRenderer::Skip { ms: h.replay_gain }).await;
                    }
                    Vec::new()
                }
                StrmCommand::Stop => {
                    *pb = Playback::default();
                    let _ = to.send(ToRenderer::Stop).await;
                    vec![*b"STMf"]
                }
                StrmCommand::Flush => {
                    pb.next = false;
                    vec![*b"STMf"]
                }
                _ => Vec::new(),
            }
        }
        b"audg" | b"aude" => {
            // the player's volume as the server holds it, rather than decoding the gain
            if let Some(p) = server.player(&identity.mac) {
                let muted = p.is_muted() || (op == b"aude" && body.first() == Some(&0));
                let volume = p.volume().clamp(0, 100) as u8;
                let _ = to.send(ToRenderer::Volume { volume, muted }).await;
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// The status events a renderer report calls for.
fn on_renderer(pb: &mut Playback, msg: FromRenderer) -> Vec<[u8; 4]> {
    match msg {
        FromRenderer::Ready => vec![*b"STMl"],
        FromRenderer::Started => {
            pb.playing = true;
            pb.loaded = true;
            pb.elapsed = 0.0;
            pb.elapsed_at = Some(Instant::now());
            pb.duration = None;
            pb.asked_next = false;
            pb.next = false;
            vec![*b"STMs"]
        }
        FromRenderer::Progress {
            elapsed, duration, ..
        } => {
            pb.elapsed = elapsed.max(0.0);
            pb.elapsed_at = Some(Instant::now());
            pb.duration = duration.filter(|d| d.is_finite() && *d > 0.0);
            match pb.duration {
                Some(d) if pb.playing && !pb.asked_next && d - pb.elapsed < PRELOAD_BEFORE_END => {
                    pb.asked_next = true;
                    vec![*b"STMd"]
                }
                _ => Vec::new(),
            }
        }
        FromRenderer::Clock { .. } => Vec::new(),
        FromRenderer::Fetched => {
            if pb.asked_next {
                return Vec::new();
            }
            pb.asked_next = true;
            vec![*b"STMd"]
        }
        FromRenderer::Paused => {
            pb.elapsed = pb.elapsed_now();
            pb.playing = false;
            vec![*b"STMp"]
        }
        FromRenderer::Resumed => {
            pb.elapsed_at = Some(Instant::now());
            pb.playing = true;
            vec![*b"STMr"]
        }
        FromRenderer::Ended => {
            pb.playing = false;
            pb.loaded = false;
            let mut ev = Vec::new();
            if !pb.asked_next {
                pb.asked_next = true;
                ev.push(*b"STMd");
            }
            ev.push(*b"STMu");
            ev
        }
        FromRenderer::Error { message } => {
            warn!("renderer error: {message}");
            pb.playing = false;
            pb.loaded = false;
            vec![*b"STMn"]
        }
    }
}

/// `GET /stream.mp3?player=… HTTP/1.0` → `/stream.mp3?player=…`.
fn request_path(req: &[u8]) -> Option<String> {
    let line = std::str::from_utf8(req).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    Some(parts.next()?.to_string())
}

/// The sample format of a `pcm` stream as its `strm` header gives it (16-bit 44.1 kHz
/// stereo little-endian where it says "unknown").
fn pcm_of(h: &StrmHeader) -> Pcm {
    use slimproto::server::{PcmSampleRate, PcmSampleSize};
    Pcm {
        bits: PcmSampleSize::bits(h.pcm_sample_size).unwrap_or(16),
        rate: PcmSampleRate::hz(h.pcm_sample_rate).unwrap_or(44100),
        channels: if h.pcm_channels == b'1' { 1 } else { 2 },
        little_endian: h.pcm_endian != b'0',
    }
}

/// `strm` format byte → LMS format code.
fn format_code(b: u8) -> String {
    match b {
        b'f' => "flc",
        b'o' => "ogg",
        b'u' => "ops",
        b'a' => "aac",
        b'p' => "pcm",
        b'l' => "alc",
        _ => "mp3",
    }
    .to_string()
}

async fn send(sock: &mut TcpStream, op: &[u8; 4], payload: &[u8]) -> std::io::Result<()> {
    let mut f = Vec::with_capacity(8 + payload.len());
    f.extend_from_slice(op);
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(payload);
    sock.write_all(&f).await
}

async fn stat(
    sock: &mut TcpStream,
    event: [u8; 4],
    pb: &Playback,
    jiffies: u32,
    server_timestamp: u32,
) -> std::io::Result<()> {
    let ms = (pb.elapsed_now() * 1000.0) as u32;
    let active = pb.loaded || pb.playing;
    let st = Stat {
        event_code: event,
        buffer_size: 2 << 20,
        fullness: if active { 1 << 20 } else { 0 },
        bytes_received: 0,
        signal_strength: 0xffff,
        jiffies,
        output_buffer_size: 3_528_000,
        output_buffer_fullness: if pb.playing { 1_764_000 } else { 0 },
        elapsed_seconds: ms / 1000,
        elapsed_milliseconds: ms,
        server_timestamp,
        ..Default::default()
    };
    let mut b = BytesMut::new();
    st.encode(&mut b);
    send(sock, b"STAT", &b).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_formats() {
        assert_eq!(
            request_path(b"GET /stream.mp3?player=02:aa HTTP/1.0\r\n\r\n").as_deref(),
            Some("/stream.mp3?player=02:aa")
        );
        assert_eq!(format_code(b'f'), "flc");
        assert_eq!(mac_for("x"), mac_for("x"));
        assert_ne!(mac_for("x"), mac_for("y"));
        assert_eq!(mac_for("x").0[0], 0x02);
    }

    #[test]
    fn whole_second_positions_to_a_fraction() {
        let mut c = PositionClock::new();
        let base = c.base;
        let at = |t: f64| base + Duration::from_secs_f64(t);
        // the track started at 0.37 s; polls every 0.75 s from 1 s, each answered with
        // the position rounded down
        let origin = 0.37;
        for k in 0..8 {
            let t = 1.0 + 0.75 * k as f64;
            let est = c.report((t - origin).floor(), true, at(t), at(t));
            let truth = t - origin;
            let within = if k >= 3 { 0.15 } else { 0.6 };
            assert!((est - truth).abs() < within, "poll {k}: {est} for {truth}");
        }
        // a seek: the next report doesn't fit, and starts over from it
        let est = c.report(60.0, true, at(7.5), at(7.5));
        assert!((60.0..61.0).contains(&est), "{est}");
        // a position with a fraction is taken as it is
        let mut c = PositionClock::new();
        let at = |t: f64| c.base + Duration::from_secs_f64(t);
        let (t, now) = (at(2.0), at(2.5));
        let est = c.report(1.234, false, t, now);
        assert!((est - 1.734).abs() < 0.01, "{est}");
    }

    #[test]
    fn stale_positions_dont_pull_the_time_back() {
        let mut c = PositionClock::new();
        let base = c.base;
        let at = |t: f64| base + Duration::from_secs_f64(t);
        // a renderer that refreshes its position every 3 s, polled every 0.75 s: the time
        // keeps going, close to the truth, never back
        let origin = 0.2;
        let mut last = 0.0;
        for k in 0..40 {
            let t = 0.5 + 0.75 * k as f64;
            let truth = t - origin;
            let reported = (truth / 3.0).floor() * 3.0;
            let est = c.report(reported, true, at(t), at(t));
            assert!(est >= last - 0.05, "poll {k}: back from {last} to {est}");
            if k >= 8 {
                assert!((est - truth).abs() < 0.6, "poll {k}: {est} for {truth}");
            }
            last = est;
        }
        // stopped (a stall) for longer than a stale report can be: back to what it says
        let t = 0.5 + 0.75 * 40.0 + 10.0;
        let est = c.report(last - 1.0, true, at(t), at(t));
        assert!((last - 1.0..last).contains(&est), "{est}");
    }

    #[test]
    fn near_the_end_the_next_track_is_asked_for_once() {
        let mut pb = Playback {
            loaded: true,
            ..Default::default()
        };
        assert_eq!(on_renderer(&mut pb, FromRenderer::Started), vec![*b"STMs"]);
        let at = |e| FromRenderer::Progress {
            elapsed: e,
            duration: Some(100.0),
            jiffies: None,
        };
        assert!(on_renderer(&mut pb, at(50.0)).is_empty());
        assert_eq!(on_renderer(&mut pb, at(91.0)), vec![*b"STMd"]);
        assert!(on_renderer(&mut pb, at(95.0)).is_empty());
        // ended without a next track: out of music
        assert_eq!(on_renderer(&mut pb, FromRenderer::Ended), vec![*b"STMu"]);
        // a stream without an end asks only when it ends
        on_renderer(&mut pb, FromRenderer::Started);
        let live = FromRenderer::Progress {
            elapsed: 5000.0,
            duration: None,
            jiffies: None,
        };
        assert!(on_renderer(&mut pb, live).is_empty());
        assert_eq!(
            on_renderer(&mut pb, FromRenderer::Ended),
            vec![*b"STMd", *b"STMu"]
        );
    }
}
