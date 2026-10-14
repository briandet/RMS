//! What kind of music a track is, by [Essentia](https://essentia.upf.edu/models.html)'s
//! Discogs-EffNet model: a neural network trained on millions of tracks labelled with Discogs'
//! 400 styles.  For each track it gives
//!
//! * an *embedding*: 1280 numbers, close for tracks of the same kind (style, instruments,
//!   production, mood), which the mixer uses next to bliss's sound ([`StyleIndex`]);
//! * its most likely Discogs *styles* ("Electronic---Techno");
//! * from small *heads* trained on the embeddings: moods (happy, sad, relaxed, aggressive,
//!   party, acoustic, electronic), danceability, voice or instrumental, approachability and
//!   engagement, and its most likely MTG-Jamendo themes and instruments.
//!
//! The models are MTG's, licensed CC BY-NC-SA 4.0: not part of RMS, they are downloaded when
//! the feature is turned on ([`FILES`]) and run here with tract (pure Rust).  The model's input
//! is made as Essentia's `TensorflowInputMusiCNN` makes it: 16 kHz mono, 512-sample Hann frames
//! every 256 samples (the first centred on the start), the power spectrum through 96 Slaney mel
//! bands of unit area up to 8 kHz, then log10(1 + 10000·x); 128 frames (2 s) a patch.  Up to
//! [`PATCHES`] patches spread over the track are analysed and their results averaged (the
//! average of all of them differs by less than 0.5% in cosine).

use half::f16;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tract_onnx::prelude::*;

/// Stored with each analysis; others are redone.
pub const VERSION: i64 = 1;
/// Embedding size.
pub const DIM: usize = 1280;
/// Patches analysed per track, spread evenly over it.
pub const PATCHES: usize = 32;
const RATE: u32 = 16000;
const FRAME: usize = 512;
const HOP: usize = 256;
const BANDS: usize = 96;
const PATCH: usize = 128;
const PATCH_HOP: usize = 62;

/// Where the models come from.
pub const BASE_URL: &str = "https://essentia.upf.edu/models/";
const EFFNET: &str = "discogs-effnet-bsdynamic-1.onnx";
const EFFNET_JSON: &str = "discogs-effnet-bs64-1.json";

/// What a head gives.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// The probability of this class.
    Class(&'static str),
    /// A value (regression).
    Value,
    /// Its most likely labels.
    Tags,
}

/// The heads: (key, file stem under classification-heads/, kind).
const HEADS: &[(&str, &str, Kind)] = &[
    ("happy", "mood_happy/mood_happy", Kind::Class("happy")),
    ("sad", "mood_sad/mood_sad", Kind::Class("sad")),
    (
        "relaxed",
        "mood_relaxed/mood_relaxed",
        Kind::Class("relaxed"),
    ),
    (
        "aggressive",
        "mood_aggressive/mood_aggressive",
        Kind::Class("aggressive"),
    ),
    ("party", "mood_party/mood_party", Kind::Class("party")),
    (
        "acoustic",
        "mood_acoustic/mood_acoustic",
        Kind::Class("acoustic"),
    ),
    (
        "electronic",
        "mood_electronic/mood_electronic",
        Kind::Class("electronic"),
    ),
    (
        "danceable",
        "danceability/danceability",
        Kind::Class("danceable"),
    ),
    (
        "voice",
        "voice_instrumental/voice_instrumental",
        Kind::Class("voice"),
    ),
    (
        "approachability",
        "approachability/approachability_regression",
        Kind::Value,
    ),
    (
        "engagement",
        "engagement/engagement_regression",
        Kind::Value,
    ),
    (
        "themes",
        "mtg_jamendo_moodtheme/mtg_jamendo_moodtheme",
        Kind::Tags,
    ),
    (
        "instruments",
        "mtg_jamendo_instrument/mtg_jamendo_instrument",
        Kind::Tags,
    ),
];

/// Styles and tags kept: the most likely few, if likely enough.
const TOP: usize = 5;
const MIN_STYLE: f32 = 0.1;
const MIN_TAG: f32 = 0.1;

