//! Changing the tags of music files (pref `editFiles`, off by default; not part of LMS): an
//! album's tags as they are next to what its MusicBrainz release says, written when the
//! user applies them, and undone on request.  Only with a login (`authorize`, a user name
//! and a password), since anyone who can reach the server could otherwise rewrite the
//! music.  The writing itself is `rms_library::tagwrite`'s; this checks, keeps the journal
//! of changes, carries what the library keeps by fingerprint over to the rewritten files and
//! rescans their folders.

use crate::server::Server;
use rms_library::releases::{Release, ReleaseTrack};
use rms_library::tagwrite::{self, Edit, Values};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

pub const PREF: &str = "editFiles";

/// One change at a time: two writes to the same file must not interleave.
static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
pub enum Error {
    NotFound,
    Invalid(String),
    /// File changes are off, or need a login.
    Forbidden(String),
    Failed(String),
}

/// Why music files can't be changed now, if they can't.
pub fn blocked(server: &Server) -> Option<String> {
    let p = &server.prefs.server;
    if !p.get_bool(PREF) {
        return Some("Changing music files is off (setting edit_files).".into());
    }
    if !p.get_bool("authorize")
        || p.get_str("username").is_empty()
        || p.get_str("password").is_empty()
    {
        return Some(
            "Changing music files needs a login: set a user name and password and turn on \
             password protection."
                .into(),
        );
    }
    None
}

/// Why this track's file can't be changed, if it can't.
fn track_path(server: &Server, track_id: i64) -> Result<(PathBuf, tagwrite::TrackFile), Error> {
    let tf = server.library.track_file(track_id).ok_or(Error::NotFound)?;
    if tf.segment {
        return Err(Error::Invalid(
            "a track of a cue sheet: its tags are in the cue sheet".into(),
        ));
    }
    let path = crate::util::path_from_file_url(&tf.url)
        .ok_or_else(|| Error::Invalid("not a local file".into()))?;
    let dirs = server.prefs.server.get_string_list("mediadirs");
    if !dirs.iter().any(|d| path.starts_with(d)) {
        return Err(Error::Invalid("not in a music folder".into()));
    }
    Ok((path, tf))
}

/// A track's tags: the field, its value in the file, what the release says.
pub struct TrackTags {
    pub track_id: i64,
    pub path: String,
    /// Why the file can't be changed, if it can't.
    pub error: Option<String>,
    pub tags: Vec<(String, Option<String>, Option<String>)>,
}

/// The album's tracks' tags, next to what `release` (the album's match when `None`) says.
pub fn proposal(
    server: &Arc<Server>,
    album_id: i64,
    release: Option<&str>,
) -> Result<(Option<Release>, Vec<TrackTags>), Error> {
    let lib = &server.library;
    let job = lib.album_job(album_id).ok_or(Error::NotFound)?;
    let matched = lib.album_match(album_id);
    let release = match release {
        Some(id) => Some(lib.cached_release(id, i64::MAX).ok_or_else(|| {
            Error::Invalid(format!(
                "release {id} is not known: choose it for the album first"
            ))
        })?),
        None => matched.as_ref().and_then(|m| m.release.clone()),
    };
    // which release track each library track is: as matched, or scored afresh
    let positions: Vec<(i64, Option<(i64, i64)>)> = match (&release, &matched) {
        (Some(r), Some(m)) if m.release_mbid.as_deref() == Some(&r.mbid) => m.tracks.clone(),
        (Some(r), _) => {
            let scored = rms_library::releases::score(&job, r);
            let ids = album_track_ids(server, album_id, &job);
            ids.into_iter().zip(scored.tracks).collect()
        }
        (None, _) => album_track_ids(server, album_id, &job)
            .into_iter()
            .map(|id| (id, None))
            .collect(),
    };
    let mut out = Vec::new();
    for (track_id, pos) in positions {
        let (path, error, current) = match track_path(server, track_id) {
            Ok((path, _)) => {
                let error = tagwrite::check_writable(&path).err();
                let current = tagwrite::read(&path).unwrap_or_default();
                (path.display().to_string(), error, current)
            }
            Err(e) => (String::new(), Some(error_text(&e)), Values::new()),
        };
        let proposed = match (&release, pos) {
            (Some(r), Some((medium, position))) => release_values(r, medium, position),
            _ => Values::new(),
        };
        let tags = tagwrite::FIELDS
            .iter()
            .map(|f| {
                (
                    f.name.to_string(),
                    current.get(f.name).cloned().flatten(),
                    proposed.get(f.name).cloned().flatten(),
                )
            })
            .collect();
        out.push(TrackTags {
            track_id,
            path,
            error,
            tags,
        });
    }
    Ok((release, out))
}

