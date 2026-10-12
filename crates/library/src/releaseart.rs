//! The Cover Art Archive's front cover for albums without art of their own.  Once an album is
//! confidently matched to its MusicBrainz release ([`crate::releases`]) and none of its tracks
//! has a picture (embedded, or in the folder), the server fetches the release's front cover
//! (`plugins::musicbrainz`), kept here in the cache folder by release, and the album's tracks
//! take it as their cover.  The files are never written; the album's own art always wins: a
//! picture put in the folder later takes over (see [`Library::apply_release_art`]), and a
//! file with an embedded one is rescanned with it.

use crate::Library;
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A release the archive had nothing for is asked again after this long.
const RETRY_SECS: i64 = 30 * 86400;

/// A release whose front cover to fetch, and its release group (the archive's fallback).
#[derive(Debug, Clone, PartialEq)]
pub struct ArtJob {
    pub release_mbid: String,
    pub release_group: Option<String>,
}

/// Confident matches only (as `releases::CONFIDENT` and the user's or the tags' choice).
fn confident() -> String {
    format!(
        "(r.how IN ('user', 'tag') OR r.score >= {})",
        crate::releases::CONFIDENT
    )
}

impl Library {
    /// Where the archive's covers are kept.
    pub fn release_art_dir(&self) -> PathBuf {
        self.cache_dir().join("release-art")
    }

    fn is_release_art(&self, cover: &str) -> bool {
        Path::new(cover).starts_with(self.release_art_dir())
    }

    /// Up to `n` releases to fetch a front cover for: confidently matched albums without art,
    /// whose release hasn't been asked (or had nothing a while ago).
    pub fn release_art_pending(&self, n: usize) -> Vec<ArtJob> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let cutoff = crate::scanner::now() - RETRY_SECS;
        let ids: Vec<String> = c
            .prepare(&format!(
                "SELECT DISTINCT r.release_mbid FROM albums al
                 JOIN tracks t ON t.album_id = al.id
                 JOIN track_release r ON r.fingerprint = t.fingerprint
                 WHERE al.artwork IS NULL AND r.release_mbid IS NOT NULL AND {}
                   AND NOT EXISTS (SELECT 1 FROM release_art a WHERE a.release_mbid = r.release_mbid
                                   AND (a.path IS NOT NULL OR a.fetched_at >= ?1))
                 LIMIT {n}",
                confident()
            ))
            .and_then(|mut st| st.query_map([cutoff], |r| r.get(0))?.collect())
            .unwrap_or_default();
        ids.into_iter()
            .map(|m| ArtJob {
                release_group: self
                    .cached_release(&m, i64::MAX)
                    .map(|r| r.release_group)
                    .filter(|g| !g.is_empty()),
                release_mbid: m,
            })
            .collect()
    }

    /// Keep a release's front cover (`None`: the archive has none).
    pub fn store_release_art(
        &self,
        release: &str,
        image: Option<(&[u8], &str)>,
    ) -> anyhow::Result<()> {
        let path = match image {
            Some((data, mime)) => {
                let dir = self.release_art_dir();
                std::fs::create_dir_all(&dir)?;
                let ext = if mime == "image/png" { "png" } else { "jpg" };
                // (only an id, checked: nothing else becomes a file name)
                anyhow::ensure!(
                    release.len() == 36
                        && release.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                    "not a release id: {release}"
                );
                let p = dir.join(format!("{release}.{ext}"));
                std::fs::write(&p, data)?;
                Some(p.to_string_lossy().into_owned())
            }
            None => None,
        };
        self.conn()?.execute(
            "INSERT OR REPLACE INTO release_art (release_mbid, path, fetched_at) VALUES (?, ?, ?)",
            params![release, path, crate::scanner::now()],
        )?;
        Ok(())
    }

    /// Give confidently matched albums without art their release's front cover, and give it
    /// up where the album has art of its own now.  Returns the tracks changed.
    pub fn apply_release_art(&self) -> usize {
        self.try_apply_release_art().unwrap_or_else(|e| {
            tracing::warn!("cover art from the archive: {e}");
            0
        })
    }

    fn try_apply_release_art(&self) -> anyhow::Result<usize> {
        let c = self.conn()?;
        // a picture in the folder now: it takes over
        let dirs: BTreeSet<PathBuf> = c
            .prepare(
                "SELECT url, cover FROM tracks WHERE cover IS NOT NULL AND cover NOT IN ('', '0')",
            )?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(Result::ok)
            .filter(|(_, cover)| self.is_release_art(cover))
            .filter_map(|(u, _)| {
                crate::url::path_from_file_url(&u)?
                    .parent()
                    .map(Path::to_path_buf)
            })
            .collect();
        let cover_art = self.config().cover_art;
        let mut n = 0;
        drop(c);
        for d in dirs {
            if crate::scanner::find_standalone_artwork(&d, &cover_art).is_some() {
                self.refresh_folder_art(&d)?;
                n += 1;
            }
        }
        // albums without art, confidently matched to a release with a front cover
        let c = self.conn()?;
        let rows: Vec<(i64, String)> = c
            .prepare(&format!(
                "SELECT DISTINCT al.id, a.path FROM albums al
                 JOIN tracks t ON t.album_id = al.id
                 JOIN track_release r ON r.fingerprint = t.fingerprint
                 JOIN release_art a ON a.release_mbid = r.release_mbid
                 WHERE al.artwork IS NULL AND a.path IS NOT NULL AND {}",
                confident()
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (album, path) in rows {
            let Some((m, s, _)) = crate::scanner::file_stat(Path::new(&path)) else {
                continue;
            };
            let tracks: Vec<(i64, String)> = c
                .prepare(
                    "SELECT id, url FROM tracks WHERE album_id = ?
                     AND (cover IS NULL OR cover IN ('', '0'))",
                )?
                .query_map([album], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut first = None;
            for (id, u) in tracks {
                let cid = crate::scanner::image_id(&format!("{u}#archive"), m, s);
                c.execute(
                    "UPDATE tracks SET cover = ?, cover_id = ? WHERE id = ?",
                    params![path, cid, id],
                )?;
                first = first.or(cid);
                n += 1;
            }
            if let Some(cid) = first {
                c.execute(
                    "UPDATE albums SET artwork = ? WHERE id = ? AND artwork IS NULL",
                    params![cid, album],
                )?;
            }
        }
        Ok(n)
    }

    /// Whether the album's picture is the archive's (not in its files).
    pub fn album_uses_release_art(&self, album_id: i64) -> bool {
        let Ok(c) = self.conn() else { return false };
        c.query_row(
            "SELECT t.cover FROM albums al JOIN tracks t ON t.cover_id = al.artwork
             WHERE al.id = ? LIMIT 1",
            [album_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
        .is_some_and(|c| self.is_release_art(&c))
    }
}
