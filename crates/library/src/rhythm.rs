//! Rhythm and key analysis for DJ-style transitions: tempo, beat grid, bars, musical key and a
//! per-bar loudness envelope, so a mix can beat-match two tracks, pick harmonically compatible
//! ones (Camelot wheel) and find intros and outros to mix in and out of.  Not part of LMS.
//!
//! Input is the same buffer bliss analyses in [`crate::sonic`]: the whole track as mono `f32`
//! at 22050 Hz.  Everything is pure Rust (FFTs with `rustfft`) and runs in a small fraction of
//! a second per track in release builds.  The methods are the standard MIR ones:
//!
//! * **Onset strength**: STFT (1024-sample Hann frames, hop 256 ≈ 11.6 ms), a 40-band mel
//!   spectrum in dB, and the half-wave rectified frame-to-frame increase averaged over bands
//!   (log spectral flux, as `librosa.onset.onset_strength`).
//! * **Tempo**: a tempogram (autocorrelation of the onset envelope in ~6 s windows, averaged
//!   over the track) weighted with a log-normal prior centred on 120 BPM (one octave wide), as
//!   librosa does, over 60–200 BPM.  The full-band tempogram is averaged with a low-band
//!   (kick/bass) one, so off-beat hi-hats don't double the tempo.  Octave errors remain possible
//!   where the music itself is ambiguous (drum & bass at 174 vs 87, half-time feels).  The final
//!   BPM is a linear regression of the tracked beat times, accurate to a few hundredths of a
//!   BPM on steady material.
//! * **Beats**: Ellis's dynamic-programming beat tracker (`librosa.beat.beat_track`), which
//!   follows moderate tempo drift (a few percent; not rubato); weak leading and trailing beats
//!   are trimmed and each beat is refined to a fraction of a frame on the onset envelope.
//!   Beats sit on the attacks (within a few ms on synthetic drums); the beat confidence is the
//!   onset envelope's periodicity at the beat period, calibrated on synthetic material only.
//! * **Downbeat** (assuming 4/4): of the four beat phases, the one whose beats carry the most
//!   low-frequency (kick/bass) onset energy.  This is a heuristic: it works when the kick or
//!   bass accents bar starts (most dance and pop music) but is a coin toss for music with even
//!   accents, syncopated bass lines, or metres other than 4/4.
//! * **Key**: spectral peaks of a long-window STFT (8192 samples, 2.7 Hz resolution) between
//!   55 Hz and 2 kHz, tuning-corrected and folded into a 12-bin chroma summed over the track,
//!   correlated with the Krumhansl–Kessler major and minor profiles in all 24 keys.  Relative
//!   major/minor and fifth-related keys are the usual confusions; the confidence is low when the
//!   best key barely beats the runner-up.
//!
//! [`Rhythm::to_blob`] packs the result into ~1.5 KB for a five-minute track for SQLite.

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// Analysis version: stored with each result so that results of an older algorithm are redone.
/// It is also the first byte of [`Rhythm::to_blob`].
pub const VERSION: i64 = 1;

/// Sample rate of the input (bliss's format).
pub const SAMPLE_RATE: u32 = 22050;
const SR: f64 = SAMPLE_RATE as f64;

/// Onset STFT: frame length and hop in samples.
const N_FFT: usize = 1024;
const HOP: usize = 256;
/// Onset envelope frames per second.
const FPS: f64 = SR / HOP as f64;
/// Mel bands of the onset spectrum, and their range.
const N_MELS: usize = 40;
const MEL_FMIN: f64 = 30.0;
const MEL_FMAX: f64 = 10_000.0;
/// Mel bands centred below this carry the "kick/bass" onsets used to find downbeats.
const LOW_BAND_HZ: f64 = 200.0;
/// Dynamic range of the dB spectrum below its loudest band (librosa's `top_db`).
const TOP_DB: f32 = 80.0;
/// Where the log flux of a sharp attack peaks relative to the attack itself (measured on
/// synthetic drum hits): about a frame early, because the attack already shows (at a level
/// the log compression makes count) in the tail of the Hann window of the frame before.
const ONSET_DELAY_SECS: f64 = -0.0105;

/// Tempo range and the perceptual prior (log-normal, in octaves).
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
const PRIOR_BPM: f64 = 120.0;
const PRIOR_OCTAVES: f64 = 1.0;
/// Tempogram window and hop, in onset frames (~5.9 s and ~3 s).
const TG_WIN: usize = 512;
const TG_HOP: usize = 256;
/// Tempogram windows whose onset envelope varies less than this (dB) are silence or a steady
/// tone and say nothing about tempo.
const MIN_ONSET_STD: f32 = 0.05;
/// Periodicity (normalised autocorrelation at the beat period) that counts as full confidence;
/// clean click tracks reach ~0.9, dense music less.
const FULL_PERIODICITY: f32 = 0.5;
/// Below this beat confidence there is no steady beat: bpm 0 and no beats.
const NO_BEAT_CONFIDENCE: f32 = 0.1;
/// DP beat tracker: how strongly beats keep to the tempo (librosa's default).
const TIGHTNESS: f32 = 100.0;

/// Key STFT: frame length and hop in samples, and the pitch range.
const KEY_FFT: usize = 8192;
const KEY_HOP: usize = 8192;
const KEY_FMIN: f64 = 55.0;
const KEY_FMAX: f64 = 2000.0;
/// Spectral peaks more than this far below a frame's strongest one are ignored (40 dB).
const KEY_PEAK_FLOOR: f32 = 0.01;
/// Frames quieter than this RMS (-80 dBFS) are skipped.
const KEY_MIN_RMS: f32 = 1e-4;
/// Correlation margin over the runner-up key that counts as full key confidence.
const KEY_FULL_MARGIN: f32 = 0.2;

/// Krumhansl–Kessler key profiles, indexed by semitones above the tonic.
const MAJOR_PROFILE: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Without a beat, the energy envelope has one value per this many seconds.
const ENERGY_SECS_NO_BEAT: f64 = 2.0;
/// Floor of the energy envelope (digital silence), dB.
const ENERGY_FLOOR_DB: f64 = -100.0;

/// Rhythm and key of a track.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rhythm {
    /// Tempo in beats per minute, to 0.01 (0 when no steady beat: speech, ambient, classical rubato)
    pub bpm: f32,
    /// How steady/strong the beat is, 0..1 (below ~0.3: don't beat-match this track)
    pub beat_confidence: f32,
    /// Beat times in seconds (the tracked beats, following tempo drift), to the millisecond
    pub beats: Vec<f32>,
    /// Index into `beats` of the first downbeat (bar start), assuming 4/4; bars = every 4th beat from it
    pub downbeat: u32,
    /// Musical key as 0..11 pitch class of the tonic (0 = C) and mode
    pub key: u8,
    pub minor: bool,
    /// 0..1: how clearly the best key beats the runner-up
    pub key_confidence: f32,
    /// Loudness envelope per bar (RMS in dB, one value per bar from the first downbeat; if no
    /// beat, per 2 seconds), to 0.01 dB, for finding intros/outros and mix points
    pub energy: Vec<f32>,
}

