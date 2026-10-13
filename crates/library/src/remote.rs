//! Tracks of streaming services (Spotify, …) that RMS has played: their metadata, and what
//! they sound like, so mixes can pick them alongside the library's songs.  No audio is kept:
//! a service's plugin hands [`Library::learn_remote`] the decoded samples of a track it has
//! played through to the end, and only the analysis ([`crate::sonic`]'s features and
//! [`crate::rhythm`]) is stored, keyed by the track's `<source>:<id>` fingerprint as a local
//! track's analysis is keyed by its audio fingerprint.
//!
//! In mixes a remote track has the negative of its row id ([`remote_id`]), so local and
//! remote tracks share one id space; a remote track that is also in the library (same ISRC,
//! or same artist and title at about the same length) is left to the local copy.

use crate::sonic;
use crate::Library;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info};

/// A track of a streaming service.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteTrack {
    /// `spotify`, …
    pub source: String,
    /// The service's id of the track.
    pub source_id: String,
    /// What plays it (`spotify://track:<id>`).
    pub url: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    /// Seconds.
    pub duration: Option<f64>,
    pub isrc: Option<String>,
    /// Image URL.
    pub cover: Option<String>,
}

impl RemoteTrack {
    /// Key of its analysis in `track_analysis` / `track_rhythm`.
    pub fn fingerprint(&self) -> String {
        format!("{}:{}", self.source, self.source_id)
    }
}

/// Mix id of the remote track with row id `row` (negative: local tracks are positive).
pub fn remote_id(row: i64) -> i64 {
    -row
}

/// Whether a mix id is a remote track's.
pub fn is_remote(id: i64) -> bool {
    id < 0
}

/// An ISRC as stored: 12 characters, upper case, without the dashes some taggers write.
pub fn normalize_isrc(s: &str) -> Option<String> {
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == 12 && s[..2].chars().all(|c| c.is_ascii_alphabetic())).then_some(s)
}

/// Shortest track that is analysed (jingles and skits say little).
const MIN_SECS: f64 = 30.0;
/// Separator of the artists' names in `remote_tracks.artists`.
const SEP: char = '\u{1f}';

/// How many tracks of a service are known and analysed.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RemoteTotals {
    pub tracks: i64,
    pub analyzed: i64,
}

impl Library {
    /// Remember a remote track and what it sounds like, from its decoded audio (mono f32
    /// at 22050 Hz, the whole track).  Runs the analysis on the calling thread (a second or
    /// so): call it off the async runtime.
    pub fn learn_remote(&self, t: &RemoteTrack, samples: &[f32]) -> anyhow::Result<()> {
        let secs = samples.len() as f64 / sonic::SAMPLE_RATE as f64;
        anyhow::ensure!(secs >= MIN_SECS, "too short to analyse ({secs:.0} s)");
        let features = sonic::features(samples).map_err(|e| anyhow::anyhow!("{e}"))?;
        let rhythm = crate::rhythm::analyze(samples);
        let print = crate::print::fingerprint(samples, sonic::SAMPLE_RATE);
        let c = self.conn()?;
        let row = self.store_remote(&c, t)?;
        let fp = t.fingerprint();
        sonic::store_features(&c, &fp, Some(&features), None)?;
        sonic::store_rhythm(&c, &fp, Some(&rhythm))?;
        let duration = t.duration.or(Some(secs));
        crate::print::store(&c, &fp, print.as_deref(), duration)?;
        // the library may have it under other tags (or none)
        if let Some(p) = &print {
            if let Some(local) = crate::print::find_local(&c, p, duration)? {
                c.execute(
                    "UPDATE remote_tracks SET local_match = ? WHERE id = ?",
                    params![local, row],
                )?;
            }
        }
        self.add_remote_to_sound_index(row, features);
        // what kind of music it is, when EffNet analysis is on
        if let Some(m) = self.effnet() {
            match crate::effnet::analyze(&m, samples, sonic::SAMPLE_RATE) {
                Ok(a) => self.store_effnet(&c, &fp, a.as_ref()),
                Err(e) => tracing::debug!("EffNet analysis of {}: {e}", t.title),
            }
        }
        info!(
            "learned {} \"{}\" by {}: {:.1} BPM, {}",
            t.source,
            t.title,
            t.artists.join(", "),
            rhythm.bpm,
            rhythm.camelot()
        );
        Ok(())
    }

