//! Sonic analysis with [bliss](https://lelele.io/bliss.html): what a song sounds like (tempo,
//! timbre, loudness, chroma) as a short feature vector, so mixes can pick songs that sound
//! alike rather than only songs that are tagged alike.  Not part of LMS, which relied on the
//! external MusicIP server and now on third-party plugins for this.
//!
//! * [`Library::analyze_pending`] runs in the background after a scan: it decodes each local
//!   track that has no current analysis (in-process with symphonia, or with ffmpeg for formats
//!   symphonia lacks and for cue-sheet tracks) and stores the features in `track_analysis`,
//!   keyed by `urlmd5` with the file's mtime and size so changed files are redone.  The same
//!   pass measures loudness for ReplayGain ([`crate::loudness`]).
//! * [`Index`] holds every analysed track's features in memory for nearest-neighbour search
//!   with bliss's weighted distance; the mixer uses it to rank candidates and to leave out
//!   [near duplicates](Index::near_duplicate).

use crate::loudness::{self, Loudness};
use crate::url::{fragment, path_from_file_url};
use crate::Library;
use bliss_audio::decoder::symphonia::SymphoniaDecoder;
use bliss_audio::decoder::Decoder;
use bliss_audio::{FeaturesVersion, Song};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tracing::{debug, info};

/// Features version stored with each analysis; others are ignored and redone.
pub fn version() -> i64 {
    FeaturesVersion::LATEST as i64
}

/// bliss's sample format: mono f32 at 22050 Hz.
const SAMPLE_RATE: u32 = 22050;
/// Longer tracks (DJ mixes, audiobooks, whole-album files) are not analysed: one feature
/// vector says little about them and decoding them takes a lot of memory.
const MAX_SECS: f64 = 20.0 * 60.0;
/// At most this many seed songs are compared against the library.
const MAX_SEEDS: usize = 50;
/// Tracks closer than this (bliss distance) are taken for the same recording: another
/// encoding, a remaster or a slightly different edit.  Measured on real music: re-encodes and
/// trims 0.01–0.05, a heavy remaster 0.13–0.23, different songs 0.43 and up (median 0.89).
/// Live versions are 0.2–0.8 away, as far as different songs, so they are not caught.
const DUPLICATE_DISTANCE: f32 = 0.12;

/// A track waiting for analysis.
#[derive(Debug, Clone)]
struct Job {
    urlmd5: String,
    url: String,
    path: PathBuf,
    timestamp: Option<i64>,
    filesize: Option<i64>,
    secs: Option<f64>,
}

/// How far the background analysis got.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AnalysisTotals {
    /// local audio tracks
    pub tracks: i64,
    /// tracks with current features
    pub analyzed: i64,
    /// tracks that could not be analysed (until the file changes)
    pub failed: i64,
    /// tracks with a loudness measurement
    pub measured: i64,
}