/// The files to download: (local name, path under [`BASE_URL`]).
pub fn files() -> Vec<(String, String)> {
    let mut out = vec![
        (
            EFFNET.to_string(),
            format!("feature-extractors/discogs-effnet/{EFFNET}"),
        ),
        (
            EFFNET_JSON.to_string(),
            format!("feature-extractors/discogs-effnet/{EFFNET_JSON}"),
        ),
    ];
    for (_, stem, _) in HEADS {
        let name = stem.rsplit('/').next().unwrap_or(stem);
        for ext in ["pb", "json"] {
            out.push((
                format!("{name}-discogs-effnet-1.{ext}"),
                format!("classification-heads/{stem}-discogs-effnet-1.{ext}"),
            ));
        }
    }
    out
}

type Plan = TypedRunnableModel<TypedModel>;

struct Head {
    key: &'static str,
    kind: Kind,
    classes: Vec<String>,
    plan: Plan,
}

/// The loaded models.
pub struct Models {
    effnet: Plan,
    styles: Vec<String>,
    heads: Vec<Head>,
}

#[derive(Deserialize)]
struct Meta {
    classes: Vec<String>,
}

fn classes(path: &Path) -> anyhow::Result<Vec<String>> {
    let m: Meta = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(m.classes)
}

impl Models {
    /// Load the models from `dir` (where [`files`] were downloaded).
    pub fn load(dir: &Path) -> anyhow::Result<Models> {
        let model = tract_onnx::onnx().model_for_path(dir.join(EFFNET))?;
        let batch = model.symbols.sym("batch_size");
        let effnet = model
            .into_typed()?
            .concretize_dims(&SymbolValues::default().with(&batch, 1))?
            .into_optimized()?
            .into_runnable()?;
        let styles = classes(&dir.join(EFFNET_JSON))?;
        let mut heads = Vec::new();
        for (key, stem, kind) in HEADS {
            let name = stem.rsplit('/').next().unwrap_or(stem);
            let plan = tract_tensorflow::tensorflow()
                .model_for_path(dir.join(format!("{name}-discogs-effnet-1.pb")))?
                .with_input_fact(0, f32::fact([1, DIM]).into())?
                .into_optimized()?
                .into_runnable()?;
            let classes = classes(&dir.join(format!("{name}-discogs-effnet-1.json")))?;
            if let Kind::Class(c) = kind {
                anyhow::ensure!(classes.iter().any(|x| x == c), "{name}: no class {c}");
            }
            heads.push(Head {
                key,
                kind: *kind,
                classes,
                plan,
            });
        }
        Ok(Models {
            effnet,
            styles,
            heads,
        })
    }
}

/// What the models say about a track.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Labels {
    /// Most likely Discogs styles ("Genre---Style"), with their probabilities.
    pub styles: Vec<(String, f32)>,
    /// Moods and qualities, 0..1 (approachability and engagement as the heads give them).
    pub moods: Vec<(String, f32)>,
    /// Most likely MTG-Jamendo themes ("dark", "christmas", …) and instruments.
    pub themes: Vec<(String, f32)>,
    pub instruments: Vec<(String, f32)>,
}

impl Labels {
    pub fn mood(&self, key: &str) -> Option<f32> {
        self.moods.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }
}

/// A track analysed.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// The average embedding of the patches analysed.
    pub embedding: Vec<f32>,
    pub labels: Labels,
}