/// The album's tracks in the order of `job.tracks` (by fingerprint).
fn album_track_ids(
    server: &Server,
    album_id: i64,
    job: &rms_library::releases::AlbumJob,
) -> Vec<i64> {
    let ids = server.library.album_track_ids(album_id);
    job.tracks
        .iter()
        .filter_map(|t| {
            ids.iter()
                .find(|(_, fp)| fp.as_deref() == Some(&t.fingerprint))
                .map(|(id, _)| *id)
        })
        .collect()
}

fn error_text(e: &Error) -> String {
    match e {
        Error::NotFound => "no such track".into(),
        Error::Invalid(m) | Error::Forbidden(m) | Error::Failed(m) => m.clone(),
    }
}

/// What the release says a track's tags are.
pub fn release_values(r: &Release, medium: i64, position: i64) -> Values {
    let Some(m) = r.media.iter().find(|m| m.position == medium) else {
        return Values::new();
    };
    let Some(t): Option<&ReleaseTrack> = m.tracks.iter().find(|t| t.position == position) else {
        return Values::new();
    };
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let pairs: [(&str, Option<String>); 19] = [
        ("title", some(&t.title)),
        ("artist", some(&t.artist)),
        ("album", some(&r.title)),
        ("album_artist", some(&r.artist)),
        ("track_number", Some(position.to_string())),
        ("track_total", Some(m.tracks.len().to_string())),
        ("disc_number", Some(medium.to_string())),
        ("disc_total", Some(r.media.len().to_string())),
        ("date", some(&r.date)),
        ("original_date", some(&r.original_date)),
        ("label", some(&r.label)),
        ("catalog_number", some(&r.catalog_number)),
        ("barcode", some(&r.barcode)),
        ("musicbrainz_recording_id", some(&t.recording)),
        ("musicbrainz_track_id", some(&t.mbid)),
        ("musicbrainz_release_id", some(&r.mbid)),
        ("musicbrainz_release_group_id", some(&r.release_group)),
        (
            "musicbrainz_artist_id",
            t.artist_mbids.first().and_then(|s| some(s)),
        ),
        (
            "musicbrainz_album_artist_id",
            r.artist_mbids.first().and_then(|s| some(s)),
        ),
    ];
    pairs
        .into_iter()
        .filter(|(_, v)| v.is_some())
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// What applying or undoing did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The journal entry of the changes (to undo them).
    pub batch: Option<i64>,
    pub changed: usize,
    pub failed: Vec<(i64, String)>,
}