/// Analyse a whole track (mono, [`SAMPLE_RATE`]).  Never fails: silence, noise and very short
/// input give a result with no beat and zero confidences.
pub fn analyze(samples: &[f32]) -> Rhythm {
    let (key, minor, key_confidence) = detect_key(samples);
    let onsets = onset_envelope(samples);
    let beat = track_beats(&onsets);
    let energy = match &beat {
        Some(b) => bar_energy(samples, &b.beats, b.downbeat as usize, 60.0 / b.bpm as f64),
        None => window_energy(samples, ENERGY_SECS_NO_BEAT),
    };
    let (bpm, beat_confidence, beats, downbeat) = match beat {
        Some(b) => (b.bpm, b.confidence, b.beats, b.downbeat),
        None => (0.0, periodicity_confidence(&onsets), Vec::new(), 0),
    };
    Rhythm {
        bpm,
        beat_confidence,
        beats,
        downbeat,
        key,
        minor,
        key_confidence,
        energy,
    }
}

impl Rhythm {
    /// Camelot wheel notation (e.g. "8A" for A minor, "8B" for C major): neighbouring numbers
    /// and the same number in the other letter mix harmonically.
    pub fn camelot(&self) -> String {
        // the wheel goes round the circle of fifths: C major is 8B, G major 9B, ...; a minor
        // key shares its number with its relative major (three semitones up)
        let major_tonic = if self.minor {
            (self.key as u32 + 3) % 12
        } else {
            self.key as u32 % 12
        };
        let n = (major_tonic * 7 + 7) % 12 + 1;
        format!("{n}{}", if self.minor { 'A' } else { 'B' })
    }

    /// Bar start times in seconds (every 4th beat from [`Rhythm::downbeat`]).
    pub fn bars(&self) -> impl Iterator<Item = f32> + '_ {
        self.beats
            .iter()
            .skip(self.downbeat as usize)
            .step_by(4)
            .copied()
    }

    /// Compact binary encoding for a SQLite BLOB, read back by [`Rhythm::from_blob`].
    ///
    /// Little-endian: version byte ([`VERSION`]), bpm, beat and key confidence (`f32`), key and
    /// mode (`u8`), downbeat (`u32`), beat count (`u32`) then the first beat in ms (`u32`) and
    /// each following one as a `u16` ms delta (`0xFFFF` escapes to an absolute `u32`), energy
    /// count (`u32`) and values in 0.01 dB (`i16`).  Beats and energy are kept exactly as
    /// [`analyze`] quantises them, so a round trip is lossless; ~1.5 KB for five minutes.
    pub fn to_blob(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32 + self.beats.len() * 2 + self.energy.len() * 2);
        b.push(VERSION as u8);
        b.extend_from_slice(&self.bpm.to_le_bytes());
        b.extend_from_slice(&self.beat_confidence.to_le_bytes());
        b.extend_from_slice(&self.key_confidence.to_le_bytes());
        b.push(self.key);
        b.push(self.minor as u8);
        b.extend_from_slice(&self.downbeat.to_le_bytes());
        b.extend_from_slice(&(self.beats.len() as u32).to_le_bytes());
        let mut prev: Option<u32> = None;
        for &t in &self.beats {
            let ms = secs_to_ms(t);
            match prev {
                Some(p) if ms >= p && ms - p < 0xFFFF => {
                    b.extend_from_slice(&((ms - p) as u16).to_le_bytes())
                }
                Some(_) => {
                    b.extend_from_slice(&0xFFFFu16.to_le_bytes());
                    b.extend_from_slice(&ms.to_le_bytes());
                }
                None => b.extend_from_slice(&ms.to_le_bytes()),
            }
            prev = Some(ms);
        }
        b.extend_from_slice(&(self.energy.len() as u32).to_le_bytes());
        for &e in &self.energy {
            b.extend_from_slice(&db_to_centi(e).to_le_bytes());
        }
        b
    }

    /// Decode [`Rhythm::to_blob`]'s encoding; `None` for another version or a damaged blob.
    pub fn from_blob(b: &[u8]) -> Option<Rhythm> {
        let mut r = Reader(b);
        if r.u8()? as i64 != VERSION {
            return None;
        }
        let bpm = r.f32()?;
        let beat_confidence = r.f32()?;
        let key_confidence = r.f32()?;
        let key = r.u8()?;
        let minor = r.u8()? != 0;
        let downbeat = r.u32()?;
        let n = r.u32()? as usize;
        // every beat takes at least two bytes: don't let a bad count allocate a lot
        let mut beats = Vec::with_capacity(n.min(r.0.len() / 2));
        let mut prev: Option<u32> = None;
        for _ in 0..n {
            let ms = match prev {
                None => r.u32()?,
                Some(p) => match r.u16()? {
                    0xFFFF => r.u32()?,
                    d => p.checked_add(d as u32)?,
                },
            };
            beats.push(ms_to_secs(ms));
            prev = Some(ms);
        }
        let n = r.u32()? as usize;
        let mut energy = Vec::with_capacity(n.min(r.0.len() / 2));
        for _ in 0..n {
            energy.push(centi_to_db(r.i16()?));
        }
        if !r.0.is_empty() || key > 11 {
            return None;
        }
        Some(Rhythm {
            bpm,
            beat_confidence,
            beats,
            downbeat,
            key,
            minor,
            key_confidence,
            energy,
        })
    }
}

/// Byte cursor for [`Rhythm::from_blob`].
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn i16(&mut self) -> Option<i16> {
        self.take().map(i16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take().map(f32::from_le_bytes)
    }
}

