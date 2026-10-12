//! What MusicBrainz knows about the library's recordings and the streamed tracks RMS has
//! learned: the recording id, its ISRC and its genres.  The lookups themselves are the
//! server's (`plugins::musicbrainz`); this keeps their results (`track_musicbrainz`, by audio
//! fingerprint, so they survive wipes and follow moved files) and fills in what the tags
//! leave out ([`Library::apply_musicbrainz`]): a track's MusicBrainz id and ISRC when it has
//! none, and its genres when it has no genre tag.  Tags always win; files are never written.

use crate::Library;
use rusqlite::{params, OptionalExtension};

/// Separator of the genre names stored.
const SEP: char = '\u{1f}';
/// A lookup that found nothing is tried again after this long (MusicBrainz grows).
const RETRY_SECS: i64 = 90 * 86400;

/// Something to identify: a local track's audio, or a streamed track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Job {
    /// `track_musicbrainz` key: the audio fingerprint, or `<source>:<id>`.
    pub fingerprint: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// From the tags (or the streaming service).
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    /// Chromaprint fingerprint (local tracks once analysed), for AcoustID.
    pub print: Option<Vec<u32>>,
}

/// What a lookup found (`mbid: None`: nothing).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    pub artist_mbid: Option<String>,
    pub genres: Vec<String>,
    /// `tag`, `isrc`, `acoustid`, `search`, or `none`.
    pub how: String,
}

/// How far identification got.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct Totals {
    /// Recordings looked up.
    pub looked_up: i64,
    /// …and found.
    pub identified: i64,
    /// Waiting for a lookup.
    pub pending: i64,
}

