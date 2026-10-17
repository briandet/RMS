//! Audio delivery: deciding how a song reaches a player (direct, passthrough, transcoded),
//! building `strm s` frames and serving `/stream.mp3`.
//!
//! This is the equivalent of `Slim::Player::Song::open`, `Slim::Player::TranscodingHelper`
//! and `Slim::Player::Squeezebox::stream_s`.  The reference server drives external helper
//! binaries (flac, sox, lame, faad) from `convert.conf`; we use a single `ffmpeg` binary for
//! every conversion and keep LMS's decision rules (prefer native format, respect bitrate and
//! sample-rate limits, passthrough when possible).

pub mod remote;
pub mod shared;
pub mod source;
pub mod spectrum;

use crate::player::song::{Song, TrackInfo};
use crate::player::{Capabilities, Player};
use crate::server::Server;
use bytes::Bytes;
use slimproto::server::{Format, ServerFrame, StrmHeader, TransitionType};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// What the HTTP stream handler should send to the player.
#[derive(Debug, Clone)]
pub enum SourceKind {
    /// Raw bytes of a local file, optionally a byte range.
    File {
        path: PathBuf,
        offset: u64,
        length: Option<u64>,
    },
    /// Output of an ffmpeg pipeline.
    Ffmpeg { args: Vec<String> },
    /// Proxy a remote HTTP(S) stream.
    Remote { url: String },
    /// A Spotify track decoded to PCM by librespot.
    Spotify {
        url: String,
        start_ms: u32,
        bitrate: u32,
        normalise: bool,
    },
}

/// Everything needed to serve one stream generation to one player.
#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub source: SourceKind,
    /// LMS format code of the bytes on the wire (`flc`, `mp3`, `pcm`, …).
    pub format: String,
    /// Song id this stream belongs to (for end-of-stream accounting).
    pub song_id: u64,
    /// The sync group's single read of the source, when several players get this stream.
    pub shared: Option<Arc<shared::SharedSource>>,
}

impl StreamSpec {
    pub fn content_type(&self) -> Option<&'static str> {
        mime_for_format(&self.format)
    }
}

/// A WAV header for a stream of raw little-endian samples (`pcm`), from `wav=<rate>,<bits>,
/// <channels>` (as the `strm` gave them), for a device that can't take samples bare.  Its
/// lengths say "as long as it goes", as streamed WAV does.
pub fn wav_header(spec: &str) -> Option<Vec<u8>> {
    let mut it = spec.split(',').map(|v| v.trim().parse::<u32>().ok());
    let (rate, bits, channels) = (it.next()??, it.next()??, it.next()??);
    if !(8_000..=384_000).contains(&rate)
        || !matches!(bits, 16 | 24 | 32)
        || !(1..=8).contains(&channels)
    {
        return None;
    }
    let align = channels * bits / 8;
    let mut h = Vec::with_capacity(44);
    h.extend(b"RIFF");
    h.extend(u32::MAX.to_le_bytes());
    h.extend(b"WAVEfmt ");
    h.extend(16u32.to_le_bytes());
    h.extend(1u16.to_le_bytes()); // integer PCM
    h.extend((channels as u16).to_le_bytes());
    h.extend(rate.to_le_bytes());
    h.extend((rate * align).to_le_bytes());
    h.extend((align as u16).to_le_bytes());
    h.extend((bits as u16).to_le_bytes());
    h.extend(b"data");
    h.extend((u32::MAX - 36).to_le_bytes());
    Some(h)
}

/// `%Slim::Music::Info::types` (first MIME per type).
pub fn mime_for_format(fmt: &str) -> Option<&'static str> {
    Some(match fmt {
        "mp3" => "audio/mpeg",
        "flc" => "audio/x-flac",
        "ogf" => "audio/ogg;codecs=flac",
        "ogg" => "audio/x-ogg",
        "ops" => "audio/opus",
        "aif" => "audio/x-aiff",
        "wav" => "audio/x-wav",
        "aac" => "audio/aac",
        "mp4" => "audio/m4a",
        "alc" => "audio/x-m4a-lossless",
        "wma" => "audio/x-ms-wma",
        "dsf" => "audio/dsf",
        "dff" => "audio/dff",
        "wvp" => "audio/x-wavpack",
        "mpc" => "audio/x-musepack",
        "ape" => "audio/monkeys-audio",
        _ => return None,
    })
}

