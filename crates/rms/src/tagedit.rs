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
            match s.library.record_edits("tags", album.as_deref(), &edits) {
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
    let kind = server
        .library
        .edit_batch(batch)
        .map(|(h, _)| h.kind)
        .ok_or(Error::NotFound)?;
    if kind == "move" {
        return undo_moves(server, batch).await;
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
    let s = server.clone();
    tokio::spawn(async move { rescan_now(&s, dirs).await });
}

/// [`rescan`], waiting for it.  A folder that is gone is seen from its nearest parent.
async fn rescan_now(server: &Arc<Server>, dirs: BTreeSet<PathBuf>) {
    let mut existing: BTreeSet<PathBuf> = dirs
        .into_iter()
        .filter_map(|d| d.ancestors().find(|a| a.is_dir()).map(Path::to_path_buf))
        .collect();
    // a folder inside another is scanned with it
    let all: Vec<PathBuf> = existing.iter().cloned().collect();
    existing.retain(|d| !all.iter().any(|o| o != d && d.starts_with(o)));
    if existing.is_empty() {
        return;
    }
    for _ in 0..1200 {
        if crate::control::commands::claim_scan(server) {
            let modes = existing
                .into_iter()
                .map(rms_library::ScanMode::Dir)
                .collect();
            crate::control::commands::run_scans(server, modes).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    warn!("file changes: the scanner stayed busy; rescan to see them");
}

// ---------------------------------------------------------------- moving and renaming

/// Pref: where files go, relative to their music folder (`rms_library::organize`).
pub const TEMPLATE_PREF: &str = "fileNameTemplate";

pub fn template(server: &Server) -> String {
    let t = server.prefs.server.get_str(TEMPLATE_PREF);
    if t.trim().is_empty() {
        rms_library::organize::DEFAULT_TEMPLATE.to_string()
    } else {
        t
    }
}

/// One file of an album and where it belongs.
#[derive(Debug, Clone)]
pub struct Planned {
    /// `None` for a file that is not a track (cover, booklet, log) going with the album.
    pub track_id: Option<i64>,
    /// The music folder it is in.
    pub root: PathBuf,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Why it can't move, if it can't.
    pub error: Option<String>,
}

/// Where the album's files belong by `template`.  Other files in a folder whose audio all
/// moves to one place (covers, booklets, logs) go with it.
pub fn move_plan(server: &Server, album_id: i64, template: &str) -> Result<Vec<Planned>, Error> {
    use rms_library::organize::{self, Template};
    let tpl = Template::parse(template).map_err(Error::Invalid)?;
    let roots: Vec<PathBuf> = server
        .prefs
        .server
        .get_string_list("mediadirs")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let ids = server.library.album_track_ids(album_id);
    if ids.is_empty() {
        return Err(Error::NotFound);
    }
    let mut out: Vec<Planned> = Vec::new();
    let mut ok: Vec<(usize, Values)> = Vec::new();
    let mut seen = BTreeSet::new();
    for (id, _) in ids {
        let (path, err) = match track_path(server, id) {
            Ok((p, _)) => (p, None),
            Err(e) => {
                let path = server
                    .library
                    .track_file(id)
                    .and_then(|t| crate::util::path_from_file_url(&t.url))
                    .unwrap_or_default();
                (path, Some(error_text(&e)))
            }
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let root = roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.as_os_str().len())
            .cloned()
            .unwrap_or_default();
        let tags = if err.is_none() {
            tagwrite::read(&path).map_err(|e| format!("can't read its tags: {e}"))
        } else {
            Err(String::new())
        };
        let error = err.or_else(|| tags.as_ref().err().cloned());
        if let Ok(t) = tags {
            ok.push((out.len(), t));
        }
        out.push(Planned {
            track_id: Some(id),
            root,
            from: path.clone(),
            to: path,
            error,
        });
    }
    // placed together, so the album decides about discs
    let sources: Vec<organize::Source> = ok
        .iter()
        .map(|(i, t)| organize::Source {
            path: &out[*i].from,
            tags: t,
        })
        .collect();
    let placed = organize::place(&tpl, Path::new(""), &sources);
    for ((i, _), rel) in ok.iter().zip(placed) {
        out[*i].to = out[*i].root.join(rel);
    }
    // the other files of folders whose audio all moves to one folder
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for p in &out {
        if let Some(d) = p.from.parent() {
            dirs.insert(d.to_path_buf());
        }
    }
    for dir in dirs {
        let audio = server.library.audio_files_in(&dir);
        let here: Vec<&Planned> = out
            .iter()
            .filter(|p| p.from.parent() == Some(dir.as_path()))
            .collect();
        let moving_all = audio.iter().all(|u| {
            crate::util::path_from_file_url(u)
                .is_some_and(|a| here.iter().any(|p| p.from == a && p.error.is_none()))
        });
        let targets: BTreeSet<&Path> = here.iter().filter_map(|p| p.to.parent()).collect();
        if !moving_all || targets.len() != 1 {
            continue;
        }
        let target = targets.into_iter().next().unwrap().to_path_buf();
        if target == dir {
            continue;
        }
        let root = here[0].root.clone();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut extras: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .filter(|p| !here.iter().any(|h| h.from == *p))
            .collect();
        extras.sort();
        for e in extras {
            let to = target.join(e.file_name().unwrap_or_default());
            out.push(Planned {
                track_id: None,
                root: root.clone(),
                from: e,
                to,
                error: None,
            });
        }
    }
    // nowhere taken
    let mut taken: BTreeSet<PathBuf> = BTreeSet::new();
    for p in &mut out {
        if p.error.is_some() || p.from == p.to {
            continue;
        }
        if !p.to.starts_with(&p.root) {
            p.error = Some("outside its music folder".into());
        } else if !taken.insert(p.to.clone()) {
            p.error = Some(format!("another file goes to {} too", p.to.display()));
        } else if p.to.exists() && !same_file(&p.from, &p.to) {
            p.error = Some(format!("{} exists already", p.to.display()));
        }
    }
    Ok(out)
}

/// The same file under two names (a case-only rename on a case-insensitive disk).
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.ino() == y.ino() && x.dev() == y.dev(),
        _ => false,
    }
}

/// What moving did.
#[derive(Debug, Default)]
pub struct Moved {
    pub outcome: Outcome,
    /// The album after the rescan (its id may change).
    pub album_id: Option<i64>,
}

/// Move the album's files where `template` says.
pub async fn move_album(
    server: &Arc<Server>,
    album_id: i64,
    template: &str,
) -> Result<Moved, Error> {
    if let Some(why) = blocked(server) {
        return Err(Error::Forbidden(why));
    }
    let _guard = WRITING.lock().await;
    let s = server.clone();
    let template = template.to_string();
    let (outcome, moves, dirs) = tokio::task::spawn_blocking(move || {
        let plan = move_plan(&s, album_id, &template)?;
        let album = s
            .library
            .album_track_ids(album_id)
            .first()
            .and_then(|(id, _)| s.library.track_file(*id))
            .and_then(|t| t.album);
        let pairs: Vec<(Option<i64>, PathBuf, PathBuf, PathBuf)> = plan
            .into_iter()
            .filter(|p| p.from != p.to)
            .filter_map(|p| match p.error {
                Some(_) => None,
                None => Some((p.track_id, p.root, p.from, p.to)),
            })
            .collect();
        Ok::<_, Error>(do_moves(&s, true, album.as_deref(), &pairs))
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))??;
    follow_moves(server, &moves);
    rescan_now(server, dirs).await;
    let album_id = moves
        .iter()
        .find_map(|(_, new)| server.library.track_by_url(new))
        .and_then(|t| t.album_id)
        .or(Some(album_id));
    Ok(Moved { outcome, album_id })
}

/// Rename files (`(track, music folder, from, to)`), journal them (with `journal`), and
/// empty folders left behind go.  Returns what happened, the URL pairs moved and the folders to rescan.
#[allow(clippy::type_complexity)]
fn do_moves(
    server: &Server,
    journal: bool,
    album: Option<&str>,
    pairs: &[(Option<i64>, PathBuf, PathBuf, PathBuf)],
) -> (Outcome, Vec<(String, String)>, BTreeSet<PathBuf>) {
    let mut out = Outcome::default();
    let mut edits = Vec::new();
    let mut moves = Vec::new();
    let mut dirs = BTreeSet::new();
    for (track, root, from, to) in pairs {
        let res = (|| -> std::io::Result<()> {
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d)?;
            }
            if to.exists() && !same_file(from, to) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists already", to.display()),
                ));
            }
            std::fs::rename(from, to)
        })();
        if let Err(e) = res {
            out.failed
                .push((track.unwrap_or(0), format!("{}: {e}", from.display())));
            continue;
        }
        let (old, new) = (
            rms_library::url::file_url_from_path(from),
            rms_library::url::file_url_from_path(to),
        );
        let one = |u: &str| -> Values { [("url".to_string(), Some(u.to_string()))].into() };
        edits.push(Edit {
            url: new.clone(),
            before: one(&old),
            after: one(&new),
        });
        moves.push((old, new));
        for d in [from.parent(), to.parent()].into_iter().flatten() {
            dirs.insert(d.to_path_buf());
        }
        if track.is_some() {
            out.changed += 1;
        }
        remove_empty_dirs(from.parent(), root);
    }
    if !edits.is_empty() {
        if journal {
            match server.library.record_edits("move", album, &edits) {
                Ok(b) => out.batch = Some(b),
                Err(e) => warn!("file moves: keeping the journal: {e}"),
            }
        }
        if let Err(e) = server.library.rename_urls(&moves) {
            warn!("file moves: {e}");
        }
    }
    info!("moved {} files ({} failed)", edits.len(), out.failed.len());
    (out, moves, dirs)
}

