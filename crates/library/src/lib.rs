//! `rms-library`: the music library of RMS (Rust port of Lyrion Music Server).
//!
//! * SQLite schema compatible with LMS schema version 26 (`tracks`, `albums`, `contributors`, ...)
//! * scanner (media dirs + playlist dir, incremental by mtime/size, cue sheets, playlists)
//! * LMS CLI library queries (`albums`, `artists`, `titles`, `songinfo`, `musicfolder`, ...)
//!   producing result hashes with the exact LMS field names and emission order
//! * artwork extraction / resizing with an on-disk cache
//! * contributor portraits (artist pictures from the `artfolder` or the music folders)

/// Lazily compiled static regex.
#[macro_export]
#[doc(hidden)]
macro_rules! re {
    ($s:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        // compiled once, wherever the macro is used
        #[allow(clippy::regex_creation_in_loops)]
        let re = RE.get_or_init(|| regex::Regex::new($s).unwrap());
        re
    }};
}

pub mod artwork;
pub mod catalog;
pub mod config;
pub mod cue;
pub mod db;
pub mod discover;
pub mod loudness;
pub mod mix;
pub mod musicbrainz;
pub mod playlist_files;
pub mod portraits;
pub mod print;
pub mod queries;
pub mod remote;
pub mod rhythm;
pub mod scanner;
pub mod songdata;
pub mod sonic;
pub mod tags;
pub mod text;
pub mod track;
pub mod types;
pub mod url;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use rusqlite::{params, OptionalExtension};

pub use config::LibraryConfig;
pub use track::Track;