/// Map an HTTP content type to an LMS type code (`Slim::Music::Info::mimeToType`).
pub fn type_for_mime(mime: &str) -> Option<&'static str> {
    let m = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match m.as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/x-mpeg" | "audio/mpeg3" | "audio/mpg" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" | "audio/mp4" | "audio/m4a" | "audio/x-m4a" => {
            "aac"
        }
        "audio/ogg" | "application/ogg" | "audio/x-ogg" | "audio/vorbis" => "ogg",
        "audio/opus" => "ops",
        "audio/flac" | "audio/x-flac" => "flc",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/aiff" | "audio/x-aiff" => "aif",
        "audio/x-ms-wma" | "audio/wma" => "wma",
        "audio/x-scpls" | "audio/scpls" => "pls",
        "audio/x-mpegurl"
        | "audio/mpegurl"
        | "application/x-mpegurl"
        | "application/vnd.apple.mpegurl" => "m3u",
        "video/x-ms-asf" | "video/x-ms-asx" | "audio/x-ms-asx" => "asx",
        "application/xspf+xml" => "xspf",
        "text/html" => "html",
        _ => return None,
    })
}

pub fn is_lossless(fmt: &str) -> bool {
    matches!(
        fmt,
        "flc" | "ogf" | "wav" | "alc" | "aif" | "dff" | "dsf" | "wvp" | "ape" | "pcm"
    )
}

/// Formats the player can play natively for a given source type, with LMS's passthrough table.
fn passthrough_target(src: &str, supported: &[String]) -> Option<String> {
    let has = |f: &str| supported.iter().any(|s| s == f);
    let cands: &[&str] = match src {
        "mp3" => &["mp3"],
        "flc" => &["flc"],
        "ogg" => &["ogg"],
        "ops" => &["ops"],
        "ogf" => &["ogf"],
        "aac" => &["aac"],
        "mp4" => &["aac"],
        "alc" => &["alc"],
        "wma" | "wmal" | "wmap" => &["wma"],
        "wav" => &["pcm", "wav"],
        "aif" => &["pcm", "aif"],
        "dsf" => &["dsd", "dsf"],
        "dff" => &["dsd", "dff"],
        _ => &[],
    };
    cands.iter().find(|c| has(c)).map(|c| {
        // squeezelite advertises "dsd" for both DSD containers
        if *c == "dsd" {
            src.to_string()
        } else {
            c.to_string()
        }
    })
}

/// Formats supported by every active player of the group, in the first player's order.
pub fn supported_formats(players: &[Arc<Player>]) -> Vec<String> {
    let mut iter = players.iter();
    let Some(first) = iter.next() else {
        return vec!["mp3".into()];
    };
    let mut formats = first.state.lock().caps.formats.clone();
    for p in iter {
        let caps = p.state.lock().caps.formats.clone();
        formats.retain(|f| caps.contains(f));
    }
    if formats.is_empty() {
        let each: Vec<String> = players
            .iter()
            .map(|p| format!("{}: {}", p.id, p.state.lock().caps.formats.join(",")))
            .collect();
        warn!(
            "no format every player in the group takes ({}): trying mp3",
            each.join("; ")
        );
        formats.push("mp3".into());
    }
    formats
}

/// Minimum non-zero `maxBitrate` (kbps) over the group; 0 = unlimited.
fn rate_limit(players: &[Arc<Player>]) -> u32 {
    players
        .iter()
        .map(|p| p.prefs.get_i64("maxBitrate").max(0) as u32)
        .filter(|r| *r > 0)
        .min()
        .unwrap_or(0)
}