impl Library {
    /// Analyse every local track lacking a current analysis, on `threads` threads, then fill in
    /// missing ReplayGain from the measurements.  Stops early when `stop` is set or a scan
    /// starts.  `ffmpeg` decodes what symphonia cannot.  Returns the number of tracks analysed.
    pub fn analyze_pending(
        &self,
        threads: usize,
        ffmpeg: Option<&str>,
        stop: &AtomicBool,
    ) -> usize {
        let jobs = match self.conn() {
            Ok(c) => pending(&c),
            Err(_) => return 0,
        };
        // a scan resets the gains of the tracks it (re)stored from their tags
        self.apply_loudness();
        if jobs.is_empty() {
            return 0;
        }
        info!(
            "sonic analysis: {} tracks to analyse on {} threads",
            jobs.len(),
            threads
        );
        let started = std::time::Instant::now();
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let total = jobs.len();
        let halt = || stop.load(Ordering::Relaxed) || self.is_scanning();
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| {
                    let Ok(c) = self.conn() else { return };
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= total || halt() {
                            return;
                        }
                        let job = &jobs[i];
                        let features = analyze(job, ffmpeg);
                        if let Err(e) = &features {
                            debug!("sonic analysis of {}: {}", job.url, e);
                        }
                        let loudness = measure_loudness(job, ffmpeg);
                        if let Err(e) = &loudness {
                            debug!("loudness of {}: {}", job.url, e);
                        }
                        let blob = features.ok().map(|f| to_blob(&f));
                        let loudness = loudness.ok();
                        let ok = blob.is_some() || loudness.is_some();
                        let _ = c.execute(
                            "INSERT OR REPLACE INTO track_analysis \
                               (urlmd5, version, timestamp, filesize, features, loudness, peak, analyzed) \
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                            params![
                                job.urlmd5,
                                version(),
                                job.timestamp,
                                job.filesize,
                                blob,
                                loudness.map(|l| l.lufs),
                                loudness.map(|l| l.peak),
                                crate::scanner::now()
                            ],
                        );
                        if ok {
                            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                            if n.is_multiple_of(250) {
                                info!("sonic analysis: {} of {} tracks", n, total);
                            }
                        }
                    }
                });
            }
        });
        let n = done.load(Ordering::Relaxed);
        info!(
            "sonic analysis: {} tracks analysed in {:.0}s{}",
            n,
            started.elapsed().as_secs_f64(),
            if halt() { " (interrupted)" } else { "" }
        );
        self.apply_loudness();
        n
    }

    /// Analysis progress over the library.
    pub fn analysis_totals(&self) -> AnalysisTotals {
        let Ok(c) = self.conn() else {
            return AnalysisTotals::default();
        };
        c.query_row(
            &format!(
                "SELECT COUNT(*), COUNT(a.features), COUNT(a.urlmd5) - COUNT(a.features), \
                   COUNT(a.loudness) \
                 FROM tracks t LEFT JOIN track_analysis a ON {CURRENT} WHERE {LOCAL}"
            ),
            [version()],
            |r| {
                Ok(AnalysisTotals {
                    tracks: r.get(0)?,
                    analyzed: r.get(1)?,
                    failed: r.get(2)?,
                    measured: r.get(3)?,
                })
            },
        )
        .unwrap_or_default()
    }
}

/// Local audio files (and cue-sheet tracks in them).
const LOCAL: &str = "t.audio = 1 AND IFNULL(t.remote, 0) = 0 AND t.url LIKE 'file:%' \
     AND t.content_type NOT IN ('cpl', 'src', 'ssp', 'dir')";
/// Joins the analysis of `t` that is still valid for its file (bind the version).
const CURRENT: &str = "a.urlmd5 = t.urlmd5 AND a.version = ? \
     AND a.timestamp IS t.timestamp AND a.filesize IS t.filesize";