impl Library {
    /// Up to `n` recordings to look up: never looked up, or not found a while ago.  Local
    /// tracks first (one per audio), then streamed tracks.
    pub fn musicbrainz_pending(&self, n: usize) -> Vec<Job> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let cutoff = crate::scanner::now() - RETRY_SECS;
        let mut out = Vec::new();
        let local = format!(
            "SELECT t.fingerprint, t.title, a.name, al.title, t.duration, t.mbid, t.isrc, p.print
             FROM tracks t
             LEFT JOIN artists a ON a.id = t.artist_id
             LEFT JOIN albums al ON al.id = t.album_id
             LEFT JOIN track_chromaprint p ON p.fingerprint = t.fingerprint
             LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
             WHERE t.fingerprint IS NOT NULL
               AND (m.fingerprint IS NULL OR (m.how = 'none' AND m.looked_up_at < ?1))
             GROUP BY t.fingerprint ORDER BY t.id LIMIT {n}"
        );
        let remote = format!(
            "SELECT r.fingerprint, r.title, r.artists, r.album, r.duration, NULL, r.isrc, p.print
             FROM remote_tracks r
             LEFT JOIN track_chromaprint p ON p.fingerprint = r.fingerprint
             LEFT JOIN track_musicbrainz m ON m.fingerprint = r.fingerprint
             WHERE m.fingerprint IS NULL OR (m.how = 'none' AND m.looked_up_at < ?1)
             ORDER BY r.id LIMIT {n}"
        );
        for sql in [local, remote] {
            if out.len() >= n {
                break;
            }
            let Ok(mut st) = c.prepare(&sql) else {
                continue;
            };
            let rows = st.query_map([cutoff], |r| {
                Ok(Job {
                    fingerprint: r.get(0)?,
                    title: r.get(1)?,
                    // (a streamed track's artists: the first)
                    artist: r
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_default()
                        .split('\u{1f}')
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    album: r.get(3)?,
                    duration: r.get(4)?,
                    mbid: r.get(5)?,
                    isrc: r.get(6)?,
                    print: r
                        .get::<_, Option<Vec<u8>>>(7)?
                        .map(|b| crate::print::from_blob(&b)),
                })
            });
            if let Ok(rows) = rows {
                out.extend(rows.flatten().take(n - out.len()));
            }
        }
        out
    }

    /// Keep a lookup's result.
    pub fn store_musicbrainz(&self, fingerprint: &str, f: &Found) -> anyhow::Result<()> {
        let c = self.conn()?;
        let genres = f.genres.join(&SEP.to_string());
        c.execute(
            "INSERT OR REPLACE INTO track_musicbrainz
               (fingerprint, mbid, isrc, artist_mbid, genres, how, looked_up_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                fingerprint,
                f.mbid,
                f.isrc.as_deref().and_then(crate::remote::normalize_isrc),
                f.artist_mbid,
                (!genres.is_empty()).then_some(genres),
                if f.how.is_empty() { "none" } else { &f.how },
                crate::scanner::now()
            ],
        )?;
        Ok(())
    }

    /// Fill in from MusicBrainz what the tags leave out: MusicBrainz ids, ISRCs, the genres
    /// of tracks without a genre tag (the "No Genre" genre), and what the albums' releases
    /// tell ([`Library::apply_releases`]).  Returns the tracks and albums changed.
    pub fn apply_musicbrainz(&self) -> usize {
        let Ok(c) = self.conn() else { return 0 };
        let mut n = 0;
        n += c
            .execute(
                "UPDATE tracks SET mbid = (SELECT m.mbid FROM track_musicbrainz m
                                           WHERE m.fingerprint = tracks.fingerprint)
                 WHERE mbid IS NULL AND fingerprint IN
                   (SELECT fingerprint FROM track_musicbrainz WHERE mbid IS NOT NULL)",
                [],
            )
            .unwrap_or(0);
        n += c
            .execute(
                "UPDATE tracks SET isrc = (SELECT m.isrc FROM track_musicbrainz m
                                           WHERE m.fingerprint = tracks.fingerprint)
                 WHERE isrc IS NULL AND fingerprint IN
                   (SELECT fingerprint FROM track_musicbrainz WHERE isrc IS NOT NULL)",
                [],
            )
            .unwrap_or(0);
        n + self.apply_genres(&c).unwrap_or(0) + self.apply_releases()
    }

    /// Genres for tracks whose only genre is "No Genre".
    fn apply_genres(&self, c: &rusqlite::Connection) -> rusqlite::Result<usize> {
        let cfg = self.config();
        let rules = crate::text::TextRules::new(&cfg.ignored_articles, &cfg.split_list);
        let none: Option<i64> = c
            .query_row(
                "SELECT id FROM genres WHERE name_search = ?",
                [rules.search_key(&cfg.no_genre_string)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(none) = none else { return Ok(0) };
        let rows: Vec<(i64, String)> = c
            .prepare(
                "SELECT t.id, m.genres FROM tracks t
                 JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
                 WHERE m.genres IS NOT NULL
                   AND EXISTS (SELECT 1 FROM track_genres g WHERE g.track_id = t.id AND g.genre_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM track_genres g WHERE g.track_id = t.id AND g.genre_id != ?1)",
            )?
            .query_map([none], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut done = 0;
        for (track, genres) in rows {
            let ids: Vec<i64> = genres
                .split(SEP)
                .filter(|g| !g.trim().is_empty())
                .map(|g| genre_id(c, &rules, g.trim()))
                .collect::<Result<_, _>>()?;
            if ids.is_empty() {
                continue;
            }
            c.execute("DELETE FROM track_genres WHERE track_id = ?", [track])?;
            for g in ids {
                c.execute(
                    "INSERT OR IGNORE INTO track_genres (track_id, genre_id) VALUES (?, ?)",
                    params![track, g],
                )?;
            }
            done += 1;
        }
        // "No Genre" may be left without tracks
        c.execute(
            "DELETE FROM genres WHERE id = ? AND NOT EXISTS (SELECT 1 FROM track_genres WHERE genre_id = ?)",
            params![none, none],
        )?;
        Ok(done)
    }

    /// Genre names MusicBrainz gives the recordings with these fingerprints.
    pub fn musicbrainz_genres(&self, fingerprint: &str) -> Vec<String> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        c.query_row(
            "SELECT genres FROM track_musicbrainz WHERE fingerprint = ?",
            [fingerprint],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
        .map(|g| g.split(SEP).map(str::to_string).collect())
        .unwrap_or_default()
    }

    pub fn musicbrainz_totals(&self) -> Totals {
        let Ok(c) = self.conn() else {
            return Totals::default();
        };
        let (looked_up, identified): (i64, i64) = c
            .query_row(
                "SELECT COUNT(*), COUNT(mbid) FROM track_musicbrainz",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_default();
        let pending: i64 = c
            .query_row(
                "SELECT (SELECT COUNT(DISTINCT t.fingerprint) FROM tracks t
                         LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
                         WHERE t.fingerprint IS NOT NULL AND m.fingerprint IS NULL)
                      + (SELECT COUNT(*) FROM remote_tracks r
                         LEFT JOIN track_musicbrainz m ON m.fingerprint = r.fingerprint
                         WHERE m.fingerprint IS NULL)",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        Totals {
            looked_up,
            identified,
            pending,
        }
    }
}

/// The id of a genre, made if new (as the scanner makes them).
pub(crate) fn genre_id(
    c: &rusqlite::Connection,
    rules: &crate::text::TextRules,
    name: &str,
) -> rusqlite::Result<i64> {
    let search = rules.search_key(name);
    if let Some(id) = c
        .query_row(
            "SELECT id FROM genres WHERE name_search = ?",
            [&search],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    c.execute(
        "INSERT INTO genres (name, name_sort, name_search) VALUES (?, ?, ?)",
        params![name, rules.sort_key(name), search],
    )?;
    Ok(c.last_insert_rowid())
}