    /// Remember a remote track's metadata (without analysis); returns its row id.
    pub fn remember_remote(&self, t: &RemoteTrack) -> anyhow::Result<i64> {
        let c = self.conn()?;
        self.store_remote(&c, t)
    }

    /// Insert or update a remote track's metadata; returns its row id.
    pub fn store_remote(&self, c: &Connection, t: &RemoteTrack) -> anyhow::Result<i64> {
        let rules = self.text_rules();
        let keys: Vec<String> = t.artists.iter().map(|a| rules.search_key(a)).collect();
        let artist_keys = format!("|{}|", keys.join("|"));
        let match_key = match_key(
            keys.first().map(String::as_str).unwrap_or(""),
            &rules.search_key(&t.title),
        );
        let now = crate::scanner::now();
        c.execute(
            "INSERT INTO remote_tracks (source, source_id, url, fingerprint, title, artists,
               artist_keys, match_key, album, year, duration, isrc, cover, added_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
             ON CONFLICT (source, source_id) DO UPDATE SET url = excluded.url,
               title = excluded.title, artists = excluded.artists,
               artist_keys = excluded.artist_keys, match_key = excluded.match_key,
               album = excluded.album, year = excluded.year, duration = excluded.duration,
               isrc = COALESCE(excluded.isrc, remote_tracks.isrc),
               cover = COALESCE(excluded.cover, remote_tracks.cover),
               updated_at = excluded.updated_at",
            params![
                t.source,
                t.source_id,
                t.url,
                t.fingerprint(),
                t.title,
                t.artists.join(&SEP.to_string()),
                artist_keys,
                match_key,
                t.album,
                t.year,
                t.duration,
                t.isrc.as_deref().and_then(normalize_isrc),
                t.cover,
                now
            ],
        )?;
        Ok(c.query_row(
            "SELECT id FROM remote_tracks WHERE source = ? AND source_id = ?",
            params![t.source, t.source_id],
            |r| r.get(0),
        )?)
    }