fn pending(c: &Connection) -> Vec<Job> {
    let sql = format!(
        "SELECT t.urlmd5, t.url, t.timestamp, t.filesize, t.secs FROM tracks t \
         LEFT JOIN track_analysis a ON {CURRENT} WHERE {LOCAL} AND a.urlmd5 IS NULL ORDER BY t.url"
    );
    let Ok(mut st) = c.prepare(&sql) else {
        return Vec::new();
    };
    let rows = st.query_map([version()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<f64>>(4)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|(urlmd5, url, timestamp, filesize, secs)| {
            let path = path_from_file_url(&url)?;
            Some(Job {
                urlmd5,
                url,
                path,
                timestamp,
                filesize,
                secs,
            })
        })
        .collect()
}

fn analyze(job: &Job, ffmpeg: Option<&str>) -> Result<Vec<f32>, String> {
    let slice = fragment(&job.url);
    let secs = slice.map(|(s, e)| e - s).or(job.secs).unwrap_or(0.0);
    if secs > MAX_SECS {
        return Err(format!("too long ({secs:.0}s)"));
    }
    let samples = match slice {
        // a cue-sheet track: only ffmpeg can cut it out without decoding the whole file
        Some((start, end)) => {
            let ff = ffmpeg.ok_or("cue-sheet track needs ffmpeg")?;
            decode_ffmpeg(ff, &job.path, Some((start, end - start)))?
        }
        None => match decode_symphonia(&job.path) {
            Ok(s) => s,
            Err(e) => match ffmpeg {
                Some(ff) => decode_ffmpeg(ff, &job.path, None)?,
                None => return Err(e),
            },
        },
    };
    if samples.len() < SAMPLE_RATE as usize {
        return Err("shorter than a second".into());
    }
    Song::analyze(&samples)
        .map(|a| a.as_vec())
        .map_err(|e| e.to_string())
}

/// Loudness for any length of track: streamed through symphonia, or measured by ffmpeg.
fn measure_loudness(job: &Job, ffmpeg: Option<&str>) -> Result<Loudness, String> {
    match fragment(&job.url) {
        Some((start, end)) => {
            let ff = ffmpeg.ok_or("cue-sheet track needs ffmpeg")?;
            loudness::measure_ffmpeg(ff, &job.path, Some((start, end - start)))
        }
        None => loudness::measure(&job.path).or_else(|e| match ffmpeg {
            Some(ff) => loudness::measure_ffmpeg(ff, &job.path, None),
            None => Err(e),
        }),
    }
}

fn decode_symphonia(path: &Path) -> Result<Vec<f32>, String> {
    // symphonia can panic on malformed files; one bad file must not end the analysis
    std::panic::catch_unwind(|| SymphoniaDecoder::decode(path))
        .map_err(|_| "decoder panicked".to_string())?
        .map(|s| s.sample_array)
        .map_err(|e| e.to_string())
}

/// Decode to bliss's format with ffmpeg (bliss's reference decoder).
fn decode_ffmpeg(ffmpeg: &str, path: &Path, slice: Option<(f64, f64)>) -> Result<Vec<f32>, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-v", "error"]);
    if let Some((start, len)) = slice {
        cmd.args(["-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}")]);
    }
    cmd.arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg: {e}"))?;
    let mut bytes = Vec::new();
    let read = child.stdout.take().map(|mut o| o.read_to_end(&mut bytes));
    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    if !status.success() || !matches!(read, Some(Ok(_))) {
        return Err(format!("ffmpeg failed: {status}"));
    }
    Ok(from_blob(&bytes))
}

