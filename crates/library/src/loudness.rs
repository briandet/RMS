//! Loudness (EBU R128, as ReplayGain 2.0 uses it) for files without ReplayGain tags.  Not part
//! of LMS, which only reads tags.
//!
//! The background analysis ([`crate::sonic`]) measures each track's integrated loudness and
//! true peak and keeps them in `track_analysis`.  [`Library::apply_loudness`] then fills in
//! `tracks.replay_gain` / `replay_peak` where the tags left them empty, and an album's gain once
//! all of its tracks are measured, so playback, song info and the UIs use them like tag values.
//! Tags always win; the files are never written.

use crate::Library;
use ebur128::{Channel, EbuR128, Mode};
use rusqlite::params;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::TrackType;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::default::{get_codecs, get_probe};
use tracing::info;

/// ReplayGain 2.0 reference level: gain = REFERENCE - loudness.
pub const REFERENCE_LUFS: f64 = -18.0;
/// R128's absolute gate: anything quieter counts as silence.
const SILENCE_LUFS: f64 = -70.0;

/// A track's integrated loudness and true peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    pub lufs: f64,
    /// linear, like `REPLAYGAIN_TRACK_PEAK` (can exceed 1)
    pub peak: f64,
}

impl Loudness {
    fn new(lufs: f64, peak: f64) -> Result<Loudness, String> {
        if !lufs.is_finite() || lufs <= SILENCE_LUFS {
            return Err("silent".into());
        }
        Ok(Loudness {
            lufs,
            peak: if peak.is_finite() { peak.max(0.0) } else { 0.0 },
        })
    }

    pub fn gain(&self) -> f64 {
        REFERENCE_LUFS - self.lufs
    }
}

/// Measure a file, decoding it with symphonia as it goes (constant memory, any length).
pub fn measure(path: &Path) -> Result<Loudness, String> {
    // symphonia can panic on malformed files
    std::panic::catch_unwind(|| measure_symphonia(path))
        .map_err(|_| "decoder panicked".to_string())?
}

fn measure_symphonia(path: &Path) -> Result<Loudness, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = get_probe()
        .probe(&hint, mss, Default::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("not an audio track")?
        .clone();
    let mut decoder = get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut meter: Option<EbuR128> = None;
    let mut buf: Vec<f32> = Vec::new();
    let mut frames = 0usize;
    let mut decode_errors = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // some formats end with an I/O error rather than a clean end of stream
            Err(_) if frames > 0 => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) if decode_errors < 3 => {
                decode_errors += 1;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if decoded.frames() == 0 {
            continue;
        }
        let channels = decoded.spec().channels().count() as u32;
        let rate = decoded.spec().rate();
        let m = match &mut meter {
            Some(m) if m.channels() == channels && m.rate() == rate => m,
            Some(m) => {
                m.change_parameters(channels, rate)
                    .map_err(|e| format!("{e:?}"))?;
                set_channel_map(m, channels)?;
                m
            }
            None => {
                let mut m =
                    EbuR128::new(channels, rate, Mode::I | Mode::TRUE_PEAK | Mode::HISTOGRAM)
                        .map_err(|e| format!("{e:?}"))?;
                set_channel_map(&mut m, channels)?;
                meter.insert(m)
            }
        };
        buf.resize(decoded.samples_interleaved(), 0.0);
        decoded.copy_to_slice_interleaved(&mut buf);
        m.add_frames_f32(&buf).map_err(|e| format!("{e:?}"))?;
        frames += decoded.frames();
    }
    let m = meter.ok_or("no audio")?;
    let lufs = m.loudness_global().map_err(|e| format!("{e:?}"))?;
    let peak = (0..m.channels())
        .filter_map(|c| m.true_peak(c).ok())
        .fold(0.0, f64::max);
    Loudness::new(lufs, peak)
}

/// Mono counts twice, as if played on both speakers (ReplayGain 2.0); the defaults (L R C LFE Ls
/// Rs) suit the rest.
fn set_channel_map(m: &mut EbuR128, channels: u32) -> Result<(), String> {
    if channels == 1 {
        m.set_channel(0, Channel::DualMono)
            .map_err(|e| format!("{e:?}"))?;
    }
    Ok(())
}

/// Measure with ffmpeg's `ebur128` filter: formats symphonia lacks, and cue-sheet tracks
/// (`slice` = start and length in seconds).
pub fn measure_ffmpeg(
    ffmpeg: &str,
    path: &Path,
    slice: Option<(f64, f64)>,
) -> Result<Loudness, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-nostats"]);
    if let Some((start, len)) = slice {
        cmd.args(["-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}")]);
    }
    cmd.arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-af",
            "ebur128=peak=true:framelog=verbose:dualmono=true",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let out = cmd.output().map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg failed: {}", out.status));
    }
    parse_ebur128_summary(&String::from_utf8_lossy(&out.stderr))
        .ok_or("no ffmpeg summary".into())
        .and_then(|(lufs, peak_db)| Loudness::new(lufs, 10f64.powf(peak_db / 20.0)))
}

/// `I:` (LUFS) and the true `Peak:` (dBFS) from the filter's summary.
fn parse_ebur128_summary(log: &str) -> Option<(f64, f64)> {
    let summary = &log[log.rfind("Summary:")?..];
    let value = |key: &str| {
        summary
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok())
    };
    Some((value("I:")?, value("Peak:")?))
}

