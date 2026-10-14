//! Spectrum bars for streams (radio, Spotify): the bytes a player is sent are also decoded
//! (by ffmpeg, to mono at the analysis rate) and analysed as they go
//! ([`rms_library::spectrum::Live`]), so the curve lines up with the player's position
//! exactly, counted in the same audio.  Library tracks have stored curves instead
//! ([`rms_library::Library::track_spectrum`]).
//!
//! Curves are kept by song (a sync group's players share one), the last
//! [`KEEP_SECS`] of each, and dropped a while after their stream ends.

use super::source::ByteStream;
use super::{SourceKind, StreamSpec};
use bytes::Bytes;
use futures::StreamExt;
use parking_lot::Mutex;
use rms_library::spectrum::{Live, FPS, FRAME_BYTES};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::{Duration, Instant};
use tracing::debug;

/// Seconds of a stream's curve kept (radio plays for hours).
const KEEP_SECS: usize = 600;
/// How long a finished stream's curve stays (its player may still be playing the buffer).
const LINGER: Duration = Duration::from_secs(120);

/// A stream's curve so far.
#[derive(Default)]
struct Curve {
    /// position in the song (seconds) of the stream's first reading
    start: f64,
    /// index of the first reading kept
    base: u64,
    /// packed readings from `base` on
    data: Vec<u8>,
    /// when the stream ended
    ended: Option<Instant>,
}

static CURVES: LazyLock<Mutex<HashMap<u64, Arc<Mutex<Curve>>>>> = LazyLock::new(Default::default);

/// Part of a stream's curve.
pub struct Window {
    /// position in the song (seconds) of the first reading
    pub start: f64,
    /// packed readings, [`FPS`] a second
    pub data: Vec<u8>,
}

/// The readings of `song_id`'s stream from about `from` seconds into the song (a second
/// earlier, for the client to blend from), or `None` when it isn't being analysed.
pub fn window(song_id: u64, from: f64) -> Option<Window> {
    let curve = CURVES.lock().get(&song_id)?.clone();
    let c = curve.lock();
    let n = (c.data.len() / FRAME_BYTES) as u64;
    let want = ((from - c.start - 1.0) * FPS as f64).floor().max(0.0) as u64;
    let first = want.clamp(c.base, c.base + n);
    let skip = (first - c.base) as usize * FRAME_BYTES;
    Some(Window {
        start: c.start + first as f64 / FPS as f64,
        data: c.data[skip..].to_vec(),
    })
}

/// How ffmpeg reads a stream's bytes (`-f` and friends), or `None` for streams that aren't
/// analysed: local files (their curves are stored) and formats that can't be read from a pipe.
fn input_args(spec: &StreamSpec) -> Option<Vec<String>> {
    let live = match &spec.source {
        SourceKind::Remote { .. } | SourceKind::Spotify { .. } => true,
        // a stream transcoded from its URL
        SourceKind::Ffmpeg { args } => args
            .iter()
            .position(|a| a == "-i")
            .and_then(|i| args.get(i + 1))
            .is_some_and(|u| u.starts_with("http://") || u.starts_with("https://")),
        SourceKind::File { .. } => false,
    };
    if !live {
        return None;
    }
    let pcm = |args: &[&str]| -> Vec<String> {
        // what ffmpeg_args writes for `pcm`: s16le stereo at the rate it was given
        let given = |flag: &str| match &spec.source {
            SourceKind::Ffmpeg { args } => args
                .iter()
                .rposition(|a| a == flag)
                .and_then(|i| args.get(i + 1).cloned()),
            _ => None,
        };
        let rate = given("-ar").unwrap_or_else(|| "44100".into());
        let channels = given("-ac").unwrap_or_else(|| "2".into());
        let format = match &spec.source {
            SourceKind::Ffmpeg { args } if args.iter().any(|a| a == "s24le") => "s24le",
            _ => "s16le",
        };
        let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        v.extend(["-f", format, "-ar", &rate, "-ac", &channels].map(String::from));
        v
    };
    let format = match spec.format.as_str() {
        "mp3" => "mp3",
        "aac" => "aac",
        "ogg" | "ops" => "ogg",
        "flc" => "flac",
        "wma" => "asf",
        "pcm" => return Some(pcm(&[])),
        _ => return None,
    };
    Some(vec!["-f".into(), format.into()])
}

/// The stream `src` of `spec`, analysed as it is read.  Streams that aren't analysed, and
/// everything when there is no ffmpeg, pass through untouched.
pub fn tap(src: ByteStream, spec: &StreamSpec) -> ByteStream {
    let Some(input) = input_args(spec) else {
        return src;
    };
    let start = match &spec.source {
        SourceKind::Spotify { start_ms, .. } => *start_ms as f64 / 1000.0,
        _ => 0.0,
    };
    let Some(tx) = start_analysis(spec.song_id, start, input) else {
        return src;
    };
    src.inspect(move |chunk| {
        if let Ok(b) = chunk {
            // the analysis ended (ffmpeg failed): the stream plays on regardless
            let _ = tx.send(b.clone());
        }
    })
    .boxed()
}