fn to_blob(f: &[f32]) -> Vec<u8> {
    f.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Every analysed playable track's features, for nearest-neighbour search.
pub struct Index {
    ids: Vec<i64>,
    /// track id → row
    pos: HashMap<i64, usize>,
    /// `ids.len() × dim` features, row by row
    features: Vec<f32>,
    /// bliss's per-feature weights (the diagonal of its Mahalanobis matrix)
    weights: Vec<f32>,
}

impl Index {
    pub fn load(c: &Connection) -> Index {
        let weights: Vec<f32> = FeaturesVersion::LATEST.feature_weights().diag().to_vec();
        let dim = weights.len();
        let mut ids = Vec::new();
        let mut features = Vec::new();
        let sql = format!(
            "SELECT t.id, a.features FROM tracks t JOIN track_analysis a ON {CURRENT} \
             WHERE {LOCAL} AND a.features IS NOT NULL"
        );
        if let Ok(mut st) = c.prepare(&sql) {
            let rows = st.query_map([version()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            });
            for (id, blob) in rows.into_iter().flatten().flatten() {
                let f = from_blob(&blob);
                if f.len() == dim {
                    ids.push(id);
                    features.extend(f);
                }
            }
        }
        let pos = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        Index {
            ids,
            pos,
            features,
            weights,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn row(&self, i: usize) -> &[f32] {
        let d = self.weights.len();
        &self.features[i * d..(i + 1) * d]
    }

    /// Squared bliss distance.
    fn distance2(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .zip(&self.weights)
            .map(|((x, y), w)| w * (x - y) * (x - y))
            .sum()
    }

    /// Whether two tracks sound like the same recording (false unless both are analysed).
    pub fn near_duplicate(&self, a: i64, b: i64) -> bool {
        match (self.pos.get(&a), self.pos.get(&b)) {
            (Some(i), Some(j)) => {
                self.distance2(self.row(*i), self.row(*j)) < DUPLICATE_DISTANCE * DUPLICATE_DISTANCE
            }
            _ => false,
        }
    }

    /// How close every analysed track sounds to the nearest of `seeds`, or `None` when no
    /// seed is analysed.
    pub fn closeness(&self, seeds: &[i64]) -> Option<Closeness> {
        let mut seed_rows: Vec<usize> = seeds
            .iter()
            .filter_map(|s| self.pos.get(s).copied())
            .collect();
        if seed_rows.is_empty() {
            return None;
        }
        if seed_rows.len() > MAX_SEEDS {
            // evenly spread over the seeds (an artist's albums, in order)
            let step = seed_rows.len() as f64 / MAX_SEEDS as f64;
            seed_rows = (0..MAX_SEEDS)
                .map(|i| seed_rows[(i as f64 * step) as usize])
                .collect();
        }
        let mut by_distance: Vec<(f32, i64)> = (0..self.ids.len())
            .map(|i| {
                let d = seed_rows
                    .iter()
                    .map(|s| self.distance2(self.row(*s), self.row(i)))
                    .fold(f32::INFINITY, f32::min);
                (d, self.ids[i])
            })
            .collect();
        by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = by_distance.len().max(2) as f64;
        let rank = by_distance
            .iter()
            .enumerate()
            .map(|(i, (_, id))| (*id, 1.0 - i as f64 / (n - 1.0)))
            .collect();
        Some(Closeness {
            nearest: by_distance.into_iter().map(|(_, id)| id).collect(),
            rank,
        })
    }
}

/// Analysed tracks ranked by how close they sound to a set of seeds.
#[derive(Debug, Default)]
pub struct Closeness {
    /// nearest first (the seeds themselves included)
    pub nearest: Vec<i64>,
    /// track → 1 for the nearest track down to 0 for the farthest
    pub rank: HashMap<i64, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(rows: &[(i64, f32)]) -> Index {
        let weights: Vec<f32> = FeaturesVersion::LATEST.feature_weights().diag().to_vec();
        let dim = weights.len();
        Index {
            ids: rows.iter().map(|r| r.0).collect(),
            pos: rows.iter().enumerate().map(|(i, r)| (r.0, i)).collect(),
            features: rows.iter().flat_map(|r| vec![r.1; dim]).collect(),
            weights,
        }
    }

    #[test]
    fn blob_round_trip() {
        let f = vec![0.5, -1.25, 3.0];
        assert_eq!(from_blob(&to_blob(&f)), f);
    }

    #[test]
    fn ranks_by_distance_to_the_nearest_seed() {
        let ix = index(&[(1, 0.0), (2, 0.1), (3, 0.9), (4, 1.0), (5, 0.5)]);
        let c = ix.closeness(&[1, 4]).unwrap();
        assert_eq!(&c.nearest[..2], &[1, 4]);
        // 2 and 3 are each 0.1 from a seed; 5 is farthest from both
        assert_eq!(*c.nearest.last().unwrap(), 5);
        assert_eq!(c.rank[&5], 0.0);
        assert!(c.rank[&2] > c.rank[&5]);
        assert!(ix.closeness(&[99]).is_none());
    }

    #[test]
    fn near_duplicates() {
        let ix = index(&[(1, 0.0), (2, 0.005), (3, 0.5)]);
        assert!(ix.near_duplicate(1, 2));
        assert!(!ix.near_duplicate(1, 3));
        assert!(!ix.near_duplicate(1, 99));
    }

    #[test]
    fn analyses_a_tone() {
        // two seconds of a 440 Hz tone: the analysis succeeds and has the expected size
        let samples: Vec<f32> = (0..2 * SAMPLE_RATE)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        let a = Song::analyze(&samples).unwrap();
        assert_eq!(a.as_vec().len(), FeaturesVersion::LATEST.feature_count());
    }
}