    /// Whether the remote track is already analysed (current versions of both analyses).
    pub fn remote_analyzed(&self, source: &str, source_id: &str) -> bool {
        let Ok(c) = self.conn() else { return false };
        let fp = format!("{source}:{source_id}");
        c.query_row(
            "SELECT 1 FROM track_analysis a
               JOIN track_rhythm r ON r.fingerprint = a.fingerprint
               JOIN track_chromaprint p ON p.fingerprint = a.fingerprint
             WHERE a.fingerprint = ? AND a.version = ? AND a.features IS NOT NULL
               AND r.version = ? AND p.version = ?",
            params![
                fp,
                sonic::version(),
                crate::rhythm::VERSION,
                crate::print::VERSION
            ],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// The remote track with mix id `id` (negative).
    pub fn remote_track(&self, id: i64) -> Option<RemoteTrack> {
        let c = self.conn().ok()?;
        c.query_row(
            "SELECT source, source_id, url, title, artists, album, year, duration, isrc, cover
             FROM remote_tracks WHERE id = ?",
            [-id],
            |r| {
                Ok(RemoteTrack {
                    source: r.get(0)?,
                    source_id: r.get(1)?,
                    url: r.get(2)?,
                    title: r.get(3)?,
                    artists: r
                        .get::<_, String>(4)?
                        .split(SEP)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                    album: r.get(5)?,
                    year: r.get(6)?,
                    duration: r.get(7)?,
                    isrc: r.get(8)?,
                    cover: r.get(9)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Mix ids of the remote tracks playing these URLs (others are left out).
    pub fn remote_ids_for_urls(&self, urls: &[String]) -> Vec<(String, i64)> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let Ok(mut st) = c.prepare_cached("SELECT id FROM remote_tracks WHERE url = ?") else {
            return Vec::new();
        };
        urls.iter()
            .filter_map(|u| {
                st.query_row([u], |r| r.get::<_, i64>(0))
                    .ok()
                    .map(|row| (u.clone(), remote_id(row)))
            })
            .collect()
    }

    /// Known and analysed tracks of `source`.
    pub fn remote_totals(&self, source: &str) -> RemoteTotals {
        let Ok(c) = self.conn() else {
            return RemoteTotals::default();
        };
        c.query_row(
            "SELECT COUNT(*), COUNT(a.features) FROM remote_tracks t
             LEFT JOIN track_analysis a ON a.fingerprint = t.fingerprint AND a.version = ?
             WHERE t.source = ?",
            params![sonic::version(), source],
            |r| {
                Ok(RemoteTotals {
                    tracks: r.get(0)?,
                    analyzed: r.get(1)?,
                })
            },
        )
        .unwrap_or_default()
    }

    /// Forget every track of `source` and its analysis.  Returns how many were forgotten.
    pub fn forget_remote(&self, source: &str) -> anyhow::Result<usize> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM track_analysis WHERE fingerprint IN
               (SELECT fingerprint FROM remote_tracks WHERE source = ?)",
            [source],
        )?;
        tx.execute(
            "DELETE FROM track_rhythm WHERE fingerprint IN
               (SELECT fingerprint FROM remote_tracks WHERE source = ?)",
            [source],
        )?;
        tx.execute(
            "DELETE FROM track_chromaprint WHERE fingerprint IN
               (SELECT fingerprint FROM remote_tracks WHERE source = ?)",
            [source],
        )?;
        tx.execute(
            "DELETE FROM track_effnet WHERE fingerprint IN
               (SELECT fingerprint FROM remote_tracks WHERE source = ?)",
            [source],
        )?;
        let n = tx.execute("DELETE FROM remote_tracks WHERE source = ?", [source])?;
        tx.commit()?;
        // the indexes have them: load them again when next needed
        *self.inner.sound.write() = None;
        self.reset_style_index();
        debug!("forgot {n} {source} tracks");
        Ok(n)
    }

    /// Of these remote mix ids, those the library has a copy of: the same audio fingerprint,
    /// the same ISRC, or the same first artist and title within 3 seconds of the same length.
    pub fn remote_with_local_copy(&self, ids: &[i64]) -> std::collections::HashSet<i64> {
        let mut out = std::collections::HashSet::new();
        let Ok(c) = self.conn() else { return out };
        let Ok(mut st) = c.prepare_cached(
            "SELECT 1 FROM remote_tracks r WHERE r.id = ?1 AND (
               (r.local_match IS NOT NULL
                  AND EXISTS (SELECT 1 FROM tracks t WHERE t.fingerprint = r.local_match))
               OR (r.isrc IS NOT NULL AND EXISTS (SELECT 1 FROM tracks t WHERE t.isrc = r.isrc))
               OR EXISTS (SELECT 1 FROM tracks t JOIN artists a ON a.id = t.artist_id
                          WHERE (a.name_search || '|' || t.title_search) = r.match_key
                            AND (r.duration IS NULL OR t.duration IS NULL
                                 OR ABS(t.duration - r.duration) <= 3)))",
        ) else {
            return out;
        };
        for id in ids.iter().filter(|id| is_remote(**id)) {
            if st.exists([-id]).unwrap_or(false) {
                out.insert(*id);
            }
        }
        out
    }

    fn add_remote_to_sound_index(&self, row: i64, features: Vec<f32>) {
        let mut slot = self.inner.sound.write();
        if let Some(ix) = slot.as_mut() {
            std::sync::Arc::make_mut(ix).add(remote_id(row), &features);
        }
    }

    fn text_rules(&self) -> crate::text::TextRules {
        let cfg = self.config();
        crate::text::TextRules::new(&cfg.ignored_articles, &cfg.split_list)
    }
}

/// `<first artist key>|<title key>`, as local tracks are matched (`artists.name_search` and
/// `tracks.title_search`).
fn match_key(artist: &str, title: &str) -> String {
    format!("{artist}|{title}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isrcs_are_normalised() {
        assert_eq!(
            normalize_isrc("us-rc1-17-02345").as_deref(),
            Some("USRC11702345")
        );
        assert_eq!(
            normalize_isrc("GBAYE0601498").as_deref(),
            Some("GBAYE0601498")
        );
        assert_eq!(normalize_isrc("12345"), None);
        assert_eq!(normalize_isrc("123456789012"), None);
    }

    #[test]
    fn remote_ids_are_negative() {
        assert_eq!(remote_id(7), -7);
        assert!(is_remote(remote_id(7)));
        assert!(!is_remote(7));
    }
}