/// `CapabilitiesHelper::samplerateLimit`.
fn samplerate_limit(players: &[Arc<Player>], track_rate: Option<u32>) -> Option<u32> {
    let rate = track_rate?;
    let limit = players
        .iter()
        .map(|p| p.state.lock().caps.max_sample_rate)
        .filter(|r| *r > 0)
        .min()?;
    if limit < rate {
        if limit % 12000 == 0 && rate % 11025 == 0 {
            return Some(limit / 12000 * 11025);
        }
        return Some(limit);
    }
    None
}

/// Decision for one song.
#[derive(Debug, Clone)]
pub struct OpenedSong {
    pub stream_format: String,
    pub spec: Option<StreamSpec>,
    /// Remote URL the player fetches itself.
    pub direct_url: Option<String>,
    pub stream_bitrate: Option<u32>,
    pub can_seek: bool,
}

/// `Song::open($seekdata)`: decide format and source.  `seek` is the time offset in seconds.
pub fn open_song(
    server: &Arc<Server>,
    song: &mut Song,
    players: &[Arc<Player>],
    seek: Option<f64>,
) -> Result<OpenedSong, String> {
    let supported = supported_formats(players);
    let track = song.track.clone();
    let start = seek.unwrap_or(0.0).max(0.0);
    song.start_offset = start;
    let rate = rate_limit(players);
    let sr_limit = samplerate_limit(players, track.samplerate);
    let all_https = players.iter().all(|p| p.state.lock().caps.can_https);

    if crate::plugins::spotify::is_spotify(&track.url) {
        use crate::plugins::spotify::stream::{CHANNELS, SAMPLE_RATE, SAMPLE_SIZE};
        // FLAC when every player of the group has it: lossless at about half the bytes
        let flac = supported.iter().any(|f| f == "flc");
        let format = if flac { "flc" } else { "pcm" };
        let pcm_rate = SAMPLE_RATE * SAMPLE_SIZE * CHANNELS;
        info!(
            "{}: streaming from Spotify as {} (start {:.1}s)",
            track.url, format, start
        );
        return Ok(OpenedSong {
            stream_format: format.into(),
            spec: Some(StreamSpec {
                source: SourceKind::Spotify {
                    url: track.url.clone(),
                    start_ms: (start * 1000.0) as u32,
                    bitrate: server.prefs.server.get_i64("spotifyBitrate").max(96) as u32,
                    normalise: server.prefs.server.get_bool("spotifyNormalisation"),
                },
                format: format.into(),
                song_id: song.id,
                shared: None,
            }),
            direct_url: None,
            // typical FLAC compression of music
            stream_bitrate: Some(if flac { pcm_rate / 10 * 6 } else { pcm_rate }),
            // a Connect stream is positioned from the Spotify app
            can_seek: !track.url.starts_with("spotify://connect:"),
        });
    }
    if track.remote {
        return open_remote(server, song, &supported, rate, all_https);
    }

    let path =
        crate::util::path_from_file_url(&track.url).ok_or_else(|| "PROBLEM_OPENING".to_string())?;
    if !path.exists() {
        return Err("PROBLEM_OPENING".into());
    }
    let src = track.content_type.as_str();
    let cue = track.cue_start.is_some() || track.cue_end.is_some();
    let abs_start = track.cue_start.unwrap_or(0.0) + start;
    let abs_end = track.cue_end;

    let mut native = passthrough_target(src, &supported);
    // a player that asks for Opus first (a browser on a slow network) gets it for everything
    if prefers_opus(&supported) && src != "ops" {
        native = None;
    }
    // WiiM garbles 32-bit PCM; send it as 24-bit FLAC instead.
    if track.samplesize == Some(32) && players.iter().any(|p| p.state.lock().caps.is_wiim()) {
        native = None;
    }
    let bitrate_ok = rate == 0
        || !matches!(src, "mp3" | "aac" | "ogg" | "ops" | "wma")
        || track
            .bitrate
            .map(|b| (rate as f64) * 1.01 >= b as f64 / 1000.0)
            .unwrap_or(true);
    let lossless_too_big = rate > 0 && is_lossless(src);

    // Passthrough: native format, no limits, and either no seek or a format we can byte-seek.
    if let Some(fmt) = native.clone() {
        if bitrate_ok && !lossless_too_big && sr_limit.is_none() && !cue {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let offset = track.audio_offset.unwrap_or(0);
            let audio_size = track.audio_size.unwrap_or(size.saturating_sub(offset));
            let strip_header = matches!(src, "wav" | "aif") && fmt == "pcm";
            if start == 0.0 {
                let (off, len) = if strip_header {
                    (offset, Some(audio_size))
                } else {
                    (0, None)
                };
                info!("{}: streaming {} natively as {}", track.url, src, fmt);
                return Ok(OpenedSong {
                    stream_format: fmt.clone(),
                    spec: Some(StreamSpec {
                        source: SourceKind::File {
                            path,
                            offset: off,
                            length: len,
                        },
                        format: fmt,
                        song_id: song.id,
                        shared: None,
                    }),
                    direct_url: None,
                    stream_bitrate: track.bitrate,
                    can_seek: true,
                });
            }
            // Byte seek for constant-structure formats: MP3 resyncs on frame headers, PCM on blocks.
            if (src == "mp3" || strip_header) && track.secs.unwrap_or(0.0) > 0.0 {
                let secs = track.secs.unwrap_or(1.0);
                let mut byte = (audio_size as f64 / secs * start) as u64;
                if strip_header {
                    let align = (track.channels.unwrap_or(2) * track.samplesize.unwrap_or(16) / 8)
                        .max(1) as u64;
                    byte -= byte % align;
                }
                let off = offset + byte;
                let len = if strip_header {
                    Some(audio_size.saturating_sub(byte))
                } else {
                    None
                };
                return Ok(OpenedSong {
                    stream_format: fmt.clone(),
                    spec: Some(StreamSpec {
                        source: SourceKind::File {
                            path,
                            offset: off,
                            length: len,
                        },
                        format: fmt,
                        song_id: song.id,
                        shared: None,
                    }),
                    direct_url: None,
                    stream_bitrate: track.bitrate,
                    can_seek: true,
                });
            }
        }
    }

    // Transcode with ffmpeg.
    if !server.has_ffmpeg() {
        if let Some(fmt) = native {
            // No transcoder: best effort raw passthrough, ignoring seek/limits.
            return Ok(OpenedSong {
                stream_format: fmt.clone(),
                spec: Some(StreamSpec {
                    source: SourceKind::File {
                        path,
                        offset: 0,
                        length: None,
                    },
                    format: fmt,
                    song_id: song.id,
                    shared: None,
                }),
                direct_url: None,
                stream_bitrate: track.bitrate,
                can_seek: false,
            });
        }
        return Err("PROBLEM_CONVERT_FILE".into());
    }
    let target = choose_transcode_target(src, &supported, rate)?;
    let args = ffmpeg_args(
        &path.to_string_lossy(),
        &target,
        abs_start,
        abs_end,
        rate,
        sr_limit,
        &track,
    );
    debug!("ffmpeg {:?}", args);
    info!(
        "{}: transcoding {} -> {} (start {:.1}s)",
        track.url, src, target, start
    );
    let stream_bitrate = match target.as_str() {
        "mp3" => Some(if rate > 0 { rate } else { 320 } * 1000),
        "pcm" => Some(
            sr_limit.or(track.samplerate).unwrap_or(44100) * track.samplesize.unwrap_or(16) * 2,
        ),
        _ => track.bitrate,
    };
    Ok(OpenedSong {
        stream_format: target.clone(),
        spec: Some(StreamSpec {
            source: SourceKind::Ffmpeg { args },
            format: target,
            song_id: song.id,
            shared: None,
        }),
        direct_url: None,
        stream_bitrate,
        can_seek: true,
    })
}

