//! TEMPORARY API stub of `rms-library` used by the server while the real crate is developed.
//! The real implementation replaces this file wholesale.

use std::path::{Path, PathBuf};

pub type Row = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Default)]
pub struct LibraryConfig {
    pub media_dirs: Vec<PathBuf>,
    pub playlist_dir: Option<PathBuf>,
    pub ignored_articles: String,
    pub split_list: String,
    pub various_artists_string: String,
    pub no_genre_string: String,
    pub group_discs: bool,
}

#[derive(Debug, Clone)]
pub enum ScanMode {
    Full,
    Wipe,
    Playlists,
    Dir(PathBuf),
}

#[derive(Debug, Clone, Default)]
pub struct ScanSummary {}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub running: bool,
    pub step: String,
    pub done: u64,
    pub total: u64,
    pub last_scan: Option<i64>,
    pub total_time: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub albums: u64,
    pub artists: u64,
    pub genres: u64,
    pub songs: u64,
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibQueryName {
    Albums,
    Artists,
    Genres,
    Years,
    Titles,
    Search,
    MusicFolder,
    Playlists,
    PlaylistTracks,
    SongInfo,
    Roles,
    Works,
    Libraries,
}

#[derive(Debug, Clone)]
pub struct LibQuery {
    pub name: LibQueryName,
    pub start: Option<usize>,
    pub quantity: Option<usize>,
    pub params: indexmap::IndexMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct TrackFilter {
    pub track_ids: Vec<i64>,
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    pub genre_id: Option<i64>,
    pub year: Option<i64>,
    pub playlist_id: Option<i64>,
    pub folder_id: Option<i64>,
    pub search: Option<String>,
    pub role_id: Option<String>,
    pub library_id: Option<String>,
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Track {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub album_id: Option<i64>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub artist_id: Option<i64>,
    pub albumartist: Option<String>,
    pub genre: Option<String>,
    pub genre_id: Option<i64>,
    pub year: Option<i64>,
    pub tracknum: Option<i64>,
    pub disc: Option<i64>,
    pub content_type: String,
    pub secs: Option<f64>,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub samplesize: Option<u32>,
    pub channels: Option<u32>,
    pub lossless: bool,
    pub vbr: bool,
    pub endian: Option<i64>,
    pub audio_offset: Option<u64>,
    pub audio_size: Option<u64>,
    pub filesize: Option<u64>,
    pub replay_gain: Option<f64>,
    pub replay_peak: Option<f64>,
    pub album_replay_gain: Option<f64>,
    pub coverid: Option<String>,
    pub remote: bool,
    pub virtual_cue: bool,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
    pub compilation: bool,
}

#[derive(Clone, Default)]
pub struct Library {}

impl Library {
    pub fn open(_db_path: &Path, _cache_dir: &Path) -> anyhow::Result<Library> {
        Ok(Library {})
    }
    pub fn set_config(&self, _cfg: LibraryConfig) {}
    pub fn scan(&self, _mode: ScanMode) -> anyhow::Result<ScanSummary> {
        Ok(ScanSummary {})
    }
    pub fn abort_scan(&self) {}
    pub fn progress(&self) -> ScanProgress {
        ScanProgress::default()
    }
    pub fn is_scanning(&self) -> bool {
        false
    }
    pub fn totals(&self) -> Totals {
        Totals::default()
    }
    pub fn query(&self, _q: &LibQuery) -> anyhow::Result<Row> {
        Ok(Row::new())
    }
    pub fn track(&self, _id: i64) -> Option<Track> {
        None
    }
    pub fn track_by_url(&self, _url: &str) -> Option<Track> {
        None
    }
    pub fn track_row(&self, _track: &Track, _tags: &str) -> Row {
        Row::new()
    }
    pub fn tracks_for(&self, _f: &TrackFilter) -> anyhow::Result<Vec<Track>> {
        Ok(vec![])
    }
    pub fn save_playlist(&self, _name: &str, _urls: &[String]) -> anyhow::Result<i64> {
        Ok(0)
    }
    pub fn delete_playlist(&self, _id: i64) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn rename_playlist(&self, _id: i64, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn playlist_by_id(&self, _id: i64) -> Option<(String, String, Vec<String>)> {
        None
    }
    pub fn mark_played(&self, _url: &str) {}
    pub fn set_rating(&self, _url_or_id: &str, _rating: i64) {}
    pub fn artwork(&self, _id: &str, _spec: &str) -> Option<(Vec<u8>, String)> {
        None
    }
    pub fn resize_image(_bytes: &[u8], _spec: &str) -> Option<(Vec<u8>, String)> {
        None
    }
}
