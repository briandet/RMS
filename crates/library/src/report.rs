//! What the analyses made of a track, all in one place, and how far each analysis has got over
//! the library: for people looking at why a mix picked what it picked, and for checking that
//! the background analysis is doing its job.
//!
//! Each analysis is keyed by the audio's fingerprint and stores its version; a row of another
//! version is *stale* (it will be redone), a current row without results *failed* (redone only
//! when the file changes), no row at all *pending*.

use crate::Library;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// Where one analysis stands for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Current results.
    Done,
    /// Analysed with the current version, but nothing came of it (unreadable, too short).
    Failed,
    /// Results of an older version: waiting to be redone.
    Stale,
    /// Not analysed yet.
    Pending,
    /// Never analysed: too long, or not a local file.
    Skipped,
    /// The analysis is turned off on this server.
    Off,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Stale => "stale",
            Status::Pending => "pending",
            Status::Skipped => "skipped",
            Status::Off => "off",
        }
    }
}

/// One analysis of a track: where it stands, which version made it and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Stage<T> {
    pub status: Status,
    pub version: Option<i64>,
    pub current_version: i64,
    pub analyzed_at: Option<i64>,
    pub data: Option<T>,
}

impl<T> Stage<T> {
    fn new(current_version: i64) -> Self {
        Stage {
            status: Status::Pending,
            version: None,
            current_version,
            analyzed_at: None,
            data: None,
        }
    }
}

/// The file and what its tags and the scanner say about the audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileInfo {
    pub url: String,
    pub fingerprint: Option<String>,
    pub format: String,
    pub size: Option<i64>,
    pub bitrate: Option<i64>,
    pub vbr: bool,
    pub sample_rate: Option<i64>,
    pub sample_size: Option<i64>,
    pub channels: Option<i64>,
    pub lossless: bool,
    pub duration: Option<f64>,
    /// A cue-sheet track: where it starts and ends in the file, seconds.
    pub segment: Option<(f64, f64)>,
    /// From the tags (or filled in from the measured loudness).
    pub replay_gain: Option<f64>,
    pub replay_peak: Option<f64>,
    /// The `BPM` tag.
    pub bpm_tag: Option<i64>,
}

/// bliss: what the track sounds like.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sound {
    /// Named features, in bliss's order.
    pub features: Vec<(String, f32)>,
    /// Integrated loudness (EBU R128), LUFS, and true peak (linear).
    pub loudness: Option<f64>,
    pub peak: Option<f64>,
    /// The library tracks that sound closest, with their bliss distance.
    pub nearest: Vec<(i64, f32)>,
    /// Tracks closer than this are taken for the same recording.
    pub duplicate_distance: f32,
}

/// Tempo, beats, key and energy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RhythmInfo {
    /// 0 when there is no steady beat.
    pub bpm: f32,
    pub beat_confidence: f32,
    pub beats: usize,
    /// Seconds into the track of the first bar's downbeat.
    pub first_downbeat: Option<f32>,
    pub bars: usize,
    /// Camelot code and name ("8A", "A minor").
    pub key: String,
    pub key_name: String,
    pub key_confidence: f32,
    /// Loudness per bar (per 2 seconds without a beat), dB.
    pub energy: Vec<f32>,
    /// Seconds per energy value.
    pub energy_step: Option<f32>,
}

/// Chromaprint: the audio fingerprint of the first two minutes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Print {
    /// 32-bit items (about 8 a second).
    pub items: usize,
    /// Seconds of audio it covers.
    pub duration: Option<f64>,
    /// Other library tracks of the same recording: bliss finds them close, Chromaprint says
    /// whether their fingerprints line up.
    pub duplicates: Vec<(i64, bool)>,
}

/// EffNet-Discogs: what kind of music it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Kind {
    pub labels: crate::effnet::Labels,
    /// The library tracks closest in kind, with their cosine similarity.
    pub nearest: Vec<(i64, f32)>,
}

/// What MusicBrainz said about the audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MusicBrainz {
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    pub artist_mbid: Option<String>,
    pub genres: Vec<String>,
    /// How it was found: `tag`, `isrc`, `acoustid`, `search`; `none` when nothing was.
    pub how: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub track_id: i64,
    pub file: FileInfo,
    pub sound: Stage<Sound>,
    pub rhythm: Stage<RhythmInfo>,
    pub print: Stage<Print>,
    pub kind: Stage<Kind>,
    pub musicbrainz: Stage<MusicBrainz>,
}

