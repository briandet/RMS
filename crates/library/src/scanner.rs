//! Library scanner: discovery, deleted/changed/new diffing and the track/album/artist logic
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
    title_sort: Option<String>,
    title_search: Option<String>,
    compilation: Option<i64>,
    year: Option<i64>,
    artwork: Option<String>,
    disc: Option<i64>,
    disc_count: Option<i64>,
    replay_gain: Option<f64>,
    replay_peak: Option<f64>,
    mbid: Option<String>,
    artist_id: Option<i64>,
    release_type: Option<String>,
}

const ALBUM_COLS: &str = "albums.id, albums.title, albums.title_sort, albums.title_search, albums.compilation,
    albums.year, albums.artwork, albums.disc, albums.disc_count, albums.replay_gain, albums.replay_peak,
    albums.mbid, albums.artist_id, albums.release_type";

fn load_album(c: &Connection, id: i64) -> Option<AlbumRow> {
    c.query_row(
        &format!("SELECT {ALBUM_COLS} FROM albums WHERE id = ?"),
        [id],
        album_from_row,
    )
    .ok()
}

fn album_from_row(r: &rusqlite::Row) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: r.get(0)?,
        title: r.get(1)?,
        title_sort: r.get(2)?,
        title_search: r.get(3)?,
        compilation: r.get(4)?,
        year: r.get(5)?,
        artwork: r.get(6)?,
        disc: r.get(7)?,
        disc_count: r.get(8)?,
        replay_gain: r.get(9)?,
        replay_peak: r.get(10)?,
        mbid: r.get(11)?,
        artist_id: r.get(12)?,
        release_type: r.get(13)?,
    })
}

/// Everything needed to write one `tracks` row.
#[derive(Debug, Clone, Default)]
pub struct TrackInput {
    pub url: String,
    pub path: PathBuf,
    pub file_id: i64,
    pub attrs: Attrs,
    pub content_type: String,
    pub info: AudioInfo,
    /// embedded cover length
    pub cover_len: Option<usize>,
    /// the file's mtime and size
    pub timestamp: i64,
    pub filesize: i64,
    /// segment of the file (cue sheets)
    pub cue: Option<(f64, f64)>,
}

/// A row of `files`.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub kind: String,
    pub cue: Option<String>,
}

pub fn file_row(c: &Connection, u: &str) -> rusqlite::Result<Option<FileRow>> {
    c.query_row("SELECT id, kind, cue FROM files WHERE url = ?", [u], |r| {
        Ok(FileRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            cue: r.get(2)?,
        })
    })
    .optional()
}

/// Insert or update a `files` row; returns its id.
pub fn upsert_file(
    c: &Connection,
    u: &str,
    kind: &str,
    cue: Option<&str>,
    mtime: i64,
    size: i64,
) -> rusqlite::Result<i64> {
    c.query_row(
        "INSERT INTO files (url, kind, cue, mtime, size) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (url) DO UPDATE SET kind = excluded.kind, cue = excluded.cue,
           mtime = excluded.mtime, size = excluded.size
         RETURNING id",
        params![u, kind, cue, mtime, size],
        |r| r.get(0),
    )
}