/// Quantisation shared by [`analyze`] and the blob, so that a round trip is exact.
fn secs_to_ms(t: f32) -> u32 {
    (t as f64 * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32
}
fn ms_to_secs(ms: u32) -> f32 {
    (ms as f64 / 1000.0) as f32
}
fn db_to_centi(db: f32) -> i16 {
    (db as f64 * 100.0)
        .round()
        .clamp(i16::MIN as f64, i16::MAX as f64) as i16
}
fn centi_to_db(c: i16) -> f32 {
    (c as f64 / 100.0) as f32
}

// ---------------------------------------------------------------------------------------------
// spectra
// ---------------------------------------------------------------------------------------------

/// Power spectrum of real frames through a half-length complex FFT (even samples as the real
/// part, odd as the imaginary part, then split), twice as fast as a full complex FFT.
struct PowerSpectrum {
    fft: Arc<dyn Fft<f32>>,
    /// e^{-2πik/n} for k in 0..=n/2
    twiddles: Vec<Complex32>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl PowerSpectrum {
    /// For frames of `n` samples (even), Hann-windowed.
    fn new(n: usize) -> PowerSpectrum {
        let h = n / 2;
        let fft = FftPlanner::new().plan_fft_forward(h);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        let twiddles = (0..=h)
            .map(|k| {
                let a = -std::f64::consts::TAU * k as f64 / n as f64;
                Complex32::new(a.cos() as f32, a.sin() as f32)
            })
            .collect();
        PowerSpectrum {
            fft,
            twiddles,
            window: hann(n),
            buf: vec![Complex32::default(); h],
            scratch,
        }
    }

    /// The frame of `x` centred on sample `centre` (zero outside `x`), windowed; its power
    /// spectrum (n/2 + 1 bins) goes to `out`.
    fn frame(&mut self, x: &[f32], centre: usize, out: &mut [f32]) {
        let n = self.window.len();
        let h = n / 2;
        let start = centre as isize - h as isize;
        let sample = |i: usize| -> f32 {
            let j = start + i as isize;
            if j < 0 || j as usize >= x.len() {
                0.0
            } else {
                x[j as usize] * self.window[i]
            }
        };
        if start >= 0 && start as usize + n <= x.len() {
            // the common case, without a range check per sample
            let s = &x[start as usize..start as usize + n];
            for ((c, s), w) in self
                .buf
                .iter_mut()
                .zip(s.chunks_exact(2))
                .zip(self.window.chunks_exact(2))
            {
                *c = Complex32::new(s[0] * w[0], s[1] * w[1]);
            }
        } else {
            for i in 0..h {
                self.buf[i] = Complex32::new(sample(2 * i), sample(2 * i + 1));
            }
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        // X[k] = E[k] + e^{-2πik/n} O[k], with E[k] = (Z[k] + Z*[h-k]) / 2 the spectrum of the
        // even samples and O[k] = (Z[k] - Z*[h-k]) / 2i that of the odd ones (written out in
        // plain floats: complex operator calls are slow in debug builds)
        for (k, (o, w)) in out.iter_mut().zip(&self.twiddles).enumerate() {
            let zk = self.buf[if k == h { 0 } else { k }];
            let zm = self.buf[if k == 0 { 0 } else { h - k }];
            let (er, ei) = (0.5 * (zk.re + zm.re), 0.5 * (zk.im - zm.im));
            let (or, oi) = (0.5 * (zk.im + zm.im), -0.5 * (zk.re - zm.re));
            let re = er + w.re * or - w.im * oi;
            let im = ei + w.re * oi + w.im * or;
            *o = re * re + im * im;
        }
    }
}

/// Periodic Hann window (the STFT convention).
fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect()
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// Triangular mel filters over the onset STFT's bins: (first bin, weights, centre Hz).
fn mel_filters() -> Vec<(usize, Vec<f32>, f64)> {
    let (lo, hi) = (hz_to_mel(MEL_FMIN), hz_to_mel(MEL_FMAX));
    let edges: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let bin_hz = SR / N_FFT as f64;
    edges
        .windows(3)
        .map(|e| {
            let first = (e[0] / bin_hz).ceil() as usize;
            let last = ((e[2] / bin_hz).floor() as usize).min(N_FFT / 2);
            let weights = (first..=last)
                .map(|k| {
                    let f = k as f64 * bin_hz;
                    let w = if f <= e[1] {
                        (f - e[0]) / (e[1] - e[0])
                    } else {
                        (e[2] - f) / (e[2] - e[1])
                    };
                    w.max(0.0) as f32
                })
                .collect();
            (first, weights, e[1])
        })
        .collect()
}

/// Onset strength per STFT frame: overall, and in the low (kick/bass) bands.
struct Onsets {
    all: Vec<f32>,
    low: Vec<f32>,
}

/// Log-mel spectral flux, one value per hop (frame `t` is centred on sample `t * HOP`).
fn onset_envelope(samples: &[f32]) -> Onsets {
    let n_frames = samples.len() / HOP + 1;
    let filters = mel_filters();
    let n_low = filters.iter().filter(|f| f.2 < LOW_BAND_HZ).count().max(1);
    let mut spec = PowerSpectrum::new(N_FFT);
    let mut power = vec![0f32; N_FFT / 2 + 1];
    let mut mel = vec![0f32; n_frames * N_MELS];
    let mut max = 0f32;
    for (t, m) in mel.chunks_exact_mut(N_MELS).enumerate() {
        spec.frame(samples, t * HOP, &mut power);
        for (v, (first, w, _)) in m.iter_mut().zip(&filters) {
            *v = w.iter().zip(&power[*first..]).map(|(w, p)| w * p).sum();
            max = max.max(*v);
        }
    }
    let mut all = vec![0f32; n_frames];
    let mut low = vec![0f32; n_frames];
    if max <= 0.0 {
        return Onsets { all, low };
    }
    // dB relative to the loudest band, floored TOP_DB below it
    let floor = max * 10f32.powf(-TOP_DB / 10.0);
    for v in &mut mel {
        *v = 10.0 * v.max(floor).log10();
    }
    for t in 1..n_frames {
        let (prev, cur) = (
            &mel[(t - 1) * N_MELS..t * N_MELS],
            &mel[t * N_MELS..(t + 1) * N_MELS],
        );
        let mut sum = 0.0;
        let mut sum_low = 0.0;
        for (b, (c, p)) in cur.iter().zip(prev).enumerate() {
            let d = (c - p).max(0.0);
            sum += d;
            if b < n_low {
                sum_low += d;
            }
        }
        all[t] = sum / N_MELS as f32;
        low[t] = sum_low / n_low as f32;
    }
    Onsets { all, low }
}

// ---------------------------------------------------------------------------------------------
// tempo and beats
// ---------------------------------------------------------------------------------------------

/// Onset-frame lag of a tempo, and back.
fn bpm_to_lag(bpm: f64) -> f64 {
    60.0 * FPS / bpm
}
fn lag_to_bpm(lag: f64) -> f64 {
    60.0 * FPS / lag
}

/// Mean normalised local autocorrelation of the onset envelope at lags 0..=max_lag, over the
/// windows that have any onsets at all; `None` when none do.
fn tempogram(onset: &[f32], max_lag: usize) -> Option<Vec<f32>> {
    let win = TG_WIN.min(onset.len());
    if win <= max_lag + 1 {
        return None;
    }
    let weights = hann(win);
    let mut acc = vec![0f64; max_lag + 1];
    let mut count = 0usize;
    let mut seg = vec![0f32; win];
    let mut start = 0;
    while start + win <= onset.len() {
        let x = &onset[start..start + win];
        start += TG_HOP;
        let mean = x.iter().sum::<f32>() / win as f32;
        let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / win as f32;
        if var.sqrt() < MIN_ONSET_STD {
            continue;
        }
        for ((s, v), w) in seg.iter_mut().zip(x).zip(&weights) {
            *s = (v - mean) * w;
        }
        let ac0: f32 = seg.iter().map(|v| v * v).sum();
        if ac0 <= 0.0 {
            continue;
        }
        for (lag, a) in acc.iter_mut().enumerate() {
            let r: f32 = seg[lag..].iter().zip(&seg).map(|(a, b)| a * b).sum();
            *a += (r / ac0) as f64;
        }
        count += 1;
    }
    (count > 0).then(|| acc.iter().map(|a| (*a / count as f64) as f32).collect())
}

/// Vertex of the parabola through `y[i-1..=i+1]` (`i` must have both neighbours).
fn parabolic(y: &[f32], i: usize) -> (f64, f32) {
    let (a, b, c) = (y[i - 1] as f64, y[i] as f64, y[i + 1] as f64);
    let den = a - 2.0 * b + c;
    if den >= 0.0 {
        return (i as f64, y[i]);
    }
    let d = (0.5 * (a - c) / den).clamp(-0.5, 0.5);
    (i as f64 + d, (b - 0.25 * (a - c) * d) as f32)
}

/// Beat period (in onset frames) and the periodicity of the onset envelope at it.
///
/// The tempo is the tempogram peak with the best prior-weighted score, where the tempogram is
/// the mean of the full-band and the low-band (kick/bass) ones when there are low-band onsets.
/// The full band alone often cannot tell a beat from its eighth notes: in log-flux terms an
/// off-beat hi-hat is nearly as strong an onset as a kick, so 95 BPM with eighth-note hats looks
/// like 190 BPM.  The low band keeps to the kicks, which is also how DJs count tempo.
fn estimate_period(onsets: &Onsets) -> Option<(f64, f32)> {
    let min_lag = bpm_to_lag(MAX_BPM).floor() as usize;
    let max_lag = bpm_to_lag(MIN_BPM).ceil() as usize;
    let tg = tempogram(&onsets.all, max_lag + 1)?;
    let score = match tempogram(&onsets.low, max_lag + 1) {
        Some(low) => tg.iter().zip(&low).map(|(a, b)| 0.5 * (a + b)).collect(),
        None => tg.clone(),
    };
    let prior = |lag: f64| {
        let octaves = (lag_to_bpm(lag) / PRIOR_BPM).log2() / PRIOR_OCTAVES;
        (-0.5 * octaves * octaves).exp() as f32
    };
    let best = (min_lag.max(1)..=max_lag)
        .filter(|&l| score[l] > 0.0 && score[l] >= score[l - 1] && score[l] >= score[l + 1])
        .max_by(|&a, &b| (score[a] * prior(a as f64)).total_cmp(&(score[b] * prior(b as f64))))?;
    let (lag, _) = parabolic(&score, best);
    let lag = lag.clamp(bpm_to_lag(MAX_BPM), bpm_to_lag(MIN_BPM));
    // periodicity of the full-band envelope there, interpolated
    let (i, frac) = (lag.floor() as usize, (lag - lag.floor()) as f32);
    let periodicity = tg[i] + frac * (tg[(i + 1).min(tg.len() - 1)] - tg[i]);
    Some((lag, periodicity))
}

/// Beat confidence from the periodicity at the beat period.
fn confidence_from_periodicity(p: f32) -> f32 {
    (p / FULL_PERIODICITY).clamp(0.0, 1.0)
}

/// Confidence for a track where no beat was tracked (for the record: why there is none).
fn periodicity_confidence(onsets: &Onsets) -> f32 {
    estimate_period(onsets).map_or(0.0, |(_, p)| confidence_from_periodicity(p))
}

/// A tracked beat grid.
struct Beats {
    bpm: f32,
    confidence: f32,
    beats: Vec<f32>,
    downbeat: u32,
}

/// Tempo, beats and downbeat; `None` without a steady beat.
fn track_beats(onsets: &Onsets) -> Option<Beats> {
    let (period, periodicity) = estimate_period(onsets)?;
    let confidence = confidence_from_periodicity(periodicity);
    if confidence < NO_BEAT_CONFIDENCE {
        return None;
    }
    let local = local_score(&onsets.all, period);
    let frames = dp_beats(&local, period);
    let frames = trim_beats(&local, frames);
    if frames.len() < 4 {
        return None;
    }
    let downbeat = find_downbeat(onsets, &frames);
    // refine each beat to a fraction of a frame on the onset envelope
    let times: Vec<f64> = frames
        .iter()
        .map(|&f| {
            let lo = f.saturating_sub(1).max(1);
            let hi = (f + 1).min(onsets.all.len() - 2);
            let j = (lo..=hi)
                .max_by(|&a, &b| onsets.all[a].total_cmp(&onsets.all[b]))
                .unwrap_or(f);
            let (x, _) = if j >= 1 && j + 1 < onsets.all.len() {
                parabolic(&onsets.all, j)
            } else {
                (j as f64, 0.0)
            };
            (x / FPS - ONSET_DELAY_SECS).max(0.0)
        })
        .collect();
    let bpm = regression_bpm(&times, period / FPS).unwrap_or(lag_to_bpm(period));
    Some(Beats {
        bpm: ((bpm * 100.0).round() / 100.0) as f32,
        confidence,
        beats: times
            .iter()
            .map(|&t| ms_to_secs(secs_to_ms(t as f32)))
            .collect(),
        downbeat,
    })
}

/// Onset envelope normalised and smoothed with a Gaussian of a 32nd of the beat period
/// (librosa's `__beat_local_score`).
fn local_score(onset: &[f32], period: f64) -> Vec<f32> {
    let n = onset.len();
    let mean = onset.iter().sum::<f32>() / n as f32;
    let var = onset.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / (n.max(2) - 1) as f32;
    let std = var.sqrt().max(1e-9);
    let radius = period.round() as usize;
    let kernel: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let x = (i as f64 - radius as f64) * 32.0 / period;
            (-0.5 * x * x).exp() as f32
        })
        .collect();
    (0..n)
        .map(|t| {
            let lo = t.saturating_sub(radius);
            let hi = (t + radius).min(n - 1);
            (lo..=hi)
                .map(|j| onset[j] * kernel[j + radius - t])
                .sum::<f32>()
                / std
        })
        .collect()
}

/// Ellis's dynamic-programming beat tracker: the beat sequence maximising onset strength at
/// the beats minus a penalty for intervals that stray from the period.  Returns beat frames.
fn dp_beats(local: &[f32], period: f64) -> Vec<usize> {
    let n = local.len();
    let min_d = (period / 2.0).round().max(1.0) as usize;
    let max_d = (2.0 * period).round() as usize;
    // transition cost by interval, d in min_d..=max_d
    let cost: Vec<f32> = (0..=max_d)
        .map(|d| {
            let r = (d as f64 / period).ln();
            (-(TIGHTNESS as f64) * r * r) as f32
        })
        .collect();
    let threshold = 0.01 * local.iter().copied().fold(0.0, f32::max);
    let mut cum = vec![0f32; n];
    let mut back = vec![usize::MAX; n];
    let mut first_beat = true;
    for t in 0..n {
        // predecessors before the start of the track count as score 0, so that a beat early
        // in the track isn't penalised for having no predecessor at the right distance
        let mut best = f32::NEG_INFINITY;
        let mut best_at = usize::MAX;
        for (d, &c) in cost.iter().enumerate().skip(min_d) {
            let (s, at) = match t.checked_sub(d) {
                Some(p) => (cum[p] + c, p),
                None => (c, usize::MAX),
            };
            if s > best {
                best = s;
                best_at = at;
            }
        }
        cum[t] = local[t] + best;
        // a beat sequence starts at the first real onset, not in the silence before it
        if first_beat && local[t] < threshold {
            back[t] = usize::MAX;
        } else {
            back[t] = best_at;
            first_beat = false;
        }
    }
    // the last beat: the last local maximum of the cumulative score that is at least half the
    // median of them (librosa's `__last_beat`)
    let is_max = |i: usize| i > 0 && i + 1 < n && cum[i] > cum[i - 1] && cum[i] >= cum[i + 1];
    let mut maxima: Vec<f32> = (0..n).filter(|&i| is_max(i)).map(|i| cum[i]).collect();
    if maxima.is_empty() {
        return Vec::new();
    }
    maxima.sort_by(f32::total_cmp);
    let median = maxima[maxima.len() / 2];
    let Some(mut b) = (0..n).rev().find(|&i| is_max(i) && 2.0 * cum[i] > median) else {
        return Vec::new();
    };
    let mut beats = vec![b];
    while back[b] != usize::MAX {
        b = back[b];
        beats.push(b);
    }
    beats.reverse();
    beats
}

/// Drop weak beats at the start and end (silence or a beatless intro/outro the tracker ran
/// through): those whose smoothed onset strength is under half the RMS of all beats'.
fn trim_beats(local: &[f32], beats: Vec<usize>) -> Vec<usize> {
    let strength: Vec<f32> = beats.iter().map(|&b| local[b]).collect();
    // Hann(5) smoothing: [0, 0.5, 1, 0.5, 0]
    let smooth: Vec<f32> = (0..strength.len())
        .map(|i| {
            let at = |j: isize| -> f32 {
                if j < 0 || j as usize >= strength.len() {
                    0.0
                } else {
                    strength[j as usize]
                }
            };
            let i = i as isize;
            0.5 * at(i - 1) + at(i) + 0.5 * at(i + 1)
        })
        .collect();
    if smooth.is_empty() {
        return beats;
    }
    let rms = (smooth.iter().map(|v| v * v).sum::<f32>() / smooth.len() as f32).sqrt();
    let threshold = 0.5 * rms;
    let Some(first) = smooth.iter().position(|&v| v > threshold) else {
        return Vec::new();
    };
    let last = smooth.iter().rposition(|&v| v > threshold).unwrap_or(first);
    beats[first..=last].to_vec()
}

/// Tempo from beat times: slope of a linear regression of time on beat number (skipped or
/// doubled beats are numbered by the interval), robust to a few frames of jitter.
fn regression_bpm(times: &[f64], period_secs: f64) -> Option<f64> {
    if times.len() < 4 {
        return None;
    }
    let mut idx = Vec::with_capacity(times.len());
    let mut k = 0f64;
    idx.push(0.0);
    for w in times.windows(2) {
        k += ((w[1] - w[0]) / period_secs).round().max(1.0);
        idx.push(k);
    }
    let n = times.len() as f64;
    let mx = idx.iter().sum::<f64>() / n;
    let my = times.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (x, y) in idx.iter().zip(times) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
    }
    let slope = sxy / sxx;
    (sxx > 0.0 && slope > 0.0).then(|| 60.0 / slope)
}

