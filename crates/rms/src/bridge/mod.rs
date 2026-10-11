//! Players that are not Squeezeboxes: a web browser, UPnP renderers, Chromecasts, AirPlay
//! speakers.  Each one gets a *bridge*: a slimproto client inside RMS that connects to RMS
//! like a Squeezebox would, so the server treats it as one (queue, sync groups, alarms,
//! Don't Stop The Music, every client's controls), and drives a [`Renderer`] — the device —
//! with plain commands: play this URL, pause, resume, stop, volume.
//!
//! The renderer fetches the audio itself from the URL it is given (`/stream.mp3?player=…`,
//! relative to the server's HTTP root), in a format it declared it can play.  It reports
//! what it is doing back, and the bridge turns that into the player's status messages:
//! a track started (`STMs`), the position (`STMt`), ready for the next track (`STMd`, near
//! the end so the next one can be preloaded), out of music (`STMu`).

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
    Resume,
    Stop,
    /// 0–100.
    Volume {
        volume: u8,
        muted: bool,
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
    /// Seconds into the current track, and its length when known.
    Progress {
        elapsed: f64,
        #[serde(default)]
        #[ts(optional)]
        duration: Option<f64>,
    },
    Paused,
    Resumed,
    /// The current track ended and nothing preloaded followed.
    Ended,
    Error {
        message: String,
    },
}

/// Seconds before the end of a track at which the next one is asked for.
const PRELOAD_BEFORE_END: f64 = 10.0;

#[derive(Default)]
struct Playback {
    loaded: bool,
    playing: bool,
    elapsed: f64,
    duration: Option<f64>,
    /// Asked for the next track (`STMd`) since the current one started.
    asked_next: bool,
    /// The next track is preloaded.
    next: bool,
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
    let jiffies = move || started.elapsed().as_millis() as u32;
    info!(
        "{} ({}) connected as {}",
        identity.name, identity.model_name, identity.mac
    );

    let caps = format!(
        "Model={},ModelName={},Firmware=rms-bridge,MaxSampleRate={},{}",
        identity.model,
        identity.model_name,
        identity.max_sample_rate,
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
                    let ev = on_frame(&server, &identity, &mut pb, &to_renderer, &op, &body).await;
                    for e in ev {
                        stat(&mut sock, e, &pb, jiffies(), 0).await?;
                    }
                    if &op == b"strm" && body.first() == Some(&b't') {
                        let ts = StrmHeader::parse(&body).map(|(h, _)| h.replay_gain).unwrap_or(0);
                        stat(&mut sock, *b"STMt", &pb, jiffies(), ts).await?;
                    }
                }
            }
            msg = from_renderer.recv() => {
                let Some(msg) = msg else {
                    info!("{}: gone", identity.name);
                    let _ = sock.shutdown().await;
                    return Ok(());
                };
                for e in on_renderer(&mut pb, msg) {
                    stat(&mut sock, e, &pb, jiffies(), 0).await?;
                }
            }
            _ = tick.tick() => {
                if pb.playing {
                    stat(&mut sock, *b"STMt", &pb, jiffies(), 0).await?;
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
                    // with an interval: pause for that long (sync adjustment), ignored
                    if h.replay_gain != 0 {
                        return Vec::new();
                    }
                    let _ = to.send(ToRenderer::Pause).await;
                    Vec::new()
                }
                StrmCommand::Unpause => {
                    let _ = to.send(ToRenderer::Resume).await;
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
            pb.duration = None;
            pb.asked_next = false;
            pb.next = false;
            vec![*b"STMs"]
        }
        FromRenderer::Progress { elapsed, duration } => {
            pb.elapsed = elapsed.max(0.0);
            pb.duration = duration.filter(|d| d.is_finite() && *d > 0.0);
            match pb.duration {
                Some(d) if pb.playing && !pb.asked_next && d - pb.elapsed < PRELOAD_BEFORE_END => {
                    pb.asked_next = true;
                    vec![*b"STMd"]
                }
                _ => Vec::new(),
            }
        }
        FromRenderer::Paused => {
            pb.playing = false;
            vec![*b"STMp"]
        }
        FromRenderer::Resumed => {
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
    let ms = (pb.elapsed * 1000.0) as u32;
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
    fn near_the_end_the_next_track_is_asked_for_once() {
        let mut pb = Playback {
            loaded: true,
            ..Default::default()
        };
        assert_eq!(on_renderer(&mut pb, FromRenderer::Started), vec![*b"STMs"]);
        let at = |e| FromRenderer::Progress {
            elapsed: e,
            duration: Some(100.0),
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
        };
        assert!(on_renderer(&mut pb, live).is_empty());
        assert_eq!(
            on_renderer(&mut pb, FromRenderer::Ended),
            vec![*b"STMd", *b"STMu"]
        );
    }
}