/// Analyse mono audio at `rate` Hz (`None` when shorter than a second).
pub fn analyze(models: &Models, samples: &[f32], rate: u32) -> anyhow::Result<Option<Analysis>> {
    if samples.len() < rate as usize {
        return Ok(None);
    }
    let n16 = (samples.len() as u64 * RATE as u64 / rate as u64) as usize;
    let frames = frames(n16);
    let patches = if frames >= PATCH {
        (frames - PATCH) / PATCH_HOP + 1
    } else {
        1
    };
    let k = PATCHES.min(patches);
    let resampler = Resampler::new(rate);
    let filters = filterbank();
    let window = hann();
    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(FRAME);

    let mut embedding = vec![0f32; DIM];
    let mut styles = vec![0f32; models.styles.len()];
    let mut heads: Vec<Vec<f32>> = models
        .heads
        .iter()
        .map(|h| vec![0f32; h.classes.len()])
        .collect();
    for i in 0..k {
        let p = if k == 1 {
            0
        } else {
            (i as f64 * (patches - 1) as f64 / (k - 1) as f64).round() as usize
        };
        // the 16 kHz samples this patch's frames cover
        let first = (p * PATCH_HOP) as i64 * HOP as i64 - (FRAME / 2) as i64;
        let len = (PATCH - 1) * HOP + FRAME;
        let audio = resampler.range(samples, first, len, n16);
        let mel = mel(&audio, PATCH, &filters, &window, fft.as_ref());
        let input = tract_ndarray::Array3::from_shape_vec((1, PATCH, BANDS), mel)?.into_tensor();
        let out = models.effnet.run(tvec!(input.into()))?;
        let (act, emb) = (
            out[0].to_array_view::<f32>()?,
            out[1].to_array_view::<f32>()?,
        );
        for (a, v) in styles.iter_mut().zip(act.iter()) {
            *a += v / k as f32;
        }
        let emb: Vec<f32> = emb.iter().copied().collect();
        for (a, v) in embedding.iter_mut().zip(&emb) {
            *a += v / k as f32;
        }
        // the heads, on each patch's embedding (as Essentia's examples apply them)
        let x = tract_ndarray::Array2::from_shape_vec((1, DIM), emb)?.into_tensor();
        for (h, acc) in models.heads.iter().zip(heads.iter_mut()) {
            let out = h.plan.run(tvec!(x.clone().into()))?;
            for (a, v) in acc.iter_mut().zip(out[0].to_array_view::<f32>()?.iter()) {
                *a += v / k as f32;
            }
        }
    }

    let top = |names: &[String], probs: &[f32], min: f32| -> Vec<(String, f32)> {
        let mut v: Vec<(String, f32)> = names
            .iter()
            .cloned()
            .zip(probs.iter().copied())
            .filter(|(_, p)| *p >= min)
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.truncate(TOP);
        v
    };
    let mut labels = Labels {
        styles: top(&models.styles, &styles, MIN_STYLE),
        ..Default::default()
    };
    for (h, out) in models.heads.iter().zip(&heads) {
        match h.kind {
            Kind::Class(c) => {
                let i = h.classes.iter().position(|x| x == c).unwrap_or(0);
                labels.moods.push((h.key.to_string(), out[i]));
            }
            Kind::Value => labels.moods.push((h.key.to_string(), out[0])),
            Kind::Tags => {
                let tags = top(&h.classes, out, MIN_TAG);
                if h.key == "instruments" {
                    labels.instruments = tags;
                } else {
                    labels.themes = tags;
                }
            }
        }
    }
    Ok(Some(Analysis { embedding, labels }))
}

// ---------------------------------------------------------------- the input

/// Frames Essentia cuts from `n` samples: the first centred on the start, then every hop
/// while one starts before the end.
fn frames(n: usize) -> usize {
    (n + 2 * HOP - 1) / HOP
}

/// Essentia's Hann window (symmetric).
fn hann() -> Vec<f32> {
    (0..FRAME)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (FRAME - 1) as f64).cos()) as f32)
        .collect()
}