/// Which beat (0..=3) starts the first bar: the phase whose beats have the most low-frequency
/// onset energy (kick drums and bass notes tend to land on bar starts), with the overall
/// onset strength as a lesser vote.
fn find_downbeat(onsets: &Onsets, frames: &[usize]) -> u32 {
    let peak = |env: &[f32], f: usize| {
        env[f.saturating_sub(2)..(f + 3).min(env.len())]
            .iter()
            .copied()
            .fold(0.0, f32::max)
    };
    let norm = |env: &[f32]| {
        let m = frames.iter().map(|&f| peak(env, f)).sum::<f32>() / frames.len() as f32;
        if m > 0.0 {
            1.0 / m
        } else {
            0.0
        }
    };
    let (nl, na) = (norm(&onsets.low), norm(&onsets.all));
    let mut score = [0f32; 4];
    let mut count = [0usize; 4];
    for (i, &f) in frames.iter().enumerate() {
        score[i % 4] += peak(&onsets.low, f) * nl + 0.5 * peak(&onsets.all, f) * na;
        count[i % 4] += 1;
    }
    (0..4)
        .max_by(|&a, &b| {
            let sa = score[a] / count[a].max(1) as f32;
            let sb = score[b] / count[b].max(1) as f32;
            sa.total_cmp(&sb).then(b.cmp(&a))
        })
        .unwrap_or(0) as u32
}

