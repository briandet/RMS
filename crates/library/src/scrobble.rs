//! What scrobbling needs from the library (the server's `plugins::scrobble` decides what
//! counts as a listen and sends it): a played track's details, with the MusicBrainz ids from
//! its tags or identification, and the queue of listens not yet sent (`scrobble_queue`), so
//! none are lost while a service or the network is down.

use crate::Library;
use rusqlite::{params, OptionalExtension};

/// A played track as a listen describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListenInfo {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    pub tracknumber: Option<i64>,
    pub recording_mbid: Option<String>,
    pub artist_mbids: Vec<String>,
    pub release_mbid: Option<String>,
    pub isrc: Option<String>,
}

/// A listen waiting to be sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub id: i64,
    pub listened_at: i64,
    pub payload: String,
    pub attempts: i64,
}

impl Library {
    /// A library track's (by URL) or learned streamed track's (by its URL) details.
    pub fn listen_info(&self, url: &str) -> Option<ListenInfo> {
        let c = self.conn().ok()?;
        let local = c
            .query_row(
                "SELECT t.title, a.name, al.title, t.duration, t.track_number,
                        COALESCE(t.mbid, m.mbid), COALESCE(a.mbid, m.artist_mbid), al.mbid,
                        COALESCE(t.isrc, m.isrc)
                 FROM tracks t
                 LEFT JOIN artists a ON a.id = t.artist_id
                 LEFT JOIN albums al ON al.id = t.album_id
                 LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
                 WHERE t.url = ?",
                [url],
                |r| {
                    Ok(ListenInfo {
                        title: r.get(0)?,
                        artist: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        album: r.get(2)?,
                        duration: r.get(3)?,
                        tracknumber: r.get(4)?,
                        recording_mbid: r.get(5)?,
                        artist_mbids: r.get::<_, Option<String>>(6)?.into_iter().collect(),
                        release_mbid: r.get(7)?,
                        isrc: r.get(8)?,
                    })
                },
            )
            .optional()
            .ok()
            .flatten();
        if local.is_some() {
            return local;
        }
        c.query_row(
            "SELECT r.title, r.artists, r.album, r.duration, m.mbid, m.artist_mbid,
                    COALESCE(r.isrc, m.isrc)
             FROM remote_tracks r
             LEFT JOIN track_musicbrainz m ON m.fingerprint = r.fingerprint
             WHERE r.url = ?",
            [url],
            |r| {
                Ok(ListenInfo {
                    title: r.get(0)?,
                    // the artists of a streamed track: all of them, as one credit
                    artist: r
                        .get::<_, Option<String>>(1)?
                        .unwrap_or_default()
                        .split('\u{1f}')
                        .collect::<Vec<_>>()
                        .join(", "),
                    album: r.get(2)?,
                    duration: r.get(3)?,
                    recording_mbid: r.get(4)?,
                    artist_mbids: r.get::<_, Option<String>>(5)?.into_iter().collect(),
                    isrc: r.get(6)?,
                    ..Default::default()
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Keep a listen until it is sent.
    pub fn queue_listen(
        &self,
        account: &str,
        listened_at: i64,
        payload: &str,
    ) -> anyhow::Result<()> {
        let c = self.conn()?;
        c.execute(
            "INSERT INTO scrobble_queue (account, listened_at, payload) VALUES (?, ?, ?)",
            params![account, listened_at, payload],
        )?;
        Ok(())
    }

    /// The oldest `n` listens waiting for an account.
    pub fn queued_listens(&self, account: &str, n: usize) -> Vec<Queued> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let Ok(mut st) = c.prepare(&format!(
            "SELECT id, listened_at, payload, attempts FROM scrobble_queue
             WHERE account = ? ORDER BY id LIMIT {n}"
        )) else {
            return Vec::new();
        };
        st.query_map([account], |r| {
            Ok(Queued {
                id: r.get(0)?,
                listened_at: r.get(1)?,
                payload: r.get(2)?,
                attempts: r.get(3)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Listens sent (or given up on).
    pub fn listens_done(&self, ids: &[i64]) {
        let Ok(c) = self.conn() else { return };
        for id in ids {
            let _ = c.execute("DELETE FROM scrobble_queue WHERE id = ?", [id]);
        }
    }

    /// Listens that could not be sent this time.
    pub fn listens_failed(&self, ids: &[i64], error: &str) {
        let Ok(c) = self.conn() else { return };
        for id in ids {
            let _ = c.execute(
                "UPDATE scrobble_queue SET attempts = attempts + 1, last_error = ? WHERE id = ?",
                params![error, id],
            );
        }
    }

    /// Listens waiting for an account, and the last error sending them.
    pub fn listens_pending(&self, account: &str) -> (i64, Option<String>) {
        let Ok(c) = self.conn() else {
            return (0, None);
        };
        c.query_row(
            "SELECT COUNT(*), (SELECT last_error FROM scrobble_queue
                               WHERE account = ?1 AND last_error IS NOT NULL ORDER BY id DESC LIMIT 1)
             FROM scrobble_queue WHERE account = ?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, None))
    }

    /// Forget an account's waiting listens (the account was removed).
    pub fn drop_listens(&self, account: &str) {
        if let Ok(c) = self.conn() {
            let _ = c.execute("DELETE FROM scrobble_queue WHERE account = ?", [account]);
        }
    }
}