/// Start ffmpeg and the analysis of a new stream of `song_id`; the bytes go to the returned
/// sender, and dropping it ends the analysis.
fn start_analysis(song_id: u64, start: f64, input: Vec<String>) -> Option<mpsc::Sender<Bytes>> {
    let ffmpeg = crate::server::ffmpeg_path();
    let rate = rms_library::sonic::SAMPLE_RATE;
    let mut child = Command::new(&ffmpeg)
        .args(["-nostdin", "-hide_banner", "-v", "error"])
        .args(["-probesize", "32768", "-analyzeduration", "0"])
        .args(&input)
        .args([
            "-i",
            "pipe:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| debug!("spectrum: cannot start ffmpeg: {e}"))
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let curve = Arc::new(Mutex::new(Curve {
        start,
        ..Default::default()
    }));
    {
        let mut all = CURVES.lock();
        // forget streams that ended a while ago
        all.retain(|_, c| c.lock().ended.is_none_or(|t| t.elapsed() < LINGER));
        // a new stream of the same song (reconnect, seek) starts a new curve
        all.insert(song_id, curve.clone());
    }
    let (tx, rx) = mpsc::channel::<Bytes>();
    std::thread::Builder::new()
        .name("spectrum-in".into())
        .spawn(move || {
            for b in rx {
                if stdin.write_all(&b).is_err() {
                    break;
                }
            }
            // dropping stdin ends ffmpeg's input
        })
        .ok()?;
    std::thread::Builder::new()
        .name("spectrum".into())
        .spawn(move || {
            let mut live = Live::new(rate);
            let mut buf = vec![0u8; 4 * 4096];
            let mut have = 0;
            loop {
                let n = match stdout.read(&mut buf[have..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                have += n;
                let whole = have / 4 * 4;
                let samples: Vec<f32> = buf[..whole]
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                buf.copy_within(whole..have, 0);
                have -= whole;
                let readings = live.feed(&samples);
                if !readings.is_empty() {
                    let mut c = curve.lock();
                    c.data.extend_from_slice(&readings);
                    let keep = KEEP_SECS * FPS as usize * FRAME_BYTES;
                    if c.data.len() > keep + FPS as usize * 60 * FRAME_BYTES {
                        let drop = c.data.len() - keep;
                        c.data.drain(..drop);
                        c.base += (drop / FRAME_BYTES) as u64;
                    }
                }
            }
            let _ = child.wait();
            curve.lock().ended = Some(Instant::now());
            debug!(
                "spectrum: stream of song {song_id} ended after {} readings",
                live.count()
            );
        })
        .ok()?;
    Some(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(source: SourceKind, format: &str) -> StreamSpec {
        StreamSpec {
            source,
            format: format.into(),
            song_id: 1,
            shared: None,
        }
    }

    #[test]
    fn only_streams_are_analysed() {
        let file = SourceKind::File {
            path: "/music/a.flac".into(),
            offset: 0,
            length: None,
        };
        assert!(input_args(&spec(file, "flc")).is_none());
        let radio = SourceKind::Remote {
            url: "http://radio.example/stream".into(),
        };
        assert_eq!(
            input_args(&spec(radio.clone(), "mp3")).unwrap(),
            ["-f", "mp3"]
        );
        assert_eq!(input_args(&spec(radio, "aac")).unwrap(), ["-f", "aac"]);
        // a local file transcoded: stored curve, not analysed
        let local = SourceKind::Ffmpeg {
            args: ["-i", "/music/a.flac", "-f", "mp3", "pipe:1"]
                .map(String::from)
                .to_vec(),
        };
        assert!(input_args(&spec(local, "mp3")).is_none());
        // HLS radio transcoded to PCM at 48 kHz
        let hls = SourceKind::Ffmpeg {
            args: [
                "-i",
                "https://r.example/a.m3u8",
                "-ar",
                "48000",
                "-f",
                "s16le",
                "pipe:1",
            ]
            .map(String::from)
            .to_vec(),
        };
        assert_eq!(
            input_args(&spec(hls, "pcm")).unwrap(),
            ["-f", "s16le", "-ar", "48000", "-ac", "2"]
        );
    }

    #[test]
    fn windows_start_a_second_early_and_keep_their_place() {
        let curve = Arc::new(Mutex::new(Curve {
            start: 10.0,
            base: 40,
            data: vec![0; 100 * FRAME_BYTES],
            ended: None,
        }));
        CURVES.lock().insert(777, curve);
        // reading 40 is at 12 s; asking from 14 s gives readings from 13 s (reading 60)
        let w = window(777, 14.0).unwrap();
        assert_eq!(w.start, 13.0);
        assert_eq!(w.data.len(), 80 * FRAME_BYTES);
        // before what is kept: from the first reading kept
        assert_eq!(window(777, 0.0).unwrap().start, 12.0);
        // past the end: empty, at the end
        let w = window(777, 100.0).unwrap();
        assert!(w.data.is_empty());
        assert_eq!(w.start, 17.0);
        assert!(window(778, 0.0).is_none());
        CURVES.lock().remove(&777);
    }
}