fn hz_to_mel(f: f64) -> f64 {
    // Slaney: linear below 1 kHz, logarithmic above
    let min_log_mel = 1000.0 / (200.0 / 3.0);
    if f < 1000.0 {
        f / (200.0 / 3.0)
    } else {
        min_log_mel + (f / 1000.0).ln() / ((6.4f64).ln() / 27.0)
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let min_log_mel = 1000.0 / (200.0 / 3.0);
    if m < min_log_mel {
        m * (200.0 / 3.0)
    } else {
        1000.0 * ((m - min_log_mel) * ((6.4f64).ln() / 27.0)).exp()
    }
}

/// The mel filterbank: [`BANDS`] triangles of unit area over the FFT bins, Slaney scale,
/// 0 to 8 kHz.
fn filterbank() -> Vec<Vec<f32>> {
    let bins = FRAME / 2 + 1;
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(RATE as f64 / 2.0));
    let edges: Vec<f64> = (0..BANDS + 2)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (BANDS + 1) as f64))
        .collect();
    let df = RATE as f64 / FRAME as f64;
    (0..BANDS)
        .map(|b| {
            let (l, c, h) = (edges[b], edges[b + 1], edges[b + 2]);
            let norm = 2.0 / (h - l);
            (0..bins)
                .map(|j| {
                    let f = j as f64 * df;
                    let w = if f > l && f <= c {
                        (f - l) / (c - l)
                    } else if f > c && f < h {
                        (h - f) / (h - c)
                    } else {
                        0.0
                    };
                    (w * norm) as f32
                })
                .collect()
        })
        .collect()
}

/// `frames` frames of log-compressed mel bands from `audio` (frame `i` starts at sample
/// `i * HOP`), row by row.
fn mel(
    audio: &[f32],
    frames: usize,
    filters: &[Vec<f32>],
    window: &[f32],
    fft: &dyn rustfft::Fft<f32>,
) -> Vec<f32> {
    use rustfft::num_complex::Complex32;
    let mut buf = vec![Complex32::new(0.0, 0.0); FRAME];
    let mut power = vec![0f32; FRAME / 2 + 1];
    let mut out = Vec::with_capacity(frames * BANDS);
    for k in 0..frames {
        for (i, b) in buf.iter_mut().enumerate() {
            let x = audio.get(k * HOP + i).copied().unwrap_or(0.0);
            *b = Complex32::new(x * window[i], 0.0);
        }
        fft.process(&mut buf);
        for (j, p) in power.iter_mut().enumerate() {
            *p = buf[j].norm_sqr();
        }
        for f in filters {
            let e: f32 = f.iter().zip(&power).map(|(w, p)| w * p).sum();
            out.push((1.0 + 10000.0 * e).log10());
        }
    }
    out
}

/// Resampling to 16 kHz with a windowed sinc (Kaiser), a filter per output phase.
struct Resampler {
    up: usize,
    down: usize,
    filters: Vec<Vec<f32>>,
}

const TAPS: usize = 48;

impl Resampler {
    fn new(from: u32) -> Resampler {
        let g = gcd(RATE as usize, from as usize);
        let (up, down) = (RATE as usize / g, from as usize / g);
        let cutoff = 0.5 * (RATE as f64 / from as f64).min(1.0) * 0.99;
        let beta = 8.6;
        let i0 = |x: f64| {
            let (mut s, mut t) = (1.0, 1.0);
            for k in 1..30 {
                t *= (x / 2.0) / k as f64;
                s += t * t;
            }
            s
        };
        let filters = (0..up)
            .map(|p| {
                let frac = (p * down % up) as f64 / up as f64;
                (0..2 * TAPS)
                    .map(|t| {
                        let x = t as f64 - (TAPS as f64 - 1.0) - frac;
                        let sinc = if x.abs() < 1e-9 {
                            2.0 * cutoff
                        } else {
                            (std::f64::consts::TAU * cutoff * x).sin() / (std::f64::consts::PI * x)
                        };
                        let r = x / TAPS as f64;
                        let w = if r.abs() <= 1.0 {
                            i0(beta * (1.0 - r * r).sqrt()) / i0(beta)
                        } else {
                            0.0
                        };
                        (sinc * w) as f32
                    })
                    .collect()
            })
            .collect();
        Resampler { up, down, filters }
    }

