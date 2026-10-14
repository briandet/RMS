//! Spectrum bars: how loud four bands of a track (bass, low mids, high mids, treble) are, 20
//! times a second, for the little analyser the web UI shows on a playing player.  Not part of
//! LMS.
//!
//! A curve is worked out ahead of time and played back in step with the player's position,
//! so it needs no audio from the player:
//!
//! * Library tracks get theirs from the background analysis ([`crate::sonic`], from the audio
//!   it decodes anyway), kept in `track_spectrum` by audio fingerprint, or the first time they
//!   play ([`Library::track_spectrum`]).
//! * Streams (radio, Spotify) are analysed as they play ([`Live`]).
//!
//! Each band is scaled to its own range (the track's quiet to loud for that band), so all four
//! bars move whatever the mix; levels are 0–15, two bands to a byte.

use crate::sonic::SAMPLE_RATE;
use crate::Library;
use rusqlite::{params, Connection, OptionalExtension};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;

/// Stored with each curve; others are redone.
pub const VERSION: i64 = 1;
/// Readings per second.
pub const FPS: u32 = 20;
/// Bands, low to high (Hz).  The top one ends below the analysis rate's Nyquist frequency.
pub const BANDS: [(f32, f32); 4] = [
    (30.0, 150.0),
    (150.0, 600.0),
    (600.0, 2500.0),
    (2500.0, 11000.0),
];
/// Bytes per reading: four 4-bit levels.
pub const FRAME_BYTES: usize = 2;
/// Highest level.
const TOP: f32 = 15.0;
/// Analysis window: ~93 ms at 22050 Hz, enough resolution for the bass band.
const WINDOW: usize = 2048;
/// Quieter than this (dB, before the tilt) is silence: level 0, and left out of the range.
const SILENCE_DB: f32 = -90.0;
/// A band's range spans at least this many dB, so a steady band doesn't flicker full height.
const MIN_SPAN_DB: f32 = 10.0;
/// The range runs from this percentile of a band's readings to [`HIGH`].
const LOW: f64 = 0.05;
const HIGH: f64 = 0.98;
/// A live stream's range comes from this many seconds of recent readings.
const LIVE_WINDOW_SECS: usize = 30;

/// Band loudness of short stretches of mono audio.
pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// FFT bins of each band
    bins: Vec<std::ops::Range<usize>>,
    /// treble lift per band (dB), so a typical mix's bands sit at similar levels
    tilt: Vec<f32>,
    /// power of a full-scale sine, to put readings in dBFS
    reference: f32,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    rate: u32,
}

impl Analyzer {
    pub fn new(rate: u32) -> Analyzer {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let window: Vec<f32> = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos())
            .collect();
        let hz = rate as f32 / WINDOW as f32;
        let nyquist = WINDOW / 2;
        let bins = BANDS
            .iter()
            .map(|(lo, hi)| {
                let a = ((lo / hz).ceil() as usize).clamp(1, nyquist);
                let b = ((hi / hz).ceil() as usize).clamp(a + 1, nyquist + 1);
                a..b
            })
            .collect();
        // +3 dB per octave above 100 Hz (music falls off about that fast)
        let tilt = BANDS
            .iter()
            .map(|(lo, hi)| 3.0 * ((lo * hi).sqrt() / 100.0).log2())
            .collect();
        let sum: f32 = window.iter().sum();
        Analyzer {
            scratch: vec![Complex32::default(); fft.get_inplace_scratch_len()],
            buf: vec![Complex32::default(); WINDOW],
            fft,
            window,
            bins,
            tilt,
            reference: (sum / 2.0) * (sum / 2.0),
            rate,
        }
    }

    /// Samples between readings (fractional: readings land on exact times).
    fn hop(&self) -> f64 {
        self.rate as f64 / FPS as f64
    }

    /// Each band's level (dB, tilted; `None` for silence) of the window starting at `at`.
    fn reading(&mut self, samples: &[f32], at: usize) -> [Option<f32>; 4] {
        for (i, c) in self.buf.iter_mut().enumerate() {
            let x = samples.get(at + i).copied().unwrap_or(0.0);
            *c = Complex32::new(x * self.window[i], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        let mut out = [None; 4];
        for (k, bins) in self.bins.iter().enumerate() {
            let n = bins.len().max(1) as f32;
            let power: f32 = self.buf[bins.clone()]
                .iter()
                .map(|c| c.norm_sqr())
                .sum::<f32>()
                / n;
            let db = 10.0 * (power / self.reference).max(1e-20).log10();
            if db > SILENCE_DB {
                out[k] = Some(db + self.tilt[k]);
            }
        }
        out
    }
}

/// A band's range: `(low, high)` dB.
type Range = (f32, f32);

fn percentile(sorted: &[f32], p: f64) -> f32 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Each band's range over `readings`.
fn ranges<'a>(readings: impl Iterator<Item = &'a [Option<f32>; 4]> + Clone) -> [Option<Range>; 4] {
    std::array::from_fn(|k| {
        let mut v: Vec<f32> = readings.clone().filter_map(|r| r[k]).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable_by(f32::total_cmp);
        let hi = percentile(&v, HIGH);
        let lo = percentile(&v, LOW).min(hi - MIN_SPAN_DB);
        Some((lo, hi))
    })
}