// ---------------------------------------------------------------------------------------------
// energy
// ---------------------------------------------------------------------------------------------

/// RMS of `samples[a..b]` in dB, quantised like the blob.
fn rms_db(samples: &[f32], a: usize, b: usize) -> f32 {
    let s = &samples[a.min(samples.len())..b.min(samples.len())];
    let ms = if s.is_empty() {
        0.0
    } else {
        s.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / s.len() as f64
    };
    let db = (10.0 * ms.log10()).max(ENERGY_FLOOR_DB);
    centi_to_db(db_to_centi(db as f32))
}

/// One RMS value per bar: from each downbeat to the next (the last bar is four beat periods
/// long, or up to the end of the track).
fn bar_energy(samples: &[f32], beats: &[f32], downbeat: usize, period: f64) -> Vec<f32> {
    let to_sample = |t: f64| (t * SR).round().max(0.0) as usize;
    let end = samples.len();
    (downbeat..beats.len())
        .step_by(4)
        .filter_map(|i| {
            let a = to_sample(beats[i] as f64);
            let b = match beats.get(i + 4) {
                Some(&t) => to_sample(t as f64),
                None => to_sample(beats[i] as f64 + 4.0 * period).min(end),
            };
            (a < b).then(|| rms_db(samples, a, b))
        })
        .collect()
}

/// One RMS value per `secs` window from the start.
fn window_energy(samples: &[f32], secs: f64) -> Vec<f32> {
    let w = (secs * SR) as usize;
    (0..samples.len())
        .step_by(w.max(1))
        .map(|a| rms_db(samples, a, a + w))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// key
// ---------------------------------------------------------------------------------------------

/// Pearson correlation.
fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa <= 0.0 || sbb <= 0.0 {
        0.0
    } else {
        sab / (saa * sbb).sqrt()
    }
}