    /// 16 kHz samples `first..first + len` of `samples` (silence outside `0..total`).
    fn range(&self, samples: &[f32], first: i64, len: usize, total: usize) -> Vec<f32> {
        (0..len as i64)
            .map(|i| {
                let o = first + i;
                if o < 0 || o as usize >= total {
                    return 0.0;
                }
                let o = o as usize;
                if self.up == self.down {
                    return samples.get(o).copied().unwrap_or(0.0);
                }
                let pos = o * self.down;
                let base = (pos / self.up) as i64 - (TAPS as i64 - 1);
                let f = &self.filters[o % self.up];
                let mut acc = 0f32;
                for (t, w) in f.iter().enumerate() {
                    let j = base + t as i64;
                    if j >= 0 && (j as usize) < samples.len() {
                        acc += samples[j as usize] * w;
                    }
                }
                acc
            })
            .collect()
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

// ---------------------------------------------------------------- storage

/// Store an analysis (`None`: the audio could not be analysed, not to be tried again).
pub(crate) fn store(
    c: &Connection,
    fingerprint: &str,
    a: Option<&Analysis>,
) -> rusqlite::Result<()> {
    let embedding: Option<Vec<u8>> = a.map(|a| {
        a.embedding
            .iter()
            .flat_map(|x| f16::from_f32(*x).to_le_bytes())
            .collect()
    });
    let labels = a.map(|a| serde_json::to_string(&a.labels).unwrap_or_default());
    c.execute(
        "INSERT OR REPLACE INTO track_effnet (fingerprint, version, embedding, labels)
         VALUES (?, ?, ?, ?)",
        params![fingerprint, VERSION, embedding, labels],
    )?;
    Ok(())
}

fn embedding_from(blob: &[u8]) -> Option<Vec<f32>> {
    (blob.len() == DIM * 2).then(|| {
        blob.chunks_exact(2)
            .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect()
    })
}

impl crate::Library {
    /// The loaded EffNet models, when the feature is on.
    pub fn effnet(&self) -> Option<Arc<Models>> {
        self.inner.effnet.read().clone()
    }

    /// Turn EffNet analysis on (with the models) or off.
    pub fn set_effnet(&self, models: Option<Arc<Models>>) {
        *self.inner.effnet.write() = models;
    }

    /// A track's labels (by its id), if analysed.
    pub fn effnet_labels(&self, track: i64) -> Option<Labels> {
        let c = self.conn().ok()?;
        let s: Option<String> = c
            .query_row(
                "SELECT e.labels FROM tracks t JOIN track_effnet e ON e.fingerprint = t.fingerprint
                 WHERE t.id = ? AND e.version = ?",
                params![track, VERSION],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        serde_json::from_str(&s?).ok()
    }

    /// Tracks analysed with EffNet, and still to analyse (local tracks).
    pub fn effnet_totals(&self) -> (i64, i64) {
        let Ok(c) = self.conn() else { return (0, 0) };
        c.query_row(
            "SELECT COUNT(DISTINCT t.fingerprint) FILTER (WHERE e.fingerprint IS NOT NULL),
                    COUNT(DISTINCT t.fingerprint) FILTER (WHERE e.fingerprint IS NULL)
             FROM tracks t
             LEFT JOIN track_effnet e ON e.fingerprint = t.fingerprint AND e.version = ?
             WHERE t.fingerprint IS NOT NULL",
            [VERSION],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0))
    }

    /// The index of every analysed track's embedding.  Loaded once and kept up to date.
    pub fn style_index(&self) -> Arc<StyleIndex> {
        if let Some(ix) = self.inner.style.read().as_ref() {
            return ix.clone();
        }
        let mut slot = self.inner.style.write();
        if let Some(ix) = slot.as_ref() {
            return ix.clone();
        }
        let Ok(c) = self.conn() else {
            return Arc::new(StyleIndex::default());
        };
        let ix = Arc::new(StyleIndex::load(&c));
        *slot = Some(ix.clone());
        ix
    }

    /// Store an analysis and add it to the index.
    pub(crate) fn store_effnet(&self, c: &Connection, fingerprint: &str, a: Option<&Analysis>) {
        if store(c, fingerprint, a).is_err() {
            return;
        }
        let Some(a) = a else { return };
        let ids: Vec<i64> = c
            .prepare("SELECT id FROM tracks WHERE fingerprint = ?")
            .and_then(|mut st| {
                st.query_map([fingerprint], |r| r.get(0))
                    .map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        let remote: Vec<i64> = c
            .prepare("SELECT id FROM remote_tracks WHERE fingerprint = ?")
            .and_then(|mut st| {
                st.query_map([fingerprint], |r| r.get::<_, i64>(0))
                    .map(|rows| rows.flatten().map(crate::remote::remote_id).collect())
            })
            .unwrap_or_default();
        let mut slot = self.inner.style.write();
        if let Some(ix) = slot.as_mut() {
            let ix = Arc::make_mut(ix);
            for id in ids.into_iter().chain(remote) {
                ix.add(id, &a.embedding);
            }
        }
    }

    /// Forget the index (a scan changed the tracks' ids).
    pub(crate) fn reset_style_index(&self) {
        *self.inner.style.write() = None;
    }
}

// ---------------------------------------------------------------- similarity

/// Every analysed track's embedding, quantised to bytes (each scaled to its largest
/// component), for cosine similarity.
#[derive(Debug, Clone, Default)]
pub struct StyleIndex {
    ids: Vec<i64>,
    pos: std::collections::HashMap<i64, usize>,
    vectors: Vec<i8>,
    /// length of each quantised vector
    norms: Vec<f32>,
}

/// How close tracks are in kind to some seeds.
pub struct Closeness {
    /// Analysed tracks, the closest first.
    pub nearest: Vec<i64>,
    /// track → 1 (the closest) .. 0 (the farthest)
    pub rank: std::collections::HashMap<i64, f64>,
}

impl StyleIndex {
    fn load(c: &Connection) -> StyleIndex {
        let mut ix = StyleIndex::default();
        let rows: Vec<(i64, Vec<u8>)> = c
            .prepare(
                "SELECT t.id, e.embedding FROM tracks t
                 JOIN track_effnet e ON e.fingerprint = t.fingerprint
                 WHERE e.version = ?1 AND e.embedding IS NOT NULL
                 UNION ALL
                 SELECT -r.id, e.embedding FROM remote_tracks r
                 JOIN track_effnet e ON e.fingerprint = r.fingerprint
                 WHERE e.version = ?1 AND e.embedding IS NOT NULL",
            )
            .and_then(|mut st| {
                st.query_map([VERSION], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map(|rows| rows.flatten().collect())
            })
            .unwrap_or_default();
        for (id, blob) in rows {
            if let Some(e) = embedding_from(&blob) {
                ix.add(id, &e);
            }
        }
        ix
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Add a track, or replace its embedding.
    pub fn add(&mut self, id: i64, embedding: &[f32]) {
        if embedding.len() != DIM {
            return;
        }
        let max = embedding.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9);
        let q: Vec<i8> = embedding
            .iter()
            .map(|x| (x / max * 127.0).round() as i8)
            .collect();
        let norm = (Self::dot(&q, &q) as f32).sqrt().max(1.0);
        match self.pos.get(&id) {
            Some(&i) => {
                self.vectors[i * DIM..(i + 1) * DIM].copy_from_slice(&q);
                self.norms[i] = norm;
            }
            None => {
                self.pos.insert(id, self.ids.len());
                self.ids.push(id);
                self.vectors.extend_from_slice(&q);
                self.norms.push(norm);
            }
        }
    }

    fn row(&self, i: usize) -> &[i8] {
        &self.vectors[i * DIM..(i + 1) * DIM]
    }

    fn dot(a: &[i8], b: &[i8]) -> i32 {
        a.iter().zip(b).map(|(x, y)| *x as i32 * *y as i32).sum()
    }

    /// How close every analysed track is to the closest of `seeds` (`None` when no seed is
    /// analysed).
    pub fn closeness(&self, seeds: &[i64]) -> Option<Closeness> {
        let seeds: Vec<usize> = seeds
            .iter()
            .filter_map(|s| self.pos.get(s).copied())
            .collect();
        if seeds.is_empty() {
            return None;
        }
        let mut sims: Vec<(i64, f32)> = (0..self.ids.len())
            .map(|i| {
                let best = seeds
                    .iter()
                    .map(|s| self.cosine(*s, i))
                    .fold(f32::MIN, f32::max);
                (self.ids[i], best)
            })
            .collect();
        sims.sort_by(|a, b| b.1.total_cmp(&a.1));
        let n = sims.len().max(2) as f64 - 1.0;
        let rank = sims
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, 1.0 - i as f64 / n))
            .collect();
        Some(Closeness {
            nearest: sims.into_iter().map(|(id, _)| id).collect(),
            rank,
        })
    }

    fn cosine(&self, i: usize, j: usize) -> f32 {
        Self::dot(self.row(i), self.row(j)) as f32 / (self.norms[i] * self.norms[j])
    }

    /// The `n` library tracks closest in kind to `id`, closest first, with their cosine
    /// similarity (empty when `id` isn't analysed).
    pub fn nearest(&self, id: i64, n: usize) -> Vec<(i64, f32)> {
        let Some(&me) = self.pos.get(&id) else {
            return Vec::new();
        };
        let mut all: Vec<(i64, f32)> = (0..self.ids.len())
            .filter(|&i| i != me && self.ids[i] > 0)
            .map(|i| (self.ids[i], self.cosine(me, i)))
            .collect();
        all.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        all.truncate(n);
        all
    }

    /// Cosine similarity of two analysed tracks.
    pub fn similarity(&self, a: i64, b: i64) -> Option<f32> {
        Some(self.cosine(*self.pos.get(&a)?, *self.pos.get(&b)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_as_essentia_cuts_them() {
        // counts Essentia gave for three real tracks at 16 kHz
        assert_eq!(frames(4_664_890), 18_224);
        assert_eq!(frames(8_680_083), 33_908);
        assert_eq!(frames(3_482_437), 13_605);
    }

    #[test]
    fn the_filterbank_is_essentias() {
        // weights probed from Essentia's MelBands (slaneyMel, unit_tri, 96 bands, 0–8 kHz)
        let f = filterbank();
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(close(f[0][1], 0.031_999_2), "{}", f[0][1]);
        assert!(close(f[1][2], 0.031_840_7), "{}", f[1][2]);
        assert!(close(f[2][3], 0.031_682_1), "{}", f[2][3]);
        assert!(close(f[47][53], 0.016_686_8), "{}", f[47][53]);
        assert!(close(f[95][248], 0.003_984_46), "{}", f[95][248]);
        // band 0 covers bin 1 only, the last band bins 241..255
        assert_eq!(f[0].iter().filter(|w| **w > 0.0).count(), 1);
        let last: Vec<usize> = (0..257).filter(|j| f[95][*j] > 0.0).collect();
        assert_eq!((last[0], *last.last().unwrap()), (241, 255));
        // unit area: each band sums to 1 / bin width
        for b in [0, 47, 95] {
            let s: f32 = f[b].iter().sum();
            assert!((s * 31.25 - 1.0).abs() < 0.03, "band {b}: {s}");
        }
    }

    #[test]
    fn the_window_is_essentias() {
        let w = hann();
        // Essentia's Windowing(normalized=false) sums to 255.5 over 512 samples
        assert!((w.iter().sum::<f32>() - 255.5).abs() < 1e-3);
        assert_eq!(w[0], 0.0);
    }

    #[test]
    fn mel_of_a_tone_peaks_in_its_band() {
        // 1 kHz at 16 kHz: the band whose centre is nearest 1 kHz is the loudest
        let audio: Vec<f32> = (0..FRAME)
            .map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / RATE as f32).sin())
            .collect();
        let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let m = mel(&audio, 1, &filterbank(), &hann(), fft.as_ref());
        let loudest = (0..BANDS).max_by(|a, b| m[*a].total_cmp(&m[*b])).unwrap();
        let centre = |b: usize| {
            let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(8000.0));
            mel_to_hz(lo + (hi - lo) * (b + 1) as f64 / (BANDS + 1) as f64)
        };
        assert!(
            (centre(loudest) - 1000.0).abs() < 60.0,
            "band {loudest} at {}",
            centre(loudest)
        );
    }

    #[test]
    fn resampling_keeps_a_tone_and_its_level() {
        // 1 kHz at 22.05 kHz → 16 kHz: still 1 kHz, same amplitude
        let from = 22050;
        let src: Vec<f32> = (0..from)
            .map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / from as f32).sin())
            .collect();
        let r = Resampler::new(from);
        let out = r.range(&src, 0, RATE as usize, RATE as usize);
        let mid = &out[4000..12000];
        let peak = mid.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 1.0).abs() < 0.01, "{peak}");
        // compare with the tone sampled at 16 kHz directly
        let err = mid
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let t = (i + 4000) as f32 / RATE as f32;
                (x - (std::f32::consts::TAU * 1000.0 * t).sin()).abs()
            })
            .fold(0f32, f32::max);
        assert!(err < 0.01, "{err}");
        // and silence before the start
        assert_eq!(r.range(&src, -10, 5, RATE as usize), vec![0.0; 5]);
    }

    /// Against Essentia and ONNX Runtime on real music: with `RMS_EFFNET_MODELS` (the downloaded
    /// files) and `RMS_EFFNET_REF` (a folder of `<name>.a22.npy`, mono f32 at 22.05 kHz, and
    /// `<name>.emb.npy`, the reference's per-patch embeddings of the same audio at 16 kHz).
    #[test]
    #[ignore]
    fn matches_essentia_on_real_music() {
        let (Ok(models), Ok(refs)) = (
            std::env::var("RMS_EFFNET_MODELS"),
            std::env::var("RMS_EFFNET_REF"),
        ) else {
            return;
        };
        let npy = |p: &Path| -> Vec<f32> {
            let b = std::fs::read(p).unwrap();
            let hl = u16::from_le_bytes([b[8], b[9]]) as usize;
            b[10 + hl..]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        };
        let m = Models::load(Path::new(&models)).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&refs)
            .unwrap()
            .filter_map(|e| {
                e.ok()?
                    .file_name()
                    .to_str()?
                    .strip_suffix(".a22.npy")
                    .map(str::to_string)
            })
            .collect();
        names.sort();
        assert!(!names.is_empty());
        for n in names {
            let audio = npy(&Path::new(&refs).join(format!("{n}.a22.npy")));
            let per_patch = npy(&Path::new(&refs).join(format!("{n}.emb.npy")));
            let patches = per_patch.len() / DIM;
            let full: Vec<f32> = (0..DIM)
                .map(|j| (0..patches).map(|i| per_patch[i * DIM + j]).sum::<f32>() / patches as f32)
                .collect();
            let t = std::time::Instant::now();
            let a = analyze(&m, &audio, 22050).unwrap().unwrap();
            let took = t.elapsed();
            let dot: f32 = a.embedding.iter().zip(&full).map(|(x, y)| x * y).sum();
            let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let cos = dot / (norm(&a.embedding) * norm(&full));
            println!(
                "{n}: cosine {cos:.4} in {took:?}\n  {:?}\n  {:?}\n  {:?} {:?}",
                a.labels.styles, a.labels.moods, a.labels.themes, a.labels.instruments
            );
            assert!(cos > 0.98, "{n}: {cos}");
        }
    }

    #[test]
    fn the_style_index_ranks_by_cosine() {
        let mut ix = StyleIndex::default();
        let v =
            |i: usize| -> Vec<f32> { (0..DIM).map(|j| ((i * 7 + j) % 13) as f32 - 6.0).collect() };
        let seed = v(1);
        let mut near = seed.clone();
        near[0] += 3.0;
        ix.add(1, &seed);
        ix.add(2, &near);
        ix.add(3, &v(5));
        let c = ix.closeness(&[1]).unwrap();
        assert_eq!(c.nearest[..2], [1, 2]);
        assert!(ix.similarity(1, 2).unwrap() > 0.99);
        assert!(ix.similarity(1, 3).unwrap() < ix.similarity(1, 2).unwrap());
        assert_eq!(c.rank[&1], 1.0);
        assert_eq!(c.rank[&3], 0.0);
        assert!(ix.closeness(&[99]).is_none());
        // an embedding stored at half precision comes back
        let blob: Vec<u8> = seed
            .iter()
            .flat_map(|x| f16::from_f32(*x).to_le_bytes())
            .collect();
        assert_eq!(embedding_from(&blob).unwrap(), seed);
    }
}