/// A reading scaled to 0–15 per band and packed.
fn pack(reading: &[Option<f32>; 4], ranges: &[Option<Range>; 4]) -> [u8; FRAME_BYTES] {
    let level = |k: usize| -> u8 {
        match (reading[k], ranges[k]) {
            (Some(db), Some((lo, hi))) => {
                ((db - lo) / (hi - lo) * TOP).round().clamp(0.0, TOP) as u8
            }
            _ => 0,
        }
    };
    [level(0) << 4 | level(1), level(2) << 4 | level(3)]
}

/// The four levels (0–15) of reading `i` of a packed curve.
pub fn levels(curve: &[u8], i: usize) -> Option<[u8; 4]> {
    let f = curve.get(i * FRAME_BYTES..(i + 1) * FRAME_BYTES)?;
    Some([f[0] >> 4, f[0] & 15, f[1] >> 4, f[1] & 15])
}

/// The packed curve of a whole track (mono samples at `rate`).
pub fn curve(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut a = Analyzer::new(rate);
    let hop = a.hop();
    let n = (samples.len() as f64 / hop).ceil() as usize;
    let readings: Vec<[Option<f32>; 4]> = (0..n)
        .map(|i| a.reading(samples, (i as f64 * hop).round() as usize))
        .collect();
    let r = ranges(readings.iter());
    readings.iter().flat_map(|x| pack(x, &r)).collect()
}

/// A stream analysed as it plays: feed it mono samples, take packed readings.  Each band's
/// range comes from the last [`LIVE_WINDOW_SECS`], refreshed every second.
pub struct Live {
    analyzer: Analyzer,
    /// samples not yet fully read
    pending: Vec<f32>,
    /// position of `pending[0]` in the stream (samples)
    offset: u64,
    /// readings made
    count: u64,
    recent: VecDeque<[Option<f32>; 4]>,
    ranges: [Option<Range>; 4],
}

impl Live {
    pub fn new(rate: u32) -> Live {
        Live {
            analyzer: Analyzer::new(rate),
            pending: Vec::new(),
            offset: 0,
            count: 0,
            recent: VecDeque::new(),
            ranges: [None; 4],
        }
    }

    /// Readings made so far (reading `i` is at `i / FPS` seconds into the stream).
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Add samples; returns the packed readings they complete.
    pub fn feed(&mut self, samples: &[f32]) -> Vec<u8> {
        self.pending.extend_from_slice(samples);
        let hop = self.analyzer.hop();
        let mut out = Vec::new();
        loop {
            let at = (self.count as f64 * hop).round() as u64;
            let start = (at - self.offset) as usize;
            if start + WINDOW > self.pending.len() {
                break;
            }
            let reading = self.analyzer.reading(&self.pending, start);
            self.recent.push_back(reading);
            if self.recent.len() > LIVE_WINDOW_SECS * FPS as usize {
                self.recent.pop_front();
            }
            // a new range every second, and for each of the first readings
            if self.count < FPS as u64 || self.count.is_multiple_of(FPS as u64) {
                self.ranges = ranges(self.recent.iter());
            }
            out.extend(pack(&reading, &self.ranges));
            self.count += 1;
        }
        // drop what no further reading needs
        let next = (self.count as f64 * hop).round() as u64;
        let used = ((next - self.offset) as usize).min(self.pending.len());
        self.pending.drain(..used);
        self.offset += used as u64;
        out
    }
}