/// Track chroma: spectral peaks folded onto pitch classes (0 = C), after correcting for
/// tuning that is not A = 440 Hz.  Each frame contributes in proportion to its share of the
/// peaks' magnitude, so loud passages don't drown out the rest.
fn chroma(samples: &[f32]) -> [f32; 12] {
    let mut spec = PowerSpectrum::new(KEY_FFT);
    let mut power = vec![0f32; KEY_FFT / 2 + 1];
    let mut mag = vec![0f32; KEY_FFT / 2 + 1];
    let bin_hz = SR / KEY_FFT as f64;
    let kmin = ((KEY_FMIN / bin_hz).floor() as usize).max(1);
    let kmax = ((KEY_FMAX / bin_hz).ceil() as usize).min(KEY_FFT / 2 - 1);
    // window energy for a Hann-windowed full-scale signal, to turn power into frame RMS
    let window_power: f32 = hann(KEY_FFT).iter().map(|w| w * w).sum();
    // (fractional MIDI pitch, weight) of every peak
    let mut peaks: Vec<(f32, f32)> = Vec::new();
    let mut centre = KEY_FFT / 2;
    while centre < samples.len() + KEY_FFT / 2 && !samples.is_empty() {
        spec.frame(samples, centre, &mut power);
        centre += KEY_HOP;
        // Parseval: sum over the full spectrum = 2 * one-sided (roughly)
        let energy: f32 = 2.0 * power.iter().sum::<f32>() / KEY_FFT as f32;
        if (energy / window_power).sqrt() < KEY_MIN_RMS {
            continue;
        }
        for k in kmin - 1..=kmax + 1 {
            mag[k] = power[k].sqrt();
        }
        let top = mag[kmin..=kmax].iter().copied().fold(0.0, f32::max);
        if top <= 0.0 {
            continue;
        }
        let first = peaks.len();
        for k in kmin..=kmax {
            let m = mag[k];
            if m > top * KEY_PEAK_FLOOR && m > mag[k - 1] && m >= mag[k + 1] {
                // parabolic interpolation on log magnitude locates a Hann-windowed sinusoid
                // to a few hundredths of a bin
                let (a, b, c) = (
                    mag[k - 1].max(1e-12).ln(),
                    m.ln(),
                    mag[k + 1].max(1e-12).ln(),
                );
                let den = a - 2.0 * b + c;
                let d = if den < 0.0 {
                    (0.5 * (a - c) / den).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                let f = (k as f64 + d as f64) * bin_hz;
                let midi = 69.0 + 12.0 * (f / 440.0).log2();
                peaks.push((midi as f32, m));
            }
        }
        let total: f32 = peaks[first..].iter().map(|p| p.1).sum();
        for p in &mut peaks[first..] {
            p.1 /= total;
        }
    }
    // tuning: the weighted circular mean of the peaks' offsets from equal temperament
    let (mut s, mut c) = (0f64, 0f64);
    for &(midi, w) in &peaks {
        let a = std::f64::consts::TAU * (midi as f64 - (midi as f64).round());
        s += w as f64 * a.sin();
        c += w as f64 * a.cos();
    }
    let tuning = if s == 0.0 && c == 0.0 {
        0.0
    } else {
        s.atan2(c) / std::f64::consts::TAU
    };
    let mut chroma = [0f32; 12];
    for &(midi, w) in &peaks {
        let note = (midi as f64 - tuning).round() as i64;
        chroma[note.rem_euclid(12) as usize] += w;
    }
    chroma
}

/// Key by correlating the chroma with the 24 rotated key profiles: (tonic, minor, confidence).
fn detect_key(samples: &[f32]) -> (u8, bool, f32) {
    let chroma = chroma(samples);
    if chroma.iter().sum::<f32>() <= 0.0 {
        return (0, false, 0.0);
    }
    let mut scores: Vec<(f32, u8, bool)> = Vec::with_capacity(24);
    for (profile, minor) in [(&MAJOR_PROFILE, false), (&MINOR_PROFILE, true)] {
        for tonic in 0..12 {
            let rotated: [f32; 12] = std::array::from_fn(|pc| profile[(pc + 12 - tonic) % 12]);
            scores.push((correlation(&chroma, &rotated), tonic as u8, minor));
        }
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, tonic, minor) = scores[0];
    let margin = best - scores[1].0;
    let confidence = (margin / KEY_FULL_MARGIN).clamp(0.0, 1.0) * best.clamp(0.0, 1.0);
    (tonic, minor, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const SRU: usize = SAMPLE_RATE as usize;

    /// The packed real FFT agrees with a plain DFT, including partial frames at the edges.
    #[test]
    fn rhythm_power_spectrum() {
        let n = 32;
        let mut noise = Noise(3);
        let x: Vec<f32> = (0..40).map(|_| noise.next()).collect();
        let mut spec = PowerSpectrum::new(n);
        let mut out = vec![0f32; n / 2 + 1];
        for centre in [0usize, 16, 20, 39] {
            spec.frame(&x, centre, &mut out);
            let w = hann(n);
            let frame: Vec<f64> = (0..n)
                .map(|i| {
                    let j = centre as isize - (n / 2) as isize + i as isize;
                    if j < 0 || j as usize >= x.len() {
                        0.0
                    } else {
                        (x[j as usize] * w[i]) as f64
                    }
                })
                .collect();
            for (k, &o) in out.iter().enumerate() {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, v) in frame.iter().enumerate() {
                    let a = -std::f64::consts::TAU * (k * i) as f64 / n as f64;
                    re += v * a.cos();
                    im += v * a.sin();
                }
                let want = re * re + im * im;
                assert!(
                    (o as f64 - want).abs() < 1e-4 * (1.0 + want),
                    "{centre} {k}"
                );
            }
        }
    }

    /// Deterministic noise in -1..1 (xorshift).
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    /// A drum-like hit at `t` seconds: a noise burst (snare/hat) and a decaying low sine
    /// (kick) at level `kick`.
    fn hit(buf: &mut [f32], t: f64, gain: f32, kick: f32, noise: &mut Noise) {
        let start = (t * SR).round() as usize;
        for i in 0..(0.15 * SR) as usize {
            let Some(s) = buf.get_mut(start + i) else {
                break;
            };
            let x = i as f32 / SRU as f32;
            let burst = noise.next() * (-x / 0.008).exp() * 0.5;
            let thump = (std::f32::consts::TAU * 60.0 * x).sin() * (-x / 0.05).exp() * kick;
            *s += gain * (burst + thump);
        }
    }

    /// Beat times of a tempo ramping linearly from `bpm0` to `bpm1` over `secs`.
    fn ramp_times(bpm0: f64, bpm1: f64, secs: f64) -> Vec<f64> {
        // beats elapsed: phi(t) = (bpm0 t + (bpm1 - bpm0) t² / (2 secs)) / 60
        let a = (bpm1 - bpm0) / (2.0 * secs * 60.0);
        let b = bpm0 / 60.0;
        (0..)
            .map(|k| {
                let k = k as f64;
                if a.abs() < 1e-12 {
                    k / b
                } else {
                    (-b + (b * b + 4.0 * a * k).sqrt()) / (2.0 * a)
                }
            })
            .take_while(|&t| t < secs - 0.2)
            .collect()
    }

    fn steady(bpm: f64, secs: f64) -> Vec<f64> {
        ramp_times(bpm, bpm, secs)
    }

    /// A drum track of `secs` with hits at `times` + `lead`; with `accent_at = Some(p)`, the
    /// hits `p, p + 4, ...` are accented (louder, with a kick) and the others are plain.
    /// Returns samples and the true beat times.
    fn drums(
        times: &[f64],
        secs: f64,
        lead: f64,
        accent_at: Option<usize>,
    ) -> (Vec<f32>, Vec<f64>) {
        let mut buf = vec![0f32; (secs * SR) as usize];
        let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
        let truth: Vec<f64> = times.iter().map(|t| t + lead).collect();
        for (i, &t) in truth.iter().enumerate() {
            let (gain, kick) = match accent_at {
                Some(p) if i % 4 == p => (1.0, 0.8),
                Some(_) => (0.6, 0.0),
                None => (0.8, 0.5),
            };
            hit(&mut buf, t, gain, kick, &mut noise);
        }
        // a little background noise, like a real recording
        for s in &mut buf {
            *s += noise.next() * 0.002;
        }
        (buf, truth)
    }

    /// Worst error of the detected beats against the nearest true beat, the mean signed error,
    /// and the fraction of true beats (away from the edges) found within 15 ms.
    fn beat_errors(beats: &[f32], truth: &[f64]) -> (f64, f64, f64) {
        let errs: Vec<f64> = beats
            .iter()
            .map(|&b| {
                let b = b as f64;
                truth
                    .iter()
                    .map(|t| b - t)
                    .min_by(|x, y| x.abs().total_cmp(&y.abs()))
                    .unwrap_or(f64::MAX)
            })
            .collect();
        let worst = errs.iter().map(|e| e.abs()).fold(0.0, f64::max);
        let mean = errs.iter().sum::<f64>() / errs.len().max(1) as f64;
        let inner = &truth[2..truth.len() - 2];
        let found = inner
            .iter()
            .filter(|&&t| beats.iter().any(|&b| (b as f64 - t).abs() <= 0.015))
            .count();
        (worst, mean, found as f64 / inner.len() as f64)
    }

    #[test]
    fn rhythm_click_tempos() {
        for bpm in [90.0, 120.0, 128.0, 140.0, 174.0] {
            let (buf, truth) = drums(&steady(bpm, 60.0), 61.0, 0.5, None);
            let t = Instant::now();
            let r = analyze(&buf);
            let (worst, mean, found) = beat_errors(&r.beats, &truth);
            println!(
                "{bpm} BPM: got {} conf {:.2}, {} beats (truth {}), worst {:.1} ms, mean {:+.1} ms, found {:.0}%, {:?}",
                r.bpm,
                r.beat_confidence,
                r.beats.len(),
                truth.len(),
                worst * 1000.0,
                mean * 1000.0,
                found * 100.0,
                t.elapsed()
            );
            assert!((r.bpm as f64 - bpm).abs() <= 0.1, "{bpm}: {}", r.bpm);
            assert!(r.beat_confidence > 0.7, "{bpm}: {}", r.beat_confidence);
            assert!(worst <= 0.015, "{bpm}: worst {worst}");
            assert!(found > 0.95, "{bpm}: found {found}");
        }
    }

    /// Kicks on the beat and quieter hi-hats on the off-beats: the tempo is the kicks', not
    /// twice it; and a beatless intro gets no beats.
    #[test]
    fn rhythm_offbeat_hats_and_intro() {
        for bpm in [95.0, 110.0, 128.0] {
            let intro = 12.0;
            let secs = 60.0;
            let times = steady(bpm, secs - intro);
            let (mut buf, truth) = drums(&times, secs, intro, Some(0));
            let mut noise = Noise(11);
            let half = 30.0 / bpm;
            for &t in &truth {
                hit(&mut buf, t + half, 0.3, 0.0, &mut noise);
            }
            // a sustained chord under the intro and the beat
            for (n, midi) in [57, 60, 64].into_iter().enumerate() {
                note(&mut buf, midi, 0.0, secs, 0.05 + 0.01 * n as f32);
            }
            let r = analyze(&buf);
            let (worst, _, found) = beat_errors(&r.beats, &truth);
            println!(
                "{bpm} BPM + off-beat hats after {intro} s: got {} conf {:.2}, first beat {}, worst {:.1} ms, found {:.0}%",
                r.bpm,
                r.beat_confidence,
                r.beats[0],
                worst * 1000.0,
                found * 100.0
            );
            assert!((r.bpm as f64 - bpm).abs() <= 0.1, "{bpm}: {}", r.bpm);
            assert!(r.beats[0] as f64 > intro - 0.1, "{bpm}: {}", r.beats[0]);
            assert!(worst <= 0.015 && found > 0.95, "{bpm}: {worst} {found}");
        }
    }

    #[test]
    fn rhythm_downbeat_accents() {
        for (bpm, accent_at) in [(120.0, 0usize), (128.0, 1), (100.0, 2), (140.0, 3)] {
            let (buf, truth) = drums(&steady(bpm, 40.0), 41.0, 0.3, Some(accent_at));
            let r = analyze(&buf);
            let first_bar = r.beats[r.downbeat as usize] as f64;
            let idx = truth
                .iter()
                .position(|t| (t - first_bar).abs() < 0.015)
                .expect("downbeat on a true beat");
            println!(
                "{bpm} BPM, accent on beat {accent_at}: downbeat index {} = true beat {idx}; first beats {:?} truth {:?}",
                r.downbeat,
                &r.beats[..3],
                &truth[..3]
            );
            assert_eq!(idx % 4, accent_at, "{bpm} BPM");
            assert!(r.downbeat < 4);
        }
    }

    #[test]
    fn rhythm_tempo_drift() {
        let times = ramp_times(120.0, 124.0, 90.0);
        let (buf, truth) = drums(&times, 90.0, 0.0, None);
        let r = analyze(&buf);
        let (worst, mean, found) = beat_errors(&r.beats, &truth);
        println!(
            "120->124 BPM: got {} conf {:.2}, worst {:.1} ms, mean {:+.1} ms, found {:.0}%",
            r.bpm,
            r.beat_confidence,
            worst * 1000.0,
            mean * 1000.0,
            found * 100.0
        );
        assert!(r.bpm > 120.0 && r.bpm < 124.0, "{}", r.bpm);
        assert!(worst <= 0.015, "worst {worst}");
        assert!(found > 0.95, "found {found}");
    }

    #[test]
    fn rhythm_no_beat() {
        let mut noise = Noise(42);
        let white: Vec<f32> = (0..30 * SRU).map(|_| noise.next() * 0.3).collect();
        let tone: Vec<f32> = (0..30 * SRU)
            .map(|i| ((std::f64::consts::TAU * 440.0 * i as f64 / SR).sin() * 0.5) as f32)
            .collect();
        let silence = vec![0f32; 10 * SRU];
        for (name, buf) in [("noise", white), ("tone", tone), ("silence", silence)] {
            let r = analyze(&buf);
            println!(
                "{name}: bpm {} conf {:.3} beats {}",
                r.bpm,
                r.beat_confidence,
                r.beats.len()
            );
            assert!(r.beat_confidence < 0.3, "{name}: {}", r.beat_confidence);
            assert!(r.bpm == 0.0 || r.beat_confidence < 0.3, "{name}");
            // no beat: energy every 2 s
            if r.bpm == 0.0 {
                assert_eq!(r.energy.len(), buf.len().div_ceil(2 * SRU), "{name}");
            }
        }
        assert_eq!(analyze(&[]).bpm, 0.0);
        assert_eq!(analyze(&[0.1; 100]).bpm, 0.0);
    }

    /// A note with a few harmonics (MIDI pitch), added to `buf` over `[t0, t1)` seconds.
    fn note(buf: &mut [f32], midi: i32, t0: f64, t1: f64, gain: f32) {
        let f = 440.0 * 2f64.powf((midi as f64 - 69.0) / 12.0);
        let (a, b) = ((t0 * SR) as usize, ((t1 * SR) as usize).min(buf.len()));
        for (i, s) in buf[a..b].iter_mut().enumerate() {
            let x = i as f64 / SR;
            let env = (x / 0.01).min(1.0) * (-x / 1.5).exp();
            let mut v = 0.0;
            for (h, amp) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.3), (4.0, 0.2), (5.0, 0.1)] {
                v += amp * (std::f64::consts::TAU * f * h * x).sin();
            }
            *s += gain * (env * v) as f32;
        }
    }

    /// 32 s of a I–IV–V–I (major) or i–iv–V–i (harmonic minor) progression on `tonic`, with a
    /// bass note and a melody running up and down the scale.
    fn progression(tonic: u8, minor: bool) -> Vec<f32> {
        let scale: [i32; 7] = if minor {
            [0, 2, 3, 5, 7, 8, 11]
        } else {
            [0, 2, 4, 5, 7, 9, 11]
        };
        let chords: [[i32; 3]; 4] = if minor {
            [[0, 3, 7], [5, 8, 12], [7, 11, 14], [0, 3, 7]]
        } else {
            [[0, 4, 7], [5, 9, 12], [7, 11, 14], [0, 4, 7]]
        };
        let secs = 32.0;
        let mut buf = vec![0f32; (secs * SR) as usize];
        let base = 48 + tonic as i32; // C3..B3
        for cycle in 0..4 {
            for (j, ch) in chords.iter().enumerate() {
                let t0 = (cycle * 4 + j) as f64 * 2.0;
                note(&mut buf, base - 12 + ch[0], t0, t0 + 2.0, 0.12);
                for &n in ch {
                    note(&mut buf, base + n, t0, t0 + 2.0, 0.08);
                }
            }
        }
        // quarter-second melody notes an octave up
        let mut i = 0usize;
        while (i as f64 + 1.0) * 0.25 <= secs {
            let k = i % 14;
            let deg = if k < 7 { k } else { 13 - k };
            let t = i as f64 * 0.25;
            note(&mut buf, base + 12 + scale[deg], t, t + 0.25, 0.06);
            i += 1;
        }
        buf
    }

    const NAMES: [&str; 12] = [
        "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
    ];

    #[test]
    fn rhythm_keys() {
        let cases = [
            (0u8, false, "8B"), // C major
            (9, true, "8A"),    // A minor
            (6, false, "2B"),   // F# major
            (3, true, "2A"),    // Eb minor
            (7, false, "9B"),   // G major
            (2, true, "7A"),    // D minor
            (10, false, "6B"),  // Bb major
            (4, true, "9A"),    // E minor
        ];
        for (tonic, minor, camelot) in cases {
            let r = analyze(&progression(tonic, minor));
            println!(
                "{}{}: got {}{} ({}) conf {:.2}",
                NAMES[tonic as usize],
                if minor { "m" } else { "" },
                NAMES[r.key as usize],
                if r.minor { "m" } else { "" },
                r.camelot(),
                r.key_confidence
            );
            assert_eq!((r.key, r.minor), (tonic, minor));
            assert_eq!(r.camelot(), camelot);
            assert!(r.key_confidence > 0.1);
        }
    }

    #[test]
    fn rhythm_key_detuned() {
        // ~a quarter of a semitone sharp (vinyl rips, old recordings)
        let buf = progression(7, false);
        let sharp: Vec<f32> = (0..buf.len() * 1000 / 1015)
            .map(|i| buf[i * 1015 / 1000])
            .collect();
        let r = analyze(&sharp);
        println!(
            "G +26 cents: got {}{} conf {:.2}",
            NAMES[r.key as usize],
            if r.minor { "m" } else { "" },
            r.key_confidence
        );
        assert_eq!((r.key, r.minor), (7, false));
    }

    #[test]
    fn rhythm_camelot() {
        let r = |key, minor| Rhythm {
            bpm: 0.0,
            beat_confidence: 0.0,
            beats: vec![],
            downbeat: 0,
            key,
            minor,
            key_confidence: 0.0,
            energy: vec![],
        };
        let major: Vec<String> = (0..12).map(|k| r(k, false).camelot()).collect();
        assert_eq!(
            major,
            ["8B", "3B", "10B", "5B", "12B", "7B", "2B", "9B", "4B", "11B", "6B", "1B"]
        );
        let minor: Vec<String> = (0..12).map(|k| r(k, true).camelot()).collect();
        assert_eq!(
            minor,
            ["5A", "12A", "7A", "2A", "9A", "4A", "11A", "6A", "1A", "8A", "3A", "10A"]
        );
    }

    /// Five minutes at 128 BPM over an A minor progression: speed, blob size and round trip.
    #[test]
    fn rhythm_blob_and_speed() {
        let secs = 300.0;
        let (mut buf, truth) = drums(&steady(128.0, secs), secs, 0.0, Some(0));
        let chords = progression(9, true);
        for (i, s) in buf.iter_mut().enumerate() {
            *s += chords[i % chords.len()];
        }
        let t = Instant::now();
        let r = analyze(&buf);
        let took = t.elapsed();
        let blob = r.to_blob();
        let (worst, _, found) = beat_errors(&r.beats, &truth);
        println!(
            "5 min: {took:?} ({} beats, {} bars, {} byte blob, {} BPM, worst {:.1} ms, found {:.0}%, key {})",
            r.beats.len(),
            r.energy.len(),
            blob.len(),
            r.bpm,
            worst * 1000.0,
            found * 100.0,
            r.camelot()
        );
        assert!(blob.len() < 4096, "{}", blob.len());
        assert!((r.bpm - 128.0).abs() <= 0.1);
        assert_eq!(r.camelot(), "8A");
        assert_eq!(Rhythm::from_blob(&blob), Some(r.clone()));
        // damaged or foreign blobs
        assert_eq!(Rhythm::from_blob(&[]), None);
        assert_eq!(Rhythm::from_blob(&blob[..blob.len() - 1]), None);
        let mut other = blob.clone();
        other[0] = 99;
        assert_eq!(Rhythm::from_blob(&other), None);
        // long gaps and unsorted beats take the escape
        let odd = Rhythm {
            beats: vec![1.0, 100.0, 50.5, 50.501],
            ..r
        };
        assert_eq!(Rhythm::from_blob(&odd.to_blob()), Some(odd));
        // generous: debug builds run rustfft unoptimised
        let limit = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
        assert!(took.as_secs_f64() < limit, "{took:?}");
    }

    #[test]
    fn rhythm_energy_per_bar() {
        // 16 bars loud, then 16 bars 20 dB quieter
        let (mut buf, _) = drums(&steady(120.0, 64.0), 64.0, 0.0, Some(0));
        for s in &mut buf[32 * SRU..] {
            *s *= 0.1;
        }
        let r = analyze(&buf);
        let bars: Vec<f32> = r.bars().collect();
        assert_eq!(bars.len(), r.energy.len());
        let mean_where = |keep: &dyn Fn(f32) -> bool| {
            let v: Vec<f32> = r
                .energy
                .iter()
                .zip(&bars)
                .filter(|(_, &t)| keep(t))
                .map(|(e, _)| *e)
                .collect();
            v.iter().sum::<f32>() / v.len() as f32
        };
        let (loud, quiet) = (mean_where(&|t| t < 30.0), mean_where(&|t| t > 32.5));
        println!("loud {loud:.1} dB, quiet {quiet:.1} dB");
        assert!((loud - quiet - 20.0).abs() < 1.0);
    }

    /// Real decoded audio: an ffmpeg-generated metronome, resampled by ffmpeg the way
    /// `sonic` decodes with it.  Skipped without ffmpeg.
    #[test]
    fn rhythm_ffmpeg_metronome() {
        // 125 BPM: a 1 kHz click every 0.48 s, and a low tone on every 4th (bar starts)
        let expr = "sin(2*PI*1000*t)*exp(-mod(t\\,0.48)*120)*0.6\
                    +sin(2*PI*80*t)*exp(-mod(t\\,1.92)*30)*0.5";
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc={expr}:s=44100:d=45"))
            .args(["-f", "f32le", "-ac", "1", "-ar", "22050", "pipe:1"])
            .output();
        let Ok(out) = out else {
            eprintln!("ffmpeg not found: skipped");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let buf: Vec<f32> = out
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert!(buf.len() > 40 * SRU);
        let r = analyze(&buf);
        let truth: Vec<f64> = (0..94).map(|k| k as f64 * 0.48).collect();
        let (worst, mean, found) = beat_errors(&r.beats, &truth);
        let d = r.beats[r.downbeat as usize] as f64;
        println!(
            "ffmpeg 125 BPM: got {} conf {:.2}, worst {:.1} ms, mean {:+.1} ms, found {:.0}%, first bar at {d}",
            r.bpm,
            r.beat_confidence,
            worst * 1000.0,
            mean * 1000.0,
            found * 100.0,
        );
        assert!((r.bpm - 125.0).abs() <= 0.1, "{}", r.bpm);
        assert!(worst <= 0.015);
        // bars start with the low tone, every 1.92 s
        assert!((d / 1.92 - (d / 1.92).round()).abs() < 0.02, "downbeat {d}");
    }
}
