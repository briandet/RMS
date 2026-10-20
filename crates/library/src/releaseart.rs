//! The Cover Art Archive's front cover for tracks without art of their own.  A track's cover
//! comes from a release: its album's, when the album is confidently matched to its MusicBrainz
//! release ([`crate::releases`]); else, for a track on its own (no album tag, an album not
//! surely identified), its recording's release: the one its album tag names, or the
//! recording's original album ([`pick_recording_release`]).  The server fetches the releases'
//! front covers (`plugins::musicbrainz`), kept here in the cache folder by release.
//!
//! The files are never written, and a track's own art wins: an embedded picture, or one in its
//! folder when the folder holds that one album.  A folder is a hint, not what identifies a
//! track: in a folder of several albums' files (or of loose tracks), a picture there is not
//! each track's own, and identified tracks show their releases' covers instead.

use crate::Library;
use rusqlite::{params, OptionalExtension};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// A release the archive had nothing for is asked again after this long.
const RETRY_SECS: i64 = 30 * 86400;

/// A track as far as its art goes.
struct TrackArt {
    id: i64,
    url: String,
    dir: Option<PathBuf>,
    album_id: Option<i64>,
    cover: Option<String>,
    cover_id: Option<String>,
    fingerprint: Option<String>,
}

struct ArtContext {
    tracks: Vec<TrackArt>,
    /// The albums of each folder's tracks.
    dirs: HashMap<PathBuf, BTreeSet<Option<i64>>>,
    /// The album of the tracks without an album tag.
    no_album: Option<i64>,
}

impl ArtContext {
    /// The folder holds one album's tracks only (not loose tracks).
    fn one_album(&self, dir: &Path) -> bool {
        self.dirs
            .get(dir)
            .is_some_and(|a| a.len() == 1 && a.iter().all(|x| x.is_some() && *x != self.no_album))
    }

    /// The track has art of its own: embedded, or its one-album folder's picture.
    fn own_art(&self, lib: &Library, t: &TrackArt) -> bool {
        match t.cover.as_deref() {
            None | Some("") | Some("0") => false,
            Some(c) if c.chars().all(|d| d.is_ascii_digit()) => true,
            Some(c) if lib.is_release_art(c) => false,
            Some(_) => t.dir.as_deref().is_some_and(|d| self.one_album(d)),
        }
    }
}

