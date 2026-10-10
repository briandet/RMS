//! Library scanner: discovery, deleted/changed/new diffing and the LMS insert logic
//! (`Slim::Utils::Scanner::Local`, `Slim::Schema::_newTrack`, `_createOrUpdateAlbum`, ...).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use crate::config::*;
use crate::cue;
use crate::tags::{self, first, Attrs, AudioInfo, TagOptions};
use crate::text::{self, TextRules};
use crate::types;
use crate::url::{self, file_url_from_path, md5_hex, path_from_file_url};
use crate::{ScanProgress, ScanSummary};

const CHUNK: usize = 50;

/// Names that are always ignored by `fileFilter` (value 1 in `%_ignoredItems`).
const IGNORED_ITEMS: &[&str] = &[
    "#recycle",
    "#snapshot",
    ".AppleDouble",
    ".AppleDB",
    ".AppleDesktop",
    ".DS_Store",
    "ShoutcastBrowser_Recently_Played",
    "lost+found",
    "@eaDir",
    "@appstore",
    "@tmp",
    "@Recycle",
    ".Trashes",
    ".Metadata",
    "Temporary Items",
    "Network Trash Folder",
    "TheVolumeSettingsFolder",
    "TheFindByContentFolder",
    "System Volume Information",
    "RECYCLER",
    "Recycled",
    "$Recycle.Bin",
];

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn mtime_of(md: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    md.mtime()
}
#[cfg(not(unix))]
fn mtime_of(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn file_stat(p: &Path) -> Option<(i64, i64, bool)> {
    let md = std::fs::metadata(p).ok()?;
    Some((mtime_of(&md), md.len() as i64, md.is_dir()))
}

/// `fileFilter` – returns true when the entry should be included.
pub fn name_allowed(name: &str, ignore_re: Option<&regex::Regex>) -> bool {
    if IGNORED_ITEMS.contains(&name) {
        return false;
    }
    if name.starts_with("__")
        && name.to_ascii_lowercase().ends_with(".m3u")
        && !name.contains(char::is_whitespace)
    {
        return false;
    }
    // dot files / dirs: /^\.[^\.]+/
    if name.starts_with('.') && name.chars().nth(1).is_some_and(|c| c != '.') {
        return false;
    }
    if let Some(re) = ignore_re {
        if re.is_match(name) {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone)]
pub struct Found {
    pub url: String,
    pub path: PathBuf,
    pub mtime: i64,
    pub size: i64,
    pub is_dir: bool,
}

/// Directory walk honouring the LMS filters. `want(ext)` decides which files are accepted.
pub fn discover(
    root: &Path,
    cfg: &LibraryConfig,
    want: &dyn Fn(&str) -> bool,
    recursive: bool,
    abort: &AtomicBool,
) -> Vec<Found> {
    let ignore_re = cfg
        .ignore_dir_re
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| regex::Regex::new(s).ok());
    let mut out = Vec::new();
    let mut walker = walkdir::WalkDir::new(root)
        .follow_links(true)
        .sort_by(|a, b| a.file_name().cmp(b.file_name()))
        .min_depth(1);
    if !recursive {
        walker = walker.max_depth(1);
    }
    let sentinel = cfg.skip_sentinel.clone().filter(|s| !s.is_empty());
    let mut it = walker.into_iter();
    loop {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        let entry = match it.next() {
            None => break,
            Some(Err(e)) => {
                tracing::debug!("walk error: {}", e);
                continue;
            }
            Some(Ok(e)) => e,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type();
        if !name_allowed(&name, ignore_re.as_ref()) {
            if ft.is_dir() {
                it.skip_current_dir();
            }
            continue;
        }
        let path = entry.path().to_path_buf();
        if ft.is_dir() {
            if let Some(s) = &sentinel {
                if path.join(s).exists() {
                    it.skip_current_dir();
                    continue;
                }
            }
            let mtime = std::fs::metadata(&path).map(|m| mtime_of(&m)).unwrap_or(0);
            out.push(Found {
                url: file_url_from_path(&path),
                path,
                mtime,
                size: 0,
                is_dir: true,
            });
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        if !want(&ext) {
            continue;
        }
        let md = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if std::fs::File::open(&path).is_err() {
            continue;
        }
        out.push(Found {
            url: file_url_from_path(&path),
            path,
            mtime: mtime_of(&md),
            size: md.len() as i64,
            is_dir: false,
        });
    }
    out
}

#[derive(Debug, Clone, Default)]
struct LastAlbum {
    id: i64,
    title: String,
    disc: Option<i64>,
    dirname: String,
}

#[derive(Debug, Clone, Default)]
struct AlbumRow {
    id: Option<i64>,
    title: Option<String>,
    titlesort: Option<String>,
    titlesearch: Option<String>,
    compilation: Option<i64>,
    year: Option<i64>,
    artwork: Option<String>,
    disc: Option<i64>,
    discc: Option<i64>,
    replay_gain: Option<f64>,
    replay_peak: Option<f64>,
    musicbrainz_id: Option<String>,
    contributor: Option<i64>,
    extid: Option<String>,
    release_type: Option<String>,
}

fn load_album(c: &Connection, id: i64) -> Option<AlbumRow> {
    c.query_row(
        "SELECT id, title, titlesort, titlesearch, compilation, year, artwork, disc, discc, replay_gain, replay_peak,
                musicbrainz_id, contributor, extid, release_type FROM albums WHERE id = ?",
        [id],
        album_from_row,
    )
    .ok()
}

fn album_from_row(r: &rusqlite::Row) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: r.get(0)?,
        title: r.get(1)?,
        titlesort: r.get(2)?,
        titlesearch: r.get(3)?,
        compilation: r.get(4)?,
        year: r.get(5)?,
        artwork: r.get(6)?,
        disc: r.get(7)?,
        discc: r.get(8)?,
        replay_gain: r.get(9)?,
        replay_peak: r.get(10)?,
        musicbrainz_id: r.get(11)?,
        contributor: r.get(12)?,
        extid: r.get(13)?,
        release_type: r.get(14)?,
    })
}

/// Everything needed to write one `tracks` row.
#[derive(Debug, Clone, Default)]
pub struct TrackInput {
    pub url: String,
    pub path: PathBuf,
    pub attrs: Attrs,
    pub content_type: String,
    pub info: AudioInfo,
    /// embedded cover length
    pub cover_len: Option<usize>,
    pub timestamp: i64,
    pub filesize: i64,
    pub is_virtual: bool,
    pub audio: bool,
}

pub struct Scanner<'a> {
    pub conn: &'a Connection,
    pub cfg: &'a LibraryConfig,
    pub rules: TextRules,
    pub tag_opts: TagOptions,
    pub abort: &'a AtomicBool,
    pub progress: Arc<Mutex<ScanProgress>>,
    va_id: Option<i64>,
    no_album_id: Option<i64>,
    last_album: Option<LastAlbum>,
    art_cache: HashMap<PathBuf, Option<PathBuf>>,
    pub changes: usize,
    pub summary: ScanSummary,
    first_scan: bool,
    in_tx: bool,
    pending: usize,
    pub touched_albums: HashSet<i64>,
    /// re-add audio files whose cue sheet was deleted (off while removing whole roots)
    resurrect_containers: bool,
}

impl<'a> Scanner<'a> {
    pub fn new(
        conn: &'a Connection,
        cfg: &'a LibraryConfig,
        abort: &'a AtomicBool,
        progress: Arc<Mutex<ScanProgress>>,
    ) -> Self {
        let first_scan = crate::db::get_meta(conn, "lastRescanTime")
            .map(|v| v == "0" || v.is_empty())
            .unwrap_or(true);
        Scanner {
            conn,
            cfg,
            rules: TextRules::new(&cfg.ignored_articles, &cfg.split_list),
            tag_opts: TagOptions {
                use_tpe2_as_album_artist: cfg.use_tpe2_as_album_artist,
                use_tit1_as_work: cfg.use_tit1_as_work,
            },
            abort,
            progress,
            va_id: None,
            no_album_id: None,
            last_album: None,
            art_cache: HashMap::new(),
            changes: 0,
            summary: ScanSummary::default(),
            first_scan,
            in_tx: false,
            pending: 0,
            touched_albums: HashSet::new(),
            resurrect_containers: true,
        }
    }