impl Library {
    /// Fill in ReplayGain the tags left empty from the measured loudness: tracks, then albums
    /// whose tracks are all measured.  Returns (tracks, albums) updated.
    pub fn apply_loudness(&self) -> (usize, usize) {
        let Ok(c) = self.conn() else { return (0, 0) };
        let version = crate::sonic::version();
        const MEASURED: &str = "FROM track_analysis a \
             WHERE a.fingerprint = tracks.fingerprint AND a.version = ? AND a.loudness IS NOT NULL";
        let tracks = c
            .execute(
                &format!(
                    "UPDATE tracks SET replay_gain = ROUND(? - a.loudness, 2) {MEASURED} \
                       AND tracks.replay_gain IS NULL"
                ),
                params![REFERENCE_LUFS, version],
            )
            .unwrap_or(0);
        // the peak also where only the gain is tagged: it keeps the gain from clipping
        let _ = c.execute(
            &format!(
                "UPDATE tracks SET replay_peak = ROUND(a.peak, 6) {MEASURED} \
                   AND tracks.replay_peak IS NULL"
            ),
            [version],
        );

        // albums without a gain: (seconds, measurement) of each track
        let mut albums: BTreeMap<i64, Vec<(f64, Option<Loudness>)>> = BTreeMap::new();
        if let Ok(mut st) = c.prepare(
            "SELECT t.album_id, t.duration, a.loudness, a.peak FROM tracks t JOIN albums al ON al.id = t.album_id \
             LEFT JOIN track_analysis a ON a.fingerprint = t.fingerprint AND a.version = ? \
             WHERE al.replay_gain IS NULL",
        ) {
            let rows = st.query_map([version], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                ))
            });
            for (album, secs, lufs, peak) in rows.into_iter().flatten().flatten() {
                let m = lufs.map(|lufs| Loudness {
                    lufs,
                    peak: peak.unwrap_or(0.0),
                });
                albums.entry(album).or_default().push((secs, m));
            }
        }
        let mut updated = 0;
        for (album, tracks) in albums {
            let Some(l) = album_loudness(&tracks) else {
                continue;
            };
            updated += c
                .execute(
                    "UPDATE albums SET replay_gain = ROUND(?, 2), \
                       replay_peak = COALESCE(replay_peak, ROUND(?, 6)) WHERE id = ?",
                    params![l.gain(), l.peak, album],
                )
                .unwrap_or(0);
        }
        if tracks > 0 || updated > 0 {
            info!(
                "loudness: ReplayGain filled in for {} tracks, {} albums",
                tracks, updated
            );
        }
        (tracks, updated)
    }
}

/// An album's loudness from its tracks' (power average weighted by length; exact R128 would
/// gate the album's audio as a whole, usually within a few tenths of a dB of this) and the
/// loudest peak.  `None` unless every track is measured.
fn album_loudness(tracks: &[(f64, Option<Loudness>)]) -> Option<Loudness> {
    let mut energy = 0.0;
    let mut secs = 0.0;
    let mut peak: f64 = 0.0;
    for (s, m) in tracks {
        let m = m.as_ref()?;
        let s = s.max(1.0);
        energy += s * 10f64.powf(m.lufs / 10.0);
        secs += s;
        peak = peak.max(m.peak);
    }
    (secs > 0.0).then(|| Loudness {
        lufs: 10.0 * (energy / secs).log10(),
        peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_ffmpeg_summary() {
        let log = "[Parsed_ebur128_0 @ 0x1] t: 1 M: -20 S: -20 I: -21.0 LUFS\n\
            [Parsed_ebur128_0 @ 0x1] Summary:\n\n  Integrated loudness:\n    I:         -16.6 LUFS\n\
            \x20   Threshold: -26.8 LUFS\n\n  True peak:\n    Peak:        0.4 dBFS\n";
        assert_eq!(parse_ebur128_summary(log), Some((-16.6, 0.4)));
        assert_eq!(parse_ebur128_summary("nothing"), None);
    }

    #[test]
    fn album_loudness_is_a_power_average() {
        let m = |lufs, peak| Some(Loudness { lufs, peak });
        let a = album_loudness(&[(100.0, m(-10.0, 0.9)), (100.0, m(-10.0, 1.1))]).unwrap();
        assert!((a.lufs + 10.0).abs() < 1e-9);
        assert_eq!(a.peak, 1.1);
        // a quiet track pulls the album down by less than the plain average would
        let a = album_loudness(&[(100.0, m(-10.0, 1.0)), (100.0, m(-20.0, 1.0))]).unwrap();
        assert!(a.lufs > -15.0 && a.lufs < -12.0, "{}", a.lufs);
        assert!(album_loudness(&[(100.0, m(-10.0, 1.0)), (100.0, None)]).is_none());
    }

    #[test]
    fn silence_is_not_a_measurement() {
        assert!(Loudness::new(f64::NEG_INFINITY, 0.0).is_err());
        assert!(Loudness::new(-75.0, 0.0).is_err());
        assert_eq!(Loudness::new(-14.0, 0.5).unwrap().gain(), -4.0);
    }
}