/// Which release's cover each track (by fingerprint) would show: its confidently matched
/// album's; and the one chosen for it on its own, with that release's group.
#[allow(clippy::type_complexity)]
fn covers_by_release(
    c: &rusqlite::Connection,
) -> rusqlite::Result<(
    HashMap<String, String>,
    HashMap<String, (String, Option<String>)>,
)> {
    let by_album = c
        .prepare(&format!(
            "SELECT r.fingerprint, r.release_mbid FROM track_release r
             WHERE r.release_mbid IS NOT NULL AND {}",
            confident()
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let by_track = c
        .prepare(
            "SELECT fingerprint, release_mbid, release_group FROM track_cover_release
             WHERE release_mbid IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((by_album, by_track))
}

/// A loose track to choose a cover for: its recording, and the album its tags name.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackCoverJob {
    pub fingerprint: String,
    pub recording: String,
    pub album: Option<String>,
}

/// The release of a recording whose front cover should stand for it, from MusicBrainz's
/// releases of it (`/ws/2/release?recording=…&inc=release-groups`): official ones only; the
/// album its tags name (`album`), else its original album (the earliest release of an album
/// release group: no compilation, live album or soundtrack), else its earliest single or EP.
/// `(release, release group)`.
pub fn pick_recording_release(
    album: Option<&str>,
    releases: &serde_json::Value,
) -> Option<(String, Option<String>)> {
    struct R<'a> {
        id: &'a str,
        group: Option<&'a str>,
        title: &'a str,
        date: &'a str,
        primary: &'a str,
        plain: bool,
    }
    let all: Vec<R> = releases["releases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["status"].as_str().is_none_or(|s| s == "Official"))
        .filter_map(|r| {
            let g = &r["release-group"];
            Some(R {
                id: r["id"].as_str()?,
                group: g["id"].as_str(),
                title: r["title"].as_str().unwrap_or_default(),
                // (undated last)
                date: r["date"]
                    .as_str()
                    .filter(|d| !d.is_empty())
                    .unwrap_or("9999"),
                primary: g["primary-type"].as_str().unwrap_or_default(),
                plain: g["secondary-types"].as_array().is_none_or(|t| t.is_empty()),
            })
        })
        .collect();
    let earliest = |f: &dyn Fn(&R) -> bool| all.iter().filter(|r| f(r)).min_by_key(|r| r.date);
    let named = album.filter(|a| !a.trim().is_empty()).and_then(|a| {
        earliest(&|r: &R| {
            crate::releases::title_similarity(a, r.title) >= crate::releases::TITLE_AGREES
        })
    });
    named
        .or_else(|| earliest(&|r: &R| r.primary == "Album" && r.plain))
        .or_else(|| earliest(&|r: &R| matches!(r.primary, "Single" | "EP") && r.plain))
        .map(|r| (r.id.to_string(), r.group.map(str::to_string)))
}

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
        self.try_release_art_pending(n).unwrap_or_default()
    }

    fn try_release_art_pending(&self, n: usize) -> anyhow::Result<Vec<ArtJob>> {
        let ctx = self.art_context()?;
        let c = self.conn()?;
        let (by_album, by_track) = covers_by_release(&c)?;
        let cutoff = crate::scanner::now() - RETRY_SECS;
        let mut asked = c.prepare(
            "SELECT 1 FROM release_art WHERE release_mbid = ?1 AND (path IS NOT NULL OR fetched_at >= ?2)",
        )?;
        let mut out: Vec<ArtJob> = Vec::new();
        for t in &ctx.tracks {
            if out.len() >= n {
                break;
            }
            let Some(fp) = &t.fingerprint else { continue };
            if ctx.own_art(self, t) {
                continue;
            }
            // the album's release, else the one chosen for the track (with its release group)
            let (release, group) = match (by_album.get(fp), by_track.get(fp)) {
                (Some(r), _) => (r.clone(), None),
                (None, Some((r, g))) => (r.clone(), g.clone()),
                _ => continue,
            };
            if out.iter().any(|j| j.release_mbid == release)
                || asked.exists(params![release, cutoff])?
            {
                continue;
            }
            out.push(ArtJob {
                release_group: group.or_else(|| {
                    crate::releases::cached_release(&c, &release, 0)
                        .map(|r| r.release_group)
                        .filter(|g| !g.is_empty())
                }),
                release_mbid: release,
            });
        }
        Ok(out)
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

    /// Give tracks without art of their own their release's front cover, and give it up where
    /// they have some now.  Returns the tracks changed.
    pub fn apply_release_art(&self) -> usize {
        self.try_apply_release_art().unwrap_or_else(|e| {
            tracing::warn!("cover art from the archive: {e}");
            0
        })
    }

    fn try_apply_release_art(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        // a picture in a one-album folder now: it takes over
        let ctx = self.art_context()?;
        let cover_art = self.config().cover_art;
        let dirs: BTreeSet<PathBuf> = ctx
            .tracks
            .iter()
            .filter(|t| t.cover.as_deref().is_some_and(|c| self.is_release_art(c)))
            .filter_map(|t| t.dir.clone())
            .filter(|d| ctx.one_album(d))
            .collect();
        for d in dirs {
            if crate::scanner::find_standalone_artwork(&d, &cover_art).is_some() {
                self.refresh_folder_art(&d)?;
                n += 1;
            }
        }
        // the cover each track without art of its own should show
        let ctx = self.art_context()?;
        let c = self.conn()?;
        let art: HashMap<String, String> = c
            .prepare("SELECT release_mbid, path FROM release_art WHERE path IS NOT NULL")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let (by_album, by_track) = covers_by_release(&c)?;
        let mut first: HashMap<i64, String> = HashMap::new();
        for t in &ctx.tracks {
            if ctx.own_art(self, t) {
                continue;
            }
            let Some(fp) = &t.fingerprint else { continue };
            let Some(path) = by_album
                .get(fp)
                .or_else(|| by_track.get(fp).map(|(r, _)| r))
                .and_then(|m| art.get(m))
            else {
                continue;
            };
            let Some((m, sz, _)) = crate::scanner::file_stat(Path::new(path)) else {
                continue;
            };
            let cid = crate::scanner::image_id(&format!("{}#archive", t.url), m, sz);
            if let (Some(a), Some(cid)) = (t.album_id, &cid) {
                first.entry(a).or_insert_with(|| cid.clone());
            }
            if t.cover.as_deref() == Some(path.as_str()) && t.cover_id == cid {
                continue;
            }
            c.execute(
                "UPDATE tracks SET cover = ?, cover_id = ? WHERE id = ?",
                params![path, cid, t.id],
            )?;
            if let Some(old) = &t.cover_id {
                c.execute(
                    "UPDATE albums SET artwork = ? WHERE artwork = ?",
                    params![cid, old],
                )?;
            }
            n += 1;
        }
        // albums without a picture: their first track's (not the pile of loose tracks)
        for (album, cid) in first {
            if Some(album) != ctx.no_album {
                c.execute(
                    "UPDATE albums SET artwork = ? WHERE id = ? AND artwork IS NULL",
                    params![cid, album],
                )?;
            }
        }
        Ok(n)
    }

    /// The library's tracks as far as their art goes, and which albums each folder holds.
    fn art_context(&self) -> anyhow::Result<ArtContext> {
        let c = self.conn()?;
        let no_album: Option<i64> = c
            .query_row(
                "SELECT id FROM albums WHERE title_search = ?",
                [self.no_album_key()],
                |r| r.get(0),
            )
            .optional()?;
        let tracks: Vec<TrackArt> = c
            .prepare("SELECT id, url, album_id, cover, cover_id, fingerprint FROM tracks")?
            .query_map([], |r| {
                let url: String = r.get(1)?;
                Ok(TrackArt {
                    id: r.get(0)?,
                    dir: crate::url::path_from_file_url(&url)
                        .and_then(|p| p.parent().map(Path::to_path_buf)),
                    url,
                    album_id: r.get(2)?,
                    cover: r.get(3)?,
                    cover_id: r.get(4)?,
                    fingerprint: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut dirs: HashMap<PathBuf, BTreeSet<Option<i64>>> = HashMap::new();
        for t in &tracks {
            if let Some(d) = &t.dir {
                dirs.entry(d.clone()).or_default().insert(t.album_id);
            }
        }
        Ok(ArtContext {
            tracks,
            dirs,
            no_album,
        })
    }

    /// Releases whose cover should stand for loose tracks: up to `n` tracks with a recording
    /// id, without art of their own, not on a confidently matched album (or on none: no album
    /// tag), whose release hasn't been chosen (or none fitted a while ago).  Tracks of albums
    /// still being matched wait.
    pub fn track_cover_pending(&self, n: usize) -> Vec<TrackCoverJob> {
        let Ok(ctx) = self.art_context() else {
            return Vec::new();
        };
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let cutoff = crate::scanner::now() - RETRY_SECS;
        let mut st = match c.prepare(&format!(
            "SELECT COALESCE(t.mbid, m.mbid), al.title,
                    EXISTS (SELECT 1 FROM track_release r WHERE r.fingerprint = t.fingerprint
                            AND r.release_mbid IS NOT NULL AND {conf}),
                    EXISTS (SELECT 1 FROM track_release r WHERE r.fingerprint = t.fingerprint),
                    EXISTS (SELECT 1 FROM track_cover_release x WHERE x.fingerprint = t.fingerprint
                            AND (x.release_mbid IS NOT NULL OR x.chosen_at >= ?2))
             FROM tracks t
             LEFT JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
             LEFT JOIN albums al ON al.id = t.album_id
             WHERE t.id = ?1",
            conf = confident()
        )) {
            Ok(st) => st,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for t in &ctx.tracks {
            if out.len() >= n {
                break;
            }
            let Some(fp) = &t.fingerprint else { continue };
            if ctx.own_art(self, t) || out.iter().any(|j: &TrackCoverJob| &j.fingerprint == fp) {
                continue;
            }
            let row = st.query_row(params![t.id, cutoff], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            });
            let Ok((Some(recording), album, confident, matched, chosen)) = row else {
                continue;
            };
            let loose = t.album_id.is_none() || t.album_id == ctx.no_album;
            if confident || chosen || (!loose && !matched) {
                continue;
            }
            out.push(TrackCoverJob {
                fingerprint: fp.clone(),
                recording,
                album: album.filter(|_| !loose),
            });
        }
        out
    }

    /// Keep the release whose cover stands for a loose track (`None`: none fits).
    pub fn store_track_cover_release(
        &self,
        fingerprint: &str,
        release: Option<(&str, Option<&str>)>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO track_cover_release
               (fingerprint, release_mbid, release_group, chosen_at) VALUES (?, ?, ?, ?)",
            params![
                fingerprint,
                release.map(|r| r.0),
                release.and_then(|r| r.1),
                crate::scanner::now()
            ],
        )?;
        Ok(())
    }

    /// The album holds the tracks without an album tag (not an album: loose tracks).
    pub fn is_no_album(&self, album_id: i64) -> bool {
        let Ok(c) = self.conn() else { return false };
        c.query_row(
            "SELECT 1 FROM albums WHERE id = ? AND title_search = ?",
            params![album_id, self.no_album_key()],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// The folder's tracks are all of this one album (so a picture there is the album's).
    pub fn folder_is_album(&self, dir: &Path, album_id: i64) -> bool {
        if self.is_no_album(album_id) {
            return false;
        }
        let files = self.audio_files_in(dir);
        let Ok(c) = self.conn() else { return false };
        files.iter().all(|u| {
            c.query_row(
                "SELECT 1 FROM tracks WHERE (url = ?1 OR url LIKE ?1 || '#%') AND album_id = ?2",
                params![u, album_id],
                |_| Ok(()),
            )
            .is_ok()
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn releases() -> serde_json::Value {
        let r =
            |id: &str, title: &str, date: &str, primary: &str, secondary: &[&str], status: &str| {
                json!({"id": id, "title": title, "date": date, "status": status,
                   "release-group": {"id": format!("g-{id}"), "primary-type": primary,
                                     "secondary-types": secondary}})
            };
        json!({"releases": [
            r("hits", "Greatest Hits", "2002", "Album", &["Compilation"], "Official"),
            r("single", "Smells Like Teen Spirit", "1991-09-10", "Single", &[], "Official"),
            r("reissue", "Nevermind", "2011-09-19", "Album", &[], "Official"),
            r("album", "Nevermind", "1991-09-24", "Album", &[], "Official"),
            r("boot", "Live at the Paramount", "1990", "Album", &[], "Bootleg"),
            r("live", "From the Muddy Banks", "1996", "Album", &["Live"], "Official")
        ]})
    }

    #[test]
    fn a_loose_track_shows_its_original_album() {
        let r = releases();
        // no album tag: the earliest plain album (not the single before it, the compilation,
        // the live album or a bootleg)
        assert_eq!(pick_recording_release(None, &r).unwrap().0, "album");
        // the album its tags name, earliest edition
        assert_eq!(
            pick_recording_release(Some("Nevermind"), &r).unwrap().0,
            "album"
        );
        assert_eq!(
            pick_recording_release(Some("Greatest Hits"), &r).unwrap(),
            ("hits".to_string(), Some("g-hits".to_string()))
        );
        // a name nothing has: as without one
        assert_eq!(
            pick_recording_release(Some("My Mixtape"), &r).unwrap().0,
            "album"
        );
        // only singles: the single; only a compilation: nothing (its cover isn't the song's)
        let only = |ids: &[&str]| {
            let v = r["releases"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|x| ids.contains(&x["id"].as_str().unwrap()))
                .cloned()
                .collect::<Vec<_>>();
            pick_recording_release(None, &json!({"releases": v})).map(|x| x.0)
        };
        assert_eq!(only(&["single", "hits"]).as_deref(), Some("single"));
        assert_eq!(only(&["hits", "live"]), None);
    }
}