    pub(crate) fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }

    pub fn begin(&mut self) -> rusqlite::Result<()> {
        if !self.in_tx {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            self.in_tx = true;
            self.pending = 0;
        }
        Ok(())
    }

    pub fn commit(&mut self) -> rusqlite::Result<()> {
        if self.in_tx {
            self.conn.execute_batch("COMMIT")?;
            self.in_tx = false;
        }
        Ok(())
    }

    /// commit every CHUNK items
    pub(crate) fn tick(&mut self) -> rusqlite::Result<()> {
        self.pending += 1;
        if self.pending >= CHUNK {
            self.commit()?;
            self.begin()?;
        }
        Ok(())
    }

    pub(crate) fn set_step(&self, name: &str, total: usize) {
        let mut p = self.progress.lock();
        p.step = name.to_string();
        p.total = total;
        p.done = 0;
        p.steps.push(name.to_string());
        drop(p);
        let _ = self.conn.execute(
            "INSERT INTO progress (type, name, active, total, done, start) VALUES ('importer', ?, 1, ?, 0, ?)",
            params![name, total as i64, now()],
        );
    }

    pub(crate) fn step_done(&self, done: usize, info: Option<&str>) {
        let mut p = self.progress.lock();
        p.done = done;
        if let Some(i) = info {
            p.info = Some(i.to_string());
        }
    }

    pub(crate) fn finish_step(&self, name: &str) {
        let p = self.progress.lock();
        let (total, done) = (p.total, p.done);
        drop(p);
        let _ = self.conn.execute(
            "UPDATE progress SET active = 0, finish = ?, done = ?, total = ?, info = NULL WHERE name = ? AND active = 1",
            params![now(), done as i64, total as i64, name],
        );
    }

    // -----------------------------------------------------------------------------------------
    // singletons
    // -----------------------------------------------------------------------------------------

    pub fn va_id(&mut self) -> rusqlite::Result<i64> {
        if let Some(id) = self.va_id {
            return Ok(id);
        }
        let id = various_artists_id(self.conn, self.cfg, &self.rules)?;
        self.va_id = Some(id);
        Ok(id)
    }

    // -----------------------------------------------------------------------------------------
    // scanning a root
    // -----------------------------------------------------------------------------------------

    /// Audio scan of one media dir (types => 'audio').
    pub fn scan_audio_root(
        &mut self,
        root: &Path,
        wipe: bool,
        recursive: bool,
    ) -> anyhow::Result<()> {
        let disabled = self.cfg.disabled_extensions_audio.clone();
        let want = move |ext: &str| types::is_audio_ext(ext, &disabled);
        let step = format!("{}|discovering_directory", root.display());
        self.set_step(&step, 0);
        let found = discover(root, self.cfg, &want, recursive, self.abort);
        self.summary.files_found += found.iter().filter(|f| !f.is_dir).count();
        let base = file_url_from_path(root);
        self.fill_scanned_files(&base, &found, recursive)?;
        self.finish_step(&step);
        if self.aborted() {
            return Ok(());
        }
        let ct_filter = "NOT IN ('dir', 'ssp')";
        self.process_diff(root, &base, ct_filter, wipe, recursive, false)
    }

    /// Playlist folder scan (types => 'list').
    pub fn scan_playlist_dir(&mut self, root: &Path) -> anyhow::Result<()> {
        let disabled = self.cfg.disabled_extensions_playlist.clone();
        let want = move |ext: &str| types::is_playlist_ext(ext, &disabled);
        let step = format!("{}|discovering_playlist", root.display());
        self.set_step(&step, 0);
        let mut found = discover(root, self.cfg, &want, true, self.abort);
        found.retain(|f| {
            let n = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            !(n.starts_with("clientplaylist") && n.ends_with(".m3u"))
        });
        let base = file_url_from_path(root);
        self.fill_scanned_files(&base, &found, true)?;
        self.finish_step(&step);
        self.process_diff(root, &base, "= 'ssp'", false, true, true)
    }

    fn fill_scanned_files(
        &mut self,
        base: &str,
        found: &[Found],
        recursive: bool,
    ) -> rusqlite::Result<()> {
        self.begin()?;
        if recursive {
            self.conn.execute(
                "DELETE FROM scanned_files WHERE url = ? OR url LIKE ?",
                params![base, format!("{}/%", like_escape(base))],
            )?;
        } else {
            for f in found {
                self.conn
                    .execute("DELETE FROM scanned_files WHERE url = ?", [&f.url])?;
            }
        }
        {
            let mut st = self.conn.prepare_cached(
                "INSERT INTO scanned_files (url, timestamp, filesize) VALUES (?, ?, ?)",
            )?;
            for f in found {
                st.execute(params![f.url, f.mtime, f.size])?;
            }
        }
        self.commit()
    }

    fn process_diff(
        &mut self,
        root: &Path,
        base: &str,
        ct_filter: &str,
        wipe: bool,
        recursive: bool,
        playlists: bool,
    ) -> anyhow::Result<()> {
        let like = format!("{}%", like_escape(base));
        let max_id: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM tracks", [], |r| r.get(0))?;
        let dir_clause = if recursive {
            String::new()
        } else {
            // only direct children of base
            format!(" AND instr(substr(url, {}), '/') = 0", base.len() + 2)
        };
        let deleted: Vec<String> = if wipe {
            Vec::new()
        } else {
            let sql = format!(
                "SELECT DISTINCT(url) FROM tracks WHERE url NOT IN (SELECT url FROM scanned_files WHERE filesize != 0)
                 AND url LIKE ? ESCAPE '\\' AND virtual IS NULL AND content_type {ct_filter} AND (remote IS NULL OR remote = 0){dir_clause}"
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map([&like], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let new: Vec<String> = {
            let sql = "SELECT DISTINCT(url) FROM scanned_files WHERE url NOT IN (SELECT url FROM tracks WHERE id <= ?)
                       AND url LIKE ? ESCAPE '\\' AND filesize != 0";
            let mut st = self.conn.prepare(sql)?;
            let rows = st.query_map(params![max_id, &like], |r| r.get::<_, String>(0))?;
            let mut v: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            // process cue sheets first, so referenced files are turned into containers once
            v.sort_by_key(|u| {
                if u.to_ascii_lowercase().ends_with(".cue") {
                    0
                } else {
                    1
                }
            });
            v
        };
        let changed: Vec<String> = if wipe {
            Vec::new()
        } else {
            let sql = format!(
                "SELECT DISTINCT(scanned_files.url) FROM scanned_files JOIN tracks ON (scanned_files.url = tracks.url
                   AND (scanned_files.timestamp != tracks.timestamp OR scanned_files.filesize != tracks.filesize)
                   AND tracks.content_type {ct_filter}) WHERE scanned_files.url LIKE ? ESCAPE '\\'"
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map([&like], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let label = if playlists { "playlist" } else { "directory" };
        let rootd = root.display().to_string();

        // deleted
        if !deleted.is_empty() {
            let step = format!("{}|{}_deleted", rootd, label);
            self.set_step(&step, deleted.len());
            self.begin()?;
            for (i, u) in deleted.iter().enumerate() {
                if self.aborted() {
                    break;
                }
                if let Err(e) = self.deleted(u) {
                    tracing::warn!("error deleting {}: {}", u, e);
                }
                self.summary.deleted += 1;
                self.changes += 1;
                self.step_done(i + 1, Some(u));
                self.tick()?;
            }
            self.commit()?;
            self.finish_step(&step);
        }

        // changed
        if !changed.is_empty() && !self.aborted() {
            let step = format!("{}|{}_changed", rootd, label);
            self.set_step(&step, changed.len());
            self.begin()?;
            let mut changed_albums: HashSet<i64> = HashSet::new();
            for (i, u) in changed.iter().enumerate() {
                if self.aborted() {
                    break;
                }
                if let Ok(Some(a)) =
                    self.conn
                        .query_row("SELECT album FROM tracks WHERE url = ?", [u], |r| {
                            r.get::<_, Option<i64>>(0)
                        })
                {
                    changed_albums.insert(a);
                }
                if let Err(e) = self.changed(u) {
                    tracing::warn!("error rescanning {}: {}", u, e);
                }
                if let Ok(Some(a)) =
                    self.conn
                        .query_row("SELECT album FROM tracks WHERE url = ?", [u], |r| {
                            r.get::<_, Option<i64>>(0)
                        })
                {
                    changed_albums.insert(a);
                }
                self.summary.changed += 1;
                self.changes += 1;
                self.step_done(i + 1, Some(u));
                self.tick()?;
            }
            // contributor_album rebuild for changed albums
            for a in &changed_albums {
                self.rebuild_contributor_album(*a)?;
            }
            self.commit()?;
            self.finish_step(&step);
        }

        // new
        if !new.is_empty() && !self.aborted() {
            let step = format!("{}|{}_new", rootd, label);
            self.set_step(&step, new.len());
            self.begin()?;
            for (i, u) in new.iter().enumerate() {
                if self.aborted() {
                    break;
                }
                match self.new_url(u, playlists) {
                    Ok(true) => {
                        self.summary.new += 1;
                        self.changes += 1;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!("ERROR SCANNING file {}: {}", u, e);
                        self.summary.errors += 1;
                    }
                }
                self.step_done(i + 1, Some(u));
                self.tick()?;
            }
            self.commit()?;
            self.finish_step(&step);
        }
        Ok(())
    }

    fn rebuild_contributor_album(&self, album: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM contributor_album WHERE album = ?", [album])?;
        self.conn.execute(
            "INSERT OR IGNORE INTO contributor_album (role, contributor, album)
             SELECT DISTINCT role, contributor, tracks.album FROM contributor_track
             JOIN tracks ON tracks.id = contributor_track.track WHERE tracks.album = ?",
            [album],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // new / changed / deleted
    // -----------------------------------------------------------------------------------------

    fn in_playlist_dir(&self, p: &Path) -> bool {
        match &self.cfg.playlist_dir {
            Some(d) => p.starts_with(d),
            None => false,
        }
    }

    /// `Slim::Utils::Scanner::Local::new`
    pub fn new_url(&mut self, u: &str, playlist_scan: bool) -> anyhow::Result<bool> {
        let Some(path) = path_from_file_url(u) else {
            return Ok(false);
        };
        let existing_ct: Option<String> = self
            .conn
            .query_row("SELECT content_type FROM tracks WHERE url = ?", [u], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let ct = existing_ct
            .clone()
            .unwrap_or_else(|| types::type_from_path(u).to_string());
        if types::is_song_type(&ct) && !playlist_scan {
            if existing_ct.as_deref() == Some("cur") {
                return Ok(false);
            }
            if existing_ct.is_some() {
                // already in the DB (e.g. created through a playlist)
                return Ok(false);
            }
            self.scan_audio_file(u, &path, None)?;
            return Ok(true);
        }
        if ct == "cue" || ct == "cur" {
            if ct == "cue" {
                self.scan_cue_file(u, &path)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if types::is_playlist_type(&ct) && self.in_playlist_dir(&path) {
            self.scan_playlist_file(u, &path)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `changed($url)`
    pub fn changed(&mut self, u: &str) -> anyhow::Result<()> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, content_type FROM tracks WHERE url = ?",
                [u],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                },
            )
            .optional()?;
        let Some((id, ct)) = row else { return Ok(()) };
        let Some(path) = path_from_file_url(u) else {
            return Ok(());
        };
        match ct.as_str() {
            "fec" => {
                // FLAC with embedded cue: drop virtual tracks and rescan as new
                self.delete_virtual_children(u)?;
                self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
                self.scan_audio_file(u, &path, None)?;
            }
            "cur" => {
                // referenced by an external cue sheet: re-run those cue sheets
                let (mt, sz, _) = file_stat(&path).unwrap_or((0, 0, false));
                self.conn.execute(
                    "UPDATE tracks SET timestamp = ?, filesize = ? WHERE id = ?",
                    params![mt, sz, id],
                )?;
                let cues: Vec<String> = {
                    let mut st = self.conn.prepare(
                        "SELECT DISTINCT p.url FROM playlist_track pt JOIN tracks p ON p.id = pt.playlist
                         WHERE pt.track LIKE ? ESCAPE '\\' AND p.content_type = 'cue'",
                    )?;
                    let rows =
                        st.query_map([format!("{}#%", like_escape(u))], |r| r.get::<_, String>(0))?;
                    rows.filter_map(|r| r.ok()).collect()
                };
                for c in cues {
                    self.deleted(&c)?;
                    if let Some(p) = path_from_file_url(&c) {
                        if p.is_file() {
                            self.scan_cue_file(&c, &p)?;
                        }
                    }
                }
            }
            "cue" => {
                self.deleted(u)?;
                self.scan_cue_file(u, &path)?;
            }
            c if types::is_playlist_type(c) => {
                self.deleted(u)?;
                if self.in_playlist_dir(&path) {
                    self.scan_playlist_file(u, &path)?;
                }
            }
            c if types::is_song_type(c) => {
                self.scan_audio_file(u, &path, Some(id))?;
            }
            _ => {
                let (mt, sz, _) = file_stat(&path).unwrap_or((0, 0, false));
                self.conn.execute(
                    "UPDATE tracks SET timestamp = ?, filesize = ? WHERE id = ?",
                    params![mt, sz, id],
                )?;
            }
        }
        Ok(())
    }

    fn delete_virtual_children(&mut self, base: &str) -> anyhow::Result<()> {
        let ids: Vec<i64> = {
            let mut st = self
                .conn
                .prepare("SELECT id FROM tracks WHERE url LIKE ? ESCAPE '\\' AND virtual = 1")?;
            let rows =
                st.query_map([format!("{}#%", like_escape(base))], |r| r.get::<_, i64>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in ids {
            self.delete_track_id(id)?;
        }
        Ok(())
    }

    /// `deleted($url)`
    pub fn deleted(&mut self, u: &str) -> anyhow::Result<()> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, content_type FROM tracks WHERE url = ?",
                [u],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                },
            )
            .optional()?;
        let Some((id, ct)) = row else { return Ok(()) };
        if ct == "cue" {
            let entries: Vec<String> = {
                let mut st = self.conn.prepare(
                    "SELECT track FROM playlist_track WHERE playlist = ? ORDER BY position",
                )?;
                let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
                rows.filter_map(|r| r.ok()).collect()
            };
            self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
            let mut bases: HashSet<String> = HashSet::new();
            for e in entries {
                let vid: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT id FROM tracks WHERE url = ? AND virtual = 1",
                        [&e],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(vid) = vid {
                    self.delete_track_id(vid)?;
                }
                bases.insert(url::strip_anchor(&e).to_string());
            }
            // containers that are no longer referenced become normal tracks again
            for b in bases {
                let still: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM tracks WHERE url LIKE ? ESCAPE '\\' AND virtual = 1",
                    [format!("{}#%", like_escape(&b))],
                    |r| r.get(0),
                )?;
                if still == 0 {
                    let cur: Option<i64> = self
                        .conn
                        .query_row(
                            "SELECT id FROM tracks WHERE url = ? AND content_type = 'cur'",
                            [&b],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(cid) = cur {
                        self.conn
                            .execute("DELETE FROM tracks WHERE id = ?", [cid])?;
                        if let (Some(p), true) = (path_from_file_url(&b), self.resurrect_containers)
                        {
                            if p.is_file() {
                                let _ = self.scan_audio_file(&b, &p, None);
                            }
                        }
                    }
                }
            }
            return Ok(());
        }
        if ct == "fec" {
            self.delete_virtual_children(u)?;
            self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
            return Ok(());
        }
        if types::is_song_type(&ct) {
            self.delete_track_id(id)?;
            return Ok(());
        }
        // playlists / dirs / others
        self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
        Ok(())
    }

    /// Delete an audio track and clean up albums/contributors/genres/years.
    pub fn delete_track_id(&mut self, id: i64) -> anyhow::Result<()> {
        let (album, year): (Option<i64>, Option<i64>) = self
            .conn
            .query_row("SELECT album, year FROM tracks WHERE id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .unwrap_or((None, None));
        let contributors = ids_for(
            self.conn,
            "SELECT DISTINCT contributor FROM contributor_track WHERE track = ?",
            id,
        )?;
        let genres = ids_for(
            self.conn,
            "SELECT DISTINCT genre FROM genre_track WHERE track = ?",
            id,
        )?;
        self.conn
            .execute("DELETE FROM contributor_track WHERE track = ?", [id])?;
        self.conn
            .execute("DELETE FROM genre_track WHERE track = ?", [id])?;
        self.conn
            .execute("DELETE FROM comments WHERE track = ?", [id])?;
        self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
        self.rescan_contributors(&contributors)?;
        if let Some(a) = album {
            let comp: Option<i64> = self
                .conn
                .query_row("SELECT compilation FROM albums WHERE id = ?", [a], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            if comp == Some(1) {
                self.conn
                    .execute("UPDATE albums SET compilation = NULL WHERE id = ?", [a])?;
                self.merge_single_va_album(a)?;
            }
            self.rescan_album(a)?;
            self.rebuild_contributor_album(a)?;
            self.touched_albums.insert(a);
        }
        if let Some(y) = year {
            self.rescan_year(y)?;
        }
        self.rescan_genres(&genres)?;
        Ok(())
    }

    fn rescan_contributors(&mut self, ids: &[i64]) -> rusqlite::Result<()> {
        let va = self.va_id()?;
        for id in ids {
            if *id == va {
                continue;
            }
            let n: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM contributor_track WHERE contributor = ?",
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                // keep contributors still referenced as album contributor
                let a: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM albums WHERE contributor = ?",
                    [id],
                    |r| r.get(0),
                )?;
                if a == 0 {
                    self.conn
                        .execute("DELETE FROM contributors WHERE id = ?", [id])?;
                }
            }
        }
        Ok(())
    }

    fn rescan_album(&mut self, id: i64) -> rusqlite::Result<()> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM tracks WHERE album = ?", [id], |r| {
                    r.get(0)
                })?;
        if n == 0 {
            self.conn.execute("DELETE FROM albums WHERE id = ?", [id])?;
            if self.last_album.as_ref().map(|l| l.id) == Some(id) {
                self.last_album = None;
            }
            if self.no_album_id == Some(id) {
                self.no_album_id = None;
            }
        }
        Ok(())
    }

    fn rescan_year(&mut self, y: i64) -> rusqlite::Result<()> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM tracks WHERE year = ?", [y], |r| {
                    r.get(0)
                })?;
        if n == 0 {
            self.conn.execute("DELETE FROM years WHERE id = ?", [y])?;
        }
        Ok(())
    }

    fn rescan_genres(&mut self, ids: &[i64]) -> rusqlite::Result<()> {
        for id in ids {
            let n: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM genre_track WHERE genre = ?",
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                self.conn.execute("DELETE FROM genres WHERE id = ?", [id])?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // audio files
    // -----------------------------------------------------------------------------------------

    /// Read tags and store an audio file (new or changed). Handles embedded cue sheets.
    pub fn scan_audio_file(
        &mut self,
        u: &str,
        path: &Path,
        existing: Option<i64>,
    ) -> anyhow::Result<i64> {
        let (mtime, size, _) =
            file_stat(path).ok_or_else(|| anyhow::anyhow!("cannot stat file"))?;
        let rt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tags::read_tags(path, &self.tag_opts)
        }));
        let mut rt = match rt {
            Ok(Ok(rt)) => rt,
            Ok(Err(e)) => {
                if types::type_from_path(u) == "mp3" {
                    // LMS skips unreadable MP3s
                    return Err(anyhow::anyhow!("Unable to read tags from file: {}", e));
                }
                tracing::debug!("tag read failed for {}: {}", path.display(), e);
                tags::ReadTags {
                    content_type: types::type_from_path(u).to_string(),
                    ..Default::default()
                }
            }
            Err(_) => return Err(anyhow::anyhow!("tag reader panicked")),
        };
        if rt.info.drm {
            return Err(anyhow::anyhow!("Track is DRM-protected"));
        }
        tags::post_process(&mut rt, path, u, &self.cfg.guess_file_formats);

        // FLAC with embedded cue sheet → container 'fec' + virtual tracks
        if let Some(lines) = rt.cuesheet.clone() {
            if rt.content_type == "flc" {
                let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
                let file_secs = rt.info.secs;
                let file_attrs = rt.attrs.clone();
                let sheet = cue::parse(&lines, &dir, Some(path), |_| {
                    Some((file_secs, file_attrs.clone()))
                });
                if let Some(mut sheet) = sheet {
                    if let Some(id) = existing {
                        self.delete_track_id(id)?;
                    }
                    let secs = rt.info.secs;
                    if let Some(last) = sheet.tracks.last_mut() {
                        if last.end <= last.start {
                            if let Some(s) = secs {
                                last.end = s;
                            }
                        }
                    }
                    let container_title = sheet
                        .album
                        .get("ALBUM")
                        .cloned()
                        .or_else(|| first(&rt.attrs, "ALBUM").map(|s| s.to_string()))
                        .unwrap_or_else(|| url::plain_title(u, false));
                    let cid =
                        self.insert_container(u, path, "fec", &container_title, mtime, size, &rt)?;
                    let _ = cid;
                    let base_attrs = rt.attrs.clone();
                    for tr in &sheet.tracks {
                        let mut attrs: Attrs = Attrs::new();
                        for (k, v) in &tr.attrs {
                            attrs.insert(k.clone(), vec![v.clone()]);
                        }
                        for (k, v) in &base_attrs {
                            if matches!(
                                k.as_str(),
                                "TITLE"
                                    | "TITLESORT"
                                    | "TRACKNUM"
                                    | "MUSICBRAINZ_ID"
                                    | "MUSICBRAINZ_RELEASETRACKID"
                            ) {
                                continue;
                            }
                            attrs.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                        attrs.insert("TRACKNUM".into(), vec![tr.tracknum.to_string()]);
                        let vurl = format!(
                            "{}#{}-{}",
                            u,
                            cue::fmt_secs(tr.start),
                            cue::fmt_secs(tr.end)
                        );
                        let input = self
                            .virtual_input(&vurl, path, attrs, &rt, tr.start, tr.end, mtime, size);
                        self.store_track(input, None)?;
                    }
                    return Ok(cid);
                }
            }
        }

        let input = TrackInput {
            url: u.to_string(),
            path: path.to_path_buf(),
            attrs: rt.attrs,
            content_type: rt.content_type,
            info: rt.info,
            cover_len: rt.cover_len,
            timestamp: mtime,
            filesize: size,
            is_virtual: false,
            audio: true,
        };
        self.store_track(input, existing)
    }

    #[allow(clippy::too_many_arguments)]
    fn virtual_input(
        &self,
        vurl: &str,
        path: &Path,
        attrs: Attrs,
        base: &tags::ReadTags,
        start: f64,
        end: f64,
        mtime: i64,
        size: i64,
    ) -> TrackInput {
        let mut info = base.info.clone();
        let dur = (end - start).max(0.0);
        // processAnchor: byte-rate estimate
        if let (Some(s), Some(sz)) = (base.info.secs, base.info.size) {
            if s > 0.0 {
                let byterate = sz as f64 / s;
                let align = base.info.block_align.unwrap_or(1).max(1) as u64;
                let sb = ((start * byterate) as u64 / align) * align;
                let eb = ((end * byterate) as u64 / align) * align;
                info.offset = Some(base.info.offset.unwrap_or(0) + sb);
                info.size = Some(eb.saturating_sub(sb));
            }
        }
        info.secs = Some(dur);
        TrackInput {
            url: vurl.to_string(),
            path: path.to_path_buf(),
            attrs,
            content_type: base.content_type.clone(),
            info,
            cover_len: base.cover_len,
            timestamp: mtime,
            filesize: size,
            is_virtual: true,
            audio: true,
        }
    }

    /// A hidden container row (`cur` / `fec`): audio = 0, no relations.
    #[allow(clippy::too_many_arguments)]
    fn insert_container(
        &mut self,
        u: &str,
        _path: &Path,
        ct: &str,
        title: &str,
        mtime: i64,
        size: i64,
        rt: &tags::ReadTags,
    ) -> anyhow::Result<i64> {
        let existing: Option<i64> = self
            .conn
            .query_row("SELECT id FROM tracks WHERE url = ?", [u], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            // turn an existing audio track into a container
            let was_audio: Option<i64> = self
                .conn
                .query_row("SELECT audio FROM tracks WHERE id = ?", [id], |r| r.get(0))
                .optional()?
                .flatten();
            if was_audio == Some(1) {
                self.delete_track_id(id)?;
            } else {
                self.conn.execute(
                    "UPDATE tracks SET content_type = ?, audio = 0, timestamp = ?, filesize = ?, updated_time = ? WHERE id = ?",
                    params![ct, mtime, size, now(), id],
                )?;
                return Ok(id);
            }
        }
        let t = now();
        self.conn.execute(
            "INSERT INTO tracks (url, urlmd5, title, titlesort, titlesearch, content_type, timestamp, filesize, secs, audio, remote,
                                 lossless, samplerate, samplesize, channels, bitrate, audio_size, audio_offset, added_time, updated_time)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                u,
                md5_hex(u),
                title,
                self.rules.sort_key(title),
                self.rules.search_key(title),
                ct,
                mtime,
                size,
                rt.info.secs,
                rt.info.lossless as i64,
                rt.info.samplerate,
                rt.info.samplesize,
                rt.info.channels,
                rt.info.bitrate,
                rt.info.size.map(|v| v as i64),
                rt.info.offset.map(|v| v as i64),
                t,
                t
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // -----------------------------------------------------------------------------------------
    // cue sheets & playlists
    // -----------------------------------------------------------------------------------------

    fn insert_playlist_row(
        &mut self,
        u: &str,
        ct: &str,
        title: &str,
        mtime: i64,
        size: i64,
    ) -> anyhow::Result<i64> {
        let t = now();
        let existing: Option<i64> = self
            .conn
            .query_row("SELECT id FROM tracks WHERE url = ?", [u], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            self.conn.execute(
                "UPDATE tracks SET content_type = ?, title = ?, titlesort = ?, titlesearch = ?, timestamp = ?, filesize = ?, updated_time = ?,
                 musicmagic_mixable = 1 WHERE id = ?",
                params![ct, title, self.rules.sort_key(title), self.rules.search_key(title), mtime, size, t, id],
            )?;
            self.conn
                .execute("DELETE FROM playlist_track WHERE playlist = ?", [id])?;
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO tracks (url, urlmd5, title, titlesort, titlesearch, content_type, timestamp, filesize, remote, musicmagic_mixable,
                                 added_time, updated_time)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 1, ?, ?)",
            params![u, md5_hex(u), title, self.rules.sort_key(title), self.rules.search_key(title), ct, mtime, size, t, t],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_playlist_tracks(&self, playlist: i64, urls: &[String]) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM playlist_track WHERE playlist = ?", [playlist])?;
        let mut st = self.conn.prepare_cached(
            "INSERT INTO playlist_track (position, playlist, track) VALUES (?, ?, ?)",
        )?;
        for (i, u) in urls.iter().enumerate() {
            st.execute(params![i as i64, playlist, u])?;
        }
        Ok(())
    }

    /// External `.cue` file: playlist row (ct `cue`, or `ssp` inside the playlist dir) +
    /// container rows (`cur`) + virtual tracks.
    pub fn scan_cue_file(&mut self, u: &str, path: &Path) -> anyhow::Result<()> {
        let (mtime, size, _) = file_stat(path).ok_or_else(|| anyhow::anyhow!("cannot stat"))?;
        let bytes = std::fs::read(path)?;
        let text = cue::decode_text(&bytes);
        let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let tag_opts = self.tag_opts.clone();
        let guess = self.cfg.guess_file_formats.clone();
        let mut file_cache: HashMap<PathBuf, tags::ReadTags> = HashMap::new();
        let sheet = cue::parse(&lines, &dir, None, |p| {
            let rt = tags::read_tags(p, &tag_opts).ok().map(|mut rt| {
                let fu = file_url_from_path(p);
                tags::post_process(&mut rt, p, &fu, &guess);
                rt
            })?;
            let mut attrs = rt.attrs.clone();
            attrs.insert("CONTENT_TYPE".into(), vec![rt.content_type.clone()]);
            let secs = rt.info.secs;
            file_cache.insert(p.to_path_buf(), rt);
            Some((secs, attrs))
        });
        let ct = if self.in_playlist_dir(path) {
            "ssp"
        } else {
            "cue"
        };
        let title = url::plain_title(u, false);
        let pid = self.insert_playlist_row(u, ct, &title, mtime, size)?;
        let Some(sheet) = sheet else {
            return Ok(());
        };
        let mut uris = Vec::new();
        let mut containers: HashSet<PathBuf> = HashSet::new();
        for tr in &sheet.tracks {
            let base_rt = match file_cache.get(&tr.file) {
                Some(rt) => rt.clone(),
                None => {
                    let mut rt = match tags::read_tags(&tr.file, &self.tag_opts) {
                        Ok(rt) => rt,
                        Err(_) => continue,
                    };
                    let fu = file_url_from_path(&tr.file);
                    tags::post_process(&mut rt, &tr.file, &fu, &self.cfg.guess_file_formats);
                    file_cache.insert(tr.file.clone(), rt.clone());
                    rt
                }
            };
            let furl = file_url_from_path(&tr.file);
            let (fm, fs, _) = file_stat(&tr.file).unwrap_or((0, 0, false));
            if containers.insert(tr.file.clone()) {
                let t = url::plain_title(&furl, false);
                self.insert_container(&furl, &tr.file, "cur", &t, fm, fs, &base_rt)?;
            }
            let mut attrs: Attrs = Attrs::new();
            for (k, v) in &tr.attrs {
                if k == "CONTENT_TYPE" {
                    continue;
                }
                attrs.insert(k.clone(), vec![v.clone()]);
            }
            attrs.insert("TRACKNUM".into(), vec![tr.tracknum.to_string()]);
            let vurl = format!(
                "{}#{}-{}",
                furl,
                cue::fmt_secs(tr.start),
                cue::fmt_secs(tr.end)
            );
            let input =
                self.virtual_input(&vurl, &tr.file, attrs, &base_rt, tr.start, tr.end, fm, fs);
            let existing: Option<i64> = self
                .conn
                .query_row("SELECT id FROM tracks WHERE url = ?", [&vurl], |r| r.get(0))
                .optional()?;
            self.store_track(input, existing)?;
            uris.push(vurl);
        }
        self.set_playlist_tracks(pid, &uris)?;
        Ok(())
    }

    /// Playlist file inside the playlist folder (ct `ssp`).
    pub fn scan_playlist_file(&mut self, u: &str, path: &Path) -> anyhow::Result<()> {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "cue" {
            return self.scan_cue_file(u, path);
        }
        let (mtime, size, _) = file_stat(path).ok_or_else(|| anyhow::anyhow!("cannot stat"))?;
        let bytes = std::fs::read(path)?;
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let entries = match ext.as_str() {
            "pls" => crate::playlist_files::parse_pls(&bytes, &dir, &self.cfg.media_dirs, u),
            _ => crate::playlist_files::parse_m3u(&bytes, &dir, &self.cfg.media_dirs, u),
        };
        let ct = if self.in_playlist_dir(path) {
            "ssp"
        } else {
            types::type_from_path(u)
        };
        let title = url::plain_title(u, false);
        let pid = self.insert_playlist_row(u, ct, &title, mtime, size)?;
        let mut urls = Vec::new();
        for e in entries {
            // make sure local files referenced by the playlist exist in the DB
            if url::is_file_url(&e.url) && url::anchor_from_url(&e.url).is_none() {
                let exists: Option<i64> = self
                    .conn
                    .query_row("SELECT id FROM tracks WHERE url = ?", [&e.url], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if exists.is_none() {
                    if let Some(p) = path_from_file_url(&e.url) {
                        if types::is_song_type(types::type_from_path(&e.url)) && p.is_file() {
                            let _ = self.scan_audio_file(&e.url, &p, None);
                        }
                    }
                }
            }
            urls.push(e.url);
        }
        self.set_playlist_tracks(pid, &urls)?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // track storage (_newTrack / update path)
    // -----------------------------------------------------------------------------------------

    fn find_standalone_artwork(&mut self, dir: &Path) -> Option<PathBuf> {
        if let Some(v) = self.art_cache.get(dir) {
            return v.clone();
        }
        let res = find_standalone_artwork(dir, &self.cfg.cover_art);
        if self.art_cache.len() > 32 {
            self.art_cache.clear();
        }
        self.art_cache.insert(dir.to_path_buf(), res.clone());
        res
    }

    /// Store a track (insert or update `existing`). Returns the track id.
    pub fn store_track(&mut self, input: TrackInput, existing: Option<i64>) -> anyhow::Result<i64> {
        let cfg = self.cfg;
        let mut attrs = input.attrs.clone();

        // --- _preCheckAttributes ---
        let title = attrs
            .get("TITLE")
            .map(|v| v.join(" / "))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| url::plain_title(&input.url, false));
        let titlesort_src = first(&attrs, "TITLESORT")
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| title.clone());
        let titlesort = self.rules.sort_key(&titlesort_src);
        let titlesearch = self.rules.search_key(&title);
        if let Some(d) = first(&attrs, "DISC").map(|s| s.to_string()) {
            let re = crate::re!(r"^(\d+)[-/](\d+)$");
            if let Some(c) = re.captures(d.trim()) {
                attrs.insert("DISC".into(), vec![c[1].to_string()]);
                if first(&attrs, "DISCC").is_none() {
                    attrs.insert("DISCC".into(), vec![c[2].to_string()]);
                }
            }
        }
        let num = |attrs: &Attrs, k: &str| -> Option<i64> {
            let v = first(attrs, k)?.trim().to_string();
            if v.is_empty() || !v.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n: i64 = v.parse().ok()?;
            if n == 0 {
                None
            } else {
                Some(n)
            }
        };
        let year = num(&attrs, "YEAR").unwrap_or(0);
        let disc = num(&attrs, "DISC").map(|d| d.min(254));
        let discc = num(&attrs, "DISCC").map(|d| d.min(254));
        let bpm = num(&attrs, "BPM");
        let tracknum = first(&attrs, "TRACKNUM").and_then(text::clean_track_number);
        let replay_gain = first(&attrs, "REPLAYGAIN_TRACK_GAIN").and_then(text::clean_gain);
        let replay_peak = first(&attrs, "REPLAYGAIN_TRACK_PEAK").and_then(text::clean_gain);
        let mut comments: Vec<String> = attrs.get("COMMENT").cloned().unwrap_or_default();
        for k in ["URLTAG", "URL"] {
            if let Some(v) = attrs.get(k) {
                comments.extend(v.iter().cloned());
            }
        }
        let comment_black = crate::re!(
            r"SoundJam_CDDB_|iTunes_CDDB_|^iTun[A-Z]{4}|^\s*[0-9A-Fa-f]{8}(\+|\s)|^\s*[0-9A-Fa-f]{2}\+[0-9A-Fa-f]{32}"
        );
        comments.retain(|c| !c.trim().is_empty() && !comment_black.is_match(c));
        let lyrics = attrs
            .get("LYRICS")
            .map(|v| v.join("\n"))
            .filter(|s| !s.is_empty());
        let trimmed = |k: &str| {
            first(&attrs, k)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let performance = trimmed("PERFORMANCE");
        let grouping = trimmed("GROUPING");
        let discsubtitle = trimmed("DISCSUBTITLE");
        let subtitle = trimmed("SUBTITLE");
        let musicbrainz_id = first(&attrs, "MUSICBRAINZ_ID").map(|s| s.to_string());
        let rating = first(&attrs, "RATING")
            .and_then(|r| r.trim().parse::<i64>().ok())
            .filter(|r| *r > 0 && *r <= 255);
        let is_compilation: Option<i64> = first(&attrs, "COMPILATION").and_then(|c| {
            let c = c.trim().to_ascii_lowercase();
            if ["1", "yes", "true"].contains(&c.as_str()) {
                Some(1)
            } else if ["0", "no", "false"].contains(&c.as_str()) {
                Some(0)
            } else {
                None
            }
        });

        // old state for change handling
        let mut old_album: Option<i64> = None;
        let mut old_year: Option<i64> = None;
        let mut old_contribs: Vec<i64> = Vec::new();
        let mut old_genres: Vec<i64> = Vec::new();
        if let Some(id) = existing {
            let r: Option<(Option<i64>, Option<i64>)> = self
                .conn
                .query_row("SELECT album, year FROM tracks WHERE id = ?", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            if let Some((a, y)) = r {
                old_album = a;
                old_year = y;
            }
            old_contribs = ids_for(
                self.conn,
                "SELECT DISTINCT contributor FROM contributor_track WHERE track = ?",
                id,
            )?;
            old_genres = ids_for(
                self.conn,
                "SELECT DISTINCT genre FROM genre_track WHERE track = ?",
                id,
            )?;
        }

        // --- contributors ---
        let contributors = self.merge_and_create_contributors(&mut attrs, is_compilation)?;
        let get_first = |r: &str| {
            contributors
                .iter()
                .find(|(n, _)| n == r)
                .and_then(|(_, ids)| ids.first().copied())
        };
        let primary_artist = get_first("ARTIST").or_else(|| get_first("TRACKARTIST"));
        let album_contributor = get_first("ALBUMARTIST").or_else(|| get_first("ARTIST"));
        let has_album_artist = get_first("ALBUMARTIST").is_some();

        // --- artwork ---
        let (cover, coverid) = if let Some(len) = input.cover_len {
            let cid = image_id(&input.url, input.timestamp, input.filesize);
            (Some(len.to_string()), cid)
        } else {
            let dir = input
                .path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            match self.find_standalone_artwork(&dir) {
                Some(p) => {
                    let (m, s, _) = file_stat(&p).unwrap_or((0, 0, false));
                    let cid = image_id(&input.url, m, s);
                    (Some(p.to_string_lossy().into_owned()), cid)
                }
                None => (Some("0".to_string()), None),
            }
        };

        // --- album ---
        let dirname = url::dirname_url(&input.url);
        let album_title = attrs
            .get("ALBUM")
            .map(|v| v.join(" / "))
            .map(|t| t.trim_matches(' ').to_string());
        let album_id = self.create_or_update_album(AlbumInput {
            title: album_title,
            albumsort: first(&attrs, "ALBUMSORT").map(|s| s.to_string()),
            disc,
            discc,
            brainz_id: first(&attrs, "MUSICBRAINZ_ALBUM_ID").map(|s| s.to_string()),
            release_types: attrs.get("RELEASETYPE").cloned().unwrap_or_default(),
            rg_gain: attrs
                .get("REPLAYGAIN_ALBUM_GAIN")
                .and_then(|v| v.first())
                .cloned(),
            rg_peak: attrs
                .get("REPLAYGAIN_ALBUM_PEAK")
                .and_then(|v| v.first())
                .cloned(),
            is_compilation,
            contributor_id: album_contributor,
            has_album_artist,
            basename: dirname,
            tracknum,
            year,
            coverid: coverid.clone(),
            update_from: old_album,
        })?;

        // --- work ---
        let genres_for_work: Vec<String> = attrs
            .get("GENRE")
            .map(|g| self.rules.split_values(g))
            .unwrap_or_default();
        let work_id = match (
            attrs.get("WORK").map(|v| v.join(", ")),
            get_first("COMPOSER"),
        ) {
            (Some(w), Some(composer))
                if !w.trim().is_empty() && self.work_required(&genres_for_work) =>
            {
                Some(self.create_work(&w, first(&attrs, "WORKSORT"), composer)?)
            }
            _ => None,
        };

        // --- tracks row ---
        let t = now();
        let urlmd5 = md5_hex(&input.url);
        let info = &input.info;
        let vbr: Option<i64> = info.vbr.map(|v| v as i64);
        let remote = url::is_remote_url(&input.url) as i64;
        let track_id = if let Some(id) = existing {
            self.conn.execute(
                "UPDATE tracks SET url = ?, urlmd5 = ?, title = ?, titlesort = ?, titlesearch = ?, album = ?, tracknum = ?, content_type = ?,
                   timestamp = ?, filesize = ?, audio_size = ?, audio_offset = ?, year = ?, secs = ?, cover = ?, vbr_scale = ?, bitrate = ?,
                   samplerate = ?, samplesize = ?, channels = ?, block_alignment = ?, endian = ?, bpm = ?, tagversion = ?, drm = 0, disc = ?,
                   audio = 1, remote = ?, lossless = ?, lyrics = ?, musicbrainz_id = ?, replay_gain = ?, replay_peak = ?, primary_artist = ?,
                   coverid = ?, cover_cached = NULL, virtual = ?, updated_time = ?, work = ?, subtitle = ?, grouping = ?, performance = ?,
                   discsubtitle = ? WHERE id = ?",
                params![
                    input.url, urlmd5, title, titlesort, titlesearch, album_id, tracknum, input.content_type, input.timestamp,
                    input.filesize, info.size.map(|v| v as i64), info.offset.map(|v| v as i64), year, info.secs, cover, vbr,
                    info.bitrate, info.samplerate, info.samplesize, info.channels, info.block_align, info.endian, bpm,
                    info.tagversion, disc, remote, info.lossless as i64, lyrics, musicbrainz_id, replay_gain, replay_peak,
                    primary_artist, coverid, if input.is_virtual { Some("1") } else { None }, t, work_id, subtitle, grouping,
                    performance, discsubtitle, id
                ],
            )?;
            id
        } else {
            self.conn.execute(
                "INSERT INTO tracks (url, urlmd5, title, titlesort, titlesearch, album, tracknum, content_type, timestamp, filesize,
                   audio_size, audio_offset, year, secs, cover, vbr_scale, bitrate, samplerate, samplesize, channels, block_alignment,
                   endian, bpm, tagversion, drm, disc, audio, remote, lossless, lyrics, musicbrainz_id, replay_gain, replay_peak,
                   primary_artist, coverid, virtual, added_time, updated_time, work, subtitle, grouping, performance, discsubtitle)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    input.url, urlmd5, title, titlesort, titlesearch, album_id, tracknum, input.content_type, input.timestamp,
                    input.filesize, info.size.map(|v| v as i64), info.offset.map(|v| v as i64), year, info.secs, cover, vbr,
                    info.bitrate, info.samplerate, info.samplesize, info.channels, info.block_align, info.endian, bpm,
                    info.tagversion, disc, remote, info.lossless as i64, lyrics, musicbrainz_id, replay_gain, replay_peak,
                    primary_artist, coverid, if input.is_virtual { Some("1") } else { None }, t, t, work_id, subtitle, grouping,
                    performance, discsubtitle
                ],
            )?;
            self.conn.last_insert_rowid()
        };

        // --- relations ---
        self.conn
            .execute("DELETE FROM contributor_track WHERE track = ?", [track_id])?;
        {
            let mut st_t = self.conn.prepare_cached(
                "REPLACE INTO contributor_track (role, contributor, track) VALUES (?, ?, ?)",
            )?;
            let mut st_a = self.conn.prepare_cached(
                "REPLACE INTO contributor_album (role, contributor, album) VALUES (?, ?, ?)",
            )?;
            for (role, ids) in &contributors {
                let rid = type_to_role(role).unwrap_or(1);
                for cid in ids {
                    st_t.execute(params![rid, cid, track_id])?;
                    st_a.execute(params![rid, cid, album_id])?;
                }
            }
        }
        // genres
        let genre_names: Vec<String> = match attrs.get("GENRE") {
            Some(g) => self
                .rules
                .split_values(g)
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect(),
            None => Vec::new(),
        };
        self.conn
            .execute("DELETE FROM genre_track WHERE track = ?", [track_id])?;
        let genre_names = if genre_names.is_empty() {
            vec![cfg.no_genre_string.clone()]
        } else {
            genre_names
        };
        for g in &genre_names {
            let gid = self.genre_id(g.trim())?;
            self.conn.execute(
                "REPLACE INTO genre_track (genre, track) VALUES (?, ?)",
                params![gid, track_id],
            )?;
        }
        // comments
        self.conn
            .execute("DELETE FROM comments WHERE track = ?", [track_id])?;
        for c in &comments {
            self.conn.execute(
                "INSERT INTO comments (track, value) VALUES (?, ?)",
                params![track_id, c],
            )?;
        }
        // persistent
        self.update_persistent(
            &input.url,
            &urlmd5,
            musicbrainz_id.as_deref(),
            input.timestamp,
            rating,
        )?;

        // compilation detection (after the track's contributors are known)
        if is_compilation.is_none() {
            self.merge_single_va_album(album_id)?;
        }
        self.touched_albums.insert(album_id);

        // change bookkeeping
        if existing.is_some() {
            let old_contribs: Vec<i64> = old_contribs.into_iter().collect();
            self.rescan_contributors(&old_contribs)?;
            if let Some(a) = old_album {
                if a != album_id {
                    self.rescan_album(a)?;
                    self.touched_albums.insert(a);
                    self.rebuild_contributor_album(a)?;
                }
            }
            if let Some(y) = old_year {
                if y != year {
                    self.rescan_year(y)?;
                }
            }
            self.rescan_genres(&old_genres)?;
        }
        Ok(track_id)
    }

    fn work_required(&self, genres: &[String]) -> bool {
        match self.cfg.works_scan {
            0 => false,
            1 => true,
            _ => self.cfg.is_classical_genre(genres),
        }
    }

    fn create_work(
        &mut self,
        title: &str,
        worksort: Option<&str>,
        composer: i64,
    ) -> rusqlite::Result<i64> {
        let search = self.rules.search_key(title);
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM works WHERE titlesearch = ? AND composer = ?",
                params![search, composer],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            return Ok(id);
        }
        let sort = match worksort {
            Some(s) if !s.is_empty() => self.rules.sort_key(s),
            _ => {
                let s = self.rules.sort_key(title);
                let re = crate::re!(r"(\d+)");
                re.replace_all(&s, |c: &regex::Captures| {
                    format!("{:04}", c[1].parse::<u64>().unwrap_or(0))
                })
                .into_owned()
            }
        };
        self.conn.execute(
            "INSERT INTO works (composer, title, titlesort, titlesearch) VALUES (?, ?, ?, ?)",
            params![composer, title, sort, search],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn update_persistent(
        &mut self,
        u: &str,
        urlmd5: &str,
        mbid: Option<&str>,
        timestamp: i64,
        rating: Option<i64>,
    ) -> rusqlite::Result<()> {
        let id: Option<i64> = match mbid {
            Some(m) => self
                .conn
                .query_row("SELECT id FROM tracks_persistent WHERE urlmd5 = ? OR musicbrainz_id = ? LIMIT 1", params![urlmd5, m], |r| r.get(0))
                .optional()?,
            None => self.conn.query_row("SELECT id FROM tracks_persistent WHERE urlmd5 = ? LIMIT 1", [urlmd5], |r| r.get(0)).optional()?,
        };
        match id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE tracks_persistent SET url = ?, urlmd5 = ?, musicbrainz_id = COALESCE(?, musicbrainz_id) WHERE id = ?",
                    params![u, urlmd5, mbid, id],
                )?;
                if let Some(r) = rating {
                    self.conn.execute("UPDATE tracks_persistent SET rating = ? WHERE id = ? AND (rating IS NULL OR rating = 0)", params![r, id])?;
                }
            }
            None => {
                let added = if self.first_scan { timestamp } else { now() };
                self.conn.execute(
                    "INSERT INTO tracks_persistent (url, urlmd5, musicbrainz_id, added, rating) VALUES (?, ?, ?, ?, ?)",
                    params![u, urlmd5, mbid, added, rating],
                )?;
            }
        }
        Ok(())
    }

    fn genre_id(&mut self, name: &str) -> rusqlite::Result<i64> {
        let search = self.rules.search_key(name);
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM genres WHERE namesearch = ? LIMIT 1",
                [&search],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO genres (name, namesort, namesearch) VALUES (?, ?, ?)",
            params![name, self.rules.sort_key(name), search],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `_mergeAndCreateContributors` → [(ROLE, [ids])] in `contributorRoles` order.
    fn merge_and_create_contributors(
        &mut self,
        attrs: &mut Attrs,
        is_compilation: Option<i64>,
    ) -> rusqlite::Result<Vec<(String, Vec<i64>)>> {
        if is_compilation != Some(1)
            && attrs.contains_key("ARTIST")
            && attrs.contains_key("ALBUMARTIST")
        {
            if let Some(v) = attrs.shift_remove("ARTIST") {
                attrs.insert("TRACKARTIST".into(), v);
            }
            if let Some(v) = attrs.shift_remove("ARTISTSORT") {
                attrs.insert("TRACKARTISTSORT".into(), v);
            }
            if let Some(v) = attrs.shift_remove("MUSICBRAINZ_ARTIST_ID") {
                attrs.insert("MUSICBRAINZ_TRACKARTIST_ID".into(), v);
            }
        }
        let mut out: Vec<(String, Vec<i64>)> = Vec::new();
        for (role, _) in CONTRIBUTOR_ROLES {
            let Some(vals) = attrs.get(*role).cloned() else {
                continue;
            };
            let vals: Vec<String> = vals
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
            if vals.is_empty() {
                continue;
            }
            let sort = attrs.get(&format!("{}SORT", role)).cloned();
            let mbid = attrs.get(&format!("MUSICBRAINZ_{}_ID", role)).cloned();
            let ids = self.add_contributors(&vals, sort.as_deref(), mbid.as_deref())?;
            if !ids.is_empty() {
                out.push((role.to_string(), ids));
            }
        }
        let has = |r: &str| out.iter().any(|(n, _)| n == r);
        if !has("ALBUMARTIST") && !has("ARTIST") {
            let name = self.cfg.no_artist_string.clone();
            let ids = self.add_contributors(&[name], None, None)?;
            // insert ARTIST keeping alphabetical role order
            let pos = out
                .iter()
                .position(|(n, _)| n.as_str() > "ARTIST")
                .unwrap_or(out.len());
            out.insert(pos, ("ARTIST".into(), ids));
        }
        Ok(out)
    }

    /// `Slim::Schema::Contributor->add`
    fn add_contributors(
        &mut self,
        vals: &[String],
        sort: Option<&[String]>,
        mbids: Option<&[String]>,
    ) -> rusqlite::Result<Vec<i64>> {
        let names = self.rules.split_values(vals);
        let sorts: Vec<String> = match sort {
            Some(s) => self.rules.split_values(s),
            None => names.clone(),
        };
        let mut mbid_list: Vec<String> = match mbids {
            Some(m) => self.rules.split_values(m),
            None => Vec::new(),
        };
        if !mbid_list.is_empty() && mbid_list.len() != names.len() {
            mbid_list.clear();
        }
        let mut ids = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let search = self.rules.search_key(name);
            let sort_src = sorts
                .get(i)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| name.clone());
            let sortkey = self.rules.sort_key(&sort_src);
            let mbid = mbid_list.get(i).cloned();
            let mut found: Option<(i64, Option<String>)>;
            if let Some(m) = &mbid {
                found = self
                    .conn
                    .query_row(
                        "SELECT id, musicbrainz_id FROM contributors WHERE musicbrainz_id = ?",
                        [m],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if found.is_none() {
                    found = self
                        .conn
                        .query_row("SELECT id, musicbrainz_id FROM contributors WHERE name = ? AND musicbrainz_id IS NULL", [name], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?;
                }
            } else {
                found = self
                    .conn
                    .query_row(
                        "SELECT id, musicbrainz_id FROM contributors WHERE name = ? LIMIT 1",
                        [name],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
            }
            let id = match found {
                None => {
                    self.conn.execute(
                        "INSERT INTO contributors (name, namesort, namesearch, musicbrainz_id) VALUES (?, ?, ?, ?)",
                        params![name, sortkey, search, mbid],
                    )?;
                    self.conn.last_insert_rowid()
                }
                Some((id, old_mbid)) => {
                    if search != text::transliterate_latin1(&sortkey) {
                        self.conn.execute(
                            "UPDATE contributors SET namesort = ? WHERE id = ?",
                            params![sortkey, id],
                        )?;
                    }
                    if let Some(m) = &mbid {
                        if old_mbid.as_deref() != Some(m.as_str()) {
                            self.conn.execute(
                                "UPDATE contributors SET musicbrainz_id = ? WHERE id = ?",
                                params![m, id],
                            )?;
                        }
                    }
                    id
                }
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// `mergeSingleVAAlbum($albumid)` (also sets compilation 0 when not yet evaluated).
    pub fn merge_single_va_album(&mut self, album: i64) -> rusqlite::Result<()> {
        let comp: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT compilation FROM albums WHERE id = ?",
                [album],
                |r| r.get(0),
            )
            .optional()?;
        let Some(comp) = comp else { return Ok(()) };
        let is_comp = comp == Some(1) || album_artists_differ(self.conn, album)?;
        if is_comp {
            let va = self.va_id()?;
            self.conn.execute(
                "UPDATE albums SET compilation = 1, contributor = ? WHERE id = ?",
                params![va, album],
            )?;
        } else if comp.is_none() || comp == Some(0) {
            self.conn
                .execute("UPDATE albums SET compilation = 0 WHERE id = ?", [album])?;
        }
        Ok(())
    }

    fn create_or_update_album(&mut self, a: AlbumInput) -> rusqlite::Result<i64> {
        let mut title = a.title.clone();
        let no_album = self.cfg.no_album_string.clone();
        let mut create = true;
        let mut exclude_id: Option<i64> = None;
        let mut hash: AlbumRow = AlbumRow::default();

        if let Some(old) = a.update_from {
            if let Some(old_row) = load_album(self.conn, old) {
                let old_title = old_row.title.clone().unwrap_or_default();
                let new_title = title.clone().unwrap_or_default();
                let different = text::transliterate_latin1(&new_title)
                    != text::transliterate_latin1(&old_title);
                if old_row.extid.is_none()
                    && !new_title.is_empty()
                    && !old_title.is_empty()
                    && ((old_title == no_album && new_title != no_album) || different)
                {
                    if different {
                        exclude_id = Some(old);
                    }
                    create = true;
                } else if !new_title.is_empty() && old_title != no_album {
                    hash = old_row;
                    create = false;
                }
            }
        }

        // No title → "No Album" singleton
        if create && title.as_deref().map(|t| t.is_empty()).unwrap_or(true) {
            if self.no_album_id.is_none() {
                self.no_album_id = self
                    .conn
                    .query_row("SELECT id FROM albums WHERE title = ?", [&no_album], |r| {
                        r.get(0)
                    })
                    .optional()?;
            }
            match self.no_album_id {
                None => {
                    let va = self.va_id()?;
                    self.conn.execute(
                        "INSERT INTO albums (title, titlesort, titlesearch, compilation, year, contributor, release_type) VALUES (?, ?, ?, 0, 0, ?, 'ALBUM')",
                        params![no_album, self.rules.sort_key(&no_album), self.rules.search_key(&no_album), va],
                    )?;
                    let id = self.conn.last_insert_rowid();
                    self.create_year(0)?;
                    self.no_album_id = Some(id);
                    return Ok(id);
                }
                Some(id) => return Ok(id),
            }
        }
        let title_s = title.take().unwrap_or_default();
        let basename = a.basename.clone();

        if create {
            let check_disc = !self.cfg.group_discs && (a.disc.is_some() || a.brainz_id.is_some());
            let fast = match &self.last_album {
                Some(l) => {
                    l.dirname == basename && l.title == title_s && (!check_disc || a.disc == l.disc)
                }
                None => false,
            };
            let mut found: Option<AlbumRow> = None;
            if fast {
                let id = self.last_album.as_ref().unwrap().id;
                found = load_album(self.conn, id);
            }
            if found.is_none() {
                let mut search: Vec<String> = vec!["albums.title = ?".into()];
                let mut values: Vec<rusqlite::types::Value> = vec![title_s.clone().into()];
                let mut join = false;
                if let Some(b) = &a.brainz_id {
                    search.push("albums.musicbrainz_id = ?".into());
                    values.push(b.clone().into());
                }
                if let Some(x) = exclude_id {
                    search.push("albums.id != ?".into());
                    values.push(x.into());
                }
                let mut check_contributor = false;
                if check_disc {
                    if let Some(d) = a.disc {
                        search.push("albums.disc = ?".into());
                        values.push(d.into());
                    }
                    check_contributor = true;
                } else if let Some(dc) = a.discc {
                    search.push("albums.discc = ?".into());
                    values.push(dc.into());
                    check_contributor = true;
                } else if a.disc.is_some() && a.discc.is_none() {
                    search.push("albums.disc IS NOT NULL".into());
                    check_contributor = true;
                }
                if check_contributor {
                    if let Some(cid) = a.contributor_id {
                        let c = if a.is_compilation == Some(1) && !a.has_album_artist {
                            self.va_id()?
                        } else {
                            cid
                        };
                        search.push("albums.contributor = ?".into());
                        values.push(c.into());
                    }
                }
                if a.disc.is_none() {
                    search.push("albums.disc IS NULL".into());
                    if a.discc.is_none() {
                        search.push("albums.discc IS NULL".into());
                    }
                }
                if let Some(c) = a.is_compilation {
                    search.push("albums.compilation = ?".into());
                    values.push(c.into());
                }
                if (check_disc && a.brainz_id.is_none() && a.disc.is_none() && a.discc.is_none())
                    || (!check_disc && a.disc.is_none() && a.discc.is_none())
                {
                    search.push("tracks.url LIKE ? ESCAPE '\\'".into());
                    values.push(format!("{}%", like_escape(&basename)).into());
                    join = true;
                }
                let sql = format!(
                    "SELECT albums.id, albums.title, albums.titlesort, albums.titlesearch, albums.compilation, albums.year, albums.artwork,
                            albums.disc, albums.discc, albums.replay_gain, albums.replay_peak, albums.musicbrainz_id, albums.contributor,
                            albums.extid, albums.release_type FROM albums {} WHERE {} LIMIT 1",
                    if join { "JOIN tracks ON (albums.id = tracks.album)" } else { "" },
                    search.join(" AND ")
                );
                found = self
                    .conn
                    .query_row(
                        &sql,
                        rusqlite::params_from_iter(values.iter()),
                        album_from_row,
                    )
                    .optional()?;
                if let (Some(f), true, None) = (&found, check_disc, a.is_compilation) {
                    if let Some(tn) = a.tracknum {
                        let m: Option<String> = self
                            .conn
                            .query_row(
                                "SELECT url FROM tracks WHERE album = ? AND tracknum = ? LIMIT 1",
                                params![f.id, tn],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(m) = m {
                            if url::dirname_url(&m) != basename {
                                found = None;
                            }
                        }
                    }
                }
            }
            match found {
                Some(f) => hash = f,
                None => {
                    hash = AlbumRow {
                        title: Some(title_s.clone()),
                        ..Default::default()
                    };
                }
            }
        }

        // always normalise
        let sort_src = a
            .albumsort
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| title_s.clone());
        hash.titlesort = Some(self.rules.sort_key(&sort_src));
        hash.titlesearch = Some(self.rules.search_key(&title_s));
        let existing_comp = hash.compilation;
        hash.compilation = a.is_compilation;
        let release = a
            .release_types
            .iter()
            .flat_map(|r| self.rules.split_tag(r))
            .find(|r| !r.eq_ignore_ascii_case("compilation"))
            .unwrap_or_else(|| "album".into());
        hash.release_type = Some(self.rules.search_key(&release));
        let va = self.va_id()?;
        if a.is_compilation == Some(1) && !a.has_album_artist {
            hash.contributor = Some(va);
        } else if let Some(c) = a.contributor_id {
            hash.contributor = Some(c);
            if c == va {
                hash.compilation = Some(1);
            }
        }
        hash.musicbrainz_id = a.brainz_id.clone();
        hash.replay_gain = a.rg_gain.as_deref().and_then(text::clean_gain);
        hash.replay_peak = a.rg_peak.as_deref().and_then(text::clean_gain);
        let mut discc = a.discc;
        if discc.is_some() || self.cfg.group_discs {
            let m = a
                .disc
                .unwrap_or(0)
                .max(a.discc.unwrap_or(0))
                .max(hash.discc.unwrap_or(0));
            discc = if m == 0 { None } else { Some(m) };
        }
        hash.disc = a.disc;
        hash.discc = discc;
        hash.year = Some(a.year);
        if !create {
            hash.title = Some(title_s.clone());
        }
        if let Some(c) = &a.coverid {
            hash.artwork = Some(c.clone());
        }
        if a.is_compilation.is_none() {
            // keep the evaluated flag; re-evaluated after the track is stored
            hash.compilation = match (hash.id, existing_comp) {
                (Some(_), Some(1)) => Some(1),
                (Some(_), c) => c.or(Some(0)),
                (None, _) => Some(0),
            };
            if hash.compilation == Some(1) {
                hash.contributor = Some(va);
            }
        }
        self.create_year(a.year)?;

        let id = match hash.id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE albums SET title = ?, titlesort = ?, titlesearch = ?, compilation = ?, year = ?, artwork = ?, disc = ?, discc = ?,
                       replay_gain = ?, replay_peak = ?, musicbrainz_id = ?, contributor = ?, release_type = ? WHERE id = ?",
                    params![
                        hash.title, hash.titlesort, hash.titlesearch, hash.compilation, hash.year, hash.artwork, hash.disc, hash.discc,
                        hash.replay_gain, hash.replay_peak, hash.musicbrainz_id, hash.contributor, hash.release_type, id
                    ],
                )?;
                id
            }
            None => {
                self.conn.execute(
                    "INSERT INTO albums (title, titlesort, titlesearch, compilation, year, artwork, disc, discc, replay_gain, replay_peak,
                       musicbrainz_id, contributor, release_type) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        hash.title, hash.titlesort, hash.titlesearch, hash.compilation, hash.year, hash.artwork, hash.disc, hash.discc,
                        hash.replay_gain, hash.replay_peak, hash.musicbrainz_id, hash.contributor, hash.release_type
                    ],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        self.last_album = Some(LastAlbum {
            id,
            title: title_s,
            disc: a.disc,
            dirname: basename,
        });
        Ok(id)
    }

    fn create_year(&self, y: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO years (id) VALUES (?)", [y])?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // post processing
    // -----------------------------------------------------------------------------------------

    /// Album artwork (embedded first, then disc/tracknum), customsearch, orphans, years.
    /// `all`: process every album (wipe/full rebuild) instead of only the touched ones.
    pub fn post_process(&mut self, all: bool, analyze: bool) -> anyhow::Result<()> {
        self.set_step("post_processing", 4);
        self.begin()?;
        let albums: Vec<i64> = if all {
            let mut st = self.conn.prepare("SELECT id FROM albums")?;
            let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        } else {
            self.touched_albums.iter().copied().collect()
        };
        {
            let mut st = self.conn.prepare_cached(
                "SELECT coverid FROM tracks WHERE album = ? AND coverid IS NOT NULL AND audio = 1
                 ORDER BY (CASE WHEN cover GLOB '[0-9]*' THEN 0 ELSE 1 END), disc, tracknum LIMIT 1",
            )?;
            let mut up = self
                .conn
                .prepare_cached("UPDATE albums SET artwork = ? WHERE id = ?")?;
            for a in &albums {
                let c: Option<String> = st.query_row([a], |r| r.get(0)).optional()?;
                up.execute(params![c, a])?;
            }
        }
        self.step_done(1, None);
        // orphan cleanup
        self.conn.execute_batch(
            "DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album FROM tracks WHERE album IS NOT NULL);
             DELETE FROM genres WHERE id NOT IN (SELECT DISTINCT genre FROM genre_track);
             DELETE FROM years WHERE id NOT IN (SELECT DISTINCT year FROM tracks WHERE year IS NOT NULL);
             DELETE FROM works WHERE id NOT IN (SELECT DISTINCT work FROM tracks WHERE work IS NOT NULL);",
        )?;
        let va = self.va_id()?;
        self.conn.execute(
            "DELETE FROM contributors WHERE id != ? AND id NOT IN (SELECT DISTINCT contributor FROM contributor_track)
               AND id NOT IN (SELECT DISTINCT contributor FROM albums WHERE contributor IS NOT NULL)",
            [va],
        )?;
        self.step_done(2, None);
        // customsearch: words used by the multi-word search (title + album + contributors)
        const ALBUM_CS: &str = "UPDATE albums SET customsearch = COALESCE(titlesearch, '') || ' ' || COALESCE((SELECT GROUP_CONCAT(c.namesearch, ' ')
                FROM contributor_album ca JOIN contributors c ON c.id = ca.contributor WHERE ca.album = albums.id), '')";
        const TRACK_CS: &str = "UPDATE tracks SET customsearch = COALESCE(titlesearch, '') || ' ' || COALESCE((SELECT titlesearch FROM albums WHERE albums.id = tracks.album), '')
                || ' ' || COALESCE((SELECT GROUP_CONCAT(c.namesearch, ' ') FROM contributor_track ct JOIN contributors c ON c.id = ct.contributor
                WHERE ct.track = tracks.id), '') WHERE audio = 1";
        if all {
            self.conn.execute(ALBUM_CS, [])?;
            self.conn.execute(TRACK_CS, [])?;
        } else {
            let mut sa = self
                .conn
                .prepare_cached(&format!("{} WHERE id = ?", ALBUM_CS))?;
            let mut stt = self
                .conn
                .prepare_cached(&format!("{} AND album = ?", TRACK_CS))?;
            for a in &albums {
                sa.execute([a])?;
                stt.execute([a])?;
            }
            // tracks without album (should not happen for audio) / NULL customsearch leftovers
            self.conn
                .execute(&format!("{} AND customsearch IS NULL", TRACK_CS), [])?;
        }
        self.touched_albums.clear();
        self.step_done(3, None);
        self.commit()?;
        if analyze {
            self.conn.execute_batch("ANALYZE;")?;
        }
        self.step_done(4, None);
        self.finish_step("post_processing");
        Ok(())
    }

    /// Remove local tracks that are no longer below a configured media dir (or the playlist dir).
    /// LMS triggers a wipe when a media dir is removed; we clean up incrementally instead.
    pub fn remove_outside_roots(
        &mut self,
        roots: &[PathBuf],
        playlist_dir: Option<&Path>,
    ) -> anyhow::Result<()> {
        let mut prefixes: Vec<String> = roots
            .iter()
            .map(|r| format!("{}/", file_url_from_path(r)))
            .collect();
        if let Some(p) = playlist_dir {
            prefixes.push(format!("{}/", file_url_from_path(p)));
        }
        let urls: Vec<String> = {
            let mut st = self.conn.prepare(
                "SELECT url FROM tracks WHERE url LIKE 'file:%' AND virtual IS NULL AND content_type NOT IN ('dir')",
            )?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok())
                .filter(|u| {
                    !prefixes
                        .iter()
                        .any(|p| u.len() >= p.len() && u[..p.len()].eq_ignore_ascii_case(p))
                })
                .collect()
        };
        if urls.is_empty() {
            return Ok(());
        }
        self.begin()?;
        self.resurrect_containers = false;
        for u in urls {
            if self.aborted() {
                break;
            }
            // may already be gone (containers removed together with their cue sheet)
            self.deleted(&u)?;
            self.summary.deleted += 1;
            self.changes += 1;
            self.tick()?;
        }
        self.resurrect_containers = true;
        self.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct AlbumInput {
    title: Option<String>,
    albumsort: Option<String>,
    disc: Option<i64>,
    discc: Option<i64>,
    brainz_id: Option<String>,
    release_types: Vec<String>,
    rg_gain: Option<String>,
    rg_peak: Option<String>,
    is_compilation: Option<i64>,
    contributor_id: Option<i64>,
    has_album_artist: bool,
    basename: String,
    tracknum: Option<i64>,
    year: i64,
    coverid: Option<String>,
    update_from: Option<i64>,
}

fn album_artists_differ(c: &Connection, album: i64) -> rusqlite::Result<bool> {
    let mut st = c.prepare_cached(
        "SELECT contributor, track FROM contributor_track WHERE track IN (SELECT id FROM tracks WHERE album = ?) AND role = 1
         ORDER BY contributor, track",
    )?;
    let rows = st.query_map([album], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut map: HashMap<i64, String> = HashMap::new();
    for r in rows.flatten() {
        map.entry(r.0).or_default().push_str(&format!("{}:", r.1));
    }
    let mut prev: Option<&String> = None;
    for v in map.values() {
        if let Some(p) = prev {
            if p != v {
                return Ok(true);
            }
        }
        prev = Some(v);
    }
    Ok(false)
}

pub fn ids_for(c: &Connection, sql: &str, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut st = c.prepare_cached(sql)?;
    let rows = st.query_map([id], |r| r.get::<_, i64>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Escape `%`, `_` and `\` for LIKE ... ESCAPE '\'.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `generateImageId`
pub fn image_id(u: &str, mtime: i64, size: i64) -> Option<String> {
    if mtime == 0 || size == 0 {
        return None;
    }
    Some(md5_hex(&format!("{}{}{}", u, mtime, size))[..8].to_string())
}

/// `findStandaloneArtwork`
pub fn find_standalone_artwork(dir: &Path, cover_art: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut found: Vec<PathBuf> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".gif"))
        {
            continue;
        }
        if !name_allowed(&name, None) {
            continue;
        }
        if e.path().is_file() {
            found.push(e.path());
        }
    }
    found.sort();
    let mut names: Vec<String> = vec![
        "cover".into(),
        "folder".into(),
        "album".into(),
        "thumb".into(),
    ];
    if !cover_art.is_empty() && !cover_art.starts_with('%') {
        names.push(cover_art.to_ascii_lowercase());
    }
    for f in &found {
        let base = f
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if names
            .iter()
            .any(|n| base.starts_with(&format!("{}.", n)) || base == *n)
        {
            return Some(f.clone());
        }
    }
    found.into_iter().next()
}

/// `variousArtistsObject`: lookup by namesearch, create if missing, rename if the string changed.
pub fn various_artists_id(
    c: &Connection,
    cfg: &LibraryConfig,
    rules: &TextRules,
) -> rusqlite::Result<i64> {
    let name = &cfg.various_artists_string;
    let search = rules.search_key(name);
    let row: Option<(i64, String)> = c
        .query_row(
            "SELECT id, name FROM contributors WHERE namesearch = ? LIMIT 1",
            [&search],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((id, n)) => {
            if &n != name {
                c.execute(
                    "UPDATE contributors SET name = ?, namesort = ? WHERE id = ?",
                    params![name, rules.sort_key(name), id],
                )?;
            }
            Ok(id)
        }
        None => {
            c.execute(
                "INSERT INTO contributors (name, namesort, namesearch) VALUES (?, ?, ?)",
                params![name, rules.sort_key(name), search],
            )?;
            Ok(c.last_insert_rowid())
        }
    }
}