/// Write these tags (`None`: remove) into the tracks' files.
pub async fn apply(server: &Arc<Server>, changes: Vec<(i64, Values)>) -> Result<Outcome, Error> {
    if let Some(why) = blocked(server) {
        return Err(Error::Forbidden(why));
    }
    for (_, v) in &changes {
        if let Some(k) = v.keys().find(|k| tagwrite::field(k).is_none()) {
            return Err(Error::Invalid(format!("unknown tag {k}")));
        }
    }
    let _guard = WRITING.lock().await;
    let s = server.clone();
    let (outcome, dirs) = tokio::task::spawn_blocking(move || {
        let scratch = s.config.data_dir.join("tag-backup");
        let mut out = Outcome::default();
        let mut edits = Vec::new();
        let mut album = None;
        let mut dirs = BTreeSet::new();
        for (track_id, values) in changes {
            let (path, tf) = match track_path(&s, track_id) {
                Ok(x) => x,
                Err(e) => {
                    out.failed.push((track_id, error_text(&e)));
                    continue;
                }
            };
            match write_one(&s, &path, tf.fingerprint.as_deref(), &values, &scratch) {
                Ok(Some(before)) => {
                    album = album.or(tf.album.clone());
                    edits.push(Edit {
                        url: tf.url,
                        before,
                        after: values,
                    });
                    if let Some(d) = path.parent() {
                        dirs.insert(d.to_path_buf());
                    }
                    out.changed += 1;
                }
                Ok(None) => {}
                Err(e) => out.failed.push((track_id, e)),
            }
        }
        if !edits.is_empty() {
            match s.library.record_edits(album.as_deref(), &edits) {
                Ok(b) => out.batch = Some(b),
                Err(e) => warn!("tag changes: keeping the journal: {e}"),
            }
        }
        (out, dirs)
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    info!(
        "tags written to {} files ({} failed)",
        outcome.changed,
        outcome.failed.len()
    );
    rescan(server, dirs);
    Ok(outcome)
}

/// Write one file's tags; returns its tags as they were (`None`: nothing to change).
fn write_one(
    server: &Server,
    path: &Path,
    fingerprint: Option<&str>,
    values: &Values,
    scratch: &Path,
) -> Result<Option<Values>, String> {
    tagwrite::check_writable(path)?;
    let current = tagwrite::read(path).map_err(|e| e.to_string())?;
    let normalize = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let changes: Values = values
        .iter()
        .filter(|(k, v)| normalize(v) != current.get(*k).and_then(normalize))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if changes.is_empty() {
        return Ok(None);
    }
    let before = changes
        .keys()
        .map(|k| (k.clone(), current.get(k).cloned().flatten()))
        .collect();
    tagwrite::write(path, &changes, scratch).map_err(|e| format!("{e:#}"))?;
    // what the library keeps by fingerprint follows the rewritten file
    if let (Some(old), Some(new)) = (fingerprint, tagwrite::file_fingerprint(path)) {
        if let Err(e) = server.library.refingerprint(old, &new) {
            warn!("tag changes: {}: {e}", path.display());
        }
    }
    Ok(Some(before))
}

/// Put back the tags a batch of changes wrote.  A file whose tags changed again since is
/// left as it is.
pub async fn undo(server: &Arc<Server>, batch: i64) -> Result<Outcome, Error> {
    if let Some(why) = blocked(server) {
        return Err(Error::Forbidden(why));
    }
    let _guard = WRITING.lock().await;
    let s = server.clone();
    let (outcome, dirs) = tokio::task::spawn_blocking(move || {
        let (head, edits) = s.library.edit_batch(batch).ok_or(Error::NotFound)?;
        if head.undone_at.is_some() {
            return Err(Error::Invalid("already undone".into()));
        }
        let scratch = s.config.data_dir.join("tag-backup");
        let mut out = Outcome::default();
        let mut dirs = BTreeSet::new();
        for e in edits {
            let id = s.library.track_by_url(&e.url).map(|t| t.id).unwrap_or(0);
            let Some(path) = crate::util::path_from_file_url(&e.url) else {
                continue;
            };
            let now = match tagwrite::read(&path) {
                Ok(v) => v,
                Err(err) => {
                    out.failed.push((id, format!("{}: {err}", path.display())));
                    continue;
                }
            };
            let same = e.after.iter().all(|(k, v)| {
                now.get(k).cloned().flatten().as_deref() == v.as_deref().map(str::trim)
            });
            if !same {
                out.failed.push((
                    id,
                    format!("{}: changed since; left as it is", path.display()),
                ));
                continue;
            }
            let fp = s.library.track_file(id).and_then(|t| t.fingerprint);
            match write_one(&s, &path, fp.as_deref(), &e.before, &scratch) {
                Ok(_) => {
                    out.changed += 1;
                    if let Some(d) = path.parent() {
                        dirs.insert(d.to_path_buf());
                    }
                }
                Err(err) => out.failed.push((id, err)),
            }
        }
        if let Err(e) = s.library.mark_undone(batch) {
            warn!("tag changes: {e}");
        }
        Ok((out, dirs))
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))??;
    rescan(server, dirs);
    Ok(outcome)
}

/// Scan the folders of rewritten files, once the scanner is free (the folder watcher may
/// see them too: no harm).
fn rescan(server: &Arc<Server>, dirs: BTreeSet<PathBuf>) {
    if dirs.is_empty() {
        return;
    }
    let s = server.clone();
    tokio::spawn(async move {
        for _ in 0..1200 {
            if crate::control::commands::claim_scan(&s) {
                let modes = dirs.into_iter().map(rms_library::ScanMode::Dir).collect();
                crate::control::commands::run_scans(&s, modes).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        warn!("tag changes: the scanner stayed busy; rescan to see them");
    });
}