/// Ordered result hash (serde_json with `preserve_order`).
pub type Row = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanMode {
    /// incremental: new + changed + deleted (LMS "rescan")
    Full,
    /// clear the library and rescan everything (LMS "wipecache")
    Wipe,
    /// only the playlist folder (LMS "rescan playlists")
    Playlists,
    /// a single directory (LMS "rescan full <dir>")
    Dir(PathBuf),
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScanSummary {
    pub files_found: usize,
    pub new: usize,
    pub changed: usize,
    pub deleted: usize,
    pub errors: usize,
    pub aborted: bool,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScanProgress {
    pub running: bool,
    /// current step name, LMS style (`<path>|directory_new`, `post_processing`, ...)
    pub step: String,
    pub done: usize,
    pub total: usize,
    /// last item processed
    pub info: Option<String>,
    pub steps: Vec<String>,
    /// epoch of the last scan that completed (`lastRescanTime`)
    pub last_scan: Option<i64>,
    /// seconds the current/last scan took
    pub total_time: f64,
    pub start_time: Option<i64>,
    /// set when the last scan was aborted
    pub last_scan_failed: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Totals {
    pub albums: u64,
    pub artists: u64,
    pub genres: u64,
    pub songs: u64,
    pub duration: f64,
    pub playlists: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibQueryName {
    Albums,
    Artists,
    Genres,
    Years,
    /// `titles`, `songs`, `tracks`
    Titles,
    Search,
    /// `musicfolder` / `mediafolder`
    MusicFolder,
    Playlists,
    /// `playlists tracks` / `playlisttracks`
    PlaylistTracks,
    SongInfo,
    Roles,
    Works,
    Libraries,
}

impl LibQueryName {
    /// Map an LMS verb (`albums`, `songs`, `playlists tracks`, ...) to a query name.
    pub fn from_verbs(verbs: &[&str]) -> Option<Self> {
        Some(match verbs {
            ["albums", ..] => Self::Albums,
            ["artists", ..] => Self::Artists,
            ["genres", ..] => Self::Genres,
            ["years", ..] => Self::Years,
            ["titles", ..] | ["songs", ..] | ["tracks", ..] => Self::Titles,
            ["search", ..] => Self::Search,
            ["musicfolder", ..] | ["mediafolder", ..] => Self::MusicFolder,
            ["playlists", "tracks", ..] | ["playlisttracks", ..] => Self::PlaylistTracks,
            ["playlists", ..] => Self::Playlists,
            ["songinfo", ..] => Self::SongInfo,
            ["roles", ..] => Self::Roles,
            ["works", ..] => Self::Works,
            ["libraries", ..] => Self::Libraries,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct LibQuery {
    pub name: LibQueryName,
    pub start: Option<usize>,
    pub quantity: Option<usize>,
    pub params: IndexMap<String, String>,
}

impl LibQuery {
    pub fn new(name: LibQueryName, start: usize, quantity: usize) -> Self {
        LibQuery {
            name,
            start: Some(start),
            quantity: Some(quantity),
            params: IndexMap::new(),
        }
    }

    pub fn param(mut self, k: &str, v: &str) -> Self {
        self.params.insert(k.to_string(), v.to_string());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct TrackFilter {
    pub track_ids: Vec<i64>,
    pub album_id: Option<String>,
    pub artist_id: Option<i64>,
    pub genre_id: Option<String>,
    pub year: Option<i64>,
    pub playlist_id: Option<i64>,
    pub folder_id: Option<i64>,
    pub search: Option<String>,
    pub role_id: Option<String>,
    pub sort: Option<String>,
    pub work_id: Option<i64>,
    pub release_type: Option<String>,
}

/// `playlists edit` operations.
#[derive(Debug, Clone)]
pub enum PlaylistEditOp {
    /// move the item at `index` one up
    Up(usize),
    Down(usize),
    Delete(usize),
    /// append a URL (or insert at `index` when given)
    Add {
        url: String,
        index: Option<usize>,
    },
    Move {
        from: usize,
        to: usize,
    },
}

pub(crate) struct Inner {
    pub pool: db::DbPool,
    pub cfg: RwLock<LibraryConfig>,
    pub cache_dir: PathBuf,
    pub scan_lock: Mutex<()>,
    pub abort: AtomicBool,
    pub scanning: AtomicBool,
    pub progress: Arc<Mutex<ScanProgress>>,
    /// What every analysed track sounds like, kept between mixes (`None` until first used,
    /// and again after a scan changed the tracks).
    pub sound: RwLock<Option<Arc<sonic::Index>>>,
}

#[derive(Clone)]
pub struct Library {
    pub(crate) inner: Arc<Inner>,
}

impl Library {
    /// Open (and create if needed) the library database.
    pub fn open(db_path: &Path, cache_dir: &Path) -> anyhow::Result<Library> {
        let pool = db::open_pool(db_path)?;
        std::fs::create_dir_all(cache_dir)?;
        let lib = Library {
            inner: Arc::new(Inner {
                pool,
                cfg: RwLock::new(LibraryConfig::default()),
                cache_dir: cache_dir.to_path_buf(),
                scan_lock: Mutex::new(()),
                abort: AtomicBool::new(false),
                scanning: AtomicBool::new(false),
                progress: Arc::new(Mutex::new(ScanProgress::default())),
                sound: RwLock::new(None),
            }),
        };
        {
            let c = lib.conn()?;
            let last = db::get_meta(&c, "last_scan")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|v| *v > 0);
            lib.inner.progress.lock().last_scan = last;
        }
        Ok(lib)
    }

    pub(crate) fn conn(&self) -> anyhow::Result<db::PooledConn> {
        Ok(self.inner.pool.get()?)
    }

    pub fn config(&self) -> LibraryConfig {
        self.inner.cfg.read().clone()
    }

    pub fn set_config(&self, cfg: LibraryConfig) {
        // keep the Various Artists contributor name in sync
        let rules = text::TextRules::new(&cfg.ignored_articles, &cfg.split_list);
        if let Ok(c) = self.conn() {
            let _ = scanner::various_artists_id(&c, &cfg, &rules);
        }
        *self.inner.cfg.write() = cfg;
    }

    pub fn cache_dir(&self) -> &Path {
        &self.inner.cache_dir
    }

    // ---------------------------------------------------------------------------------------
    // scanning
    // ---------------------------------------------------------------------------------------

    /// Run a scan (blocking). Only one scan runs at a time; a concurrent call waits.
    pub fn scan(&self, mode: ScanMode) -> anyhow::Result<ScanSummary> {
        let _guard = self.inner.scan_lock.lock();
        self.inner.abort.store(false, Ordering::SeqCst);
        self.inner.scanning.store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        {
            let mut p = self.inner.progress.lock();
            let last = p.last_scan;
            *p = ScanProgress {
                running: true,
                last_scan: last,
                start_time: Some(scanner::now()),
                ..Default::default()
            };
        }
        let res = self.scan_inner(&mode);
        // track ids may have changed: the next mix reloads the sound index
        *self.inner.sound.write() = None;
        let elapsed = started.elapsed().as_secs_f64();
        self.inner.scanning.store(false, Ordering::SeqCst);
        let aborted = self.inner.abort.load(Ordering::SeqCst);
        {
            let mut p = self.inner.progress.lock();
            p.running = false;
            p.total_time = elapsed;
            if aborted {
                p.last_scan_failed = Some("SCAN_ABORTED".into());
            }
        }
        let mut summary = res?;
        summary.aborted = aborted;
        summary.duration_secs = elapsed;
        Ok(summary)
    }

    fn scan_inner(&self, mode: &ScanMode) -> anyhow::Result<ScanSummary> {
        let cfg = self.config();
        let mut conn = self.conn()?;
        // a dedicated non-pooled-like usage: we hold this connection for the whole scan
        let c: &mut rusqlite::Connection = &mut conn;
        if *mode == ScanMode::Wipe {
            c.execute_batch(db::CLEAR)?;
            let _ = std::fs::remove_dir_all(self.inner.cache_dir.join("artwork"));
        }
        let progress = self.inner.progress.clone();
        let mut sc = scanner::Scanner::new(c, &cfg, &self.inner.abort, progress);
        sc.va_id()?;
        let wipe = *mode == ScanMode::Wipe;
        match mode {
            ScanMode::Full | ScanMode::Wipe => {
                for d in &cfg.media_dirs {
                    if sc_aborted(&self.inner.abort) {
                        break;
                    }
                    if d.is_dir() {
                        sc.scan_audio_root(d, wipe, true)?;
                    }
                }
                if let Some(pd) = &cfg.playlist_dir {
                    if pd.is_dir() && !sc_aborted(&self.inner.abort) {
                        sc.scan_playlist_dir(pd)?;
                    }
                }
            }
            ScanMode::Playlists => {
                if let Some(pd) = &cfg.playlist_dir {
                    if pd.is_dir() {
                        sc.scan_playlist_dir(pd)?;
                    }
                }
            }
            ScanMode::Dir(d) => {
                let d = d.clone();
                if d.is_dir() {
                    sc.scan_audio_root(&d, false, true)?;
                } else if d.is_file() {
                    // a single file
                    let u = url::file_url_from_path(&d);
                    sc.begin()?;
                    match scanner::file_row(sc.conn, &u)? {
                        Some(_) => sc.changed(&u)?,
                        None => {
                            sc.new_url(&u, false)?;
                        }
                    }
                    sc.commit()?;
                }
            }
        }
        if matches!(mode, ScanMode::Full) && !sc_aborted(&self.inner.abort) {
            let roots: Vec<PathBuf> = cfg
                .media_dirs
                .iter()
                .filter(|d| d.is_dir())
                .cloned()
                .collect();
            // only clean up when every configured media dir is reachable (avoid wiping offline volumes)
            if roots.len() == cfg.media_dirs.len() {
                sc.remove_outside_roots(&roots, cfg.playlist_dir.as_deref())?;
            }
        }
        let changes = sc.changes;
        if changes > 0 || wipe {
            sc.post_process(wipe, true)?;
        }
        if matches!(mode, ScanMode::Full | ScanMode::Wipe)
            && cfg.contributor_pictures
            && !sc_aborted(&self.inner.abort)
        {
            sc.scan_contributor_pictures(wipe, Some(&self.artist_image_dir()))?;
        }
        let summary = sc.summary.clone();
        drop(sc);
        if matches!(mode, ScanMode::Full | ScanMode::Wipe)
            && !sc_aborted(&self.inner.abort)
            && db::table_exists(c, "legacy_analysis")?
        {
            // every track had its chance to claim its old analysis
            c.execute_batch("DROP TABLE legacy_analysis")?;
        }
        if changes > 0 || wipe || self.inner.progress.lock().last_scan.is_none() {
            let t = scanner::now();
            db::set_meta(c, "last_scan", &t.to_string())?;
            self.inner.progress.lock().last_scan = Some(t);
        }
        Ok(summary)
    }

    pub fn abort_scan(&self) {
        self.inner.abort.store(true, Ordering::SeqCst);
    }

    pub fn progress(&self) -> ScanProgress {
        self.inner.progress.lock().clone()
    }

    pub fn is_scanning(&self) -> bool {
        self.inner.scanning.load(Ordering::SeqCst)
    }

    /// `lastRescanTime`
    pub fn last_scan_time(&self) -> Option<i64> {
        self.inner.progress.lock().last_scan
    }

    pub fn totals(&self) -> Totals {
        queries::totals(self).unwrap_or_default()
    }

    // ---------------------------------------------------------------------------------------
    // queries
    // ---------------------------------------------------------------------------------------

    pub fn query(&self, q: &LibQuery) -> anyhow::Result<Row> {
        queries::run(self, q)
    }

    pub fn track(&self, id: i64) -> Option<Track> {
        let c = self.conn().ok()?;
        track::load_by_id(&c, id)
    }

    pub fn track_by_url(&self, u: &str) -> Option<Track> {
        let c = self.conn().ok()?;
        track::load_by_url(&c, u)
    }

    /// Render one track for the given tag letters (LMS `_getTagDataForTracks` + `_songDataFromHash`,
    /// i.e. the representation used by `status` and `titles`).
    pub fn track_row(&self, t: &Track, tags: &str) -> Row {
        let cfg = self.config();
        match self.conn() {
            Ok(c) => songdata::sql_rows(&c, &cfg, &[t.id], tags)
                .ok()
                .and_then(|mut v| v.pop())
                .unwrap_or_else(|| songdata::minimal_row(t)),
            Err(_) => songdata::minimal_row(t),
        }
    }

    /// Like [`track_row`] but using the object-path semantics of `_songData` (used by `songinfo`
    /// and `playlists tracks`).
    pub fn track_row_object(&self, t: &Track, tags: &str) -> Row {
        let cfg = self.config();
        match self.conn() {
            Ok(c) => songdata::object_row(&c, &cfg, t, tags),
            Err(_) => songdata::minimal_row(t),
        }
    }

    /// Batch version of [`track_row`] preserving the order of `ids`.
    pub fn track_rows(&self, ids: &[i64], tags: &str) -> Vec<Row> {
        let cfg = self.config();
        match self.conn() {
            Ok(c) => songdata::sql_rows(&c, &cfg, ids, tags).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub fn tracks_for(&self, f: &TrackFilter) -> anyhow::Result<Vec<Track>> {
        queries::tracks_for(self, f)
    }

    // ---------------------------------------------------------------------------------------
    // saved playlists
    // ---------------------------------------------------------------------------------------

    fn playlist_path(&self, name: &str) -> anyhow::Result<PathBuf> {
        let cfg = self.config();
        let dir = cfg
            .playlist_dir
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no playlist directory configured"))?;
        Ok(dir.join(format!("{}.m3u", playlist_files::cleanup_filename(name))))
    }

    fn write_playlist_file(
        &self,
        c: &rusqlite::Connection,
        path: &Path,
        urls: &[String],
    ) -> anyhow::Result<()> {
        let mut entries = Vec::new();
        for u in urls {
            let t = track::load_by_url(c, u);
            entries.push((
                u.clone(),
                t.as_ref().map(|t| t.title.clone()),
                t.and_then(|t| t.secs),
            ));
        }
        playlist_files::write_m3u(path, &entries, true)?;
        Ok(())
    }

    /// Save (create or replace) a playlist; written as `<playlistdir>/<name>.m3u`.
    pub fn save_playlist(&self, name: &str, urls: &[String]) -> anyhow::Result<i64> {
        let path = self.playlist_path(name)?;
        let c = self.conn()?;
        self.write_playlist_file(&c, &path, urls)?;
        let u = url::file_url_from_path(&path);
        let id = self.store_saved_playlist(&c, &u, &path, name, None)?;
        set_playlist_tracks(&c, id, urls)?;
        Ok(id)
    }

    /// The `files` and `playlists` rows of a saved playlist file; `id` keeps an existing
    /// playlist's id (rename).
    fn store_saved_playlist(
        &self,
        c: &rusqlite::Connection,
        u: &str,
        path: &Path,
        name: &str,
        id: Option<i64>,
    ) -> anyhow::Result<i64> {
        let cfg = self.config();
        let rules = text::TextRules::new(&cfg.ignored_articles, &cfg.split_list);
        let (mt, sz, _) = scanner::file_stat(path).unwrap_or((0, 0, false));
        let file_id = scanner::upsert_file(c, u, "playlist", None, mt, sz)?;
        let values = params![
            file_id,
            u,
            name,
            rules.sort_key(name),
            rules.search_key(name)
        ];
        let id = match id {
            Some(id) => {
                c.execute(
                    "UPDATE playlists SET file_id = ?1, url = ?2, kind = 'saved', name = ?3, name_sort = ?4,
                       name_search = ?5 WHERE id = ?6",
                    params![file_id, u, name, rules.sort_key(name), rules.search_key(name), id],
                )?;
                id
            }
            None => c.query_row(
                "INSERT INTO playlists (file_id, url, kind, name, name_sort, name_search)
                 VALUES (?1, ?2, 'saved', ?3, ?4, ?5)
                 ON CONFLICT (url) DO UPDATE SET file_id = excluded.file_id, kind = 'saved',
                   name = excluded.name, name_sort = excluded.name_sort, name_search = excluded.name_search
                 RETURNING id",
                values,
                |r| r.get(0),
            )?,
        };
        Ok(id)
    }

    pub fn delete_playlist(&self, id: i64) -> anyhow::Result<()> {
        let c = self.conn()?;
        let row: Option<(String, Option<i64>)> = c
            .query_row(
                "SELECT url, file_id FROM playlists WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((u, file_id)) = row else {
            anyhow::bail!("playlist {} not found", id)
        };
        if let Some(p) = url::path_from_file_url(&u) {
            let cfg = self.config();
            if cfg
                .playlist_dir
                .as_ref()
                .map(|d| p.starts_with(d))
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&p);
            }
        }
        // the playlist and its items go with its file
        match file_id {
            Some(f) => c.execute("DELETE FROM files WHERE id = ?", [f])?,
            None => c.execute("DELETE FROM playlists WHERE id = ?", [id])?,
        };
        Ok(())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> anyhow::Result<()> {
        let (_, old_url, urls) = self
            .playlist_by_id(id)
            .ok_or_else(|| anyhow::anyhow!("playlist {} not found", id))?;
        let new_path = self.playlist_path(name)?;
        let c = self.conn()?;
        if let Some(old) = url::path_from_file_url(&old_url) {
            if old != new_path {
                let _ = std::fs::remove_file(&old);
            }
        }
        self.write_playlist_file(&c, &new_path, &urls)?;
        let nu = url::file_url_from_path(&new_path);
        let old_file: Option<i64> = c
            .query_row("SELECT file_id FROM playlists WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        if nu != old_url {
            // replaces another playlist saved under the new name
            c.execute(
                "DELETE FROM playlists WHERE url = ? AND id != ?",
                params![nu, id],
            )?;
        }
        self.store_saved_playlist(&c, &nu, &new_path, name, Some(id))?;
        if nu != old_url {
            if let Some(f) = old_file {
                c.execute("DELETE FROM files WHERE id = ?", [f])?;
            }
        }
        Ok(())
    }

    /// (name, url, track urls) of a playlist.
    pub fn playlist_by_id(&self, id: i64) -> Option<(String, String, Vec<String>)> {
        let c = self.conn().ok()?;
        let (name, u): (String, String) = c
            .query_row("SELECT name, url FROM playlists WHERE id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok()?;
        Some((name, u, playlist_urls(&c, id).ok()?))
    }

    /// Id of the playlist stored at `u`.
    pub fn playlist_id_by_url(&self, u: &str) -> Option<i64> {
        let c = self.conn().ok()?;
        c.query_row("SELECT id FROM playlists WHERE url = ?", [u], |r| r.get(0))
            .ok()
    }

    pub fn playlist_by_name(&self, name: &str) -> Option<i64> {
        let c = self.conn().ok()?;
        c.query_row(
            "SELECT id FROM playlists WHERE name = ? AND kind = 'saved' LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .ok()
    }

    /// `playlists edit cmd:up|down|delete|add|move`
    pub fn playlist_edit(&self, id: i64, op: PlaylistEditOp) -> anyhow::Result<()> {
        let (name, _, mut urls) = self
            .playlist_by_id(id)
            .ok_or_else(|| anyhow::anyhow!("playlist {} not found", id))?;
        match op {
            PlaylistEditOp::Up(i) => {
                if i > 0 && i < urls.len() {
                    urls.swap(i, i - 1);
                }
            }
            PlaylistEditOp::Down(i) => {
                if i + 1 < urls.len() {
                    urls.swap(i, i + 1);
                }
            }
            PlaylistEditOp::Delete(i) => {
                if i < urls.len() {
                    urls.remove(i);
                }
            }
            PlaylistEditOp::Add { url, index } => match index {
                Some(i) if i <= urls.len() => urls.insert(i, url),
                _ => urls.push(url),
            },
            PlaylistEditOp::Move { from, to } => {
                if from < urls.len() {
                    let u = urls.remove(from);
                    let to = to.min(urls.len());
                    urls.insert(to, u);
                }
            }
        }
        let c = self.conn()?;
        set_playlist_tracks(&c, id, &urls)?;
        let path = self.playlist_path(&name)?;
        self.write_playlist_file(&c, &path, &urls)?;
        let (mt, sz, _) = scanner::file_stat(&path).unwrap_or((0, 0, false));
        c.execute(
            "UPDATE files SET mtime = ?, size = ? WHERE id = (SELECT file_id FROM playlists WHERE id = ?)",
            params![mt, sz, id],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // listening history
    // ---------------------------------------------------------------------------------------

    pub fn mark_played(&self, u: &str) {
        let Ok(c) = self.conn() else { return };
        let now = scanner::now();
        let n = c
            .execute(
                "UPDATE track_stats SET play_count = play_count + 1, last_played = ? WHERE url = ?",
                params![now, u],
            )
            .unwrap_or(0);
        if n == 0 && track::load_by_url(&c, u).is_some() {
            let _ = c.execute(
                "INSERT INTO track_stats (url, added_at, play_count, last_played) VALUES (?, ?, 1, ?)",
                params![u, now, now],
            );
        }
    }

    /// Set a rating (0..100) for a track given by URL or numeric id.
    pub fn set_rating(&self, url_or_id: &str, rating: u8) {
        let Ok(c) = self.conn() else { return };
        let u = match url_or_id.parse::<i64>() {
            Ok(id) => match track::load_by_id(&c, id) {
                Some(t) => t.url,
                None => return,
            },
            Err(_) => url_or_id.to_string(),
        };
        let rating = (rating > 0).then_some(rating as i64);
        let n = c
            .execute(
                "UPDATE track_stats SET rating = ? WHERE url = ?",
                params![rating, u],
            )
            .unwrap_or(0);
        if n == 0 {
            let _ = c.execute(
                "INSERT INTO track_stats (url, added_at, rating) VALUES (?, ?, ?)",
                params![u, scanner::now(), rating],
            );
        }
    }

    /// Rating of a track (url or id), 0 when unknown.
    pub fn rating(&self, url_or_id: &str) -> u8 {
        let Ok(c) = self.conn() else { return 0 };
        let t = match url_or_id.parse::<i64>() {
            Ok(id) => track::load_by_id(&c, id),
            Err(_) => track::load_by_url(&c, url_or_id),
        };
        t.and_then(|t| t.rating).unwrap_or(0)
    }

    // ---------------------------------------------------------------------------------------
    // artwork
    // ---------------------------------------------------------------------------------------

    pub fn artwork(&self, id: &str, spec: &str) -> Option<(Vec<u8>, String)> {
        artwork::artwork(self, id, spec)
    }

    pub fn resize_image(bytes: &[u8], spec: &str) -> Option<(Vec<u8>, String)> {
        artwork::resize_image(bytes, spec)
    }

    /// Artist portrait for `/contributor/<portraitid>/<spec>` (`spec` like `image_300x300_m`).
    pub fn contributor_image(&self, portraitid: &str, spec: &str) -> Option<(Vec<u8>, String)> {
        artwork::contributor_image(self, portraitid, spec)
    }

    /// Picture file behind a `portraitid`.
    pub fn contributor_image_path(&self, portraitid: &str) -> Option<PathBuf> {
        let c = self.conn().ok()?;
        let u: String = c
            .query_row(
                "SELECT portrait FROM artists WHERE portrait_id = ? LIMIT 1",
                [portraitid],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()?
            .flatten()?;
        url::path_from_file_url(&u)
    }

    /// Where downloaded artist pictures are cached (`<cache>/artists`). Portraits stored there
    /// are replaced by local pictures on the next scan.
    pub fn artist_image_dir(&self) -> PathBuf {
        self.inner.cache_dir.join("artists")
    }

    /// Contributors the portrait scan considered but found no picture for: `(id, name)`.
    pub fn contributors_without_portrait(&self) -> Vec<(i64, String)> {
        let cfg = self.config();
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let va = cfg.various_artists_string.to_lowercase();
        let none = cfg.no_artist_string.to_lowercase();
        portraits::candidates(&c, &cfg)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, name, portrait, _)| {
                portrait.is_none() && !name.is_empty() && {
                    let n = name.to_lowercase();
                    n != va && n != none
                }
            })
            .map(|(id, name, _, _)| (id, name))
            .collect()
    }

    /// Use the picture at `path` as the portrait of contributor `id`; returns the `portraitid`.
    pub fn set_contributor_portrait(&self, id: i64, path: &Path) -> Option<String> {
        let c = self.conn().ok()?;
        portraits::set_portrait(&c, id, path).ok().flatten()
    }
}

fn sc_aborted(a: &AtomicBool) -> bool {
    a.load(Ordering::Relaxed)
}

pub(crate) fn set_playlist_tracks(
    c: &rusqlite::Connection,
    playlist: i64,
    urls: &[String],
) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM playlist_items WHERE playlist_id = ?",
        [playlist],
    )?;
    let mut st = c.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, position, url) VALUES (?, ?, ?)",
    )?;
    for (i, u) in urls.iter().enumerate() {
        st.execute(params![playlist, i as i64, u])?;
    }
    Ok(())
}

pub(crate) fn playlist_urls(
    c: &rusqlite::Connection,
    playlist: i64,
) -> rusqlite::Result<Vec<String>> {
    let mut st =
        c.prepare_cached("SELECT url FROM playlist_items WHERE playlist_id = ? ORDER BY position")?;
    let rows = st.query_map([playlist], |r| r.get(0))?;
    rows.collect()
}