/// Remove `dir` and its parents while they are empty, up to (not including) `root`.
fn remove_empty_dirs(dir: Option<&Path>, root: &Path) {
    let mut d = dir;
    while let Some(p) = d {
        if p == root || !p.starts_with(root) || std::fs::remove_dir(p).is_err() {
            break;
        }
        d = p.parent();
    }
}

/// Favorites and players' queues follow moved files.
fn follow_moves(server: &Server, moves: &[(String, String)]) {
    if moves.is_empty() {
        return;
    }
    let map: std::collections::HashMap<String, String> = moves.iter().cloned().collect();
    server.favorites.replace_urls(&map);
    let mut done: Vec<*const ()> = Vec::new();
    for p in server.players() {
        let c = p.controller();
        // players synced together share their controller
        let key = Arc::as_ptr(&c) as *const ();
        if done.contains(&key) {
            continue;
        }
        done.push(key);
        c.lock().replace_urls(&map);
    }
}

/// Move a batch of moved files back where they were.
async fn undo_moves(server: &Arc<Server>, batch: i64) -> Result<Outcome, Error> {
    let _guard = WRITING.lock().await;
    let s = server.clone();
    let (outcome, moves, dirs) = tokio::task::spawn_blocking(move || {
        let (head, edits) = s.library.edit_batch(batch).ok_or(Error::NotFound)?;
        if head.undone_at.is_some() {
            return Err(Error::Invalid("already undone".into()));
        }
        let roots: Vec<PathBuf> = s
            .prefs
            .server
            .get_string_list("mediadirs")
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let path = |v: &Values| {
            v.get("url")
                .cloned()
                .flatten()
                .and_then(|u| crate::util::path_from_file_url(&u))
        };
        let mut pairs = Vec::new();
        let mut out = Outcome::default();
        for e in edits.iter().rev() {
            let (Some(back), Some(now)) = (path(&e.before), path(&e.after)) else {
                continue;
            };
            let id = s.library.track_by_url(&e.url).map(|t| t.id);
            if !now.exists() {
                out.failed.push((
                    id.unwrap_or(0),
                    format!("{}: gone; left as it is", now.display()),
                ));
                continue;
            }
            let root = roots
                .iter()
                .filter(|r| now.starts_with(r))
                .max_by_key(|r| r.as_os_str().len())
                .cloned()
                .unwrap_or_default();
            pairs.push((Some(id.unwrap_or(0)), root, now, back));
        }
        let (mut o, moves, dirs) = do_moves(&s, false, head.album.as_deref(), &pairs);
        o.failed.extend(out.failed);
        if let Err(e) = s.library.mark_undone(batch) {
            warn!("file moves: {e}");
        }
        Ok((o, moves, dirs))
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))??;
    follow_moves(server, &moves);
    rescan_now(server, dirs).await;
    Ok(outcome)
}
