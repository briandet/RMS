//! Discovery: what a mix proposes from outside what was played — recordings similar to the
//! seeds and tracks by similar artists, as ListenBrainz knows them (the server's
//! `plugins::listenbrainz` asks it).  The library keeps what was proposed and how it went
//! (`discovered`), counts skips with the listening history (`track_stats.skip_count`), and
//! caches the web services' answers (`web_cache`).
//!
//! A proposed track that is skipped within 30 seconds is *rejected*: it is not proposed
//! again, and an artist rejected twice no longer is either.

use crate::Library;
use rusqlite::{params, OptionalExtension};

/// An artist whose tracks were rejected this often is not proposed any more.
pub const ARTIST_REJECTIONS: i64 = 2;

/// How discovery went.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct Totals {
    pub proposed: i64,
    pub played: i64,
    pub rejected: i64,
}

/// A proposed track, for [`Library::note_discovered`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Proposal {
    pub url: String,
    pub mbid: Option<String>,
    pub title: String,
    pub artist: String,
    pub artist_mbid: Option<String>,
    /// The seed it is similar to.
    pub because: Option<String>,
}

impl Library {
    /// A cached web service answer no older than `max_age` seconds.
    pub fn cached(&self, key: &str, max_age: i64) -> Option<String> {
        let c = self.conn().ok()?;
        c.query_row(
            "SELECT body FROM web_cache WHERE key = ? AND fetched_at >= ?",
            params![key, crate::scanner::now() - max_age],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn cache(&self, key: &str, body: &str) {
        if let Ok(c) = self.conn() {
            let _ = c.execute(
                "INSERT OR REPLACE INTO web_cache (key, body, fetched_at) VALUES (?, ?, ?)",
                params![key, body, crate::scanner::now()],
            );
        }
    }

    /// The MusicBrainz recording and artist ids of these mix ids (local tracks positive,
    /// learned streamed ones negative), from their tags or identification.
    pub fn seed_mbids(&self, ids: &[i64]) -> (Vec<String>, Vec<String>) {
        let mut recs = Vec::new();
        let mut artists = Vec::new();
        let Ok(c) = self.conn() else {
            return (recs, artists);
        };
        let local = "SELECT COALESCE(t.mbid, m.mbid), COALESCE(a.mbid, m.artist_mbid)
             FROM tracks t
             LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
             LEFT JOIN artists a ON a.id = t.artist_id
             WHERE t.id = ?";
        let remote = "SELECT m.mbid, m.artist_mbid FROM remote_tracks r
             JOIN track_musicbrainz m ON m.fingerprint = r.fingerprint WHERE r.id = ?";
        for id in ids {
            let (sql, key) = if *id < 0 { (remote, -id) } else { (local, *id) };
            let row: Option<(Option<String>, Option<String>)> = c
                .query_row(sql, [key], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .ok()
                .flatten();
            if let Some((rec, artist)) = row {
                if let Some(r) = rec.filter(|r| !recs.contains(r)) {
                    recs.push(r);
                }
                if let Some(a) = artist.filter(|a| !artists.contains(a)) {
                    artists.push(a);
                }
            }
        }
        (recs, artists)
    }

    /// The library's track of a recording: by MusicBrainz id, else by artist and title.
    pub fn local_recording(&self, mbid: Option<&str>, artist: &str, title: &str) -> Option<i64> {
        let c = self.conn().ok()?;
        if let Some(m) = mbid {
            let id: Option<i64> = c
                .query_row(
                    "SELECT t.id FROM tracks t
                     LEFT JOIN track_musicbrainz mb ON mb.fingerprint = t.fingerprint
                     WHERE t.mbid = ?1 OR mb.mbid = ?1 LIMIT 1",
                    [m],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten();
            if id.is_some() {
                return id;
            }
        }
        let rules = self.rules();
        c.query_row(
            "SELECT t.id FROM tracks t JOIN artists a ON a.id = t.artist_id
             WHERE a.name_search = ? AND t.title_search = ? LIMIT 1",
            params![rules.search_key(artist), rules.search_key(title)],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Up to `n` library tracks by an artist (by MusicBrainz id or name) never played or
    /// skipped, at random.
    pub fn unplayed_by_artist(&self, mbid: Option<&str>, name: &str, n: usize) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let key = self.rules().search_key(name);
        let Ok(mut st) = c.prepare(&format!(
            "SELECT DISTINCT t.id FROM tracks t
             JOIN track_artists ta ON ta.track_id = t.id
             JOIN artists a ON a.id = ta.artist_id
             LEFT JOIN track_stats s ON s.url = t.url
             WHERE (a.mbid = ?1 OR a.name_search = ?2)
               AND COALESCE(s.play_count, 0) = 0 AND COALESCE(s.skip_count, 0) = 0
             ORDER BY RANDOM() LIMIT {n}"
        )) else {
            return Vec::new();
        };
        st.query_map(params![mbid, key], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Whether a track (by URL) was played, skipped, learned or already proposed.
    pub fn heard(&self, url: &str) -> bool {
        let Ok(c) = self.conn() else { return true };
        c.query_row(
            "SELECT EXISTS (SELECT 1 FROM track_stats WHERE url = ?1 AND (play_count > 0 OR skip_count > 0))
                 OR EXISTS (SELECT 1 FROM discovered WHERE url = ?1 AND status != 'new')
                 OR EXISTS (SELECT 1 FROM remote_tracks r JOIN track_analysis a
                            ON a.fingerprint = r.fingerprint WHERE r.url = ?1)",
            [url],
            |r| r.get(0),
        )
        .unwrap_or(true)
    }

    /// Whether the artist (by name) has had [`ARTIST_REJECTIONS`] tracks rejected.
    pub fn artist_rejected(&self, artist: &str) -> bool {
        let Ok(c) = self.conn() else { return false };
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM discovered WHERE artist_key = ? AND status = 'rejected'",
                [self.rules().search_key(artist)],
                |r| r.get(0),
            )
            .unwrap_or(0);
        n >= ARTIST_REJECTIONS
    }

    /// Remember a proposed track (if new).
    pub fn note_discovered(&self, p: &Proposal) {
        let Ok(c) = self.conn() else { return };
        let now = crate::scanner::now();
        let _ = c.execute(
            "INSERT OR IGNORE INTO discovered
               (url, mbid, title, artist, artist_key, artist_mbid, because, status, added_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'new', ?, ?)",
            params![
                p.url,
                p.mbid,
                p.title,
                p.artist,
                self.rules().search_key(&p.artist),
                p.artist_mbid,
                p.because,
                now,
                now
            ],
        );
    }

    /// A track started playing: a proposed one counts as played.
    pub fn mark_started(&self, url: &str) {
        if let Ok(c) = self.conn() {
            let _ = c.execute(
                "UPDATE discovered SET status = 'played', updated_at = ? WHERE url = ? AND status = 'new'",
                params![crate::scanner::now(), url],
            );
        }
    }

    /// A track was skipped (something else started within 30 seconds): counted, and a proposed
    /// track is rejected.
    pub fn mark_skipped(&self, url: &str) {
        let Ok(c) = self.conn() else { return };
        let now = crate::scanner::now();
        let n = c
            .execute(
                "UPDATE track_stats SET skip_count = skip_count + 1 WHERE url = ?",
                [url],
            )
            .unwrap_or(0);
        if n == 0 {
            let _ = c.execute(
                "INSERT INTO track_stats (url, added_at, skip_count) VALUES (?, ?, 1)",
                params![url, now],
            );
        }
        let _ = c.execute(
            "UPDATE discovered SET status = 'rejected', updated_at = ? WHERE url = ?",
            params![now, url],
        );
    }

    pub fn discovery_totals(&self) -> Totals {
        let Ok(c) = self.conn() else {
            return Totals::default();
        };
        c.query_row(
            "SELECT COUNT(*), COUNT(*) FILTER (WHERE status = 'played'),
                    COUNT(*) FILTER (WHERE status = 'rejected') FROM discovered",
            [],
            |r| {
                Ok(Totals {
                    proposed: r.get(0)?,
                    played: r.get(1)?,
                    rejected: r.get(2)?,
                })
            },
        )
        .unwrap_or_default()
    }

    /// The URL of a library track.
    pub fn track_url(&self, id: i64) -> Option<String> {
        let c = self.conn().ok()?;
        c.query_row("SELECT url FROM tracks WHERE id = ?", [id], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }
}