/// One analysis over the library.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Count {
    /// Distinct local audio (cue tracks of one file count once per segment's audio).
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    /// Not analysed yet, or with an older version.
    pub pending: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    pub sound: Count,
    pub rhythm: Count,
    pub print: Count,
    pub kind: Count,
    pub musicbrainz: Count,
}

/// bliss's feature names for a vector of `n` features (version 2: 23, version 1: 20).
pub fn feature_names(n: usize) -> Vec<String> {
    let mut names: Vec<String> = [
        "tempo",
        "zero_crossing_rate",
        "spectral_centroid_mean",
        "spectral_centroid_std",
        "spectral_rolloff_mean",
        "spectral_rolloff_std",
        "spectral_flatness_mean",
        "spectral_flatness_std",
        "loudness_mean",
        "loudness_std",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let chroma = n.saturating_sub(names.len());
    names.extend((1..=chroma).map(|i| format!("chroma_{i}")));
    names.truncate(n);
    names
}

/// "A minor", "F♯ major" for a pitch class and mode.
pub fn key_name(key: u8, minor: bool) -> String {
    const NAMES: [&str; 12] = [
        "C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B",
    ];
    format!(
        "{} {}",
        NAMES[key as usize % 12],
        if minor { "minor" } else { "major" }
    )
}

/// The stored version, timestamp and results of `table` for `fingerprint`.
fn row<T>(
    c: &Connection,
    sql: &str,
    fingerprint: &str,
    map: impl FnOnce(&rusqlite::Row) -> rusqlite::Result<(i64, Option<i64>, Option<T>)>,
) -> Option<(i64, Option<i64>, Option<T>)> {
    c.query_row(sql, [fingerprint], map)
        .optional()
        .ok()
        .flatten()
}

fn stage_from<T>(current: i64, found: Option<(i64, Option<i64>, Option<T>)>) -> Stage<T> {
    let mut s = Stage::new(current);
    if let Some((version, at, data)) = found {
        s.version = Some(version);
        s.analyzed_at = at;
        s.status = match (version == current, data.is_some()) {
            (true, true) => Status::Done,
            (true, false) => Status::Failed,
            (false, _) => Status::Stale,
        };
        if version == current {
            s.data = data;
        }
    }
    s
}

impl Library {
    /// Everything the analyses know about library track `id` (`None` for no such track).
    /// `effnet_on`: whether the EffNet models are in use, so a missing analysis reads `off`
    /// rather than `pending`; `musicbrainz_on` likewise.
    pub fn analysis_report(&self, id: i64, musicbrainz_on: bool) -> Option<Report> {
        let c = self.conn().ok()?;
        let file = c
            .query_row(
                "SELECT t.url, t.fingerprint, t.format, f.size, t.bitrate, t.vbr, t.sample_rate,
                        t.sample_size, t.channels, t.lossless, t.duration, t.cue_start, t.cue_end,
                        t.replay_gain, t.replay_peak, NULLIF(t.bpm, 0)
                 FROM tracks t LEFT JOIN files f ON f.id = t.file_id WHERE t.id = ?",
                [id],
                |r| {
                    let start: Option<f64> = r.get(11)?;
                    let end: Option<f64> = r.get(12)?;
                    Ok(FileInfo {
                        url: r.get(0)?,
                        fingerprint: r.get(1)?,
                        format: r.get(2)?,
                        size: r.get(3)?,
                        bitrate: r.get(4)?,
                        vbr: r.get::<_, i64>(5)? != 0,
                        sample_rate: r.get(6)?,
                        sample_size: r.get(7)?,
                        channels: r.get(8)?,
                        lossless: r.get::<_, i64>(9)? != 0,
                        duration: r.get(10)?,
                        segment: start.map(|s| (s, end.unwrap_or(0.0))),
                        replay_gain: r.get(13)?,
                        replay_peak: r.get(14)?,
                        bpm_tag: r.get(15)?,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()?;
        let effnet_on = self.effnet().is_some();
        let too_long = file.duration.unwrap_or(0.0) > crate::sonic::MAX_SECS;
        fn skipped<T>(mut s: Stage<T>, too_long: bool) -> Stage<T> {
            if s.status == Status::Pending && too_long {
                s.status = Status::Skipped;
            }
            s
        }
        let Some(fp) = file.fingerprint.clone() else {
            // not a local audio file: nothing is analysed
            fn skip<T>(v: i64) -> Stage<T> {
                Stage {
                    status: Status::Skipped,
                    ..Stage::new(v)
                }
            }
            return Some(Report {
                track_id: id,
                file,
                sound: skip(crate::sonic::version()),
                rhythm: skip(crate::rhythm::VERSION),
                print: skip(crate::print::VERSION),
                kind: skip(crate::effnet::VERSION),
                musicbrainz: skip(0),
            });
        };

        let sound = row(
            &c,
            "SELECT version, analyzed_at, features, loudness, peak FROM track_analysis
             WHERE fingerprint = ?",
            &fp,
            |r| {
                let features: Option<Vec<u8>> = r.get(2)?;
                let data = features.map(|b| {
                    let f: Vec<f32> = b
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    Sound {
                        features: feature_names(f.len()).into_iter().zip(f).collect(),
                        loudness: r.get(3).ok().flatten(),
                        peak: r.get(4).ok().flatten(),
                        nearest: Vec::new(),
                        duplicate_distance: crate::sonic::duplicate_distance(),
                    }
                });
                Ok((r.get(0)?, r.get(1)?, data))
            },
        );
        let mut sound = skipped(stage_from(crate::sonic::version(), sound), too_long);
        let index = self.sound_index();
        if let Some(s) = sound.data.as_mut() {
            s.nearest = index.nearest(id, 10);
        }

        let rhythm = row(
            &c,
            "SELECT version, analyzed_at, rhythm FROM track_rhythm WHERE fingerprint = ?",
            &fp,
            |r| {
                let blob: Option<Vec<u8>> = r.get(2)?;
                let data = blob
                    .and_then(|b| crate::rhythm::Rhythm::from_blob(&b))
                    .map(|rh| {
                        let first_downbeat = rh.beats.get(rh.downbeat as usize).copied();
                        let bars = rh.bars().count();
                        let energy_step = if rh.bpm > 0.0 {
                            Some(4.0 * 60.0 / rh.bpm)
                        } else {
                            Some(2.0)
                        };
                        RhythmInfo {
                            bpm: rh.bpm,
                            beat_confidence: rh.beat_confidence,
                            beats: rh.beats.len(),
                            first_downbeat,
                            bars,
                            key: rh.camelot(),
                            key_name: key_name(rh.key, rh.minor),
                            key_confidence: rh.key_confidence,
                            energy: rh.energy.clone(),
                            energy_step,
                        }
                    });
                Ok((r.get(0)?, r.get(1)?, data))
            },
        );
        let rhythm = skipped(stage_from(crate::rhythm::VERSION, rhythm), too_long);

        let print = row(
            &c,
            "SELECT version, analyzed_at, print, duration FROM track_chromaprint
             WHERE fingerprint = ?",
            &fp,
            |r| {
                let blob: Option<Vec<u8>> = r.get(2)?;
                let data = blob.map(|b| Print {
                    items: b.len() / 4,
                    duration: r.get(3).ok().flatten(),
                    duplicates: Vec::new(),
                });
                Ok((r.get(0)?, r.get(1)?, data))
            },
        );
        let mut print = skipped(stage_from(crate::print::VERSION, print), too_long);
        // the same recording elsewhere: close by sound, confirmed (or not) by fingerprint
        let near: Vec<i64> = index
            .nearest(id, 20)
            .into_iter()
            .take_while(|(_, d)| *d < crate::sonic::duplicate_distance())
            .map(|(t, _)| t)
            .collect();
        if let (Some(p), false) = (print.data.as_mut(), near.is_empty()) {
            let mine = self.print_of(&c, id);
            for other in near {
                let same = match (&mine, self.print_of(&c, other)) {
                    (Some(a), Some(b)) => crate::print::same_recording(a, &b),
                    _ => false,
                };
                p.duplicates.push((other, same));
            }
        }

        let kind = row(
            &c,
            "SELECT version, NULL, labels FROM track_effnet WHERE fingerprint = ?",
            &fp,
            |r| {
                let labels: Option<String> = r.get(2)?;
                let data = labels
                    .and_then(|l| serde_json::from_str::<crate::effnet::Labels>(&l).ok())
                    .map(|labels| Kind {
                        labels,
                        nearest: Vec::new(),
                    });
                Ok((r.get(0)?, r.get(1)?, data))
            },
        );
        let mut kind = skipped(stage_from(crate::effnet::VERSION, kind), too_long);
        if kind.status == Status::Pending && !effnet_on {
            kind.status = Status::Off;
        }
        if let Some(k) = kind.data.as_mut() {
            k.nearest = self.style_index().nearest(id, 10);
        }

        let mb = row(
            &c,
            "SELECT 0, looked_up_at, how, mbid, isrc, artist_mbid, genres FROM track_musicbrainz
             WHERE fingerprint = ?",
            &fp,
            |r| {
                let genres: Option<String> = r.get(6)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Some(MusicBrainz {
                        how: r.get(2)?,
                        mbid: r.get(3)?,
                        isrc: r.get(4)?,
                        artist_mbid: r.get(5)?,
                        genres: genres
                            .map(|g| {
                                g.split(crate::musicbrainz::SEP)
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    }),
                ))
            },
        );
        let mut musicbrainz = stage_from(0, mb);
        if musicbrainz.status == Status::Pending && !musicbrainz_on {
            musicbrainz.status = Status::Off;
        }
        if musicbrainz.data.as_ref().is_some_and(|m| m.mbid.is_none()) {
            // looked up, nothing found
            musicbrainz.status = Status::Failed;
        }

        Some(Report {
            track_id: id,
            file,
            sound,
            rhythm,
            print,
            kind,
            musicbrainz,
        })
    }

    /// A library track's stored fingerprint (current version).
    fn print_of(&self, c: &Connection, id: i64) -> Option<Vec<u32>> {
        let blob: Option<Vec<u8>> = c
            .query_row(
                "SELECT p.print FROM tracks t JOIN track_chromaprint p
                   ON p.fingerprint = t.fingerprint AND p.version = ?2
                 WHERE t.id = ?1",
                params![id, crate::print::VERSION],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        blob.map(|b| crate::print::from_blob(&b))
    }

    /// How far each analysis has got over the library's local audio.
    pub fn analysis_progress(&self) -> Progress {
        let Ok(c) = self.conn() else {
            return Progress::default();
        };
        let count = |table: &str, data: &str, version: i64| -> Count {
            let sql = format!(
                "SELECT COUNT(*),
                        COUNT(*) FILTER (WHERE x.version = ?1 AND x.{data} IS NOT NULL),
                        COUNT(*) FILTER (WHERE x.version = ?1 AND x.{data} IS NULL)
                 FROM (SELECT DISTINCT fingerprint FROM tracks WHERE fingerprint IS NOT NULL) t
                 LEFT JOIN {table} x ON x.fingerprint = t.fingerprint"
            );
            c.query_row(&sql, [version], |r| {
                let (total, done, failed): (i64, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
                Ok(Count {
                    total,
                    done,
                    failed,
                    pending: total - done - failed,
                })
            })
            .unwrap_or_default()
        };
        let mb = c
            .query_row(
                "SELECT COUNT(*), COUNT(m.mbid), COUNT(m.fingerprint) - COUNT(m.mbid)
                 FROM (SELECT DISTINCT fingerprint FROM tracks WHERE fingerprint IS NOT NULL) t
                 LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint",
                [],
                |r| {
                    let (total, done, failed): (i64, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
                    Ok(Count {
                        total,
                        done,
                        failed,
                        pending: total - done - failed,
                    })
                },
            )
            .unwrap_or_default();
        Progress {
            sound: count("track_analysis", "features", crate::sonic::version()),
            rhythm: count("track_rhythm", "rhythm", crate::rhythm::VERSION),
            print: count("track_chromaprint", "print", crate::print::VERSION),
            kind: count("track_effnet", "labels", crate::effnet::VERSION),
            musicbrainz: mb,
        }
    }

    /// Ids of library tracks, for reports that list other tracks: (id → title, artist).
    pub fn titles(&self, ids: &[i64]) -> HashMap<i64, (String, Option<String>)> {
        let Ok(c) = self.conn() else {
            return HashMap::new();
        };
        let mut out = HashMap::new();
        for id in ids {
            if let Ok(Some(v)) = c
                .query_row(
                    "SELECT t.title, a.name FROM tracks t LEFT JOIN artists a ON a.id = t.artist_id
                     WHERE t.id = ?",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()
            {
                out.insert(*id, v);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let v2 = feature_names(23);
        assert_eq!(v2.len(), 23);
        assert_eq!(v2[0], "tempo");
        assert_eq!(v2[9], "loudness_std");
        assert_eq!(v2[22], "chroma_13");
        assert_eq!(feature_names(20)[19], "chroma_10");
        assert_eq!(key_name(9, true), "A minor");
        assert_eq!(key_name(6, false), "F♯ major");
    }

    #[test]
    fn stages() {
        let s: Stage<u8> = stage_from(2, Some((2, Some(5), Some(1))));
        assert_eq!((s.status, s.data), (Status::Done, Some(1)));
        let s: Stage<u8> = stage_from(2, Some((2, None, None)));
        assert_eq!(s.status, Status::Failed);
        let s: Stage<u8> = stage_from(2, Some((1, None, Some(1))));
        assert_eq!((s.status, s.data), (Status::Stale, None));
        let s: Stage<u8> = stage_from(2, None);
        assert_eq!(s.status, Status::Pending);
    }
}