/// The stored curve of an audio fingerprint: `Some(None)` when it could not be made.
pub fn load(c: &Connection, fingerprint: &str) -> rusqlite::Result<Option<Option<Vec<u8>>>> {
    c.query_row(
        "SELECT curve FROM track_spectrum WHERE fingerprint = ? AND version = ?",
        params![fingerprint, VERSION],
        |r| r.get(0),
    )
    .optional()
}

/// Store a curve (`None`: the audio could not be decoded).
pub(crate) fn store(
    c: &Connection,
    fingerprint: &str,
    curve: Option<&[u8]>,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO track_spectrum (fingerprint, version, curve) VALUES (?, ?, ?)",
        params![fingerprint, VERSION, curve],
    )?;
    Ok(())
}

impl Library {
    /// A library track's spectrum curve: stored, or worked out now (decoding the whole file,
    /// about a second) and stored.  `None` for tracks that aren't local files or can't be
    /// decoded.  Blocking.
    pub fn track_spectrum(&self, track_id: i64, ffmpeg: Option<&str>) -> Option<Vec<u8>> {
        let c = self.conn().ok()?;
        let (fingerprint, url, secs): (String, String, Option<f64>) = c
            .query_row(
                "SELECT fingerprint, url, duration FROM tracks WHERE id = ? AND fingerprint IS NOT NULL",
                [track_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()?;
        if let Ok(Some(stored)) = load(&c, &fingerprint) {
            return stored;
        }
        let samples = crate::sonic::decode_url(&url, secs, ffmpeg)
            .map_err(|e| tracing::debug!("spectrum of {}: {}", url, e))
            .ok();
        let made = samples.map(|s| curve(&s, SAMPLE_RATE));
        let _ = store(&c, &fingerprint, made.as_deref());
        made
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = SAMPLE_RATE;

    fn tone(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize)
            .map(|i| amp * (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin())
            .collect()
    }

    fn all_levels(curve: &[u8]) -> Vec<[u8; 4]> {
        (0..curve.len() / FRAME_BYTES)
            .map(|i| levels(curve, i).unwrap())
            .collect()
    }

    #[test]
    fn twenty_readings_a_second() {
        let c = curve(&tone(440.0, 3.0, 0.5), RATE);
        assert_eq!(c.len(), 60 * FRAME_BYTES);
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        // a bass tone getting louder: the bass bar climbs, the treble bar stays dark
        let mut s = tone(80.0, 1.0, 0.05);
        s.extend(tone(80.0, 1.0, 0.8));
        let l = all_levels(&curve(&s, RATE));
        let early = l[5];
        let late = l[30];
        assert!(late[0] > early[0], "{early:?} → {late:?}");
        assert_eq!(late[0], 15);
        assert!(late[3] < 5, "treble {late:?}");
    }

    #[test]
    fn silence_is_level_zero() {
        let mut s = vec![0.0; RATE as usize];
        s.extend(tone(1000.0, 1.0, 0.5));
        let l = all_levels(&curve(&s, RATE));
        assert_eq!(l[5], [0, 0, 0, 0]);
        assert!(l[30][2] > 0);
    }

    #[test]
    fn levels_unpack_what_pack_packed() {
        let r = [Some(0.0), Some(10.0), None, Some(5.0)];
        let ranges = [Some((0.0, 10.0)); 4];
        let packed = pack(&r, &ranges);
        assert_eq!(levels(&packed, 0), Some([0, 15, 0, 8]));
        assert_eq!(levels(&packed, 1), None);
    }

    #[test]
    fn live_matches_the_whole_track_reading_for_reading() {
        // the same readings, at the same times, fed in uneven pieces
        let mut s = tone(200.0, 1.0, 0.3);
        s.extend(tone(3000.0, 1.0, 0.6));
        let whole = curve(&s, RATE);
        let mut live = Live::new(RATE);
        let mut got = Vec::new();
        for piece in s.chunks(777) {
            got.extend(live.feed(piece));
        }
        // the live analyser can't read the last windows until more audio comes
        let n = got.len() / FRAME_BYTES;
        assert!(n >= 38, "{n}");
        assert_eq!(live.count() as usize, n);
        // the ranges differ (live: only what was heard so far), the shape doesn't:
        // the 200 Hz band leads in the first second, the treble in the second
        let l = all_levels(&got);
        let w = all_levels(&whole);
        assert!(l[10][1] > l[10][3] && w[10][1] > w[10][3]);
        assert!(l[n - 2][3] > l[n - 2][1] && w[n - 2][3] > w[n - 2][1]);
    }
}