/// The group's formats put Opus first: a browser that saves bandwidth.  (Other players list
/// it after their lossless formats, and get those.)
fn prefers_opus(supported: &[String]) -> bool {
    supported.first().is_some_and(|f| f == "ops")
}

fn choose_transcode_target(src: &str, supported: &[String], rate: u32) -> Result<String, String> {
    let has = |f: &str| supported.iter().any(|s| s == f);
    if prefers_opus(supported) {
        return Ok("ops".into());
    }
    let order: &[&str] = if rate > 0 {
        &["mp3", "flc", "pcm", "ops"]
    } else if is_lossless(src) {
        &["flc", "pcm", "mp3", "ops"]
    } else {
        // LMS prefers the player's native ordering for lossy sources; a lossless container
        // avoids generation loss when the player supports it.
        &["flc", "mp3", "pcm", "ops"]
    };
    order
        .iter()
        .find(|f| has(f))
        .map(|f| f.to_string())
        .ok_or_else(|| "PROBLEM_CONVERT_FILE".to_string())
}

/// Build ffmpeg arguments producing `target` on stdout.
pub fn ffmpeg_args(
    input: &str,
    target: &str,
    start: f64,
    end: Option<f64>,
    rate_kbps: u32,
    sr_limit: Option<u32>,
    track: &TrackInfo,
) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let remote = input.contains("://");
    if remote {
        a.extend(
            [
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_delay_max",
                "5",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
    }
    if start > 0.0 {
        a.push("-ss".into());
        a.push(format!("{:.3}", start));
    }
    a.push("-i".into());
    a.push(input.to_string());
    if let Some(e) = end {
        let dur = (e - start).max(0.0);
        a.push("-t".into());
        a.push(format!("{:.3}", dur));
    }
    a.extend(
        ["-map", "0:a:0", "-vn", "-sn", "-dn"]
            .iter()
            .map(|s| s.to_string()),
    );
    if let Some(sr) = sr_limit {
        a.push("-ar".into());
        a.push(sr.to_string());
    }
    match target {
        "flc" => {
            a.extend(
                ["-c:a", "flac", "-compression_level", "0", "-f", "flac"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            if track.samplesize.unwrap_or(16) > 16 {
                a.extend(
                    ["-sample_fmt", "s32", "-bits_per_raw_sample", "24"]
                        .iter()
                        .map(|s| s.to_string()),
                );
            } else {
                a.extend(["-sample_fmt", "s16"].iter().map(|s| s.to_string()));
            }
        }
        "pcm" => {
            if track.samplesize.unwrap_or(16) > 16 {
                a.extend(
                    ["-c:a", "pcm_s24le", "-f", "s24le"]
                        .iter()
                        .map(|s| s.to_string()),
                );
            } else {
                a.extend(
                    ["-c:a", "pcm_s16le", "-f", "s16le"]
                        .iter()
                        .map(|s| s.to_string()),
                );
            }
            a.extend(["-ac", "2"].iter().map(|s| s.to_string()));
        }
        "ops" => a.extend(
            // (Opus runs at 48 kHz)
            [
                "-c:a", "libopus", "-b:a", "160k", "-ar", "48000", "-f", "ogg",
            ]
            .iter()
            .map(|s| s.to_string()),
        ),
        "aac" => a.extend(
            ["-c:a", "aac", "-b:a", "256k", "-f", "adts"]
                .iter()
                .map(|s| s.to_string()),
        ),
        _ => {
            let kbps = if rate_kbps > 0 { rate_kbps } else { 320 };
            a.extend(["-c:a", "libmp3lame", "-b:a"].iter().map(|s| s.to_string()));
            a.push(format!("{}k", kbps));
            a.extend(["-f", "mp3"].iter().map(|s| s.to_string()));
            if sr_limit.is_none() && track.samplerate.unwrap_or(44100) > 48000 {
                a.push("-ar".into());
                a.push("48000".into());
            }
        }
    }
    a.push("pipe:1".into());
    a
}

fn open_remote(
    server: &Arc<Server>,
    song: &mut Song,
    supported: &[String],
    rate: u32,
    can_https: bool,
) -> Result<OpenedSong, String> {
    let url = song
        .stream_url
        .clone()
        .unwrap_or_else(|| song.track.url.clone());
    let ct = if song.track.content_type.is_empty() {
        "mp3".to_string()
    } else {
        song.track.content_type.clone()
    };
    let https = url.starts_with("https://");
    let native = passthrough_target(&ct, supported);
    let hls = song.remote_meta.content_type.as_deref() == Some("hls")
        || ct == "m3u"
        || url.contains(".m3u8");
    if let Some(fmt) = native.clone().filter(|_| !hls) {
        // through the server unless asked: one connection per sync group, and spectrum bars
        if server.prefs.server.get_bool("radioDirect")
            && (!https || can_https)
            && (rate == 0 || song.track.bitrate.map(|b| rate * 1000 >= b).unwrap_or(true))
        {
            return Ok(OpenedSong {
                stream_format: fmt,
                spec: None,
                direct_url: Some(url),
                stream_bitrate: song.track.bitrate,
                can_seek: false,
            });
        }
        if !hls {
            // Proxy the bytes unchanged.
            return Ok(OpenedSong {
                stream_format: fmt.clone(),
                spec: Some(StreamSpec {
                    source: SourceKind::Remote { url },
                    format: fmt,
                    song_id: song.id,
                    shared: None,
                }),
                direct_url: None,
                stream_bitrate: song.track.bitrate,
                can_seek: false,
            });
        }
    }
    if !server.has_ffmpeg() {
        return Err("PROBLEM_CONVERT_STREAM".into());
    }
    let target = if supported.iter().any(|f| f == "aac") && hls && rate == 0 {
        "aac".to_string()
    } else {
        choose_transcode_target(&ct, supported, rate)?
    };
    let args = ffmpeg_args(&url, &target, 0.0, None, rate, None, &song.track);
    Ok(OpenedSong {
        stream_format: target.clone(),
        spec: Some(StreamSpec {
            source: SourceKind::Ffmpeg { args },
            format: target,
            song_id: song.id,
            shared: None,
        }),
        direct_url: None,
        stream_bitrate: song.track.bitrate,
        can_seek: false,
    })
}

/// Parameters of one `strm s` (`Squeezebox::stream_s` `$params`).
pub struct PlayParams {
    pub paused: bool,
    pub reconnect: bool,
    pub fade_in: f64,
    pub replay_gain_db: Option<f64>,
    pub buffer_threshold: Option<u8>,
    pub transition: TransitionType,
    pub transition_duration: u8,
    /// One read of the source for the whole sync group.
    pub shared: Option<Arc<shared::SharedSource>>,
}

/// Build and send the `strm s` frame for `song` to `player`.  Returns false if the frame
/// could not be built (e.g. direct stream host unresolvable).
pub fn send_stream_start(
    server: &Arc<Server>,
    player: &Arc<Player>,
    song: &Song,
    p: &PlayParams,
) -> bool {
    let fmt = Format::from_content_type(&song.stream_format);
    let mut h = StrmHeader::start(fmt, !p.paused);
    let track = &song.track;
    match song.stream_format.as_str() {
        "pcm" | "wav" | "aif" => {
            let ffmpeg = match song.spec.as_ref().map(|s| &s.source) {
                Some(SourceKind::Ffmpeg { args }) => Some(args),
                _ => None,
            };
            let transcoded = ffmpeg.is_some();
            // resampled for a player that can't take the track's rate
            let rate = ffmpeg
                .and_then(|a| a.windows(2).find(|w| w[0] == "-ar"))
                .and_then(|w| w[1].parse().ok())
                .or(track.samplerate)
                .unwrap_or(44100);
            let bits = if transcoded {
                if track.samplesize.unwrap_or(16) > 16 {
                    24
                } else {
                    16
                }
            } else {
                track.samplesize.unwrap_or(16)
            };
            let little = transcoded || track.little_endian;
            let chans = if transcoded {
                2
            } else {
                track.channels.unwrap_or(2)
            };
            h = h.with_pcm(bits, rate, chans, little);
        }
        "flc" => {
            if track.samplerate.unwrap_or(0) >= 88200 {
                h.output_threshold = 20;
            }
        }
        "aac" => {
            // '5' = mp4ff container, '2' = ADTS
            let container = song.direct_url.is_none()
                && matches!(track.content_type.as_str(), "mp4" | "alc")
                && !matches!(
                    song.spec.as_ref().map(|s| &s.source),
                    Some(SourceKind::Ffmpeg { .. })
                );
            h.pcm_sample_size = if container { b'5' } else { b'2' };
        }
        "dsf" => h.pcm_sample_size = b'0',
        "dff" => h.pcm_sample_size = b'1',
        _ => {}
    }

    let mut threshold = p
        .buffer_threshold
        .unwrap_or_else(|| player.prefs.get_i64("bufferThreshold").clamp(1, 255) as u8);
    if let Some(size) = track.filesize {
        if size < threshold as u64 * 1024 {
            threshold = ((size / 1024).max(2) - 1) as u8;
        }
    }
    h.buffer_threshold = threshold;

    let mut flags: u8 = 0;
    if p.reconnect {
        flags |= 0x40;
    }
    flags |= (player.prefs.get_i64("polarityInversion") & 0x03) as u8;
    let out_ch = player.prefs.get_i64("outputChannels") as u8 & 0x03;
    if out_ch != 0 {
        flags |= out_ch << 2;
    }

    if p.fade_in > 0.0 {
        h.transition_type = TransitionType::FadeIn;
        h.transition_duration = p.fade_in.min(10.0) as u8;
    } else {
        h.transition_type = p.transition;
        h.transition_duration = p
            .transition_duration
            .min(player.state.lock().caps.max_transition_duration);
    }
    if let Some(db) = p.replay_gain_db {
        h.replay_gain = slimproto::server::AudioGain::db_to_fixed(db);
    }

    let request: Bytes;
    if let Some(url) = &song.direct_url {
        let Ok(u) = url::Url::parse(url) else {
            return false;
        };
        let host = u.host_str().unwrap_or("").to_string();
        let port = u.port_or_known_default().unwrap_or(80);
        let ip = match (host.as_str(), port).to_socket_addrs_v4() {
            Some(ip) => ip,
            None => return false,
        };
        h.server_ip = ip;
        h.server_port = port;
        h.autostart += 2;
        if u.scheme() == "https" {
            flags |= 0x20;
        }
        request = Bytes::from(remote::direct_request_string(&u));
    } else {
        let mut r = format!("GET /stream.mp3?player={} HTTP/1.0\r\n", player.id);
        if server.prefs.server.get_bool("authorize") {
            // Players authenticate with a per-stream random password, as in LMS.
            let pw: String = {
                use rand::Rng;
                let mut rng = rand::rng();
                (0..10)
                    .map(|_| rng.sample(rand::distr::Alphanumeric) as char)
                    .collect()
            };
            player.set_stream_password(Some(pw.clone()));
            use base64::Engine;
            r.push_str(&format!(
                "Authorization: Basic {}\r\n",
                base64::engine::general_purpose::STANDARD.encode(format!("squeezeboxXXX:{}", pw))
            ));
        }
        r.push_str("\r\n");
        request = Bytes::from(r);
        h.server_port = server.http_port();
        h.server_ip = 0;
    }
    h.flags = flags;

    // reset buffer fullness before a new stream (bug 8861 in LMS)
    {
        let mut s = player.state.lock();
        s.buffer_fullness = 0;
        s.stream_start_ts = None;
        s.ready_to_stream = false;
        s.buffer_ready = false;
    }
    let gen = if player.is_bridged() {
        player.next_stream_generation()
    } else {
        player.new_stream_generation()
    };
    if let Some(spec) = &song.spec {
        let mut spec = spec.clone();
        spec.shared = p.shared.clone();
        player.set_pending_stream(gen, spec);
    } else {
        player.clear_pending_stream();
    }
    info!(
        "{}: strm s format={} autostart={} threshold={} direct={}",
        player.id,
        song.stream_format,
        h.autostart as char,
        h.buffer_threshold,
        song.direct_url.as_deref().unwrap_or("-")
    );
    player.send(ServerFrame::Strm { header: h, request });
    true
}

trait ToV4 {
    fn to_socket_addrs_v4(&self) -> Option<u32>;
}

impl ToV4 for (&str, u16) {
    fn to_socket_addrs_v4(&self) -> Option<u32> {
        use std::net::ToSocketAddrs;
        let addrs = (self.0, self.1).to_socket_addrs().ok()?;
        for a in addrs {
            if let std::net::SocketAddr::V4(v4) = a {
                return Some(u32::from(*v4.ip()));
            }
        }
        None
    }
}

#[allow(unused)]
pub fn capabilities_summary(c: &Capabilities) -> String {
    format!(
        "{} ({}), formats {:?}, max {} Hz",
        c.model_name, c.model, c.formats, c.max_sample_rate
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_samples_get_a_streaming_wav_header() {
        let h = wav_header("48000,24,2").unwrap();
        assert_eq!(h.len(), 44);
        assert_eq!(&h[..4], b"RIFF");
        assert_eq!(&h[8..16], b"WAVEfmt ");
        let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
        assert_eq!(u16_at(22), 2); // channels
        assert_eq!(u32_at(24), 48_000);
        assert_eq!(u32_at(28), 48_000 * 6); // bytes a second
        assert_eq!(u16_at(32), 6); // a frame
        assert_eq!(u16_at(34), 24);
        assert_eq!(&h[36..40], b"data");
        for bad in ["", "44100", "44100,12,2", "44100,16,0", "x,16,2", "1,16,2"] {
            assert!(wav_header(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn passthrough_rules() {
        let sq: Vec<String> = ["flc", "pcm", "mp3", "ogg", "aac", "alc", "ops", "dsd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(passthrough_target("flc", &sq).as_deref(), Some("flc"));
        assert_eq!(passthrough_target("wav", &sq).as_deref(), Some("pcm"));
        assert_eq!(passthrough_target("mp4", &sq).as_deref(), Some("aac"));
        assert_eq!(passthrough_target("dsf", &sq).as_deref(), Some("dsf"));
        assert_eq!(passthrough_target("ape", &sq), None);
        assert_eq!(choose_transcode_target("ape", &sq, 0).unwrap(), "flc");
        assert_eq!(choose_transcode_target("flc", &sq, 192).unwrap(), "mp3");
        let mp3only = vec!["mp3".to_string()];
        assert_eq!(choose_transcode_target("flc", &mp3only, 0).unwrap(), "mp3");
        // a browser saving bandwidth asks for Opus first, and gets it for everything
        let opus: Vec<String> = ["ops", "flc", "pcm"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(choose_transcode_target("flc", &opus, 0).unwrap(), "ops");
        assert_eq!(choose_transcode_target("mp3", &opus, 0).unwrap(), "ops");
        // ...but a Squeezebox synced with it doesn't play Opus: FLAC for both
        let both: Vec<String> = ["flc", "pcm"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose_transcode_target("flc", &both, 0).unwrap(), "flc");
    }

    #[test]
    fn mime_mapping() {
        assert_eq!(type_for_mime("audio/aacp"), Some("aac"));
        assert_eq!(type_for_mime("audio/x-scpls; charset=utf-8"), Some("pls"));
        assert_eq!(mime_for_format("flc"), Some("audio/x-flac"));
    }
}