/// Identifies a track's audio so its statistics and analysis follow it when the file is
/// moved or renamed: MD5 over the audio size and 8 KiB from the middle and from the end of
/// the audio data (where tags rarely are), plus the cue segment.
pub fn fingerprint(path: &Path, info: &AudioInfo, cue: Option<(f64, f64)>) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 8192;
    let mut f = std::fs::File::open(path).ok()?;
    let file_len = f.metadata().ok()?.len();
    let start = info.offset.unwrap_or(0).min(file_len);
    let len = info.size.unwrap_or(file_len - start).min(file_len - start);
    if len == 0 {
        return None;
    }
    let mut ctx = md5::Context::new();
    ctx.consume(len.to_le_bytes());
    let mut buf = vec![0u8; CHUNK as usize];
    for at in [start + len / 2, start + len.saturating_sub(CHUNK)] {
        f.seek(SeekFrom::Start(at)).ok()?;
        let n = (&mut f).take(CHUNK).read(&mut buf).ok()?;
        ctx.consume(&buf[..n]);
    }
    if let Some((s, e)) = cue {
        ctx.consume(format!("#{}-{}", cue::fmt_secs(s), cue::fmt_secs(e)));
    }
    Some(format!("{:x}", ctx.finalize()))
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
    /// analysis results converted from a Lyrion-schema database, not yet matched to tracks
    legacy_analysis: bool,
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
        let first_scan = crate::db::get_meta(conn, "last_scan").is_none();
        let legacy_analysis = crate::db::table_exists(conn, "legacy_analysis").unwrap_or(false);
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
            legacy_analysis,
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
    }

    pub(crate) fn step_done(&self, done: usize, info: Option<&str>) {
        let mut p = self.progress.lock();
        p.done = done;
        if let Some(i) = info {
            p.info = Some(i.to_string());
        }
    }

    pub(crate) fn finish_step(&self, _name: &str) {
        self.progress.lock().info = None;
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

    /// Audio scan of one media dir.
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
        // audio files and cue sheets, but not saved playlists (the playlist scan has those)
        let scope = "files.kind IN ('audio', 'cue') AND NOT EXISTS (SELECT 1 FROM playlists
            WHERE playlists.file_id = files.id AND playlists.kind = 'saved')";
        self.process_diff(root, &base, scope, wipe, recursive, false)
    }

    /// Playlist folder scan.
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
        let scope = "EXISTS (SELECT 1 FROM playlists
            WHERE playlists.file_id = files.id AND playlists.kind = 'saved')";
        self.process_diff(root, &base, scope, false, true, true)
    }

    fn fill_scanned_files(
        &mut self,
        base: &str,
        found: &[Found],
        recursive: bool,
    ) -> rusqlite::Result<()> {
        // per connection, so concurrent scanners (folder browsing) don't see each other's
        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS scanned (url TEXT NOT NULL, mtime INTEGER, size INTEGER);
             CREATE INDEX IF NOT EXISTS temp.scanned_url ON scanned (url);",
        )?;
        self.begin()?;
        if recursive {
            self.conn.execute(
                "DELETE FROM scanned WHERE url = ? OR url LIKE ? ESCAPE '\\'",
                params![base, format!("{}/%", like_escape(base))],
            )?;
        } else {
            for f in found {
                self.conn
                    .execute("DELETE FROM scanned WHERE url = ?", [&f.url])?;
            }
        }
        {
            let mut st = self
                .conn
                .prepare_cached("INSERT INTO scanned (url, mtime, size) VALUES (?, ?, ?)")?;
            for f in found {
                st.execute(params![f.url, f.mtime, f.size])?;
            }
        }
        self.commit()
    }

    /// Compare what was found below `base` with the `files` in `scope` and process the
    /// deleted, changed and new files.
    fn process_diff(
        &mut self,
        root: &Path,
        base: &str,
        scope: &str,
        wipe: bool,
        recursive: bool,
        playlists: bool,
    ) -> anyhow::Result<()> {
        let like = format!("{}%", like_escape(base));
        let max_id: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM files", [], |r| r.get(0))?;
        let dir_clause = if recursive {
            String::new()
        } else {
            // only direct children of base
            format!(" AND instr(substr(files.url, {}), '/') = 0", base.len() + 2)
        };
        let deleted: Vec<String> = if wipe {
            Vec::new()
        } else {
            let sql = format!(
                "SELECT files.url FROM files WHERE files.url NOT IN (SELECT url FROM scanned WHERE size != 0)
                 AND files.url LIKE ? ESCAPE '\\' AND {scope}{dir_clause}"
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map([&like], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let new: Vec<String> = {
            let sql = "SELECT DISTINCT url FROM scanned WHERE url NOT IN (SELECT url FROM files WHERE id <= ?)
                       AND url LIKE ? ESCAPE '\\' AND size != 0";
            let mut st = self.conn.prepare(sql)?;
            let rows = st.query_map(params![max_id, &like], |r| r.get::<_, String>(0))?;
            let mut v: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            // process cue sheets first, so referenced files are split into tracks once
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
                "SELECT DISTINCT scanned.url FROM scanned JOIN files ON (scanned.url = files.url
                   AND (scanned.mtime != files.mtime OR scanned.size != files.size) AND {scope})
                 WHERE scanned.url LIKE ? ESCAPE '\\'"
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
                changed_albums.extend(self.albums_of_file(u)?);
                if let Err(e) = self.changed(u) {
                    tracing::warn!("error rescanning {}: {}", u, e);
                }
                changed_albums.extend(self.albums_of_file(u)?);
                self.summary.changed += 1;
                self.changes += 1;
                self.step_done(i + 1, Some(u));
                self.tick()?;
            }
            for a in &changed_albums {
                self.rebuild_album_artists(*a)?;
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

    fn albums_of_file(&self, u: &str) -> rusqlite::Result<Vec<i64>> {
        let mut st = self.conn.prepare_cached(
            "SELECT DISTINCT tracks.album_id FROM tracks JOIN files ON files.id = tracks.file_id
             WHERE files.url = ? AND tracks.album_id IS NOT NULL",
        )?;
        let rows = st.query_map([u], |r| r.get(0))?;
        rows.collect()
    }

    fn rebuild_album_artists(&self, album: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM album_artists WHERE album_id = ?", [album])?;
        self.conn.execute(
            "INSERT OR IGNORE INTO album_artists (album_id, artist_id, role)
             SELECT DISTINCT tracks.album_id, track_artists.artist_id, track_artists.role
             FROM track_artists JOIN tracks ON tracks.id = track_artists.track_id
             WHERE tracks.album_id = ?",
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

    /// A file found for the first time.  Returns whether it was added.
    pub fn new_url(&mut self, u: &str, playlist_scan: bool) -> anyhow::Result<bool> {
        let Some(path) = path_from_file_url(u) else {
            return Ok(false);
        };
        // already known: e.g. split by a cue sheet, or added through a playlist
        if file_row(self.conn, u)?.is_some() {
            return Ok(false);
        }
        let ct = types::type_from_path(u);
        if types::is_song_type(ct) && !playlist_scan {
            self.scan_audio_file(u, &path, None)?;
            return Ok(true);
        }
        if ct == "cue" {
            self.scan_cue_file(u, &path)?;
            return Ok(true);
        }
        if types::is_playlist_type(ct) && self.in_playlist_dir(&path) {
            self.scan_playlist_file(u, &path)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// A known file whose mtime or size changed.
    pub fn changed(&mut self, u: &str) -> anyhow::Result<()> {
        let Some(file) = file_row(self.conn, u)? else {
            return Ok(());
        };
        let Some(path) = path_from_file_url(u) else {
            return Ok(());
        };
        match (file.kind.as_str(), file.cue.as_deref()) {
            ("audio", Some("embedded")) => {
                // its own cue sheet may have changed: drop the segments and read it again
                self.delete_file_tracks(file.id)?;
                self.scan_audio_file(u, &path, None)?;
            }
            ("audio", Some("external")) => {
                // split by .cue files: read those again
                let (mt, sz, _) = file_stat(&path).unwrap_or((0, 0, false));
                self.conn.execute(
                    "UPDATE files SET mtime = ?, size = ? WHERE id = ?",
                    params![mt, sz, file.id],
                )?;
                for c in self.cue_sheets_splitting(u)? {
                    self.deleted(&c)?;
                    if let Some(p) = path_from_file_url(&c) {
                        if p.is_file() {
                            self.scan_cue_file(&c, &p)?;
                        }
                    }
                }
            }
            ("audio", _) => {
                let existing: Option<i64> = self
                    .conn
                    .query_row("SELECT id FROM tracks WHERE url = ?", [u], |r| r.get(0))
                    .optional()?;
                self.scan_audio_file(u, &path, existing)?;
            }
            ("cue", _) => {
                self.deleted(u)?;
                self.scan_cue_file(u, &path)?;
            }
            ("playlist", _) => {
                self.deleted(u)?;
                if self.in_playlist_dir(&path) {
                    self.scan_playlist_file(u, &path)?;
                }
            }
            _ => {
                let (mt, sz, _) = file_stat(&path).unwrap_or((0, 0, false));
                self.conn.execute(
                    "UPDATE files SET mtime = ?, size = ? WHERE id = ?",
                    params![mt, sz, file.id],
                )?;
            }
        }
        Ok(())
    }

    /// URLs of the cue sheets that split the audio file `u` into tracks.
    fn cue_sheets_splitting(&self, u: &str) -> rusqlite::Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT DISTINCT playlists.url FROM playlist_items
             JOIN playlists ON playlists.id = playlist_items.playlist_id
             JOIN files ON files.id = playlists.file_id
             WHERE playlist_items.url LIKE ? ESCAPE '\\' AND files.kind = 'cue'",
        )?;
        let rows = st.query_map([format!("{}#%", like_escape(u))], |r| r.get(0))?;
        rows.collect()
    }

    /// Delete every track of a file, with the usual cleanup.
    fn delete_file_tracks(&mut self, file_id: i64) -> anyhow::Result<()> {
        for id in ids_for(
            self.conn,
            "SELECT id FROM tracks WHERE file_id = ?",
            file_id,
        )? {
            self.delete_track_id(id)?;
        }
        Ok(())
    }

    /// A known file that is gone.
    pub fn deleted(&mut self, u: &str) -> anyhow::Result<()> {
        let Some(file) = file_row(self.conn, u)? else {
            return Ok(());
        };
        if file.kind == "cue" {
            let entries: Vec<String> = {
                let mut st = self.conn.prepare(
                    "SELECT playlist_items.url FROM playlist_items
                     JOIN playlists ON playlists.id = playlist_items.playlist_id
                     WHERE playlists.file_id = ? ORDER BY position",
                )?;
                let rows = st.query_map([file.id], |r| r.get::<_, String>(0))?;
                rows.filter_map(|r| r.ok()).collect()
            };
            self.conn
                .execute("DELETE FROM files WHERE id = ?", [file.id])?;
            let mut bases: HashSet<String> = HashSet::new();
            for e in entries {
                let seg: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT id FROM tracks WHERE url = ? AND cue_start IS NOT NULL",
                        [&e],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = seg {
                    self.delete_track_id(id)?;
                }
                bases.insert(url::strip_anchor(&e).to_string());
            }
            // audio files no cue sheet splits any more are whole tracks again
            for b in bases {
                let Some(container) = file_row(self.conn, &b)? else {
                    continue;
                };
                let left: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM tracks WHERE file_id = ?",
                    [container.id],
                    |r| r.get(0),
                )?;
                if left == 0 && container.cue.as_deref() == Some("external") {
                    self.conn
                        .execute("DELETE FROM files WHERE id = ?", [container.id])?;
                    if let (Some(p), true) = (path_from_file_url(&b), self.resurrect_containers) {
                        if p.is_file() {
                            let _ = self.scan_audio_file(&b, &p, None);
                        }
                    }
                }
            }
            return Ok(());
        }
        if file.kind == "audio" {
            self.delete_file_tracks(file.id)?;
        }
        // playlists and their items go with the file
        self.conn
            .execute("DELETE FROM files WHERE id = ?", [file.id])?;
        Ok(())
    }

    /// Delete a track and clean up albums, artists and genres.
    pub fn delete_track_id(&mut self, id: i64) -> anyhow::Result<()> {
        let album: Option<i64> = self
            .conn
            .query_row("SELECT album_id FROM tracks WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let artists = ids_for(
            self.conn,
            "SELECT DISTINCT artist_id FROM track_artists WHERE track_id = ?",
            id,
        )?;
        let genres = ids_for(
            self.conn,
            "SELECT genre_id FROM track_genres WHERE track_id = ?",
            id,
        )?;
        // relations cascade
        self.conn.execute("DELETE FROM tracks WHERE id = ?", [id])?;
        self.rescan_artists(&artists)?;
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
            self.rebuild_album_artists(a)?;
            self.touched_albums.insert(a);
        }
        self.rescan_genres(&genres)?;
        Ok(())
    }

    fn rescan_artists(&mut self, ids: &[i64]) -> rusqlite::Result<()> {
        let va = self.va_id()?;
        for id in ids {
            if *id == va {
                continue;
            }
            // keep artists still on a track or credited for an album
            self.conn.execute(
                "DELETE FROM artists WHERE id = ?1
                   AND NOT EXISTS (SELECT 1 FROM track_artists WHERE artist_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM albums WHERE artist_id = ?1)",
                [id],
            )?;
        }
        Ok(())
    }

    fn rescan_album(&mut self, id: i64) -> rusqlite::Result<()> {
        let n = self.conn.execute(
            "DELETE FROM albums WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM tracks WHERE album_id = ?1)",
            [id],
        )?;
        if n > 0 {
            if self.last_album.as_ref().map(|l| l.id) == Some(id) {
                self.last_album = None;
            }
            if self.no_album_id == Some(id) {
                self.no_album_id = None;
            }
        }
        Ok(())
    }

    fn rescan_genres(&mut self, ids: &[i64]) -> rusqlite::Result<()> {
        for id in ids {
            self.conn.execute(
                "DELETE FROM genres WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM track_genres WHERE genre_id = ?1)",
                [id],
            )?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // audio files
    // -----------------------------------------------------------------------------------------

    /// Read tags and store an audio file (new or changed). Handles embedded cue sheets.
    /// Returns the id of its file.
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

        // FLAC with its own cue sheet: one track per cue sheet entry
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
                    let file_id =
                        upsert_file(self.conn, u, "audio", Some("embedded"), mtime, size)?;
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
                        let input = self.segment_input(
                            u, path, file_id, attrs, &rt, tr.start, tr.end, mtime, size,
                        );
                        self.store_track(input, None)?;
                    }
                    return Ok(file_id);
                }
            }
        }

        let file_id = upsert_file(self.conn, u, "audio", None, mtime, size)?;
        let input = TrackInput {
            url: u.to_string(),
            path: path.to_path_buf(),
            file_id,
            attrs: rt.attrs,
            content_type: rt.content_type,
            info: rt.info,
            cover_len: rt.cover_len,
            timestamp: mtime,
            filesize: size,
            cue: None,
        };
        self.store_track(input, existing)?;
        Ok(file_id)
    }

    /// The track for one cue sheet entry of `file_url`.
    #[allow(clippy::too_many_arguments)]
    fn segment_input(
        &self,
        file_url: &str,
        path: &Path,
        file_id: i64,
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
            url: format!(
                "{}#{}-{}",
                file_url,
                cue::fmt_secs(start),
                cue::fmt_secs(end)
            ),
            path: path.to_path_buf(),
            file_id,
            attrs,
            content_type: base.content_type.clone(),
            info,
            cover_len: base.cover_len,
            timestamp: mtime,
            filesize: size,
            cue: Some((start, end)),
        }
    }

    /// Mark an audio file as split by a separate cue sheet (its whole-file track goes).
    fn split_by_cue_sheet(&mut self, u: &str, mtime: i64, size: i64) -> anyhow::Result<i64> {
        if let Some(f) = file_row(self.conn, u)? {
            if f.cue.is_none() {
                self.delete_file_tracks(f.id)?;
            }
        }
        Ok(upsert_file(
            self.conn,
            u,
            "audio",
            Some("external"),
            mtime,
            size,
        )?)
    }

    // -----------------------------------------------------------------------------------------
    // cue sheets & playlists
    // -----------------------------------------------------------------------------------------

    /// The `playlists` row of a cue sheet or playlist file; its items are cleared.
    fn playlist_row(
        &mut self,
        u: &str,
        file_kind: &str,
        kind: &str,
        name: &str,
        mtime: i64,
        size: i64,
    ) -> anyhow::Result<i64> {
        let file_id = upsert_file(self.conn, u, file_kind, None, mtime, size)?;
        let id: i64 = self.conn.query_row(
            "INSERT INTO playlists (file_id, url, kind, name, name_sort, name_search) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (url) DO UPDATE SET file_id = excluded.file_id, kind = excluded.kind,
               name = excluded.name, name_sort = excluded.name_sort, name_search = excluded.name_search
             RETURNING id",
            params![file_id, u, kind, name, self.rules.sort_key(name), self.rules.search_key(name)],
            |r| r.get(0),
        )?;
        self.conn
            .execute("DELETE FROM playlist_items WHERE playlist_id = ?", [id])?;
        Ok(id)
    }

    pub fn set_playlist_tracks(&self, playlist: i64, urls: &[String]) -> rusqlite::Result<()> {
        crate::set_playlist_tracks(self.conn, playlist, urls)
    }

    /// A `.cue` file: a playlist of its entries (saved when in the playlist folder), and one
    /// track per entry in the audio files it splits.
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
        let kind = if self.in_playlist_dir(path) {
            "saved"
        } else {
            "cue"
        };
        let title = url::plain_title(u, false);
        let pid = self.playlist_row(u, "cue", kind, &title, mtime, size)?;
        let Some(sheet) = sheet else {
            return Ok(());
        };
        let mut uris = Vec::new();
        let mut containers: HashMap<PathBuf, i64> = HashMap::new();
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
            let file_id = match containers.get(&tr.file) {
                Some(id) => *id,
                None => {
                    let id = self.split_by_cue_sheet(&furl, fm, fs)?;
                    containers.insert(tr.file.clone(), id);
                    id
                }
            };
            let mut attrs: Attrs = Attrs::new();
            for (k, v) in &tr.attrs {
                if k == "CONTENT_TYPE" {
                    continue;
                }
                attrs.insert(k.clone(), vec![v.clone()]);
            }
            attrs.insert("TRACKNUM".into(), vec![tr.tracknum.to_string()]);
            let input = self.segment_input(
                &furl, &tr.file, file_id, attrs, &base_rt, tr.start, tr.end, fm, fs,
            );
            let vurl = input.url.clone();
            self.store_track(input, None)?;
            uris.push(vurl);
        }
        self.set_playlist_tracks(pid, &uris)?;
        Ok(())
    }

    /// A playlist file (`.m3u`, `.pls`, …); saved when in the playlist folder.
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
        let kind = if self.in_playlist_dir(path) {
            "saved"
        } else {
            "file"
        };
        let title = url::plain_title(u, false);
        let pid = self.playlist_row(u, "playlist", kind, &title, mtime, size)?;
        let mut urls = Vec::new();
        for e in entries {
            // make sure local files referenced by the playlist are in the library
            if url::is_file_url(&e.url)
                && url::anchor_from_url(&e.url).is_none()
                && file_row(self.conn, &e.url)?.is_none()
            {
                if let Some(p) = path_from_file_url(&e.url) {
                    if types::is_song_type(types::type_from_path(&e.url)) && p.is_file() {
                        let _ = self.scan_audio_file(&e.url, &p, None);
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

    /// Store a track (insert, or update `existing` / the track with the same URL).
    /// Returns the track id.
    pub fn store_track(&mut self, input: TrackInput, existing: Option<i64>) -> anyhow::Result<i64> {
        let cfg = self.cfg;
        let mut attrs = input.attrs.clone();
        let existing = match existing {
            Some(id) => Some(id),
            None => self
                .conn
                .query_row("SELECT id FROM tracks WHERE url = ?", [&input.url], |r| {
                    r.get(0)
                })
                .optional()?,
        };

        // --- _preCheckAttributes ---
        let title = attrs
            .get("TITLE")
            .map(|v| v.join(" / "))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| url::plain_title(&input.url, false));
        let title_sort_src = first(&attrs, "TITLESORT")
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| title.clone());
        let title_sort = self.rules.sort_key(&title_sort_src);
        let title_search = self.rules.search_key(&title);
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
        let disc_count = num(&attrs, "DISCC").map(|d| d.min(254));
        let bpm = num(&attrs, "BPM");
        let track_number = first(&attrs, "TRACKNUM").and_then(text::clean_track_number);
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
        let disc_subtitle = trimmed("DISCSUBTITLE");
        let subtitle = trimmed("SUBTITLE");
        let mbid = first(&attrs, "MUSICBRAINZ_ID")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
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
        let mut old_artists: Vec<i64> = Vec::new();
        let mut old_genres: Vec<i64> = Vec::new();
        if let Some(id) = existing {
            old_album = self
                .conn
                .query_row("SELECT album_id FROM tracks WHERE id = ?", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            old_artists = ids_for(
                self.conn,
                "SELECT DISTINCT artist_id FROM track_artists WHERE track_id = ?",
                id,
            )?;
            old_genres = ids_for(
                self.conn,
                "SELECT genre_id FROM track_genres WHERE track_id = ?",
                id,
            )?;
        }

        // --- artists ---
        let artists = self.merge_and_create_artists(&mut attrs, is_compilation)?;
        let get_first = |r: &str| {
            artists
                .iter()
                .find(|(n, _)| n == r)
                .and_then(|(_, ids)| ids.first().copied())
        };
        let primary_artist = get_first("ARTIST").or_else(|| get_first("TRACKARTIST"));
        let album_artist = get_first("ALBUMARTIST").or_else(|| get_first("ARTIST"));
        let has_album_artist = get_first("ALBUMARTIST").is_some();

        // --- artwork ---
        let (cover, cover_id) = if let Some(len) = input.cover_len {
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
            disc_count,
            mbid: first(&attrs, "MUSICBRAINZ_ALBUM_ID").map(|s| s.to_string()),
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
            artist_id: album_artist,
            has_album_artist,
            basename: dirname,
            track_number,
            year,
            cover_id: cover_id.clone(),
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
        let info = &input.info;
        let fingerprint = fingerprint(&input.path, info, input.cue);
        let (cue_start, cue_end) = match input.cue {
            Some((s, e)) => (Some(s), Some(e)),
            None => (None, None),
        };
        let values = params![
            input.file_id,
            input.url,
            cue_start,
            cue_end,
            fingerprint,
            title,
            title_sort,
            title_search,
            album_id,
            primary_artist,
            work_id,
            track_number,
            disc,
            disc_subtitle,
            year,
            info.secs,
            input.content_type,
            info.bitrate.map(|b| b.round() as i64),
            info.vbr.unwrap_or(false) as i64,
            info.samplerate,
            info.samplesize,
            info.channels,
            info.lossless as i64,
            info.endian,
            info.block_align,
            info.offset.map(|v| v as i64),
            info.size.map(|v| v as i64),
            bpm,
            info.tagversion,
            replay_gain,
            replay_peak,
            mbid,
            lyrics,
            subtitle,
            grouping,
            performance,
            cover,
            cover_id,
            t,
        ];
        let track_id = if let Some(id) = existing {
            self.conn.execute(
                "UPDATE tracks SET file_id = ?1, url = ?2, cue_start = ?3, cue_end = ?4, fingerprint = ?5,
                   title = ?6, title_sort = ?7, title_search = ?8, album_id = ?9, artist_id = ?10, work_id = ?11,
                   track_number = ?12, disc = ?13, disc_subtitle = ?14, year = ?15, duration = ?16, format = ?17,
                   bitrate = ?18, vbr = ?19, sample_rate = ?20, sample_size = ?21, channels = ?22, lossless = ?23,
                   big_endian = ?24, block_align = ?25, audio_offset = ?26, audio_size = ?27, bpm = ?28,
                   tag_version = ?29, replay_gain = ?30, replay_peak = ?31, mbid = ?32, lyrics = ?33,
                   subtitle = ?34, grouping = ?35, performance = ?36, cover = ?37, cover_id = ?38,
                   updated_at = ?39
                 WHERE id = ?40",
                rusqlite::params_from_iter(
                    values
                        .iter()
                        .copied()
                        .chain(std::iter::once(&id as &dyn rusqlite::ToSql)),
                ),
            )?;
            id
        } else {
            self.conn.execute(
                "INSERT INTO tracks (file_id, url, cue_start, cue_end, fingerprint, title, title_sort,
                   title_search, album_id, artist_id, work_id, track_number, disc, disc_subtitle, year,
                   duration, format, bitrate, vbr, sample_rate, sample_size, channels, lossless,
                   big_endian, block_align, audio_offset, audio_size, bpm, tag_version, replay_gain,
                   replay_peak, mbid, lyrics, subtitle, grouping, performance, cover, cover_id,
                   updated_at, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33,
                   ?34, ?35, ?36, ?37, ?38, ?39, ?39)",
                values,
            )?;
            self.conn.last_insert_rowid()
        };

        // --- relations ---
        self.conn
            .execute("DELETE FROM track_artists WHERE track_id = ?", [track_id])?;
        {
            let mut st_t = self.conn.prepare_cached(
                "INSERT OR IGNORE INTO track_artists (track_id, artist_id, role, position) VALUES (?, ?, ?, ?)",
            )?;
            let mut st_a = self.conn.prepare_cached(
                "INSERT OR IGNORE INTO album_artists (album_id, artist_id, role) VALUES (?, ?, ?)",
            )?;
            let mut position = 0i64;
            for (role, ids) in &artists {
                let rid = type_to_role(role).unwrap_or(1);
                for aid in ids {
                    st_t.execute(params![track_id, aid, rid, position])?;
                    st_a.execute(params![album_id, aid, rid])?;
                    position += 1;
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
            .execute("DELETE FROM track_genres WHERE track_id = ?", [track_id])?;
        let genre_names = if genre_names.is_empty() {
            vec![cfg.no_genre_string.clone()]
        } else {
            genre_names
        };
        for g in &genre_names {
            let gid = self.genre_id(g.trim())?;
            self.conn.execute(
                "INSERT OR IGNORE INTO track_genres (track_id, genre_id) VALUES (?, ?)",
                params![track_id, gid],
            )?;
        }
        // comments
        self.conn
            .execute("DELETE FROM track_comments WHERE track_id = ?", [track_id])?;
        for (i, c) in comments.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO track_comments (track_id, position, text) VALUES (?, ?, ?)",
                params![track_id, i as i64, c],
            )?;
        }
        // listening history and analysis that belong to this audio
        self.link_stats(
            &input.url,
            mbid.as_deref(),
            fingerprint.as_deref(),
            input.timestamp,
            rating,
        )?;
        if let Some(fp) = &fingerprint {
            self.adopt_legacy_analysis(&input.url, fp, input.timestamp, input.filesize)?;
        }

        // compilation detection (after the track's artists are known)
        if is_compilation.is_none() {
            self.merge_single_va_album(album_id)?;
        }
        self.touched_albums.insert(album_id);

        // change bookkeeping
        if existing.is_some() {
            self.rescan_artists(&old_artists)?;
            if let Some(a) = old_album {
                if a != album_id {
                    self.rescan_album(a)?;
                    self.touched_albums.insert(a);
                    self.rebuild_album_artists(a)?;
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
                "SELECT id FROM works WHERE title_search = ? AND composer_id = ?",
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
            "INSERT INTO works (composer_id, title, title_sort, title_search) VALUES (?, ?, ?, ?)",
            params![composer, title, sort, search],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Find this track's listening history — by URL, else (for a moved or renamed file) by
    /// MusicBrainz id or fingerprint of a track no longer in the library — or start it.
    fn link_stats(
        &mut self,
        u: &str,
        mbid: Option<&str>,
        fingerprint: Option<&str>,
        timestamp: i64,
        rating: Option<i64>,
    ) -> rusqlite::Result<()> {
        let mut id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM track_stats WHERE url = ?", [u], |r| {
                r.get(0)
            })
            .optional()?;
        if id.is_none() {
            // only history whose track is gone can move; two copies keep their own
            const ORPHAN: &str =
                "NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.url = track_stats.url)";
            if let Some(m) = mbid {
                id = self
                    .conn
                    .query_row(
                        &format!("SELECT id FROM track_stats WHERE mbid = ? AND {ORPHAN} LIMIT 1"),
                        [m],
                        |r| r.get(0),
                    )
                    .optional()?;
            }
            if let (None, Some(fp)) = (id, fingerprint) {
                id = self
                    .conn
                    .query_row(
                        &format!(
                            "SELECT id FROM track_stats WHERE fingerprint = ? AND {ORPHAN} LIMIT 1"
                        ),
                        [fp],
                        |r| r.get(0),
                    )
                    .optional()?;
            }
        }
        match id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE track_stats SET url = ?, mbid = COALESCE(?, mbid),
                       fingerprint = COALESCE(?, fingerprint),
                       rating = COALESCE(rating, ?) WHERE id = ?",
                    params![u, mbid, fingerprint, rating, id],
                )?;
            }
            None => {
                let added = if self.first_scan { timestamp } else { now() };
                self.conn.execute(
                    "INSERT INTO track_stats (url, mbid, fingerprint, added_at, rating) VALUES (?, ?, ?, ?, ?)",
                    params![u, mbid, fingerprint, added, rating],
                )?;
            }
        }
        Ok(())
    }

    /// Analysis converted from a Lyrion-schema database is keyed by URL; file it under the
    /// track's fingerprint when the file is unchanged since.
    fn adopt_legacy_analysis(
        &mut self,
        u: &str,
        fingerprint: &str,
        mtime: i64,
        size: i64,
    ) -> rusqlite::Result<()> {
        if !self.legacy_analysis {
            return Ok(());
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO track_analysis (fingerprint, version, features, loudness, peak, analyzed_at)
             SELECT ?, version, features, loudness, peak, COALESCE(analyzed, 0) FROM legacy_analysis
             WHERE urlmd5 = ? AND timestamp IS ? AND filesize IS ? AND version IS NOT NULL",
            params![fingerprint, url::md5_hex(u), mtime, size],
        )?;
        Ok(())
    }

    fn genre_id(&mut self, name: &str) -> rusqlite::Result<i64> {
        let search = self.rules.search_key(name);
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM genres WHERE name_search = ?",
                [&search],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO genres (name, name_sort, name_search) VALUES (?, ?, ?)",
            params![name, self.rules.sort_key(name), search],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `_mergeAndCreateContributors` → [(ROLE, [ids])] in `CONTRIBUTOR_ROLES` order.
    fn merge_and_create_artists(
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
            let ids = self.add_artists(&vals, sort.as_deref(), mbid.as_deref())?;
            if !ids.is_empty() {
                out.push((role.to_string(), ids));
            }
        }
        let has = |r: &str| out.iter().any(|(n, _)| n == r);
        if !has("ALBUMARTIST") && !has("ARTIST") {
            let name = self.cfg.no_artist_string.clone();
            let ids = self.add_artists(&[name], None, None)?;
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
    fn add_artists(
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
                    .query_row("SELECT id, mbid FROM artists WHERE mbid = ?", [m], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?;
                if found.is_none() {
                    found = self
                        .conn
                        .query_row(
                            "SELECT id, mbid FROM artists WHERE name = ? AND mbid IS NULL",
                            [name],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                }
            } else {
                found = self
                    .conn
                    .query_row(
                        "SELECT id, mbid FROM artists WHERE name = ? LIMIT 1",
                        [name],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
            }
            let id = match found {
                None => {
                    self.conn.execute(
                        "INSERT INTO artists (name, name_sort, name_search, mbid) VALUES (?, ?, ?, ?)",
                        params![name, sortkey, search, mbid],
                    )?;
                    self.conn.last_insert_rowid()
                }
                Some((id, old_mbid)) => {
                    if search != text::transliterate_latin1(&sortkey) {
                        self.conn.execute(
                            "UPDATE artists SET name_sort = ? WHERE id = ?",
                            params![sortkey, id],
                        )?;
                    }
                    if let Some(m) = &mbid {
                        if old_mbid.as_deref() != Some(m.as_str()) {
                            self.conn.execute(
                                "UPDATE artists SET mbid = ? WHERE id = ?",
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
                "UPDATE albums SET compilation = 1, artist_id = ? WHERE id = ?",
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
        let mut row: AlbumRow = AlbumRow::default();

        if let Some(old) = a.update_from {
            if let Some(old_row) = load_album(self.conn, old) {
                let old_title = old_row.title.clone().unwrap_or_default();
                let new_title = title.clone().unwrap_or_default();
                let different = text::transliterate_latin1(&new_title)
                    != text::transliterate_latin1(&old_title);
                if !new_title.is_empty()
                    && !old_title.is_empty()
                    && ((old_title == no_album && new_title != no_album) || different)
                {
                    if different {
                        exclude_id = Some(old);
                    }
                    create = true;
                } else if !new_title.is_empty() && old_title != no_album {
                    row = old_row;
                    create = false;
                }
            }
        }

        // No title → "No Album" singleton
        if create && title.as_deref().map(|t| t.is_empty()).unwrap_or(true) {
            if self.no_album_id.is_none() {
                self.no_album_id = self
                    .conn
                    .query_row(
                        "SELECT id FROM albums WHERE title = ? LIMIT 1",
                        [&no_album],
                        |r| r.get(0),
                    )
                    .optional()?;
            }
            match self.no_album_id {
                None => {
                    let va = self.va_id()?;
                    self.conn.execute(
                        "INSERT INTO albums (title, title_sort, title_search, compilation, year, artist_id, release_type)
                         VALUES (?, ?, ?, 0, 0, ?, 'ALBUM')",
                        params![no_album, self.rules.sort_key(&no_album), self.rules.search_key(&no_album), va],
                    )?;
                    let id = self.conn.last_insert_rowid();
                    self.no_album_id = Some(id);
                    return Ok(id);
                }
                Some(id) => return Ok(id),
            }
        }
        let title_s = title.take().unwrap_or_default();
        let basename = a.basename.clone();

        if create {
            let check_disc = !self.cfg.group_discs && (a.disc.is_some() || a.mbid.is_some());
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
                if let Some(b) = &a.mbid {
                    search.push("albums.mbid = ?".into());
                    values.push(b.clone().into());
                }
                if let Some(x) = exclude_id {
                    search.push("albums.id != ?".into());
                    values.push(x.into());
                }
                let mut check_artist = false;
                if check_disc {
                    if let Some(d) = a.disc {
                        search.push("albums.disc = ?".into());
                        values.push(d.into());
                    }
                    check_artist = true;
                } else if let Some(dc) = a.disc_count {
                    search.push("albums.disc_count = ?".into());
                    values.push(dc.into());
                    check_artist = true;
                } else if a.disc.is_some() && a.disc_count.is_none() {
                    search.push("albums.disc IS NOT NULL".into());
                    check_artist = true;
                }
                if check_artist {
                    if let Some(cid) = a.artist_id {
                        let c = if a.is_compilation == Some(1) && !a.has_album_artist {
                            self.va_id()?
                        } else {
                            cid
                        };
                        search.push("albums.artist_id = ?".into());
                        values.push(c.into());
                    }
                }
                if a.disc.is_none() {
                    search.push("albums.disc IS NULL".into());
                    if a.disc_count.is_none() {
                        search.push("albums.disc_count IS NULL".into());
                    }
                }
                if let Some(c) = a.is_compilation {
                    search.push("albums.compilation = ?".into());
                    values.push(c.into());
                }
                if (check_disc && a.mbid.is_none() && a.disc.is_none() && a.disc_count.is_none())
                    || (!check_disc && a.disc.is_none() && a.disc_count.is_none())
                {
                    search.push("tracks.url LIKE ? ESCAPE '\\'".into());
                    values.push(format!("{}%", like_escape(&basename)).into());
                    join = true;
                }
                let sql = format!(
                    "SELECT {ALBUM_COLS} FROM albums {} WHERE {} LIMIT 1",
                    if join {
                        "JOIN tracks ON (albums.id = tracks.album_id)"
                    } else {
                        ""
                    },
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
                    if let Some(tn) = a.track_number {
                        let m: Option<String> = self
                            .conn
                            .query_row(
                                "SELECT url FROM tracks WHERE album_id = ? AND track_number = ? LIMIT 1",
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
                Some(f) => row = f,
                None => {
                    row = AlbumRow {
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
        row.title_sort = Some(self.rules.sort_key(&sort_src));
        row.title_search = Some(self.rules.search_key(&title_s));
        let existing_comp = row.compilation;
        row.compilation = a.is_compilation;
        let release = a
            .release_types
            .iter()
            .flat_map(|r| self.rules.split_tag(r))
            .find(|r| !r.eq_ignore_ascii_case("compilation"))
            .unwrap_or_else(|| "album".into());
        row.release_type = Some(self.rules.search_key(&release));
        let va = self.va_id()?;
        if a.is_compilation == Some(1) && !a.has_album_artist {
            row.artist_id = Some(va);
        } else if let Some(c) = a.artist_id {
            row.artist_id = Some(c);
            if c == va {
                row.compilation = Some(1);
            }
        }
        row.mbid = a.mbid.clone();
        row.replay_gain = a.rg_gain.as_deref().and_then(text::clean_gain);
        row.replay_peak = a.rg_peak.as_deref().and_then(text::clean_gain);
        let mut disc_count = a.disc_count;
        if disc_count.is_some() || self.cfg.group_discs {
            let m = a
                .disc
                .unwrap_or(0)
                .max(a.disc_count.unwrap_or(0))
                .max(row.disc_count.unwrap_or(0));
            disc_count = if m == 0 { None } else { Some(m) };
        }
        row.disc = a.disc;
        row.disc_count = disc_count;
        row.year = Some(a.year);
        if !create {
            row.title = Some(title_s.clone());
        }
        if let Some(c) = &a.cover_id {
            row.artwork = Some(c.clone());
        }
        if a.is_compilation.is_none() {
            // keep the evaluated flag; re-evaluated after the track is stored
            row.compilation = match (row.id, existing_comp) {
                (Some(_), Some(1)) => Some(1),
                (Some(_), c) => c.or(Some(0)),
                (None, _) => Some(0),
            };
            if row.compilation == Some(1) {
                row.artist_id = Some(va);
            }
        }

        let values = params![
            row.title.clone().unwrap_or_default(),
            row.title_sort,
            row.title_search,
            row.compilation,
            row.year.unwrap_or(0),
            row.artwork,
            row.disc,
            row.disc_count,
            row.replay_gain,
            row.replay_peak,
            row.mbid,
            row.artist_id,
            row.release_type.clone().unwrap_or_else(|| "ALBUM".into()),
        ];
        let id = match row.id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE albums SET title = ?1, title_sort = ?2, title_search = ?3, compilation = ?4, year = ?5,
                       artwork = ?6, disc = ?7, disc_count = ?8, replay_gain = ?9, replay_peak = ?10, mbid = ?11,
                       artist_id = ?12, release_type = ?13 WHERE id = ?14",
                    rusqlite::params_from_iter(
                        values
                            .iter()
                            .copied()
                            .chain(std::iter::once(&id as &dyn rusqlite::ToSql)),
                    ),
                )?;
                id
            }
            None => {
                self.conn.execute(
                    "INSERT INTO albums (title, title_sort, title_search, compilation, year, artwork, disc,
                       disc_count, replay_gain, replay_peak, mbid, artist_id, release_type)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    values,
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

    // -----------------------------------------------------------------------------------------
    // post processing
    // -----------------------------------------------------------------------------------------

    /// Album artwork (embedded first, then disc/track number), search text, orphans.
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
                "SELECT cover_id FROM tracks WHERE album_id = ? AND cover_id IS NOT NULL
                 ORDER BY (CASE WHEN cover GLOB '[0-9]*' THEN 0 ELSE 1 END), disc, track_number LIMIT 1",
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
        // orphans
        let va = self.va_id()?;
        self.conn.execute_batch(
            "DELETE FROM albums WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.album_id = albums.id);
             DELETE FROM genres WHERE NOT EXISTS (SELECT 1 FROM track_genres WHERE track_genres.genre_id = genres.id);
             DELETE FROM works WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.work_id = works.id);",
        )?;
        self.conn.execute(
            "DELETE FROM artists WHERE id != ?
               AND NOT EXISTS (SELECT 1 FROM track_artists WHERE track_artists.artist_id = artists.id)
               AND NOT EXISTS (SELECT 1 FROM albums WHERE albums.artist_id = artists.id)",
            [va],
        )?;
        if let Some(id) = self.no_album_id {
            if load_album(self.conn, id).is_none() {
                self.no_album_id = None;
            }
        }
        self.last_album = None;
        self.step_done(2, None);
        // search text: the words multi-word search looks in (title, album, artists)
        const ALBUM_TEXT: &str = "UPDATE albums SET search_text = title_search || ' ' || COALESCE((SELECT GROUP_CONCAT(a.name_search, ' ')
                FROM album_artists aa JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id = albums.id), '')";
        const TRACK_TEXT: &str = "UPDATE tracks SET search_text = title_search || ' ' || COALESCE((SELECT title_search FROM albums WHERE albums.id = tracks.album_id), '')
                || ' ' || COALESCE((SELECT GROUP_CONCAT(a.name_search, ' ') FROM track_artists ta JOIN artists a ON a.id = ta.artist_id
                WHERE ta.track_id = tracks.id), '')";
        if all {
            self.conn.execute(ALBUM_TEXT, [])?;
            self.conn.execute(TRACK_TEXT, [])?;
        } else {
            let mut sa = self
                .conn
                .prepare_cached(&format!("{} WHERE id = ?", ALBUM_TEXT))?;
            let mut stt = self
                .conn
                .prepare_cached(&format!("{} WHERE album_id = ?", TRACK_TEXT))?;
            for a in &albums {
                sa.execute([a])?;
                stt.execute([a])?;
            }
            self.conn
                .execute(&format!("{} WHERE search_text IS NULL", TRACK_TEXT), [])?;
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

    /// Remove local files that are no longer below a configured media dir (or the playlist
    /// dir).  LMS wipes when a media dir is removed; we clean up incrementally instead.
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
            let mut st = self
                .conn
                .prepare("SELECT url FROM files WHERE kind != 'dir'")?;
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
            // may already be gone (split files removed together with their cue sheet)
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
    disc_count: Option<i64>,
    mbid: Option<String>,
    release_types: Vec<String>,
    rg_gain: Option<String>,
    rg_peak: Option<String>,
    is_compilation: Option<i64>,
    artist_id: Option<i64>,
    has_album_artist: bool,
    basename: String,
    track_number: Option<i64>,
    year: i64,
    cover_id: Option<String>,
    update_from: Option<i64>,
}

fn album_artists_differ(c: &Connection, album: i64) -> rusqlite::Result<bool> {
    let mut st = c.prepare_cached(
        "SELECT artist_id, track_id FROM track_artists
         WHERE track_id IN (SELECT id FROM tracks WHERE album_id = ?) AND role = 1
         ORDER BY artist_id, track_id",
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

/// `variousArtistsObject`: look up by search key, create if missing, rename if the string
/// changed.
pub fn various_artists_id(
    c: &Connection,
    cfg: &LibraryConfig,
    rules: &TextRules,
) -> rusqlite::Result<i64> {
    let name = &cfg.various_artists_string;
    let search = rules.search_key(name);
    let row: Option<(i64, String)> = c
        .query_row(
            "SELECT id, name FROM artists WHERE name_search = ? LIMIT 1",
            [&search],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((id, n)) => {
            if &n != name {
                c.execute(
                    "UPDATE artists SET name = ?, name_sort = ? WHERE id = ?",
                    params![name, rules.sort_key(name), id],
                )?;
            }
            Ok(id)
        }
        None => {
            c.execute(
                "INSERT INTO artists (name, name_sort, name_search) VALUES (?, ?, ?)",
                params![name, rules.sort_key(name), search],
            )?;
            Ok(c.last_insert_rowid())
        }
    }
}
